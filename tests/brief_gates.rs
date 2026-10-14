//! The brief names each PR gate that is on, so an agent knows the
//! constraints its PR will meet before it writes code.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    path::Path,
    process::{Command, Stdio},
};

fn git(dir: &Path, args: &[&str]) {
    devkit_git::Git::fixture(dir)
        .args(args.iter().copied())
        .output()
        .unwrap();
}

fn brief(config: &str) -> String {
    let p = tempfile::tempdir().unwrap();
    git(p.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(p.path().join("devkit.toml"), config).unwrap();
    brief_in(p.path())
}

fn brief_in(dir: &Path) -> String {
    let state = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.arg("brief")
        .current_dir(dir)
        .env("HOME", state.path())
        .env("XDG_STATE_HOME", state.path())
        .env("XDG_CONFIG_HOME", state.path().join("config"))
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .stdin(Stdio::null());
    testenv::scrub_identity(&mut cmd);
    let out = cmd.output().expect("devkit brief");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_brief_names_each_gate_that_is_on() {
    for (config, mode) in [
        ("", "require"),
        ("[pr.gates]\nweakened_tests = \"require\"\n", "require"),
        ("[pr.gates]\nweakened_tests = \"advise\"\n", "advise"),
    ] {
        let body = brief(config);
        assert!(body.contains("### PR gates"), "{config}: {body}");
        assert!(
            body.contains(&format!("`weakened_tests` ({mode})")),
            "{config}: {body}"
        );
    }
    let body = brief("[pr.gates]\nweakened_tests = \"require\"\n");
    assert!(body.contains("Test changes:"), "names the way out: {body}");
    assert!(
        body.contains("`devkit pr weakened-tests -h` says what it flags"),
        "points at the gate's one description: {body}"
    );
}

#[test]
fn the_brief_names_no_gate_when_all_are_off() {
    let body = brief("[pr.gates]\nweakened_tests = \"off\"\n");
    assert!(body.contains("devkit project"), "still a brief: {body}");
    assert!(!body.contains("PR gates"), "{body}");
    assert!(!body.contains("weakened_tests"), "{body}");
}

#[test]
fn a_gate_alone_still_makes_a_brief() {
    let body = brief("[brief]\nlocks = false\n");
    assert!(body.contains("### PR gates"), "{body}");
    assert!(body.contains("`weakened_tests` (require)"), "{body}");
}

/// A repository whose `main` commits `devkit.toml` with the gate on, checked
/// out on a branch.
fn gated_repo() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    git(p.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(
        p.path().join("devkit.toml"),
        "[pr.gates]\nweakened_tests = \"require\"\n",
    )
    .unwrap();
    git(p.path(), &["add", "devkit.toml"]);
    git(p.path(), &["commit", "-q", "-m", "init"]);
    git(p.path(), &["checkout", "-q", "-b", "feature"]);
    p
}

#[test]
fn a_local_layer_does_not_hide_a_gate_the_merge_base_sets() {
    let p = gated_repo();
    std::fs::write(
        p.path().join("devkit.local.toml"),
        "[pr.gates]\nweakened_tests = \"off\"\n",
    )
    .unwrap();
    let body = brief_in(p.path());
    assert!(body.contains("`weakened_tests` (require)"), "{body}");
}

#[test]
fn a_branch_commit_does_not_hide_a_gate_the_merge_base_sets() {
    let p = gated_repo();
    std::fs::write(
        p.path().join("devkit.toml"),
        "[pr.gates]\nweakened_tests = \"off\"\n",
    )
    .unwrap();
    git(p.path(), &["commit", "-q", "-am", "loosen"]);
    let body = brief_in(p.path());
    assert!(body.contains("`weakened_tests` (require)"), "{body}");
}
