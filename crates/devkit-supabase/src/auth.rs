//! Supabase Auth over its HTTP API: the grants that sign a user in, the
//! session they give, and the file a session is kept in between processes.
//!
//! No error repeats a password, an access token or a refresh token.

use std::{
    fmt,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use devkit_common::http;
use reqwest::blocking::{RequestBuilder, Response};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// How long before its expiry an access token is refreshed rather than sent,
/// for the clocks of this machine and the server to disagree by.
const MARGIN_SECS: i64 = 60;

/// A signed-in user's tokens.
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// When the access token expires, in Unix seconds.
    pub expires_at: i64,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// An email and password to sign in with.
pub struct Credentials {
    pub email: String,
    pub password: String,
}

/// A token grant's answer. `expires_at` is absent from some grants, which
/// give `expires_in` alone.
#[derive(Deserialize)]
struct Granted {
    access_token: String,
    refresh_token: String,
    expires_at: Option<i64>,
    expires_in: Option<i64>,
}

/// The error body Supabase Auth answers a refused request with, in either of
/// the shapes its versions send.
#[derive(Default, Deserialize)]
struct Refusal {
    msg: Option<String>,
    message: Option<String>,
    error_description: Option<String>,
    error: Option<String>,
}

/// One project's Supabase Auth, under `/auth/v1/`.
pub struct Client {
    url: String,
    publishable_key: String,
    wait: Duration,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client").field("url", &self.url).finish()
    }
}

impl Client {
    /// The Auth API of the project at `url`, each request carrying
    /// `publishable_key` and giving up after `wait`.
    pub fn new(url: &str, publishable_key: &str, wait: Duration) -> Client {
        Client {
            url: url.trim().trim_end_matches('/').to_string(),
            publishable_key: publishable_key.to_string(),
            wait,
        }
    }

    /// The project's URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The key every request to the project carries as `apikey`.
    pub fn publishable_key(&self) -> &str {
        &self.publishable_key
    }

    /// Signs in with an email and password.
    pub fn password(&self, c: &Credentials) -> Result<Session> {
        self.grant(
            "password",
            json!({"email": c.email, "password": c.password}),
        )
        .with_context(|| format!("signing in to {} as {}", self.url, c.email))
    }

    /// A new session for the one `refresh_token` belongs to.
    pub fn refresh(&self, refresh_token: &str) -> Result<Session> {
        self.grant("refresh_token", json!({"refresh_token": refresh_token}))
            .with_context(|| format!("refreshing the session for {}", self.url))
    }

    /// The session a browser sign-in's `auth_code` stands for, proven by the
    /// PKCE `verifier` it was started with.
    pub fn pkce(&self, auth_code: &str, verifier: &str) -> Result<Session> {
        self.grant(
            "pkce",
            json!({"auth_code": auth_code, "code_verifier": verifier}),
        )
        .with_context(|| format!("exchanging the sign-in code with {}", self.url))
    }

    /// Asks Supabase to email `email` a one-time code, for an existing user.
    pub fn send_code(&self, email: &str) -> Result<()> {
        let req = http::client()
            .post(self.endpoint("otp"))
            .json(&json!({"email": email, "create_user": false}));
        self.send(req)
            .with_context(|| format!("asking {} to email a code to {email}", self.url))?;
        Ok(())
    }

    /// Signs in with the one-time code emailed to `email`.
    pub fn verify_code(&self, email: &str, code: &str) -> Result<Session> {
        let req = http::client()
            .post(self.endpoint("verify"))
            .json(&json!({"type": "email", "email": email, "token": code}));
        let granted = self
            .send(req)
            .and_then(|resp| self.body::<Granted>(resp))
            .with_context(|| format!("verifying the code for {email} with {}", self.url))?;
        Ok(session_of(granted))
    }

    /// The external sign-in providers the project enables, sorted.
    pub fn providers(&self) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Settings {
            #[serde(default)]
            external: serde_json::Map<String, Value>,
        }
        let settings: Settings = self
            .send(http::client().get(self.endpoint("settings")))
            .and_then(|resp| self.body(resp))
            .with_context(|| format!("reading the sign-in settings of {}", self.url))?;
        let mut providers: Vec<String> = settings
            .external
            .into_iter()
            .filter(|(name, on)| {
                on.as_bool() == Some(true)
                    && !matches!(name.as_str(), "email" | "phone" | "anonymous_users")
            })
            .map(|(name, _)| name)
            .collect();
        providers.sort();
        Ok(providers)
    }

    /// The page that signs in with `provider` and sends the browser back to
    /// `redirect_to` with a code [`Client::pkce`] exchanges.
    pub fn authorize_url(&self, provider: &str, redirect_to: &str, challenge: &str) -> String {
        let mut url = match reqwest::Url::parse(&self.endpoint("authorize")) {
            Ok(url) => url,
            Err(_) => return self.endpoint("authorize"),
        };
        url.query_pairs_mut()
            .append_pair("provider", provider)
            .append_pair("redirect_to", redirect_to)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "s256");
        url.to_string()
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/auth/v1/{path}", self.url)
    }

    fn grant(&self, grant_type: &str, body: Value) -> Result<Session> {
        let req = http::client()
            .post(format!(
                "{}?grant_type={grant_type}",
                self.endpoint("token")
            ))
            .json(&body);
        let granted: Granted = self.send(req).and_then(|resp| self.body(resp))?;
        Ok(session_of(granted))
    }

    fn send(&self, req: RequestBuilder) -> Result<Response> {
        let resp = req
            .header("apikey", &self.publishable_key)
            .timeout(self.wait)
            .send()
            .map_err(http::explain)
            .with_context(|| format!("Supabase Auth {}", self.url))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let refusal: Refusal = resp.json().unwrap_or_default();
        let message = refusal
            .msg
            .or(refusal.error_description)
            .or(refusal.message)
            .or(refusal.error)
            .unwrap_or_else(|| status.to_string());
        Err(anyhow!(
            "Supabase Auth {} answered {status}: {message}",
            self.url
        ))
    }

    fn body<T: DeserializeOwned>(&self, resp: Response) -> Result<T> {
        resp.json()
            .map_err(http::explain)
            .with_context(|| format!("reading the answer of Supabase Auth {}", self.url))
    }
}

fn session_of(granted: Granted) -> Session {
    let expires_at = granted
        .expires_at
        .unwrap_or_else(|| now() + granted.expires_in.unwrap_or(0));
    Session {
        access_token: granted.access_token,
        refresh_token: granted.refresh_token,
        expires_at,
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The value of `name` in a URL query string, percent-decoded, such as the
/// `code` a sign-in callback carries.
pub fn query_param(query: &str, name: &str) -> Option<String> {
    reqwest::Url::parse(&format!("http://localhost/?{query}"))
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// A PKCE pair: the verifier kept by the signing-in process, and the S256
/// challenge the authorize page is given.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// A pair from 32 random bytes.
    pub fn new() -> Pkce {
        use ring::rand::SecureRandom;

        let mut bytes = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut bytes)
            .expect("the system's random source answers");
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
        Pkce {
            challenge: URL_SAFE_NO_PAD.encode(digest.as_ref()),
            verifier,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Pkce::new()
    }
}

/// Where one project's session is kept: a JSON file per project host under
/// the state directory, readable by its owner alone.
#[derive(Clone, Debug)]
pub struct SessionFile {
    path: PathBuf,
}

impl SessionFile {
    /// The file for the project at `url`, under `state_dir`.
    pub fn for_url(state_dir: &Path, url: &str) -> SessionFile {
        let host = reqwest::Url::parse(url.trim())
            .ok()
            .and_then(|u| {
                let host = u.host_str()?.to_string();
                Some(match u.port() {
                    Some(port) => format!("{host}-{port}"),
                    None => host,
                })
            })
            .unwrap_or_else(|| "unknown".to_string());
        let name: String = host
            .chars()
            .map(
                |c| match c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    true => c,
                    false => '_',
                },
            )
            .collect();
        SessionFile {
            path: state_dir
                .join("supabase-sessions")
                .join(format!("{name}.json")),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The kept session, `None` when there is none or it does not parse.
    pub fn load(&self) -> Option<Session> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Keeps `session`, replacing the file whole so a reader never sees half
    /// of it.
    pub fn save(&self, session: &Session) -> Result<()> {
        let dir = self
            .path
            .parent()
            .context("a session file has a directory")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let temp = self
            .path
            .with_extension(format!("json.{}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options
            .open(&temp)
            .and_then(|mut file| {
                file.write_all(serde_json::to_string(session)?.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&temp, &self.path));
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        written.with_context(|| format!("writing {}", self.path.display()))
    }

    /// Removes the kept session, if any.
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A user's session for one project, kept in its file, refreshed before it
/// expires, and replaced by a password sign-in when refreshing fails and a
/// password resolves.
pub struct Sessions {
    client: Client,
    file: SessionFile,
    credentials: Box<dyn Fn() -> Option<Credentials> + Send + Sync>,
    /// Held while a session is read or replaced, so one process refreshes
    /// once.
    turn: Mutex<()>,
}

impl fmt::Debug for Sessions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sessions")
            .field("client", &self.client)
            .field("file", &self.file)
            .finish()
    }
}

impl Sessions {
    pub fn new(
        client: Client,
        file: SessionFile,
        credentials: Box<dyn Fn() -> Option<Credentials> + Send + Sync>,
    ) -> Sessions {
        Sessions {
            client,
            file,
            credentials,
            turn: Mutex::new(()),
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn file(&self) -> &SessionFile {
        &self.file
    }

    /// An access token that is not about to expire: the kept one, else a
    /// refreshed one, else one from a password sign-in.
    pub fn access_token(&self) -> Result<String> {
        let _turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        match self.file.load() {
            Some(session) if session.expires_at - now() >= MARGIN_SECS => Ok(session.access_token),
            Some(session) => self.replace(Some(&session)),
            None => self.replace(None),
        }
    }

    /// A new access token, for one the API refused: a refreshed session,
    /// else a password sign-in.
    pub fn renew(&self) -> Result<String> {
        let _turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
        let kept = self.file.load();
        self.replace(kept.as_ref())
    }

    fn replace(&self, kept: Option<&Session>) -> Result<String> {
        let refreshed = kept.map(|session| self.client.refresh(&session.refresh_token));
        let session = match refreshed {
            Some(Ok(session)) => session,
            refreshed => match ((self.credentials)(), refreshed) {
                (Some(credentials), _) => self
                    .client
                    .password(&credentials)
                    .with_context(|| self.not_signed_in())?,
                (None, Some(Err(e))) => return Err(e.context(self.not_signed_in())),
                (None, _) => bail!(self.not_signed_in()),
            },
        };
        self.file.save(&session)?;
        Ok(session.access_token)
    }

    fn not_signed_in(&self) -> String {
        format!(
            "not signed in to {}: run `devkit auth supabase`",
            self.client.url
        )
    }
}
