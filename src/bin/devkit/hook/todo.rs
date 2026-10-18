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

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use devkit_command::{Analysis, Dialect, Invocation};
use devkit_common::{
    shim::{OwnBinary, Shim},
    store::LockBusy,
    ui::printable,
    vcs::Checkout,
};
use devkit_ports::guard::runs_own;
use devkit_todo::{
    Claimed, Edit, Filter, Holder, NewTodo, ORDER_GAP, StatusKind, Todo, TodoStore,
    diff::{Change, Mirrored, Step, diff, pair},
    graph::{self, Graph},
    hold,
    holder::HOLDER_VAR,
    layout::{Facts, Layout},
    native::{MirroredStep, NativeMap},
    node::{self, GLOBAL, Harness, Place, SessionRef},
    render,
    roles::{ResolvedRole, Roles},
    transition,
};
use pabal::AnyHarness;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::payload::{self, Payload};
use crate::todo::{
    anchor, global_notice,
    hold::{self as hold_mode, HOLD_VAR},
    queue::{self, Deferred},
    store::{BACKEND_VAR, Store},
    tickets,
};

/// The harnesses whose sessions name a todo node.
pub(crate) fn harness_of(harness: AnyHarness) -> Option<Harness> {
    match harness {
        AnyHarness::ClaudeCode => Some(Harness::Claude),
        AnyHarness::Codex => Some(Harness::Codex),
        AnyHarness::Grok => Some(Harness::Grok),
        AnyHarness::Cursor | AnyHarness::Antigravity => None,
    }
}

pub(crate) fn to_todo_holder(holder: &payload::Holder) -> Holder {
    Holder::new(&**holder)
}

pub(crate) fn spawn(payload: &Payload, checkout: &Checkout, cwd: &Path) {
    let Some(holder) = payload.subagent_holder() else {
        return;
    };
    let (Ok(store), Ok(state)) = (Store::for_hook(checkout, cwd), devkit_todo::state_dir()) else {
        return;
    };
    let in_repo = node::place_of(checkout).is_ok_and(|place| place != Place::Global);
    report(Layout::new(store.config()).and_then(|layout| {
        Roles::at(state).spawn(
            &layout,
            &to_todo_holder(&holder),
            payload.agent_type(),
            in_repo.then_some(checkout.dir()),
        )
    }));
}

pub(crate) fn resolve_role(
    payload: &Payload,
    checkout: &Checkout,
    store: &Store,
    holder: &Holder,
) -> Option<(Layout, Facts, ResolvedRole)> {
    let session = SessionRef {
        harness: harness_of(payload.harness())?,
        id: payload.session_id()?.to_string(),
    };
    let layout = Layout::new(store.config()).ok()?;
    let facts = Facts::new(&node::place_of(checkout).ok()?, Some(&session), holder);
    let role = Roles::at(devkit_todo::state_dir().ok()?)
        .resolve(&layout, &facts, holder, payload.agent_type())
        .ok()?;
    if let Some(warning) = &role.warning {
        eprintln!("devkit todo: {warning}");
    }
    Some((layout, facts, role))
}

/// The checkout a hook from `holder` resolves its todo place and config
/// from, as [`anchor`] picks it; `checkout` when the role record cannot be
/// read.
pub(crate) fn anchored(checkout: &Checkout, holder: &Holder) -> Checkout {
    devkit_todo::state_dir()
        .and_then(|dir| anchor(checkout.clone(), &Roles::at(dir), holder))
        .unwrap_or_else(|_| checkout.clone())
}

/// The checkout a hook with `payload` from `cwd` works in: `cwd`'s own, or,
/// when `cwd` is outside any repository, the one the payload's holder
/// recorded with its role, so its todo work and its activity read the same
/// store.
pub(crate) fn anchored_at(payload: &Payload, cwd: &Path) -> Checkout {
    let here = Checkout::at(cwd);
    match payload.holder() {
        Ok(holder) => anchored(&here, &to_todo_holder(&holder)),
        Err(_) => here,
    }
}

pub(crate) fn nudge(payload: &Payload, checkout: &Checkout) -> Option<String> {
    let holder = to_todo_holder(&payload.holder().ok()?);
    let checkout = anchored(checkout, &holder);
    let store = Store::for_hook(&checkout, checkout.dir()).ok()?;
    let (layout, facts, _) = resolve_role(payload, &checkout, &store, &holder)?;
    if facts.subagent
        && payload.agent_type().is_some_and(|agent_type| {
            layout
                .roles()
                .values()
                .any(|role| role.agent_types.iter().any(|name| name == agent_type))
        })
    {
        return None;
    }
    Roles::at(devkit_todo::state_dir().ok()?)
        .nudge(&layout, &facts, &holder)
        .ok()
        .flatten()
}

/// Returns every todo `holder` covers from in progress to pending, so a
/// crashed agent's todos do not show as in progress forever, and forgets the
/// lists last injected for it. The release is local and the sync that pushes
/// it runs detached, so no harness's cap on a session-end hook cuts it short.
/// Silent: a store failure leaves the claims for a person to reset.
pub(crate) fn release(holder: Option<payload::Holder>, checkout: &Checkout) {
    if let Some(holder) = holder {
        let holder = to_todo_holder(&holder);
        if let Ok(digest) = devkit_todo::digest_path(&holder) {
            let _ = std::fs::remove_file(digest);
        }
        let cwd = checkout.dir();
        let Ok(store) = Store::for_hook(checkout, cwd) else {
            return;
        };
        let entry = || Deferred::Release {
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
end your turn again: this reminder comes once per unchanged list.
If the user finds these reminders disruptive, `devkit todo hold never` turns them off for this \
session.";

/// The answer that refuses `holder`'s stop, or `None` to let it stop. A
/// stop is refused when the hold mode in effect holds `holder` (see
/// [`hold_mode::effective`]), `holder` has open todos (see
/// [`hold::open_for`]), and it was not already refused over the same list;
/// pending todos count when the caller's role holds its own node. Fails open:
/// a harness whose sessions name no node, a payload that cannot block, an
/// unreadable config or store, an unknown mode in [`HOLD_VAR`], and a
/// fingerprint that cannot be written all let it stop.
pub(crate) fn hold(
    payload: &Payload,
    holder: &payload::Holder,
    checkout: &Checkout,
) -> Option<String> {
    harness_of(payload.harness())?;
    let holder = to_todo_holder(holder);
    let session = holder.session();
    let backend_var = std::env::var(BACKEND_VAR).ok();
    let hold_var = std::env::var(HOLD_VAR).ok();
    let holds = |config: &devkit_config::TodoConfig| {
        hold_mode::effective(Some(&session), hold_var.as_deref(), Some(config))
            .is_ok_and(|(mode, _)| mode.holds(holder != session))
    };
    let store = Store::for_hold(checkout, checkout.dir(), backend_var.as_deref(), holds)?;
    if let Some(replica) = store.queued_replica() {
        drain(replica.data_dir());
    }
    let (_, _, role) = resolve_role(payload, checkout, &store, &holder)?;
    let pending_node = role.hold_pending.then_some(role.node);
    let todos = store.list(&Filter::all()).ok()?;
    let graph = Graph::new(&todos);
    let open = hold::open_for(&todos, &holder, pending_node.as_deref(), &graph);
    if open.is_empty() {
        return None;
    }
    let fingerprint = hold::fingerprint(&open);
    let path = hold::hold_path(&holder).ok()?;
    if std::fs::read_to_string(&path).is_ok_and(|seen| seen == fingerprint) {
        return None;
    }
    let mut nodes: Vec<String> = open.iter().map(|t| t.node().to_string()).collect();
    nodes.dedup();
    let open: Vec<Todo> = open.into_iter().cloned().collect();
    let graph = Graph::new(&todos);
    let notes = render::Notes {
        graph: &graph,
        show_ready: &|_| false,
        work: render::TicketWork::none(),
        names: &store.names(&todos),
    };
    let lists = render::render_lists(&nodes, &open, &holder, &notes);
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
    if let Ok(path) = hold::hold_path(&to_todo_holder(holder)) {
        let _ = std::fs::remove_file(path);
    }
}

/// Forgets every list `session` and its sub-agents were refused over, and the
/// hold mode it set.
pub(crate) fn forget_holds(session: &payload::Holder) {
    if let Ok(dir) = hold::hold_dir(&to_todo_holder(session)) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Records one hook write: on a replica with a queue, appends `entry` (given
/// no additional routing) and applies the queue; on any other store, makes the
/// write with `direct`. `Ok` once the write is applied or safely queued.
fn record_write(
    store: &Store,
    entry: impl FnOnce() -> Deferred,
    direct: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let Some(replica) = store.queued_replica() else {
        return direct();
    };
    let dir = replica.data_dir();
    let queued = queue::push(dir, &entry());
    drain(dir);
    queued
}

/// The arguments after `todo` when `inv` runs `devkit todo`, or through the
/// `todo` shim when that name resolves to `own`.
fn todo_args<'a>(
    inv: &'a Invocation,
    own: &OwnBinary,
) -> Option<impl Iterator<Item = Option<&'a str>>> {
    let program = inv.program.known().unwrap_or_default();
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let mut args = inv.args.iter().map(|a| a.known());
    if name.strip_suffix(".exe").unwrap_or(name) == "devkit" {
        return (args.next().flatten() == Some("todo")).then_some(args);
    }
    (Shim::from_argv0(program) == Some(Shim::Todo) && runs_own(inv, own)).then_some(args)
}

pub(crate) fn writes_todo(analysis: &Analysis, own: &OwnBinary) -> bool {
    analysis
        .invocations
        .iter()
        .filter_map(|inv| todo_args(inv, own))
        .any(|mut args| args.next().flatten() == Some("add"))
}

/// Every `todo start|stop|done|undone|cancel <id>...` in a command, run as
/// `devkit todo` or through devkit's `todo` shim, as the status each asks for
/// and the id it names. Ids the analysis could not resolve are skipped.
pub(crate) fn status_edits(analysis: &Analysis, own: &OwnBinary) -> Vec<(StatusKind, String)> {
    let mut out = Vec::new();
    for mut args in analysis
        .invocations
        .iter()
        .filter_map(|inv| todo_args(inv, own))
    {
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
    own: &OwnBinary,
    checkout: &Checkout,
    cwd: &Path,
) -> Option<String> {
    let actor = to_todo_holder(&payload.subagent_holder()?);
    let edits = status_edits(analysis, own);
    if edits.is_empty() {
        return None;
    }
    let (checkout, cwd) = (checkout.clone(), cwd.to_path_buf());
    let check = move || {
        let anchor = anchored(&checkout, &actor);
        let cwd = if anchor.dir() == checkout.dir() {
            cwd.as_path()
        } else {
            anchor.dir()
        };
        let store = Store::for_hook(&anchor, cwd).ok()?;
        edits.into_iter().find_map(|(to, id)| {
            let todo = store.resolve(&id).ok()??;
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

/// A subagent's shell inherits its parent's session id. Carry the exact
/// actor within this invocation, including scripts that call devkit.
pub(crate) fn rewrite(payload: &Payload, command: &str, dialect: Dialect) -> Option<String> {
    if dialect != Dialect::Bash || harness_of(payload.harness()).is_none() {
        return None;
    }
    let holder = payload.subagent_holder()?;
    Some(with_holder(command, &holder))
}

fn with_holder(command: &str, holder: &str) -> String {
    format!(
        "export {}='{}'\n{command}",
        HOLDER_VAR,
        holder.replace('\'', "'\\''")
    )
}

/// Mirrors the harness's own task and plan tools into the store. The native
/// tool has already run, so a failure, a claim conflict included, is reported
/// on stderr and skipped. A store whose lock stays busy past a hook's wait
/// keeps the write queued, and the next write or sync applies it.
pub(crate) fn capture(payload: &Payload, checkout: &Checkout) {
    if payload.tool_name().and_then(NativeTool::parse).is_none() {
        return;
    }
    if payload.holder().is_err() {
        return;
    }
    let cwd = checkout.dir().to_path_buf();
    let (Ok(store), Ok(_)) = (Store::for_hook(checkout, &cwd), devkit_todo::state_dir()) else {
        return;
    };
    let Some(mirror) = Mirror::of(payload, checkout, &store) else {
        return;
    };
    if mirror.node == GLOBAL && node::place_of(checkout).is_ok_and(|p| p == Place::Global) {
        eprintln!("{}", global_notice(&cwd));
    }
    let entry = || Deferred::Capture {
        mirror: Box::new(mirror.clone()),
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

/// Applies a queued write to the replica at `dir`, under the node it
/// was resolved with: the checkout may have changed branch or gone since.
fn apply_deferred(dir: &Path, entry: &Deferred) -> Result<()> {
    match entry {
        Deferred::Capture { mirror } => {
            mirror.apply(&Store::queued_at(dir, mirror.origin.as_deref())?)
        }
        Deferred::Release { holder } => Store::queued_at(dir, None)?
            .apply(&Edit::ReleaseAll {
                holder: holder.clone(),
            })
            .map(drop),
    }
}

/// A native tool call as a hook resolved it: the tool, whose ids it uses,
/// who acted, the node its session writes to, the repository node its ticket
/// todos are on, and the checkout the hook ran in, which a ticket event
/// fires from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Mirror {
    tool: String,
    /// The harness's node prefix, such as `claude`.
    harness: String,
    session: Holder,
    actor: Holder,
    node: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    origin: Option<PathBuf>,
    input: Value,
    response: Value,
}

impl Mirror {
    /// `None` for a call capture does not mirror: another tool, a harness
    /// without session nodes, no session, or a checkout git cannot read.
    fn of(payload: &Payload, checkout: &Checkout, store: &Store) -> Option<Self> {
        let tool = payload
            .tool_name()
            .filter(|t| NativeTool::parse(t).is_some())?;
        let harness = harness_of(payload.harness())?;
        let (Some(session), Ok(actor)) = (payload.session_holder(), payload.holder()) else {
            return None;
        };
        let (layout, facts, role) =
            resolve_role(payload, checkout, store, &to_todo_holder(&actor))?;
        let node = role.node;
        let repo = layout.fill("repo", &facts).ok();
        let raw = payload.raw();
        Some(Self {
            tool: tool.to_string(),
            harness: harness.prefix().to_string(),
            session: to_todo_holder(&session),
            actor: to_todo_holder(&actor),
            node,
            repo,
            origin: Some(checkout.dir().to_path_buf()),
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
        let Some(harness) = Harness::ALL
            .into_iter()
            .find(|h| h.prefix() == self.harness)
        else {
            anyhow::bail!("queued capture names an unknown harness {:?}", self.harness);
        };
        let c = Capture {
            harness,
            session: self.session.clone(),
            actor: self.actor.clone(),
            node: self.node.clone(),
            repo: self.repo.clone(),
            store,
            map: NativeMap::at(devkit_todo::state_dir()?),
        };
        let input = &self.input;
        store.while_locked(|| match tool {
            NativeTool::TaskCreate => task_create(&c, input, &self.response),
            NativeTool::TaskUpdate => task_update(&c, input),
            NativeTool::UpdatePlan => list_replace(&c, &input["plan"], "step"),
            NativeTool::TodoWrite => list_replace(&c, &input["todos"], "content"),
            NativeTool::GrokTodoWrite => {
                list_replace(&c, &self.response["TodosUpdated"]["todos"], "content")
            }
        })
    }
}

/// What mirroring a native call needs: whose ids it uses, who acted, the
/// node its session writes to, the repository node, and the store.
struct Capture<'a> {
    harness: Harness,
    session: Holder,
    actor: Holder,
    node: String,
    repo: Option<String>,
    store: &'a Store,
    map: NativeMap,
}

impl Capture<'_> {
    /// The todos on the repository node, among which a new native task or
    /// step finds the ticket todo it names.
    fn repo_todos(&self) -> Result<Vec<Todo>> {
        match &self.repo {
            Some(repo) => self.store.list(&Filter::exact([repo.clone()])),
            None => Ok(Vec::new()),
        }
    }
}

/// The open ticket todo among `repo_todos` that `text` names, which a new
/// native task or step maps to instead of adding a todo.
fn ticket_named(repo_todos: &[Todo], text: &str) -> Option<String> {
    tickets::named_ticket(repo_todos, text).map(|t| t.id.clone())
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
    /// Grok's `todo_write`, which by default sends only the items it changes
    /// and answers with the whole list.
    GrokTodoWrite,
}

impl NativeTool {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "TaskCreate" => Some(Self::TaskCreate),
            "TaskUpdate" => Some(Self::TaskUpdate),
            "update_plan" => Some(Self::UpdatePlan),
            "TodoWrite" => Some(Self::TodoWrite),
            "todo_write" => Some(Self::GrokTodoWrite),
            _ => None,
        }
    }
}

pub(crate) fn writes_native_todo(payload: &Payload) -> bool {
    payload
        .tool_name()
        .and_then(NativeTool::parse)
        .is_some_and(|tool| match tool {
            NativeTool::TaskCreate
            | NativeTool::TodoWrite
            | NativeTool::UpdatePlan
            | NativeTool::GrokTodoWrite => true,
            NativeTool::TaskUpdate => false,
        })
}

/// Claude Code's task list is shared by a session and its sub-agents, so the
/// mapping is recorded under the session for either to update it.
fn task_create(c: &Capture<'_>, input: &Value, response: &Value) -> Result<()> {
    let (Some(subject), Some(native)) =
        (input["subject"].as_str(), response["task"]["id"].as_str())
    else {
        return Ok(());
    };
    let id = match ticket_named(&c.repo_todos()?, subject) {
        Some(ticket) => ticket,
        None => c.store.add(NewTodo {
            ticket: None,
            project: Some(c.node.clone()),
            description: subject.to_string(),
            parent: None,
            order: None,
        })?,
    };
    c.map.record(c.harness, &c.session, native, &id)
}

fn task_update(c: &Capture<'_>, input: &Value) -> Result<()> {
    let Some(native) = input["taskId"].as_str() else {
        return Ok(());
    };
    let Some(id) = c.map.lookup(c.harness, &c.actor, native)? else {
        return Ok(());
    };
    let current = c.store.get(&id)?;
    let ticket = current.as_ref().is_some_and(|t| t.ticket.is_some());
    if let Some(subject) = input["subject"].as_str()
        && !ticket
        && current.is_some_and(|t| t.description != devkit_todo::one_line(subject))
    {
        c.store.apply(&Edit::Describe {
            id: id.clone(),
            description: subject.to_string(),
        })?;
    }
    if let Some(to) = input["status"].as_str().and_then(native_status)
        && !(ticket && to == StatusKind::Cancelled)
    {
        c.store.apply(&Edit::SetStatus {
            id: id.clone(),
            to,
            actor: c.actor.clone(),
        })?;
    }
    mirror_blockers(c, &id, input)
}

/// Records `addBlockedBy` and `addBlocks` as edges, skipping unknown native
/// ids and any single edge that would close a cycle. The hook still exits 0.
fn mirror_blockers(c: &Capture<'_>, id: &str, input: &Value) -> Result<()> {
    if let Some(blockers) = input["addBlockedBy"].as_array() {
        let by = native_ids(c, blockers)?;
        record_block(c, id, &by)?;
    }
    if let Some(blocked) = input["addBlocks"].as_array() {
        for blocked in native_ids(c, blocked)? {
            record_block(c, &blocked, &[id.to_string()])?;
        }
    }
    Ok(())
}

/// The mirrored ids of `natives`, skipping entries that are not strings and
/// native ids this session has not recorded.
fn native_ids(c: &Capture<'_>, natives: &[Value]) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for native in natives {
        let Some(native) = native.as_str() else {
            continue;
        };
        if let Some(id) = c.map.lookup(c.harness, &c.actor, native)? {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Records each member of `by` as a blocker of `id`. A member that would
/// close a cycle on its own is skipped; the rest are recorded.
fn record_block(c: &Capture<'_>, id: &str, by: &[String]) -> Result<()> {
    if by.is_empty() {
        return Ok(());
    }
    let todos = c.store.list(&Filter::all())?;
    let safe: Vec<String> = by
        .iter()
        .filter(|blocker| graph::cycle_through(&todos, id, &[(*blocker).clone()]).is_none())
        .cloned()
        .collect();
    if safe.is_empty() {
        return Ok(());
    }
    c.store.apply(&Edit::Block {
        id: id.to_string(),
        by: safe,
    })?;
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
    // A ticket todo's status also moves with its tracker, so a step naming
    // one changes it only when the tool's own status for the step changed:
    // resending a plan never undoes what a sync did.
    let previous: Vec<Mirrored> = snapshot
        .into_iter()
        .filter_map(|MirroredStep { text, todo, status }| {
            let current = todos.iter().find(|t| t.id == todo)?;
            let status = match status {
                Some(sent) if current.ticket.is_some() => sent,
                _ => current.status.kind(),
            };
            Some(Mirrored { text, todo, status })
        })
        .collect();
    let is_ticket = |id: &str| todos.iter().any(|t| t.id == id && t.ticket.is_some());
    // The list follows its session, so open steps left on the node the
    // session last wrote to move to the one it writes to now. A ticket todo
    // stays on its repository node.
    for prev in &previous {
        let open = matches!(prev.status, StatusKind::Pending | StatusKind::InProgress);
        let elsewhere = todos.iter().any(|t| {
            t.id == prev.todo && t.ticket.is_none() && t.project.as_deref() != Some(c.node.as_str())
        });
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
    let mut repo_todos = None;
    for change in changes {
        match change {
            Change::Add { index } => {
                let step = &next[index];
                if repo_todos.is_none() {
                    repo_todos = Some(c.repo_todos()?);
                }
                let ticket = ticket_named(repo_todos.as_deref().unwrap_or_default(), &step.text);
                let id = match &ticket {
                    Some(ticket) => ticket.clone(),
                    None => c.store.add(NewTodo {
                        ticket: None,
                        project: Some(c.node.clone()),
                        description: step.text.clone(),
                        parent: None,
                        order: Some(index as i64 * ORDER_GAP),
                    })?,
                };
                if step.status != StatusKind::Pending
                    && !(ticket.is_some() && step.status == StatusKind::Cancelled)
                {
                    apply(Edit::SetStatus {
                        id: id.clone(),
                        to: step.status,
                        actor: c.actor.clone(),
                    });
                }
                ids[index] = Some(id);
            }
            Change::Status {
                todo,
                to: StatusKind::Cancelled,
            } if is_ticket(&todo) => {}
            Change::Status { todo, to } => {
                apply(Edit::SetStatus {
                    id: todo,
                    to,
                    actor: c.actor.clone(),
                });
            }
            // Dropping a ticket's step forgets it; only the ticket closes
            // its todo.
            Change::Cancel { todo } if is_ticket(&todo) => {}
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
                        status: Some(prev.status),
                    });
                }
            }
            Change::Reorder { todo, .. } if is_ticket(&todo) => {}
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
                status: Some(step.status),
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
        "deleted" | "cancelled" => Some(StatusKind::Cancelled),
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
        status_edits(
            &devkit_command::analyze(command, &ctx),
            &OwnBinary::current(),
        )
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
