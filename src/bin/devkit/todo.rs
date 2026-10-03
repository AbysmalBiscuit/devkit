//! `devkit todo`: the todo lists agents and people share, kept in devkit's
//! own store.

use std::{collections::BTreeSet, path::Path};

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use devkit_common::{
    caller::{self, Caller},
    vcs::Checkout,
};
use devkit_todo::{
    BuiltinStore, Edit, Filter, Holder, NewTodo, NodeMatch, ORDER_GAP, Status, StatusKind, Todo,
    TodoStore,
    node::{self, GLOBAL, Place, SessionRef},
    render,
};
use serde_json::{Value, json};

#[derive(Args)]
pub struct TodoCli {
    #[command(subcommand)]
    pub command: TodoCommand,
}

#[derive(Subcommand)]
pub enum TodoCommand {
    /// Print the node this caller writes its todos to.
    Scope,
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
        #[arg(long)]
        node: Option<String>,
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
    /// Remove a todo for good, such as one whose text holds a secret. Needs a
    /// person at a terminal.
    Purge { id: String },
}

#[derive(Args)]
pub struct ListArgs {
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
}

pub fn run(cli: TodoCli) -> Result<()> {
    let caller = caller::caller();
    let get = |key: &str| std::env::var(key).ok();
    let actor = actor_from_env(caller, get);
    let session = node::session_from_env(get);
    let cwd = std::env::current_dir()?;
    let place = place_at(&cwd);
    let store = store();
    match cli.command {
        TodoCommand::Scope => println!("{}", node::node(&place, session.as_ref())),
        TodoCommand::List(args) => list(&store, &args, &place, session.as_ref(), &actor)?,
        TodoCommand::Add {
            text,
            parent,
            order,
            node,
        } => {
            let node = node.unwrap_or_else(|| node::node(&place, session.as_ref()));
            let id = store.add(NewTodo {
                project: project_of(node),
                description: text,
                parent,
                order,
            })?;
            println!("{id}");
        }
        TodoCommand::Start { ids } => set_status(&store, ids, StatusKind::InProgress, &actor)?,
        TodoCommand::Stop { ids } | TodoCommand::Undone { ids } => {
            set_status(&store, ids, StatusKind::Pending, &actor)?
        }
        TodoCommand::Done { ids } => set_status(&store, ids, StatusKind::Completed, &actor)?,
        TodoCommand::Cancel { ids } => set_status(&store, ids, StatusKind::Cancelled, &actor)?,
        TodoCommand::Describe { id, text } => store.apply(&Edit::Describe {
            id,
            description: text,
        })?,
        TodoCommand::Move {
            id,
            parent,
            top,
            order,
        } => move_todo(&store, id, parent, top, order)?,
        TodoCommand::Purge { id } => {
            if caller == Caller::Agent {
                bail!(
                    "devkit todo purge needs a person at a terminal; agents cancel instead \
                     (devkit todo cancel <id>)"
                );
            }
            store.apply(&Edit::Purge(id))?;
        }
    }
    Ok(())
}

pub(crate) fn store() -> BuiltinStore {
    BuiltinStore::at(BuiltinStore::default_dir())
}

/// Where `dir` sits: global outside any repository.
pub(crate) fn place_at(dir: &Path) -> Place {
    node::place_of(&Checkout::at(dir))
}

/// The holder a CLI call acts as: the harness session for an agent, `agent`
/// for an agent outside any harness session, `human` for a person.
pub(crate) fn actor_from_env(caller: Caller, get: impl Fn(&str) -> Option<String>) -> Holder {
    match caller {
        Caller::Human => Holder::human(),
        Caller::Agent => {
            node::session_from_env(get).map_or_else(|| Holder::new("agent"), |s| Holder::new(s.id))
        }
    }
}

/// The global list is stored without a node.
fn project_of(node: String) -> Option<String> {
    (node != GLOBAL).then_some(node)
}

fn set_status(
    store: &BuiltinStore,
    ids: Vec<String>,
    to: StatusKind,
    actor: &Holder,
) -> Result<()> {
    if ids.is_empty() {
        bail!("name at least one todo id");
    }
    for id in ids {
        store.apply(&Edit::SetStatus {
            id,
            to,
            actor: actor.clone(),
        })?;
    }
    Ok(())
}

fn move_todo(
    store: &BuiltinStore,
    id: String,
    parent: Option<String>,
    top: bool,
    order: Option<i64>,
) -> Result<()> {
    if parent.is_none() && !top {
        let Some(order) = order else {
            bail!("name a new --parent, --top or an --order");
        };
        return store.apply(&Edit::Reorder { id, order });
    }
    let order = match order {
        Some(order) => order,
        None => {
            let todos = store.list(&Filter::all())?;
            let Some(me) = todos.iter().find(|t| t.id == id) else {
                bail!("no todo {id}");
            };
            todos
                .iter()
                .filter(|t| t.project == me.project && t.parent == parent && t.id != id)
                .filter_map(|t| t.order)
                .max()
                .unwrap_or(0)
                + ORDER_GAP
        }
    };
    store.apply(&Edit::Move { id, parent, order })
}

fn list(
    store: &BuiltinStore,
    args: &ListArgs,
    place: &Place,
    session: Option<&SessionRef>,
    viewer: &Holder,
) -> Result<()> {
    let visible = node::visible_nodes(place, session);
    let filter = match (&args.node, &args.subtree, args.all) {
        (Some(n), ..) => Filter::exact([n.clone()]),
        (_, Some(n), _) => Filter {
            nodes: vec![NodeMatch::Subtree(n.clone())],
        },
        (_, _, true) => Filter::all(),
        _ => Filter::exact(visible.clone()),
    };
    let todos = store.list(&filter)?;
    if args.json {
        let rows: Vec<Value> = todos.iter().filter_map(alacritree_json).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let nodes = if args.node.is_none() && args.subtree.is_none() && !args.all {
        visible
    } else {
        let present: BTreeSet<&str> = todos.iter().map(Todo::node).collect();
        present.into_iter().map(str::to_string).collect()
    };
    print!("{}", render::render_full(&nodes, &todos, viewer));
    Ok(())
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
