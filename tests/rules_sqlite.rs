//! `devkit rules` and the write hook against the SQLite store
//! `repo-rules-agent` writes.
//!
//! The fixture store is `repo-rules-agent import-json source.json` dumped to
//! SQL, and `export.json` is the extractor's `export-json` of that store.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const STORE_SQL: &str = "crates/devkit-rules/tests/fixtures/sqlite/store.sql";
const EXPORT_JSON: &str = "crates/devkit-rules/tests/fixtures/sqlite/export.json";

/// The fixture store, written to `path`.
fn store_at(path: &Path) {
    let db = rusqlite::Connection::open(path).unwrap();
    // The dump creates tables alphabetically, so a membership row arrives
    // before the `rules` table its foreign key names.
    db.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    db.execute_batch(&std::fs::read_to_string(STORE_SQL).unwrap())
        .unwrap();
}

fn sql(path: &Path, statements: &str) {
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(statements)
        .unwrap();
}

/// A git repository with one commit, so it can carry worktrees.
fn repo() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    for args in [
        ["init", "-q", "-b", "main"].as_slice(),
        ["commit", "-q", "--allow-empty", "-m", "init"].as_slice(),
    ] {
        devkit_git::Git::fixture(p.path())
            .args(args.iter().copied())
            .output()
            .unwrap();
    }
    p
}

/// A linked worktree of `main` at `<parent>/wt`.
fn worktree(main: &Path, parent: &Path) -> PathBuf {
    let wt = parent.join("wt");
    devkit_git::Git::fixture(main)
        .args(["worktree", "add", "-q", "-b", "wt", wt.to_str().unwrap()])
        .output()
        .unwrap();
    wt
}

/// The extractor's cache directory for `repo` under the private `home` the
/// child runs with, laid out as platformdirs lays it out per platform.
fn cache_dir(home: &Path, repo: &Path) -> PathBuf {
    let root = if cfg!(target_os = "macos") {
        home.join("Library/Caches/repo-rules")
    } else if cfg!(windows) {
        home.join("repo-rules\\repo-rules\\Cache")
    } else {
        home.join("cache/repo-rules")
    };
    root.join(devkit_rules::index::cache_dir_name(repo))
}

fn devkit(cwd: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.current_dir(cwd)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home)
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("LOCALAPPDATA", home)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES");
    testenv::scrub_identity(&mut cmd);
    cmd
}

fn rules(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    devkit(cwd, home).arg("rules").args(args).output().unwrap()
}

fn rules_ok(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = rules(cwd, home, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn query_ids(cwd: &Path, home: &Path, args: &[&str]) -> Vec<String> {
    let mut all = vec!["query", "--format", "json", "--limit", "0"];
    all.extend_from_slice(args);
    let rules: Vec<serde_json::Value> = serde_json::from_str(&rules_ok(cwd, home, &all)).unwrap();
    rules
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

fn hook(cwd: &Path, home: &Path, target: &str) -> Output {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "session_id": "S",
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "file_path": target }
    });
    let mut child = devkit(cwd, home)
        .args(["hook", "pre-tool-use", "--harness", "claude-code"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn injected(out: &Output) -> Option<String> {
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    Some(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

fn configure(project: &Path, rules: &str) {
    std::fs::write(
        project.join("devkit.toml"),
        format!("[harness]\nenforce_writes = true\n\n[rules]\nenabled = true\n{rules}"),
    )
    .unwrap();
}

/// `stats` output without its first line, which names where it read from.
fn stats_body(out: &str) -> String {
    out.lines().skip(1).collect::<Vec<_>>().join("\n")
}

#[test]
fn a_cached_store_governs_worktree_writes_and_reads_like_its_json_export() {
    let home = tempfile::tempdir().unwrap();
    let main = repo();
    let parent = tempfile::tempdir().unwrap();
    let wt = worktree(main.path(), parent.path());
    configure(main.path(), "");
    configure(&wt, "");
    let cache = cache_dir(home.path(), main.path());
    std::fs::create_dir_all(&cache).unwrap();
    store_at(&cache.join("index.sqlite"));

    let text = injected(&hook(&wt, home.path(), "crates/foo/src/a.rs"))
        .expect("the store's rules are injected");
    assert!(text.contains("Foo should"), "{text}");
    assert!(text.contains("Root must"), "{text}");
    assert!(!text.contains("Gone"), "a tombstone: {text}");

    let export = std::fs::canonicalize(EXPORT_JSON).unwrap();
    let export = export.to_str().unwrap();
    for args in [
        &["query", "--format", "json", "--limit", "0"][..],
        &[
            "query",
            "--path",
            "crates/foo/a.rs",
            "--task",
            "code-review",
        ][..],
        &["query", "--format", "prompt", "--min-severity", "should"][..],
    ] {
        let from_store = rules_ok(&wt, home.path(), args);
        let mut with_export = args.to_vec();
        with_export.insert(1, export);
        assert_eq!(
            from_store,
            rules_ok(&wt, home.path(), &with_export),
            "{args:?}"
        );
    }
    let stats = rules_ok(&wt, home.path(), &["stats"]);
    assert!(stats.contains("index.sqlite"), "{stats}");
    assert_eq!(
        stats_body(&stats),
        stats_body(&rules_ok(&wt, home.path(), &["stats", export]))
    );

    let context = rules_ok(&wt, home.path(), &["context"]);
    assert!(context.contains("Root must"), "{context}");
    configure(&wt, &format!("index = '{export}'\n"));
    assert_eq!(context, rules_ok(&wt, home.path(), &["context"]));
}

#[test]
fn a_cached_store_wins_over_a_json_index_beside_it() {
    let home = tempfile::tempdir().unwrap();
    let main = repo();
    configure(main.path(), "");
    let cache = cache_dir(home.path(), main.path());
    std::fs::create_dir_all(&cache).unwrap();
    let json =
        serde_json::json!({"repo": main.path(), "rules": [{"id": "json-only", "title": "J"}]});
    std::fs::write(cache.join("index.json"), json.to_string()).unwrap();

    assert_eq!(query_ids(main.path(), home.path(), &[]), ["json-only"]);
    let added = rules_ok(main.path(), home.path(), &["add", "--title", "Added"]);
    let raw = std::fs::read_to_string(cache.join("index.json")).unwrap();
    assert!(raw.contains(added.trim()), "{raw}");

    store_at(&cache.join("index.sqlite"));
    let ids = query_ids(main.path(), home.path(), &[]);
    assert!(ids.contains(&"r-root-must".to_string()), "{ids:?}");
    assert!(!ids.contains(&"json-only".to_string()), "{ids:?}");
}

#[test]
fn a_configured_store_is_read_whatever_it_is_called() {
    let home = tempfile::tempdir().unwrap();
    let main = repo();
    let store = main.path().join("rules.data");
    store_at(&store);
    configure(main.path(), &format!("index = '{}'\n", store.display()));
    let ids = query_ids(main.path(), home.path(), &[]);
    assert!(ids.contains(&"r-foo-should".to_string()), "{ids:?}");
    assert!(!ids.contains(&"r-gone".to_string()), "a tombstone: {ids:?}");

    let positional = query_ids(main.path(), home.path(), &[store.to_str().unwrap()]);
    assert_eq!(positional, ids);
}

/// Every file in `dir` with its bytes and modification time.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| {
            let modified = p.metadata().unwrap().modified().unwrap();
            (p.clone(), std::fs::read(&p).unwrap(), modified)
        })
        .collect();
    entries.sort();
    entries
}

#[test]
fn a_store_that_cannot_be_read_injects_nothing_and_says_so_once() {
    enum Breakage {
        Missing,
        Sql(&'static str),
        Directory,
    }
    let breakages = [
        ("missing", "index.sqlite", Breakage::Missing),
        (
            "malformed",
            "index.sqlite",
            Breakage::Sql("UPDATE rules SET extra = 'not json' WHERE external_id = 'r-root-must';"),
        ),
        (
            "future",
            "index.sqlite",
            Breakage::Sql("UPDATE storage_version SET version = 2;"),
        ),
        (
            "WAL",
            "index.sqlite",
            Breakage::Sql("PRAGMA journal_mode = WAL;"),
        ),
        ("directory", "index.sqlite", Breakage::Directory),
        (
            "directory without extension",
            "rules.data",
            Breakage::Directory,
        ),
    ];
    for (name, file, breakage) in breakages {
        let home = tempfile::tempdir().unwrap();
        let main = repo();
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join(file);
        match breakage {
            Breakage::Missing => {}
            Breakage::Sql(statements) => {
                store_at(&store);
                sql(&store, statements);
            }
            Breakage::Directory => std::fs::create_dir(&store).unwrap(),
        }
        configure(main.path(), &format!("index = '{}'\n", store.display()));
        let before = snapshot(dir.path());

        let out = hook(main.path(), home.path(), "crates/foo/src/a.rs");
        assert_eq!(injected(&out), None, "{name}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(stderr.lines().count(), 1, "{name}: {stderr}");
        assert!(
            stderr.contains(&store.display().to_string()),
            "{name}: {stderr}"
        );
        if name == "WAL" {
            assert!(stderr.contains("WAL journal mode"), "{stderr}");
        }

        let out = rules(main.path(), home.path(), &["query"]);
        assert!(!out.status.success(), "{name}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&store.display().to_string()),
            "{name}: {stderr}"
        );

        assert_eq!(
            snapshot(dir.path()),
            before,
            "{name}: a read changed the directory"
        );
    }
}

/// The columns of the rule `id` an edit may touch, plus the ones it must not.
#[derive(Debug, PartialEq)]
struct Row {
    title: String,
    severity: String,
    pinned: bool,
    removed: bool,
    position: i64,
    fingerprint: Option<String>,
    extra: serde_json::Value,
    tasks: Vec<String>,
    languages: Vec<String>,
}

fn row(store: &Path, id: &str) -> Option<Row> {
    let db = rusqlite::Connection::open(store).unwrap();
    let found = db.query_row(
        "SELECT rule_key, title, severity, pinned, removed, position, extraction_fingerprint, extra
         FROM rules WHERE external_id = ?1",
        [id],
        |r| {
            Ok((r.get::<_, String>(0)?, Row {
                title: r.get(1)?,
                severity: r.get(2)?,
                pinned: r.get(3)?,
                removed: r.get(4)?,
                position: r.get(5)?,
                fingerprint: r.get(6)?,
                extra: serde_json::from_str(&r.get::<_, String>(7)?).unwrap(),
                tasks: Vec::new(),
                languages: Vec::new(),
            }))
        },
    );
    let (key, mut row) = match found {
        Ok(found) => found,
        Err(rusqlite::Error::QueryReturnedNoRows) => return None,
        Err(e) => panic!("{e}"),
    };
    let list = |table: &str, column: &str| -> Vec<String> {
        let mut stmt = db
            .prepare(&format!(
                "SELECT {column} FROM {table} WHERE rule_key = ?1 ORDER BY position"
            ))
            .unwrap();
        stmt.query_map([&key], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    row.tasks = list("rule_tasks", "task");
    row.languages = list("rule_languages", "language");
    Some(row)
}

/// The repository's revision and active generation.
fn repository(store: &Path) -> (i64, String) {
    rusqlite::Connection::open(store)
        .unwrap()
        .query_row(
            "SELECT revision, active_generation FROM repositories",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn configured_store() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let main = repo();
    let store = main.path().join("index.sqlite");
    store_at(&store);
    configure(main.path(), &format!("index = '{}'\n", store.display()));
    (home, main, store)
}

#[test]
fn edit_pins_the_rule_replaces_its_lists_and_bumps_only_the_revision() {
    let (home, main, store) = configured_store();
    let before = row(&store, "r-foo-should").unwrap();
    let (revision, generation) = repository(&store);

    rules_ok(main.path(), home.path(), &[
        "edit",
        "r-foo-should",
        "--title",
        "Foo must now",
        "--severity",
        "must",
        "--task",
        "code-questions",
        "--lang",
        "ts",
        "--lang",
        "rust",
    ]);

    assert_eq!(row(&store, "r-foo-should").unwrap(), Row {
        title: "Foo must now".into(),
        severity: "must".into(),
        pinned: true,
        tasks: vec!["code-questions".into()],
        languages: vec!["typescript".into(), "rust".into()],
        ..before
    });
    assert_eq!(repository(&store), (revision + 1, generation));
    let ids = query_ids(main.path(), home.path(), &["--severity", "must"]);
    assert!(ids.contains(&"r-foo-should".to_string()), "{ids:?}");
}

#[test]
fn edit_keeps_topics_and_unknown_metadata_unless_told_otherwise() {
    let (home, main, store) = configured_store();
    rules_ok(main.path(), home.path(), &[
        "edit",
        "r-foo-should",
        "--description",
        "New.",
    ]);
    assert_eq!(
        row(&store, "r-foo-should").unwrap().extra,
        serde_json::json!({"topics": ["errors"], "future_field": 7})
    );
    rules_ok(main.path(), home.path(), &[
        "edit",
        "r-foo-should",
        "--topic",
        "Perf",
    ]);
    assert_eq!(
        row(&store, "r-foo-should").unwrap().extra,
        serde_json::json!({"topics": ["perf"], "future_field": 7})
    );
}

#[test]
fn remove_tombstones_an_extracted_rule_and_deletes_an_added_one() {
    let (home, main, store) = configured_store();
    let before = row(&store, "r-root-must").unwrap();
    let (revision, generation) = repository(&store);

    rules_ok(main.path(), home.path(), &["remove", "r-root-must"]);
    assert_eq!(row(&store, "r-root-must").unwrap(), Row {
        pinned: true,
        removed: true,
        ..before
    });
    assert_eq!(repository(&store), (revision + 1, generation.clone()));
    assert!(!query_ids(main.path(), home.path(), &[]).contains(&"r-root-must".to_string()));
    assert!(
        !rules(main.path(), home.path(), &["remove", "r-root-must"])
            .status
            .success()
    );

    let id = rules_ok(main.path(), home.path(), &[
        "add",
        "--title",
        "Short-lived",
        "--task",
        "code-review",
        "--lang",
        "rs",
    ]);
    rules_ok(main.path(), home.path(), &["rm", id.trim()]);
    assert_eq!(row(&store, id.trim()), None);
    let orphans: i64 = rusqlite::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM rule_tasks WHERE rule_key NOT IN (SELECT rule_key FROM rules))
                  + (SELECT count(*) FROM rule_languages WHERE rule_key NOT IN (SELECT rule_key FROM rules))",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphans, 0);
    assert_eq!(repository(&store), (revision + 3, generation));
}

/// `sha256("<manual>:Log with tracing")[:12]`, the extractor's id for a rule
/// with that title in its synthetic `<manual>` source.
const MANUAL_ID: &str = "4709cf44ad95";

#[test]
fn add_puts_a_pinned_rule_in_the_manual_source_after_every_other() {
    let (home, main, store) = configured_store();
    let (revision, generation) = repository(&store);
    let id = rules_ok(main.path(), home.path(), &[
        "add",
        "--title",
        "Log with tracing",
        "--severity",
        "must",
        "--task",
        "code-generation",
        "--lang",
        "rs",
        "--directory",
        "crates/foo",
    ]);
    assert_eq!(id.trim(), MANUAL_ID);
    let added = row(&store, MANUAL_ID).unwrap();
    assert_eq!(added, Row {
        title: "Log with tracing".into(),
        severity: "must".into(),
        pinned: true,
        removed: false,
        position: 7,
        fingerprint: None,
        extra: serde_json::json!({"topics": []}),
        tasks: vec!["code-generation".into()],
        languages: vec!["rust".into()],
    });
    let (source, tier, discovered): (String, i64, bool) = rusqlite::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT s.path, s.tier, s.discovered FROM rules r JOIN sources s
             ON s.repo_id = r.repo_id AND s.source_key = r.source_key
             WHERE r.external_id = ?1",
            [MANUAL_ID],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((source.as_str(), tier, discovered), ("<manual>", 0, false));
    assert_eq!(repository(&store), (revision + 1, generation));
    assert!(
        query_ids(main.path(), home.path(), &["--path", "crates/foo/a.rs"])
            .contains(&MANUAL_ID.to_string())
    );

    let out = rules(main.path(), home.path(), &[
        "add",
        "--title",
        "Log with tracing",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("already exists"));
}

#[test]
fn a_failed_edit_rolls_back_whole() {
    let (home, main, store) = configured_store();
    sql(
        &store,
        "CREATE TRIGGER refuse BEFORE UPDATE ON repositories BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    );
    let before = row(&store, "r-foo-should").unwrap();
    let (revision, generation) = repository(&store);
    for args in [
        &[
            "edit",
            "r-foo-should",
            "--title",
            "Changed",
            "--task",
            "code-questions",
        ][..],
        &["remove", "r-foo-should"][..],
        &["add", "--title", "Never"][..],
    ] {
        let out = rules(main.path(), home.path(), args);
        assert!(!out.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("refused"),
            "{out:?}"
        );
    }
    assert_eq!(row(&store, "r-foo-should").unwrap(), before);
    assert_eq!(repository(&store), (revision, generation));
    let manual: i64 = rusqlite::Connection::open(&store)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sources WHERE path = '<manual>'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(manual, 0);
}

#[test]
fn an_id_several_rules_share_is_refused_naming_each() {
    let (home, main, store) = configured_store();
    sql(
        &store,
        "UPDATE rules SET external_id = 'r-root-must' WHERE external_id = 'r-untagged';",
    );
    for args in [
        &["edit", "r-root-must", "--title", "x"][..],
        &["remove", "r-root-must"][..],
    ] {
        let out = rules(main.path(), home.path(), args);
        assert!(!out.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Root must") && stderr.contains("Untagged"),
            "{stderr}"
        );
    }
    assert!(!row_pinned(&store, "Root must"));
}

fn row_pinned(store: &Path, title: &str) -> bool {
    rusqlite::Connection::open(store)
        .unwrap()
        .query_row("SELECT pinned FROM rules WHERE title = ?1", [title], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn an_edit_refuses_a_store_outside_rollback_journaling() {
    let (home, main, store) = configured_store();
    sql(&store, "PRAGMA journal_mode = WAL;");
    let out = rules(main.path(), home.path(), &["remove", "r-root-must"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("WAL journal mode"), "{stderr}");
    let mode: String = rusqlite::Connection::open(&store)
        .unwrap()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert!(!row_pinned(&store, "Root must"));
}
