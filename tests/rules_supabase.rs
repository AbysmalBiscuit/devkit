//! `[rules] source = "supabase"` through `devkit rules` and the hooks,
//! against a fake Data API speaking the `repo_rules_api` contract.

#[path = "../crates/devkit-rules/tests/common/fakeapi.rs"]
mod fakeapi;
#[path = "common/testenv.rs"]
mod testenv;

use std::{
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use devkit_rules::model::RuleIndex;
use devkit_supabase::fakehttp::FakeServer;
use serde_json::{Value, json};

const REPO: &str = "0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10";
const URL_VAR: &str = "DEVKIT_RULES_SUPABASE_URL";
const INDEX: &str = include_str!("../crates/devkit-rules/tests/fixtures/index.json");

const AMBIENT: [&str; 7] = [
    URL_VAR,
    "DEVKIT_RULES_SUPABASE_EMAIL",
    "DEVKIT_RULES_SUPABASE_PASSWORD",
    "DEVKIT_RULES_DATABASE_URL",
    "DEVKIT_CONFIG",
    "DEVKIT_ENFORCE_WRITES",
    "XDG_CONFIG_HOME",
];

struct Proj {
    _root: tempfile::TempDir,
    home: tempfile::TempDir,
    path: PathBuf,
}

impl Proj {
    fn new(toml: &str) -> Proj {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("proj");
        std::fs::create_dir_all(&path).unwrap();
        devkit_git::Git::fixture(&path)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(path.join("devkit.toml"), toml).unwrap();
        Proj {
            _root: root,
            home: tempfile::tempdir().unwrap(),
            path,
        }
    }

    /// Keeps a session for the project at `url` that is good for an hour.
    fn signed_in(&self, url: &str, token: &str) {
        let host = url.trim_start_matches("http://").replace(':', "-");
        let dir = self.home.path().join("devkit").join("supabase-sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        let session = json!({
            "access_token": token,
            "refresh_token": "r",
            "expires_at": expires_at,
        });
        std::fs::write(dir.join(format!("{host}.json")), session.to_string()).unwrap();
    }

    fn devkit(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
        cmd.args(args)
            .current_dir(&self.path)
            .env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.home.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in AMBIENT {
            cmd.env_remove(var);
        }
        testenv::scrub_identity(&mut cmd);
        cmd.envs(env.iter().copied());
        cmd.output().unwrap()
    }
}

fn config(extra: &str) -> String {
    format!("[rules]\nsource = \"supabase\"\n[rules.supabase]\nrepository = \"{REPO}\"\n{extra}")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn context_hook_reads_supabase_rules() {
    let index: RuleIndex = serde_json::from_str(INDEX).unwrap();
    let rules = fakeapi::payloads(&index);
    let server = FakeServer::start(vec![
        (200, fakeapi::stats(3)),
        (200, fakeapi::page(3, &rules, None)),
        (200, fakeapi::stats(3)),
        (200, fakeapi::page(3, &rules, None)),
    ]);
    let url = server.url();
    let p = Proj::new(&config("publishable_key = \"sb_publishable_test\"\n"));
    p.signed_in(&url, "tok");
    let env = [(URL_VAR, url.as_str())];

    let out = p.devkit(&["rules", "context"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Root must"), "{text}{}", stderr(&out));
    assert!(!text.contains("Foo should"), "{text}");

    let out = p.devkit(&["rules", "pull"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("revision 3, 6 rules"),
        "{}",
        stdout(&out)
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        assert!(
            request.path_and_query.starts_with("/rest/v1/rpc/"),
            "{request:?}"
        );
        assert_eq!(request.header("authorization"), Some("Bearer tok"));
        assert_eq!(request.header("apikey"), Some("sb_publishable_test"));
        assert_eq!(request.header("content-profile"), Some("repo_rules_api"));
    }
}

/// A hook's refresh shares one wait across every request it makes, so an API
/// that answers each request inside the wait still cannot hold the hook for a
/// wait per request. With a wait apiece, every answer would arrive and the
/// hook would take at least the sum of the delays.
#[test]
fn a_hook_refresh_gives_up_within_one_wait() {
    let index: RuleIndex = serde_json::from_str(INDEX).unwrap();
    let rules = fakeapi::payloads(&index);
    let more = Some((1, "00000000-0000-4000-8000-000000000000"));
    let answers = vec![
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &rules[..1], more)),
        (200, fakeapi::page(1, &rules[..1], more)),
        (200, fakeapi::page(1, &rules[1..], None)),
    ];
    let delay = Duration::from_millis(700);
    let every_answer = delay * u32::try_from(answers.len()).unwrap();
    let server = FakeServer::start_slow(answers.clone(), delay);
    let p = Proj::new(&config(""));
    let started = Instant::now();
    let out = p.devkit(&["rules", "context"], &[(URL_VAR, &server.url())]);
    let took = started.elapsed();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(took < every_answer, "took {took:?}");
    let asked = server.requests().len();
    assert!(
        (1..answers.len()).contains(&asked),
        "asked {asked}: {}",
        stderr(&out)
    );
}

#[test]
fn no_publishable_key_sends_no_credentials() {
    let server = FakeServer::start(vec![
        (200, fakeapi::stats(1)),
        (200, fakeapi::page(1, &[], None)),
    ]);
    let p = Proj::new(&config(""));
    let out = p.devkit(&["rules", "pull"], &[(URL_VAR, &server.url())]);
    assert!(out.status.success(), "{}", stderr(&out));
    for request in server.requests() {
        assert_eq!(request.header("authorization"), None);
        assert_eq!(request.header("apikey"), None);
    }
}

#[test]
fn not_signed_in_names_the_command() {
    let server = FakeServer::start(Vec::new());
    let p = Proj::new(&config("publishable_key = \"pk\"\n"));
    let out = p.devkit(&["rules", "pull"], &[(URL_VAR, &server.url())]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("devkit auth supabase"),
        "{}",
        stderr(&out)
    );
    assert!(server.requests().is_empty());
}

#[test]
fn project_url_is_ignored() {
    let p = Proj::new(&config("url = \"http://127.0.0.1:1\"\n"));
    let out = p.devkit(&["rules", "pull"], &[]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("[rules.supabase] url in the global config"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn doctor_shows_the_api_without_secrets() {
    let server = FakeServer::start(vec![(200, fakeapi::stats(9))]);
    let url = server.url();
    let p = Proj::new(&config("publishable_key = \"sb_publishable_secretish\"\n"));
    p.signed_in(&url, "tok-secret");
    let out = p.devkit(&["doctor", "--json"], &[(URL_VAR, &url)]);
    let rows: Vec<Value> = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}{}", stdout(&out), stderr(&out)));
    let row = |key: &str| {
        rows.iter()
            .find(|r| r["key"] == key)
            .unwrap_or_else(|| panic!("no {key} row"))
            .clone()
    };
    assert_eq!(row("rules_api_url")["source"], "env");
    assert!(
        row("rules_api_session")["detail"]
            .as_str()
            .unwrap()
            .contains("signed in")
    );
    let api = row("rules_api");
    assert_eq!(api["status"], "ok", "{api}");
    assert!(
        api["detail"].as_str().unwrap().contains("revision 9"),
        "{api}"
    );
    let text = serde_json::to_string(&rows).unwrap();
    assert!(!text.contains("tok-secret"), "{text}");
    assert!(!text.contains("sb_publishable_secretish"), "{text}");
}
