//! Every shipped manifest names verbs this binary understands, and names the
//! harness that reads it.
//!
//! The manifests flip in the same release as the verbs, so a spelling that
//! merely looks right is not enough: each one is parsed against the binary it
//! ships beside.

use std::path::Path;

const MANIFESTS: [(&str, &str); 3] = [
    ("hooks/hooks.json", "claude-code"),
    ("hooks/hooks-codex.json", "codex"),
    ("hooks/hooks-cursor.json", "cursor"),
];

fn commands(path: &str) -> Vec<String> {
    let body = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
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

fn hook_commands(path: &str) -> Vec<String> {
    commands(path)
        .into_iter()
        .filter(|c| c.contains("devkit hook "))
        .collect()
}

#[test]
fn no_manifest_names_a_retired_command() {
    for (f, _) in MANIFESTS {
        for c in commands(f) {
            assert!(!c.contains("lockm hook"), "{f}: {c}");
            assert!(!c.contains("harness shell"), "{f}: {c}");
        }
    }
}

#[test]
fn every_hook_command_names_its_harness() {
    for (f, name) in MANIFESTS {
        let found = hook_commands(f);
        assert!(!found.is_empty(), "{f} names no hook verbs at all");
        for c in &found {
            assert!(c.contains(&format!("--harness {name}")), "{f}: {c}");
        }
    }
}

#[test]
fn every_verb_a_manifest_names_parses() {
    let exe = Path::new(env!("CARGO_BIN_EXE_devkit"));
    for (f, _) in MANIFESTS {
        for c in hook_commands(f) {
            let verb = c.split_whitespace().nth(2).unwrap();
            let out = std::process::Command::new(exe)
                .args(["hook", verb, "--help"])
                .env("DEVKIT_SKIP_AUTOLINK", "1")
                .output()
                .unwrap();
            assert!(out.status.success(), "{f} names an unknown verb: {verb}");
        }
    }
}

/// The merged `PreToolUse` block needs a matcher covering both tool families,
/// or it spawns on every Read, Grep, Glob, MCP and Agent call as well.
#[test]
fn the_merged_pre_tool_use_block_keeps_a_matcher() {
    for (f, _) in MANIFESTS {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(f).unwrap()).unwrap();
        // Cursor's manifest is a flat map of event to command list and carries
        // no matchers at all; only the two that take them are checked.
        let Some(blocks) = v["hooks"]["PreToolUse"].as_array() else {
            continue;
        };
        assert_eq!(blocks.len(), 1, "{f}: the two blocks merge into one");
        let matcher = blocks[0]["matcher"].as_str().unwrap_or_default();
        assert!(matcher.contains("Bash"), "{f}: {matcher}");
        assert!(matcher.contains("Write"), "{f}: {matcher}");
    }
}

/// `SessionEnd` releases, records and prunes, and Claude Code gives every
/// `SessionEnd` hook a shared 1.5-second budget unless a manifest asks for
/// more.
#[test]
fn session_end_asks_for_a_budget_of_its_own() {
    for (f, _) in MANIFESTS {
        let found: Vec<_> = hook_commands(f)
            .into_iter()
            .filter(|c| c.contains("hook session-end"))
            .collect();
        assert_eq!(found.len(), 1, "{f} wires session-end exactly once");
        let body = std::fs::read_to_string(f).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let mut timeouts = Vec::new();
        find_timeouts(&v, "hook session-end", &mut timeouts);
        assert_eq!(timeouts, vec![15], "{f}: session-end needs its own timeout");
    }
}

fn find_timeouts(v: &serde_json::Value, needle: &str, out: &mut Vec<u64>) {
    match v {
        serde_json::Value::Object(m) => {
            if m.get("command")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|c| c.contains(needle))
            {
                out.extend(m.get("timeout").and_then(serde_json::Value::as_u64));
            }
            m.values().for_each(|x| find_timeouts(x, needle, out));
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| find_timeouts(x, needle, out)),
        _ => {}
    }
}
