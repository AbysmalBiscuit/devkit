//! Supabase Auth sessions against a scripted loopback server, with the
//! session file in a scratch state directory.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use devkit_supabase::{
    Api, Auth,
    auth::{Client, Credentials, Pkce, Session, SessionFile, Sessions},
    fakehttp::FakeServer,
};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(5);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn token(access: &str, refresh: &str) -> Value {
    json!({
        "access_token": access,
        "refresh_token": refresh,
        "expires_at": now() + 3600,
        "expires_in": 3600,
        "token_type": "bearer",
        "user": {"email": "f@x"},
    })
}

fn credentials() -> Box<dyn Fn() -> Option<Credentials> + Send + Sync> {
    Box::new(|| {
        Some(Credentials {
            email: "f@x".into(),
            password: "p".into(),
        })
    })
}

fn none() -> Box<dyn Fn() -> Option<Credentials> + Send + Sync> {
    Box::new(|| None)
}

struct Fixture {
    server: FakeServer,
    state: tempfile::TempDir,
}

impl Fixture {
    fn new(answers: Vec<(u16, Value)>) -> Fixture {
        Fixture {
            server: FakeServer::start(answers),
            state: tempfile::tempdir().unwrap(),
        }
    }

    fn client(&self) -> Client {
        Client::new(&self.server.url(), "pk", WAIT).unwrap()
    }

    fn file(&self) -> SessionFile {
        SessionFile::for_url(self.state.path(), &self.server.url())
    }

    fn sessions(
        &self,
        credentials: Box<dyn Fn() -> Option<Credentials> + Send + Sync>,
    ) -> Sessions {
        Sessions::new(self.client(), self.file(), credentials)
    }

    fn save(&self, access: &str, expires_at: i64) {
        self.file()
            .save(&Session {
                access_token: access.into(),
                refresh_token: "r0".into(),
                expires_at,
            })
            .unwrap();
    }
}

#[test]
fn password_grant_posts_credentials_and_saves_the_session() {
    let f = Fixture::new(vec![(200, token("a1", "r1"))]);
    let session = f
        .client()
        .password(&Credentials {
            email: "f@x".into(),
            password: "p".into(),
        })
        .unwrap();
    f.file().save(&session).unwrap();
    let request = &f.server.requests()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path_and_query, "/auth/v1/token?grant_type=password");
    assert_eq!(request.json(), json!({"email": "f@x", "password": "p"}));
    assert_eq!(request.header("apikey"), Some("pk"));
    let saved = f.file().load().expect("a saved session");
    assert_eq!(saved.access_token, "a1");
    assert_eq!(saved.refresh_token, "r1");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(f.file().path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn a_fresh_token_is_used_as_is() {
    let f = Fixture::new(Vec::new());
    f.save("a0", now() + 3600);
    assert_eq!(f.sessions(none()).access_token().unwrap(), "a0");
    assert!(f.server.requests().is_empty());
}

#[test]
fn token_inside_margin_is_refreshed_first() {
    let f = Fixture::new(vec![(200, token("a1", "r1"))]);
    f.save("a0", now() + 30);
    assert_eq!(f.sessions(none()).access_token().unwrap(), "a1");
    let request = &f.server.requests()[0];
    assert_eq!(
        request.path_and_query,
        "/auth/v1/token?grant_type=refresh_token"
    );
    assert_eq!(request.json(), json!({"refresh_token": "r0"}));
    assert_eq!(f.file().load().unwrap().access_token, "a1");
}

#[test]
fn successful_refresh_resolves_no_credentials() {
    let f = Fixture::new(vec![(200, token("a1", "r1"))]);
    f.save("a0", now() - 10);
    let resolved = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&resolved);
    let sessions = f.sessions(Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        None
    }));
    assert_eq!(sessions.access_token().unwrap(), "a1");
    assert_eq!(resolved.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_refresh_falls_back_to_password() {
    let f = Fixture::new(vec![
        (
            400,
            json!({"error": "invalid_grant", "error_description": "Refresh Token Not Found"}),
        ),
        (200, token("a2", "r2")),
    ]);
    f.save("a0", now() - 10);
    assert_eq!(f.sessions(credentials()).access_token().unwrap(), "a2");
    let requests = f.server.requests();
    assert_eq!(
        requests[1].path_and_query,
        "/auth/v1/token?grant_type=password"
    );
}

#[test]
fn no_session_signs_in_with_the_password() {
    let f = Fixture::new(vec![(200, token("a1", "r1"))]);
    assert_eq!(f.sessions(credentials()).access_token().unwrap(), "a1");
    assert_eq!(f.file().load().unwrap().refresh_token, "r1");
}

#[test]
fn no_session_no_credentials_names_the_command() {
    let f = Fixture::new(Vec::new());
    let err = f.sessions(none()).access_token().unwrap_err();
    assert!(err.to_string().contains("devkit auth supabase"), "{err}");
}

#[test]
fn a_refused_sign_in_never_repeats_the_password() {
    let f = Fixture::new(vec![(
        400,
        json!({"code": 400, "error_code": "invalid_credentials", "msg": "Invalid login credentials"}),
    )]);
    let err = f
        .client()
        .password(&Credentials {
            email: "f@x".into(),
            password: "hunter2".into(),
        })
        .unwrap_err();
    let message = format!("{err:#}");
    assert!(message.contains("Invalid login credentials"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
}

#[test]
fn corrupt_session_file_reads_as_none() {
    let f = Fixture::new(Vec::new());
    let path = f.file().path().to_path_buf();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{not json").unwrap();
    assert!(f.file().load().is_none());
}

#[test]
fn api_retries_once_after_a_401() {
    let f = Fixture::new(vec![
        (401, json!({"message": "JWT expired"})),
        (200, token("a1", "r1")),
        (200, json!({})),
    ]);
    f.save("a0", now() + 3600);
    let api = Api::new(
        &f.server.url(),
        "repo_rules_api",
        Auth::User(Arc::new(f.sessions(none()))),
        WAIT,
        "rules API",
    )
    .unwrap();
    api.call("stats_rules", &json!({})).unwrap();
    let requests = f.server.requests();
    let rpc: Vec<_> = requests
        .iter()
        .filter(|r| r.path_and_query.starts_with("/rest/v1/rpc/"))
        .collect();
    assert_eq!(rpc.len(), 2);
    assert_eq!(rpc[0].header("authorization"), Some("Bearer a0"));
    assert_eq!(rpc[1].header("authorization"), Some("Bearer a1"));
    assert_eq!(rpc[1].header("apikey"), Some("pk"));
}

#[test]
fn providers_lists_enabled_external_providers() {
    let f = Fixture::new(vec![(
        200,
        json!({"external": {"github": true, "google": false, "email": true, "azure": true}}),
    )]);
    assert_eq!(f.client().providers().unwrap(), vec!["azure", "github"]);
    assert_eq!(f.server.requests()[0].path_and_query, "/auth/v1/settings");
}

#[test]
fn pkce_exchange_posts_code_and_verifier() {
    let f = Fixture::new(vec![(200, token("a1", "r1"))]);
    let session = f.client().pkce("abc", "v").unwrap();
    assert_eq!(session.access_token, "a1");
    let request = &f.server.requests()[0];
    assert_eq!(request.path_and_query, "/auth/v1/token?grant_type=pkce");
    assert_eq!(
        request.json(),
        json!({"auth_code": "abc", "code_verifier": "v"})
    );
}

#[test]
fn one_time_code_is_sent_then_verified() {
    let f = Fixture::new(vec![(200, json!({})), (200, token("a1", "r1"))]);
    f.client().send_code("f@x").unwrap();
    let session = f.client().verify_code("f@x", "123456").unwrap();
    assert_eq!(session.access_token, "a1");
    let requests = f.server.requests();
    assert_eq!(requests[0].path_and_query, "/auth/v1/otp");
    assert_eq!(
        requests[0].json(),
        json!({"email": "f@x", "create_user": false})
    );
    assert_eq!(requests[1].path_and_query, "/auth/v1/verify");
    assert_eq!(
        requests[1].json(),
        json!({"type": "email", "email": "f@x", "token": "123456"})
    );
}

#[test]
fn pkce_challenge_is_s256_of_verifier() {
    let pkce = Pkce::new();
    assert_eq!(pkce.verifier.len(), 43);
    let digest = ring::digest::digest(&ring::digest::SHA256, pkce.verifier.as_bytes());
    assert_eq!(
        URL_SAFE_NO_PAD.decode(&pkce.challenge).unwrap(),
        digest.as_ref()
    );
    assert_ne!(Pkce::new().verifier, pkce.verifier);
}

#[test]
fn authorize_url_carries_pkce() {
    let client = Client::new("https://ref.supabase.co", "pk", WAIT).unwrap();
    let url = client.authorize_url("github", "http://localhost:7471/callback", "ch");
    assert!(
        url.starts_with("https://ref.supabase.co/auth/v1/authorize?"),
        "{url}"
    );
    assert!(url.contains("provider=github"), "{url}");
    assert!(url.contains("code_challenge=ch"), "{url}");
    assert!(url.contains("code_challenge_method=s256"), "{url}");
    assert!(
        url.contains("redirect_to=http%3A%2F%2Flocalhost%3A7471%2Fcallback"),
        "{url}"
    );
}

#[test]
fn a_refused_password_sign_in_names_the_command() {
    let f = Fixture::new(vec![(
        400,
        json!({"code": 400, "error_code": "invalid_credentials", "msg": "Invalid login credentials"}),
    )]);
    let sessions = Sessions::new(
        f.client(),
        f.file(),
        Box::new(|| {
            Some(Credentials {
                email: "f@x".into(),
                password: "hunter2".into(),
            })
        }),
    );
    let message = format!("{:#}", sessions.access_token().unwrap_err());
    assert!(message.contains("devkit auth supabase"), "{message}");
    assert!(message.contains("Invalid login credentials"), "{message}");
    assert!(!message.contains("hunter2"), "{message}");
}

/// A refused sign-in says the kept credentials went stale, as a 401 from
/// the API does.
#[test]
fn a_refused_sign_in_runs_on_rejected() {
    let f = Fixture::new(vec![(
        400,
        json!({"code": 400, "error_code": "invalid_credentials", "msg": "Invalid login credentials"}),
    )]);
    let api = Api::new(
        &f.server.url(),
        "repo_rules_api",
        Auth::User(Arc::new(f.sessions(credentials()))),
        WAIT,
        "rules API",
    )
    .unwrap();
    let rejected = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&rejected);
    api.on_rejected(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    api.call("stats_rules", &json!({})).unwrap_err();
    assert_eq!(rejected.load(Ordering::SeqCst), 1);
}

#[test]
fn an_unanswered_sign_in_fails_the_next_request_at_once() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", silent.local_addr().unwrap());
    let state = tempfile::tempdir().unwrap();
    let sessions = Sessions::new(
        Client::new(&url, "pk", Duration::from_secs(30)).unwrap(),
        SessionFile::for_url(state.path(), &url),
        credentials(),
    );
    let api = Api::new(
        &url,
        "repo_rules_api",
        Auth::User(Arc::new(sessions)),
        Duration::from_millis(500),
        "rules API",
    )
    .unwrap();
    let rejected = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&rejected);
    api.on_rejected(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let started = std::time::Instant::now();
    let first = api.call("stats_rules", &json!({})).unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(devkit_supabase::is_unreachable(&first), "{first:#}");
    let started = std::time::Instant::now();
    let second = api.call("stats_rules", &json!({})).unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(devkit_supabase::is_unreachable(&second), "{second:#}");
    assert_eq!(rejected.load(Ordering::SeqCst), 0);
    drop(silent);
}

#[test]
fn a_url_with_credentials_is_refused_without_repeating_it() {
    for url in [
        "https://user:secret@ref.supabase.co",
        "https://secret@ref.supabase.co",
        "https://ref.supabase.co/?apikey=secret",
        "https://ref.supabase.co/#secret",
    ] {
        let err = Client::new(url, "pk", WAIT).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("URL"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }
}
