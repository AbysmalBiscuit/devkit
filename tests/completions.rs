//! Each user-facing CLI emits a shell-completion script via `<bin> completions <shell>`.

#[path = "common/shimtest.rs"]
mod shimtest;
use std::process::Command;

fn completions_contain_name(bin: &str, exe: &str, shell: &str) {
    let out = Command::new(exe)
        .args(["completions", shell])
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
    emits_every_shell("devrun", env!("CARGO_BIN_EXE_devrun"));
}

#[test]
fn issue_emits_completions() {
    emits_every_shell("issue", env!("CARGO_BIN_EXE_issue"));
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
    emits_every_shell("docm", env!("CARGO_BIN_EXE_docm"));
}

#[test]
fn docm_emits_completions() {
    let (_dir, link) = shimtest::linked("docm");
    completions_contain_name("docm", link.to_str().expect("utf-8 link path"));
}
