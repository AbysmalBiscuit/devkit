//! Each user-facing CLI emits a shell-completion script via `<bin> completions <shell>`.

use std::process::Command;

fn completions_contain_name(bin: &str, exe: &str) {
    let out = Command::new(exe)
        .args(["completions", "bash"])
        .output()
        .expect("spawn completions");
    assert!(
        out.status.success(),
        "{bin} completions bash exited non-zero"
    );
    let script = String::from_utf8(out.stdout).expect("utf8 completion script");
    assert!(
        script.contains(bin),
        "{bin} completion script should mention the command name"
    );
}

#[test]
fn portman_emits_completions() {
    completions_contain_name("portman", env!("CARGO_BIN_EXE_portman"));
}

#[test]
fn devrun_emits_completions() {
    completions_contain_name("devrun", env!("CARGO_BIN_EXE_devrun"));
}

#[test]
fn issue_emits_completions() {
    completions_contain_name("issue", env!("CARGO_BIN_EXE_issue"));
}

#[test]
fn lock_emits_completions() {
    completions_contain_name("lock", env!("CARGO_BIN_EXE_lock"));
}
