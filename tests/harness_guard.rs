//! End-to-end coverage of `devkit harness shell`: the gate, the deny envelopes,
//! and the fail-open contract. Each test runs in a private temp project with a
//! private HOME so it never reads the developer's real global config.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn project(config: &str) -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(p.path().join("devkit.toml"), config).unwrap();
    p
}

fn run_hook(project: &Path, home: &Path, payload: &str) -> Output {
    run_hook_with(project, home, payload, &[])
}

/// [`run_hook`], plus environment overrides applied after the private-HOME
/// setup, so a caller can force the enforcement env var one way or the other
/// without losing the isolation every test relies on.
fn run_hook_with(project: &Path, home: &Path, payload: &str, extra_env: &[(&str, &str)]) -> Output {
    run_argv(project, home, &["hook", "pre-tool-use"], payload, extra_env)
}

/// The same run under an argument vector of the caller's choosing, so each
/// case can be driven through the retired spelling as well as the current one
/// for as long as the alias exists.
fn run_argv(
    project: &Path,
    home: &Path,
    argv: &[&str],
    payload: &str,
    extra_env: &[(&str, &str)],
) -> Output {
    let exe = Path::new(env!("CARGO_BIN_EXE_devkit"));
    let mut cmd = Command::new(exe);
    cmd.args(argv)
        .current_dir(project)
        .env("HOME", home)
        .env("XDG_STATE_HOME", home)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_COMMANDS")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("spawn the devkit hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

fn claude_payload(command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command }
    })
    .to_string()
}

fn denied(out: &Output) -> bool {
    assert_eq!(
        out.status.code(),
        Some(0),
        "the guard must always exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return false;
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is JSON");
    v["hookSpecificOutput"]["permissionDecision"] == "deny"
        || v["permission"] == "deny"
        || v["decision"] == "deny"
}

const GUARDED: &str = r#"
[harness]
enforce_commands = true

[harness.commands.bun-only]
programs = ["node"]
reason = "This workspace is bun-only."
"#;

const GATE_OFF: &str = r#"
[harness]
enforce_commands = false

[harness.commands.bun-only]
programs = ["node"]
reason = "This workspace is bun-only."
"#;

#[test]
fn a_rule_arg_glob_denies_through_the_binary() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(
        r#"
[harness]
enforce_commands = true

[harness.commands.nitro-via-node]
programs = ["node"]
args = ["*/nitro", "dev"]
reason = "Start the api with devrun up api."
"#,
    );
    let launch = "doppler run -c dev -- node ./node_modules/.bin/nitro dev --port 9200";
    assert!(denied(&run_hook(
        proj.path(),
        home.path(),
        &claude_payload(launch)
    )));
    assert!(!denied(&run_hook(
        proj.path(),
        home.path(),
        &claude_payload("node ./node_modules/.bin/nitro build")
    )));
}

#[test]
fn a_double_star_rule_arg_skips_runner_options_through_the_binary() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(
        r#"
[harness]
enforce_commands = true

[harness.commands.nitro-dev]
programs = ["bun"]
args = ["**", "nitro", "dev"]
reason = "Start the api with devrun up api."
"#,
    );
    let launch = "doppler run -c dev -- bun --cwd /abs/apps/api nitro dev --port 9200";
    assert!(denied(&run_hook(
        proj.path(),
        home.path(),
        &claude_payload(launch)
    )));
    assert!(!denied(&run_hook(
        proj.path(),
        home.path(),
        &claude_payload("bun --cwd /abs/apps/api nitro build")
    )));
}

#[test]
fn a_rule_naming_a_wrapper_denies_through_the_binary() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(
        r#"
[harness]
enforce_commands = true

[harness.commands.sudo]
programs = ["sudo"]
reason = "Never run sudo"

[harness.commands.as-root]
programs = ["sudo"]
args = ["-u", "root"]
reason = "Not as root"
"#,
    );
    for command in [
        "sudo ls",
        "sudo -u root ls",
        "nohup sudo apt-get install jq",
        "sudo devkit todo list",
        "sudo devrun task build",
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        assert!(denied(&out), "{command}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("Never run sudo"),
            "{command}"
        );
    }
    let out = run_hook(proj.path(), home.path(), &claude_payload("sudo -u root ls"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Not as root"));
    let out = run_hook(proj.path(), home.path(), &claude_payload("sudo -u lev ls"));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("Not as root"));
    assert!(!denied(&run_hook(
        proj.path(),
        home.path(),
        &claude_payload("ls")
    )));
}

#[test]
fn a_user_rule_denies_through_the_binary() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let out = run_hook(proj.path(), home.path(), &claude_payload("node server.js"));
    assert!(denied(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("bun-only"),
        "reason reaches the agent: {stdout}"
    );
}

#[test]
fn the_gate_off_allows_everything() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GATE_OFF);
    let out = run_hook(proj.path(), home.path(), &claude_payload("node server.js"));
    assert!(
        !denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The same rule in the same config, with only the env override flipped:
    // proves the assertion above passed because the gate was off, not
    // because the rule never matched anything.
    let out = run_hook_with(
        proj.path(),
        home.path(),
        &claude_payload("node server.js"),
        &[("DEVKIT_ENFORCE_COMMANDS", "1")],
    );
    assert!(
        denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_cursor_payload_gets_the_cursor_envelope() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    // `cursor_version` is what names the harness now: Cursor sends
    // `hook_event_name` and `model` like the other two, so its identity can no
    // longer be read off the absence of either.
    let payload = serde_json::json!({
        "hook_event_name": "beforeShellExecution",
        "cursor_version": "1.7.0",
        "command": "node server.js"
    })
    .to_string();
    let out = run_hook(proj.path(), home.path(), &payload);
    assert!(denied(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(v["permission"], "deny");
    assert!(v["agent_message"].is_string());
}

/// Cursor runs hooks configured for Claude Code and sends them its own
/// camelCase payload, so the payload's shape beats the Claude Code manifest.
#[test]
fn a_cursor_payload_through_the_claude_code_manifest_gets_the_cursor_envelope() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let payload = serde_json::json!({
        "conversation_id": "c1",
        "session_id": "c1",
        "hook_event_name": "preToolUse",
        "cursor_version": "2026.09.26-dd393fe",
        "tool_name": "Shell",
        "tool_input": { "command": "node server.js", "cwd": "" },
        "tool_use_id": "u1",
        "cwd": ""
    })
    .to_string();
    let out = run_argv(
        proj.path(),
        home.path(),
        &["hook", "pre-tool-use", "--harness", "claude-code"],
        &payload,
        &[],
    );
    assert!(denied(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(v["permission"], "deny");
}

/// Antigravity sends no `hook_event_name`, so the verb names the event.
#[test]
fn an_antigravity_run_command_gets_the_antigravity_envelope() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let payload = serde_json::json!({
        "conversationId": "b1aed54b-b201-4432-aaa4-c4403f2863e6",
        "modelName": "gemini-3.8-flash-low",
        "stepIdx": 2,
        "toolCall": {
            "name": "run_command",
            "args": {
                "CommandLine": "node server.js",
                "Cwd": proj.path().to_string_lossy(),
                "WaitMsBeforeAsync": 5000
            }
        },
        "workspacePaths": [proj.path().to_string_lossy()]
    })
    .to_string();
    let out = run_argv(
        proj.path(),
        home.path(),
        &["hook", "pre-tool-use", "--harness", "antigravity"],
        &payload,
        &[],
    );
    assert!(denied(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(v["decision"], "deny");
    assert!(v["reason"].as_str().unwrap().contains("bun-only"), "{v}");
}

/// Grok runs hooks configured for Claude Code and sends them its own payload,
/// so a Claude Code manifest guards a Grok shell call too.
#[test]
fn a_grok_run_terminal_command_is_guarded_under_either_manifest() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let cwd = proj.path().to_string_lossy();
    let input = serde_json::json!({ "command": "node server.js", "description": "serve" });
    let payload = serde_json::json!({
        "hookEventName": "pre_tool_use",
        "hook_event_name": "PreToolUse",
        "sessionId": "g1",
        "session_id": "g1",
        "cwd": cwd,
        "workspaceRoot": cwd,
        "permissionMode": "default",
        "toolName": "run_terminal_command",
        "tool_name": "run_terminal_command",
        "toolUseId": "call_3_0",
        "toolInput": input,
        "tool_input": input,
        "toolInputTruncated": false
    })
    .to_string();
    for harness in ["grok", "claude-code"] {
        let out = run_argv(
            proj.path(),
            home.path(),
            &["hook", "pre-tool-use", "--harness", harness],
            &payload,
            &[],
        );
        assert!(denied(&out), "{harness}");
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    }
}

#[test]
fn an_unparseable_config_fails_open() {
    // The gate must be forced on: the only layer is unparseable, so no layer
    // opts in, the gate would read false on its own, and this would prove only
    // that the gate-off path works.
    let home = tempfile::tempdir().unwrap();
    let proj = project("[[[ not toml");
    let out = run_hook_with(
        proj.path(),
        home.path(),
        &claude_payload("node server.js"),
        &[("DEVKIT_ENFORCE_COMMANDS", "1")],
    );
    assert!(!denied(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("devkit.toml failed to load"),
        "a broken config must be reported, not silently dropped: {stderr}"
    );
}

#[test]
fn the_env_override_disables_the_guard() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let out = run_hook_with(
        proj.path(),
        home.path(),
        &claude_payload("node server.js"),
        &[("DEVKIT_ENFORCE_COMMANDS", "0")],
    );
    assert!(!denied(&out));
}

#[test]
fn a_garbage_payload_exits_zero_and_says_nothing() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let out = run_hook(proj.path(), home.path(), "not json at all");
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
}

#[test]
fn a_task_denies_through_the_binary_and_writes_no_registry_row() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(&format!(
        "{GUARDED}\n[tasks.check]\nrun = [\"bun\", \"test\"]\napp = \"web\"\n\
         [apps.web]\nbase_port = 3000\npath = \"apps/web\"\nlaunch = [\"vite\"]\n"
    ));
    std::fs::create_dir_all(proj.path().join("apps/web")).unwrap();

    // Assert the denial first: without it, the no-registry-row assertion
    // below would also pass against a project whose config failed to
    // resolve, proving nothing about the guard.
    let out = run_hook(proj.path(), home.path(), &claude_payload("bun test"));
    assert!(
        denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("devrun task check"));

    let ports = home.path().join("devkit/ports.json");
    assert!(
        !ports.exists(),
        "the guard allocated a port: {}",
        ports.display()
    );
}

#[test]
fn a_redirect_names_the_args_the_task_requires() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(
        &(GUARDED.to_string()
            + r#"
[tasks.commit]
run = ["git", "commit", "-m", "{{ msg }}"]
guard = true
"#),
    );
    let out = run_hook(
        proj.path(),
        home.path(),
        &claude_payload("git commit -m wip"),
    );
    assert!(
        denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("devrun task commit --arg msg="), "{stdout}");
}

const WORKSPACE_TASKS: &str = r#"
[tasks.build]
run = ["cargo", "build", "--workspace", "--locked"]
guard = true

[tasks.check]
run = ["cargo", "check", "--workspace", "--all-targets", "--all-features", "--locked"]
guard = true

[tasks.test]
run = ["cargo", "nextest", "run", "--workspace", "--all-features", "--locked", "--no-fail-fast"]
guard = true

[tasks.test-doc]
run = ["cargo", "test", "--doc", "--workspace", "--all-features", "--locked"]
guard = true

[tasks.lint]
run = ["cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--", "-D", "warnings"]
guard = true
"#;

#[test]
fn a_command_asking_for_more_than_a_task_does_is_not_redirected() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(&(GUARDED.to_string() + WORKSPACE_TASKS));
    for typed in [
        "cargo test -p devkit-locks --test registry",
        "cargo build -p devkit-locks",
        "cargo check -p devkit-locks",
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(typed));
        assert!(
            !denied(&out),
            "{typed}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn a_command_within_a_task_is_redirected_to_it() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(&(GUARDED.to_string() + WORKSPACE_TASKS));
    for (typed, task) in [
        ("cargo test --workspace --doc", "test-doc"),
        ("cargo build", "build"),
        ("cargo nextest run --workspace", "test"),
        (
            "cargo clippy --workspace --all-targets -- -D warnings",
            "lint",
        ),
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(typed));
        assert!(denied(&out), "{typed} was allowed");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(&format!("devrun task {task}`")),
            "{typed}: {stdout}"
        );
    }
}

#[test]
fn the_cwd_names_the_app_for_a_catalog_hit() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(&format!(
        "{GUARDED}\n[apps.web]\nbase_port = 3000\npath = \"apps/web\"\n\
         launch = [\"vite\", \"--port\", \"{{{{ port }}}}\"]\n"
    ));
    let app_dir = proj.path().join("apps/web");
    std::fs::create_dir_all(&app_dir).unwrap();
    let out = run_hook(&app_dir, home.path(), &claude_payload("vite"));
    assert!(
        denied(&out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("devrun up web"));
}

/// Every case the retired spelling covers must answer identically under the
/// current one, because the manifests move to it in the same release.
#[test]
fn both_spellings_answer_identically() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let payload = claude_payload("node server.js");
    let forced = [("DEVKIT_ENFORCE_COMMANDS", "1")];
    let old = run_argv(
        proj.path(),
        home.path(),
        &["harness", "shell"],
        &payload,
        &forced,
    );
    let new = run_argv(
        proj.path(),
        home.path(),
        &["hook", "pre-tool-use"],
        &payload,
        &forced,
    );
    assert!(
        denied(&new),
        "the guard still denies under the new spelling"
    );
    assert_eq!(old.status.code(), new.status.code());
    assert_eq!(
        String::from_utf8_lossy(&old.stdout),
        String::from_utf8_lossy(&new.stdout)
    );
}

/// The alias spelled the way every installed `lockm hook` manifest spells it.
#[test]
fn the_one_word_alias_answers_the_same_way() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    let payload = claude_payload("node server.js");
    let forced = [("DEVKIT_ENFORCE_COMMANDS", "1")];
    let kebab = run_argv(
        proj.path(),
        home.path(),
        &["hook", "pre-tool-use"],
        &payload,
        &forced,
    );
    let one_word = run_argv(
        proj.path(),
        home.path(),
        &["hook", "pretooluse"],
        &payload,
        &forced,
    );
    assert_eq!(
        String::from_utf8_lossy(&kebab.stdout),
        String::from_utf8_lossy(&one_word.stdout)
    );
}

/// `--harness` wins over inference, and the envelope is the named harness's.
#[test]
fn the_declared_harness_picks_the_envelope() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(GUARDED);
    // Cursor's tool fields with neither its event nor `cursor_version`, which
    // inference alone reads as Claude Code.
    let payload = serde_json::json!({
        "tool_name": "Shell",
        "tool_input": { "command": "node server.js" }
    })
    .to_string();
    let out = run_argv(
        proj.path(),
        home.path(),
        &["hook", "pre-tool-use", "--harness", "cursor"],
        &payload,
        &[("DEVKIT_ENFORCE_COMMANDS", "1")],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    assert_eq!(
        v["permission"], "deny",
        "the Cursor envelope, not clauded's"
    );
    assert!(v["agent_message"].is_string());
}

/// The context lines an allowed command came back with, none when silent.
fn notes(out: &Output) -> Vec<String> {
    assert!(
        !denied(out),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if out.stdout.iter().all(u8::is_ascii_whitespace) {
        return Vec::new();
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout is JSON");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .lines()
        .map(str::to_string)
        .collect()
}

const CARGO_BUILD_TASK: &str = r#"
[harness]
enforce_commands = true

[harness.commands.bun-only]
programs = ["node"]
args = ["server.js"]
reason = "This workspace is bun-only."

[tasks.build]
run = ["cargo", "build"]

[apps.web]
base_port = 3000
path = "apps/web"
launch = ["nitro", "dev"]
"#;

const PROJECT_NOTE: &str = "devkit did not check it against the project's tasks and apps.";

#[test]
fn a_loop_repeats_no_unresolved_note() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(CARGO_BUILD_TASK);
    let out = run_hook(
        proj.path(),
        home.path(),
        &claude_payload(r#"for p in a b c; do cargo build "$(date)"; node "$y"; done"#),
    );
    let notes = notes(&out);
    let mut distinct = notes.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(notes.len(), distinct.len(), "{notes:#?}");
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("`cargo build \"$(date)\"`") && n.ends_with(PROJECT_NOTE)),
        "{notes:#?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("`[harness.commands]` rule")),
        "{notes:#?}"
    );
}

#[test]
fn an_unresolved_program_no_task_or_app_runs_gets_no_project_note() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(CARGO_BUILD_TASK);
    for command in [r#"printf %s "$q""#, r#"jq -r .x <<<"$base""#] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        assert_eq!(notes(&out), Vec::<String>::new(), "{command}");
    }
}

#[test]
fn an_unresolved_program_a_task_or_app_could_run_keeps_its_project_note() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(CARGO_BUILD_TASK);
    for command in [
        r#"cargo build "$x""#,
        r#"nitro "$x""#,
        r#"vite "$x""#,
        r#""$tool" build"#,
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        let notes = notes(&out);
        assert!(
            notes.iter().any(|n| n.ends_with(PROJECT_NOTE)),
            "{command}: {notes:#?}"
        );
    }
}

fn deny_reason(out: &Output) -> String {
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("a denial is a JSON envelope");
    v["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn an_agents_gh_pr_create_is_denied_in_any_devkit_project_naming_devkit_pr_create() {
    let home = tempfile::tempdir().unwrap();
    for config in ["[defaults]\n", GATE_OFF, GUARDED] {
        let proj = project(config);
        for command in [
            "gh pr create --title t --body b",
            "cd sub && gh pr create --fill",
            "gh  pr   new --fill",
        ] {
            let out = run_hook(proj.path(), home.path(), &claude_payload(command));
            assert!(denied(&out), "{config}: {command}");
            assert!(
                deny_reason(&out).contains("devkit pr create"),
                "{config}: {command}: {}",
                deny_reason(&out)
            );
        }
        let view = run_hook(proj.path(), home.path(), &claude_payload("gh pr view 9"));
        assert!(!denied(&view), "{config}: only pr create is refused");
    }
}

#[test]
fn a_project_rule_naming_another_caller_spelling_still_matches() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(
        r#"
[harness]
enforce_commands = true

[harness.commands.devkit-caller-human]
enabled = false

[harness.commands.no-human-env]
programs = ["env"]
args = ["DEVKIT_CALLER=HUMAN"]
reason = "project rule refuses the human caller"
"#,
    );
    for command in [
        "env DEVKIT_CALLER=HUMAN true",
        "env DEVKIT_CALLER=human true",
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        assert!(denied(&out), "{command}");
        assert!(
            deny_reason(&out).contains("project rule refuses"),
            "{command}: {}",
            deny_reason(&out)
        );
    }
}

#[test]
fn a_project_rule_with_a_wildcard_caller_pattern_matches_the_raw_word() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(
        r#"
[harness]
enforce_commands = true

[harness.commands.devkit-caller-human]
enabled = false

[harness.commands.no-human-env]
programs = ["env"]
args = ["DEVKIT_CALLER=HUM*"]
reason = "project rule refuses the human caller"
"#,
    );
    let out = run_hook(
        proj.path(),
        home.path(),
        &claude_payload("env DEVKIT_CALLER=HUMAN true"),
    );
    assert!(denied(&out));
    assert!(
        deny_reason(&out).contains("project rule refuses"),
        "{}",
        deny_reason(&out)
    );
}

#[test]
fn a_gh_pr_create_beside_a_todo_command_is_denied_with_every_gate_off() {
    let home = tempfile::tempdir().unwrap();
    let proj = project("[harness]\nenforce_commands = false\nenforce_writes = false\n");
    for command in [
        "devkit todo add x && gh pr create --fill",
        "gh pr create --body todo",
        "DEVKIT_CALLER=human gh pr create --body 'fixes .todo'",
    ] {
        let payload = serde_json::json!({
            "session_id": "s1",
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command }
        })
        .to_string();
        let out = run_hook(proj.path(), home.path(), &payload);
        assert!(denied(&out), "{command}");
        assert!(
            deny_reason(&out).contains("devkit pr create"),
            "{command}: {}",
            deny_reason(&out)
        );
    }
}

#[test]
fn a_powershell_gh_pr_create_is_denied_with_every_gate_off() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(
        "[harness]\nenforce_commands = false\nenforce_writes = false\nshell = \"powershell\"\n",
    );
    for command in [
        r#"$x = "gh"; & $x pr create --fill"#,
        r#"& "g`h" pr create"#,
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        assert!(denied(&out), "{command}");
        assert!(
            deny_reason(&out).contains("devkit pr create"),
            "{command}: {}",
            deny_reason(&out)
        );
    }
}

#[test]
fn a_gh_pr_create_inside_an_appending_assignment_is_denied() {
    let home = tempfile::tempdir().unwrap();
    let proj = project("[defaults]\n");
    for command in ["X+=$(gh pr create) true", "X+=$(gh pr create)"] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        assert!(denied(&out), "{command}");
    }
}

#[test]
fn a_project_rule_switches_the_gh_pr_create_guard_off() {
    let home = tempfile::tempdir().unwrap();
    let proj = project("[harness.commands.gh-pr-create]\nenabled = false\n");
    let out = run_hook(
        proj.path(),
        home.path(),
        &claude_payload("gh pr create --fill"),
    );
    assert!(!denied(&out), "{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn gh_pr_create_outside_a_devkit_project_is_left_alone() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(dir.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    let out = run_hook(
        dir.path(),
        home.path(),
        &claude_payload("gh pr create --fill"),
    );
    assert!(!denied(&out), "{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn a_global_rule_named_like_a_builtin_is_off_outside_a_devkit_project() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(dir.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    let global = home.path().join("global.toml");
    std::fs::write(
        &global,
        "[harness.commands.gh-pr-create]\nprograms = [\"gh\"]\nreason = \"no gh\"\n",
    )
    .unwrap();
    let out = run_hook_with(
        dir.path(),
        home.path(),
        &claude_payload("gh issue list"),
        &[("DEVKIT_CONFIG", global.to_str().unwrap())],
    );
    assert!(!denied(&out), "{}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn an_agent_declaring_itself_human_is_denied_in_any_devkit_project() {
    let home = tempfile::tempdir().unwrap();
    for config in ["[defaults]\n", GATE_OFF, GUARDED] {
        let proj = project(config);
        for command in [
            "DEVKIT_CALLER=human devkit pr create",
            "env DEVKIT_CALLER=human devkit pr create",
            "env -u CLAUDE_CODE_SESSION_ID DEVKIT_CALLER=human devkit pr create",
            "env -u CLAUDE_CODE_SESSION_ID -u CODEX_SESSION_ID DEVKIT_CALLER=human devkit pr create",
            "export DEVKIT_CALLER=human",
            "export DEVKIT_CALLER=human && devkit pr create",
            "declare -x DEVKIT_CALLER=human",
            "local DEVKIT_CALLER=human",
            "f() { local -x DEVKIT_CALLER=human; devkit pr create; }; f",
            r#"DEVKIT_CALLER="human" devkit pr create"#,
            "FOO=1 DEVKIT_CALLER='human' devkit pr create",
            "bash -c 'DEVKIT_CALLER=human devkit pr create'",
            r#"sh -c "export DEVKIT_CALLER=human; devkit pr create""#,
            "who=human; DEVKIT_CALLER=$who devkit pr create",
            "who=human; export who=agent DEVKIT_CALLER=$who",
            "who=human; declare -x who=agent DEVKIT_CALLER=\"$who\"",
            "DEVKIT_CALLER=human; export DEVKIT_CALLER",
            "DEVKIT_CALLER=HUMAN devkit pr create",
            r#"DEVKIT_CALLER=" human" devkit pr create"#,
            "env DEVKIT_CALLER=Human devkit pr create",
            "export DEVKIT_CALLER='human '",
            "env  -u  CLAUDE_CODE_SESSION_ID   DEVKIT_CALLER=human  devkit pr create",
            "env -u CLAUDE_CODE_SESSION_ID -u CODEX_SESSION_ID -- DEVKIT_CALLER=human devkit pr create",
            "env -- DEVKIT_CALLER=human devkit pr create",
        ] {
            let out = run_hook(proj.path(), home.path(), &claude_payload(command));
            assert!(denied(&out), "{config}: {command}");
            assert!(
                deny_reason(&out).contains("DEVKIT_CALLER"),
                "{config}: {command}: {}",
                deny_reason(&out)
            );
        }
    }
}

#[test]
fn setting_devkit_caller_to_agent_or_unsetting_a_session_is_allowed() {
    let home = tempfile::tempdir().unwrap();
    let proj = project("[defaults]\n");
    for command in [
        "DEVKIT_CALLER=agent devkit pr create",
        "env DEVKIT_CALLER=agent devkit pr create",
        "export DEVKIT_CALLER=agent",
        "env -u CLAUDE_CODE_SESSION_ID devkit pr create",
        "unset CLAUDE_CODE_SESSION_ID",
        "echo DEVKIT_CALLER=human",
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        assert!(!denied(&out), "{command}: {}", deny_reason(&out));
    }
}

#[test]
fn an_unresolved_assignment_adds_no_rule_note() {
    let home = tempfile::tempdir().unwrap();
    let proj = project("[defaults]\n");
    for command in [
        r#"FOO="$(date)" cargo build"#,
        r#"export PATH="$PATH:/opt/bin""#,
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        assert_eq!(notes(&out), Vec::<String>::new(), "{command}");
    }
}

#[test]
fn a_project_rule_switches_the_devkit_caller_guard_off() {
    let home = tempfile::tempdir().unwrap();
    let proj = project("[harness.commands.devkit-caller-human]\nenabled = false\n");
    for command in [
        "DEVKIT_CALLER=human devkit pr create",
        "export DEVKIT_CALLER=human",
    ] {
        let out = run_hook(proj.path(), home.path(), &claude_payload(command));
        assert!(!denied(&out), "{command}: {}", deny_reason(&out));
    }
}

#[test]
fn a_builtin_set_to_warn_adds_its_note_with_enforcement_off() {
    let home = tempfile::tempdir().unwrap();
    let proj = project(
        "[harness]\nenforce_commands = false\n\n[harness.commands.gh-pr-create]\naction = \"warn\"\n",
    );
    let out = run_hook(
        proj.path(),
        home.path(),
        &claude_payload("gh pr create --fill"),
    );
    let notes = notes(&out);
    assert!(
        notes.iter().any(|n| n.contains("devkit pr create")),
        "{notes:?}"
    );
}
