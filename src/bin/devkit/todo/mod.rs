//! `devkit todo`: the todo lists agents and people share, kept in the store
//! `[todo] backend` names.

pub(crate) mod queue;
pub(crate) mod store;
pub(crate) mod sync;

use std::{collections::BTreeSet, path::Path};

use anyhow::{Result, bail};
use clap::{Args, Subcommand, ValueEnum};
use devkit_common::{
    caller::{self, Caller},
    vcs::Checkout,
};
use devkit_todo::{
    Edit, Filter, Holder, NewTodo, NodeMatch, Status, StatusKind, Todo, TodoStore,
    holder::HOLDER_VAR,
    layout::{Facts, Layout},
    native::NativeMap,
    node::{self, GLOBAL, Place},
    render,
    roles::Roles,
    transition,
};
use pabal::AnyHarness;
use serde_json::{Value, json};

use self::{store::Store, sync::SyncOutcome};
use crate::hook::{
    self, HookEvent,
    todo::{resolve_role, to_todo_holder},
};

#[derive(Args)]
pub struct TodoCli {
    #[command(subcommand)]
    pub command: TodoCommand,
}

#[derive(Subcommand)]
pub enum TodoCommand {
    /// Print the node this caller writes its todos to.
    Scope,
    /// Print the caller's role and write node, or select its role.
    Role { name: Option<String> },
    /// List todos. Defaults to the caller's own node and every node above it.
    List(ListArgs),
    /// Add a todo and print its id.
    Add {
        /// What the todo says. Newlines and tabs collapse to single spaces.
        text: String,
        /// Nest the todo under this one.
        #[arg(long)]
        parent: Option<String>,
        /// Position among its siblings, lowest first. Defaults to after the
        /// last sibling.
        #[arg(long)]
        order: Option<i64>,
        /// The node to add to. Defaults to the one `scope` prints.
        #[arg(long, conflicts_with = "scope")]
        node: Option<String>,
        /// Fill this named scope with the caller's identity, walking up if
        /// needed.
        #[arg(long)]
        scope: Option<String>,
    },
    /// Mark todos in progress, claiming them for this caller.
    Start { ids: Vec<String> },
    /// Return todos in progress to pending.
    Stop { ids: Vec<String> },
    /// Mark todos completed.
    Done { ids: Vec<String> },
    /// Return completed todos to pending.
    Undone { ids: Vec<String> },
    /// Drop todos. The record stays, marked cancelled.
    Cancel { ids: Vec<String> },
    /// Replace a todo's text.
    Describe { id: String, text: String },
    /// Nest a todo under another, or change its position.
    Move {
        id: String,
        /// The new parent. Without it and without `--top` the todo keeps its
        /// parent.
        #[arg(long, conflicts_with = "top")]
        parent: Option<String>,
        /// Move the todo to the top level.
        #[arg(long)]
        top: bool,
        /// Position among its siblings. Defaults to after the last one.
        #[arg(long)]
        order: Option<i64>,
    },
    /// Remove a todo for good. Needs a person at a terminal.
    ///
    /// For text that must disappear, such as a pasted secret. Agents cancel
    /// instead, which keeps the record.
    Purge { id: String },
    /// Sync the todo store with its sync target.
    ///
    /// Only a taskchampion store with a sync target syncs. A failure is
    /// reported on stderr and exits 0: changes stay saved and sync with the
    /// next write.
    #[command(after_help = sync::hold_help())]
    Sync {
        #[arg(long, hide = true, help = sync::background_help())]
        background: bool,
        /// Write this sync's failure reason here. What a caller waiting on
        /// the sync passes, to learn its own result.
        #[arg(long, hide = true)]
        result_file: Option<std::path::PathBuf>,
    },
    /// Print the todo block a hook injects.
    ///
    /// Reads the hook payload on stdin, and prints nothing on any failure.
    Context(ContextArgs),
}

#[derive(Args)]
pub struct ContextArgs {
    /// Which harness sent the payload.
    #[arg(long)]
    pub harness: AnyHarness,
    /// Whether the agent guide precedes the lists.
    #[arg(long, value_enum, default_value_t = Guide::Full)]
    pub guide: Guide,
    /// Print nothing when the lists match the last ones injected for this
    /// agent.
    #[arg(long)]
    pub if_changed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Guide {
    Full,
    None,
}

#[derive(Args)]
pub struct ListArgs {
    /// Only todos on this named scope, filled with the caller's identity.
    #[arg(long, conflicts_with_all = ["node", "subtree", "all"])]
    pub scope: Option<String>,
    /// Only todos on exactly this node.
    #[arg(long, conflicts_with_all = ["subtree", "all"])]
    pub node: Option<String>,
    /// Todos on this node and every node below it.
    #[arg(long, conflicts_with = "all")]
    pub subtree: Option<String>,
    /// Every todo on every node.
    #[arg(long)]
    pub all: bool,
    /// Print alacritree's task shape as JSON. Cancelled todos are left out.
    #[arg(long)]
    pub json: bool,
    #[arg(long, help = sync::list_sync_help())]
    pub sync: bool,
}

pub fn run(cli: TodoCli) -> Result<()> {
    if let TodoCommand::Context(args) = cli.command {
        if let Some(text) = context(&args) {
            print!("{text}");
        }
        return Ok(());
    }
    let caller = caller::caller();
    let get = |key: &str| std::env::var(key).ok();
    let actor = actor_from_env(caller, get);
    let session = node::session_from_env(get);
    let cwd = std::env::current_dir()?;
    let store = Store::for_cli(&cwd)?;
    let layout = Layout::new(store.config())?;
    let facts = Facts::new(&place_at(&cwd)?, session.as_ref(), &actor);
    let roles = Roles::at(devkit_todo::state_dir());
    if let TodoCommand::Role { name: Some(name) } = &cli.command {
        if caller == Caller::Human || session.is_none() {
            bail!("devkit todo role needs an agent session; terminal callers have no role record");
        }
        roles.record(&layout, &actor, name)?;
    }
    let role = roles.resolve(&layout, &facts, &actor, None)?;
    if let Some(warning) = &role.warning {
        eprintln!("devkit todo: {warning}");
    }
    let writes = !matches!(
        cli.command,
        TodoCommand::Scope
            | TodoCommand::Role { .. }
            | TodoCommand::List(_)
            | TodoCommand::Sync { .. }
    );
    match cli.command {
        TodoCommand::Scope => println!("{}", role.node),
        TodoCommand::Role { .. } => println!("Role `{}`, writing to `{}`.", role.name, role.node),
        TodoCommand::List(args) => list(&store, &args, &cwd, &layout, &facts, &role.name, &actor)?,
        TodoCommand::Add {
            text,
            parent,
            order,
            node,
            scope,
        } => {
            let node = match node {
                Some(node) => node,
                None => match scope {
                    Some(scope) => layout.fill(&scope, &facts)?,
                    None => role.node,
                },
            };
            let id = store.add(NewTodo {
                project: project_of(node),
                description: text,
                parent,
                order,
            })?;
            println!("{}", devkit_todo::short_id(&id));
        }
        TodoCommand::Start { ids } => set_status(&store, ids, StatusKind::InProgress, &actor)?,
        TodoCommand::Stop { ids } | TodoCommand::Undone { ids } => {
            set_status(&store, ids, StatusKind::Pending, &actor)?
        }
        TodoCommand::Done { ids } => set_status(&store, ids, StatusKind::Completed, &actor)?,
        TodoCommand::Cancel { ids } => set_status(&store, ids, StatusKind::Cancelled, &actor)?,
        TodoCommand::Describe { id, text } => {
            store.apply(&Edit::Describe {
                id,
                description: text,
            })?;
        }
        TodoCommand::Move {
            id,
            parent,
            top,
            order,
        } => move_todo(&store, id, parent, top, order)?,
        TodoCommand::Sync {
            background,
            result_file,
        } => match Store::sync_replica(&cwd)? {
            Some(replica) => sync::run(&replica, background, result_file.as_deref())?,
            None if !background => eprintln!("devkit todo: this todo store has no sync target"),
            None => {}
        },
        TodoCommand::Context(_) => unreachable!("answered before the store is opened"),
        TodoCommand::Purge { id } => {
            if caller == Caller::Agent {
                bail!(
                    "devkit todo purge needs a person at a terminal; agents cancel instead \
                     (devkit todo cancel <id>)"
                );
            }
            let Some(todo) = store.get(&id)? else {
                bail!("no todo {id}");
            };
            store.apply(&Edit::Purge(todo.id.clone()))?;
            NativeMap::at(devkit_todo::state_dir()).forget(&todo.id)?;
        }
    }
    if writes {
        if let Some(replica) = store.queued_replica() {
            crate::hook::todo::drain(replica.data_dir());
        }
        store.spawn_sync(&cwd);
    }
    Ok(())
}

/// Where `dir` sits: global outside any repository.
pub(crate) fn place_at(dir: &Path) -> Result<Place> {
    node::place_of(&Checkout::at(dir))
}

/// The holder a CLI call acts as: the sub-agent [`HOLDER_VAR`] names when its
/// session covers it, else the harness session for an agent, `agent` for an
/// agent outside any harness session, and `human` for a person.
pub(crate) fn actor_from_env(caller: Caller, get: impl Fn(&str) -> Option<String>) -> Holder {
    if caller == Caller::Human {
        return Holder::human();
    }
    let session = node::session_from_env(&get).map(|s| Holder::new(s.id));
    let sub_agent = get(HOLDER_VAR)
        .map(|h| Holder::new(h.trim()))
        .filter(|h| !h.is_empty() && session.as_ref().is_some_and(|s| s.covers(h)));
    sub_agent
        .or(session)
        .unwrap_or_else(|| Holder::new("agent"))
}

/// The global list is stored without a node.
fn project_of(node: String) -> Option<String> {
    (node != GLOBAL).then_some(node)
}

fn set_status(
    store: &impl TodoStore,
    ids: Vec<String>,
    to: StatusKind,
    actor: &Holder,
) -> Result<()> {
    if ids.is_empty() {
        bail!("name at least one todo id");
    }
    // Every id is checked before any is written, so a batch with one unknown
    // or claimed todo changes nothing.
    let mut todos = Vec::new();
    for id in &ids {
        let Some(todo) = store.get(id)? else {
            bail!("no todo {id}");
        };
        transition(&todo.status, to, actor)?;
        todos.push(todo);
    }
    for todo in todos {
        store.apply(&Edit::SetStatus {
            id: todo.id,
            to,
            actor: actor.clone(),
        })?;
    }
    Ok(())
}

fn move_todo(
    store: &impl TodoStore,
    id: String,
    parent: Option<String>,
    top: bool,
    order: Option<i64>,
) -> Result<()> {
    if parent.is_none() && !top {
        let Some(order) = order else {
            bail!("name a new --parent, --top or an --order");
        };
        return store.apply(&Edit::Reorder { id, order }).map(drop);
    }
    store.apply(&Edit::Move { id, parent, order }).map(drop)
}

fn list(
    store: &Store,
    args: &ListArgs,
    cwd: &Path,
    layout: &Layout,
    facts: &Facts,
    role: &str,
    viewer: &Holder,
) -> Result<()> {
    let filter = match (&args.scope, &args.node, &args.subtree, args.all) {
        (Some(scope), ..) => Filter::exact([layout.fill(scope, facts)?]),
        (_, Some(n), ..) => Filter::exact([n.clone()]),
        (_, _, Some(n), _) => Filter {
            nodes: vec![NodeMatch::Subtree(n.clone())],
        },
        (_, _, _, true) => Filter::all(),
        _ => layout.visible(facts, role)?,
    };
    if args.sync {
        match store.sync(cwd, sync::FRESH_WAIT) {
            SyncOutcome::Failed(reason) => eprintln!("{}", sync::failure_text(&reason)),
            SyncOutcome::Refused(reason) => eprintln!("devkit todo: {reason}; listing local todos"),
            SyncOutcome::StillRunning => eprintln!(
                "devkit todo: sync still running after {} seconds; listing what this machine has",
                sync::FRESH_WAIT.as_secs()
            ),
            SyncOutcome::Done | SyncOutcome::NoTarget => {}
        }
    }
    let todos = if args.all || args.subtree.is_some() {
        layout
            .fence(store.list(&Filter::all())?)
            .into_iter()
            .filter(|todo| filter.matches(todo.project.as_deref()))
            .collect()
    } else {
        store.list(&filter)?
    };
    if args.json {
        let rows: Vec<Value> = todos.iter().filter_map(alacritree_json).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let nodes = list_nodes(&filter, &todos);
    print!("{}", render::render_full(&nodes, &todos, viewer));
    Ok(())
}

fn list_nodes(filter: &Filter, todos: &[Todo]) -> Vec<String> {
    let mut nodes: Vec<String> = filter
        .nodes
        .iter()
        .filter_map(|node| match node {
            NodeMatch::Exact(node) => Some(node.clone()),
            NodeMatch::Subtree(_) | NodeMatch::SessionDescendants { .. } => None,
        })
        .collect();
    let present: BTreeSet<&str> = todos.iter().map(Todo::node).collect();
    nodes.extend(
        present
            .into_iter()
            .filter(|node| {
                !filter
                    .nodes
                    .iter()
                    .any(|exact| matches!(exact, NodeMatch::Exact(n) if n == node))
            })
            .map(str::to_string),
    );
    nodes
}

/// A todo as alacritree's command backend reads a task. A cancelled todo has
/// no such task, as taskwarrior's deleted tasks have none.
pub(crate) fn alacritree_json(todo: &Todo) -> Option<Value> {
    let (status, started) = match &todo.status {
        Status::Pending => ("pending", false),
        Status::InProgress { .. } => ("pending", true),
        Status::Completed { .. } => ("completed", false),
        Status::Cancelled { .. } => return None,
    };
    Some(json!({
        "id": todo.id,
        "description": todo.description,
        "status": status,
        "started": started,
        "parent": todo.parent,
        "order": todo.order,
        "project": todo.project,
        "entry": todo.entry,
        "modified": todo.modified,
    }))
}

/// The block to inject, or `None` for silence: no payload, no session, a
/// harness without session nodes, nothing new under `--if-changed`, or any
/// store failure.
fn context(args: &ContextArgs) -> Option<String> {
    let payload = hook::read_payload(Some(args.harness), HookEvent::SessionStart)?;
    payload.session_id()?;
    let viewer = to_todo_holder(&payload.holder().ok()?);
    let cwd = hook::record::payload_cwd(&payload);
    let checkout = Checkout::at(&cwd);
    let store = Store::for_hook(&checkout, &cwd);
    let (layout, facts, role) = resolve_role(&payload, &checkout, &store, &viewer)?;
    let visible = layout.visible(&facts, &role.name).ok()?;
    let own = &role.node;
    let workspace = facts
        .values
        .contains_key("branch")
        .then(|| layout.fill("workspace", &facts).ok())
        .flatten();
    let mut filter = visible.clone();
    filter
        .nodes
        .extend(workspace.iter().map(|w| NodeMatch::Subtree(w.clone())));
    let at_start = payload.event_name().as_deref() == Some("SessionStart");
    // A new container's replica is empty until it pulls the lists.
    if at_start && let SyncOutcome::Refused(reason) = store.sync(&cwd, sync::FRESH_WAIT) {
        eprintln!("devkit todo: {reason}; listing local todos");
    }
    let all = store.list(&Filter::all()).ok()?;
    let todos: Vec<Todo> = all
        .iter()
        .filter(|t| visible.matches(t.project.as_deref()))
        .cloned()
        .collect();
    let siblings = layout.fence(all).into_iter().filter(|t| {
        filter.matches(t.project.as_deref())
            && !visible.matches(t.project.as_deref())
            && layout.is_other_session(t.node(), &facts)
    });
    let nodes = list_nodes(&visible, &todos);
    let mut lists = render::render_lists(&nodes, &todos, &viewer);
    let own_open = todos.iter().any(|t| {
        t.node() == own
            && matches!(
                t.status.kind(),
                StatusKind::Pending | StatusKind::InProgress
            )
    });
    let pending = siblings
        .filter(|t| t.status.kind() == StatusKind::Pending)
        .count();
    if let Some(line) = workspace
        .filter(|_| at_start || !own_open)
        .and_then(|w| render::left_by_others(&w, pending))
    {
        if !lists.is_empty() {
            lists.push('\n');
        }
        lists.push_str(&line);
        lists.push('\n');
    }
    let role_line = format!("Role `{}`, writing to `{}`.", role.name, role.node);
    let digest = render::digest(&format!("{role_line}\n{lists}"));
    let digest_path = devkit_todo::digest_path(&viewer);
    if args.if_changed && std::fs::read_to_string(&digest_path).is_ok_and(|seen| seen == digest) {
        return None;
    }
    let text = match args.guide {
        Guide::Full => {
            let guide = format!("{}\n{role_line}", render::guide(&role.node));
            if lists.is_empty() {
                guide
            } else {
                format!("{guide}\n{lists}")
            }
        }
        Guide::None if lists.is_empty() => return None,
        Guide::None => format!("{role_line}\n{lists}"),
    };
    if let Some(dir) = digest_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&digest_path, digest);
    Some(match payload.context_answer(&text) {
        Some(envelope) => format!("{envelope}\n"),
        None => text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn a_sub_agent_holder_under_the_session_is_the_actor() {
        let sub = [("CLAUDE_CODE_SESSION_ID", "S"), (HOLDER_VAR, "S/a1")];
        assert_eq!(
            actor_from_env(Caller::Agent, env(&sub)),
            Holder::new("S/a1")
        );
        let foreign = [("CLAUDE_CODE_SESSION_ID", "S"), (HOLDER_VAR, "T/a1")];
        assert_eq!(
            actor_from_env(Caller::Agent, env(&foreign)),
            Holder::new("S")
        );
        assert_eq!(
            actor_from_env(Caller::Agent, env(&[(HOLDER_VAR, "S/a1")])),
            Holder::new("agent")
        );
        assert_eq!(actor_from_env(Caller::Human, env(&sub)), Holder::human());
    }
}
