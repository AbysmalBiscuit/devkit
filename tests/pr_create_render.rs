//! `issue pr create` renders a template only for the run that sends it.
//!
//! Templating is strict about undefined variables, and `{{ issue }}` is bound
//! only in a worktree `issue setup` recorded. So a template that reads it is a
//! live failure for every run that renders one it has no use for.

use std::path::Path;

#[path = "common/ghfake.rs"]
mod ghfake;

/// A `pr_body` that cannot render outside an `issue setup` worktree.
const BODY_NEEDS_THE_RECORD: &str = r#"
[templates]
pr_body = "Closes {{ issue }}"
"#;

/// A `pr_title` with the same problem, so a run that renders one before
/// deciding it needs one fails on the title instead.
const TITLE_NEEDS_THE_RECORD: &str = r#"
[templates]
pr_title = "{{ issue }}: {{ input }}"
"#;

#[test]
fn reusing_a_pr_renders_neither_template() {
    for templates in [BODY_NEEDS_THE_RECORD, TITLE_NEEDS_THE_RECORD] {
        let fake = ghfake::Fake::new(templates, &ghfake::Pr {
            number: 7,
            state: "OPEN",
            is_draft: true,
            author: "LevValle",
        });
        let out = fake.issue(&["pr", "create", "--no-push"]);

        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "reuse must not render a template it never sends: {stderr}"
        );
        assert!(
            !fake.calls().contains("pr create"),
            "the PR already exists: {}",
            fake.calls()
        );
    }
}

/// A run the reviewer gate refuses must say so. Rendering first would answer a
/// policy refusal with a template error, naming a problem the user does not
/// have and hiding the one they do.
#[test]
fn the_reviewer_gate_refuses_before_a_template_can_fail() {
    let fake = ghfake::Fake::without_pr(&format!(
        "require_pr_reviewer = true\n{BODY_NEEDS_THE_RECORD}"
    ));
    let out = fake.issue(&["pr", "create", "--ready", "--no-push"]);

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(!out.status.success(), "the gate must refuse: {stderr}");
    assert!(
        stderr.contains("no human reviewer"),
        "the refusal must name the gate, not the template: {stderr}"
    );
    assert!(
        !fake.calls().contains("pr create"),
        "nothing is opened: {}",
        fake.calls()
    );
}

/// The fixed directory `run_issue` points the fake `gh` at, so `setup` and a
/// test wanting a different canned answer agree on where to write it.
fn ghfake_bin(root: &Path) -> std::path::PathBuf {
    root.join(".ghfake")
}

/// A git repo on `lev/eng-1-fix` with one commit, a pushable `origin` that
/// resolves to `github.com/o/r`, and a fake `gh` reporting one open PR for
/// that branch — everything `issue pr create` needs to reuse a PR end to end,
/// for a test that supplies its own devkit.toml (with no `[github]` table)
/// and so relies on the `origin` default.
///
/// `origin` is a local bare repo. `run_issue` points `HOME` at this same
/// directory, where a `.gitconfig` here rewrites pushes to `origin` with
/// `pushInsteadOf`: `git remote get-url origin` — what devkit's repository
/// resolution reads — still reports the `github.com` URL, only the transport
/// changes. Plain `insteadOf` would rewrite `remote get-url` too, making the
/// origin unrecognizable as a github.com remote.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let git = |args: &[&str]| {
        devkit_common::git::Git::fixture(root)
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    };
    git(&["init", "-q", "-b", "lev/eng-1-fix"]);
    std::fs::write(root.join("README"), "x").expect("write README");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["remote", "add", "origin", "https://github.com/o/r.git"]);
    let head = devkit_common::git::Git::fixture(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse")
        .trim()
        .to_string();

    let origin = root.join("origin.git");
    std::fs::create_dir_all(&origin).expect("origin dir");
    devkit_common::git::Git::fixture(&origin)
        .args(["init", "-q", "--bare", "-b", "main"])
        .output()
        .expect("git init --bare");
    std::fs::write(
        root.join(".gitconfig"),
        format!(
            "[url \"{}\"]\n\tpushInsteadOf = https://github.com/o/r.git\n",
            origin.display().to_string().replace('\\', "/")
        ),
    )
    .expect("write .gitconfig");

    let bin = ghfake_bin(root);
    std::fs::create_dir_all(&bin).expect("ghfake bin dir");
    ghfake::install_fake_gh(&bin);
    std::fs::write(
        bin.join("pr_list.json"),
        format!(
            r#"[{{"number":7,"state":"OPEN","url":"https://github.com/o/r/pull/7",
                 "headRefName":"lev/eng-1-fix","headRefOid":"{head}","isDraft":false,
                 "author":{{"login":"lev"}}}}]"#
        ),
    )
    .expect("write pr list payload");

    dir
}

/// Run `devkit issue <args…>` in `dir`, against the `gh` `setup` installed.
/// Mirrors `run_in` in `tests/task_cmd.rs`, plus the GitHub/Slack env vars
/// `ghfake::Fake::issue` strips so the run resolves no bearer token and takes
/// its `gh` fallback for every lookup.
fn run_issue(dir: &Path, args: &[&str]) -> std::process::Output {
    let state = dir.join("state");
    let bin = ghfake_bin(dir);
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&inherited)))
            .expect("join PATH");
    std::process::Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("issue")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("PATH", path)
        .env("GHFAKE_DIR", &bin)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("LOCALAPPDATA", &state) // Windows: keep the registry off the real one
        .env("USERPROFILE", dir) // Windows: keep config resolution off the real home
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_HOST")
        .env_remove("GH_REPO")
        .env_remove("SLACK_TOKEN")
        .output()
        .expect("run devkit issue")
}

#[test]
fn issue_pr_refuses_a_required_arg_the_agent_did_not_pass() {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[templates]
pr_title = "{{ ticket }}: {{ input }}"

[templates.variables]
ticket = { default = "NONE", required = "agents" }
"#,
    )
    .unwrap();

    let out = run_issue(dir.path(), &["pr", "create", "--pr-title", "add a thing"]);
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--arg ticket=..."), "{stderr}");
    assert!(stderr.contains("required for agents"), "{stderr}");
}

#[test]
fn a_name_only_another_surfaces_template_reads_does_not_bind() {
    let dir = setup();
    std::fs::write(
        dir.path().join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[templates]
review_finish = "{{ ticket }} {{ pr_url }}"

[templates.variables]
ticket = { default = "NONE", required = "agents" }
"#,
    )
    .unwrap();

    // `pr_title`/`pr_body` default to `{{ input }}` and read no `ticket`,
    // so the marking is irrelevant to this command.
    let out = run_issue(dir.path(), &["pr", "create", "--pr-title", "add a thing"]);
    assert!(out.status.success(), "{out:?}");
}
