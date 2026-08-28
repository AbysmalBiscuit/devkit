//! Each user-facing CLI emits a shell-completion script via `<bin> completions <shell>`.

#[path = "common/shimtest.rs"]
mod shimtest;
use std::process::Command;

fn completions_contain_name(bin: &str, exe: &str, shell: &str) {
    let out = Command::new(exe)
        .args(["completions", shell])
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .expect("spawn completions");
    assert!(
        out.status.success(),
        "{bin} completions {shell} exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let script = String::from_utf8(out.stdout).expect("utf8 completion script");
    assert!(
        script.contains(bin),
        "{bin} {shell} completion script should mention the command name"
    );
}

fn emits_every_shell(bin: &str, exe: &str) {
    for shell in ["bash", "elvish", "fish", "nushell", "powershell", "zsh"] {
        completions_contain_name(bin, exe, shell);
    }
}

#[test]
fn portm_emits_completions() {
    let (_dir, link) = shimtest::linked("portm");
    emits_every_shell("portm", link.to_str().expect("utf-8 link path"));
}

#[test]
fn devrun_emits_completions() {
    let (_dir, link) = shimtest::linked("devrun");
    emits_every_shell("devrun", link.to_str().expect("utf-8 link path"));
}

#[test]
fn issue_emits_completions() {
    let (_dir, link) = shimtest::linked("issue");
    emits_every_shell("issue", link.to_str().expect("utf-8 link path"));
}

#[test]
fn lockm_emits_completions() {
    let (_dir, link) = shimtest::linked("lockm");
    emits_every_shell("lockm", link.to_str().expect("utf-8 link path"));
}

#[test]
fn devkit_emits_completions() {
    emits_every_shell("devkit", env!("CARGO_BIN_EXE_devkit"));
}

#[test]
fn docm_emits_completions() {
    let (_dir, link) = shimtest::linked("docm");
    emits_every_shell("docm", link.to_str().expect("utf-8 link path"));
}

/// `--all` is the one-file form: `devkit`'s own script plus one per old name,
/// so a dotfile manager can regenerate every completion with a single command.
fn all_shells_script(shell: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["completions", "--all", shell])
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .expect("spawn completions --all");
    assert!(
        out.status.success(),
        "completions --all {shell} exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 completion script")
}

#[test]
fn all_emits_a_registration_per_name_for_zsh() {
    let script = all_shells_script("zsh");
    for name in ["devkit", "issue", "devrun", "portm", "lockm", "docm"] {
        assert!(
            script.contains(&format!("compdef _{name} {name}")),
            "zsh --all script should register {name}"
        );
    }
}

#[test]
fn all_emits_a_registration_per_name_for_fish() {
    let script = all_shells_script("fish");
    for name in ["devkit", "issue", "devrun", "portm", "lockm", "docm"] {
        assert!(
            script.contains(&format!("complete -c {name} ")),
            "fish --all script should register {name}"
        );
    }
}

/// `devkit-mcp` takes no subcommands, so it has no `completions` of its own and
/// `--all` has nothing to emit for it. Derived from the command tree rather than
/// listed, so a future `devkit mcp` subcommand surface would be picked up.
#[test]
fn all_skips_the_name_with_no_completions() {
    let script = all_shells_script("zsh");
    assert!(
        !script.contains("compdef _devkit-mcp devkit-mcp"),
        "devkit-mcp has no completions subcommand to emit"
    );
}

/// A reader that closes the pipe early (`| head`, `| grep -q`) must not crash
/// the writer. `clap_complete::generate` panics on the failed write; the shared
/// writer treats a broken pipe as a clean exit.
#[cfg(unix)]
#[test]
fn a_closed_pipe_does_not_panic() {
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["completions", "--all", "zsh"])
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn completions");
    drop(child.stdout.take().expect("piped stdout"));
    let out = child.wait_with_output().expect("wait for completions");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked"),
        "a closed pipe must not panic: {stderr}"
    );
    assert!(
        out.status.success(),
        "a closed pipe is a clean exit, got {:?}: {stderr}",
        out.status
    );
}
