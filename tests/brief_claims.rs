//! The brief's claims about rule delivery, each held against the real hooks.
//!
//! A test asserts the brief says a thing, then drives the hook or command that
//! makes it true, so a change to either side fails and names the sentence.
//! `evals/brief` renders the same fixture, and its answer key rests on these.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

const FIXTURE: &str = "tests/fixtures/brief-monorepo";

/// The fixture as a git checkout with its `[rules]` index wired in, and a
/// private state home the hooks record fired rules under.
struct Monorepo {
    root: tempfile::TempDir,
    state: tempfile::TempDir,
}

impl Monorepo {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        devkit_common::git::Git::fixture(root.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        let index = root.path().join("index.json");
        std::fs::copy(format!("{FIXTURE}/index.json"), &index).unwrap();
        let config = std::fs::read_to_string(format!("{FIXTURE}/devkit.toml")).unwrap();
        std::fs::write(
            root.path().join("devkit.toml"),
            // A literal string: a Windows path's backslashes are not escapes.
            format!("{config}\n[rules]\nindex = '{}'\n", index.display()),
        )
        .unwrap();
        Monorepo {
            root,
            state: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn devkit(&self, args: &[&str], stdin: &str) -> Output {
        self.devkit_with(args, stdin, &[])
    }

    fn devkit_with(&self, args: &[&str], stdin: &str, env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
        cmd.args(args)
            .current_dir(self.path())
            .env("HOME", self.state.path())
            .env("XDG_STATE_HOME", self.state.path())
            .env("XDG_CONFIG_HOME", self.state.path().join("config"))
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .env_remove("DEVKIT_CONFIG")
            .env_remove("DEVKIT_ENFORCE_WRITES")
            .env_remove("CURSOR_PROJECT_DIR")
            .env_remove("CURSOR_PLUGIN_ROOT")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        testenv::scrub_identity(&mut cmd);
        let mut child = cmd.spawn().expect("spawn devkit");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("devkit output");
        assert!(
            out.status.success(),
            "devkit {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    /// The brief with its line breaks folded, since it wraps to the terminal
    /// width and a claim can straddle a break.
    fn brief(&self) -> String {
        let out = self.devkit(&["brief"], "");
        String::from_utf8(out.stdout)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// The context `pre-tool-use` adds for one Claude Code tool call, empty
    /// when it adds none.
    fn injected(&self, session: &str, tool: &str, input: Value) -> String {
        let payload = json!({
            "session_id": session,
            "cwd": self.path(),
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "tool_input": input,
        });
        let out = self.devkit(
            &["hook", "pre-tool-use", "--harness", "claude-code"],
            &payload.to_string(),
        );
        additional_context(&out)
    }

    fn file(&self, relative: &str) -> String {
        self.path().join(relative).to_string_lossy().into_owned()
    }
}

fn additional_context(out: &Output) -> String {
    if out.stdout.is_empty() {
        return String::new();
    }
    let v: Value = serde_json::from_slice(&out.stdout).expect("one JSON object");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The severities of the rules in a rendered block, from each bullet's
/// `(severity)` marker.
fn severities(block: &str) -> Vec<&str> {
    block
        .lines()
        .filter_map(|line| line.strip_prefix("- **"))
        .filter_map(|rest| rest.split_once("** (")?.1.split_once(')'))
        .map(|(severity, _)| severity)
        .collect()
}

#[test]
fn an_edit_gets_the_files_must_and_should_rules() {
    let repo = Monorepo::new();
    let brief = repo.brief();
    assert!(
        brief.contains(
            "Before an edit tool writes a file, devkit adds that file's `must` and `should` rules"
        ),
        "{brief}"
    );

    let block = repo.injected(
        "claims-edit",
        "Write",
        json!({"file_path": repo.file("apps/web/src/page.tsx"), "content": "x"}),
    );
    assert!(block.contains("Default to server components"), "{block}");
    assert!(
        !block.contains("Prefer loaders over fetch"),
        "the `can` rule stays out: {block}"
    );
    let found = severities(&block);
    assert!(!found.is_empty(), "{block}");
    assert!(
        found.iter().all(|s| *s == "must" || *s == "should"),
        "{found:?}"
    );
}

#[test]
fn each_rule_is_added_once_per_session() {
    let repo = Monorepo::new();
    let brief = repo.brief();
    assert!(brief.contains("each rule once per session"), "{brief}");

    let edit = |file: &str| {
        repo.injected(
            "claims-once",
            "Edit",
            json!({"file_path": repo.file(file), "old_string": "a", "new_string": "b"}),
        )
    };
    let first = edit("apps/api/src/routes/users.ts");
    assert!(
        first.contains("Validate every request body with zod"),
        "{first}"
    );
    let second = edit("apps/api/src/routes/orders.ts");
    assert!(second.is_empty(), "nothing new governs orders.ts: {second}");
}

#[test]
fn shell_writes_get_no_rules() {
    let repo = Monorepo::new();
    let brief = repo.brief();
    assert!(
        brief.contains("shell writes (`sed -i`, `>`, heredocs) get none"),
        "{brief}"
    );

    for command in [
        "cat > apps/api/src/new.ts <<EOF\nexport const a = 1\nEOF",
        "echo 'export const a = 1' > apps/api/src/new.ts",
        "sed -i 's/a/b/' apps/api/src/routes/users.ts",
    ] {
        let block = repo.injected("claims-shell", "Bash", json!({ "command": command }));
        assert!(block.is_empty(), "{command}: {block}");
    }
}

#[test]
fn the_query_lists_every_severity() {
    let repo = Monorepo::new();
    let brief = repo.brief();
    assert!(
        brief.contains(
            "`devkit rules query --path <file>` lists all of a file's rules, whatever their \
             severity"
        ),
        "{brief}"
    );

    let out = repo.devkit(
        &[
            "rules",
            "query",
            "--path",
            "apps/web/src/page.tsx",
            "--format",
            "json",
        ],
        "",
    );
    let rules: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let severities: Vec<&str> = rules
        .iter()
        .map(|r| r["severity"].as_str().unwrap())
        .collect();
    for severity in ["must", "should", "can"] {
        assert!(severities.contains(&severity), "{severity}: {severities:?}");
    }
}

/// Cursor's manifest hangs `pre-tool-use` off shell execution alone, and the
/// shell stage adds no context for Cursor, so no Cursor edit brings rules.
#[test]
fn cursor_edits_get_no_rules() {
    let repo = Monorepo::new();
    let out = repo.devkit_with(&["brief", "--additional-context"], "", &[(
        "CURSOR_PROJECT_DIR",
        "/cursor",
    )]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let brief = v["additional_context"]
        .as_str()
        .unwrap()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        brief.contains("Cursor edits get no rules added automatically"),
        "{brief}"
    );

    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string("hooks/hooks-cursor.json").unwrap()).unwrap();
    for (event, hooks) in manifest["hooks"].as_object().unwrap() {
        let runs_pre_tool_use = hooks.as_array().unwrap().iter().any(|h| {
            h["command"]
                .as_str()
                .is_some_and(|c| c.contains("pre-tool-use"))
        });
        assert!(
            !runs_pre_tool_use || event == "beforeShellExecution",
            "{event} runs pre-tool-use"
        );
    }

    let payload = json!({
        "hook_event_name": "beforeShellExecution",
        "cursor_version": "1.7.0",
        "command": "cat > apps/api/src/new.ts <<EOF\nexport const a = 1\nEOF",
        "cwd": repo.path(),
    });
    let out = repo.devkit(
        &["hook", "pre-tool-use", "--harness", "cursor"],
        &payload.to_string(),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("Rules for"), "{stdout}");
}

#[test]
fn the_session_start_block_carries_only_repo_wide_rules() {
    let repo = Monorepo::new();
    let out = repo.devkit(&["rules", "context"], "");
    let block = String::from_utf8(out.stdout).unwrap();
    assert!(
        block.starts_with("## Rules for all code in this repository\n"),
        "{block}"
    );

    let out = repo.devkit(
        &[
            "rules",
            "query",
            "--scope",
            "repo",
            "--severity",
            "must",
            "--format",
            "json",
        ],
        "",
    );
    let repo_must: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
    let listed = block.lines().filter(|l| l.starts_with("- **")).count();
    assert_eq!(listed, repo_must.len(), "{block}");
    for rule in &repo_must {
        let title = rule["title"].as_str().unwrap();
        assert!(block.contains(title), "{title}: {block}");
    }
}
