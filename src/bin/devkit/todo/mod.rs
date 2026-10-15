//! `devkit todo`: the todo lists agents and people share, kept in the store
//! `[todo] backend` names.

pub(crate) mod hold;
mod names;
pub(crate) mod promote;
pub(crate) mod push;
pub(crate) mod queue;
pub(crate) mod store;
pub(crate) mod sync;
pub(crate) mod ticket_event;
pub(crate) mod tickets;

use std::{cell::LazyCell, collections::BTreeSet, path::Path};

use anyhow::{Result, bail};
use clap::{Args, Subcommand, ValueEnum, builder::TypedValueParser as _};
use devkit::completions::Shell;
use devkit_common::{
    caller::{self, Caller},
    vcs::Checkout,
};
use devkit_config::{HoldStop, IdStyle, PushKind};
use devkit_todo::{
    Claimed, Edit, Filter, Holder, NewTodo, NodeMatch, Status, StatusKind, Todo, TodoStore,
    activity::{ActivityStore as _, SessionState},
    graph::{self, Graph},
    holder::HOLDER_VAR,
    layout::{Facts, Layout},
    native::NativeMap,
    node::{self, GLOBAL, Place},
    push::LocalStatus,
    render,
    roles::{ResolvedRole, Roles},
    transition,
};
use pabal::AnyHarness;
use serde_json::{Value, json};
use strum::VariantNames as _;

use self::{store::Store, sync::SyncOutcome};
use crate::hook::{
    self, HookEvent,
    todo::{anchored, resolve_role, to_todo_holder},
};

#[derive(Args)]
pub struct TodoCli {
    /// How listings name todos: `name`, `short` or `full`. Overrides
    /// [todo] ids for this command.
    #[arg(long = "ids", global = true, value_name = "STYLE", value_parser = id_styles())]
    pub id_style: Option<IdStyle>,
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
        /// Nest the todo under this one. With "parent" in [ticket.sync] push,
        /// not under a ticket todo: file a sub-issue with `devkit ticket
        /// create` instead.
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
        /// Todos this one waits on, comma-separated.
        #[arg(long, value_delimiter = ',')]
        blocked_by: Vec<String>,
    },
    /// Record that a todo waits on others.
    #[command(after_help = LINK_PUSH_HELP)]
    Block {
        id: String,
        /// The todos it waits on, comma-separated.
        #[arg(long, value_delimiter = ',', required = true)]
        by: Vec<String>,
        #[arg(long, help = LINK_OVERWRITE_HELP)]
        overwrite_tracker: bool,
    },
    /// Remove blockers from a todo.
    #[command(after_help = LINK_PUSH_HELP)]
    Unblock {
        id: String,
        /// The blockers to remove, comma-separated.
        #[arg(long, value_delimiter = ',', required = true)]
        by: Vec<String>,
        #[arg(long, help = LINK_OVERWRITE_HELP)]
        overwrite_tracker: bool,
    },
    /// Mark todos in progress, claiming them for this caller.
    #[command(after_help = PUSH_HELP)]
    Start {
        #[arg(required = true)]
        ids: Vec<String>,
        /// Start a todo even while a blocker is still open.
        #[arg(long)]
        force: bool,
        #[arg(long, help = OVERWRITE_HELP)]
        overwrite_tracker: bool,
    },
    /// Return todos in progress to pending.
    Stop {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Mark todos completed.
    #[command(after_help = PUSH_HELP)]
    Done {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long, help = OVERWRITE_HELP)]
        overwrite_tracker: bool,
    },
    /// Return completed todos to pending.
    Undone {
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Drop todos. The record stays, marked cancelled.
    #[command(after_help = PUSH_HELP)]
    Cancel {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long, help = OVERWRITE_HELP)]
        overwrite_tracker: bool,
    },
    /// Replace a todo's text.
    Describe { id: String, text: String },
    /// File a todo as a ticket, or link it to an existing one.
    ///
    /// The ticket's title is the todo's text; body and field args are as for
    /// `ticket create`. Its todo comes from syncing it, so [ticket.sync] rules
    /// are needed. Blocking edges move to that todo, and the old todo is
    /// cancelled with the ticket's key as its text; its children stay put.
    Promote {
        id: String,
        /// Link this existing ticket instead of filing one, by its key or as
        /// the tracker reads an issue (`#65`, `65`, its URL). The recovery
        /// after a promote whose create may have gone through.
        #[arg(long, value_name = "KEY", conflicts_with_all = ["body", "args", "arg_files", "none"])]
        ticket: Option<String>,
        /// Issue body, the `input` of the `ticket_body` template.
        #[arg(long)]
        body: Option<String>,
        #[command(flatten)]
        vars: crate::template::VarArgs,
        #[command(flatten)]
        fields: crate::template::FieldArgs,
    },
    /// Nest a todo under another, or change its position.
    #[command(after_help = LINK_PUSH_HELP)]
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
        #[arg(long, help = LINK_OVERWRITE_HELP)]
        overwrite_tracker: bool,
    },
    /// Remove a todo for good. Needs a person at a terminal.
    ///
    /// For text that must disappear, such as a pasted secret. Agents cancel
    /// instead, which keeps the record.
    Purge { id: String },
    /// Sync the todo store with its sync target, then the repo's tickets.
    ///
    /// Only a taskchampion store with a sync target syncs. A failure is
    /// reported on stderr and exits 0: changes stay saved and sync with the
    /// next write, and no tickets sync that run. With `[ticket.sync]` rules,
    /// the tickets they select then become one todo each on the repository's
    /// node, reading the tracker and never writing to it; a ticket in a
    /// status the map does not name fails the sync and changes nothing.
    #[command(after_help = sync::hold_help())]
    Sync {
        #[arg(long, hide = true, help = sync::background_help())]
        background: bool,
        /// Print the ticket changes a sync would make, and sync nothing.
        #[arg(long, conflicts_with = "background")]
        dry_run: bool,
        /// Write this sync's failure reason here. What a caller waiting on
        /// the sync passes, to learn its own result.
        #[arg(long, hide = true)]
        result_file: Option<std::path::PathBuf>,
        /// Sync only the repo's tickets, not the store. With `--background`,
        /// what a read starts when the last ticket sync is stale.
        #[arg(long, hide = true, conflicts_with = "dry_run")]
        tickets_only: bool,
        /// Sync only this ticket, by its key (`owner/repo#65`, `ENG-123`),
        /// or as the tracker reads an issue (`#65`, `65`, its URL), reading
        /// no other and not the store's sync target. A ticket no
        /// rule selects is brought in too.
        #[arg(long, value_name = "KEY", conflicts_with_all = ["background", "dry_run", "tickets_only"])]
        ticket: Option<String>,
    },
    /// Set whose stop this session's open todos hold, or print the mode.
    ///
    /// `always` holds every stop, the main agent's included, `subagents`
    /// only a sub-agent's, and `never` none. The mode applies to the calling
    /// session and its sub-agents until the session ends, over
    /// DEVKIT_TODO_HOLD_STOP and `[todo] hold_stop`. With no argument, prints
    /// the mode in effect and where it came from.
    Hold {
        #[arg(value_parser = hold_modes())]
        mode: Option<HoldStop>,
        /// Drop this session's mode, so the variable or the config decides.
        #[arg(long, conflicts_with = "mode")]
        clear: bool,
    },
    /// Manage the todo database's schema on the postgres backend.
    Schema {
        #[command(subcommand)]
        action: SchemaAction,
    },
    /// Print the todos that can start now, critical path first.
    ///
    /// In a worktree whose `workspace setup` record names a synced ticket,
    /// the ticket's todo is offered beside the workspace's own.
    Ready,
    /// Print the open todos as batches, and the critical path through them.
    Plan {
        /// Cap each batch at this many todos.
        #[arg(long)]
        agents: Option<std::num::NonZeroUsize>,
        /// Print the batches as Mermaid flowchart source.
        #[arg(long, conflicts_with = "expand")]
        mermaid: bool,
        /// List each synced ticket's open workspace todos under it.
        #[arg(long)]
        expand: bool,
    },
    /// Print the todo block a hook injects.
    ///
    /// Reads the hook payload on stdin, and prints nothing on any failure.
    Context(ContextArgs),
    /// Print a shell-completion script (bash, zsh, fish, ...) to stdout.
    Completions {
        /// Shell to emit the script for.
        shell: Shell,
    },
}

const OVERWRITE_HELP: &str = "Write a ticket todo's change over a status the tracker changed since \
     the last sync. Only on the user's word.";

const PUSH_HELP: &str = "With \"status\" in [ticket.sync] push, a ticket todo's change moves its \
     ticket before the command returns, as [ticket.events] todo_start, todo_done and todo_cancel \
     configure; on GitHub, done and cancel also close the issue. A ticket whose status changed \
     on the tracker since the last sync stops the command, naming both values, and changes \
     nothing. Other todos in the same command change as usual.";

const LINK_OVERWRITE_HELP: &str = "Write a link between ticket todos over a link the tracker \
     changed since the last sync. Only on the user's word.";

const LINK_PUSH_HELP: &str = "A parent or blocking link between two ticket todos of one tracker \
     reaches the tickets before the command returns, with \"parent\" or \"blocking\" in \
     [ticket.sync] push, run from a checkout of their repository: a sub-issue or blocked-by link \
     on GitHub, a parent or blocks relation on Linear. Without the opt-in, or from elsewhere, \
     the change is refused, since the next sync would revert it. A tracker link changed since \
     the last sync stops the command, naming both, and changes nothing. With \"parent\" \
     pushing, a todo that is not a ticket todo cannot go under a ticket todo, nor a ticket todo \
     under it. Links with other todos stay local.";

#[derive(Subcommand)]
pub enum SchemaAction {
    /// Create the tables and functions the database lacks.
    ///
    /// Creates every table and function this devkit needs that the database
    /// lacks, and replaces each function with this version's. devkit creates
    /// its schema on first use, but a database that already has one gains a
    /// function a newer devkit adds only when a postgres call needs it, and
    /// the supabase backend cannot create it at all. Run this after upgrading
    /// devkit. It needs the postgres backend: for a supabase deployment, run
    /// it with DEVKIT_TODO_BACKEND=postgres and DEVKIT_TODO_DATABASE_URL
    /// naming the project's database.
    Update,
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
    /// List each synced ticket's open workspace todos under it.
    #[arg(long, conflicts_with = "json")]
    pub expand: bool,
}

pub fn run(cli: TodoCli) -> Result<()> {
    if let TodoCommand::Completions { shell } = cli.command {
        return crate::emit_completions(shell, "todo", "todo");
    }
    if let TodoCommand::Context(args) = cli.command {
        if let Some(text) = context(&args) {
            print!("{text}");
        }
        return Ok(());
    }
    if let TodoCommand::Hold { mode, clear } = cli.command {
        return hold::run(mode, clear, &std::env::current_dir()?);
    }
    if let TodoCommand::Schema {
        action: SchemaAction::Update,
    } = cli.command
    {
        let (target, created) = Store::update_schema(&std::env::current_dir()?)?;
        for object in &created {
            println!("created {object}");
        }
        println!("devkit's schema in {target} is up to date");
        return Ok(());
    }
    let caller = caller::caller();
    let get = |key: &str| std::env::var(key).ok();
    let actor = actor_from_env(caller, get);
    let session = node::session_from_env(get);
    let here = std::env::current_dir()?;
    let roles = Roles::at(devkit_todo::state_dir());
    let checkout = anchor(Checkout::at(&here), &roles, &actor)?;
    let cwd = checkout.dir().to_path_buf();
    let place = node::place_of(&checkout)?;
    let store = Store::for_cli(&cwd)?;
    let layout = Layout::new(store.config())?;
    let facts = Facts::new(&place, session.as_ref(), &actor);
    if let TodoCommand::Role { name: Some(name) } = &cli.command {
        if caller == Caller::Human || session.is_none() {
            bail!("devkit todo role needs an agent session; terminal callers have no role record");
        }
        let checkout = (place != Place::Global).then_some(cwd.as_path());
        roles.record(&layout, &actor, name, checkout)?;
    }
    let role = roles.resolve(&layout, &facts, &actor, None)?;
    if let Some(warning) = &role.warning {
        eprintln!("devkit todo: {warning}");
    }
    let mut writes = !matches!(
        cli.command,
        TodoCommand::Scope
            | TodoCommand::Role { .. }
            | TodoCommand::List(_)
            | TodoCommand::Ready
            | TodoCommand::Plan { .. }
            | TodoCommand::Sync { .. }
    );
    let refreshes = match &cli.command {
        TodoCommand::List(args) => !args.sync,
        TodoCommand::Ready | TodoCommand::Plan { .. } => true,
        _ => false,
    };
    if refreshes {
        refresh_tickets(&store, &cwd, &layout, &facts);
    }
    let style = cli.id_style.unwrap_or(store.config().ids);
    let fence = Fence::new(&store, &layout, &facts, &role.name, &checkout);
    let status_push = |local, overwrite| push::StatusPush {
        local,
        overwrite,
        caller,
        actor: &actor,
        cwd: &cwd,
    };
    let link_push = |overwrite| push::LinkPush {
        overwrite,
        caller,
        actor: &actor,
        cwd: &cwd,
    };
    match cli.command {
        TodoCommand::Scope => println!("{}", role.node),
        TodoCommand::Role { .. } => println!("Role `{}`, writing to `{}`.", role.name, role.node),
        TodoCommand::List(args) => list(&fence, &args, &cwd, &role, &actor, style)?,
        TodoCommand::Ready => ready(&fence, style)?,
        TodoCommand::Plan {
            agents,
            mermaid,
            expand,
        } => plan(&fence, agents, mermaid, expand, style)?,
        TodoCommand::Add {
            text,
            parent,
            order,
            node,
            scope,
            blocked_by,
        } => {
            let node = match node {
                Some(node) => node,
                None => {
                    let node = match scope {
                        Some(scope) => layout.fill(&scope, &facts)?,
                        None => role.node,
                    };
                    if place == Place::Global && node == GLOBAL {
                        eprintln!("{}", global_notice(&here));
                    }
                    node
                }
            };
            let parent = parent.map(|p| resolve_id(&store, &p)).transpose()?;
            if let Some(parent) = &parent {
                refuse_under_a_ticket(&store, parent, &link_push(false))?;
            }
            let blockers = resolve_ids(&store, &blocked_by)?;
            refuse_cycle(&store, None, &blockers)?;
            let id = store.add(NewTodo {
                ticket: None,
                project: project_of(node),
                description: text,
                parent: parent.map(|p| p.id),
                order,
            })?;
            if !blockers.is_empty()
                && let Err(err) = store.apply(&Edit::Block {
                    id: id.clone(),
                    by: blockers,
                })
            {
                // The todo is already stored. Drop it so a retry does not
                // leave an unblocked copy beside the one the edges attach to.
                return match store.apply(&Edit::Purge(id)) {
                    Ok(_) => Err(err),
                    Err(purge) => Err(err.context(purge)),
                };
            }
            println!("{}", names::Names::plain(style).id(&id));
        }
        TodoCommand::Block {
            id,
            by,
            overwrite_tracker,
        } => link(&store, &id, &by, true, &link_push(overwrite_tracker))?,
        TodoCommand::Unblock {
            id,
            by,
            overwrite_tracker,
        } => link(&store, &id, &by, false, &link_push(overwrite_tracker))?,
        TodoCommand::Start {
            ids,
            force,
            overwrite_tracker,
        } => {
            let activity = LazyCell::new(|| {
                Store::activity_for_cli(&cwd)
                    .and_then(|log| log.read_now())
                    .ok()
            });
            let gone = |holder: &Holder| {
                activity.as_ref().is_some_and(|activity| {
                    matches!(
                        activity.session_state(holder),
                        Some(SessionState::Ended { .. } | SessionState::Silent { .. })
                    )
                })
            };
            let push = status_push(LocalStatus::Start, overwrite_tracker);
            start(&store, ids, &actor, gone, force, &push)?
        }
        TodoCommand::Stop { ids } | TodoCommand::Undone { ids } => {
            let _closed = set_status(&store, ids, StatusKind::Pending, &actor, None)?;
        }
        TodoCommand::Done {
            ids,
            overwrite_tracker,
        } => {
            let push = status_push(LocalStatus::Done, overwrite_tracker);
            finish(&fence, ids, StatusKind::Completed, &actor, &push)?
        }
        TodoCommand::Cancel {
            ids,
            overwrite_tracker,
        } => {
            let push = status_push(LocalStatus::Cancel, overwrite_tracker);
            finish(&fence, ids, StatusKind::Cancelled, &actor, &push)?
        }
        TodoCommand::Describe { id, text } => {
            store.apply(&Edit::Describe {
                id: resolve_id(&store, &id)?.id,
                description: text,
            })?;
        }
        TodoCommand::Promote {
            id,
            ticket,
            body,
            vars,
            fields,
        } => {
            if place == Place::Global {
                bail!("devkit todo promote runs in a checkout of the ticket's repository");
            }
            let ticket = ticket.as_deref().map(str::trim);
            if ticket.is_some_and(str::is_empty) {
                bail!("--ticket needs a ticket key, such as owner/repo#65 or ENG-123");
            }
            let node = layout.fill("repo", &facts)?;
            let at = promote::Promote {
                store: &store,
                checkout: &checkout,
                layout: &layout,
                node: &node,
                actor: &actor,
                push: &link_push(false),
            };
            promote::promote(&at, &id, ticket, promote::Filing {
                body,
                vars,
                fields,
                dir: cwd.to_string_lossy().into_owned(),
            })?
        }
        TodoCommand::Move {
            id,
            parent,
            top,
            order,
            overwrite_tracker,
        } => move_todo(
            &store,
            &id,
            parent,
            top,
            order,
            &link_push(overwrite_tracker),
        )?,
        TodoCommand::Sync {
            ticket: Some(key), ..
        } => {
            let key = key.trim();
            if key.is_empty() {
                bail!("--ticket needs a ticket key, such as owner/repo#65 or ENG-123");
            }
            if place == Place::Global {
                bail!("devkit todo sync --ticket runs in a checkout of the ticket's repository");
            }
            let node = layout.fill("repo", &facts)?;
            if let Some(key) = tickets::sync_ticket(&store, &cwd, &node, key)? {
                writes = true;
                println!("devkit todo: ticket {key} synced");
            }
        }
        TodoCommand::Sync {
            background,
            dry_run,
            result_file,
            tickets_only,
            ticket: None,
        } => {
            let replica = if tickets_only {
                None
            } else {
                Store::sync_replica(&cwd)?
            };
            let replica_current = match replica.as_ref().filter(|_| !dry_run) {
                Some(replica) => sync::run(replica, background, result_file.as_deref())?,
                None => dry_run || store.replica_pulled(),
            };
            // Tickets merged into a replica that missed the server's changes
            // would overwrite them at the next sync.
            let tickets = match (background, tickets_only, &place) {
                (_, _, Place::Global) | (true, false, _) => None,
                (true, true, _) if !replica_current => None,
                (true, true, _) => {
                    let node = layout.fill("repo", &facts)?;
                    tickets::sync_in_background(&store, &cwd, &node, &store.ticket().sync)?
                }
                (false, ..) if !replica_current => {
                    eprintln!("devkit todo: tickets not synced: the store's sync did not complete");
                    None
                }
                (false, ..) => {
                    let node = layout.fill("repo", &facts)?;
                    let report = tickets::sync_tickets(&store, &cwd, &node, dry_run)?;
                    if report.is_some() && !dry_run {
                        tickets::record_success(&node);
                    }
                    report
                }
            };
            match tickets {
                Some(report) if !dry_run => {
                    let changed = report.added + report.changed > 0;
                    match replica.as_ref().filter(|_| changed && !background) {
                        Some(replica) => {
                            sync::run(replica, false, result_file.as_deref())?;
                        }
                        None => writes = changed,
                    }
                    println!(
                        "devkit todo: tickets synced: {} added, {} changed",
                        report.added, report.changed
                    );
                }
                Some(_) => {}
                None if replica.is_none() && !background && !tickets_only => {
                    eprintln!("devkit todo: this todo store has no sync target")
                }
                None => {}
            }
        }
        TodoCommand::Context(_)
        | TodoCommand::Hold { .. }
        | TodoCommand::Schema { .. }
        | TodoCommand::Completions { .. } => {
            unreachable!("answered before the store is opened")
        }
        TodoCommand::Purge { id } => {
            if caller == Caller::Agent {
                bail!(
                    "devkit todo purge needs a person at a terminal; agents cancel instead \
                     (devkit todo cancel <id>)"
                );
            }
            let todo = resolve_id(&store, &id)?;
            store.apply(&Edit::Purge(todo.id.clone()))?;
            NativeMap::at(devkit_todo::state_dir()).forget(&todo.id)?;
        }
    }
    if writes {
        after_write(&store, &cwd);
    }
    Ok(())
}

/// After a command changed `store` from `cwd`: lands the hook writes queued
/// meanwhile and starts a background sync.
pub(crate) fn after_write(store: &Store, cwd: &Path) {
    if let Some(replica) = store.queued_replica() {
        crate::hook::todo::drain(replica.data_dir());
    }
    store.spawn_sync(cwd);
}

/// Starts a background ticket sync from `cwd` when the repository's last one
/// is older than `[ticket.sync] max_age_secs`, without waiting for it.
fn refresh_tickets(store: &Store, cwd: &Path, layout: &Layout, facts: &Facts) {
    if let Ok(node) = layout.fill("repo", facts) {
        tickets::spawn_if_stale(cwd, &node, &store.ticket().sync);
    }
}

/// The id display styles by name.
fn id_styles() -> impl clap::builder::TypedValueParser<Value = IdStyle> {
    clap::builder::PossibleValuesParser::new(IdStyle::VARIANTS)
        .map(|name| name.parse::<IdStyle>().expect("a listed style parses"))
}

/// The hold modes by name, `true` and `false` left to the config.
fn hold_modes() -> impl clap::builder::TypedValueParser<Value = HoldStop> {
    clap::builder::PossibleValuesParser::new(HoldStop::VARIANTS)
        .map(|name| name.parse::<HoldStop>().expect("a listed mode parses"))
}

/// The checkout a todo call from `holder` resolves its place and config
/// from: `checkout` itself inside a repository, else the one `holder` last
/// selected its role in while that is still a repository, else `checkout`,
/// whose place is global.
pub(crate) fn anchor(checkout: Checkout, roles: &Roles, holder: &Holder) -> Result<Checkout> {
    if node::place_of(&checkout)? != Place::Global {
        return Ok(checkout);
    }
    let Some(recorded) = roles.checkout(holder)?.filter(|dir| dir.is_dir()) else {
        return Ok(checkout);
    };
    let recorded = Checkout::at(&recorded);
    Ok(match node::place_of(&recorded)? {
        Place::Global => checkout,
        _ => recorded,
    })
}

/// What a caller whose todo lands on the global list for want of a
/// repository is told.
pub(crate) fn global_notice(dir: &Path) -> String {
    format!(
        "devkit todo: no repository found at {}; adding to the `{GLOBAL}` list. A role \
         selected in a checkout with `devkit todo role <name>` writes there from anywhere.",
        dir.display()
    )
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

/// Claims each todo for `actor`. A todo another agent holds is taken over,
/// its claim ending as handed, when `gone` says that agent's session ended or
/// has been silent past the backstop; otherwise it is refused naming the
/// holder.
fn start(
    store: &Store,
    ids: Vec<String>,
    actor: &Holder,
    gone: impl Fn(&Holder) -> bool,
    force: bool,
    push: &push::StatusPush,
) -> Result<()> {
    if !force {
        let listed = graph::with_blockers(store, store.list(&Filter::all())?)?;
        let graph = Graph::new(&listed);
        for id in &ids {
            let Some(todo) = store.resolve(id)? else {
                continue;
            };
            let open = graph.open_blockers(&todo);
            if !open.is_empty() {
                let names = open
                    .iter()
                    .map(|blocker| devkit_todo::short_id(&blocker.id))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "todo {} is blocked by {names}",
                    devkit_todo::short_id(&todo.id)
                );
            }
        }
    }
    apply_checked(store, ids, Some(push), |todo| {
        Ok((
            false,
            match transition(&todo.status, StatusKind::InProgress, actor) {
                Err(Claimed { by }) if !by.is_human() && gone(&by) => Edit::TakeOver {
                    id: todo.id,
                    from: by,
                    actor: actor.clone(),
                },
                Err(claimed) => return Err(claimed.into()),
                Ok(_) => Edit::SetStatus {
                    id: todo.id,
                    to: StatusKind::InProgress,
                    actor: actor.clone(),
                },
            },
        ))
    })
    .map(drop)
}

/// Completes or cancels `ids`, then names the offered todos that became ready.
fn finish(
    fence: &Fence,
    ids: Vec<String>,
    to: StatusKind,
    actor: &Holder,
    push: &push::StatusPush,
) -> Result<()> {
    let store = fence.store;
    let before = offered_ids(fence)?;
    let closed = set_status(store, ids, to, actor, Some(push))?;
    if !store.config().print_unblocked || closed.is_empty() {
        return Ok(());
    }
    let offered = offered_todos(&fence.reread())?;
    let graph = Graph::new(&offered);
    let ready = graph.unblocked_by(&closed.iter().map(String::as_str).collect::<Vec<_>>());
    let names = ready
        .iter()
        .filter(|todo| before.contains(&todo.id))
        .map(|todo| devkit_todo::short_id(&todo.id))
        .collect::<Vec<_>>()
        .join(" ");
    if !names.is_empty() {
        println!("unblocked: {names}");
    }
    Ok(())
}

/// Full ids the ticket fence offers, before a change.
fn offered_ids(fence: &Fence) -> Result<BTreeSet<String>> {
    Ok(offered_todos(fence)?
        .into_iter()
        .filter(|todo| fence.offers(todo))
        .map(|todo| todo.id)
        .collect())
}

/// The todos the ticket fence offers this caller, with the blockers the
/// listing itself does not hold.
fn offered_todos(fence: &Fence) -> Result<Vec<Todo>> {
    let offered = fence
        .layout
        .fence(fence.todos()?.to_vec())
        .into_iter()
        .filter(|todo| fence.offers(todo))
        .collect();
    graph::with_blockers(fence.store, offered)
}

/// Prints the frontier, critical-path todos first.
fn ready(fence: &Fence, style: IdStyle) -> Result<()> {
    let offered = offered_todos(fence)?;
    let names = names::names(style, &offered, fence.checkout.dir());
    let plan = ticket_plan(fence, &offered, None);
    for todo in plan.frontier() {
        let mark = if plan.critical.iter().any(|on| on.id == todo.id) {
            "  *"
        } else {
            ""
        };
        println!(
            "{}  {}{mark}",
            devkit_common::ui::printable(&names.id(&todo.id)),
            devkit_common::ui::printable(names.text(todo))
        );
    }
    Ok(())
}

/// Prints the ticket's batches, or their Mermaid source.
fn plan(
    fence: &Fence,
    agents: Option<std::num::NonZeroUsize>,
    mermaid: bool,
    expand: bool,
    style: IdStyle,
) -> Result<()> {
    let offered = offered_todos(fence)?;
    let names = names::names(style, &offered, fence.checkout.dir());
    let plan = ticket_plan(fence, &offered, agents);
    let root = fence
        .layout
        .ticket_root(fence.facts, fence.store.config().ready_outside_ticket)
        .unwrap_or_else(|| fence.facts.values.get("repo").cloned().unwrap_or_default());
    let text = if mermaid {
        render::render_mermaid(&plan, &names)
    } else {
        let work = fence.work(&offered, expand)?;
        render::render_plan(&root, &plan, &work, &names)
    };
    print!("{text}");
    Ok(())
}

/// The plan over the todos the ticket fence offers.
fn ticket_plan<'a>(
    fence: &Fence,
    offered: &'a [Todo],
    cap: Option<std::num::NonZeroUsize>,
) -> devkit_todo::graph::Plan<'a> {
    // Blockers fetched for status sit in `offered` and are not members.
    Graph::new(offered).plan(|todo| fence.offers(todo), cap)
}

/// What a caller is offered: the ticket's nodes, or the caller's own default
/// listing when there is no ticket, and the synced ticket the caller's
/// worktree record names.
struct Fence<'a> {
    store: &'a Store,
    layout: &'a Layout,
    facts: &'a Facts,
    role: &'a str,
    checkout: &'a Checkout,
    /// Every todo in the store, read once.
    todos: std::cell::OnceCell<Vec<Todo>>,
    /// The ticket todos worktrees of this repository record, read once.
    workspaces: std::cell::OnceCell<Vec<tickets::TicketWorkspace>>,
}

impl<'a> Fence<'a> {
    fn new(
        store: &'a Store,
        layout: &'a Layout,
        facts: &'a Facts,
        role: &'a str,
        checkout: &'a Checkout,
    ) -> Self {
        Self {
            store,
            layout,
            facts,
            role,
            checkout,
            todos: std::cell::OnceCell::new(),
            workspaces: std::cell::OnceCell::new(),
        }
    }

    /// This fence over the store as it stands now, for a caller that changed
    /// it. A todo's status leaves the ticket workspaces as they were.
    fn reread(&self) -> Self {
        Self {
            todos: std::cell::OnceCell::new(),
            workspaces: self.workspaces.clone(),
            ..*self
        }
    }

    /// Every todo in the store.
    fn todos(&self) -> Result<&[Todo]> {
        if let Some(todos) = self.todos.get() {
            return Ok(todos);
        }
        let todos = self.store.list(&Filter::all())?;
        Ok(self.todos.get_or_init(|| todos))
    }

    /// The ticket todos worktrees of this repository record. A store that
    /// cannot be read has none.
    fn workspaces(&self) -> &[tickets::TicketWorkspace] {
        self.workspaces.get_or_init(|| {
            let todos = self.todos().unwrap_or_default();
            if !todos.iter().any(|t| t.ticket.is_some()) {
                return Vec::new();
            }
            tickets::ticket_workspaces(self.checkout, self.layout, todos)
        })
    }

    /// Whether the fence offers `todo` to this caller.
    fn offers(&self, todo: &Todo) -> bool {
        let wide = self.store.config().ready_outside_ticket;
        match self.layout.ticket_root(self.facts, wide) {
            Some(root) => {
                self.layout
                    .offered(todo.project.as_deref(), &root, self.facts, self.role)
                    || self
                        .workspaces()
                        .iter()
                        .any(|w| w.here && w.todo == todo.id)
            }
            None => self
                .layout
                .visible(self.facts, self.role)
                .is_ok_and(|filter| filter.matches(todo.project.as_deref())),
        }
    }

    /// The work under each synced ticket among `shown`, each ticket's todos
    /// listed when `expand`.
    fn work(&self, shown: &[Todo], expand: bool) -> Result<render::TicketWork> {
        let workspaces: Vec<_> = self
            .workspaces()
            .iter()
            .filter(|w| shown.iter().any(|t| t.id == w.todo))
            .collect();
        if workspaces.is_empty() {
            return Ok(render::TicketWork::default());
        }
        Ok(tickets::ticket_work(&workspaces, self.todos()?, expand))
    }
}

/// The todo `id` names, its full id included, or the error `no todo <id>`.
fn resolve_id(store: &Store, id: &str) -> Result<Todo> {
    store
        .resolve(id)?
        .ok_or_else(|| anyhow::anyhow!("no todo {id}"))
}

/// Each id resolved to the full id the store holds.
fn resolve_ids(store: &Store, ids: &[String]) -> Result<Vec<String>> {
    ids.iter()
        .map(|id| resolve_id(store, id).map(|todo| todo.id))
        .collect()
}

/// Records `by` as blockers of `id`, or with `add` false removes them,
/// refusing an edge that would close a cycle. The edges between two ticket
/// todos that `push` takes to the tracker are written there one by one,
/// after the others have changed, and the first that fails stops the rest,
/// naming the blockers done and not done. An edge the todo already has, or
/// already lacks, stays local.
fn link(store: &Store, id: &str, by: &[String], add: bool, push: &push::LinkPush) -> Result<()> {
    let todo = resolve_id(store, id)?;
    let blockers = by
        .iter()
        .map(|b| resolve_id(store, b))
        .collect::<Result<Vec<_>>>()?;
    if add {
        let ids: Vec<String> = blockers.iter().map(|b| b.id.clone()).collect();
        refuse_cycle(store, Some(&todo.id), &ids)?;
    }
    let mut pushed = Vec::new();
    let mut local = Vec::new();
    for blocker in blockers {
        if todo.blocked_by.contains(&blocker.id) == add {
            local.push(blocker.id);
            continue;
        }
        match push.reaches(store, PushKind::Blocking, &todo, &blocker)? {
            true => pushed.push(blocker),
            false => local.push(blocker.id),
        }
    }
    let mut done: Vec<&str> = local.iter().map(|b| devkit_todo::short_id(b)).collect();
    if !local.is_empty() {
        let (id, by) = (todo.id.clone(), local.clone());
        store.apply(&match add {
            true => Edit::Block { id, by },
            false => Edit::Unblock { id, by },
        })?;
    }
    for (at, blocker) in pushed.iter().enumerate() {
        match push::push_block(store, &todo, blocker, add, push) {
            Ok(true) => done.push(devkit_todo::short_id(&blocker.id)),
            Ok(false) => {}
            Err(err) if by.len() == 1 => return Err(err),
            Err(err) => {
                let not_done: Vec<&str> = pushed[at..]
                    .iter()
                    .map(|b| devkit_todo::short_id(&b.id))
                    .collect();
                let done = match done.is_empty() {
                    true => "(none)".to_string(),
                    false => done.join(" "),
                };
                return Err(err.context(format!("done: {done}; not done: {}", not_done.join(" "))));
            }
        }
    }
    Ok(())
}

/// Refuses a new child under a ticket todo while its repository pushes
/// parents, changed from a checkout of that repository: that child is a
/// sub-issue, made with `ticket create` or `todo promote`. From elsewhere the
/// child stays local.
fn refuse_under_a_ticket(store: &Store, parent: &Todo, push: &push::LinkPush) -> Result<()> {
    match &parent.ticket {
        Some(ticket) if push.pushes(store, PushKind::Parent, parent) => bail!(
            "todo {} is ticket {}, and with \"parent\" in [ticket.sync] push a todo under it is \
             a sub-issue: file one with `devkit ticket create`, or turn an existing todo into a \
             ticket with `devkit todo promote`",
            devkit_todo::short_id(&parent.id),
            ticket.key
        ),
        _ => Ok(()),
    }
}

/// Refuses when adding `by` to `id` would close a cycle. `id` absent is a
/// todo not added yet, so only a blocker naming itself can cycle.
fn refuse_cycle(store: &impl TodoStore, id: Option<&str>, by: &[String]) -> Result<()> {
    let Some(id) = id else {
        return Ok(());
    };
    let todos = store.list(&Filter::all())?;
    let Some(cycle) = graph::cycle_through(&todos, id, by) else {
        return Ok(());
    };
    let names = cycle
        .iter()
        .map(|id| devkit_todo::short_id(id))
        .collect::<Vec<_>>()
        .join(" -> ");
    let blockers = by
        .iter()
        .map(|id| devkit_todo::short_id(id))
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "blocking {} on {blockers} would close the cycle {names}",
        devkit_todo::short_id(id)
    );
}

/// Applies the status change and returns the full ids that moved from open
/// to closed. A todo already closed is not among them.
fn set_status(
    store: &Store,
    ids: Vec<String>,
    to: StatusKind,
    actor: &Holder,
    push: Option<&push::StatusPush>,
) -> Result<Vec<String>> {
    let closing = matches!(to, StatusKind::Completed | StatusKind::Cancelled);
    apply_checked(store, ids, push, |todo| {
        let was_open = matches!(
            todo.status.kind(),
            StatusKind::Pending | StatusKind::InProgress
        );
        transition(&todo.status, to, actor)?;
        Ok((was_open && closing, Edit::SetStatus {
            id: todo.id,
            to,
            actor: actor.clone(),
        }))
    })
}

/// Applies the edit `edit_for` makes of each todo, after checking every id,
/// so a batch with one unknown or refused todo changes nothing. Returns the
/// full ids whose pair was marked.
///
/// With `push`, the todos it pushes move their tickets one by one after the
/// others have changed, and the first that fails stops the rest, naming
/// which todos changed and which did not.
fn apply_checked(
    store: &Store,
    ids: Vec<String>,
    push: Option<&push::StatusPush>,
    edit_for: impl Fn(Todo) -> Result<(bool, Edit)>,
) -> Result<Vec<String>> {
    let mut checked = Vec::new();
    for id in &ids {
        let todo = resolve_id(store, id)?;
        let (keep, edit) = edit_for(todo.clone())?;
        checked.push((todo, keep, edit));
    }
    let (pushed, local): (Vec<_>, Vec<_>) = checked
        .into_iter()
        .partition(|(todo, ..)| push.is_some_and(|push| push.pushes(store, todo)));
    let mut marked = Vec::new();
    let mut changed = Vec::new();
    for (todo, keep, edit) in local {
        store.apply(&edit)?;
        changed.push(devkit_todo::short_id(&todo.id).to_string());
        if keep {
            marked.push(todo.id);
        }
    }
    let Some(push) = push else {
        return Ok(marked);
    };
    for (at, (todo, keep, edit)) in pushed.iter().enumerate() {
        let applied = match push::push_status(store, todo, edit, push) {
            Ok(applied) => applied,
            Err(err) if ids.len() == 1 => return Err(err),
            Err(err) => {
                let not_done: Vec<&str> = pushed[at..]
                    .iter()
                    .map(|(todo, ..)| devkit_todo::short_id(&todo.id))
                    .collect();
                let done = if changed.is_empty() {
                    "(none)".to_string()
                } else {
                    changed.join(" ")
                };
                return Err(err.context(format!("done: {done}; not done: {}", not_done.join(" "))));
            }
        };
        if applied {
            changed.push(devkit_todo::short_id(&todo.id).to_string());
            if *keep {
                marked.push(todo.id.clone());
            }
        }
    }
    Ok(marked)
}

/// Moves `id` under `parent`, to the top level with `top`, or only to
/// `order`. A ticket todo's move between ticket parents reaches the tracker
/// as `push` allows; with parents pushing from the ticket todo's
/// repository, a ticket todo and a todo that is not one never nest under
/// each other. A ticket todo under a ticket parent never moves under a
/// ticket of another tracker or repository, which the next sync would undo.
/// A move to the parent `id` already has stays local.
fn move_todo(
    store: &Store,
    id: &str,
    parent: Option<String>,
    top: bool,
    order: Option<i64>,
    push: &push::LinkPush,
) -> Result<()> {
    if parent.is_none() && !top {
        let Some(order) = order else {
            bail!("name a new --parent, --top or an --order");
        };
        let id = resolve_id(store, id)?.id;
        return store.apply(&Edit::Reorder { id, order }).map(drop);
    }
    let todo = resolve_id(store, id)?;
    let parent = parent.map(|p| resolve_id(store, &p)).transpose()?;
    let edit = Edit::Move {
        id: todo.id.clone(),
        parent: parent.as_ref().map(|p| p.id.clone()),
        order,
    };
    if parent.as_ref().map(|p| &p.id) == todo.parent.as_ref() {
        return store.apply(&edit).map(drop);
    }
    let Some(ticket) = &todo.ticket else {
        if let Some(parent) = &parent {
            refuse_under_a_ticket(store, parent, push)?;
        }
        return store.apply(&edit).map(drop);
    };
    if let Some(step) = parent.as_ref().filter(|p| p.ticket.is_none())
        && push.pushes(store, PushKind::Parent, &todo)
    {
        bail!(
            "todo {} is ticket {}, and with \"parent\" in [ticket.sync] push its parent is a \
             ticket too; todo {step} is not one, so it cannot hold it: turn todo {step} into a \
             ticket with `devkit todo promote`, or file a new sub-issue with `devkit ticket \
             create`",
            devkit_todo::short_id(&todo.id),
            ticket.key,
            step = devkit_todo::short_id(&step.id)
        );
    }
    let before = todo
        .parent
        .as_deref()
        .map(|p| store.get(p))
        .transpose()?
        .flatten()
        .filter(|p| p.ticket.as_ref().is_some_and(|t| t.same_tracker(ticket)));
    if let Some(old) = before.as_ref().and_then(|p| p.ticket.as_ref())
        && let Some(new) = parent.as_ref().and_then(|p| p.ticket.as_ref())
        && !new.same_tracker(ticket)
    {
        bail!(
            "todo {} is ticket {key}, a sub-issue of {old}; {new} is a ticket of another \
             tracker or repository, so it cannot become {key}'s parent there, and the next sync \
             would put {key} back under {old}. Change {key}'s parent on the tracker instead",
            devkit_todo::short_id(&todo.id),
            new = new.key,
            key = ticket.key,
            old = old.key
        );
    }
    let linked = parent
        .as_ref()
        .filter(|p| p.ticket.is_some())
        .or(before.as_ref());
    match linked {
        Some(other) if push.reaches(store, PushKind::Parent, &todo, other)? => {
            let baseline = before
                .as_ref()
                .and_then(|p| p.ticket.as_ref())
                .map(|t| t.key.as_str());
            let parent = parent.as_ref().filter(|p| p.ticket.is_some());
            push::push_parent(store, &todo, parent, baseline, &edit, push).map(drop)
        }
        _ => store.apply(&edit).map(drop),
    }
}

fn list(
    fence: &Fence,
    args: &ListArgs,
    cwd: &Path,
    role: &ResolvedRole,
    viewer: &Holder,
    style: IdStyle,
) -> Result<()> {
    let (store, layout, facts) = (fence.store, fence.layout, fence.facts);
    let filter = match (&args.scope, &args.node, &args.subtree, args.all) {
        (Some(scope), ..) => Filter::exact([layout.fill(scope, facts)?]),
        (_, Some(n), ..) => Filter::exact([n.clone()]),
        (_, _, Some(n), _) => Filter {
            nodes: vec![NodeMatch::Subtree(n.clone())],
        },
        (_, _, _, true) => Filter::all(),
        _ => layout.visible(facts, &role.name)?,
    };
    if args.sync {
        match store.sync(cwd, sync::FRESH_WAIT) {
            SyncOutcome::Failed(reason) => eprintln!("{}", sync::failure_text(&reason)),
            SyncOutcome::Refused(reason) => eprintln!("devkit todo: {reason}; listing local todos"),
            SyncOutcome::StillRunning => eprintln!(
                "devkit todo: sync still running after {} seconds; listing what this machine has",
                sync::FRESH_WAIT.as_secs()
            ),
            SyncOutcome::Done | SyncOutcome::NoTarget => refresh_tickets(store, cwd, layout, facts),
        }
    }
    let todos = if args.all || args.subtree.is_some() {
        layout
            .fence(fence.todos()?.to_vec())
            .into_iter()
            .filter(|todo| filter.matches(todo.project.as_deref()))
            .collect()
    } else {
        store.list(&filter)?
    };
    let graph_todos = graph::with_blockers(store, todos.clone())?;
    if args.json {
        let rows: Vec<Value> = todos.iter().filter_map(alacritree_json).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    let nodes = list_nodes(&filter, &todos);
    let graph = Graph::new(&graph_todos);
    let own = &role.node;
    let work = fence.work(&todos, args.expand)?;
    let names = names::names(style, &graph_todos, cwd);
    let notes = render::Notes {
        graph: &graph,
        show_ready: &|todo| fence.offers(todo) && (!facts.subagent || todo.node() == own),
        work: &work,
        names: &names,
    };
    print!("{}", render::render_full(&nodes, &todos, viewer, &notes));
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
    let checkout = anchored(&Checkout::at(&hook::record::payload_cwd(&payload)), &viewer);
    let cwd = checkout.dir().to_path_buf();
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
    refresh_tickets(&store, &cwd, &layout, &facts);
    let fence = Fence::new(&store, &layout, &facts, &role.name, &checkout);
    let all = fence.todos().ok()?;
    let todos: Vec<Todo> = all
        .iter()
        .filter(|t| visible.matches(t.project.as_deref()))
        .cloned()
        .collect();
    let graph_todos = graph::with_blockers(&store, todos.clone()).ok()?;
    let siblings = layout.fence(all.to_vec()).into_iter().filter(|t| {
        filter.matches(t.project.as_deref())
            && !visible.matches(t.project.as_deref())
            && layout.is_other_session(t.node(), &facts)
    });
    let nodes = list_nodes(&visible, &todos);
    let graph = Graph::new(&graph_todos);
    let work = fence.work(&todos, false).ok()?;
    let names = names::names(store.config().ids, &graph_todos, &cwd);
    let notes = render::Notes {
        graph: &graph,
        show_ready: &|todo| fence.offers(todo) && (!facts.subagent || todo.node() == own),
        work: &work,
        names: &names,
    };
    let mut lists = render::render_lists(&nodes, &todos, &viewer, &notes);
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
