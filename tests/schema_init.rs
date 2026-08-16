//! `devkit schema init` — point a `devkit.toml` at the published schema.

use std::path::Path;
use std::process::{Command, Output};

const DIRECTIVE: &str =
    "#:schema https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-config.json";

fn init(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(["schema", "init"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("devkit-schema-init-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_missing_config_is_created_with_the_directive_first() {
    let dir = tmp("create");
    let out = init(&dir, &[]);
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );

    let body = std::fs::read_to_string(dir.join("devkit.toml")).unwrap();
    assert_eq!(
        body.lines().next().unwrap(),
        DIRECTIVE,
        "taplo only honors the directive on the first line: {body}"
    );
    // A starter that omits a key teaches the wrong shape. `[defaults]` shows
    // the four the merged config requires, and the app shows
    // `base_port`/`path`/`launch` — the omission that makes every devkit
    // binary go quiet about why.
    for key in [
        "worktree_root",
        "branch_prefix",
        "baseline_ref",
        "baseline_path",
    ] {
        assert!(body.contains(key), "missing {key}: {body}");
    }
    for key in ["base_port", "path", "launch"] {
        assert!(body.contains(key), "missing {key}: {body}");
    }
}

#[test]
fn every_setting_in_the_starter_is_commented_out() {
    let dir = tmp("commented");
    assert!(init(&dir, &[]).status.success());
    let body = std::fs::read_to_string(dir.join("devkit.toml")).unwrap();

    // Nothing is active until its owner has read it: an uncommented
    // worktree_root would have devkit creating worktrees nobody chose, and a
    // <placeholder> that parses is worse than one that does not.
    let settings: toml::Table = toml::from_str(&body).unwrap();
    assert!(settings.is_empty(), "{body}");
}

#[test]
fn an_existing_config_keeps_its_content_and_gains_the_directive() {
    let dir = tmp("prepend");
    let original = "# a comment\n\n[defaults]\nworktree_root = \"/w\"\n";
    std::fs::write(dir.join("devkit.toml"), original).unwrap();

    let out = init(&dir, &[]);
    assert!(out.status.success());

    let body = std::fs::read_to_string(dir.join("devkit.toml")).unwrap();
    assert_eq!(body.lines().next().unwrap(), DIRECTIVE);
    assert!(
        body.ends_with(original),
        "content preserved verbatim: {body}"
    );
}

#[test]
fn running_it_twice_does_not_stack_directives() {
    let dir = tmp("idempotent");
    init(&dir, &[]);
    let once = std::fs::read_to_string(dir.join("devkit.toml")).unwrap();

    let out = init(&dir, &[]);
    assert!(out.status.success());
    let twice = std::fs::read_to_string(dir.join("devkit.toml")).unwrap();

    assert_eq!(once, twice, "a second run must not rewrite the file");
    assert_eq!(twice.matches("#:schema").count(), 1, "{twice}");
}

#[test]
fn an_explicit_path_is_honored() {
    let dir = tmp("explicit");
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    let out = init(&dir, &["nested/devkit.toml"]);
    assert!(out.status.success());

    let body = std::fs::read_to_string(dir.join("nested/devkit.toml")).unwrap();
    assert_eq!(body.lines().next().unwrap(), DIRECTIVE);
    assert!(
        !dir.join("devkit.toml").exists(),
        "only the named file is written"
    );
}
