//! End-to-end `devkit brief`: what a session-start hook actually receives.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// `config::resolve` requires `[defaults]` after merging every layer; a
/// project `devkit.toml` that carries only `[brief]`/`[tasks]` needs this
/// alongside them or resolution fails and `[brief]` settings never take
/// effect.
const DEFAULTS: &str = "[defaults]\nworktree_root = \"/w\"\nbranch_prefix = \"x/\"\nbaseline_ref = \"r\"\nbaseline_path = \"/b\"\n\n";

struct Project {
    root: PathBuf,
    home: PathBuf,
}

impl Project {
    /// A git checkout with a docs-only devkit.toml and a Cargo lockfile that
    /// declares `serde`, plus a global docs manifest registering it.
    fn docs_only(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("devkit-brief-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .output()
                .unwrap();
        }
        write(
            &repo.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1.0.200\"\n",
        );
        write(
            &repo.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"serde\"]\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"aa\"\n",
        );
        // Nothing devrun can use: no [defaults], no [apps].
        write(&repo.join("devkit.toml"), "[config]\nroot = true\n");
        write(
            &home.join(".config/devkit/docs.toml"),
            "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n",
        );
        Project { root: repo, home }
    }

    fn brief(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_devkit"))
            .arg("brief")
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            // Isolate the state and data dirs so a test never reads or writes
            // the machine's real registry, watermarks, or docs cache. `devkit
            // brief` doesn't reach the data dir today, but isolating only one
            // of the two helpers invites the next caller to copy the wrong
            // one.
            .env("XDG_STATE_HOME", self.home.join("state"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("COLUMNS", "100")
            .output()
            .unwrap()
    }

    fn docm(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_docm"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_STATE_HOME", self.home.join("state"))
            // `docm`'s main() runs upgrade::run(cache::docs_root()) before
            // dispatching most subcommands, and docs_root() resolves through
            // XDG_DATA_HOME when the parent process has it set — without
            // this, the migration runs against the developer's real cache.
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("COLUMNS", "100")
            .output()
            .unwrap()
    }

    fn set_config(&self, body: &str) {
        write(&self.root.join("devkit.toml"), body);
    }
}

/// `brief` with a hook payload on stdin. The no-stdin path never reaches the
/// watermark: `session_id` reads stdin only when it is not a terminal and the
/// payload carries an id, so a run driven by `output()` with an inherited
/// stdin exercises a different branch entirely.
fn brief_with_stdin(project: &Project, args: &[&str], stdin: &str, columns: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("brief")
        .args(args)
        .current_dir(&project.root)
        .env("HOME", &project.home)
        .env("USERPROFILE", &project.home)
        .env("XDG_STATE_HOME", project.home.join("state"))
        .env("XDG_DATA_HOME", project.home.join("data"))
        .env("COLUMNS", columns)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn pins_only_emits_just_the_library_section() {
    let project = Project::docs_only("pins-only");
    project.set_config(&format!(
        "[config]\nroot = true\n\n{DEFAULTS}[tasks.check]\nrun = [\"cargo\", \"test\"]\ndescription = \"tests\"\n"
    ));
    let out = project.brief(&["--pins-only"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("serde"), "{text}");
    assert!(!text.contains("tests"), "no tasks section: {text}");
    assert!(!text.contains("devrun up"), "no devrun preamble: {text}");
}

#[test]
fn the_two_emission_modes_are_mutually_exclusive() {
    // The watermark records the whole brief. Honouring both flags at once
    // would stamp it after emitting only the library table, and the next
    // full --if-changed run would suppress a brief the session never saw.
    let project = Project::docs_only("modes-exclusive");
    let out = project.brief(&["--pins-only", "--if-changed"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn if_changed_emits_once_per_session_and_ignores_width() {
    let project = Project::docs_only("watermark");
    let session = r#"{"session_id":"abc-123"}"#;

    let first = brief_with_stdin(&project, &["--if-changed"], session, "100");
    assert!(!first.stdout.is_empty(), "first emission");

    // Same content, different terminal width: the digest is over data, not
    // rendered text, so this must stay silent.
    let second = brief_with_stdin(&project, &["--if-changed"], session, "60");
    assert!(
        second.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&second.stdout)
    );

    // A brief whose tasks changed while its pins held still must emit again.
    project.set_config(&format!(
        "[config]\nroot = true\n\n{DEFAULTS}[tasks.check]\nrun = [\"cargo\", \"test\"]\ndescription = \"tests\"\n"
    ));
    let third = brief_with_stdin(&project, &["--if-changed"], session, "100");
    assert!(!third.stdout.is_empty(), "content changed, emit again");
}

#[test]
fn a_full_brief_is_not_repeated_to_the_session_that_received_it() {
    // SessionStart runs the bare brief and CwdChanged runs `--if-changed` in
    // the same checkout. Without a stamp on the bare path the second call
    // finds no watermark and re-emits everything the first already delivered.
    let project = Project::docs_only("watermark-session-start");
    let session = r#"{"session_id":"start-then-cd"}"#;

    let start = brief_with_stdin(&project, &[], session, "100");
    assert!(!start.stdout.is_empty(), "the session received a brief");

    let changed = brief_with_stdin(&project, &["--if-changed"], session, "100");
    assert!(
        changed.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&changed.stdout)
    );

    // A session that has not received it still does.
    let other = brief_with_stdin(
        &project,
        &["--if-changed"],
        r#"{"session_id":"cd-only"}"#,
        "100",
    );
    assert!(!other.stdout.is_empty());
}

#[test]
fn a_pins_only_emission_leaves_the_full_brief_owed() {
    // `--pins-only` carries neither the apps, tasks nor server sections, so
    // stamping after it would suppress a full brief the session never saw.
    let project = Project::docs_only("watermark-pins-only");
    let session = r#"{"session_id":"pins-then-cd"}"#;

    assert!(
        !brief_with_stdin(&project, &["--pins-only"], session, "100")
            .stdout
            .is_empty()
    );
    assert!(
        !brief_with_stdin(&project, &["--if-changed"], session, "100")
            .stdout
            .is_empty()
    );
}

#[test]
fn two_session_ids_do_not_share_a_watermark() {
    let project = Project::docs_only("watermark-sessions");
    let a = brief_with_stdin(&project, &["--if-changed"], r#"{"session_id":"a"}"#, "100");
    assert!(!a.stdout.is_empty());
    let b = brief_with_stdin(&project, &["--if-changed"], r#"{"session_id":"b"}"#, "100");
    assert!(
        !b.stdout.is_empty(),
        "a second session gets its own watermark"
    );

    // Two ids differing only in characters an allowlist would strip must not
    // collide: the filename is a hash of the complete raw id.
    let x = brief_with_stdin(
        &project,
        &["--if-changed"],
        r#"{"session_id":"s/1"}"#,
        "100",
    );
    let y = brief_with_stdin(
        &project,
        &["--if-changed"],
        r#"{"session_id":"s:1"}"#,
        "100",
    );
    assert!(!x.stdout.is_empty());
    assert!(!y.stdout.is_empty());
}

#[test]
fn no_session_id_emits_every_time() {
    // Falling back to a per-cwd key makes concurrent sessions share one
    // watermark, so A → B → A would suppress A's re-injection even though B
    // displaced it. A duplicate brief is the acceptable failure.
    let project = Project::docs_only("watermark-anonymous");
    assert!(
        !brief_with_stdin(&project, &["--if-changed"], "", "100")
            .stdout
            .is_empty()
    );
    assert!(
        !brief_with_stdin(&project, &["--if-changed"], "", "100")
            .stdout
            .is_empty()
    );
}

#[test]
fn an_unwritable_state_dir_fails_open() {
    use std::io::Write;
    let project = Project::docs_only("watermark-unwritable");
    // A session id is supplied, so this exercises the watermark path rather
    // than the no-id path.
    let run = || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_devkit"))
            .args(["brief", "--if-changed"])
            .current_dir(&project.root)
            .env("HOME", &project.home)
            .env("USERPROFILE", &project.home)
            // A regular file where the state dir should be, so every
            // create_dir_all and write beneath it fails.
            .env("XDG_STATE_HOME", project.root.join("Cargo.toml"))
            .env("XDG_DATA_HOME", project.home.join("data"))
            .env("COLUMNS", "100")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(br#"{"session_id":"unwritable"}"#)
            .unwrap();
        child.wait_with_output().unwrap()
    };
    assert!(!run().stdout.is_empty());
    let second = run();
    assert!(second.status.success());
    assert!(
        !second.stdout.is_empty(),
        "an unwritable watermark costs a duplicate brief, never a withheld one"
    );
}

#[test]
fn leaving_a_project_says_so_once() {
    let project = Project::docs_only("watermark-leaving");
    let session = r#"{"session_id":"leaving"}"#;
    assert!(
        !brief_with_stdin(&project, &["--if-changed"], session, "100")
            .stdout
            .is_empty()
    );

    // A directory outside any devkit project, same session: the earlier
    // brief's content is stale, and silence would leave it the most recent
    // thing the agent was told.
    let elsewhere = project.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let out = brief_from(&project, &elsewhere, session);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(text.contains("no longer applies"), "{text}");

    // Announced once: the watermark is dropped with the notice, and a repeat
    // from the same directory has nothing left to contradict.
    let again = brief_from(&project, &elsewhere, session);
    assert!(
        again.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&again.stdout)
    );
}

/// `brief --if-changed` from an arbitrary directory, with the project's home
/// and isolated state so the watermark from a previous run is visible.
fn brief_from(project: &Project, cwd: &Path, stdin: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["brief", "--if-changed"])
        .current_dir(cwd)
        .env("HOME", &project.home)
        .env("USERPROFILE", &project.home)
        .env("XDG_STATE_HOME", project.home.join("state"))
        .env("XDG_DATA_HOME", project.home.join("data"))
        .env("COLUMNS", "100")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn a_repo_with_no_devkit_toml_renders_pins() {
    // The case `load::load(..).ok()?` silently killed. `config::resolve` bails
    // when there is no devkit.toml above the cwd and no personal config, so
    // today this repo gets no brief at all — even though its lockfile declares
    // a globally registered library.
    let project = Project::docs_only("no-devkit-toml");
    std::fs::remove_file(project.root.join("devkit.toml")).unwrap();

    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "brief never fails: {out:?}");
    assert!(text.contains("serde"), "{text}");
    assert!(text.contains("1.0.200"), "{text}");
}

#[test]
fn an_unrelated_repo_stays_silent() {
    // The inverse, and it must hold at the same time: the machine-wide catalog
    // accumulates every library ever asked about, so a checkout that evidences
    // none of them gets no section — not an empty one.
    let project = Project::docs_only("unrelated-repo");
    std::fs::remove_file(project.root.join("devkit.toml")).unwrap();
    std::fs::remove_file(project.root.join("Cargo.lock")).unwrap();
    std::fs::remove_file(project.root.join("Cargo.toml")).unwrap();

    let out = project.brief(&[]);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn a_docs_only_project_renders_pins() {
    // A devkit.toml with a [docs] section and nothing devrun can use.
    let project = Project::docs_only("docs-only");
    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "brief never fails: {out:?}");
    assert!(text.contains("serde"), "{text}");
    assert!(text.contains("1.0.200"), "{text}");
    assert!(
        text.contains("docm info"),
        "the caveat prose is present: {text}"
    );
}

#[test]
fn a_pins_only_brief_makes_no_devrun_claim() {
    // Task 8 review finding: "This checkout is a devkit-managed project:
    // dev servers, ports, canned tasks, and cross-session file locks are
    // coordinated by the devkit CLIs" is false for a checkout with no
    // devkit.toml at all — an agent would act on that claim as fact.
    let project = Project::docs_only("no-devrun-claim");
    std::fs::remove_file(project.root.join("devkit.toml")).unwrap();

    let text = String::from_utf8_lossy(&project.brief(&[]).stdout).into_owned();
    assert!(text.contains("serde"), "{text}");
    assert!(
        !text.contains("coordinated by the devkit"),
        "pins-only brief still makes the devrun claim: {text}"
    );
}

#[test]
fn a_devrun_brief_still_makes_the_devrun_claim() {
    // The inverse of the above: the claim is accurate and must not
    // disappear when this checkout does have a devrun setup.
    let project = Project::docs_only("devrun-claim");
    project.set_config(&format!(
        "[config]\nroot = true\n\n{DEFAULTS}[tasks.check]\nrun = [\"cargo\", \"test\"]\ndescription = \"tests\"\n"
    ));

    let text = String::from_utf8_lossy(&project.brief(&[]).stdout).into_owned();
    assert!(text.contains("coordinated by the devkit"), "{text}");
}

#[test]
fn a_broken_docs_manifest_leaves_the_rest_of_the_brief() {
    let project = Project::docs_only("broken-manifest");
    write(
        &project.home.join(".config/devkit/docs.toml"),
        "not toml [[[",
    );
    project.set_config(&format!(
        "[config]\nroot = true\n\n{DEFAULTS}[tasks.check]\nrun = [\"cargo\", \"test\"]\ndescription = \"tests\"\n"
    ));

    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    // "check" alone is also a substring of the always-present "checkout" in
    // the intro sentence; the task's own description column is what only the
    // Tasks section can produce.
    assert!(text.contains("tests"), "tasks still render: {text}");
    assert!(
        !text.contains("Library versions"),
        "the pins section is omitted: {text}"
    );
}

#[test]
fn brief_enabled_false_suppresses_everything() {
    let project = Project::docs_only("gate-off");
    project.set_config(&format!(
        "[config]\nroot = true\n\n{DEFAULTS}[brief]\nenabled = false\n"
    ));
    let out = project.brief(&[]);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn brief_pins_false_suppresses_only_that_section() {
    let project = Project::docs_only("pins-off");
    project.set_config(&format!(
        "[config]\nroot = true\n\n{DEFAULTS}[brief]\npins = false\n\n[tasks.check]\nrun = [\"cargo\", \"test\"]\ndescription = \"tests\"\n"
    ));
    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    // "check" alone is also a substring of the always-present "checkout" in
    // the intro sentence; the task's own description column is what only the
    // Tasks section can produce.
    assert!(text.contains("tests"), "tasks still render: {text}");
    assert!(!text.contains("serde"), "{text}");
}

#[test]
fn the_gate_precedes_the_work() {
    // With enabled = false, no manifest is discovered and no importer runs:
    // point the config at a manifest whose resolution would fail loudly and
    // observe silence and exit 0.
    let project = Project::docs_only("gate-first");
    write(
        &project.home.join(".config/devkit/docs.toml"),
        "not toml [[[",
    );
    project.set_config(&format!(
        "[config]\nroot = true\n\n{DEFAULTS}[brief]\nenabled = false\n"
    ));
    let out = project.brief(&[]);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(
        out.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_hundred_column_render_is_bounded() {
    let project = Project::docs_only("width-bound");
    // A library whose SOURCE cell is a full unresolved sentence.
    write(
        &project.home.join(".config/devkit/docs.toml"),
        "[[libs]]\nname = \"kysely\"\necosystem = \"js\"\nrepo = \"https://example.invalid/kysely\"\n",
    );
    project.set_config("[config]\nroot = true\n\n[docs]\n\n[[docs.libs]]\nname = \"kysely\"\n");

    let out = project.brief(&[]);
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        assert!(line.chars().count() <= 100, "unbounded line: {line}");
    }
}

#[test]
fn both_callers_render_the_same_rows() {
    // One renderer, asserted rather than assumed: the brief's section and
    // `docm list --project` must agree row for row from the same cwd.
    let project = Project::docs_only("both-callers");
    let listing =
        String::from_utf8_lossy(&project.docm(&["list", "--project"]).stdout).into_owned();
    let brief = String::from_utf8_lossy(&project.brief(&[]).stdout).into_owned();

    let rows: Vec<&str> = listing.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(!rows.is_empty(), "{listing}");
    for row in rows {
        assert!(
            brief.contains(row.trim_end()),
            "brief is missing a row `docm list --project` printed:\n{row}\n--- brief ---\n{brief}"
        );
    }
}

#[test]
fn a_machine_wide_undeclared_library_never_reaches_the_brief() {
    // The /docs-accumulation guard, end to end: two registered libraries where
    // only one is declared. The undeclared one produces no row and does not
    // suppress the one that resolved.
    let project = Project::docs_only("accumulation-guard");
    write(
        &project.home.join(".config/devkit/docs.toml"),
        "[[libs]]\nname = \"serde\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/serde\"\n\n[[libs]]\nname = \"tokio\"\necosystem = \"rust\"\nrepo = \"https://example.invalid/tokio\"\n",
    );

    let text = String::from_utf8_lossy(&project.brief(&[]).stdout).into_owned();
    assert!(text.contains("serde"), "{text}");
    assert!(!text.contains("tokio"), "{text}");
    assert!(
        text.contains("1 registered library not evidenced here"),
        "{text}"
    );
}
