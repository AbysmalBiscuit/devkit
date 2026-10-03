//! The todo store's side of the hooks.
//!
//! - Release: when a sub-agent or session ends, the todos it still has in
//!   progress return to pending.
//! - Shell-guard attribution: a sub-agent's `devkit todo` status changes are
//!   checked against other holders' claims, and on Claude Code its command is
//!   rewritten so the CLI acts as the sub-agent when the invocation runs.
//! - Native capture: the harness's own task and plan tools are mirrored into
//!   the store after they run.

use std::path::Path;

use anyhow::Result;
use devkit_command::{Analysis, Dialect, Invocation};
use devkit_common::vcs::Checkout;
use devkit_todo::{
    Claimed, Edit, Holder, NewTodo, ORDER_GAP, StatusKind, Todo, TodoStore,
    diff::{Change, Mirrored, Step, diff, pair},
    holder::HOLDER_VAR,
    native::{MirroredStep, NativeMap},
    node::{self, Harness, SessionRef},
    transition,
};
use pabal::AnyHarness;
use serde_json::Value;

use super::{
    payload::{self, Payload},
    record,
};
use crate::todo::store::Store;

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
pub(crate) fn release(holder: Option<payload::Holder>, checkout: &Checkout, cwd: &Path) {
    if let Some(holder) = holder {
        let holder = to_todo_holder(&holder);
        let _ = std::fs::remove_file(devkit_todo::digest_path(&holder));
        let _ = Store::for_hook(checkout, cwd).apply(&Edit::ReleaseAll { holder });
    }
}

/// The arguments after `todo` when `inv` runs `devkit todo`.
fn todo_args(inv: &Invocation) -> Option<impl Iterator<Item = Option<&str>>> {
    let program = inv.program.known().unwrap_or_default();
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    if name.strip_suffix(".exe").unwrap_or(name) != "devkit" {
        return None;
    }
    let mut args = inv.args.iter().map(|a| a.known());
    (args.next().flatten() == Some("todo")).then_some(args)
}

/// Every `devkit todo start|stop|done|undone|cancel <id>...` in a command,
/// as the status each asks for and the id it names. Ids the analysis could
/// not resolve are skipped.
pub(crate) fn status_edits(analysis: &Analysis) -> Vec<(StatusKind, String)> {
    let mut out = Vec::new();
    for mut args in analysis.invocations.iter().filter_map(todo_args) {
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

/// A block reason when a sub-agent's command would change a todo another
/// holder has in progress. Writes nothing: the command makes its own changes
/// when it runs, attributed by [`rewrite`]. A store failure blocks nothing.
pub(crate) fn check_claims(
    payload: &Payload,
    analysis: &Analysis,
    checkout: &Checkout,
    cwd: &Path,
) -> Option<String> {
    let actor = to_todo_holder(&payload.subagent_holder()?);
    let edits = status_edits(analysis);
    if edits.is_empty() {
        return None;
    }
    let store = Store::for_hook(checkout, cwd);
    edits.into_iter().find_map(|(to, id)| {
        let todo = store.get(&id).ok()??;
        let Claimed { by } = transition(&todo.status, to, &actor).err()?;
        Some(format!(
            "devkit todo: todo {id} is in progress by {by}; pick another todo"
        ))
    })
}

/// A sub-agent's command with each `devkit todo` invocation prefixed by the
/// sub-agent's holder, so the CLI acts as the sub-agent when, and only if, the
/// invocation runs. A sub-agent's shell carries its session's id, so only the
/// hook can tell the two apart.
///
/// `None` leaves the command alone and the CLI acts as the session. That is
/// the answer outside Bash, and on a harness whose handling of a rewritten
/// command is unverified.
pub(crate) fn rewrite(
    payload: &Payload,
    analysis: &Analysis,
    command: &str,
    dialect: Dialect,
) -> Option<String> {
    if payload.harness() != AnyHarness::ClaudeCode || dialect != Dialect::Bash {
        return None;
    }
    let holder = payload.subagent_holder()?;
    with_holder(command, analysis, &holder)
}

fn with_holder(command: &str, analysis: &Analysis, holder: &str) -> Option<String> {
    let mut starts: Vec<usize> = analysis
        .invocations
        .iter()
        .filter(|inv| todo_args(inv).is_some())
        .map(|inv| inv.location.outer.start)
        .filter(|&at| command.is_char_boundary(at))
        .collect();
    if starts.is_empty() {
        return None;
    }
    starts.sort_unstable();
    starts.dedup();
    let prefix = format!("{}='{}' ", HOLDER_VAR, holder.replace('\'', "'\\''"));
    let mut out = command.to_string();
    for at in starts.into_iter().rev() {
        out.insert_str(at, &prefix);
    }
    Some(out)
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
struct Capture<S: TodoStore> {
    harness: Harness,
    session: Holder,
    actor: Holder,
    node: String,
    store: S,
    map: NativeMap,
}

/// The harness tools capture mirrors.
#[derive(Clone, Copy)]
enum NativeTool {
    /// Claude Code's `TaskCreate`.
    TaskCreate,
    /// Claude Code's `TaskUpdate`.
    TaskUpdate,
    /// Codex's `update_plan`, which resends the whole plan.
    UpdatePlan,
    /// Claude Code's `TodoWrite`, which resends the whole list.
    TodoWrite,
}

impl NativeTool {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "TaskCreate" => Some(Self::TaskCreate),
            "TaskUpdate" => Some(Self::TaskUpdate),
            "update_plan" => Some(Self::UpdatePlan),
            "TodoWrite" => Some(Self::TodoWrite),
            _ => None,
        }
    }
}

fn try_capture(payload: &Payload, checkout: &Checkout) -> Result<()> {
    let Some(tool) = payload.tool_name().and_then(NativeTool::parse) else {
        return Ok(());
    };
    let Some(harness) = harness_of(payload.harness()) else {
        return Ok(());
    };
    let (Some(session), Ok(actor)) = (payload.session_holder(), payload.holder()) else {
        return Ok(());
    };
    // A checkout git could not read says nothing about where the todo lives.
    let Ok(place) = node::place_of(checkout) else {
        return Ok(());
    };
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
        store: Store::for_hook(checkout, &record::payload_cwd(payload)),
        map: NativeMap::at(devkit_todo::state_dir()),
    };
    let raw = payload.raw();
    let input = &raw["tool_input"];
    match tool {
        NativeTool::TaskCreate => task_create(&c, input, &raw["tool_response"]),
        NativeTool::TaskUpdate => task_update(&c, input),
        NativeTool::UpdatePlan => list_replace(&c, &input["plan"], "step"),
        NativeTool::TodoWrite => list_replace(&c, &input["todos"], "content"),
    }
}

/// Claude Code's task list is shared by a session and its sub-agents, so the
/// mapping is recorded under the session for either to update it.
fn task_create(c: &Capture<impl TodoStore>, input: &Value, response: &Value) -> Result<()> {
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

fn task_update(c: &Capture<impl TodoStore>, input: &Value) -> Result<()> {
    let Some(native) = input["taskId"].as_str() else {
        return Ok(());
    };
    let Some(id) = c.map.lookup(c.harness, &c.actor, native)? else {
        return Ok(());
    };
    if let Some(subject) = input["subject"].as_str() {
        let current = c.store.get(&id)?;
        if current.is_some_and(|t| t.description != devkit_todo::one_line(subject)) {
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
fn list_replace(c: &Capture<impl TodoStore>, list: &Value, text_key: &str) -> Result<()> {
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
    let snapshot = c.map.snapshot(c.harness, &c.actor)?;
    let mut todos: Vec<Todo> = Vec::new();
    for step in &snapshot {
        todos.extend(c.store.get(&step.todo)?);
    }
    let previous: Vec<Mirrored> = snapshot
        .into_iter()
        .filter_map(|MirroredStep { text, todo }| {
            let status = todos.iter().find(|t| t.id == todo)?.status.kind();
            Some(Mirrored { text, todo, status })
        })
        .collect();
    // The list follows its session, so open steps left on the node the
    // session last wrote to move to the one it writes to now.
    for prev in &previous {
        let open = matches!(prev.status, StatusKind::Pending | StatusKind::InProgress);
        let elsewhere = todos
            .iter()
            .any(|t| t.id == prev.todo && t.project.as_deref() != Some(c.node.as_str()));
        if open && elsewhere {
            c.store.apply(&Edit::Relocate {
                id: prev.todo.clone(),
                project: Some(c.node.clone()),
            })?;
        }
    }
    let mut ids: Vec<Option<String>> = pair(&previous, &next)
        .into_iter()
        .map(|p| p.map(|i| previous[i].todo.clone()))
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
                if !cancelled && let Some(prev) = previous.iter().find(|p| p.todo == todo) {
                    uncancelled.push(MirroredStep {
                        text: prev.text.clone(),
                        todo,
                    });
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
        .filter_map(|(step, id)| {
            Some(MirroredStep {
                text: step.text,
                todo: id?,
            })
        })
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

    fn prefixed(command: &str) -> Option<String> {
        let ctx = Context {
            dialect: Dialect::Bash,
            cwd: Some("/repo".into()),
            home: None,
            path_style: PathStyle::Unix,
            limits: Limits::default(),
        };
        with_holder(command, &devkit_command::analyze(command, &ctx), "S/a1")
    }

    #[test]
    fn each_todo_invocation_carries_the_holder() {
        assert_eq!(
            prefixed("false && devkit todo done 1; devkit todo list | cat").as_deref(),
            Some(
                "false && DEVKIT_TODO_HOLDER='S/a1' devkit todo done 1; \
                 DEVKIT_TODO_HOLDER='S/a1' devkit todo list | cat"
            )
        );
        assert_eq!(
            prefixed("bash -c 'devkit todo start 2'").as_deref(),
            Some("DEVKIT_TODO_HOLDER='S/a1' bash -c 'devkit todo start 2'")
        );
        assert_eq!(prefixed("devkit locks list"), None);
    }

    #[test]
    fn ignores_other_devkit_commands() {
        assert!(edits("devkit todo add x; devkit locks list").is_empty());
        assert!(edits("echo devkit todo start 3").is_empty());
    }
}
