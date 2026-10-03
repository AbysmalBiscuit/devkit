//! The todo store's side of the hooks: who a payload acts as, and the
//! harness its session belongs to.

use devkit_command::Analysis;
use devkit_todo::{Claimed, Edit, Holder, StatusKind, TodoStore, node::Harness};
use pabal::AnyHarness;

use super::payload::{self, Payload};

/// The harnesses whose sessions name a todo node.
pub(crate) fn harness_of(harness: AnyHarness) -> Option<Harness> {
    match harness {
        AnyHarness::ClaudeCode => Some(Harness::Claude),
        AnyHarness::Codex => Some(Harness::Codex),
        AnyHarness::Cursor | AnyHarness::Antigravity => None,
    }
}

pub(crate) fn to_todo_holder(holder: &payload::Holder) -> Holder {
    Holder::new(&**holder)
}

/// Returns every todo `holder` covers from in progress to pending, so a
/// crashed agent's todos do not show as in progress forever, and forgets the
/// lists last injected for it. Silent: a store failure leaves the claims for
/// a person to reset.
pub(crate) fn release(holder: Option<payload::Holder>) {
    if let Some(holder) = holder {
        let holder = to_todo_holder(&holder);
        let _ = std::fs::remove_file(crate::todo::digest_path(&holder));
        let _ = crate::todo::store().apply(&Edit::ReleaseAll { holder });
    }
}

/// Every `devkit todo start|stop|done|undone|cancel <id>...` in a command,
/// as the status each asks for and the id it names. Ids the analysis could
/// not resolve are skipped.
pub(crate) fn status_edits(analysis: &Analysis) -> Vec<(StatusKind, String)> {
    let mut out = Vec::new();
    for inv in &analysis.invocations {
        let program = inv.program.known().unwrap_or_default();
        let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
        if name.strip_suffix(".exe").unwrap_or(name) != "devkit" {
            continue;
        }
        let mut args = inv.args.iter().map(|a| a.known());
        if args.next().flatten() != Some("todo") {
            continue;
        }
        let kind = match args.next().flatten() {
            Some("start") => StatusKind::InProgress,
            Some("stop" | "undone") => StatusKind::Pending,
            Some("done") => StatusKind::Completed,
            Some("cancel") => StatusKind::Cancelled,
            _ => continue,
        };
        out.extend(
            args.flatten()
                .filter(|id| !id.starts_with('-'))
                .map(|id| (kind, id.to_string())),
        );
    }
    out
}

/// Applies a sub-agent's `devkit todo` status changes as the sub-agent before
/// its command runs. The command itself runs as the session, which covers the
/// sub-agent, so the claim recorded here stays. A sub-agent's shell carries
/// its session's id, so only the hook can tell them apart.
///
/// Returns a block reason when another holder has a named todo in progress.
/// Any other store failure lets the command run, and its change lands as the
/// session.
pub(crate) fn attribute(payload: &Payload, analysis: &Analysis) -> Option<String> {
    let actor = to_todo_holder(&payload.subagent_holder()?);
    let store = crate::todo::store();
    for (to, id) in status_edits(analysis) {
        let edit = Edit::SetStatus {
            id: id.clone(),
            to,
            actor: actor.clone(),
        };
        if let Err(e) = store.apply(&edit)
            && let Some(Claimed { by }) = e.downcast_ref::<Claimed>()
        {
            return Some(format!(
                "devkit todo: todo {id} is in progress by {by}; pick another todo"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use devkit_command::{Context, Dialect, Limits, PathStyle};
    use devkit_todo::StatusKind;

    use super::*;

    fn edits(command: &str) -> Vec<(StatusKind, String)> {
        let ctx = Context {
            dialect: Dialect::Bash,
            cwd: Some("/repo".into()),
            home: None,
            path_style: PathStyle::Unix,
            limits: Limits::default(),
        };
        status_edits(&devkit_command::analyze(command, &ctx))
    }

    #[test]
    fn finds_status_verbs_and_ids() {
        assert_eq!(edits("devkit todo start 3 && devkit todo done 4 5"), [
            (StatusKind::InProgress, "3".to_string()),
            (StatusKind::Completed, "4".to_string()),
            (StatusKind::Completed, "5".to_string()),
        ]);
        assert_eq!(edits("/usr/local/bin/devkit todo cancel 7"), [(
            StatusKind::Cancelled,
            "7".to_string()
        )]);
    }

    #[test]
    fn ignores_other_devkit_commands() {
        assert!(edits("devkit todo add x; devkit locks list").is_empty());
        assert!(edits("echo devkit todo start 3").is_empty());
    }
}
