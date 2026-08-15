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
            // Isolate the state dir so a test never reads or writes the
            // machine's real registry and watermarks.
            .env("XDG_STATE_HOME", self.home.join("state"))
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
            .env("COLUMNS", "100")
            .output()
            .unwrap()
    }

    fn set_config(&self, body: &str) {
        write(&self.root.join("devkit.toml"), body);
    }
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
    assert!(text.contains("check"), "tasks still render: {text}");
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
    assert!(text.contains("check"), "{text}");
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
