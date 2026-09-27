//! The edit path of `devkit hook pre-tool-use`, and the two release verbs.
//!
//! A structured edit names its targets in the payload, so there is nothing to
//! analyse: the write gate claims the targets as they stand, and fails closed
//! the way it does for a shell write. A payload this cannot evaluate denies
//! rather than allows, because an unevaluable write must not open the window.
//!
//! The release verbs carry no permission decision at all. Nothing they can
//! answer would be read.

use std::path::Path;

use devkit_common::{
    harness_log::{self, Decision, EditPre, Kind, Verdict},
    vcs::Checkout,
};
use devkit_config::PolicyAction;
use devkit_locks::Registry;
use pabal::Tool;

use super::{
    HookEvent,
    gate::{self, Armed, Claims, WriteGate},
    payload::{self, Holder, Payload},
    print_envelope, record, rules,
};

/// Tools pabal reads write targets from, the last three being Antigravity's.
/// One that arrives without a readable target is a harness format change, so
/// it is a write to deny rather than a tool to ignore.
const WRITE_TOOLS: [&str; 8] = [
    "Edit",
    "MultiEdit",
    "Write",
    "NotebookEdit",
    "apply_patch",
    "write_to_file",
    "replace_file_content",
    "multi_replace_file_content",
];

/// What a `PreToolUse` call asks to write.
pub enum Write {
    Targets {
        paths: Vec<String>,
        holder: Holder,
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
    let holder = match payload.holder() {
        Ok(holder) => holder,
        Err(missing) => return Some(Write::Unusable(missing.to_string())),
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
        holder,
    })
}

/// Claim the write targets a structured-edit payload names, before the tool
/// runs. A panic once the gate applies denies the call.
pub fn guard(payload: &Payload, write: Write) -> anyhow::Result<()> {
    let _ = gate::guarded(|armed| respond(payload, write, &WriteGate::live(), armed));
    Ok(())
}

/// The edit path's single emission site. Anything appended to stdout after a
/// denial makes the whole output unparseable, and a harness that cannot parse
/// a hook's stdout proceeds with the call it was asked to gate.
fn respond(payload: &Payload, write: Write, gate: &WriteGate, armed: &Armed) {
    let cwd = record::payload_cwd(payload);
    // One `git worktree list` for the whole invocation, shared by the
    // enforcement gate, the lock scoping and the log settings. It resolves
    // lazily, so a tool that writes nothing spawns nothing.
    let checkout = Checkout::at(&cwd);
    let harness = payload.harness();
    let enabled = gate::enabled(harness, &checkout, &cwd);
    if enabled {
        armed.arm(harness);
    }
    let blocks = if enabled {
        blocks(&write, gate, &checkout, &cwd)
    } else {
        Vec::new()
    };
    let targets = match write {
        Write::Targets { paths, holder } => {
            if blocks.is_empty() {
                rules::inject(payload, &checkout, &cwd, &paths, &holder);
            }
            paths
        }
        Write::Unusable(_) => Vec::new(),
    };
    if !blocks.is_empty() {
        print_envelope(&payload::deny(harness, &blocks.join("\n")));
    }
    armed.disarm();
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
}

/// Why the gate refuses `write`, or nothing when it may proceed.
fn blocks<R>(write: &Write, gate: &WriteGate<R>, checkout: &Checkout, cwd: &Path) -> Vec<String>
where
    R: Registry + Send + Sync + 'static,
{
    match write {
        Write::Targets { paths, holder } => {
            // A structured edit names no tree, so the unresolved-write policy
            // has nothing to decide.
            gate.decide(
                &Claims::paths(paths.clone()),
                holder,
                checkout,
                cwd,
                PolicyAction::Block,
            )
            .blocks
        }
        Write::Unusable(reason) => vec![format!("{} {reason} (fail-closed)", gate::PREFIX)],
    }
}

pub fn release_subagent(payload: &Payload) {
    // Releasing the bare session holder here would free the parent's and every
    // sibling's locks, so an unattributable stop, a fork's included, releases
    // nothing.
    if let Some(holder) = payload.subagent_holder() {
        release(&holder);
    }
}

pub fn release_session(payload: &Payload) {
    if let Some(holder) = payload.session_holder() {
        release(&holder);
    }
}

fn release(holder: &str) {
    if devkit_locks::release_prefix(holder).is_ok() {
        rules::clear_for_holder(holder);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{super::gate::fixture::Project, *};

    fn payload(raw: serde_json::Value) -> Payload {
        Payload::new(None, HookEvent::PreToolUse, raw).unwrap()
    }

    fn targets(raw: serde_json::Value) -> (Vec<String>, String) {
        match write(&payload(raw)) {
            Some(Write::Targets { paths, holder }) => (paths, holder.to_string()),
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

    fn write_to(path: &str, session: &str) -> Write {
        Write::Targets {
            paths: vec![path.to_string()],
            holder: payload(json!({ "session_id": session })).holder().unwrap(),
        }
    }

    /// A registry whose every call runs `fail`, which never returns.
    struct Broken(fn() -> !);

    fn stall() -> ! {
        loop {
            std::thread::park();
        }
    }

    fn explode() -> ! {
        panic!("the registry panicked")
    }

    impl Registry for Broken {
        fn write_decide(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<&str>,
            _: u64,
        ) -> anyhow::Result<devkit_locks::model::WriteDecision> {
            (self.0)()
        }

        fn check_scope(
            &self,
            _: &str,
            _: &str,
            _: &[String],
        ) -> anyhow::Result<Vec<devkit_locks::model::Conflict>> {
            (self.0)()
        }

        fn check_covering(
            &self,
            _: &str,
            _: &str,
            _: &[String],
        ) -> anyhow::Result<Vec<devkit_locks::model::Conflict>> {
            (self.0)()
        }
    }

    #[test]
    fn a_stalled_registry_denies_the_edit() {
        let p = Project::new();
        let gate = WriteGate::new(Broken(stall), std::time::Duration::from_millis(100));
        let blocks = blocks(&write_to("a.rs", "S"), &gate, &p.checkout(), p.path());
        assert!(
            blocks.iter().any(|b| b.contains("did not answer")),
            "{blocks:?}"
        );
    }

    #[test]
    fn a_panicking_registry_denies_the_edit() {
        let p = Project::new();
        let gate = WriteGate::new(Broken(explode), std::time::Duration::from_secs(10));
        let blocks = blocks(&write_to("a.rs", "S"), &gate, &p.checkout(), p.path());
        assert!(
            blocks.iter().any(|b| b.contains("internal failure")),
            "{blocks:?}"
        );
    }

    #[test]
    fn a_relative_target_is_claimed_under_the_sessions_cwd() {
        let p = Project::new();
        let blocks = blocks(
            &write_to("src/a.rs", "S"),
            &p.gate(),
            &p.checkout(),
            p.path(),
        );
        assert!(blocks.is_empty(), "{blocks:?}");
        assert_eq!(p.rows(), [("src/a.rs".to_string(), "S".to_string())]);
    }

    #[test]
    fn another_sessions_claim_denies_the_edit_and_names_it() {
        let p = Project::new();
        p.hold("T", "src/a.rs");
        let blocks = blocks(
            &write_to("src/a.rs", "S"),
            &p.gate(),
            &p.checkout(),
            p.path(),
        );
        assert!(
            blocks.iter().any(|b| b.contains("src/a.rs (held by T)")),
            "{blocks:?}"
        );
    }

    #[test]
    fn an_unusable_write_is_denied_by_the_gate() {
        let p = Project::new();
        let blocks = blocks(
            &Write::Unusable("write payload names no target".into()),
            &p.gate(),
            &p.checkout(),
            p.path(),
        );
        assert!(blocks[0].contains("fail-closed"), "{blocks:?}");
        assert!(p.rows().is_empty());
    }
}
