//! The edit path of `devkit hook pre-tool-use`, and the two release verbs.
//!
//! A structured edit names its targets in the payload, so there is nothing to
//! analyse: the targets are claimed as they stand. Fails closed, the same as
//! the shell path's write stage — a payload this cannot evaluate denies rather
//! than allows, because an unevaluable write must not open the window.
//!
//! The release verbs carry no permission decision at all. Nothing they can
//! answer would be read.

use anyhow::Result;
use devkit_common::harness_log::{self, Decision, EditPre, Kind, Verdict};
use devkit_locks::{
    hook::{self, LockAction},
    model::{Conflict, WriteDecision},
};
use serde_json::Value;

use super::{HookEvent, record};

/// Claim the write targets a structured-edit payload names, before the tool
/// runs.
pub fn guard(payload: &Value) -> Result<()> {
    let cwd = cwd_of(payload);
    let (targets, blocks) = match hook::parse_write(payload) {
        Some(LockAction::Write {
            file_paths, holder, ..
        }) => {
            let blocks = claim(payload, &cwd, &file_paths, &holder);
            (file_paths, blocks)
        }
        Some(LockAction::Unusable { reason }) => {
            let message = format!("devkit write-harness: {reason} (fail-closed)");
            if hook::enforcement_enabled(&cwd) {
                println!("{}", hook::deny_json(&message));
            }
            (Vec::new(), vec![message])
        }
        // A tool that does not write, which is the common case. The release
        // variants never come back from `parse_write`.
        Some(LockAction::ReleaseSubagent { .. } | LockAction::ReleaseSession { .. }) | None => {
            return Ok(());
        }
    };
    // Envelope first, then the record: a stall on the log directory before the
    // envelope exists runs into the manifest timeout, and a harness timeout
    // allows the call.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let settings = harness_log::resolve(&cwd);
    if settings.enabled {
        let rec = record::envelope(
            payload,
            HookEvent::PreToolUse,
            None,
            Kind::EditPre(EditPre {
                tool_name: payload
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
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

/// Claim each target, returning the messages that denied the write. An empty
/// return is an allow.
fn claim(
    payload: &Value,
    cwd: &std::path::Path,
    file_paths: &[String],
    holder: &str,
) -> Vec<String> {
    if !hook::enforcement_enabled(cwd) {
        return Vec::new(); // no opt-in (env, project layers, or global config) → no enforcement
    }
    let mut conflicts = Vec::new();
    let mut resolver = devkit_locks::WriteResolver::new();
    for path in file_paths {
        let target = resolve_against(payload, path);
        match resolver.decide_write(&target, holder, Some("write-harness"), 1800) {
            Ok(WriteDecision::Denied(c)) => conflicts.extend(c),
            Ok(_) => {}
            Err(e) => {
                // fail closed: a registry error must not silently reopen the
                // window
                let message = format!("devkit write-harness: registry error (fail-closed): {e:#}");
                println!("{}", hook::deny_json(&message));
                return vec![message];
            }
        }
    }
    if conflicts.is_empty() {
        return Vec::new();
    }
    let envelope = conflict_envelope(&conflicts);
    println!("{envelope}");
    vec![
        envelope["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    ]
}

pub fn release_subagent(payload: &Value) -> Result<()> {
    release(hook::parse_subagent_stop(payload));
    Ok(())
}

pub fn release_session(payload: &Value) -> Result<()> {
    release(hook::parse_session_end(payload));
    Ok(())
}

fn release(action: Option<LockAction>) {
    if let Some(LockAction::ReleaseSubagent { holder } | LockAction::ReleaseSession { holder }) =
        action
    {
        let _ = devkit_locks::release_prefix(&holder);
    }
}

/// The deny envelope naming every holder in the way. `Acquired` and
/// `AllowedByOwnership` emit nothing: an allow is silence.
fn conflict_envelope(conflicts: &[Conflict]) -> serde_json::Value {
    let who = conflicts
        .iter()
        .map(|c| format!("{} (held by {})", c.path, c.held_by))
        .collect::<Vec<_>>()
        .join(", ");
    hook::deny_json(&format!(
        "devkit write-harness: {who} — locked by another agent; \
         coordinate or wait for it to finish"
    ))
}

/// Where the write would land. Paths in the payload are relative to the
/// session, not to wherever the harness spawned this process.
fn cwd_of(payload: &Value) -> std::path::PathBuf {
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// Resolve a payload path against the session's own working directory, so a
/// relative target names the file the session meant rather than one relative to
/// wherever the harness spawned the hook process.
fn resolve_against(payload: &Value, path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(|cwd| {
            std::path::Path::new(cwd)
                .join(p)
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_denied_decision_names_the_path_and_its_holder() {
        let out = conflict_envelope(&[Conflict {
            path: "src/a.rs".into(),
            held_by: "S/b2".into(),
            age_secs: 5,
            note: None,
        }]);
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains("S/b2"), "reason names the holder: {reason}");
        assert!(
            reason.contains("src/a.rs"),
            "reason names the path: {reason}"
        );
    }

    #[test]
    fn an_absolute_target_is_left_alone() {
        let p = serde_json::json!({ "cwd": "/repo" });
        assert_eq!(resolve_against(&p, "/tmp/a.rs"), "/tmp/a.rs");
    }

    #[test]
    fn a_relative_target_resolves_against_the_sessions_cwd() {
        let p = serde_json::json!({ "cwd": "/repo" });
        let got = resolve_against(&p, "src/a.rs");
        assert_ne!(got, "src/a.rs", "a relative target is not left as it came");
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
