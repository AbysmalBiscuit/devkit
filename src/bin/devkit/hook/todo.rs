//! The todo store's side of the hooks: who a payload acts as, and the
//! harness its session belongs to.

use anyhow::Result;
use devkit_command::Analysis;
use devkit_common::vcs::Checkout;
use devkit_todo::{
    BuiltinStore, Claimed, Edit, Filter, Holder, NewTodo, ORDER_GAP, StatusKind, TodoStore,
    diff::{Change, Step, diff, pair},
    native::NativeMap,
    node::{self, Harness, SessionRef},
};
use pabal::AnyHarness;
use serde_json::Value;

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

/// Mirrors the harness's own task and plan tools into the store. The native
/// tool has already run, so a failure, a claim conflict included, is reported
/// on stderr and skipped.
pub(crate) fn capture(payload: &Payload, checkout: &Checkout) {
    if let Err(e) = try_capture(payload, checkout) {
        eprintln!("devkit todo: {e:#}");
    }
}

/// What a native capture needs from a payload: which harness's ids it uses,
/// who acted, and the node its session writes to.
struct Capture {
    harness: Harness,
    session: Holder,
    actor: Holder,
    node: String,
    store: BuiltinStore,
    map: NativeMap,
}

fn try_capture(payload: &Payload, checkout: &Checkout) -> Result<()> {
    let tool = payload.tool_name();
    if !matches!(
        tool,
        Some("TaskCreate" | "TaskUpdate" | "update_plan" | "TodoWrite")
    ) {
        return Ok(());
    }
    let Some(harness) = harness_of(payload.harness()) else {
        return Ok(());
    };
    let (Some(session), Ok(actor)) = (payload.session_holder(), payload.holder()) else {
        return Ok(());
    };
    let place = node::place_of(checkout);
    let node = node::node(
        &place,
        Some(&SessionRef {
            harness,
            id: session.to_string(),
        }),
    );
    let c = Capture {
        harness,
        session: to_todo_holder(&session),
        actor: to_todo_holder(&actor),
        node,
        store: crate::todo::store(),
        map: NativeMap::at(BuiltinStore::default_dir()),
    };
    let raw = payload.raw();
    let input = &raw["tool_input"];
    match tool {
        Some("TaskCreate") => task_create(&c, input, &raw["tool_response"]),
        Some("TaskUpdate") => task_update(&c, input),
        Some("update_plan") => list_replace(&c, &input["plan"], "step"),
        Some("TodoWrite") => list_replace(&c, &input["todos"], "content"),
        _ => Ok(()),
    }
}

/// Claude Code's task list is shared by a session and its sub-agents, so the
/// mapping is recorded under the session for either to update it.
fn task_create(c: &Capture, input: &Value, response: &Value) -> Result<()> {
    let (Some(subject), Some(native)) =
        (input["subject"].as_str(), response["task"]["id"].as_str())
    else {
        return Ok(());
    };
    let id = c.store.add(NewTodo {
        project: Some(c.node.clone()),
        description: subject.to_string(),
        parent: None,
        order: None,
    })?;
    c.map.record(c.harness, &c.session, native, &id)
}

fn task_update(c: &Capture, input: &Value) -> Result<()> {
    let Some(native) = input["taskId"].as_str() else {
        return Ok(());
    };
    let Some(id) = c.map.lookup(c.harness, &c.actor, native)? else {
        return Ok(());
    };
    if let Some(subject) = input["subject"].as_str() {
        let current = c.store.list(&Filter::all())?;
        if current
            .iter()
            .any(|t| t.id == id && t.description != devkit_todo::one_line(subject))
        {
            c.store.apply(&Edit::Describe {
                id: id.clone(),
                description: subject.to_string(),
            })?;
        }
    }
    if let Some(to) = input["status"].as_str().and_then(native_status) {
        c.store.apply(&Edit::SetStatus {
            id,
            to,
            actor: c.actor.clone(),
        })?;
    }
    Ok(())
}

/// A tool that resends its whole list on every call, diffed against the
/// acting holder's previous list. Each agent context keeps its own list, so a
/// sub-agent's first call never cancels its session's steps.
fn list_replace(c: &Capture, list: &Value, text_key: &str) -> Result<()> {
    let Some(list) = list.as_array() else {
        return Ok(());
    };
    let next: Vec<Step> = list
        .iter()
        .filter_map(|item| {
            Some(Step {
                text: devkit_todo::one_line(item[text_key].as_str()?),
                status: item["status"].as_str().and_then(native_status)?,
            })
        })
        .collect();
    let todos = c.store.list(&Filter::all())?;
    let previous: Vec<(String, String, StatusKind)> = c
        .map
        .snapshot(c.harness, &c.actor)?
        .into_iter()
        .filter_map(|(text, id)| {
            let status = todos.iter().find(|t| t.id == id)?.status.kind();
            Some((text, id, status))
        })
        .collect();
    let mut ids: Vec<Option<String>> = pair(&previous, &next)
        .into_iter()
        .map(|p| p.map(|i| previous[i].1.clone()))
        .collect();
    let changes = diff(&previous, &next);
    let mut failed = None;
    let mut apply = |edit: Edit| match c.store.apply(&edit) {
        Ok(()) => true,
        Err(e) => {
            failed.get_or_insert(e);
            false
        }
    };
    // A step whose cancel was refused stays in the snapshot, so the next call
    // retries it instead of forgetting the todo while it is still open.
    let mut uncancelled = Vec::new();
    for change in changes {
        match change {
            Change::Add { index } => {
                let step = &next[index];
                let id = c.store.add(NewTodo {
                    project: Some(c.node.clone()),
                    description: step.text.clone(),
                    parent: None,
                    order: Some(index as i64 * ORDER_GAP),
                })?;
                if step.status != StatusKind::Pending {
                    apply(Edit::SetStatus {
                        id: id.clone(),
                        to: step.status,
                        actor: c.actor.clone(),
                    });
                }
                ids[index] = Some(id);
            }
            Change::Status { todo, to } => {
                apply(Edit::SetStatus {
                    id: todo,
                    to,
                    actor: c.actor.clone(),
                });
            }
            Change::Cancel { todo } => {
                let cancelled = apply(Edit::SetStatus {
                    id: todo.clone(),
                    to: StatusKind::Cancelled,
                    actor: c.actor.clone(),
                });
                if !cancelled
                    && let Some((text, ..)) = previous.iter().find(|(_, id, _)| *id == todo)
                {
                    uncancelled.push((text.clone(), todo));
                }
            }
            Change::Reorder { todo, order } => {
                apply(Edit::Reorder { id: todo, order });
            }
        }
    }
    let snapshot = next
        .into_iter()
        .zip(ids)
        .filter_map(|(step, id)| Some((step.text, id?)))
        .chain(uncancelled)
        .collect();
    c.map.set_snapshot(c.harness, &c.actor, snapshot)?;
    failed.map_or(Ok(()), Err)
}

fn native_status(status: &str) -> Option<StatusKind> {
    match status {
        "pending" => Some(StatusKind::Pending),
        "in_progress" => Some(StatusKind::InProgress),
        "completed" => Some(StatusKind::Completed),
        "deleted" => Some(StatusKind::Cancelled),
        _ => None,
    }
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
