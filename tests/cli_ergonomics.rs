//! The CLI shapes agents naturally guess must work (or point at the right
//! command): subcommand aliases, a defaulted `--holder`, positional app/issue
//! arguments. Each test isolates state via a private HOME/XDG_STATE_HOME.

#[path = "common/shimtest.rs"]
mod shimtest;
use std::path::Path;
use std::process::{Command, Output};

/// A real git repo (so `--show-toplevel` resolves) with a two-app devkit.toml.
fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[apps.api]
base_port = 39400
path = "."
launch = ["git", "version"]

[apps.web]
base_port = 39500
path = "."
launch = ["git", "version"]
"#,
    )
    .unwrap();
    p
}

fn run(exe: &Path, project: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(exe)
        .args(args)
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("XDG_CONFIG_HOME", state.join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", exe.display()))
}

fn toplevel(project: &Path) -> String {
    devkit_common::git::checkout_root(project)
        .expect("git rev-parse")
        .to_string_lossy()
        .into_owned()
}

// ---- lockm ----

#[test]
fn lockm_list_aliases_status() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(&link, proj.path(), state.path(), &["list"]);
    assert!(
        out.status.success(),
        "`lockm list` should work as an alias of `status`: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn lockm_status_with_paths_points_at_check() {
    let (_dir, link) = shimtest::linked("lockm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(
        &link,
        proj.path(),
        state.path(),
        &["status", "src/some/file.rs"],
    );
    assert!(!out.status.success(), "status with paths is an error");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("lockm check"),
        "error should point at `lockm check`: {err}"
    );
}

// ---- portm ----

#[test]
fn portm_reserve_aliases_alloc() {
    let (_dir, link) = shimtest::linked("portm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(&link, proj.path(), state.path(), &["reserve", "--help"]);
    assert!(
        out.status.success(),
        "`portm reserve` should work as an alias of `alloc`: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn portm_alloc_defaults_holder_to_worktree_root() {
    let (_dir, link) = shimtest::linked("portm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(&link, proj.path(), state.path(), &["alloc", "api"]);
    assert!(
        out.status.success(),
        "alloc without --holder should default to the worktree root: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("api="),
        "prints the reserved port: {stdout}"
    );

    let status = run(&link, proj.path(), state.path(), &["status"]);
    let table = String::from_utf8_lossy(&status.stdout);
    let leaf = Path::new(&toplevel(proj.path()))
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        table.contains(&leaf),
        "status shows the worktree root as holder: {table}"
    );
}

#[test]
fn portm_release_positional_apps_frees_only_those() {
    let (_dir, link) = shimtest::linked("portm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let alloc = run(&link, proj.path(), state.path(), &["alloc", "api", "web"]);
    assert!(
        alloc.status.success(),
        "alloc: {}",
        String::from_utf8_lossy(&alloc.stderr)
    );

    let rel = run(&link, proj.path(), state.path(), &["release", "api"]);
    assert!(
        rel.status.success(),
        "`portm release api` should release that app's port: {}",
        String::from_utf8_lossy(&rel.stderr)
    );

    let status = run(&link, proj.path(), state.path(), &["status"]);
    let table = String::from_utf8_lossy(&status.stdout);
    assert!(!table.contains("api"), "api's reservation is gone: {table}");
    assert!(table.contains("web"), "web's reservation survives: {table}");
}

#[test]
fn portm_release_without_holder_frees_current_worktree() {
    let (_dir, link) = shimtest::linked("portm");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    run(&link, proj.path(), state.path(), &["alloc", "api"]);

    let rel = run(&link, proj.path(), state.path(), &["release"]);
    assert!(
        rel.status.success(),
        "bare `portm release` should default to the worktree root: {}",
        String::from_utf8_lossy(&rel.stderr)
    );
    let status = run(&link, proj.path(), state.path(), &["status"]);
    let table = String::from_utf8_lossy(&status.stdout);
    assert!(!table.contains("api"), "reservation released: {table}");
}

// ---- issue ----

#[test]
fn issue_setup_accepts_positional_issue_id() {
    let (_dir, link) = shimtest::linked("issue");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    // No full config here — the point is only that clap accepts the shape
    // (a later config/network error exits 1, never clap's usage error 2).
    let out = run(
        &link,
        proj.path(),
        state.path(),
        &["setup", "ABC-123", "--slug", "s", "--dry-run"],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(2),
        "positional issue id must parse: {err}"
    );
    assert!(
        !err.contains("unexpected argument"),
        "no clap usage error: {err}"
    );
}

#[test]
fn issue_setup_rejects_both_positional_and_flag() {
    let (_dir, link) = shimtest::linked("issue");
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let out = run(
        &link,
        proj.path(),
        state.path(),
        &["setup", "ABC-123", "--issue", "ABC-999", "--slug", "s"],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "conflicting issue ids are a usage error"
    );
}

/// `devkit` is the one name an agent can guess from a `devkit.toml`. Its help
/// has to name the shim spellings, because a subcommand list alone never says
/// that `issue status` also works as a bare command. Asserting on the whole
/// mapping line, not the bare name, is what keeps this from passing on the
/// subcommand list alone — `issue`, `docs`, and `ports` already appear there.
#[test]
fn devkit_help_names_every_shim() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("--help")
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .expect("spawn devkit --help");
    let text = String::from_utf8(out.stdout).expect("utf-8 help");
    for line in [
        "issue       = devkit issue",
        "devrun      = devkit run",
        "portm       = devkit ports",
        "lockm       = devkit locks",
        "docm        = devkit docs",
        "devkit-mcp  = devkit mcp",
    ] {
        assert!(
            text.contains(line),
            "`devkit --help` never maps the shim: {line}\n{text}"
        );
    }
}

/// Run the built `devkit` with `DEVKIT_HELP` pinned, since `cargo nextest`
/// cannot hand a test a terminal and the piped stdout would otherwise decide
/// the view for us.
fn help_run(view: &str, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(args)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_HELP", view)
        .output()
        .expect("spawn devkit");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (text, out.status.success())
}

#[test]
fn the_full_view_descends_into_every_group() {
    let (text, ok) = help_run("full", &["--help"]);
    assert!(ok, "devkit --help failed: {text}");
    assert!(text.contains("devkit docs prune"), "{text}");
    assert!(text.contains("devkit issue review request"), "{text}");
}

#[test]
fn the_terse_view_lists_only_the_top_level() {
    let (text, _) = help_run("terse", &["--help"]);
    assert!(!text.contains("docs prune"), "{text}");
    assert!(!text.contains("issue review request"), "{text}");
}

#[test]
fn short_help_stays_terse_under_a_full_environment() {
    let (text, _) = help_run("full", &["-h"]);
    assert!(!text.contains("docs prune"), "-h is unconditional: {text}");
    assert!(text.contains("issue       = devkit issue"), "{text}");
}

#[test]
fn the_full_view_keeps_the_shim_footer() {
    let (text, _) = help_run("full", &["--help"]);
    assert!(text.contains("issue       = devkit issue"), "{text}");
}

#[test]
fn help_matches_the_long_flag() {
    assert_eq!(
        help_run("full", &["help"]).0,
        help_run("full", &["--help"]).0
    );
    assert_eq!(
        help_run("terse", &["help"]).0,
        help_run("terse", &["--help"]).0
    );
}

#[test]
fn full_outranks_a_terse_environment() {
    let (text, ok) = help_run("terse", &["help", "--full"]);
    assert!(ok, "help --full failed: {text}");
    assert!(text.contains("devkit docs prune"), "{text}");
}

#[test]
fn a_group_renders_only_its_own_subtree() {
    let (text, _) = help_run("full", &["issue", "--help"]);
    assert!(text.contains("devkit issue setup"), "{text}");
    assert!(!text.contains("docs prune"), "no sibling groups: {text}");
}

/// A help flag claims the node it appears under, so a later token that also
/// names a real subcommand does not move the target deeper. `issue setup`
/// appears in a tree rooted at `issue` and could not appear in one rooted at
/// `issue review`, which is what separates the two outcomes.
#[test]
fn the_first_help_wins() {
    let (text, _) = help_run("full", &["issue", "--help", "review"]);
    assert!(
        text.contains("devkit issue setup"),
        "rooted at issue, not at the trailing token: {text}"
    );
    assert!(!text.contains("docs prune"), "not the whole tree: {text}");
}

#[test]
fn an_alias_resolves_to_its_canonical_node() {
    let (text, ok) = help_run("full", &["docs", "remove", "--help"]);
    assert!(ok, "docs remove --help failed: {text}");
    assert!(text.contains("Usage: devkit docs rm"), "{text}");
}

#[test]
fn a_valued_global_flag_does_not_swallow_the_subcommand() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().to_string_lossy().to_string();
    let (text, _) = help_run("full", &["issue", "-C", &path, "status", "--help"]);
    assert!(
        text.contains("Usage: devkit issue status"),
        "resolved to issue status, not the issue root: {text}"
    );
    assert!(
        !text.contains("devkit issue setup"),
        "not the issue tree: {text}"
    );
}

#[test]
fn an_unknown_subcommand_is_still_an_error() {
    let (text, ok) = help_run("full", &["issue", "typo", "--help"]);
    assert!(!ok, "an unrecognized subcommand must fail: {text}");
    assert!(text.contains("typo"), "{text}");
}

/// The `help` subcommand's own path positional accepts any tokens and is
/// never validated, so an unrecognized name reaches `intercept_help`. It must
/// decline rather than report its own error, so clap's usual usage-carrying
/// error and exit code are what the caller sees in every view.
#[test]
fn an_unrecognized_help_path_gets_clap_s_own_error() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["help", "docs", "lst"])
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_HELP", "full")
        .output()
        .expect("spawn devkit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "clap's exit code: {stderr}");
    assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
    assert!(stderr.contains("Usage:"), "{stderr}");
}

/// clap's `help` node carries a child per sibling command, so a tree rooted
/// there would list `devkit help auth`, `devkit help brief` and the rest.
/// Both views hand `help help` back to clap.
#[test]
fn the_help_node_itself_never_grows_a_tree() {
    let (full, _) = help_run("full", &["help", "help"]);
    let (terse, _) = help_run("terse", &["help", "help"]);
    assert_eq!(full, terse, "the help node renders the same in both views");
    assert!(
        !full.contains("devkit help auth"),
        "walked into clap's synthetic help node: {full}"
    );
}

#[test]
fn a_required_option_does_not_block_a_help_request() {
    let (text, ok) = help_run("full", &["issue", "setup", "--help"]);
    assert!(ok, "issue setup --help failed: {text}");
    assert!(text.contains("--slug"), "{text}");
}

#[test]
fn a_separator_hides_a_later_help() {
    let (text, ok) = help_run("full", &["docs", "path", "--", "--help"]);
    assert!(
        !ok || !text.contains("devkit docs prune"),
        "no tree after --: {text}"
    );
}

#[test]
fn full_help_for_a_leaf_prints_its_long_help() {
    let (text, ok) = help_run("full", &["help", "docs", "add", "--full"]);
    assert!(ok, "help docs add --full failed: {text}");
    assert!(text.contains("--eco"), "leaf argument help: {text}");
    assert!(!text.contains("devkit docs prune"), "not a tree: {text}");
}

#[test]
fn short_help_outranks_a_long_help_in_the_same_argv() {
    let (text, _) = help_run("full", &["--help", "-h"]);
    assert!(!text.contains("docs prune"), "-h wins: {text}");
}

#[test]
fn the_three_help_spellings_agree() {
    let a = help_run("full", &["issue", "help", "status"]).0;
    let b = help_run("full", &["help", "issue", "status"]).0;
    let c = help_run("full", &["issue", "status", "--help"]).0;
    assert_eq!(a, b);
    assert_eq!(b, c);
}

#[test]
fn the_full_view_stays_inside_the_line_cap_and_ascii() {
    let (text, _) = help_run("full", &["--help"]);
    for line in text.lines() {
        assert!(line.chars().count() <= 100, "line over cap: {line}");
        assert!(line.is_ascii(), "line is not ascii: {line}");
    }
    assert!(!text.contains("devkit help "), "no help nodes: {text}");
}
