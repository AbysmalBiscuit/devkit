//! `devkit todo context`: what the hooks inject, driven with the payloads
//! each event sends.

#[path = "common/todoenv.rs"]
mod todoenv;

use serde_json::{Value, json};
use todoenv::{Proj, stderr, stdout};

const S1: [(&str, &str); 1] = [("CLAUDE_CODE_SESSION_ID", "s1")];

fn context(p: &Proj, flags: &[&str], payload: Value) -> String {
    let mut args = vec!["todo", "context", "--harness", "claude-code"];
    args.extend_from_slice(flags);
    let out = p.devkit_in(&p.path, &args, &[], &payload.to_string());
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out)
}

fn event(p: &Proj, name: &str) -> Value {
    json!({"hook_event_name": name, "session_id": "s1", "cwd": p.path, "source": "startup"})
}

fn injected(out: &str) -> String {
    let v: Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("{e}: {out}"));
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn session_start_injects_the_guide_and_lists() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "one"], &S1);
    let text = injected(&context(&p, &[], event(&p, "SessionStart")));
    assert!(
        text.contains("Write your own todos to `proj.main.claude-s1`"),
        "{text}"
    );
    assert!(text.contains("- [ ] one (1)"), "{text}");
    assert!(text.contains("Ids are in parentheses"), "{text}");
    assert!(
        text.contains("quote a GitHub ticket's key, `devkit todo start '#65'`, or give its full key `owner/repo#65`"),
        "{text}"
    );
    assert!(text.contains("`#` starts a shell comment"), "{text}");
}

#[test]
fn an_unchanged_list_is_not_repeated_on_the_next_prompt() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "one"], &S1);
    context(&p, &[], event(&p, "SessionStart"));
    let prompt = ["--guide", "none", "--if-changed"];
    assert_eq!(context(&p, &prompt, event(&p, "UserPromptSubmit")), "");
    p.devkit(&["todo", "add", "two"], &S1);
    let text = injected(&context(&p, &prompt, event(&p, "UserPromptSubmit")));
    assert!(text.contains("- [ ] two (2)"), "{text}");
    assert!(!text.contains("Todo list, kept by devkit."), "{text}");
    assert_eq!(context(&p, &prompt, event(&p, "UserPromptSubmit")), "");
}

#[test]
fn a_sub_agents_injection_does_not_suppress_the_parent() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "one"], &S1);
    let prompt = ["--guide", "none", "--if-changed"];
    context(&p, &prompt, event(&p, "UserPromptSubmit"));
    p.devkit(&["todo", "add", "two"], &S1);
    let mut sub = event(&p, "SubagentStart");
    sub["agent_id"] = json!("a1");
    sub["agent_type"] = json!("general-purpose");
    assert!(injected(&context(&p, &[], sub)).contains("- [ ] two (2)"));
    let parent = context(&p, &prompt, event(&p, "UserPromptSubmit"));
    assert!(injected(&parent).contains("- [ ] two (2)"), "{parent}");
}

#[test]
fn no_session_means_silence() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "one"], &S1);
    let payload = json!({"hook_event_name": "SessionStart", "cwd": p.path});
    assert_eq!(context(&p, &[], payload), "");
    let out = p.devkit_in(
        &p.path,
        &["todo", "context", "--harness", "codex"],
        &[],
        "not json",
    );
    assert!(out.status.success());
    assert_eq!(stdout(&out), "");
}

#[test]
fn post_compact_prints_plain_text() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "one"], &S1);
    let out = context(&p, &[], event(&p, "PostCompact"));
    assert!(out.starts_with("Todo list, kept by devkit."), "{out}");
    assert!(out.contains("- [ ] one (1)"), "{out}");
}

#[test]
fn a_failed_repository_lookup_means_silence() {
    let p = Proj::new();
    let no_git = tempfile::tempdir().unwrap();
    let out = p.devkit_in(
        &p.path,
        &["todo", "context", "--harness", "claude-code"],
        &[("PATH", no_git.path().to_str().unwrap())],
        &event(&p, "SessionStart").to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
}

const LEFT: &str = "Other sessions on `proj.main` left 1 pending todo; \
                    `devkit todo list --subtree proj.main` lists them.";

#[test]
fn session_start_names_the_todos_other_sessions_left() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "earlier work"], &[(
        "CLAUDE_CODE_SESSION_ID",
        "s0",
    )]);
    p.devkit(&["todo", "add", "elsewhere"], &[(
        "CLAUDE_CODE_SESSION_ID",
        "s0",
    )]);
    p.devkit(&["todo", "done", "2"], &[("CLAUDE_CODE_SESSION_ID", "s0")]);
    let text = injected(&context(&p, &[], event(&p, "SessionStart")));
    assert!(text.contains(LEFT), "{text}");
    assert!(
        !text.contains("earlier work"),
        "a sibling's list is never shown: {text}"
    );
}

#[test]
fn the_line_returns_on_a_prompt_only_while_the_own_list_is_empty() {
    let p = Proj::new();
    p.devkit(&["todo", "add", "earlier work"], &[(
        "CLAUDE_CODE_SESSION_ID",
        "s0",
    )]);
    let prompt = ["--guide", "none"];
    let empty_own = injected(&context(&p, &prompt, event(&p, "UserPromptSubmit")));
    assert!(empty_own.contains(LEFT), "{empty_own}");
    p.devkit(&["todo", "add", "mine"], &S1);
    let busy = injected(&context(&p, &prompt, event(&p, "UserPromptSubmit")));
    assert!(!busy.contains("Other sessions"), "{busy}");
    assert!(busy.contains("- [ ] mine (2)"), "{busy}");
}

#[test]
fn another_branchs_sessions_are_not_counted() {
    let p = Proj::new();
    p.devkit(
        &["todo", "add", "--node", "proj.other.claude-s0", "far"],
        &[("DEVKIT_CALLER", "human")],
    );
    let text = injected(&context(&p, &[], event(&p, "SessionStart")));
    assert!(!text.contains("Other sessions"), "{text}");
}

#[test]
fn injected_lists_show_blocking_notes() {
    let p = Proj::new();
    let blocker = add_text(&p, "blocker");
    p.devkit(&["todo", "add", "waiting", "--blocked-by", &blocker], &S1);
    let text = injected(&context(&p, &[], event(&p, "SessionStart")));
    assert!(text.contains("blocked by 1"), "{text}");
    p.devkit(&["todo", "done", &blocker], &S1);
    let ready = injected(&context(&p, &[], event(&p, "SessionStart")));
    assert!(ready.contains("ready"), "{ready}");
}

#[test]
fn a_subagent_sees_no_ready_note_on_its_managers_list() {
    let p = Proj::with_home_config(
        "[todo.roles.manager]\nscope = \"workspace\"\n[todo.roles.implementer]\nscope = \"agent\"\nparent = \"manager\"\n",
    );
    p.devkit(&["todo", "role", "manager"], &S1);
    let blocker = add_text(&p, "blocker");
    p.devkit(&["todo", "done", &blocker], &S1);
    p.devkit(&["todo", "add", "waiting", "--blocked-by", &blocker], &S1);
    let mut sub = event(&p, "SubagentStart");
    sub["agent_id"] = json!("a1");
    sub["agent_type"] = json!("implementer");
    let text = injected(&context(&p, &[], sub));
    assert!(text.contains("waiting"), "{text}");
    assert!(!text.contains("ready"), "{text}");
}

#[test]
fn a_purged_blocker_leaves_its_dependent_ready() {
    let p = Proj::new();
    let blocker = add_text(&p, "blocker");
    p.devkit(&["todo", "add", "waiting", "--blocked-by", &blocker], &S1);
    let purged = p.devkit(&["todo", "purge", &blocker], &[("DEVKIT_CALLER", "human")]);
    assert!(purged.status.success(), "{}", stderr(&purged));
    let text = injected(&context(&p, &[], event(&p, "SessionStart")));
    assert!(text.contains("ready"), "{text}");
}

fn add_text(p: &Proj, text: &str) -> String {
    let out = p.devkit(&["todo", "add", text], &S1);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).trim().to_string()
}
