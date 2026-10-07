//! `devkit template` end to end: listing, showing and rendering custom and
//! built-in templates, driven through the binary.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

#[path = "common/ghfake.rs"]
mod ghfake;

const CONFIG: &str = r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"

[templates]
pr_body = "Closes {{ issue }}.\n\n{{ input }}"

[templates.custom.standup]
description = "Daily update for #eng-standup"
body = """
**Yesterday:** {{ yesterday }}
**Today:** {{ today }}
{% if blockers %}**Blocked on:** {{ blockers }}{% endif %}
"""

[templates.custom.vibe]
body = "{{ mood }}"

[templates.variables]
yesterday = { required = "always", description = "what shipped, one line per item" }
today = { required = "always" }
blockers = { default = "" }
mood = { default = "fine", required = "agents" }
"#;

fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    devkit_git::Git::fixture(dir.path())
        .args(["init", "-q", "-b", "x/eng-1-fix"])
        .output()
        .expect("git init");
    std::fs::write(dir.path().join("devkit.toml"), CONFIG).expect("write devkit.toml");
    std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
    std::fs::write(
        dir.path().join(".devkit/issue.toml"),
        "issue = \"ENG-1\"\nslug = \"fix\"\napps = []\n",
    )
    .unwrap();
    dir
}

/// `devkit` sandboxed to `dir`, running as `caller`.
fn devkit(dir: &Path, caller: &str) -> Command {
    let state = dir.join("state");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.current_dir(dir)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("LOCALAPPDATA", &state)
        .env("USERPROFILE", dir)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", caller)
        .arg("template");
    cmd
}

fn run(dir: &Path, args: &[&str]) -> Output {
    devkit(dir, "agent")
        .args(args)
        .output()
        .expect("run devkit")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn render_prints_the_body_and_nothing_else() {
    let dir = setup();
    let out = run(dir.path(), &[
        "render",
        "standup",
        "--arg",
        "yesterday=shipped #163",
        "--arg",
        "today=ship #164",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        stdout(&out),
        "**Yesterday:** shipped #163\n**Today:** ship #164\n\n"
    );
}

#[test]
fn render_refuses_every_missing_required_arg_before_rendering() {
    let dir = setup();
    let out = run(dir.path(), &["render", "standup"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(stdout(&out).is_empty(), "nothing rendered: {out:?}");
    let err = stderr(&out);
    assert!(err.contains("--arg today=..."), "{err}");
    assert!(err.contains("--arg yesterday=..."), "{err}");
    assert!(
        err.contains("yesterday: what shipped, one line per item"),
        "the description rides along: {err}"
    );
    assert!(!err.contains("--arg blockers"), "defaulted: {err}");
    assert!(!err.contains("undefined"), "not minijinja's error: {err}");
}

#[test]
fn a_marking_for_agents_binds_only_agents() {
    let dir = setup();
    let agent = run(dir.path(), &["render", "vibe"]);
    assert!(!agent.status.success(), "{agent:?}");
    assert!(
        stderr(&agent).contains("--arg mood=... (required for agents)"),
        "{agent:?}"
    );

    let human = devkit(dir.path(), "human")
        .args(["render", "vibe"])
        .output()
        .unwrap();
    assert!(human.status.success(), "{human:?}");
    assert_eq!(stdout(&human), "fine");
}

#[test]
fn render_refuses_an_arg_the_template_does_not_read() {
    let dir = setup();
    let out = run(dir.path(), &[
        "render", "vibe", "--arg", "mood=ok", "--arg", "moood=x",
    ]);
    assert!(!out.status.success(), "{out:?}");
    assert!(stderr(&out).contains("`moood`"), "{out:?}");
}

#[test]
fn render_reads_an_arg_file_from_stdin() {
    let dir = setup();
    let mut child = devkit(dir.path(), "agent")
        .args([
            "render",
            "standup",
            "--arg",
            "today=ship #164",
            "--arg-file",
            "yesterday=-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"- one\n- two\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        stdout(&out),
        "**Yesterday:** - one\n- two\n\n**Today:** ship #164\n\n"
    );
}

#[test]
fn an_unknown_template_is_refused() {
    let dir = setup();
    let out = run(dir.path(), &["render", "nope"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(stderr(&out).contains("`nope`"), "{out:?}");
}

#[test]
fn list_names_custom_and_built_in_templates_with_their_args() {
    let dir = setup();
    let out = run(dir.path(), &["list"]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);
    let standup = text
        .lines()
        .find(|l| l.contains("standup"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(standup.contains("custom"), "{standup}");
    assert!(
        standup.contains("Daily update for #eng-standup"),
        "{standup}"
    );
    assert!(standup.contains("[blockers] today yesterday"), "{standup}");
    let pr_body = text
        .lines()
        .find(|l| l.contains("pr_body"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(pr_body.contains("built-in"), "{pr_body}");

    let json = run(dir.path(), &["list", "--json"]);
    let rows: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let standup = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "standup")
        .unwrap();
    assert_eq!(standup["kind"], "custom");
    assert_eq!(standup["args"][2]["name"], "yesterday");
    assert_eq!(standup["args"][2]["required"], true);
    let pr_body = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "pr_body")
        .unwrap();
    let names: Vec<&str> = pr_body["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["input"],
        "the worktree supplies `issue`, so it is no arg"
    );
}

#[test]
fn show_prints_the_source_then_each_arg() {
    let dir = setup();
    let out = run(dir.path(), &["show", "standup"]);
    assert!(out.status.success(), "{out:?}");
    let text = stdout(&out);
    assert!(text.contains("**Yesterday:** {{ yesterday }}"), "{text}");
    let yesterday = text
        .lines()
        .find(|l| l.contains("what shipped, one line per item"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(yesterday.contains("yesterday"), "{yesterday}");
    assert!(yesterday.contains("always"), "{yesterday}");

    let json = run(dir.path(), &["show", "standup", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(
        v["source"]
            .as_str()
            .unwrap()
            .starts_with("**Yesterday:** {{ yesterday }}\n")
    );
    assert_eq!(v["args"][0]["name"], "blockers");
    assert_eq!(v["args"][0]["default"], "");
}

#[test]
fn a_setup_built_in_renders_from_the_record_or_from_args() {
    let dir = setup();
    let out = run(dir.path(), &["render", "branch"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        stdout(&out),
        "x/fix",
        "prefix from config, slug from the record"
    );

    std::fs::remove_file(dir.path().join(".devkit/issue.toml")).unwrap();
    let out = run(dir.path(), &["render", "branch"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(stderr(&out).contains("--arg slug=..."), "{out:?}");

    let out = run(dir.path(), &["render", "branch", "--arg", "slug=add-login"]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stdout(&out), "x/add-login");
}

#[test]
fn render_json_carries_the_text() {
    let dir = setup();
    let out = run(dir.path(), &[
        "render", "vibe", "--arg", "mood=ok", "--json",
    ]);
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["text"], "ok");
}

/// The built-in `pr_body` renders from the worktree's own record, and the
/// command's other context keys become args, so the output is the body
/// `issue pr create` hands `gh`.
#[test]
fn a_built_in_renders_what_its_command_would_send() {
    let fake =
        ghfake::Fake::without_pr("[templates]\npr_body = \"Closes {{ issue }}.\\n\\n{{ input }}\"");
    let project = fake.project();
    std::fs::create_dir_all(project.join(".devkit")).unwrap();
    std::fs::write(
        project.join(".devkit/issue.toml"),
        "issue = \"ENG-1\"\nslug = \"fix\"\napps = []\n",
    )
    .unwrap();

    let _ = fake.issue(&[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "Fix it",
        "--pr-body",
        "the details",
    ]);
    let home = tempfile::tempdir().unwrap();
    let rendered = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .current_dir(project)
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_CALLER", "agent")
        .args([
            "template",
            "render",
            "pr_body",
            "--arg",
            "input=the details",
        ])
        .output()
        .unwrap();
    assert!(rendered.status.success(), "{rendered:?}");
    let body = stdout(&rendered);
    assert_eq!(body, "Closes ENG-1.\n\nthe details");
    assert!(
        fake.calls().contains(&format!("--body {body}")),
        "{}",
        fake.calls()
    );
}

/// `commit_message` asks for its parts the way `devkit commit` does: by the
/// command's flags, with only `--subject` required unless the project
/// requires another part.
#[test]
fn show_names_the_commit_message_parts_by_their_devkit_commit_flags() {
    let dir = setup();
    let out = run(dir.path(), &["show", "commit_message", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows: Vec<(String, String, bool)> = v["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["name"].as_str().unwrap().to_string(),
                a["required_of"].as_str().unwrap().to_string(),
                a["required"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(rows, [
        ("--body".to_string(), "never".to_string(), false),
        ("--coauthor".to_string(), "never".to_string(), false),
        ("--subject".to_string(), "always".to_string(), true),
    ]);
}

#[test]
fn render_commit_message_needs_only_what_devkit_commit_needs() {
    let dir = setup();
    let out = run(dir.path(), &[
        "render",
        "commit_message",
        "--arg",
        "subject=fix: x",
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stdout(&out), "fix: x");

    let out = run(dir.path(), &["render", "commit_message"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(stderr(&out).contains("--arg subject=..."), "{out:?}");
    assert!(!stderr(&out).contains("body"), "{out:?}");
}
