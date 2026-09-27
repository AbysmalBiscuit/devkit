//! The edit path of `devkit hook pre-tool-use`, and the two release verbs.
//!
//! A structured edit names its targets in the payload, so there is nothing to
//! analyse: the targets are claimed as they stand. Fails closed, the same as
//! the shell path's write stage. A payload this cannot evaluate denies rather
//! than allows, because an unevaluable write must not open the window.
//!
//! The release verbs carry no permission decision at all. Nothing they can
//! answer would be read.

use devkit_common::{
    git::Checkout,
    harness_log::{self, Decision, EditPre, Kind, Verdict},
};
use devkit_locks::{
    hook,
    model::{Conflict, WriteDecision},
};
use pabal::Tool;

use super::{HookEvent, payload::Payload, record, rules, shell};

/// Tools pabal reads write targets from. One that arrives without a readable
/// target is a harness format change, so it is a write to deny rather than a
/// tool to ignore.
const WRITE_TOOLS: [&str; 5] = ["Edit", "MultiEdit", "Write", "NotebookEdit", "apply_patch"];

/// What a `PreToolUse` call asks to write.
pub enum Write {
    Targets {
        paths: Vec<String>,
        holder: String,
    },
    /// A write this hook cannot evaluate.
    Unusable(String),
}

/// The call's write intent. `None` when the tool does not write, which is the
/// common case and not a failure.
pub fn write(payload: &Payload) -> Option<Write> {
    let name = payload.tool_name().unwrap_or_default();
    let paths = match payload.tool() {
        Some(Tool::Edit(edit)) => edit.paths(),
        _ if WRITE_TOOLS.contains(&name) => Vec::new(),
        _ => return None,
    };
    let Some(session) = payload.session_id() else {
        return Some(Write::Unusable(
            "write payload carries no session_id".into(),
        ));
    };
    if paths.is_empty() {
        return Some(Write::Unusable(format!(
            "write payload for {name} names no target"
        )));
    }
    Some(Write::Targets {
        paths: paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        holder: hook::holder_from_fields(session, payload.agent()),
    })
}

/// Claim the write targets a structured-edit payload names, before the tool
/// runs.
///
/// This is the edit path's single emission site. Anything appended to stdout
/// after a denial makes the whole output unparseable, and a harness that
/// cannot parse a hook's stdout proceeds with the call it was asked to gate.
pub fn guard(payload: &Payload, write: Write) -> anyhow::Result<()> {
    let cwd = record::payload_cwd(payload);
    // One `git worktree list` for the whole invocation, shared by the
    // enforcement gate, the lock scoping and the log settings. It resolves
    // lazily, so a tool that writes nothing spawns nothing.
    let checkout = Checkout::at(&cwd);
    let (targets, blocks) = match write {
        Write::Targets { paths, holder } => {
            let blocks = match claim(payload, &checkout, &cwd, &paths, &holder) {
                Some(reason) => vec![reason],
                None => {
                    rules::inject(payload, &checkout, &cwd, &paths, &holder);
                    Vec::new()
                }
            };
            (paths, blocks)
        }
        Write::Unusable(reason) => {
            let message = format!("devkit write-harness: {reason} (fail-closed)");
            let blocks = if hook::enforcement_enabled_in(&checkout, &cwd) {
                vec![message]
            } else {
                Vec::new()
            };
            (Vec::new(), blocks)
        }
    };
    if let Some(reason) = blocks.first() {
        shell::print_envelope(&payload.harness().deny(reason));
    }
    // Envelope first, then the record: a stall on the log directory before the
    // envelope exists runs into the manifest timeout, and a harness timeout
    // allows the call.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let settings = harness_log::resolve_in(&checkout, &cwd);
    if settings.enabled {
        let rec = record::envelope(
            payload,
            HookEvent::PreToolUse,
            &checkout,
            Kind::EditPre(EditPre {
                tool_name: payload.tool_name().map(str::to_string),
                targets,
                verdict: Verdict {
                    decision: if blocks.is_empty() {
                        Decision::Allow
                    } else {
                        Decision::Deny
                    },
                    blocks,
                    warnings: Vec::new(),
                },
            }),
        );
        harness_log::record(&settings, &rec);
    }
    Ok(())
}

/// Claim each target, returning the reason the write is denied, if it is.
/// Nothing here prints; `guard` is the only site that emits.
fn claim(
    payload: &Payload,
    checkout: &Checkout,
    cwd: &std::path::Path,
    file_paths: &[String],
    holder: &str,
) -> Option<String> {
    if !hook::enforcement_enabled_in(checkout, cwd) {
        return None; // no opt-in (env, project layers, or global config) -> no enforcement
    }
    let mut conflicts = Vec::new();
    let mut resolver = devkit_locks::WriteResolver::with_checkout(checkout.clone());
    for path in file_paths {
        let target = resolve_against(payload, path);
        match resolver.decide_write(&target, holder, Some("write-harness"), 1800) {
            Ok(WriteDecision::Denied(c)) => conflicts.extend(c),
            Ok(_) => {}
            // fail closed: a registry error must not silently reopen the window
            Err(e) => {
                return Some(format!(
                    "devkit write-harness: registry error (fail-closed): {e:#}"
                ));
            }
        }
    }
    (!conflicts.is_empty()).then(|| conflict_reason(&conflicts))
}

pub fn release_subagent(payload: &Payload) {
    // Releasing the bare session holder here would free the parent's and every
    // sibling's locks, so an unattributable stop, a fork's included, releases
    // nothing.
    if let (Some(session), Some(agent)) = (payload.session_id(), payload.agent()) {
        release(&hook::holder_from_fields(session, Some(agent)));
    }
}

pub fn release_session(payload: &Payload) {
    if let Some(session) = payload.session_id() {
        release(session);
    }
}

fn release(holder: &str) {
    if devkit_locks::release_prefix(holder).is_ok() {
        rules::clear_for_holder(holder);
    }
}

/// The deny reason naming every holder in the way.
fn conflict_reason(conflicts: &[Conflict]) -> String {
    let who = conflicts
        .iter()
        .map(|c| format!("{} (held by {})", c.path, c.held_by))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "devkit write-harness: {who}, locked by another agent; \
         coordinate or wait for it to finish"
    )
}

/// Resolve a payload path against the session's own working directory, so a
/// relative target names the file the session meant rather than one relative to
/// wherever the harness spawned the hook process.
fn resolve_against(payload: &Payload, path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    payload
        .cwd()
        .map(|cwd| cwd.join(p).to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn payload(raw: serde_json::Value) -> Payload {
        Payload::new(None, raw).unwrap()
    }

    fn targets(raw: serde_json::Value) -> (Vec<String>, String) {
        match write(&payload(raw)) {
            Some(Write::Targets { paths, holder }) => (paths, holder),
            Some(Write::Unusable(reason)) => panic!("expected targets, got Unusable({reason})"),
            None => panic!("expected targets, got no write"),
        }
    }

    fn unusable(raw: serde_json::Value) -> String {
        match write(&payload(raw)) {
            Some(Write::Unusable(reason)) => reason,
            _ => panic!("expected Unusable"),
        }
    }

    #[test]
    fn a_write_names_its_file_and_its_session() {
        let (paths, holder) = targets(json!({
            "hook_event_name": "PreToolUse", "session_id": "S",
            "tool_name": "Edit", "tool_input": { "file_path": "/repo/src/a.rs" }
        }));
        assert_eq!(paths, vec!["/repo/src/a.rs"]);
        assert_eq!(holder, "S");
    }

    #[test]
    fn a_subagents_write_is_held_by_session_slash_agent() {
        let (_, holder) = targets(json!({
            "session_id": "S", "agent_id": "a1", "agent_type": "general-purpose",
            "tool_name": "Write", "tool_input": { "file_path": "/repo/x" }
        }));
        assert_eq!(holder, "S/a1");
    }

    #[test]
    fn a_forks_write_is_held_by_its_session() {
        let (_, holder) = targets(json!({
            "session_id": "S", "agent_id": "afork",
            "tool_name": "Write", "tool_input": { "file_path": "/repo/x" }
        }));
        assert_eq!(holder, "S");
    }

    #[test]
    fn a_tool_that_does_not_write_is_not_a_write() {
        assert!(write(&payload(json!({"tool_name": "Read", "session_id": "s1"}))).is_none());
        assert!(
            write(&payload(
                json!({"tool_name": "Bash", "tool_input": {"command": "ls"}})
            ))
            .is_none()
        );
    }

    #[test]
    fn a_write_with_no_session_names_the_missing_field() {
        let reason = unusable(json!({
            "tool_name": "Edit", "tool_input": { "file_path": "/repo/src/a.rs" }
        }));
        assert!(reason.contains("session_id"), "{reason}");
    }

    #[test]
    fn a_write_with_no_readable_target_is_unusable() {
        let reason = unusable(json!({ "session_id": "S", "tool_name": "Edit", "tool_input": {} }));
        assert!(reason.contains("target"), "{reason}");
    }

    #[test]
    fn a_patch_claims_every_file_it_names() {
        let (paths, _) = targets(json!({
            "hook_event_name": "PreToolUse", "turn_id": "t1", "session_id": "S",
            "tool_name": "apply_patch",
            "tool_input": {
                "command": "*** Begin Patch\n*** Update File: a.rs\n*** Move to: c.rs\n*** Add File: b.rs\n*** End Patch\n"
            }
        }));
        assert_eq!(paths, vec!["a.rs", "c.rs", "b.rs"]);
    }

    #[test]
    fn a_denial_names_the_path_and_its_holder() {
        let reason = conflict_reason(&[Conflict {
            path: "src/a.rs".into(),
            held_by: "S/b2".into(),
            age_secs: 5,
            note: None,
        }]);
        assert!(reason.contains("S/b2"), "reason names the holder: {reason}");
        assert!(
            reason.contains("src/a.rs"),
            "reason names the path: {reason}"
        );
    }

    #[test]
    fn an_absolute_target_is_left_alone() {
        let p = payload(json!({ "cwd": "/repo" }));
        assert_eq!(resolve_against(&p, "/tmp/a.rs"), "/tmp/a.rs");
    }

    #[test]
    fn a_relative_target_resolves_against_the_sessions_cwd() {
        let p = payload(json!({ "cwd": "/repo" }));
        let got = resolve_against(&p, "src/a.rs");
        // Compared as paths, not as strings: `join` writes the platform's
        // separator, so a hand-spelled `/repo/src/a.rs` matches on Unix and
        // not on Windows. `Path` equality is component-wise, and Windows
        // treats both separators as one.
        assert_eq!(
            std::path::Path::new(&got),
            std::path::Path::new("/repo").join("src/a.rs")
        );
    }
}
