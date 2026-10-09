//! A remote rules source read through its local cache, end to end: the
//! session hook fills it, a write hook reads it with the database gone, and
//! `devkit rules pull` refreshes it. Tests that need a database read
//! `DEVKIT_TEST_POSTGRES_URL` and return early without it;
//! `crates/devkit-todo-postgres/testdb/up.sh` starts one.

#[path = "../crates/devkit-rules/tests/common/pgstore.rs"]
mod pgstore;
#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

use pgstore::TestStore;
use serde_json::{Value, json};

const DATABASE_VAR: &str = "DEVKIT_RULES_DATABASE_URL";

const EXPORT: &str = include_str!("../crates/devkit-rules/tests/fixtures/export.json");

const UNREACHABLE: &str = "postgres://x@127.0.0.1:1/x?sslmode=disable";

/// Variables a developer's shell may set that would pick the test's database
/// or config.
const AMBIENT: [&str; 5] = [
    DATABASE_VAR,
    "DEVKIT_CONFIG",
    "DEVKIT_ENFORCE_WRITES",
    "XDG_CONFIG_HOME",
    "DEVKIT_TODO_DATABASE_URL",
];

/// A git checkout with a private home and state directory.
struct Proj {
    _root: tempfile::TempDir,
    home: tempfile::TempDir,
    path: PathBuf,
}

impl Proj {
    fn new(toml: &str) -> Proj {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("proj");
        std::fs::create_dir_all(path.join("crates/foo/bar")).unwrap();
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

    fn reading(repo: &str) -> Proj {
        Proj::new(&format!(
            "[rules]\nsource = \"postgres\"\n[rules.postgres]\nrepository = \"{repo}\"\n"
        ))
    }

    fn cache_dir(&self) -> PathBuf {
        self.home.path().join("devkit").join("rules-cache")
    }

    fn run(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
        cmd.args(args)
            .current_dir(&self.path)
            .env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.home.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in AMBIENT {
            cmd.env_remove(var);
        }
        testenv::scrub_identity(&mut cmd);
        cmd.envs(env.iter().copied());
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn devkit(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.run(args, env, "")
    }

    fn write_hook(&self, target: &str, env: &[(&str, &str)]) -> Output {
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "session_id": "S",
            "cwd": self.path,
            "tool_input": { "file_path": self.path.join(target) }
        });
        self.run(
            &["hook", "pre-tool-use", "--harness", "claude-code"],
            env,
            &payload.to_string(),
        )
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn imported() -> Option<(TestStore, String)> {
    let store = TestStore::create()?;
    let repo = store.import(&serde_json::from_str(EXPORT).unwrap());
    Some((store, repo))
}

/// The database `url` names, reached through a local relay until `close`
/// takes it down, after which the relay's address refuses connections, as
/// a database that went away looks. The cache knows a database by its
/// address, so the relay keeps one address across both.
struct Relay {
    url: String,
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    accepting: JoinHandle<()>,
}

impl Relay {
    fn to(url: &str) -> Relay {
        let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
        let after_user = after_scheme
            .rsplit_once('@')
            .map_or(after_scheme, |(_, rest)| rest);
        let upstream = after_user.split(['/', '?']).next().unwrap().to_string();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let target = upstream.clone();
        let accepting = std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(server) = TcpStream::connect(&target) {
                    pipe(&client, &server);
                    pipe(&server, &client);
                }
            }
        });
        Relay {
            url: url.replacen(&upstream, &addr.to_string(), 1),
            addr,
            stop,
            accepting,
        }
    }

    fn close(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        self.accepting.join().unwrap();
    }
}

/// Copies what `from` sends to `to` until `from` closes.
fn pipe(from: &TcpStream, to: &TcpStream) {
    let (mut from, mut to) = (from.try_clone().unwrap(), to.try_clone().unwrap());
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut from, &mut to);
        let _ = to.shutdown(Shutdown::Write);
    });
}

fn cache_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

#[test]
fn session_hook_fills_the_cache_and_writes_read_it_offline() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let relay = Relay::to(&store.url);
    let out = p.devkit(&["rules", "context"], &[(DATABASE_VAR, &relay.url)]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("Root must"), "{}", stdout(&out));
    let cache = p.cache_dir().join(format!("postgres-{repo}.sqlite"));
    assert!(cache.is_file(), "{:?}", cache_files(&p.cache_dir()));

    let url = relay.url.clone();
    relay.close();
    let out = p.write_hook("crates/foo/bar/lib.rs", &[(DATABASE_VAR, &url)]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let answer: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}{}", stdout(&out), stderr(&out)));
    let text = answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(text.contains("Foo bar must"), "{text}");
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

#[test]
fn pull_prints_revision_and_count() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let out = p.devkit(&["rules", "pull"], &[(DATABASE_VAR, &store.url)]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(&repo), "{text}");
    let (_, tail) = text.split_once("revision ").expect("a revision");
    let (revision, rest) = tail.split_once(", ").expect("a count");
    revision.trim().parse::<i64>().expect("a numeric revision");
    assert!(rest.trim_end().ends_with(" rules"), "{text}");
    assert_eq!(rest.trim_end(), "10 rules", "{text}");
}

/// The revision `devkit rules pull` printed.
fn pulled_revision(out: &Output) -> i64 {
    let text = stdout(out);
    let (_, tail) = text.split_once("revision ").expect("a revision");
    let (revision, _) = tail.split_once(", ").expect("a count");
    revision.trim().parse().expect("a numeric revision")
}

#[test]
fn pull_replaces_a_cache_ahead_of_the_remote() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let env = [(DATABASE_VAR, store.url.as_str())];
    let out = p.devkit(&["rules", "pull"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let remote = pulled_revision(&out);

    let cache = p.cache_dir().join(format!("postgres-{repo}.sqlite"));
    let ahead = remote + 5;
    rusqlite::Connection::open(&cache)
        .unwrap()
        .execute("UPDATE meta SET revision = ?1", [ahead])
        .unwrap();

    let out = p.devkit(&["rules", "pull"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(pulled_revision(&out), remote, "{}", stdout(&out));
    let held: i64 = rusqlite::Connection::open(&cache)
        .unwrap()
        .query_row("SELECT revision FROM meta", [], |row| row.get(0))
        .unwrap();
    assert_eq!(held, remote);
}

#[test]
fn pull_fails_naming_an_unreachable_database() {
    let p = Proj::reading("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
    let out = p.devkit(&["rules", "pull"], &[(DATABASE_VAR, UNREACHABLE)]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("rules database 127.0.0.1:1/x"),
        "{}",
        stderr(&out)
    );
    assert!(cache_files(&p.cache_dir()).is_empty());
}

#[test]
fn pull_on_file_source_errors() {
    let p = Proj::new("[rules]\nsource = \"file\"\n");
    let out = p.devkit(&["rules", "pull"], &[]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("has no cache to pull"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn explicit_index_bypasses_cache() {
    let p = Proj::reading("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
    let index = p.path.join("index.json");
    std::fs::write(
        &index,
        include_str!("../crates/devkit-rules/tests/fixtures/index.json"),
    )
    .unwrap();
    let out = p.devkit(
        &[
            "rules",
            "query",
            index.to_str().unwrap(),
            "--format",
            "json",
        ],
        &[(DATABASE_VAR, UNREACHABLE)],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
    assert!(cache_files(&p.cache_dir()).is_empty());
}

#[test]
fn a_query_with_the_database_gone_reads_the_cache_and_says_so() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let relay = Relay::to(&store.url);
    let out = p.devkit(&["rules", "pull"], &[(DATABASE_VAR, &relay.url)]);
    assert!(out.status.success(), "{}", stderr(&out));
    let url = relay.url.clone();
    relay.close();
    let out = p.devkit(&["rules", "query", "--format", "json"], &[(
        DATABASE_VAR,
        &url,
    )]);
    assert!(out.status.success(), "{}", stderr(&out));
    let rules: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!rules.is_empty());
    assert_eq!(stderr(&out).lines().count(), 1, "{}", stderr(&out));
}

#[test]
fn doctor_reports_the_cache() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let env = [(DATABASE_VAR, store.url.as_str())];
    let row = |p: &Proj| {
        let out = p.devkit(&["doctor", "--json"], &env);
        let rows: Vec<Value> = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}{}", stdout(&out), stderr(&out)));
        rows.into_iter()
            .find(|r| r["key"] == "rules_cache")
            .expect("a rules_cache row")
    };
    assert_eq!(row(&p)["status"], "warn");
    assert!(p.devkit(&["rules", "pull"], &env).status.success());
    let cache = row(&p);
    assert_eq!(cache["status"], "ok", "{cache}");
    assert!(
        cache["detail"].as_str().unwrap().contains("revision"),
        "{cache}"
    );
}

/// A `doppler` in `dir` that counts its calls in `calls` and gives `url` as
/// the rules database URL, or fails as an offline Doppler does when `url` is
/// `None`, and the `PATH` that finds it first.
#[cfg(unix)]
fn fake_doppler(dir: &Path, url: Option<&str>) -> String {
    use std::os::unix::fs::PermissionsExt;
    let calls = dir.join("calls");
    let answer = match url {
        Some(url) => format!("echo '{}'", json!({DATABASE_VAR: {"computed": url}})),
        None => "exit 1".to_string(),
    };
    let doppler = dir.join("doppler");
    std::fs::write(
        &doppler,
        format!("#!/bin/sh\necho call >> '{}'\n{answer}\n", calls.display()),
    )
    .unwrap();
    std::fs::set_permissions(&doppler, std::fs::Permissions::from_mode(0o755)).unwrap();
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
fn doppler_calls(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("calls"))
        .map(|calls| calls.lines().count())
        .unwrap_or(0)
}

#[cfg(unix)]
#[test]
fn a_write_hook_reads_the_cache_offline_with_the_url_doppler_gave() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::new(&format!(
        "[rules]\nsource = \"postgres\"\n\
         [rules.postgres]\nrepository = \"{repo}\"\ndoppler_project = \"swarm\"\n"
    ));
    let relay = Relay::to(&store.url);
    let online = tempfile::tempdir().unwrap();
    let path = fake_doppler(online.path(), Some(&relay.url));
    let out = p.devkit(&["rules", "context"], &[("PATH", &path)]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("Root must"), "{}", stdout(&out));

    relay.close();
    let out = p.devkit(&["rules", "context"], &[("PATH", &path)]);
    assert!(out.status.success(), "{}", stderr(&out));
    let kept = p.home.path().join("devkit/rules-database-url/swarm.json");
    let hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&kept)
        .expect("the URL Doppler gave is kept while the database is unreachable")
        .set_modified(hours_ago)
        .unwrap();

    let offline = tempfile::tempdir().unwrap();
    let path = fake_doppler(offline.path(), None);
    let out = p.write_hook("crates/foo/bar/lib.rs", &[("PATH", &path)]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let answer: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}{}", stdout(&out), stderr(&out)));
    let text = answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(text.contains("Foo bar must"), "{text}");
    assert_eq!(doppler_calls(offline.path()), 0);
}
