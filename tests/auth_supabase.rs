//! `devkit auth supabase` against a fake Supabase Auth: a password sign-in,
//! and the browser flow's code exchange through the local callback.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

use devkit_supabase::fakehttp::FakeServer;
use serde_json::json;

const URL_VAR: &str = "DEVKIT_RULES_SUPABASE_URL";
const REPO: &str = "0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10";

const AMBIENT: [&str; 6] = [
    URL_VAR,
    "DEVKIT_RULES_SUPABASE_EMAIL",
    "DEVKIT_RULES_SUPABASE_PASSWORD",
    "DEVKIT_CONFIG",
    "XDG_CONFIG_HOME",
    "BROWSER",
];

struct Proj {
    _root: tempfile::TempDir,
    home: tempfile::TempDir,
    path: PathBuf,
    /// A `PATH` holding `git` alone, so no browser opener is found.
    bin: PathBuf,
}

impl Proj {
    fn new(extra: &str) -> Proj {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("proj");
        std::fs::create_dir_all(&path).unwrap();
        devkit_git::Git::fixture(&path)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(
            path.join("devkit.toml"),
            format!(
                "[rules]\nsource = \"supabase\"\n[rules.supabase]\nrepository = \"{REPO}\"\n{extra}"
            ),
        )
        .unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        #[cfg(unix)]
        if let Some(git) = find_on_path("git") {
            std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
        }
        Proj {
            _root: root,
            home: tempfile::tempdir().unwrap(),
            path,
            bin,
        }
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
        cmd.args(args)
            .current_dir(&self.path)
            .env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.home.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .env("PATH", &self.bin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in AMBIENT {
            cmd.env_remove(var);
        }
        testenv::scrub_identity(&mut cmd);
        cmd.envs(env.iter().copied());
        cmd
    }

    fn devkit(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.command(args, env).output().unwrap()
    }

    fn sessions(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.home.path().join("devkit").join("supabase-sessions"))
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn token() -> serde_json::Value {
    json!({
        "access_token": "a1",
        "refresh_token": "r1",
        "expires_in": 3600,
        "user": {"email": "f@x"},
    })
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Sends `GET <path>` to the callback on `port` once it listens, and returns
/// the answer.
fn callback(child: &mut Child, port: u16, path: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(mut tcp) = TcpStream::connect(("127.0.0.1", port)) {
            tcp.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .unwrap();
            let mut answer = String::new();
            let _ = tcp.read_to_string(&mut answer);
            return answer;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "devkit exited before listening"
        );
        assert!(Instant::now() < deadline, "the callback never listened");
        std::thread::yield_now();
    }
}

fn wait(child: Child) -> Output {
    child.wait_with_output().unwrap()
}

#[test]
fn password_sign_in_saves_a_session() {
    let server = FakeServer::start(vec![(200, token())]);
    let p = Proj::new("publishable_key = \"pk\"\n");
    let url = server.url();
    let out = p.devkit(&["auth", "supabase", "--password"], &[
        (URL_VAR, &url),
        ("DEVKIT_RULES_SUPABASE_EMAIL", "f@x"),
        ("DEVKIT_RULES_SUPABASE_PASSWORD", "hunter2"),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("signed in to"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(p.sessions().len(), 1, "{:?}", p.sessions());
    let request = &server.requests()[0];
    assert_eq!(request.path_and_query, "/auth/v1/token?grant_type=password");
    assert_eq!(request.header("apikey"), Some("pk"));
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(!text.contains("hunter2"), "{text}");
}

#[test]
fn password_sign_in_without_the_secrets_names_them() {
    let server = FakeServer::start(Vec::new());
    let p = Proj::new("publishable_key = \"pk\"\n");
    let out = p.devkit(&["auth", "supabase", "--password"], &[(
        URL_VAR,
        &server.url(),
    )]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("DEVKIT_RULES_SUPABASE_PASSWORD"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn browser_flow_exchanges_the_code() {
    let server = FakeServer::start(vec![
        (200, json!({"external": {"github": true, "email": true}})),
        (200, token()),
    ]);
    let port = free_port();
    let p = Proj::new(&format!(
        "publishable_key = \"pk\"\ncallback_port = {port}\n"
    ));
    let mut child = p
        .command(&["auth", "supabase", "--with", "github"], &[(
            URL_VAR,
            &server.url(),
        )])
        .spawn()
        .unwrap();
    let answer = callback(&mut child, port, "/callback?code=abc");
    assert!(answer.contains("Signed in"), "{answer}");
    let out = wait(child);
    assert!(out.status.success(), "{}", stderr(&out));
    let requests = server.requests();
    let exchange = requests
        .iter()
        .find(|r| r.path_and_query == "/auth/v1/token?grant_type=pkce")
        .expect("a code exchange");
    let body = exchange.json();
    assert_eq!(body["auth_code"], "abc");
    assert_eq!(body["code_verifier"].as_str().unwrap().len(), 43);
    assert!(
        stderr(&out).contains("/auth/v1/authorize?provider=github"),
        "{}",
        stderr(&out)
    );
    assert_eq!(p.sessions().len(), 1);
}

#[test]
fn a_refused_browser_sign_in_names_the_allow_list() {
    let server = FakeServer::start(vec![(200, json!({"external": {"github": true}}))]);
    let port = free_port();
    let p = Proj::new(&format!(
        "publishable_key = \"pk\"\ncallback_port = {port}\n"
    ));
    let mut child = p
        .command(&["auth", "supabase"], &[(URL_VAR, &server.url())])
        .spawn()
        .unwrap();
    callback(
        &mut child,
        port,
        "/callback?error=invalid_request&error_description=redirect%20url%20not%20allowed",
    );
    let out = wait(child);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(err.contains("redirect url not allowed"), "{err}");
    assert!(
        err.contains(&format!("http://localhost:{port}/callback")),
        "{err}"
    );
    assert!(p.sessions().is_empty());
}

#[test]
fn a_held_callback_port_is_refused() {
    let server = FakeServer::start(vec![(200, json!({"external": {"github": true}}))]);
    let held = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let p = Proj::new(&format!(
        "publishable_key = \"pk\"\ncallback_port = {port}\n"
    ));
    let out = p.devkit(&["auth", "supabase", "--with", "github"], &[(
        URL_VAR,
        &server.url(),
    )]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains(&format!("port {port} is in use")),
        "{}",
        stderr(&out)
    );
}

#[test]
fn unknown_provider_lists_the_enabled_ones() {
    let server = FakeServer::start(vec![(200, json!({"external": {"github": true}}))]);
    let p = Proj::new("publishable_key = \"pk\"\n");
    let out = p.devkit(&["auth", "supabase", "--with", "gitlab"], &[(
        URL_VAR,
        &server.url(),
    )]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("github"), "{}", stderr(&out));
}

#[test]
fn no_publishable_key_is_refused() {
    let p = Proj::new("");
    let out = p.devkit(&["auth", "supabase", "--password"], &[(
        URL_VAR,
        "http://127.0.0.1:1",
    )]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("[rules.supabase] publishable_key is not set"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn flags_refused_for_other_providers() {
    let p = Proj::new("");
    let out = p.devkit(&["auth", "linear", "--with", "github"], &[]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("apply to `devkit auth supabase` alone"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_token_is_refused_for_supabase() {
    let p = Proj::new("publishable_key = \"pk\"\n");
    let out = p.devkit(&["auth", "supabase", "--token", "t"], &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--token"), "{}", stderr(&out));
}
