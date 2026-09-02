#[path = "common/shimtest.rs"]
mod shimtest;
use std::process::Command;

#[test]
fn portm_shim_parses_portm_arguments() {
    let (_dir, link) = shimtest::linked("portm");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .args(["--help"])
        .output()
        .expect("spawn portm shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "portm --help exited non-zero: {text}");
    assert!(
        text.contains("Port registry"),
        "shim should show portm's own about text: {text}"
    );
    assert!(
        !text.contains("Configure and diagnose"),
        "shim must not show devkit's about text: {text}"
    );
}

/// `portm` with no subcommand shows status, which needs no project to exit 0
/// against an empty registry.
#[test]
fn portm_shim_defaults_to_status() {
    let (_dir, link) = shimtest::linked("portm");
    let state = tempfile::tempdir().expect("state dir");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn bare portm shim");
    assert!(
        out.status.success(),
        "bare portm should run status: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn portm_shim_reports_the_package_version() {
    let (_dir, link) = shimtest::linked("portm");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("--version")
        .output()
        .expect("spawn portm --version");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "portm --version should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn devkit_ports_reaches_the_same_command() {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .args(["ports", "--help"])
        .output()
        .expect("spawn devkit ports --help");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("Port registry"),
        "devkit ports should show portm's about text: {text}"
    );
}

#[test]
fn lockm_shim_parses_lockm_arguments() {
    let (_dir, link) = shimtest::linked("lockm");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("--help")
        .output()
        .expect("spawn lockm shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "lockm --help exited non-zero: {text}");
    assert!(
        text.contains("acquire"),
        "shim should list lockm's own subcommands: {text}"
    );
}

/// The PreToolUse hook runs this on every edit; it must keep working through a
/// shim and must not require a terminal.
#[test]
fn lockm_shim_runs_the_pretooluse_hook() {
    let (_dir, link) = shimtest::linked("lockm");
    let state = tempfile::tempdir().expect("state dir");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .args(["hook", "pretooluse"])
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn lockm hook pretooluse");
    assert!(
        out.status.success(),
        "lockm hook pretooluse should exit 0: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn docm_shim_parses_docm_arguments() {
    let (_dir, link) = shimtest::linked("docm");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("--help")
        .output()
        .expect("spawn docm shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "docm --help exited non-zero: {text}");
    assert!(
        text.contains("prune"),
        "shim should list docm's own subcommands: {text}"
    );
}

#[test]
fn devrun_shim_parses_devrun_arguments() {
    let (_dir, link) = shimtest::linked("devrun");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("--help")
        .output()
        .expect("spawn devrun shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "devrun --help exited non-zero: {text}"
    );
    assert!(
        text.contains("reap"),
        "shim should list devrun's own subcommands: {text}"
    );
}

/// `reap` is TTY-gated with no bypass. A shim inherits the same non-terminal
/// stdin a hook or agent has, so it must still refuse — and it must actually
/// reach the gate: an in-process listener on a configured app's port gives
/// `strays::scan` a real port-band stray to find, so the run can't return
/// early on "no stray servers found" before ever checking the terminal.
#[test]
fn devrun_shim_still_refuses_reap_without_a_terminal() {
    let (_dir, link) = shimtest::linked("devrun");
    let state = tempfile::tempdir().expect("state dir");

    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind stray listener");
    let port = listener.local_addr().expect("listener addr").port();

    let project = tempfile::tempdir().expect("project dir");
    devkit_common::git::Git::fixture(project.path())
        .args(["init", "-q"])
        .output()
        .expect("git init");
    std::fs::write(
        project.path().join("devkit.toml"),
        format!(
            r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "b"

[apps.api]
base_port = {port}
path = "."
launch = ["git", "version"]
"#
        ),
    )
    .expect("write devkit.toml");

    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("-C")
        .arg(project.path())
        .arg("reap")
        .stdin(std::process::Stdio::null())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn devrun reap");
    drop(listener);

    assert!(
        !out.status.success(),
        "reap through a shim must refuse without a TTY"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("reap requires an interactive terminal"),
        "stderr was: {stderr}"
    );
}

/// A git repo with no issue worktrees, which is all `status` needs to render an
/// empty table and exit 0. Outside a repo it fails on `git worktree list`, so a
/// bare-`issue` test that skipped the repo would never reach `status::run`.
fn empty_repo() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("project dir");
    devkit_common::git::Git::fixture(project.path())
        .args(["init", "-q"])
        .output()
        .expect("git init");
    project
}

#[test]
fn issue_shim_parses_issue_arguments() {
    let (_dir, link) = shimtest::linked("issue");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("--help")
        .output()
        .expect("spawn issue shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "issue --help exited non-zero: {text}");
    assert!(
        text.contains("Pull-request lifecycle"),
        "shim should list issue's own subcommands: {text}"
    );
}

#[test]
fn hidden_aliases_stay_reachable_but_unlisted() {
    let (_dir, link) = shimtest::linked("issue");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("--help")
        .output()
        .expect("spawn issue shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !text.contains("checkout-pr"),
        "checkout-pr should be hidden from help: {text}"
    );
    assert!(
        !text.contains("info"),
        "info should be hidden from help: {text}"
    );

    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .args(["info", "--help"])
        .output()
        .expect("spawn issue shim");
    assert!(
        out.status.success(),
        "issue info must still parse: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .args(["checkout-pr", "--help"])
        .output()
        .expect("spawn issue shim");
    assert!(
        out.status.success(),
        "issue checkout-pr must still parse: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The group's own help must list both actions, not just exist.
#[test]
fn pr_group_lists_status_and_checkout() {
    let (_dir, link) = shimtest::linked("issue");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .args(["pr", "--help"])
        .output()
        .expect("spawn issue shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "issue pr --help exited non-zero: {text}"
    );
    assert!(
        text.contains("status") && text.contains("checkout"),
        "issue pr --help should list both subcommands: {text}"
    );
}

/// Bare `issue pr` must resolve to `pr status`, byte-for-byte the same as the
/// hidden `issue info` alias it replaces, not fall through expecting
/// `checkout`'s TARGET positional.
#[test]
fn bare_pr_matches_info() {
    let (_dir, link) = shimtest::linked("issue");
    let state = tempfile::tempdir().expect("state dir");
    let project = empty_repo();
    // `info`/`pr status` resolve the current branch, unlike `status`, which
    // `empty_repo` alone suffices for; give it a HEAD to resolve.
    devkit_common::git::Git::fixture(project.path())
        .args(["commit", "--allow-empty", "-q", "-m", "init"])
        .output()
        .expect("git commit");

    let pr_out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("pr")
        .current_dir(project.path())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn bare issue pr");
    assert!(
        pr_out.status.success(),
        "bare issue pr should run status: {}",
        String::from_utf8_lossy(&pr_out.stderr)
    );

    let info_out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("info")
        .current_dir(project.path())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn issue info");
    assert!(
        info_out.status.success(),
        "issue info should still run: {}",
        String::from_utf8_lossy(&info_out.stderr)
    );

    assert_eq!(
        String::from_utf8_lossy(&pr_out.stdout),
        String::from_utf8_lossy(&info_out.stdout),
        "bare `issue pr` should render exactly what `issue info` does"
    );
}

/// Bare `issue` runs `status`, the invocation most users type.
#[test]
fn issue_shim_defaults_to_status() {
    let (_dir, link) = shimtest::linked("issue");
    let state = tempfile::tempdir().expect("state dir");
    let project = empty_repo();
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .current_dir(project.path())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn bare issue shim");
    assert!(
        out.status.success(),
        "bare issue should run status: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ISSUE WORKTREES"),
        "bare issue should render the status table: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Parity requirement: the defaulted subcommand must reach `status` through
/// `devkit issue` too, not just through the shim.
#[test]
fn devkit_issue_defaults_to_status() {
    let state = tempfile::tempdir().expect("state dir");
    let project = empty_repo();
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("issue")
        .current_dir(project.path())
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .output()
        .expect("spawn bare devkit issue");
    assert!(
        out.status.success(),
        "bare `devkit issue` should run status: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ISSUE WORKTREES"),
        "bare `devkit issue` should render the status table: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn devkit_mcp_shim_reports_the_package_version() {
    let (_dir, link) = shimtest::linked("devkit-mcp");
    let out = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .arg("--version")
        .output()
        .expect("spawn devkit-mcp --version");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains(env!("CARGO_PKG_VERSION")),
        "devkit-mcp --version should print {}, got: {text}",
        env!("CARGO_PKG_VERSION")
    );
}

/// The server answers a JSON-RPC request on stdin when run through the shim,
/// which is how `.mcp.json` starts it.
#[test]
fn devkit_mcp_shim_serves_a_request() {
    use std::io::Write;
    let (_dir, link) = shimtest::linked("devkit-mcp");
    let state = tempfile::tempdir().expect("state dir");
    let mut child = Command::new(&link)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn devkit-mcp shim");
    let req = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let mut stdin = child.stdin.take().expect("stdin");
    stdin.write_all(req).expect("write request");
    stdin.write_all(b"\n").expect("write newline");
    drop(stdin);
    let out = child.wait_with_output().expect("wait for mcp shim");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        text.contains("\"id\":1"),
        "shim should answer the request: {text}"
    );
}

/// Under a shim name the tree roots every path at the name the caller typed,
/// so a line can be copied and run as-is. The scope test is the other half:
/// `docm` shows the docs subtree and nothing else in the CLI.
#[test]
fn the_shim_tree_is_rooted_at_the_shim_name() {
    let (_dir, link) = shimtest::linked("docm");
    let out = Command::new(&link)
        .arg("--help")
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_HELP", "full")
        .output()
        .expect("spawn docm --help");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "docm --help exited non-zero: {text}");
    assert!(text.contains("docm add"), "rooted at the shim name: {text}");
    assert!(
        !text.contains("devkit docs add"),
        "not the canonical path: {text}"
    );
    assert!(!text.contains("issue setup"), "docs subtree only: {text}");
}
