//! Claude Code hook glue: holder derivation and payload parsing. Agent-specific
//! shapes live here; the deny envelope and per-checkout activation gate are
//! `devkit_common::harness`, re-exported below; the registry decision logic
//! stays in `model`/`store`.

use std::path::Path;

pub use devkit_common::harness::deny_json;
use serde_json::Value;

/// Whether write enforcement is active for a write originating at `cwd`.
pub fn enforcement_enabled(cwd: &Path) -> bool {
    devkit_common::harness::enforcement_enabled(cwd, "enforce_writes", "DEVKIT_ENFORCE_WRITES")
}

/// Tool names whose writes the harness governs. The Claude Code names carry one
/// target in `tool_input.file_path`; Codex's `apply_patch` carries a whole
/// patch envelope in `tool_input.command` and may name several.
const WRITE_TOOLS: [&str; 5] = ["Edit", "MultiEdit", "Write", "NotebookEdit", "apply_patch"];

/// Codex's `apply_patch` tool name, whose payload needs the envelope parser.
const APPLY_PATCH: &str = "apply_patch";

/// Envelope headers that name a file, in the order `apply_patch` writes them.
/// `Move to` is a rename's destination, so a rename claims both ends.
const PATCH_VERBS: [&str; 4] = ["Add File", "Update File", "Delete File", "Move to"];

/// Two-level holder id: top-level agents are `session_id`; sub-agents are
/// `session_id/agent_id`. The Claude Code payload exposes no deeper ancestry.
pub fn holder_from_fields(session_id: &str, agent_id: Option<&str>) -> String {
    match agent_id {
        Some(a) if !a.is_empty() => format!("{session_id}/{a}"),
        _ => session_id.to_string(),
    }
}

/// What a hook payload asks the lock registry to do. Produced by the per-verb
/// parsers below; an event with nothing to do returns `None` rather than a
/// variant, so an unrecognised event has nowhere to arrive.
#[derive(Debug)]
pub enum LockAction {
    Write {
        tool_name: String,
        file_paths: Vec<String>,
        holder: String,
    },
    ReleaseSubagent {
        holder: String,
    },
    ReleaseSession {
        holder: String,
    },
    /// A write this hook cannot evaluate. Denied rather than allowed: the write
    /// path fails closed, so an unreadable payload must not open the window.
    Unusable {
        reason: String,
    },
}

fn str_field<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Every file an `apply_patch` envelope writes, in the order it names them.
/// Paths are taken verbatim — they are relative to the invoking session's cwd,
/// which the caller supplies; this parser does not resolve them.
pub fn apply_patch_paths(command: &str) -> Vec<String> {
    command
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("*** ")?;
            let (verb, path) = rest.split_once(": ")?;
            PATCH_VERBS
                .contains(&verb)
                .then(|| path.trim())
                .filter(|p| !p.is_empty())
                .map(str::to_string)
        })
        .collect()
}

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

/// A session end with no session id names no holder, so it releases nothing.
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn holder_top_level_is_session() {
        assert_eq!(holder_from_fields("S", None), "S");
    }

    #[test]
    fn holder_subagent_is_session_slash_agent() {
        assert_eq!(holder_from_fields("S", Some("a1")), "S/a1");
    }

    #[test]
    fn parse_write_event_pulls_file_and_holder() {
        let p = json!({
            "session_id": "S",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/a.rs" }
        });
        match parse_write(&p) {
            Some(LockAction::Write {
                tool_name,
                file_paths,
                holder,
            }) => {
                assert_eq!(tool_name, "Edit");
                assert_eq!(file_paths, vec!["/repo/src/a.rs"]);
                assert_eq!(holder, "S");
            }
            other => panic!("expected Write, got {other:?}"),
        }
    }

    #[test]
    fn parse_write_event_subagent_holder() {
        let p = json!({
            "session_id": "S", "agent_id": "a1",
            "tool_name": "Write", "tool_input": { "file_path": "/repo/x" }
        });
        match parse_write(&p) {
            Some(LockAction::Write { holder, .. }) => assert_eq!(holder, "S/a1"),
            other => panic!("expected Write, got {other:?}"),
        }
    }

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
    fn write_without_session_id_names_the_missing_field() {
        let p = json!({
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/a.rs" }
        });
        match parse_write(&p) {
            Some(LockAction::Unusable { reason }) => assert!(reason.contains("session_id")),
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn write_with_no_extractable_target_is_unusable() {
        let p = json!({ "session_id": "S", "tool_name": "Edit", "tool_input": {} });
        match parse_write(&p) {
            Some(LockAction::Unusable { reason }) => assert!(reason.contains("target")),
            other => panic!("expected Unusable, got {other:?}"),
        }
    }

    #[test]
    fn non_write_tool_without_session_id_is_still_not_a_write() {
        let p = json!({ "tool_name": "Bash", "tool_input": { "command": "ls" } });
        assert!(parse_write(&p).is_none());
    }

    #[test]
    fn the_write_tools_are_the_ones_the_dispatch_asks_about() {
        assert!(is_write_tool("Edit"));
        assert!(is_write_tool("apply_patch"));
        assert!(!is_write_tool("Bash"));
        assert!(!is_write_tool("Shell"));
    }

    #[test]
    fn apply_patch_paths_covers_every_envelope_verb() {
        let patch = "*** Begin Patch\n\
                     *** Add File: src/new.rs\n\
                     +fn main() {}\n\
                     *** Update File: src/old.rs\n\
                     @@\n\
                     -a\n\
                     +b\n\
                     *** Delete File: src/gone.rs\n\
                     *** End Patch\n";
        assert_eq!(apply_patch_paths(patch), vec![
            "src/new.rs",
            "src/old.rs",
            "src/gone.rs"
        ]);
    }

    #[test]
    fn apply_patch_paths_includes_both_ends_of_a_rename() {
        let patch = "*** Begin Patch\n\
                     *** Update File: src/from.rs\n\
                     *** Move to: src/to.rs\n\
                     *** End Patch\n";
        assert_eq!(apply_patch_paths(patch), vec!["src/from.rs", "src/to.rs"]);
    }

    #[test]
    fn parse_apply_patch_event_pulls_every_target() {
        let p = json!({
            "session_id": "S",
            "tool_name": "apply_patch",
            "tool_input": {
                "command": "*** Begin Patch\n*** Update File: a.rs\n*** Add File: b.rs\n*** End Patch\n"
            }
        });
        match parse_write(&p) {
            Some(LockAction::Write {
                tool_name,
                file_paths,
                holder,
            }) => {
                assert_eq!(tool_name, "apply_patch");
                assert_eq!(file_paths, vec!["a.rs", "b.rs"]);
                assert_eq!(holder, "S");
            }
            other => panic!("expected Write, got {other:?}"),
        }
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
    fn a_subagent_stop_with_no_session_releases_nothing() {
        assert!(parse_subagent_stop(&json!({ "agent_id": "a1" })).is_none());
    }

    #[test]
    fn a_session_end_needs_a_session_id() {
        assert!(parse_session_end(&json!({})).is_none());
        assert!(matches!(
            parse_session_end(&json!({"session_id": "s1"})),
            Some(LockAction::ReleaseSession { holder }) if holder == "s1"
        ));
    }

    #[test]
    fn deny_json_has_pretooluse_envelope() {
        let v = deny_json("blocked by S/a1");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "blocked by S/a1"
        );
    }
}
