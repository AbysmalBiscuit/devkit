# Hook surface unification and harness logging

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `devkit harness shell` and `lockm hook <event>` with one `devkit hook <event>` family covering every event the three harnesses fire, and hang an off-by-default JSONL log off it that captures what agents try to run and what devkit decided.

**Architecture:** A clap `ValueEnum` of hook verbs in `src/bin/devkit/hook/`, dispatching on the payload's `tool_name` to the existing shell guard or the existing lock-claim path. `devkit_common::harness_log` owns record types, redaction, the writer and the prune sweep behind one infallible entry point, so a logging failure can never reach the hook's verdict. Config lives in a new `[harness.log]` table whose enabling keys are global-only.

**Tech Stack:** Rust 2024, clap 4 (`ValueEnum`, `try_get_matches`), serde/serde_json, `fd-lock`, `schemars`, `tempfile` + `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-09-15-hook-surface-and-logging-design.md`

## Global Constraints

- One pull request. Tasks land as separate commits on `69-feat-add-ability-to-log-telemetry`.
- The gate is `devrun task test`, `devrun task test-doc`, `devrun task lint` and `devrun task fmt-check`, all green before each commit. Never call `cargo nextest` or `cargo test` directly; the command guard refuses them.
- **No verb in the `hook` family ever exits 2.** Exit 2 blocks the tool call on Claude Code `PreToolUse` and on Codex (`codex-rs/hooks/src/events/pre_tool_use.rs:261`). Usage errors, unknown verbs and panics all exit 1 with a message on stderr.
- **Only `pre-tool-use` writes to stdout.** Every other verb writes nothing and exits 0. `UserPromptSubmit` appends stdout to the prompt, and `Stop` and `PermissionRequest` honour a JSON decision, so a stray `println!` changes agent behaviour.
- **The command guard fails open; the write stage fails closed.** Unchanged from AGENTS.md. Logging is outside both: `harness_log::record` returns `()` and can never alter a verdict.
- **The envelope is printed and flushed before any record is written.** A blocked write before the envelope exists runs into the manifest's 30-second timeout, and a harness timeout allows the call.
- Commit messages follow Conventional Commits. Co-author line: `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- Test scratch comes from `tempfile`; bind the `TempDir` guard for as long as its paths are used.
- `devkit-common` does not gain a dependency on `devkit-command`. It would pull six tree-sitter C grammars into every library crate.

## File structure

| File | Responsibility |
|---|---|
| `crates/devkit-locks/src/hook.rs` | Modify: `HookEvent` becomes `LockAction`; `parse_event` splits into three per-verb functions |
| `crates/devkit-common/src/harness.rs` | Modify: Cursor detection, `Shell` tool name, per-harness id and cwd reading |
| `src/bin/devkit/hook/mod.rs` | Create: the `HookEvent` clap enum, `--harness`, the never-exit-2 wrapper, verb dispatch |
| `src/bin/devkit/hook/shell.rs` | Create: the shell path, moved from `harness/mod.rs` |
| `src/bin/devkit/hook/edit.rs` | Create: the edit path, moved from `locks.rs::run_hook` |
| `src/bin/devkit/hook/record.rs` | Create: payload to `Record` mapping, including the `Analysis` projection |
| `crates/devkit-config/src/harness.rs` | Modify: the `[harness.log]` table as `LogSection` |
| `crates/devkit-common/src/harness_log/mod.rs` | Create: `record`, the `Record` types, the resolved settings |
| `crates/devkit-common/src/harness_log/redact.rs` | Create: redaction and hashing |
| `crates/devkit-common/src/harness_log/writer.rs` | Create: path layout, id sanitising, the append |
| `crates/devkit-common/src/harness_log/prune.rs` | Create: the age and size sweeps and their guards |
| `src/bin/devkit/hook_log.rs` | Create: `devkit hook-log path|prune` |
| `hooks/hooks.json`, `hooks-codex.json`, `hooks-cursor.json` | Modify: every verb, with `--harness` |

---

### Task 1: Rename the locks hook enum and split `parse_event`

The binary's new clap enum will also be called `HookEvent`, and a library cannot import it, so the string-matching entry point is replaced by three per-verb functions.

**Files:**
- Modify: `crates/devkit-locks/src/hook.rs:37-135`
- Modify: `src/bin/devkit/locks.rs:227-272` (the `run_hook` match)
- Test: `crates/devkit-locks/src/hook.rs` (the existing `mod tests`)

**Interfaces:**
- Produces: `LockAction` (was `HookEvent`), with variants `Write { tool_name, file_paths, holder }`, `ReleaseSubagent { holder }`, `ReleaseSession { holder }`, `Unusable { reason }`. No `Ignore` variant.
- Produces: `parse_write(&Value) -> Option<LockAction>`, `parse_subagent_stop(&Value) -> Option<LockAction>`, `parse_session_end(&Value) -> Option<LockAction>`. `None` replaces `Ignore`.
- Produces: `is_write_tool(tool: &str) -> bool`, so the verb dispatch in Task 3 picks the edit path over the shell one without owning `WRITE_TOOLS`.

- [ ] **Step 1: Write the failing tests**

Add to `crates/devkit-locks/src/hook.rs`, inside `mod tests`:

```rust
#[test]
fn a_non_write_tool_is_not_a_write() {
    let p = json!({"tool_name": "Read", "session_id": "s1"});
    assert!(parse_write(&p).is_none());
}

#[test]
fn a_write_with_no_session_is_unusable_not_ignored() {
    let p = json!({"tool_name": "Write", "tool_input": {"file_path": "a.rs"}});
    assert!(matches!(parse_write(&p), Some(LockAction::Unusable { .. })));
}

#[test]
fn a_subagent_stop_needs_both_ids() {
    assert!(parse_subagent_stop(&json!({"session_id": "s1"})).is_none());
    let both = json!({"session_id": "s1", "agent_id": "a1"});
    assert!(matches!(
        parse_subagent_stop(&both),
        Some(LockAction::ReleaseSubagent { holder }) if holder == "s1/a1"
    ));
}

#[test]
fn a_session_end_needs_a_session_id() {
    assert!(parse_session_end(&json!({})).is_none());
    assert!(matches!(
        parse_session_end(&json!({"session_id": "s1"})),
        Some(LockAction::ReleaseSession { holder }) if holder == "s1"
    ));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`
Expected: FAIL, `cannot find function parse_write in this scope`.

- [ ] **Step 3: Rename the enum and split the function**

In `crates/devkit-locks/src/hook.rs`, rename `HookEvent` to `LockAction`, delete the `Ignore` variant, and replace `parse_event` with:

```rust
/// A `PreToolUse` payload's write intent. `None` when the tool does not write,
/// which is the common case and not a failure.
pub fn parse_write(p: &Value) -> Option<LockAction> {
    let tool = str_field(p, "tool_name").unwrap_or("");
    if !WRITE_TOOLS.contains(&tool) {
        return None;
    }
    let Some(session) = str_field(p, "session_id") else {
        return Some(LockAction::Unusable {
            reason: "write payload carries no session_id".into(),
        });
    };
    let input = p.get("tool_input");
    let file_paths = if tool == APPLY_PATCH {
        input
            .and_then(|ti| str_field(ti, "command"))
            .map(apply_patch_paths)
            .unwrap_or_default()
    } else {
        input
            .and_then(|ti| str_field(ti, "file_path"))
            .map(|fp| vec![fp.to_string()])
            .unwrap_or_default()
    };
    if file_paths.is_empty() {
        return Some(LockAction::Unusable {
            reason: format!("write payload for {tool} names no target"),
        });
    }
    Some(LockAction::Write {
        tool_name: tool.to_string(),
        file_paths,
        holder: holder_from_fields(session, str_field(p, "agent_id")),
    })
}

/// Releasing the bare session holder here would free the parent's and every
/// sibling's locks, so an unattributable stop releases nothing.
pub fn parse_subagent_stop(p: &Value) -> Option<LockAction> {
    let session = str_field(p, "session_id")?;
    let agent = str_field(p, "agent_id")?;
    Some(LockAction::ReleaseSubagent {
        holder: holder_from_fields(session, Some(agent)),
    })
}

pub fn parse_session_end(p: &Value) -> Option<LockAction> {
    Some(LockAction::ReleaseSession {
        holder: str_field(p, "session_id")?.to_string(),
    })
}

/// Whether this tool's writes the harness governs. The `hook` verb dispatch
/// asks before choosing the edit path over the shell one.
pub fn is_write_tool(tool: &str) -> bool {
    WRITE_TOOLS.contains(&tool)
}
```

- [ ] **Step 4: Update the caller**

In `src/bin/devkit/locks.rs`, `run_hook` matches on the event string and calls the matching parser, then matches `Option<LockAction>` with `None => {}` where `HookEvent::Ignore => {}` was.

- [ ] **Step 5: Run the gate**

Run: `devrun task test -C <worktree>` then `devrun task lint -C <worktree>`
Expected: PASS, zero warnings.

- [ ] **Step 6: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-locks/src/hook.rs,src/bin/devkit/locks.rs' \
  --arg commit_subject='refactor(locks): split parse_event by verb' \
  --arg commit_body='The binary gains a clap HookEvent of its own, and two types
under one name in one workspace is a reading hazard. Rename the
locks enum to LockAction and replace the string match with one
parser per verb, so an unrecognised event has nowhere to arrive.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 2: Fix Cursor detection and read per-harness payload fields

Cursor sends `hook_event_name` and `model` on every hook, so `parse_shell_payload` resolves Cursor payloads as Codex today and answers them with an envelope Cursor rejects.

**Files:**
- Modify: `crates/devkit-common/src/harness.rs:290-355`
- Test: `crates/devkit-common/src/harness.rs` (`mod tests`)

**Interfaces:**
- Produces: `ShellPayload` unchanged in shape. `harness` now resolves from `cursor_version`; `session_id` falls back to `conversation_id`; `cwd` falls back to `tool_input.working_directory`.
- Produces: `SHELL_TOOLS` gains `"Shell"`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_cursor_payload_is_not_codex() {
    let p = json!({
        "hook_event_name": "preToolUse",
        "cursor_version": "1.7.0",
        "model": "claude-4.5-sonnet",
        "conversation_id": "c1",
        "tool_name": "Shell",
        "tool_input": {"command": "npm install", "working_directory": "/w"}
    });
    let parsed = parse_shell_payload(&p).expect("a Shell payload is a shell payload");
    assert_eq!(parsed.harness, Harness::Cursor);
    assert_eq!(parsed.session_id.as_deref(), Some("c1"));
    assert_eq!(parsed.cwd.as_deref(), Some(Path::new("/w")));
}

#[test]
fn codex_is_still_codex() {
    let p = json!({
        "hook_event_name": "PreToolUse", "turn_id": "t1", "model": "gpt-5",
        "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": "/w"
    });
    assert_eq!(parse_shell_payload(&p).unwrap().harness, Harness::Codex);
}

#[test]
fn claude_code_is_still_claude_code() {
    let p = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": "/w"
    });
    assert_eq!(parse_shell_payload(&p).unwrap().harness, Harness::ClaudeCode);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`
Expected: FAIL on `a_cursor_payload_is_not_codex`, left `Codex` right `Cursor`. That failure is the live bug.

- [ ] **Step 3: Implement**

Replace the detection block and the field reads in `parse_shell_payload`:

```rust
const SHELL_TOOLS: [&str; 3] = ["Bash", "PowerShell", "Shell"];

// Cursor is detected by a field it sends rather than by one it omits: it
// sends `hook_event_name` and `model` like the other two, so absence
// resolved every Cursor payload to Codex.
let harness = if p.get("cursor_version").is_some() {
    Harness::Cursor
} else if p.get("turn_id").is_some() || p.get("model").is_some() {
    Harness::Codex
} else {
    Harness::ClaudeCode
};
```

and, after `command` is read:

```rust
let working_dir = p
    .get("tool_input")
    .and_then(|ti| ti.get("working_directory"))
    .and_then(Value::as_str)
    .filter(|s| !s.is_empty())
    .map(PathBuf::from);
Some(ShellPayload {
    harness,
    tool_name,
    command,
    cwd: text("cwd").map(PathBuf::from).or(working_dir),
    session_id: text("session_id").or_else(|| text("conversation_id")),
    agent_id: text("agent_id").or_else(|| text("parent_conversation_id")),
})
```

Rewrite the doc comment above `parse_shell_payload`: it currently states that Cursor sends no `hook_event_name`, which is the opposite of what Cursor sends.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-common/src/harness.rs' \
  --arg commit_subject='fix(harness): detect cursor by a field it sends' \
  --arg commit_body='Cursor sends hook_event_name and model on every hook, so
detecting it by their absence resolved every Cursor payload to
Codex and answered it with an envelope Cursor rejects. Detect
cursor_version instead, accept the Shell tool name, and read the
conversation id and working directory Cursor sends in place of
session_id and cwd.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 3: The `hook` verb family and the never-exit-2 wrapper

**Files:**
- Create: `src/bin/devkit/hook/mod.rs`
- Modify: `src/bin/devkit/main.rs:47-140` (add the `Hook` variant and its dispatch arm)
- Test: `tests/hook_exit_codes.rs`

**Interfaces:**
- Produces: `pub struct HookCli { pub event: HookEvent, pub harness: Option<HarnessArg> }`
- Produces: `pub enum HookEvent` with the seventeen variants from the spec.
- Produces: `pub fn run(cli: HookCli) -> Result<()>`, and `pub fn main_entry(argv) -> !`, the wrapper that turns a parse failure into exit 1.

- [ ] **Step 1: Write the failing test**

Create `tests/hook_exit_codes.rs`:

```rust
//! Exit 2 blocks the tool call on Claude Code and Codex, so no verb in the
//! `hook` family may reach it, whatever the argument error.

use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(args)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn devkit")
}

#[test]
fn an_unknown_verb_exits_one_with_a_message() {
    let out = run(&["hook", "pre-tool-yoose"]);
    assert_eq!(out.status.code(), Some(1), "exit 2 would block the tool call");
    assert!(
        !String::from_utf8_lossy(&out.stderr).is_empty(),
        "an unrecognised verb is an error, not silence"
    );
}

#[test]
fn an_unknown_harness_exits_one() {
    let out = run(&["hook", "pre-tool-use", "--harness", "emacs"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn a_missing_verb_exits_one() {
    assert_eq!(run(&["hook"]).status.code(), Some(1));
}

#[test]
fn the_pretooluse_alias_still_resolves() {
    let out = run(&["hook", "pretooluse"]);
    assert_ne!(out.status.code(), Some(1), "the installed manifests spell it this way");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `devrun task test -C <worktree>`
Expected: FAIL, exit 2 from clap's unrecognised-subcommand error.

- [ ] **Step 3: Implement the enum and the wrapper**

Create `src/bin/devkit/hook/mod.rs`:

```rust
//! `devkit hook <event>`: every coding-agent hook event enters here.
//!
//! Exit codes are a control channel. Exit 2 blocks the tool call on Claude
//! Code `PreToolUse` and on Codex, so a usage error, an unknown verb and a
//! panic all exit 1 instead. Only `pre-tool-use` writes to stdout; every
//! other verb is silent, because `UserPromptSubmit` appends stdout to the
//! prompt and `Stop` honours a JSON decision.

mod edit;
mod record;
mod shell;

use anyhow::Result;
use clap::{Args, ValueEnum};
use devkit_common::harness::Harness;

#[derive(Args)]
pub struct HookCli {
    /// Which event the harness is reporting.
    pub event: HookEvent,
    /// Which harness sent it. Beats inferring from the payload's shape.
    #[arg(long)]
    pub harness: Option<HarnessArg>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum HarnessArg {
    ClaudeCode,
    Codex,
    Cursor,
}

impl From<HarnessArg> for Harness {
    fn from(a: HarnessArg) -> Self {
        match a {
            HarnessArg::ClaudeCode => Harness::ClaudeCode,
            HarnessArg::Codex => Harness::Codex,
            HarnessArg::Cursor => Harness::Cursor,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum HookEvent {
    #[value(alias = "pretooluse")]
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    SessionStart,
    SessionEnd,
    SubagentStart,
    SubagentStop,
    PermissionRequest,
    PermissionDenied,
    Stop,
    StopFailure,
    PreCompact,
    PostCompact,
    CwdChanged,
    WorktreeCreate,
    WorktreeRemove,
    UserPromptSubmit,
}

pub fn run(cli: HookCli) -> Result<()> {
    let harness = cli.harness.map(Harness::from);
    match cli.event {
        HookEvent::PreToolUse => pre_tool_use(harness),
        HookEvent::SubagentStop => edit::release_subagent(),
        HookEvent::SessionEnd => edit::release_session(),
        _ => Ok(()),
    }
}

fn pre_tool_use(harness: Option<Harness>) -> Result<()> {
    let Some(payload) = read_payload() else {
        return shell::deny_unreadable_payload();
    };
    match payload.get("tool_name").and_then(serde_json::Value::as_str) {
        Some(t) if devkit_locks::hook::is_write_tool(t) => edit::guard(&payload),
        _ => shell::guard(&payload, harness),
    }
}
```

The exit-1 wrapper goes in `main.rs`, where argument parsing happens. Parse with `try_get_matches`; when the failing invocation's first positional is `hook`, print the error to stderr and `std::process::exit(1)` rather than letting clap exit 2. Wrap `hook::run` in `catch_unwind` for the same reason: a panic exits 101, which aborts a `WorktreeCreate`.

- [ ] **Step 4: Register the subcommand**

In `src/bin/devkit/main.rs`, add to `enum Cmd`:

```rust
    /// Coding-agent hook events.
    #[command(display_name = "devkit hook")]
    Hook(hook::HookCli),
```

and the dispatch arm `Cmd::Hook(c) => hook::run(c),`.

- [ ] **Step 5: Run the test to verify it passes**

Run: `devrun task test -C <worktree>`
Expected: PASS on all four.

- [ ] **Step 6: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='src/bin/devkit/hook/mod.rs,src/bin/devkit/main.rs,tests/hook_exit_codes.rs' \
  --arg commit_subject='feat(hook): add the devkit hook verb family' \
  --arg commit_body='One verb family for every harness event, with --harness so
each installed manifest names the harness that reads it rather
than having devkit infer it.

No verb exits 2. Exit 2 blocks the tool call on Claude Code
PreToolUse and on Codex, so a usage error, an unknown verb and a
panic all exit 1 with a message on stderr.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 4: Move the shell guard under `hook pre-tool-use`

**Files:**
- Create: `src/bin/devkit/hook/shell.rs` (moved from `src/bin/devkit/harness/mod.rs`)
- Modify: `src/bin/devkit/harness/mod.rs` (becomes a hidden alias that calls into `hook::shell`)
- Modify: `tests/harness_guard.rs`, `tests/harness_shell_writes.rs` (run each case against both spellings)

**Interfaces:**
- Consumes: `HookEvent::PreToolUse` from Task 3, `ShellPayload` from Task 2.
- Produces: `shell::guard(&Value, Option<Harness>) -> Result<()>`, carrying the whole of today's `guard_shell` including its `catch_unwind` and `WRITE_STAGE_DEADLINE`.
- Produces: `shell::deny_unreadable_payload() -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Add to `tests/harness_guard.rs`, and change `run_hook_with` to take the argv so both spellings run:

```rust
/// Every case the old spelling covers must answer identically under the new
/// one, because the manifests move to it in the same release.
#[test]
fn both_spellings_answer_identically() {
    let p = project("[harness]\nenforce_commands = true\n");
    let home = tempfile::tempdir().unwrap();
    let payload = claude_payload("node server.js");
    let old = run_argv(p.path(), home.path(), &["harness", "shell"], &payload);
    let new = run_argv(p.path(), home.path(), &["hook", "pre-tool-use"], &payload);
    assert_eq!(old.status.code(), new.status.code());
    assert_eq!(
        String::from_utf8_lossy(&old.stdout),
        String::from_utf8_lossy(&new.stdout)
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `devrun task test -C <worktree>`
Expected: FAIL, the new spelling produces empty stdout because `pre_tool_use` does not yet reach the guard.

- [ ] **Step 3: Move the module**

Move the body of `src/bin/devkit/harness/mod.rs` to `src/bin/devkit/hook/shell.rs`, along with `dialect` and `writes`. Two changes as it moves:

```rust
/// The harness the manifest named, else the payload's own shape.
fn resolve_harness(declared: Option<Harness>, shell: &ShellPayload) -> Harness {
    declared.unwrap_or(shell.harness)
}
```

and `respond` takes the payload as an argument instead of reading stdin, so the verb dispatch in `mod.rs` owns the read.

Leave `src/bin/devkit/harness/mod.rs` as a hidden alias:

```rust
/// Retired in favour of `devkit hook pre-tool-use`. Kept because an installed
/// plugin manifest can outlive the binary it was installed beside.
#[derive(Subcommand)]
enum Cmd {
    #[command(hide = true)]
    Shell,
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `devrun task test -C <worktree>`
Expected: PASS. `tests/harness_shell_writes.rs` and `tests/abort_hook.rs` stay green unchanged.

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='src/bin/devkit/hook/shell.rs,src/bin/devkit/harness/mod.rs,tests/harness_guard.rs' \
  --arg commit_subject='refactor(hook): move the shell guard under pre-tool-use' \
  --arg commit_body='The payload tool_name now picks the shell or the edit path,
which is where that decision belongs: the two matcher blocks in
each manifest existed only because two subsystems answered one
event. devkit harness shell stays as a hidden alias.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 5: Move the edit path and the release verbs

**Files:**
- Create: `src/bin/devkit/hook/edit.rs` (moved from `src/bin/devkit/locks.rs:191-272`)
- Modify: `src/bin/devkit/locks.rs` (`Cmd::Hook` becomes a hidden alias calling into `hook::edit`)
- Modify: `tests/locks.rs`, `tests/lock_harness_race.rs`

**Interfaces:**
- Consumes: `LockAction`, `parse_write`, `parse_subagent_stop`, `parse_session_end` from Task 1.
- Consumes: `is_write_tool` from Task 1, which the Task 3 dispatch already calls.
- Produces: `edit::guard(&Value) -> Result<()>`, `edit::release_subagent() -> Result<()>`, `edit::release_session() -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Add to `tests/locks.rs`:

```rust
#[test]
fn the_new_edit_spelling_claims_the_same_lock() {
    let p = project_with_enforcement();
    let home = tempfile::tempdir().unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse", "tool_name": "Write",
        "session_id": "s1", "cwd": p.path(),
        "tool_input": {"file_path": "src/a.rs"}
    })
    .to_string();
    let out = run_argv(p.path(), home.path(), &["hook", "pre-tool-use"], &payload);
    assert_eq!(out.status.code(), Some(0));
    assert!(lock_held(p.path(), "src/a.rs", "s1"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `devrun task test -C <worktree>`
Expected: FAIL, no lock held.

- [ ] **Step 3: Implement**

Move `run_hook`, `deny_unparsable` and `resolve_against` from `locks.rs` into `hook/edit.rs`, split by verb so each function reads its own payload:

```rust
/// A write the harness governs, claimed before the tool runs. Fails closed:
/// an unusable payload denies rather than allows.
pub fn guard(payload: &serde_json::Value) -> anyhow::Result<()> {
    let cwd = cwd_of(payload);
    match devkit_locks::hook::parse_write(payload) {
        None => Ok(()),
        Some(LockAction::Unusable { reason }) => {
            if devkit_locks::hook::enforcement_enabled(&cwd) {
                println!(
                    "{}",
                    devkit_locks::hook::deny_json(&format!(
                        "devkit write-harness: {reason} (fail-closed)"
                    ))
                );
            }
            Ok(())
        }
        Some(LockAction::Write { file_paths, holder, .. }) => claim(payload, &cwd, &file_paths, &holder),
        Some(_) => Ok(()),
    }
}

pub fn release_subagent() -> anyhow::Result<()> {
    let payload = read_payload_or_empty();
    if let Some(LockAction::ReleaseSubagent { holder }) =
        devkit_locks::hook::parse_subagent_stop(&payload)
    {
        let _ = devkit_locks::release_prefix(&holder);
    }
    Ok(())
}
```

`release_session` is the same shape over `parse_session_end`. `claim` is today's `run_hook` body from `WriteResolver::new()` through the conflict print, moved verbatim.

- [ ] **Step 4: Run the test to verify it passes**

Run: `devrun task test -C <worktree>`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='src/bin/devkit/hook/edit.rs,src/bin/devkit/hook/mod.rs,src/bin/devkit/locks.rs,crates/devkit-locks/src/hook.rs,tests/locks.rs' \
  --arg commit_subject='refactor(hook): move the edit and release paths' \
  --arg commit_body='lockm hook stays as a hidden alias. The claim and release
logic is unchanged; only its entry point moves.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 6: Rewrite the three manifests and probe for a stale binary

**Files:**
- Modify: `hooks/hooks.json`, `hooks/hooks-codex.json`, `hooks/hooks-cursor.json`
- Modify: `hooks/run-hook.cmd` (the `bootstrap-binaries` path)
- Test: `tests/hooks/` (the existing manifest validity suite)

**Interfaces:**
- Consumes: every verb from Task 3.
- Produces: manifests whose every `command` is `devkit hook <verb> --harness <name>`.

- [ ] **Step 1: Write the failing test**

Create `tests/hook_manifests.rs`:

```rust
//! Every shipped manifest names verbs this binary understands, and names the
//! harness that reads it.

use std::path::Path;

fn commands(path: &str) -> Vec<String> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut out = Vec::new();
    collect(&v, &mut out);
    out
}

fn collect(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            if let Some(serde_json::Value::String(c)) = m.get("command") {
                out.push(c.clone());
            }
            m.values().for_each(|x| collect(x, out));
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        _ => {}
    }
}

#[test]
fn no_manifest_names_a_retired_command() {
    for f in ["hooks/hooks.json", "hooks/hooks-codex.json", "hooks/hooks-cursor.json"] {
        for c in commands(f) {
            assert!(!c.contains("lockm hook"), "{f}: {c}");
            assert!(!c.contains("harness shell"), "{f}: {c}");
        }
    }
}

#[test]
fn every_hook_command_names_its_harness() {
    for (f, name) in [
        ("hooks/hooks.json", "claude-code"),
        ("hooks/hooks-codex.json", "codex"),
        ("hooks/hooks-cursor.json", "cursor"),
    ] {
        for c in commands(f).iter().filter(|c| c.contains("devkit hook ")) {
            assert!(c.contains(&format!("--harness {name}")), "{f}: {c}");
        }
    }
}

#[test]
fn every_verb_a_manifest_names_parses() {
    let exe = Path::new(env!("CARGO_BIN_EXE_devkit"));
    for f in ["hooks/hooks.json", "hooks/hooks-codex.json", "hooks/hooks-cursor.json"] {
        for c in commands(f).iter().filter(|c| c.contains("devkit hook ")) {
            let verb = c.split_whitespace().nth(2).unwrap();
            let out = std::process::Command::new(exe)
                .args(["hook", verb, "--help"])
                .output()
                .unwrap();
            assert!(out.status.success(), "{f} names an unknown verb: {verb}");
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `devrun task test -C <worktree>`
Expected: FAIL on `no_manifest_names_a_retired_command`.

- [ ] **Step 3: Rewrite `hooks/hooks.json`**

The `PreToolUse` block merges to one entry whose matcher is the union of the two it replaces. Every command gains `--harness claude-code`. `SessionEnd` gains an explicit timeout, because Claude Code gives all `SessionEnd` hooks a shared 1.5 second budget and this one releases, records and prunes.

```json
    "PreToolUse": [
      {
        "matcher": "Edit|MultiEdit|Write|NotebookEdit|Bash|PowerShell",
        "hooks": [
          {
            "type": "command",
            "command": "devkit hook pre-tool-use --harness claude-code",
            "timeout": 30
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Edit|MultiEdit|Write|NotebookEdit|Bash|PowerShell",
        "hooks": [
          { "type": "command", "command": "devkit hook post-tool-use --harness claude-code" }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "devkit hook session-end --harness claude-code",
            "timeout": 15
          }
        ]
      }
    ],
```

Add the remaining Claude Code rows from the spec's mapping table: `PostToolUseFailure`, `SessionStart`, `SubagentStart`, `SubagentStop`, `PermissionRequest`, `PermissionDenied`, `Stop`, `StopFailure`, `PreCompact`, `PostCompact`, `CwdChanged`, `WorktreeCreate`, `WorktreeRemove`, `UserPromptSubmit`. The `devkit brief` entries on `SessionStart`, `PostCompact` and `CwdChanged` stay alongside the new ones.

- [ ] **Step 4: Rewrite `hooks/hooks-codex.json` and `hooks/hooks-cursor.json`**

Codex takes every verb in the mapping table's Codex column, with `--harness codex`, keeping `commandWindows` where it is set today. Codex has no `PostToolUseFailure`, `PermissionDenied`, `StopFailure`, `CwdChanged` or worktree events, so those rows are absent.

Cursor moves off `beforeShellExecution` onto the generic trio, with `--harness cursor`, and gains `sessionEnd`:

```json
{
  "version": 1,
  "hooks": {
    "sessionStart": [
      { "type": "command", "command": "./hooks/run-hook.cmd bootstrap-binaries" },
      { "type": "command", "command": "devkit brief --additional-context" },
      { "type": "command", "command": "devkit hook session-start --harness cursor" }
    ],
    "preToolUse": [
      { "type": "command", "command": "devkit hook pre-tool-use --harness cursor", "timeout": 30 }
    ],
    "postToolUse": [
      { "type": "command", "command": "devkit hook post-tool-use --harness cursor" }
    ],
    "postToolUseFailure": [
      { "type": "command", "command": "devkit hook post-tool-use-failure --harness cursor" }
    ],
    "sessionEnd": [
      { "type": "command", "command": "devkit hook session-end --harness cursor", "timeout": 15 }
    ],
    "subagentStart": [
      { "type": "command", "command": "devkit hook subagent-start --harness cursor" }
    ],
    "subagentStop": [
      { "type": "command", "command": "devkit hook subagent-stop --harness cursor" }
    ],
    "stop": [
      { "type": "command", "command": "devkit hook stop --harness cursor" }
    ],
    "preCompact": [
      { "type": "command", "command": "devkit hook pre-compact --harness cursor" }
    ],
    "workspaceOpen": [
      { "type": "command", "command": "devkit hook cwd-changed --harness cursor" }
    ],
    "beforeSubmitPrompt": [
      { "type": "command", "command": "devkit hook user-prompt-submit --harness cursor" }
    ]
  }
}
```

Before this ships, capture one real payload from each of `preToolUse`, `beforeSubmitPrompt` and `subagentStart` in a Cursor session and confirm that empty stdout is an allow and that a `"version": 1` manifest accepts these event names. The spec lists these three as unverified. If `"version": 1` rejects any, drop that row rather than raising the version, which changes how every other row is read.

- [ ] **Step 5: Add the stale-binary probe**

In `hooks/run-hook.cmd`'s `bootstrap-binaries` path, after the existing externally-managed check, run `devkit hook --help` once per version stamp. A binary that fails it is too old for the installed manifests, and the message says so and names `cargo install --path .` as the fix, in the same place a failed install is already reported.

- [ ] **Step 6: Run the gate**

Run: `devrun task test -C <worktree>`
Expected: PASS on all three manifest tests.

- [ ] **Step 7: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='hooks/hooks.json,hooks/hooks-codex.json,hooks/hooks-cursor.json,hooks/run-hook.cmd,tests/hook_manifests.rs' \
  --arg commit_subject='feat(hooks): wire every harness event to devkit hook' \
  --arg commit_body='Each manifest names the harness that reads it, so identity
never depends on guessing which fields a vendor sends. The two
PreToolUse blocks merge into one with the union matcher, since
the payload tool_name now picks the path.

Cursor moves off beforeShellExecution onto the generic tool trio,
which carries tool_use_id, and gains sessionEnd.

The session-start bootstrap probes for the verb family, because a
binary it did not install is never upgraded and would otherwise
meet a manifest naming subcommands it lacks.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 7: The `[harness.log]` config table

**Files:**
- Modify: `crates/devkit-config/src/harness.rs:197-260`
- Modify: `schema/devkit-config.json` (regenerated, not hand-edited)
- Test: `crates/devkit-config/src/harness.rs` (`mod tests`), plus the doctest on `LogSection`

**Interfaces:**
- Produces: `LogSection { enabled: Option<bool>, command: Option<Fidelity>, prompt: Option<PromptFidelity>, auto_prune: Option<bool>, dir: Option<PathBuf>, max_age_days: Option<u32>, max_bytes: Option<u64> }`, every leaf `Option` so a layer that did not set a key is distinguishable from one that set the default.
- Produces: `Fidelity { Hashed, Redacted, Full }` and `PromptFidelity { Off, Hashed, Redacted, Full }`, both `Ord`, lowest first, so the clamp is `min`.
- Produces: `HarnessSection::log: LogSection`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn fidelity_orders_from_least_to_most_revealing() {
    assert!(Fidelity::Hashed < Fidelity::Redacted);
    assert!(Fidelity::Redacted < Fidelity::Full);
    assert!(PromptFidelity::Off < PromptFidelity::Hashed);
}

#[test]
fn a_zero_cap_is_rejected_rather_than_guessed() {
    let err = Config::parse("[harness.log]\nmax_age_days = 0\n").unwrap_err();
    assert!(format!("{err:#}").contains("max_age_days"), "{err:#}");
}

#[test]
fn an_unset_key_stays_none() {
    let c = Config::parse("[harness.log]\nenabled = true\n").unwrap();
    assert_eq!(c.harness.log.enabled, Some(true));
    assert_eq!(c.harness.log.command, None, "an unset key must not read as its default");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`
Expected: FAIL, `no field log on type HarnessSection`.

- [ ] **Step 3: Implement**

```rust
/// What a logged command carries. Ordered least to most revealing, so
/// resolution across layers is `min` and no layer can raise it.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Fidelity {
    Hashed,
    Redacted,
    Full,
}

/// Harness logging. Off unless the global config turns it on.
///
/// ```toml
/// [harness.log]
/// enabled = true              # global config only
/// command = "redacted"        # full | redacted | hashed
/// prompt = "off"              # off | hashed | redacted | full
/// auto_prune = true           # global config only
/// dir = "${HOME}/logs/devkit" # global config only
/// max_age_days = 30           # global config only; absent means unlimited
/// max_bytes = 2_000_000_000   # global config only; absent means unlimited
/// ```
#[derive(Deserialize, Debug, Clone, Default, PartialEq, schemars::JsonSchema)]
#[serde(default)]
pub struct LogSection {
    pub enabled: Option<bool>,
    pub command: Option<Fidelity>,
    pub prompt: Option<PromptFidelity>,
    pub auto_prune: Option<bool>,
    pub dir: Option<PathBuf>,
    #[serde(deserialize_with = "nonzero_u32")]
    pub max_age_days: Option<u32>,
    #[serde(deserialize_with = "nonzero_u64")]
    pub max_bytes: Option<u64>,
}
```

Reject `0` in both caps with a `#[serde(deserialize_with = ...)]` that errors by name. The two readings of a zero retention cap differ by the whole corpus.

- [ ] **Step 4: Regenerate the schema**

Run: `DEVKIT_UPDATE_SCHEMA=1 devrun task test -C <worktree>`
Then re-run `devrun task test -C <worktree>` and confirm the schema drift test passes.

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-config/src/harness.rs,schema/devkit-config.json' \
  --arg commit_subject='feat(config): add the harness.log table' \
  --arg commit_body='Every leaf is Option so a layer that set nothing is
distinguishable from one that set the default, which is what the
downward clamp needs. Both fidelities are Ord from least to most
revealing, so resolving across layers is min. A zero retention cap
is rejected: unlimited and delete-everything are both plausible
readings of it.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 8: Resolve the settings, global-only and clamped

**Files:**
- Create: `crates/devkit-common/src/harness_log/mod.rs`
- Modify: `crates/devkit-common/src/lib.rs` (add `pub mod harness_log;`)
- Test: `crates/devkit-common/src/harness_log/mod.rs` (`mod tests`), `tests/harness_log_config.rs`

**Interfaces:**
- Produces: `Settings { enabled, command, prompt, auto_prune, dir, max_age_days, max_bytes }`, every field resolved.
- Produces: `resolve(cwd: &Path) -> Settings`, and the pure `resolve_from(global: Option<&LogSection>, layers: &[LogSection], env: Option<bool>) -> Settings` it wraps, so precedence is testable with no filesystem.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_project_layer_cannot_enable_logging() {
    let project = LogSection { enabled: Some(true), ..Default::default() };
    let s = resolve_from(None, &[project], None);
    assert!(!s.enabled, "only the global config turns logging on");
}

#[test]
fn any_layer_can_disable_logging() {
    let global = LogSection { enabled: Some(true), ..Default::default() };
    let project = LogSection { enabled: Some(false), ..Default::default() };
    assert!(!resolve_from(Some(&global), &[project], None).enabled);
}

#[test]
fn a_project_layer_cannot_move_the_directory_or_the_caps() {
    let global = LogSection {
        enabled: Some(true),
        dir: Some("/global/logs".into()),
        max_bytes: Some(10),
        ..Default::default()
    };
    let project = LogSection {
        dir: Some("./.logs".into()),
        max_bytes: Some(1_000_000),
        ..Default::default()
    };
    let s = resolve_from(Some(&global), &[project], None);
    assert_eq!(s.dir, std::path::PathBuf::from("/global/logs"));
    assert_eq!(s.max_bytes, Some(10));
}

#[test]
fn fidelity_clamps_downward_in_any_order() {
    let global = LogSection {
        enabled: Some(true),
        command: Some(Fidelity::Full),
        ..Default::default()
    };
    let lower = LogSection { command: Some(Fidelity::Hashed), ..Default::default() };
    let mid = LogSection { command: Some(Fidelity::Redacted), ..Default::default() };
    let a = resolve_from(Some(&global), &[lower.clone(), mid.clone()], None).command;
    let b = resolve_from(Some(&global), &[mid, lower], None).command;
    assert_eq!(a, Fidelity::Hashed);
    assert_eq!(a, b, "min is order-independent, so layer precedence cannot matter here");
}

#[test]
fn the_env_override_beats_a_project_disable() {
    let global = LogSection { enabled: Some(true), ..Default::default() };
    let project = LogSection { enabled: Some(false), ..Default::default() };
    assert!(resolve_from(Some(&global), &[project], Some(true)).enabled);
}
```

And in `tests/harness_log_config.rs`, the case the obvious implementation passes and the correct one needs:

```rust
//! On a machine with no global config, `resolve_rules` never pushes a global
//! layer, so index 0 is a project layer. A resolver that read global-only keys
//! by index would read a project's.

#[test]
fn a_machine_with_no_global_config_cannot_be_enabled_by_a_project() {
    let home = tempfile::tempdir().unwrap();
    let p = tempfile::tempdir().unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        "[harness.log]\nenabled = true\ncommand = \"full\"\n",
    )
    .unwrap();
    assert!(!home.path().join(".config/devkit/config.toml").exists());
    // Driven through the binary so the real layer discovery runs.
    let out = run_devkit(p.path(), home.path(), &["doctor", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["rows"]["harness_log"]["enabled"], serde_json::json!(false));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`
Expected: FAIL, `harness_log` does not exist.

- [ ] **Step 3: Implement**

```rust
/// The global table is carried separately rather than read by index.
/// `resolve_rules` pushes a global layer only when `global_config_path()`
/// resolves and the file parses, so on a machine with no global config index 0
/// is a project layer, and an index-based read would let a project enable
/// logging and move its directory.
pub fn resolve_from(
    global: Option<&LogSection>,
    layers: &[LogSection],
    env: Option<bool>,
) -> Settings {
    let enabled_globally = global.and_then(|g| g.enabled).unwrap_or(false);
    let disabled_anywhere = layers.iter().any(|l| l.enabled == Some(false));
    let enabled = env.unwrap_or(enabled_globally && !disabled_anywhere);

    let command = layers.iter().filter_map(|l| l.command).fold(
        global.and_then(|g| g.command).unwrap_or(Fidelity::Redacted),
        std::cmp::min,
    );
    let prompt = layers.iter().filter_map(|l| l.prompt).fold(
        global.and_then(|g| g.prompt).unwrap_or(PromptFidelity::Off),
        std::cmp::min,
    );
    Settings {
        enabled,
        command,
        prompt,
        auto_prune: global.and_then(|g| g.auto_prune).unwrap_or(true),
        dir: global
            .and_then(|g| g.dir.clone())
            .unwrap_or_else(|| paths::state_dir().join("harness-log")),
        max_age_days: global.and_then(|g| g.max_age_days),
        max_bytes: global.and_then(|g| g.max_bytes),
    }
}
```

`dir`, `auto_prune` and both caps read from `global` alone and ignore `layers` entirely.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-common/src/harness_log/mod.rs,crates/devkit-common/src/lib.rs,tests/harness_log_config.rs' \
  --arg commit_subject='feat(harness-log): resolve settings global-first' \
  --arg commit_body='Enabling keys, the directory and both retention caps are read
from the global config alone. A project layer may only tighten:
turn logging off, or lower either fidelity. A project-writable
directory would land command text inside the checkout, which is
the one outcome the boundary exists to prevent.

The global table is carried rather than indexed, because
resolve_rules pushes a global layer only when one exists, so on a
fresh machine index 0 is a project layer.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 9: Redaction and hashing

**Files:**
- Create: `crates/devkit-common/src/harness_log/redact.rs`
- Test: same file (`mod tests`)

**Interfaces:**
- Produces: `apply(command: &str, mode: Fidelity) -> (String, bool)`, the text and whether anything was substituted.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_command_with_no_secret_comes_through_byte_identical() {
    let input = "cargo nextest run --workspace --no-fail-fast";
    let (out, hit) = apply(input, Fidelity::Redacted);
    assert_eq!(out, input, "redaction must not disturb ordinary commands");
    assert!(!hit);
}

#[test]
fn a_known_env_assignment_is_substituted_by_kind() {
    let (out, hit) = apply("GH_TOKEN=ghp_abcdefghijklmnop gh pr list", Fidelity::Redacted);
    assert!(hit);
    assert!(!out.contains("ghp_abcdefghijklmnop"));
    assert!(out.contains("GH_TOKEN="), "the command's structure must survive");
    assert!(out.contains("gh pr list"));
}

#[test]
fn hashed_keeps_nothing_but_a_stable_digest() {
    let (a, _) = apply("echo hi", Fidelity::Hashed);
    let (b, _) = apply("echo hi", Fidelity::Hashed);
    let (c, _) = apply("echo ho", Fidelity::Hashed);
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert!(!a.contains("echo"));
}

#[test]
fn full_is_a_passthrough() {
    let input = "GH_TOKEN=ghp_secret gh pr list";
    assert_eq!(apply(input, Fidelity::Full).0, input);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`

- [ ] **Step 3: Implement**

Match `KEY=value` where `KEY` is one of `LINEAR_API_KEY`, `LINEAR_WORKSPACE`, `SLACK_TOKEN` (the three `secrets` resolves) plus `GH_TOKEN` and `GITHUB_TOKEN`, and the token shapes `ghp_`, `gho_`, `ghs_`, `github_pat_`, `xoxb-`, `xoxp-`, `lin_api_` and `sk-`. Substitute a placeholder naming the kind. Document in the module header that this is best-effort and that `redacted` is not safe to hand to a third party.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-common/src/harness_log/redact.rs' \
  --arg commit_subject='feat(harness-log): redact known token shapes' \
  --arg commit_body='Best effort over the variable names devkit resolves
credentials from and the token prefixes of the services it talks
to. The substitute names the kind so the command structure
survives for the analyzer corpus. Novel formats are missed, and
the module header says so.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 10: The writer, the path layout, and the infallible entry point

**Files:**
- Create: `crates/devkit-common/src/harness_log/writer.rs`
- Modify: `crates/devkit-common/src/harness_log/mod.rs` (add `record`)
- Test: `crates/devkit-common/src/harness_log/writer.rs` (`mod tests`)

**Interfaces:**
- Produces: `record(&Record) -> ()`, infallible, internally `catch_unwind` plus a deadline.
- Produces: `sanitize_component(&str) -> Option<String>` and `file_for(&Settings, session: Option<&str>, agent: Option<&str>, now: SystemTime) -> PathBuf`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_day_directory_is_utc() {
    let s = settings_at(dir.path());
    // 2026-01-01T00:30:00Z is still 2025-12-31 in UTC-8.
    let p = file_for(&s, Some("s1"), None, unix(1767227400));
    assert!(p.to_string_lossy().contains("2026-01-01"), "{p:?}");
}

#[test]
fn a_subagent_gets_its_own_file() {
    let s = settings_at(dir.path());
    let solo = file_for(&s, Some("s1"), None, now());
    let sub = file_for(&s, Some("s1"), Some("a1"), now());
    assert_ne!(solo, sub, "parallel subagents share a session id");
    assert!(sub.file_name().unwrap().to_string_lossy().starts_with("s1-a1"));
}

#[test]
fn a_path_separator_in_an_id_cannot_escape_the_directory() {
    assert_eq!(sanitize_component("../../etc"), Some("______etc".into()));
    assert_eq!(sanitize_component(""), None);
}

#[test]
fn a_payload_with_no_session_id_still_lands() {
    let s = settings_at(dir.path());
    let p = file_for(&s, None, None, now());
    assert!(p.file_name().unwrap().to_string_lossy().starts_with("unknown-"));
}

#[test]
fn a_panic_inside_record_never_escapes() {
    // The fault knob is debug-only, and these tests run under it.
    unsafe { std::env::set_var("DEVKIT_HARNESS_LOG_FAULT", "panic") };
    record(&some_record());
    unsafe { std::env::remove_var("DEVKIT_HARNESS_LOG_FAULT") };
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`

- [ ] **Step 3: Implement**

```rust
/// Never fails, because the caller cannot afford it to. `guard_shell` turns a
/// panic after its write stage is live into a denial, so a logging panic here
/// would deny a command the guard had already allowed.
pub fn record(rec: &Record) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let settings = resolve(&rec.cwd);
        if !settings.enabled {
            return;
        }
        let _ = with_deadline(RECORD_DEADLINE, || write_one(&settings, rec));
    }));
}

/// Well under a second: nothing downstream waits on a record, and a hung
/// network home must not hold the hook.
const RECORD_DEADLINE: Duration = Duration::from_millis(250);
```

`write_one` builds the path, `create_dir_all`s the day directory, opens with `append(true)`, and issues one `write_all` of the serialized line plus a newline. No lock. `sanitize_component` maps every character outside `[A-Za-z0-9._-]` to `_` and returns `None` for an empty result.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-common/src/harness_log/writer.rs,crates/devkit-common/src/harness_log/mod.rs' \
  --arg commit_subject='feat(harness-log): add the record writer' \
  --arg commit_body='One infallible entry point: record returns unit, catches its
own panics, and runs the write under a short deadline.
catch_unwind makes it panic-safe but not block-safe, and the log
directory can name a network home.

Day directories are UTC, since prune reads the name for both its
retention arithmetic and its never-delete-today guard. Ids are
sanitised before they reach a path, and a payload with no session
id still lands.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 11: Record types and the analysis projection

**Files:**
- Modify: `crates/devkit-common/src/harness_log/mod.rs` (the `Record` types)
- Create: `src/bin/devkit/hook/record.rs` (the `Analysis` mapping)
- Modify: `crates/devkit-command/src/lib.rs` (add `ANALYZER_VERSION`)
- Test: `src/bin/devkit/hook/record.rs` (`mod tests`)

**Interfaces:**
- Produces: `devkit_command::ANALYZER_VERSION: u32`, hand-bumped.
- Produces: `Record { schema_version, recorded_at, devkit_version, analyzer_version, harness, event, vendor_event, session_id, agent_id, tool_use_id, cwd, project_root, kind }`, with `kind: Kind`, a tagged enum of the eight kinds.
- Produces: `record::projection(&Analysis) -> AnalysisProjection`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_projection_carries_every_uncertainty_and_the_counts() {
    let a = devkit_command::analyze("cat $UNKNOWN > out.txt", &ctx());
    let p = projection(&a);
    assert_eq!(p.uncertainties.len(), a.uncertainties.len());
    assert_eq!(p.counts.uncertainties, a.uncertainties.len());
    assert_eq!(p.counts.invocations, a.invocations.len());
    assert!(p.resolved_writes.iter().any(|w| w.ends_with("out.txt")));
}

#[test]
fn the_analyzer_version_is_not_the_crate_version() {
    assert_ne!(
        devkit_command::ANALYZER_VERSION.to_string(),
        env!("CARGO_PKG_VERSION"),
        "a release-please stamped version would call the corpus stale each release"
    );
}

#[test]
fn every_record_names_both_its_verb_and_its_vendor_event() {
    let r = shell_pre_record_from(&codex_stop_payload(), HookEvent::Stop);
    assert_eq!(r.event, "stop");
    assert_eq!(r.vendor_event.as_deref(), Some("Interrupt"), "the mapping is not injective");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`

- [ ] **Step 3: Implement**

```rust
/// Bumped by hand when analysis semantics change, which is the only thing a
/// regression diff cares about. The crate version cannot serve: release-please
/// moves it every release whether the parser changed or not.
pub const ANALYZER_VERSION: u32 = 1;
```

`AnalysisProjection` and its leaf types are plain serde structs in `harness_log`. `Analysis` gains no `Serialize`, and `devkit-common` gains no dependency on `devkit-command`; the mapping lives in `hook/record.rs`, which already depends on both.

The four counts are invocations, resolved write targets, unresolved write targets, and uncertainties, so a cohort query can rank without reading every projection.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-common/src/harness_log/mod.rs,src/bin/devkit/hook/record.rs,crates/devkit-command/src/lib.rs' \
  --arg commit_subject='feat(harness-log): add record types and the projection' \
  --arg commit_body='The projection types are plain serde structs in devkit-common
and the Analysis mapping lives in the binary. Depending on
devkit-command from devkit-common would compile six tree-sitter C
grammars into every library crate and put the IO-free analyzer
under the IO crate.

ANALYZER_VERSION is hand-bumped. The crate version is stamped by
release-please, so a diff keyed on it would call the whole corpus
stale at every release.

Every record names both the devkit verb and the vendor event,
because the mapping is not injective: Codex sends Stop and
Interrupt to one verb.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 12: Wire the records into `pre-tool-use`

**Files:**
- Modify: `src/bin/devkit/hook/shell.rs`, `src/bin/devkit/hook/edit.rs`
- Test: `tests/harness_log_e2e.rs`

**Interfaces:**
- Consumes: `record`, `Record`, `projection` from Tasks 10 and 11.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_guarded_command_lands_a_record_carrying_its_verdict() {
    let (p, home) = enabled_project();
    run_argv(p.path(), home.path(), &["hook", "pre-tool-use"], &claude_payload("node server.js"));
    let rec = sole_record(home.path());
    assert_eq!(rec["kind"], "shell_pre");
    assert_eq!(rec["verdict"]["decision"], "deny");
    assert!(!rec["verdict"]["blocks"].as_array().unwrap().is_empty());
}

#[test]
fn a_blocked_write_never_delays_or_changes_the_envelope() {
    let (p, home) = enabled_project();
    let out = run_argv_env(
        p.path(), home.path(), &["hook", "pre-tool-use"],
        &claude_payload("node server.js"),
        &[("DEVKIT_HARNESS_LOG_FAULT", "block")],
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[test]
fn a_panic_in_the_logging_path_leaves_the_verdict_unchanged() {
    let (p, home) = enabled_project();
    let clean = run_argv(p.path(), home.path(), &["hook", "pre-tool-use"], &claude_payload("ls"));
    let faulted = run_argv_env(
        p.path(), home.path(), &["hook", "pre-tool-use"], &claude_payload("ls"),
        &[("DEVKIT_HARNESS_LOG_FAULT", "panic")],
    );
    assert_eq!(clean.stdout, faulted.stdout, "logging must never reach the verdict");
    assert_eq!(faulted.status.code(), Some(0));
}

#[test]
fn a_payload_that_does_not_parse_is_still_recorded() {
    let (p, home) = enabled_project();
    run_argv(p.path(), home.path(), &["hook", "pre-tool-use"], "{not json");
    assert_eq!(sole_record(home.path())["kind"], "shell_pre");
}

#[test]
fn a_payload_with_no_cwd_is_recorded_rather_than_dropped() {
    let (p, home) = enabled_project();
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse", "tool_name": "Bash",
        "tool_input": {"command": "ls"}
    })
    .to_string();
    run_argv(p.path(), home.path(), &["hook", "pre-tool-use"], &payload);
    assert_eq!(sole_record(home.path())["kind"], "shell_pre");
}

#[test]
fn logging_on_with_enforcement_off_still_analyses() {
    // The early return that skips the parse is skipped in turn: a record with
    // an empty verdict is half a record. The cost is bounded by logging being
    // off by default and enablable only from the global config.
    let (p, home) = enabled_project_without_enforcement();
    run_argv(p.path(), home.path(), &["hook", "pre-tool-use"], &claude_payload("cat a > b.txt"));
    let rec = sole_record(home.path());
    assert!(
        rec["analysis"]["resolved_writes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().ends_with("b.txt"))
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`

- [ ] **Step 3: Implement**

The ordering is the contract. In `shell::guard`, after the verdict is decided:

```rust
// Print and flush first. record catches its own panics but cannot be made
// block-safe, and a stall before the envelope exists runs into the
// manifest's 30-second timeout, which allows the call. A closed stdout pipe
// fails the write immediately rather than blocking, so the record still
// lands afterwards.
print_envelope(&envelope);
let _ = std::io::stdout().flush();
harness_log::record(&rec);
```

The write-stage deadline branch keeps the same order before `std::process::exit(0)`: a missed deadline is one of the operational signals this exists to collect, and it is the one path that would otherwise never reach a log. The panic arm in `guard_shell` writes a minimal record; a second `OnceLock` alongside `write_stage` carries the harness and the ids out of the `catch_unwind` closure so the panic record can name them rather than the stage alone.

`DEVKIT_HARNESS_LOG_FAULT` is read only under `#[cfg(debug_assertions)]`, so the knob does not exist in a release binary. Values: `panic` and `block`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='src/bin/devkit/hook/shell.rs,src/bin/devkit/hook/edit.rs,tests/harness_log_e2e.rs' \
  --arg commit_subject='feat(harness-log): record the pre-tool-use verdict' \
  --arg commit_body='The envelope is printed and flushed before any record is
written, and the record runs under its own deadline. The panic arm
and the write-stage deadline branch both log, since a panic on
real traffic and a missed deadline are the two highest-value
records and today each leaves one stderr line that scrolls away.

Two tests are the failure contract: a panic in the logging path
leaves the verdict byte-identical, and a blocked write neither
delays nor alters the envelope.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 13: The remaining record kinds

**Files:**
- Modify: `src/bin/devkit/hook/mod.rs` (dispatch every record-only verb), `src/bin/devkit/hook/record.rs`
- Test: `tests/harness_log_kinds.rs`

**Interfaces:**
- Consumes: `Kind` from Task 11.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn each_verb_writes_the_kind_its_table_row_names() {
    let (p, home) = enabled_project();
    for (verb, kind) in [
        ("post-tool-use", "shell_post"),
        ("post-tool-use-failure", "shell_post"),
        ("session-start", "session"),
        ("subagent-start", "session"),
        ("permission-request", "permission"),
        ("permission-denied", "permission"),
        ("stop", "lifecycle"),
        ("pre-compact", "lifecycle"),
        ("cwd-changed", "lifecycle"),
        ("worktree-create", "worktree"),
        ("user-prompt-submit", "prompt"),
    ] {
        clear_records(home.path());
        let out = run_argv(p.path(), home.path(), &["hook", verb], &payload_for(verb));
        assert!(out.stdout.is_empty(), "{verb} must be silent on stdout");
        assert_eq!(out.status.code(), Some(0));
        assert_eq!(sole_record(home.path())["kind"], kind, "{verb}");
    }
}

#[test]
fn a_prompt_is_not_recorded_at_the_default_fidelity() {
    let (p, home) = enabled_project(); // prompt defaults to "off"
    run_argv(p.path(), home.path(), &["hook", "user-prompt-submit"], &prompt_payload("my secret plan"));
    let rec = sole_record(home.path());
    assert_eq!(rec["kind"], "prompt");
    assert!(rec["text"].is_null(), "off records that a prompt happened, not its text");
}

#[test]
fn a_shell_post_carries_absent_rather_than_zero_for_what_codex_omits() {
    let (p, home) = enabled_project();
    run_argv(p.path(), home.path(), &["hook", "post-tool-use"], &codex_post_payload());
    let rec = sole_record(home.path());
    assert!(rec["exit_code"].is_null(), "Codex sends no exit code");
    assert!(rec["stdout_bytes"].is_number());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`

- [ ] **Step 3: Implement**

Each record-only arm reads stdin, builds its `Record`, calls `record`, and returns `Ok(())`. Nothing reaches stdout. The dispatch reads the global config and nothing else: no project layer load, no tree-sitter, so a disabled verb costs one config read on top of the spawn.

`shell_post` carries `exit_code`, `duration_ms`, the error and interrupt flags, and the byte lengths of stdout and stderr, each `None` when the harness does not supply it. Not the output itself: command output is large and is where credentials actually surface, and redaction over arbitrary program output would be theatre.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`

- [ ] **Step 5: Measure the spawn cost**

Run `hyperfine 'devkit hook stop' --warmup 3` with logging off and put the number in the PR body. A verb whose cost does not justify its record comes out of the shipped manifests and stays available to a hand-wired one.

- [ ] **Step 6: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='src/bin/devkit/hook/mod.rs,src/bin/devkit/hook/record.rs,tests/harness_log_kinds.rs' \
  --arg commit_subject='feat(harness-log): record every remaining verb' \
  --arg commit_body='Every verb but pre-tool-use writes nothing to stdout. That is a
requirement rather than an observation: UserPromptSubmit appends
stdout to the prompt, and Stop and PermissionRequest honour a JSON
decision, so a stray println would change what the agent does.

Prompt text is gated by its own fidelity key, defaulting to off,
so a corpus can carry full command text without carrying what the
human typed.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 14: Retention and the `hook-log` CLI

**Files:**
- Create: `crates/devkit-common/src/harness_log/prune.rs`, `src/bin/devkit/hook_log.rs`
- Modify: `src/bin/devkit/main.rs`, `src/bin/devkit/hook/mod.rs` (session-end runs the sweep last)
- Test: `crates/devkit-common/src/harness_log/prune.rs` (`mod tests`)

**Interfaces:**
- Produces: `prune::sweep(&Settings) -> Outcome`, where `Outcome` names files removed, bytes freed, whether it bailed on a held lock, and whether the size cap was reached.
- Produces: `devkit hook-log path` and `devkit hook-log prune`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_retention_sweep_reads_the_directory_name_not_an_mtime() {
    let t = tree(&[("2020-01-01", &["s1.jsonl"]), ("2099-01-01", &["s2.jsonl"])]);
    touch_now(&t.path().join("2020-01-01/s1.jsonl")); // fresh mtime, stale name
    sweep(&settings(t.path(), Some(30), None));
    assert!(!t.path().join("2020-01-01").exists());
    assert!(t.path().join("2099-01-01").exists());
}

#[test]
fn a_file_touched_inside_the_recency_window_is_spared() {
    let t = tree(&[("2020-01-01", &["live.jsonl", "old.jsonl"])]);
    touch_now(&t.path().join("2020-01-01/live.jsonl"));
    set_mtime_days_ago(&t.path().join("2020-01-01/old.jsonl"), 400);
    sweep(&settings(t.path(), Some(30), None));
    assert!(t.path().join("2020-01-01/live.jsonl").exists(), "a live session's file");
    assert!(!t.path().join("2020-01-01/old.jsonl").exists());
}

#[test]
fn a_directory_is_removed_only_once_empty() {
    let t = tree(&[("2020-01-01", &["a.jsonl"])]);
    set_mtime_days_ago(&t.path().join("2020-01-01/a.jsonl"), 400);
    sweep(&settings(t.path(), Some(30), None));
    assert!(!t.path().join("2020-01-01").exists());
}

#[test]
fn todays_directory_survives_the_size_sweep() {
    let t = tree(&[(&today_utc(), &["big.jsonl"])]);
    write_bytes(&t.path().join(format!("{}/big.jsonl", today_utc())), 10_000);
    let out = sweep(&settings(t.path(), None, Some(100)));
    assert!(t.path().join(today_utc()).exists());
    assert!(!out.cap_reached, "a sweep that cannot meet the cap reports it rather than deleting on");
}

#[test]
fn a_held_lock_bails_immediately() {
    let t = tree(&[]);
    let _held = hold_lock(&t.path().join("prune.lock"));
    let start = std::time::Instant::now();
    let out = sweep(&settings(t.path(), Some(1), None));
    assert!(out.bailed);
    assert!(start.elapsed() < std::time::Duration::from_millis(500), "never a blocking wait");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`

- [ ] **Step 3: Implement**

A non-blocking `try_lock` on `<dir>/prune.lock` coordinates pruners with each other; a held lock bails, silently on the auto path and with a message on the manual one, because session end must not sit behind another session's sweep. The recency window is one hour and is the only thing between the sweep and a live session's file, since writers take no lock. `NotFound` from a removal is success. The whole sweep is fail-open: a prune failure never changes the hook's exit.

In `hook/mod.rs`, `session-end` runs release, then record, then the sweep. Release first because it is the step with a correctness consequence; the sweep last because it is the only one that can be skipped without loss.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-common/src/harness_log/prune.rs,src/bin/devkit/hook_log.rs,src/bin/devkit/main.rs,src/bin/devkit/hook/mod.rs' \
  --arg commit_subject='feat(harness-log): add retention and the hook-log verbs' \
  --arg commit_body='Named hook-log rather than log: paths::logs_dir is already
state_dir/logs for daemon and server logs, and devrun logs already
tails those, so a bare devkit log would name the wrong thing twice.

Three guards cover three different races. A non-blocking lock
coordinates pruners with each other. An mtime recency window is
the only thing between a sweep and a live session file, because
writers take no lock, and without it a session spanning midnight
would be a scheduled loss rather than a rare one. Today never
goes, whatever the size sweep says.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 15: The doctor row

**Files:**
- Modify: `src/bin/devkit/doctor.rs:442-453`
- Test: `tests/doctor_harness_log.rs`

**Interfaces:**
- Consumes: `harness_log::resolve` from Task 8.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_row_reports_the_effective_mode_not_the_configured_one() {
    let (p, home) = project_with_global("[harness.log]\nenabled = true\ncommand = \"full\"\n");
    write_project(&p, "[harness.log]\ncommand = \"hashed\"\n");
    let v = doctor_json(p.path(), home.path());
    assert_eq!(v["rows"]["harness_log"]["command"], "hashed");
}

#[test]
fn full_fidelity_is_marked_as_a_warning() {
    let (p, home) = project_with_global("[harness.log]\nenabled = true\ncommand = \"full\"\n");
    let out = doctor_human(p.path(), home.path());
    assert!(out.contains("harness_log"));
    assert!(out.contains("full"));
    assert!(out.contains('\u{26a0}'), "full command text warrants a marker");
}

#[test]
fn the_row_says_when_no_global_config_was_found() {
    let (p, home) = project_with_no_global();
    let v = doctor_json(p.path(), home.path());
    assert_eq!(v["rows"]["harness_log"]["global_config"], serde_json::json!(false));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `devrun task test -C <worktree>`

- [ ] **Step 3: Implement**

The row names the resolved directory, its size on disk, the effective `command` and `prompt` modes, and whether a global config was found at all. `print_human` gains its first colour path: `ui::yellow` plus a warning marker when `command` resolves to `full`. `ui` has no orange helper and does not gain one. The `--json` output carries no colour, so the effective mode is a data field there.

The row exists because the runtime probe reads each `[harness]` key independently, so a misspelled key is invisible at runtime. `HarnessSection` carries no `deny_unknown_fields`, and adding one would not help, since nothing deserialises through the struct on that path.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `devrun task test -C <worktree>`

- [ ] **Step 5: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='src/bin/devkit/doctor.rs,tests/doctor_harness_log.rs' \
  --arg commit_subject='feat(doctor): report the effective harness log mode' \
  --arg commit_body='The runtime probe reads each harness key independently, so a
misspelled key changes nothing and reports nothing. A row printing
what you are actually getting is what catches it, which is why it
reports the effective mode rather than the configured one.

Full command text carries a warning marker and a yellow row.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

### Task 16: The offline consumer and the docs

**Files:**
- Modify: `crates/devkit-command/examples/corpus_probe.rs`
- Modify: `docs/agents.md:42-56`, `docs/commands.md:286-288`, `docs/configuration.md:340,352`, `skills/using-devkit/references/locks.md:73`, `AGENTS.md`
- Test: `crates/devkit-command/examples/corpus_probe.rs` round-trip

**Interfaces:**
- Consumes: the `shell_pre` record shape from Task 11.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn both_corpus_formats_feed_the_probe() {
    // A devkit shell_pre record, top-level `command`.
    let native = r#"{"kind":"shell_pre","command":"ls -la","dialect":"bash"}"#;
    // An externally produced record, nested.
    let legacy = r#"{"tool_input":{"command":"ls -la"}}"#;
    assert_eq!(command_of(&parse(native)).unwrap(), "ls -la");
    assert_eq!(command_of(&parse(legacy)).unwrap(), "ls -la");
    assert_eq!(dialect_of(&parse(native)), Some(Dialect::Bash), "the record's own dialect wins");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `devrun task test -C <worktree>`

- [ ] **Step 3: Implement**

`command_of` already reads a top-level `command`, so the only real edit is dialect selection: prefer the record's own `dialect` over inferring from harness plus platform. Orphaning the accumulated external corpus would be a bad trade for a format change, so the nested fallback stays.

- [ ] **Step 4: Update the docs**

`docs/agents.md`: the hook table grows from six rows to the full per-harness mapping, and gains the three vendor documentation links from the spec's References section, so the next person adding a verb checks the vendor rather than recalling it.

`docs/commands.md`: replace the `harness` section with `hook` and `hook-log`.

`docs/configuration.md`: `[harness.log]`, and the places naming the retired commands, at line 340 (`devkit harness shell`) and line 352 (`lockm hook <event>`) with their neighbours.

`skills/using-devkit/references/locks.md:73`: `devkit harness shell` becomes `devkit hook pre-tool-use`.

`AGENTS.md`: the layout rows for `src/bin/devkit/` and `crates/devkit-common`, and the shell-hook invariant, which gains the exit-code rule as a sentence: no verb in the `hook` family exits 2, because exit 2 is a block on two of the three harnesses.

- [ ] **Step 5: Run the whole gate**

Run: `devrun task test -C <worktree>`, `devrun task test-doc -C <worktree>`, `devrun task lint -C <worktree>`, `devrun task fmt-check -C <worktree>`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
devrun task commit -C <worktree> \
  --arg files='crates/devkit-command/examples/corpus_probe.rs,docs/agents.md,docs/commands.md,docs/configuration.md,skills/using-devkit/references/locks.md,AGENTS.md' \
  --arg commit_subject='docs: document the hook family and harness logging' \
  --arg commit_body='The agents.md hook table becomes the full per-harness mapping
and carries the vendor documentation links, so a verb added later
is checked against the vendor rather than recalled.

corpus_probe prefers a record dialect over inferring one, and
keeps its nested fallback so an externally produced corpus is not
orphaned.' \
  --arg coauthors='Claude Opus 5 <noreply@anthropic.com>'
```

---

## Verification before opening the PR

1. `devrun task test`, `test-doc`, `lint` and `fmt-check` all green.
2. With logging off, drive one real Claude Code session and confirm no file appears under the default log directory.
3. Enable it in the global config at `command = "redacted"`, run a session, and read the records with `jq` from `devkit hook-log path`.
4. Confirm a Cursor session resolves as Cursor, using a captured `preToolUse` payload. This is the one behaviour fix with no local source to check against.
5. Put the measured per-verb spawn cost from Task 13 in the PR body.

## Unresolved questions

1. Cursor's `"version": 1` manifest schema is undocumented for `subagentStart`, `workspaceOpen` and `postToolUseFailure`. If it rejects any of them, drop that row rather than raising the version, which changes how every other row is read. One real session settles it.
2. Whether empty stdout from Cursor's `preToolUse` and `beforeSubmitPrompt` is an allow. The shipped `beforeShellExecution` hook prints nothing on an allow and works, which is evidence but not proof for the generic trio.
3. Cursor's edit tool names, which the `preToolUse` matcher needs. Without them the matcher covers `Shell` alone and Cursor edits stay unclaimed, which is today's behaviour rather than a regression.
4. Whether `worktree-create` and `worktree-remove` earn their spawn at all. They are record-only, and no consumer reads a `worktree` record yet.
