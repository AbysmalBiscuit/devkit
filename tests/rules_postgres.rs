//! `[rules] source = "postgres"` through `devkit rules`, the write hook and
//! `devkit doctor`, against `repo-rules-agent`'s query schema in a disposable
//! database. Tests that need a database read `DEVKIT_TEST_POSTGRES_URL` and
//! return early without it; `crates/devkit-todo-postgres/testdb/up.sh` starts
//! one. The URL-resolution tests need no database.

#[path = "common/pgstall.rs"]
mod pgstall;
#[path = "../crates/devkit-rules/tests/common/pgstore.rs"]
mod pgstore;
#[path = "common/testenv.rs"]
mod testenv;

#[cfg(unix)]
use std::path::Path;
use std::{
    io::Write,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

use pgstore::TestStore;
use serde_json::{Value, json};

const DATABASE_VAR: &str = "DEVKIT_RULES_DATABASE_URL";

const EXPORT: &str = include_str!("../crates/devkit-rules/tests/fixtures/export.json");

/// Variables a developer's shell may set that would pick the test's database
/// or config.
const AMBIENT: [&str; 5] = [
    DATABASE_VAR,
    "DEVKIT_CONFIG",
    "DEVKIT_ENFORCE_WRITES",
    "XDG_CONFIG_HOME",
    "DEVKIT_TODO_DATABASE_URL",
];

/// A git checkout with a private home, its rules read from Postgres.
struct Proj {
    _root: tempfile::TempDir,
    home: tempfile::TempDir,
    path: PathBuf,
}

impl Proj {
    /// A checkout whose `devkit.toml` is `toml`.
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

    /// A checkout reading repository `repo` from the database.
    fn reading(repo: &str) -> Proj {
        Proj::new(&postgres_config(repo))
    }

    /// Writes `body` to `relative` under the private home.
    fn home_file(&self, relative: &str, body: &str) {
        let path = self.home.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
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
        cmd
    }

    fn start(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Child {
        let mut child = self.command(args, env).spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child
    }

    fn devkit(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.start(args, env, "").wait_with_output().unwrap()
    }

    /// The write hook for a `Write` to `target`.
    fn write_hook(&self, target: &str, env: &[(&str, &str)]) -> Output {
        self.write_hook_as("S", target, env)
    }

    /// [`Proj::write_hook`] from session `session`, which has seen no rule
    /// yet when it is new.
    fn write_hook_as(&self, session: &str, target: &str, env: &[(&str, &str)]) -> Output {
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Write",
            "session_id": session,
            "cwd": self.path,
            "tool_input": { "file_path": self.path.join(target) }
        });
        self.start(
            &["hook", "pre-tool-use", "--harness", "claude-code"],
            env,
            &payload.to_string(),
        )
        .wait_with_output()
        .unwrap()
    }

    /// `devkit doctor --json`'s row `key`.
    fn doctor_row(&self, key: &str, env: &[(&str, &str)]) -> Value {
        let out = self.devkit(&["doctor", "--json"], env);
        let rows: Vec<Value> = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}{}", stdout(&out), stderr(&out)));
        rows.into_iter()
            .find(|r| r["key"] == key)
            .unwrap_or_else(|| panic!("no {key} row"))
    }
}

fn postgres_config(repo: &str) -> String {
    format!("[rules]\nsource = \"postgres\"\n[rules.postgres]\nrepository = \"{repo}\"\n")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A database holding the fixture's records, and the repository's UUID.
fn imported() -> Option<(TestStore, String)> {
    let store = TestStore::create()?;
    let repo = store.import(&serde_json::from_str(EXPORT).unwrap());
    Some((store, repo))
}

/// The `additionalContext` a hook injected, or `None` when it printed nothing.
fn injected(out: &Output) -> Option<String> {
    assert_eq!(out.status.code(), Some(0), "{}", stderr(out));
    let text = stdout(out);
    if text.trim().is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(&text).expect("one JSON object");
    Some(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

fn ids(out: &Output) -> Vec<String> {
    assert!(out.status.success(), "{}", stderr(out));
    let rules: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    rules
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_write_injects_the_rules_the_database_holds() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let env = [(DATABASE_VAR, store.url.as_str())];
    let text = injected(&p.write_hook("crates/foo/bar/lib.rs", &env))
        .expect("a write under crates/foo/bar injects its rules");
    for title in ["Foo bar must", "Root must", "Foo should"] {
        assert!(text.contains(title), "{title}: {text}");
    }
    for title in ["Removed by hand", "Foo can", "Review only"] {
        assert!(!text.contains(title), "{title}: {text}");
    }
}

#[test]
fn query_stats_and_context_print_the_rules_the_database_holds() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let env = [(DATABASE_VAR, store.url.as_str())];

    let out = p.devkit(
        &[
            "rules",
            "query",
            "--path",
            "crates/foo/bar/lib.rs",
            "--format",
            "json",
        ],
        &env,
    );
    let found = ids(&out);
    assert_eq!(found[..2], ["r-foo-bar-must", "r-foo-should"], "{found:?}");
    assert!(!found.contains(&"r-gone".to_string()), "{found:?}");

    let out = p.devkit(&["rules", "stats"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains(&format!("repository {repo}")), "{text}");
    assert!(text.contains("10 rules across 3 files"), "{text}");
    assert!(text.contains("crates/foo/AGENTS.md: chunk 3"), "{text}");

    let out = p.devkit(&["rules", "context"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("Root must"), "{text}");
    assert!(!text.contains("Removed by hand"), "{text}");
}

/// A rule's row: pinned, removed, its source's path, its topics, its lists in
/// order, and its fingerprint.
fn rule_row(store: &TestStore, repo: &str, title: &str) -> Value {
    let rows = store.query(
        "SELECT r.pinned, r.removed, s.path, (r.extra->'topics')::text,
                ARRAY(SELECT task FROM repo_rules.rule_tasks t
                      WHERE t.rule_key = r.rule_key ORDER BY position),
                ARRAY(SELECT language FROM repo_rules.rule_languages l
                      WHERE l.rule_key = r.rule_key ORDER BY position),
                r.extraction_fingerprint, r.external_id
         FROM repo_rules.rules r JOIN repo_rules.sources s USING (repo_id, source_key)
         WHERE r.repo_id = $1::text::uuid AND r.title = $2",
        &[&repo, &title],
    );
    let row = &rows[0];
    json!({
        "pinned": row.get::<_, bool>(0),
        "removed": row.get::<_, bool>(1),
        "source": row.get::<_, String>(2),
        "topics": serde_json::from_str::<Value>(row.get(3)).unwrap(),
        "tasks": row.get::<_, Vec<String>>(4),
        "languages": row.get::<_, Vec<String>>(5),
        "fingerprint": row.get::<_, Option<String>>(6),
        "id": row.get::<_, String>(7),
    })
}

fn revision(store: &TestStore, repo: &str) -> i64 {
    store.query(
        "SELECT revision FROM repo_rules.repositories WHERE repo_id = $1::text::uuid",
        &[&repo],
    )[0]
    .get(0)
}

#[test]
fn add_edit_and_remove_pin_replace_lists_tombstone_and_bump_the_revision() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let env = [(DATABASE_VAR, store.url.as_str())];
    let before = revision(&store, &repo);

    let out = p.devkit(
        &[
            "rules",
            "add",
            "--title",
            "Log with tracing",
            "--severity",
            "must",
            "--task",
            "code-generation",
            "--lang",
            "Rust",
            "--topic",
            "Observability",
        ],
        &env,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    // Python's `hashlib.sha256(b"<manual>:Log with tracing").hexdigest()[:12]`.
    assert_eq!(stdout(&out).trim(), "4709cf44ad95");
    assert_eq!(
        rule_row(&store, &repo, "Log with tracing"),
        json!({
            "pinned": true, "removed": false, "source": "<manual>", "topics": ["observability"],
            "tasks": ["code-generation"], "languages": ["rust"], "fingerprint": null,
            "id": "4709cf44ad95",
        })
    );
    assert_eq!(revision(&store, &repo), before + 1);

    let out = p.devkit(
        &[
            "rules",
            "edit",
            "r-style-pinned",
            "--lang",
            "go",
            "--lang",
            "python",
            "--task",
            "code-questions",
        ],
        &env,
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        rule_row(&store, &repo, "Style, edited by hand"),
        json!({
            "pinned": true, "removed": false, "source": "docs/STYLE.md", "topics": ["style"],
            "tasks": ["code-questions"], "languages": ["go", "python"],
            "fingerprint": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "id": "r-style-pinned",
        })
    );
    assert_eq!(revision(&store, &repo), before + 2);

    let out = p.devkit(&["rules", "remove", "r-root-must"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    let row = rule_row(&store, &repo, "Root must");
    assert_eq!(
        (&row["pinned"], &row["removed"]),
        (&json!(true), &json!(true))
    );
    assert_eq!(revision(&store, &repo), before + 3);
    let found = ids(&p.devkit(
        &["rules", "query", "--format", "json", "--limit", "0"],
        &env,
    ));
    assert!(!found.contains(&"r-root-must".to_string()), "{found:?}");

    // Two live records share `r-dup`: the edit refuses, and its transaction
    // leaves the revision where it was.
    let out = p.devkit(&["rules", "edit", "r-dup", "--severity", "must"], &env);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("2 rules"), "{}", stderr(&out));
    assert_eq!(revision(&store, &repo), before + 3);
}

/// An edit waits on another writer's lock on the repository row before it
/// reads the rule, so it builds on what that writer committed: neither the
/// other writer's change nor either revision bump is lost.
#[test]
fn concurrent_edits_to_one_repository_serialize_on_its_row() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let before = revision(&store, &repo);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (mut client, connection) = tokio_postgres::connect(&store.url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        let tx = client.transaction().await.unwrap();
        tx.execute(
            "SELECT 1 FROM repo_rules.repositories WHERE repo_id = $1::text::uuid FOR UPDATE",
            &[&repo],
        )
        .await
        .unwrap();
        tx.execute(
            "UPDATE repo_rules.repositories SET revision = revision + 1
             WHERE repo_id = $1::text::uuid",
            &[&repo],
        )
        .await
        .unwrap();
        tx.execute(
            "UPDATE repo_rules.rules SET title = 'Foo can, renamed elsewhere'
             WHERE repo_id = $1::text::uuid AND external_id = 'r-foo-can'",
            &[&repo],
        )
        .await
        .unwrap();

        // Statistics views hold one snapshot per transaction, so the watching
        // happens on a connection of its own.
        let (watcher, connection) = tokio_postgres::connect(&store.url, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(connection);
        let child = p.start(
            &["rules", "edit", "r-foo-can", "--severity", "should"],
            &[(DATABASE_VAR, store.url.as_str())],
            "",
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let waiting: i64 = watcher
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity
                     WHERE datname = current_database() AND wait_event_type = 'Lock'",
                    &[],
                )
                .await
                .unwrap()
                .get(0);
            if waiting == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the edit never waited on the lock"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tx.commit().await.unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", stderr(&out));
    });
    assert_eq!(revision(&store, &repo), before + 2);
    let rows = store.query(
        "SELECT title, severity FROM repo_rules.rules
         WHERE repo_id = $1::text::uuid AND external_id = 'r-foo-can'",
        &[&repo],
    );
    let (title, severity): (String, String) = (rows[0].get(0), rows[0].get(1));
    assert_eq!(
        (title.as_str(), severity.as_str()),
        ("Foo can, renamed elsewhere", "should")
    );
}

/// The database or store each failure below is made of, and the cause the
/// reader names.
fn assert_fails_naming(p: &Proj, env: &[(&str, &str)], cause: &str) {
    let started = Instant::now();
    let out = p.write_hook("crates/foo/bar/lib.rs", env);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{cause}: the hook took {:?}",
        started.elapsed()
    );
    assert_eq!(injected(&out), None, "{cause}");
    let err = stderr(&out);
    assert_eq!(err.lines().count(), 1, "{cause}: {err}");
    assert!(err.contains(cause), "{cause}: {err}");

    let out = p.devkit(&["rules", "query"], env);
    assert!(!out.status.success(), "{cause}");
    assert!(stderr(&out).contains(cause), "{cause}: {}", stderr(&out));
}

#[test]
fn an_unreachable_database_injects_nothing_and_fails_the_query() {
    let refused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = refused.local_addr().unwrap();
    drop(refused);
    let p = Proj::reading("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
    let url = format!("postgres://agent:hunter2@{addr}/rules?sslmode=disable");
    assert_fails_naming(
        &p,
        &[(DATABASE_VAR, &url)],
        &format!("rules database {addr}"),
    );
}

#[test]
fn a_stalled_database_injects_nothing_within_the_hooks_wait() {
    let stalled = pgstall::Stalled::start();
    let p = Proj::reading("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
    let url = format!("postgres://agent@{}/rules?sslmode=disable", stalled.addr);
    let started = Instant::now();
    let out = p.write_hook("crates/foo/bar/lib.rs", &[(DATABASE_VAR, &url)]);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(injected(&out), None);
    let err = stderr(&out);
    assert_eq!(err.lines().count(), 1, "{err}");
    assert!(err.contains("no answer within 1s"), "{err}");
}

/// `devkit brief` with `args`, as session `S`'s hook runs it.
fn brief(p: &Proj, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut all = vec!["brief"];
    all.extend_from_slice(args);
    p.start(&all, env, r#"{"session_id":"S"}"#)
        .wait_with_output()
        .unwrap()
}

#[test]
fn the_brief_asks_a_stalled_database_once_per_run() {
    for args in [&[][..], &["--if-changed"][..]] {
        let stalled = pgstall::Stalled::start();
        let p = Proj::reading("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
        let url = format!(
            "postgres://agent@{}/{}?sslmode=disable",
            stalled.addr, stalled.database
        );
        let started = Instant::now();
        let out = brief(&p, args, &[(DATABASE_VAR, &url)]);
        let elapsed = started.elapsed();
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        assert_eq!(stalled.connections(), 1, "{args:?}");
        assert!(elapsed < Duration::from_secs(2), "{args:?}: {elapsed:?}");
        assert!(!stdout(&out).contains("### Rules"), "{args:?}");
        let err = stderr(&out);
        assert_eq!(err.lines().count(), 1, "{args:?}: {err}");
    }
}

#[test]
fn the_brief_names_the_rules_a_reachable_database_holds() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let out = brief(&p, &[], &[(DATABASE_VAR, store.url.as_str())]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("### Rules"), "{}", stdout(&out));
}

#[test]
fn a_refused_login_injects_nothing_and_fails_the_query() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let wrong = store
        .url
        .replacen("postgres:postgres@", "postgres:wrong@", 1);
    assert_ne!(wrong, store.url, "the test URL carries postgres:postgres");
    assert_fails_naming(
        &Proj::reading(&repo),
        &[(DATABASE_VAR, &wrong)],
        "password authentication failed",
    );
}

#[test]
fn an_unknown_repository_injects_nothing_and_fails_the_query() {
    let Some((store, _)) = imported() else {
        return;
    };
    let unknown = "0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10";
    assert_fails_naming(
        &Proj::reading(unknown),
        &[(DATABASE_VAR, &store.url)],
        &format!("no repository {unknown}"),
    );
}

#[test]
fn an_unsupported_storage_version_injects_nothing_and_fails_the_query() {
    let Some((store, repo)) = imported() else {
        return;
    };
    pgstore::batch(
        &store.url,
        "UPDATE repo_rules.storage_version SET version = 2",
    );
    assert_fails_naming(
        &Proj::reading(&repo),
        &[(DATABASE_VAR, &store.url)],
        "storage version 2",
    );
}

#[test]
fn doctor_reports_the_source_repository_and_connection_without_the_url() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = Proj::reading(&repo);
    let env = [(DATABASE_VAR, store.url.as_str())];
    let source = p.doctor_row("rules_source", &env);
    assert_eq!(source["name"], "postgres", "{source}");
    assert_eq!(source["kind"], "postgres", "{source}");
    assert!(
        source["location"].as_str().unwrap().contains(&repo),
        "{source}"
    );
    let database = p.doctor_row("rules_database", &env);
    assert_eq!(database["source"], "env", "{database}");
    assert_eq!(database["status"], "ok", "{database}");
    let detail = database["detail"].as_str().unwrap();
    assert!(detail.contains("connected"), "{detail}");
    assert!(detail.contains(&repo), "{detail}");
    assert!(detail.contains("10 rules"), "{detail}");
    let text = format!("{source}{database}");
    assert!(!text.contains("postgres:postgres"), "{text}");
}

/// A port nothing listens on, as `127.0.0.1:<port>`.
#[cfg(unix)]
fn refused_addr() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().to_string()
}

/// A `doppler` on a fresh `PATH` that gives `url` as the rules database URL
/// and counts its calls in `calls`.
#[cfg(unix)]
fn fake_doppler(dir: &Path, url: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let doppler = dir.join("doppler");
    let body = json!({DATABASE_VAR: {"computed": url}}).to_string();
    let calls = dir.join("calls");
    std::fs::write(
        &doppler,
        format!(
            "#!/bin/sh\necho call >> '{}'\necho '{body}'\n",
            calls.display()
        ),
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
    std::fs::read_to_string(dir.join("calls")).map_or(0, |calls| calls.lines().count())
}

/// A checkout reading `repo` with its URL in the `rules` Doppler project.
#[cfg(unix)]
fn doppler_proj(repo: &str) -> Proj {
    Proj::new(&format!(
        "{}doppler_project = \"rules\"\n",
        postgres_config(repo)
    ))
}

/// Session start asks Doppler and keeps its URL, and write hooks reuse the
/// kept URL without asking Doppler at all.
#[cfg(unix)]
#[test]
fn hooks_reuse_the_url_doppler_gave() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let p = doppler_proj(&repo);
    let bin = tempfile::tempdir().unwrap();
    let path = fake_doppler(bin.path(), &store.url);
    let env = [("PATH", path.as_str())];
    let out = p.devkit(&["rules", "context"], &env);
    assert!(out.status.success(), "{}", stderr(&out));
    for session in ["S1", "S2", "S3"] {
        let out = p.write_hook_as(session, "crates/foo/bar/lib.rs", &env);
        assert!(injected(&out).is_some(), "{}", stderr(&out));
    }
    assert_eq!(doppler_calls(bin.path()), 1);
}

/// A write hook with no kept URL injects nothing rather than wait on
/// Doppler.
#[cfg(unix)]
#[test]
fn a_write_hook_never_asks_doppler() {
    let p = doppler_proj("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
    let bin = tempfile::tempdir().unwrap();
    let url = format!(
        "postgres://agent:pw@{}/rules?sslmode=disable",
        refused_addr()
    );
    let path = fake_doppler(bin.path(), &url);
    let out = p.write_hook("crates/foo/bar/lib.rs", &[("PATH", path.as_str())]);
    assert_eq!(injected(&out), None);
    assert_eq!(doppler_calls(bin.path()), 0);
}

/// A database that does not answer leaves the kept URL in place, since the
/// write hooks find their cache through it, and session start reuses it.
#[cfg(unix)]
#[test]
fn an_unreachable_database_keeps_the_url_doppler_gave() {
    let p = doppler_proj("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
    let bin = tempfile::tempdir().unwrap();
    let url = format!(
        "postgres://agent:pw@{}/rules?sslmode=disable",
        refused_addr()
    );
    let path = fake_doppler(bin.path(), &url);
    for _ in 0..2 {
        p.devkit(&["rules", "context"], &[("PATH", path.as_str())]);
    }
    assert_eq!(doppler_calls(bin.path()), 1);
}

/// A server that answers every startup with an error carrying `sqlstate`, as
/// one with no free connection slot answers with `53300`. Returns its
/// `127.0.0.1:<port>`.
#[cfg(unix)]
fn refusing_addr(sqlstate: &'static str) -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut len = [0; 4];
            if stream.read_exact(&mut len).is_err() {
                continue;
            }
            let mut rest = vec![0; (u32::from_be_bytes(len) as usize).saturating_sub(4)];
            if stream.read_exact(&mut rest).is_err() {
                continue;
            }
            let mut fields = Vec::new();
            for (kind, value) in [(b'S', "FATAL"), (b'C', sqlstate), (b'M', "refused")] {
                fields.push(kind);
                fields.extend_from_slice(value.as_bytes());
                fields.push(0);
            }
            fields.push(0);
            let mut message = vec![b'E'];
            message.extend_from_slice(&(fields.len() as u32 + 4).to_be_bytes());
            message.extend_from_slice(&fields);
            let _ = stream.write_all(&message);
        }
    });
    addr
}

/// A server too busy to take the connection says nothing about the URL, so
/// the kept URL stays and session start reuses it.
#[cfg(unix)]
#[test]
fn a_database_out_of_connections_keeps_the_url_doppler_gave() {
    let p = doppler_proj("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
    let bin = tempfile::tempdir().unwrap();
    let url = format!(
        "postgres://agent:pw@{}/rules?sslmode=disable",
        refusing_addr("53300")
    );
    let path = fake_doppler(bin.path(), &url);
    for _ in 0..2 {
        let out = p.devkit(&["rules", "context"], &[("PATH", path.as_str())]);
        assert!(stderr(&out).contains("refused"), "{}", stderr(&out));
    }
    assert_eq!(doppler_calls(bin.path()), 1);
}

/// A login the database refuses drops the kept URL, so the next session
/// asks Doppler for a rotated credential.
#[cfg(unix)]
#[test]
fn a_refused_login_asks_doppler_again() {
    let Some((store, repo)) = imported() else {
        return;
    };
    let wrong = store
        .url
        .replacen("postgres:postgres@", "postgres:wrong@", 1);
    assert_ne!(wrong, store.url, "the test URL carries postgres:postgres");
    let p = doppler_proj(&repo);
    let bin = tempfile::tempdir().unwrap();
    let path = fake_doppler(bin.path(), &wrong);
    for _ in 0..2 {
        p.devkit(&["rules", "context"], &[("PATH", path.as_str())]);
    }
    assert_eq!(doppler_calls(bin.path()), 2);
}

/// The URL resolves from the environment, then Doppler, then the secrets
/// file: each case's doctor row names where it came from and the database
/// it reached.
#[cfg(unix)]
#[test]
fn the_url_resolves_from_the_environment_then_doppler_then_the_secrets_file() {
    let [env_addr, doppler_addr, file_addr] = [refused_addr(), refused_addr(), refused_addr()];
    let url =
        |addr: &str, db: &str| format!("postgres://agent:hunter2@{addr}/{db}?sslmode=disable");
    let p = doppler_proj("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10");
    p.home_file(
        ".config/devkit/secrets.toml",
        &format!(
            "devkit_rules_database_url = \"{}\"\n",
            url(&file_addr, "fromfile")
        ),
    );
    let bin = tempfile::tempdir().unwrap();
    let path = fake_doppler(bin.path(), &url(&doppler_addr, "fromdoppler"));
    let from_env = url(&env_addr, "fromenv");

    for (env, source, db) in [
        (
            vec![("PATH", path.as_str()), (DATABASE_VAR, from_env.as_str())],
            "env",
            "fromenv",
        ),
        (vec![("PATH", path.as_str())], "doppler", "fromdoppler"),
        (vec![], "file", "fromfile"),
    ] {
        let row = p.doctor_row("rules_database", &env);
        assert_eq!(row["source"], source, "{row}");
        assert!(row.to_string().contains(&format!("/{db}")), "{row}");
        assert!(!row.to_string().contains("hunter2"), "{row}");
    }
}

#[test]
fn a_project_layer_cannot_name_the_ca_file() {
    let global_ca = "/nonexistent/global-ca.crt";
    let p = Proj::new(&format!(
        "{}ca_file = \"/nonexistent/project-ca.crt\"\n",
        postgres_config("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10")
    ));
    p.home_file(
        ".config/devkit/config.toml",
        &format!("[rules.postgres]\nca_file = \"{global_ca}\"\n"),
    );
    let row = p.doctor_row("rules_database", &[(
        DATABASE_VAR,
        "postgres://agent@127.0.0.1:1/rules",
    )]);
    let detail = row.to_string();
    assert!(!detail.contains("project-ca.crt"), "{detail}");
    assert!(
        detail.contains(global_ca),
        "the global CA file is read: {detail}"
    );
}

#[test]
fn revision_counts_each_edit() {
    use devkit_rules::{edit::Fields, remote::RemoteRules, source::RuleSource};

    let Some((store, repo)) = imported() else {
        return;
    };
    let db = devkit_postgres::Database::new(
        &store.url,
        Duration::from_secs(10),
        &devkit_common::tls::Trust::default(),
        "rules database",
    )
    .unwrap();
    let source = devkit_rules::postgres::PostgresSource::new(
        std::sync::Arc::new(db),
        Some(&repo),
        std::path::Path::new("/srv/acme"),
    );
    let before = source.revision().unwrap();
    assert_eq!(before, revision(&store, &repo));
    let fields = Fields {
        title: Some("Counted".to_string()),
        ..Fields::default()
    };
    source.add("", fields).unwrap();
    assert_eq!(source.revision().unwrap(), before + 1);
    assert_eq!(source.pull().unwrap().0, before + 1);
}
