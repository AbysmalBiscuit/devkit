//! The todo store's side of the hooks.
//!
//! - Release: when a sub-agent or session ends, the todos it still has in
//!   progress return to pending.
//! - Shell-guard attribution: a sub-agent's `devkit todo` status changes are
//!   checked against other holders' claims, and on a harness whose shell
//!   carries a session id the todo CLI reads, its command is rewritten so the
//!   CLI acts as the sub-agent when the invocation runs.
//! - Native capture: the harness's own task and plan tools are mirrored into
//!   the store after they run.
//! - Hold: a stop is refused, once per unchanged list, while the agent has open
//!   todos.

use std::{path::Path, time::Duration};

use anyhow::Result;
use devkit_command::{Analysis, Dialect, Invocation};
use devkit_common::{store::LockBusy, ui::printable, vcs::Checkout};
use devkit_todo::{
    Claimed, Edit, Filter, Holder, NewTodo, ORDER_GAP, StatusKind, Todo, TodoStore,
    diff::{Change, Mirrored, Step, diff, pair},
    hold,
    holder::HOLDER_VAR,
    native::{MirroredStep, NativeMap},
    node::{self, Harness, Place, SessionRef},
    render, transition,
};
use pabal::AnyHarness;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    payload::{self, Payload},
    record,
};
use crate::todo::{
    queue::{self, Deferred},
    store::{BACKEND_VAR, Store},
};

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
/// lists last injected for it. The release is local and the sync that pushes
/// it runs detached, so no harness's cap on a session-end hook cuts it short.
/// Silent: a store failure leaves the claims for a person to reset.
pub(crate) fn release(holder: Option<payload::Holder>, checkout: &Checkout, cwd: &Path) {
    if let Some(holder) = holder {
        let holder = to_todo_holder(&holder);
        let _ = std::fs::remove_file(devkit_todo::digest_path(&holder));
        let store = Store::for_hook(checkout, cwd);
        let entry = |root: &str| Deferred::Release {
            root: root.to_string(),
            holder: holder.clone(),
        };
        let direct = || {
            store
                .apply(&Edit::ReleaseAll {
                    holder: holder.clone(),
                })
                .map(drop)
        };
        if record_write(&store, entry, direct).is_ok() {
            store.spawn_sync(cwd);
        }
    }
}

/// What follows the open todos in a hold's reason.
const HOLD_ADVICE: &str = "Finish each one, or cancel one that no longer applies \
(`devkit todo cancel <id>`, or delete it in your task tool).
Before you stop to ask the user something:
- With a clear recommendation, take it and say so in your final report.
- Without one, ask a sub-agent on a bigger model and take its answer.
- Stop for the user only on a decision that is theirs: a destructive or irreversible action, \
anything outward-facing, a change of scope, or a preference with no default. To stop for one, \
end your turn again: this reminder comes once per unchanged list.";

/// The answer that refuses `holder`'s stop, or `None` to let it stop. A
/// stop is refused while `holder` has open todos (see [`hold::open_for`])
/// and was not already refused over the same list; a session's pending
/// todos count only on its own workspace node. Fails open: a harness whose
/// sessions name no node, a payload that cannot block, an unreadable config
/// or store, and a fingerprint that cannot be written all let it stop.
pub(crate) fn hold(
    payload: &Payload,
    holder: &payload::Holder,
    checkout: &Checkout,
    cwd: &Path,
) -> Option<String> {
    let harness = harness_of(payload.harness())?;
    let backend_var = std::env::var(BACKEND_VAR).ok();
    let store = Store::for_hold(checkout, cwd, backend_var.as_deref())?;
    if let Some(replica) = store.queued_replica() {
        drain(replica.data_dir());
    }
    let holder = to_todo_holder(holder);
    let session = holder.session();
    let place = node::place_of(checkout).ok()?;
    let pending_node = match place {
        Place::Workspace { .. } if holder == session => Some(node::node(
            &place,
            Some(&SessionRef {
                harness,
                id: session.to_string(),
            }),
        )),
        _ => None,
    };
    let todos = store.list(&Filter::all()).ok()?;
    let open = hold::open_for(&todos, &holder, pending_node.as_deref());
    if open.is_empty() {
        return None;
    }
    let fingerprint = hold::fingerprint(&open);
    let path = hold::hold_path(&holder);
    if std::fs::read_to_string(&path).is_ok_and(|seen| seen == fingerprint) {
        return None;
    }
    let mut nodes: Vec<String> = open.iter().map(|t| t.node().to_string()).collect();
    nodes.dedup();
    let open: Vec<Todo> = open.into_iter().cloned().collect();
    let lists = render::render_lists(&nodes, &open, &holder);
    let reason = format!(
        "devkit todo: you have open todos.\n\n{}\n\n{HOLD_ADVICE}",
        lists.trim_end()
    );
    let answer = payload.block(&reason)?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, fingerprint).ok()?;
    Some(answer)
}

/// Forgets the list `holder` was last refused over, so its next stop with
/// open todos is refused again.
pub(crate) fn rearm(holder: &payload::Holder) {
    let _ = std::fs::remove_file(hold::hold_path(&to_todo_holder(holder)));
}

/// Forgets every list `session` and its sub-agents were refused over.
pub(crate) fn forget_holds(session: &payload::Holder) {
    let _ = std::fs::remove_dir_all(hold::hold_dir(&to_todo_holder(session)));
}

/// Records one hook write: on a replica with a queue, appends `entry` (given
/// the replica's root) and applies the queue; on any other store, makes the
/// write with `direct`. `Ok` once the write is applied or safely queued.
fn record_write(
    store: &Store,
    entry: impl FnOnce(&str) -> Deferred,
    direct: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let Some(replica) = store.queued_replica() else {
        return direct();
    };
    let dir = replica.data_dir();
    let queued = queue::push(dir, &entry(replica.root()));
    drain(dir);
    queued
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
/// when it runs, attributed by [`rewrite`]. A store failure, or a
/// store with no answer within [`CLAIM_CHECK_WAIT`], blocks nothing.
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
    let (checkout, cwd) = (checkout.clone(), cwd.to_path_buf());
    let check = move || {
        let store = Store::for_hook(&checkout, &cwd);
        edits.into_iter().find_map(|(to, id)| {
            let todo = store.get(&id).ok()??;
            let Claimed { by } = transition(&todo.status, to, &actor).err()?;
            Some(format!(
                "devkit todo: todo {} is in progress by {}; pick another todo",
                printable(&id),
                printable(&by)
            ))
        })
    };
    // A store that never answers is a store that failed: the check blocks
    // nothing rather than hold the hook until its harness lets the call
    // through anyway.
    super::gate::with_deadline(super::within_deadline(CLAIM_CHECK_WAIT), check)
        .ok()
        .flatten()
}

/// How long the claim check waits for the store, setup included, before it
/// blocks nothing, when the hook's overall deadline leaves that long.
const CLAIM_CHECK_WAIT: Duration = Duration::from_secs(1);

/// A sub-agent's command with each `devkit todo` invocation prefixed by the
/// sub-agent's holder, so the CLI acts as the sub-agent when, and only if, the
/// invocation runs. A sub-agent's shell carries its session's id, so only the
/// hook can tell the two apart.
///
/// `None` leaves the command alone, and the CLI acts as the session. That is
/// the answer outside Bash, for a payload with no sub-agent holder, on a
/// harness whose shell carries no session id the todo CLI reads, and for a
/// command with no `devkit todo` invocation.
pub(crate) fn rewrite(
    payload: &Payload,
    analysis: &Analysis,
    command: &str,
    dialect: Dialect,
) -> Option<String> {
    if dialect != Dialect::Bash || harness_of(payload.harness()).is_none() {
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
/// on stderr and skipped. A store whose lock stays busy past a hook's wait
/// keeps the write queued, and the next write or sync applies it.
pub(crate) fn capture(payload: &Payload, checkout: &Checkout) {
    let Some(mirror) = Mirror::of(payload, checkout) else {
        return;
    };
    let cwd = record::payload_cwd(payload);
    let store = Store::for_hook(checkout, &cwd);
    let entry = |root: &str| Deferred::Capture {
        root: root.to_string(),
        mirror: mirror.clone(),
    };
    report(record_write(&store, entry, || mirror.apply(&store)));
    // A capture can fail after some of its writes committed.
    store.spawn_sync(&cwd);
}

fn report(result: Result<()>) {
    if let Err(e) = result
        && e.downcast_ref::<LockBusy>().is_none()
    {
        eprintln!("devkit todo: {e:#}");
    }
}

/// Applies the hook writes queued beside the replica in `dir`, oldest first,
/// and returns how many it applied. One that finds the replica lock still
/// busy stays queued for the next write or sync.
pub(crate) fn drain(dir: &Path) -> usize {
    queue::drain(dir, |entry| apply_deferred(dir, entry), |e| report(Err(e)))
}

/// Applies a queued write to the replica at `dir`, under the root and node it
/// was resolved with: the checkout may have changed branch or gone since.
fn apply_deferred(dir: &Path, entry: &Deferred) -> Result<()> {
    match entry {
        Deferred::Capture { root, mirror } => mirror.apply(&Store::queued_at(dir, root)),
        Deferred::Release { root, holder } => Store::queued_at(dir, root)
            .apply(&Edit::ReleaseAll {
                holder: holder.clone(),
            })
            .map(drop),
    }
}

/// A native tool call as a hook resolved it: the tool, whose ids it uses,
/// who acted, and the node its session writes to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Mirror {
    tool: String,
    /// The harness's node prefix, `claude` or `codex`.
    harness: String,
    session: Holder,
    actor: Holder,
    node: String,
    input: Value,
    response: Value,
}

impl Mirror {
    /// `None` for a call capture does not mirror: another tool, a harness
    /// without session nodes, no session, or a checkout git cannot read.
    fn of(payload: &Payload, checkout: &Checkout) -> Option<Self> {
        let tool = payload
            .tool_name()
            .filter(|t| NativeTool::parse(t).is_some())?;
        let harness = harness_of(payload.harness())?;
        let (Some(session), Ok(actor)) = (payload.session_holder(), payload.holder()) else {
            return None;
        };
        let place = node::place_of(checkout).ok()?;
        let node = node::node(
            &place,
            Some(&SessionRef {
                harness,
                id: session.to_string(),
            }),
        );
        let raw = payload.raw();
        Some(Self {
            tool: tool.to_string(),
            harness: harness.prefix().to_string(),
            session: to_todo_holder(&session),
            actor: to_todo_holder(&actor),
            node,
            input: raw["tool_input"].clone(),
            response: raw["tool_response"].clone(),
        })
    }

    /// Mirrors the call into `store` under one hold of its lock: a queued
    /// capture that found the lock busy partway would otherwise replay the
    /// writes that had landed.
    fn apply(&self, store: &Store) -> Result<()> {
        let Some(tool) = NativeTool::parse(&self.tool) else {
            return Ok(());
        };
        let harness = match self.harness.as_str() {
            "claude" => Harness::Claude,
            "codex" => Harness::Codex,
            other => anyhow::bail!("queued capture names an unknown harness {other:?}"),
        };
        let c = Capture {
            harness,
            session: self.session.clone(),
            actor: self.actor.clone(),
            node: self.node.clone(),
            store,
            map: NativeMap::at(devkit_todo::state_dir()),
        };
        let input = &self.input;
        store.while_locked(|| match tool {
            NativeTool::TaskCreate => task_create(&c, input, &self.response),
            NativeTool::TaskUpdate => task_update(&c, input),
            NativeTool::UpdatePlan => list_replace(&c, &input["plan"], "step"),
            NativeTool::TodoWrite => list_replace(&c, &input["todos"], "content"),
        })
    }
}

/// What mirroring a native call needs: whose ids it uses, who acted, the
/// node its session writes to, and the store.
struct Capture<'a> {
    harness: Harness,
    session: Holder,
    actor: Holder,
    node: String,
    store: &'a Store,
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

/// Claude Code's task list is shared by a session and its sub-agents, so the
/// mapping is recorded under the session for either to update it.
fn task_create(c: &Capture<'_>, input: &Value, response: &Value) -> Result<()> {
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

fn task_update(c: &Capture<'_>, input: &Value) -> Result<()> {
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
fn list_replace(c: &Capture<'_>, list: &Value, text_key: &str) -> Result<()> {
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
        Ok(_) => true,
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
