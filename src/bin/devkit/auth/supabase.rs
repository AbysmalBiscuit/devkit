//! `devkit auth supabase`: sign in to the Supabase project the `supabase`
//! rules source reads, and keep the session for hooks. The browser flow is
//! PKCE, its code taken on a local callback.

use std::{
    io::{BufRead, BufReader, IsTerminal, Write},
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::Args;
use devkit_common::vcs::Checkout;
use devkit_config::RulesSupabaseConfig;
use devkit_supabase::auth::{Client, Pkce, Session, SessionFile, query_param};

use crate::secret::SecretLookup;

/// How long Supabase Auth may take to answer.
const WAIT: Duration = Duration::from_secs(15);

/// How long the browser sign-in may take.
const SIGN_IN_WAIT: Duration = Duration::from_secs(5 * 60);

/// How `devkit auth supabase` signs in.
#[derive(Args, Default)]
pub struct SupabaseLogin {
    /// Sign in through this provider in the browser, such as `github`,
    /// instead of choosing among the ones the project enables. supabase only.
    #[arg(long, conflicts_with_all = ["email", "password"])]
    pub with: Option<String>,
    /// Sign in with a one-time code Supabase emails you, typed into this
    /// terminal; no browser. supabase only.
    #[arg(long)]
    pub email: bool,
    /// Sign in with DEVKIT_RULES_SUPABASE_EMAIL and
    /// DEVKIT_RULES_SUPABASE_PASSWORD, from the environment, Doppler or the
    /// secrets file. supabase only.
    #[arg(long, conflicts_with = "email")]
    pub password: bool,
    /// The project URL, over DEVKIT_RULES_SUPABASE_URL and the global
    /// config's [rules.supabase] url. supabase only.
    #[arg(long)]
    pub url: Option<String>,
}

impl SupabaseLogin {
    /// Whether any of these flags is given.
    pub fn any(&self) -> bool {
        self.with.is_some() || self.email || self.password || self.url.is_some()
    }
}

pub fn run(login: SupabaseLogin) -> Result<()> {
    let config = project_config()?;
    let Some(key) = &config.publishable_key else {
        bail!("[rules.supabase] publishable_key is not set");
    };
    let url = match login.url {
        Some(url) => url,
        None => crate::rules::supabase_url().0?,
    };
    let url = url.trim().trim_end_matches('/').to_string();
    let client = Client::new(&url, key, WAIT);
    let file = SessionFile::for_url(&devkit_common::paths::state_dir(), &url);
    let (session, who) = if login.password {
        password(&client, &config)?
    } else if login.email {
        email(&client)?
    } else {
        (browser(&client, &config, login.with.as_deref())?, None)
    };
    file.save(&session)?;
    match who {
        Some(email) => println!("signed in to {url} as {email}"),
        None => println!("signed in to {url}"),
    }
    println!("  session kept in {}", file.path().display());
    Ok(())
}

/// The `[rules.supabase]` settings this checkout's config gives.
fn project_config() -> Result<RulesSupabaseConfig> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    let checkout = Checkout::at(&cwd);
    let (project, _) = devkit_common::config::resolve_in(&checkout, None, &cwd)?;
    Ok(project.rules.supabase)
}

fn password(client: &Client, config: &RulesSupabaseConfig) -> Result<(Session, Option<String>)> {
    let (credentials, _) = crate::rules::resolve_credentials(config, SecretLookup::Doppler);
    let Some(credentials) = credentials else {
        bail!(
            "{} and {} must both resolve, from the environment, Doppler or the secrets file",
            crate::rules::SUPABASE_EMAIL_VAR,
            crate::rules::SUPABASE_PASSWORD_VAR
        );
    };
    let session = client.password(&credentials)?;
    Ok((session, Some(credentials.email)))
}

fn email(client: &Client) -> Result<(Session, Option<String>)> {
    if !std::io::stdin().is_terminal() {
        bail!("--email reads a code typed into a terminal; use --password elsewhere");
    }
    let email = prompt("Email: ")?;
    client.send_code(&email)?;
    eprintln!("Supabase emailed a code to {email}.");
    let code = prompt("Code: ")?;
    let session = client.verify_code(&email, &code)?;
    Ok((session, Some(email)))
}

fn prompt(label: &str) -> Result<String> {
    eprint!("{label}");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("reading the terminal")?;
    let line = line.trim().to_string();
    if line.is_empty() {
        bail!("nothing entered");
    }
    Ok(line)
}

/// Signs in through a provider in the browser and returns the session the
/// callback's code exchanges for.
fn browser(client: &Client, config: &RulesSupabaseConfig, with: Option<&str>) -> Result<Session> {
    let providers = client.providers()?;
    let provider = choose(&providers, with)?;
    let port = config.callback_port;
    if let Some(entry) = devkit_ports::registry::load().entries.get(&port) {
        bail!(
            "port {port} is held by {} ({}) in devkit's port registry; set [rules.supabase] \
             callback_port, and add http://localhost:<port>/callback to the project's \
             redirect allow list",
            entry.holder,
            entry.app
        );
    }
    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("port {port} is in use; set [rules.supabase] callback_port"))?;
    let redirect = format!("http://localhost:{port}/callback");
    let pkce = Pkce::new();
    let page = client.authorize_url(&provider, &redirect, &pkce.challenge);
    eprintln!("Open this page to sign in with {provider}:\n\n  {page}\n");
    open(&page);
    let code = callback(&listener, &redirect)?;
    client.pkce(&code, &pkce.verifier)
}

/// The provider `with` names, the only one enabled, or the one picked from a
/// numbered list.
fn choose(providers: &[String], with: Option<&str>) -> Result<String> {
    let list = || providers.join(", ");
    if providers.is_empty() {
        bail!("the project enables no external sign-in provider; use --email or --password");
    }
    if let Some(with) = with {
        return match providers.iter().find(|p| p.eq_ignore_ascii_case(with)) {
            Some(provider) => Ok(provider.clone()),
            None => bail!("the project does not enable {with}; enabled: {}", list()),
        };
    }
    if let [only] = providers {
        return Ok(only.clone());
    }
    if !std::io::stdin().is_terminal() {
        bail!("pass --with to pick a provider: {}", list());
    }
    for (n, provider) in providers.iter().enumerate() {
        eprintln!("  {}. {provider}", n + 1);
    }
    let picked = prompt("Sign in with: ")?;
    picked
        .parse::<usize>()
        .ok()
        .and_then(|n| providers.get(n.checked_sub(1)?))
        .or_else(|| providers.iter().find(|p| **p == picked))
        .cloned()
        .with_context(|| format!("not one of: {}", list()))
}

/// Tries the platform's opener on `page`, leaving the printed link as the
/// fallback.
fn open(page: &str) {
    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(windows) {
        ("cmd", &["/c", "start", ""])
    } else {
        ("xdg-open", &[])
    };
    let _ = std::process::Command::new(program)
        .args(args)
        .arg(page)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Waits for the browser's request to `/callback` and returns its code.
/// Other requests, and connections that send nothing, are answered and
/// ignored.
fn callback(listener: &TcpListener, redirect: &str) -> Result<String> {
    let deadline = Instant::now() + SIGN_IN_WAIT;
    listener.set_nonblocking(true)?;
    loop {
        let tcp = match listener.accept() {
            Ok((tcp, _)) => tcp,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!(
                        "no sign-in arrived within {} minutes",
                        SIGN_IN_WAIT.as_secs() / 60
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e).context("waiting for the sign-in callback"),
        };
        let Some(query) = callback_query(&tcp) else {
            answer(&tcp, "404 Not Found", "Not found.");
            continue;
        };
        let param = |name: &str| query_param(&query, name);
        if let Some(code) = param("code") {
            answer(&tcp, "200 OK", "Signed in. You can close this tab.");
            return Ok(code);
        }
        let reason = param("error_description")
            .or_else(|| param("error"))
            .unwrap_or_else(|| "the callback carried no code".to_string());
        answer(&tcp, "400 Bad Request", "Sign-in failed. See the terminal.");
        if reason.to_ascii_lowercase().contains("redirect") {
            bail!("sign-in failed: {reason}; add {redirect} to the project's redirect allow list");
        }
        bail!("sign-in failed: {reason}");
    }
}

/// The query of a `GET /callback` request on `tcp`, `None` for anything
/// else.
fn callback_query(tcp: &TcpStream) -> Option<String> {
    tcp.set_nonblocking(false).ok()?;
    tcp.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut reader = BufReader::new(tcp);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    (path == "/callback").then(|| query.to_string())
}

fn answer(mut tcp: &TcpStream, status: &str, text: &str) {
    let body = format!("<!doctype html><title>devkit</title><p>{text}</p>\n");
    let _ = write!(
        tcp,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}
