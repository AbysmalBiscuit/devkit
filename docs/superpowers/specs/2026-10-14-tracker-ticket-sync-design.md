# Tracker ticket sync

## Problem

The tracker (Linear or GitHub) is where a person keeps tickets. devkit's todo store is where the work under a ticket lives: workflow stages, subtasks, review findings, too small or short-lived to be tickets. The two do not meet. A ticket's todo exists only when someone copies it in by hand, with its dependencies pasted into the title as text, so:

- `devkit todo ready`, `plan` and the critical path cannot span tickets and the work under them.
- An agent's todos do not say which ticket they belong to.
- A ticket's blocking links and sub-issues never become todo edges, and someone recreates them by hand.

## Goal

`devkit todo sync` brings the tickets a repository's rules select into the todo store, one todo per ticket, kept current without polling. When an agent starts or finishes a ticket's todo, the ticket's status moves, if the repository configures it.

This design is the first of three pieces:

1. **This design**: one-way sync, tracker to devkit, plus status moves on todo events.
2. **A tracker field model** (separate design): per destination, which fields exist (a GitHub repo with or without a project, a Linear team with or without projects), which an agent must fill or explicitly mark as none, declared in config or a template, and checked on `ticket create`, `ticket edit`, and in a `PreToolUse` gate on tracker MCP calls.
3. **Two-way sync** (separate design, opt-in): writes body, labels and other metadata back through piece 2's field model.

## Non-goals

- Writing anything to the tracker except the status moves `[ticket.events]` configures.
- Tracker webhooks or any other push from the tracker.
- A background poller.
- A ticket that leaves the rules while still open (reassigned, label removed). Its todo stays, and sync keeps following its status while the todo is open; nothing deletes or detaches it. Anything more is left for later.
- User-scripted todo hooks under `[hooks]`.
- Per-team Linear status maps. One map per repository; a later design can split it if a repository's teams genuinely disagree.

## Anti-goals

Each of these is a failure even if the sync otherwise works:

- **A tracker write nobody configured.** Sync only reads. The only writes are `[ticket.events]` moves.
- **A duplicate.** A ticket has exactly one todo per store, whichever machine, session or person synced it. Re-syncing updates it.
- **Lost local work.** Sync never deletes or rewrites the todos under a ticket's todo, and changes a ticket todo's status only when the ticket's own status changed since the last sync.
- **A silent wrong guess or a mismap.** A tracker status missing from the map, a configured status name the tracker does not have, or a rule devkit cannot evaluate stops the sync with an error naming it. A ticket lands on the wrong status, parent or blocker only through config the person wrote.
- **No room for custom statuses.** Trackers let teams define their own statuses. devkit names no status in code; every one comes from config.

## Design

### Which tickets: match rules

Per repository, in its `devkit.toml`. A list of tables; a ticket syncs when any table matches, and a table matches when all its conditions hold. The conditions:

- **assignee**: the authenticated account, a named user, or none (unassigned).
- **project role**: the ticket's project has the authenticated account as lead, or as lead or member. Linear only; GitHub has no project roles.
- **labels**: the ticket carries every listed label.

The monorepo's rules read as: assigned to me; or unassigned and in a project I lead. devkit's read as: every issue in its repository.

Key names, and whether a condition takes one value or a list, are left to the builder. The JSON schema (`devkit schema`) and a doctest on the config type document them.

### What a sync fetches

1. **Rule matches**: every ticket a rule selects, except those in a status the map sends to completed or cancelled. The fetch filters on those closed names only, so a ticket in a status the map does not name is fetched and fails the sync with that name. Closed tickets are never fetched by the rules, so the first sync imports no history.
2. **Open ticket todos**: every ticket whose todo is still pending or in progress, re-read by id whatever the rules say, so a close, a cancel or a reassignment reaches its todo. A todo already completed or cancelled is not re-read; a reopened ticket comes back through the rule fetch and is found by its id.

Reading filtered ticket lists, blocking links and sub-issues is new tracker code: devkit today reads one ticket at a time, with no blocking links on either tracker and no parent on GitHub.

### Status map

Per repository, config names each tracker status and the todo status it maps to: pending, in progress, completed or cancelled. The map takes any status name a tracker allows, and any number of names may map to one todo status ("Backlog", "Triage" and "Todo" to pending; "In progress" and "In review" to in progress), so a team's own workflow needs no code change.

`[ticket.events]` moves tickets between the same names, so a board's statuses are written in one place. Whether the map lives under `[ticket]`, `[todo]`, or beside the tracker's own table is the builder's call.

**Validation.** Each sync checks the map against the tracker: a fetched ticket in an unmapped status, or a mapped name that no status of the fetched tickets' teams (Linear) or of the status field (GitHub) has, is an error naming it. Nothing is written in that case.

**Linear**: the statuses are the workflow states of the teams the fetched tickets belong to, checked against their union.

**GitHub**: an issue's open/closed state and its project status field are independent, and an issue outside the project has no field value. So:

- A closed issue maps by its close reason, whatever the field says: completed, or `not planned` to cancelled.
- An open issue maps by its `[github] status_field` option. An open issue with no value (not in the project, or no option set) uses the name `""`, which the map may list, as `[ticket.events]` `from` lists already do; unlisted, it is an error like any unmapped status.
- Without `[github] project`, every open issue maps through `""`. `[ticket.events]` cannot move such a repository's issues, and no new write path closes or reopens issues; a board that wants status moves configures a project. devkit's own board, project 3, is not in its `devkit.toml` yet.

### The ticket's todo

- **Where:** on the repository's node (the `repo` scope).
- **Identity:** the todo records its ticket (tracker and key) and the tracker status name it last synced. Both are new fields on `Todo`, stored by all four backends: properties on taskchampion, keys in the builtin JSON, columns on Postgres added the way `blocked_by` was (`ADD COLUMN IF NOT EXISTS`, the `COLUMNS` list, the row mapping, and every SQL function that returns a row), and on Supabase the same tolerance `blocked_by` has for an older database that lacks the columns.
- **One todo per ticket:** a ticket todo's id is derived from the ticket, a name-based (v5) uuid over the store's root, the tracker and the ticket key, not drawn at random. Two syncs that race, on one machine, on two taskchampion replicas, or from two people sharing a Postgres or Supabase root, create the same id: taskchampion merges the two creations into one task, and Postgres and Supabase insert it with `ON CONFLICT (id) DO NOTHING` and then update. The builtin store, whose ids are counters, finds an existing todo for the ticket under its file lock and returns it. Every backend's `add` of a ticket that already has a todo returns that todo and changes nothing. Where two repositories' rules match one ticket, the todo stays on the node of the repository that created it.
- **Text:** the ticket's key and title. The exact format is the builder's call; it must let a person and an agent recognise the ticket at a glance.
- **Status:** a sync changes the todo's status only when the ticket's tracker status differs from the last-synced name; it then writes the mapped todo status and records the new name. A todo an agent started or finished, on a ticket whose tracker status has not moved, stays as the agent left it.
- **Blocking edges:** each tracker blocking link between two synced tickets becomes a todo blocking edge, through the same store edits `devkit todo block` uses. A link to a ticket outside the rules is not an edge. The builder decides whether it is noted on the todo.
- **Sub-issues:** a synced sub-issue's todo is a child of its parent ticket's todo.
- **Changes on re-sync:** text, edges and parent follow the ticket; status follows it as above. Edges and children that devkit did not create from the tracker are left as they are.

### The tracker holder

Sync acts as a dedicated holder, `tracker`, never as `human` and never as the session that ran it. In `crates/devkit-todo` `Holder::covers`, `tracker` covers every holder, as `human` does, while `is_human()` stays false. Postgres's `devkit.todo_covers` mirrors it. That gives:

- A ticket todo sync moves to in progress is held by `tracker`. An agent's or a person's `devkit todo start` on it takes the claim over (`transition` hands a non-human claimant's todo to any actor it covers), and finishing it fires the completion event.
- When the ticket closes, sync completes or cancels the todo even while an agent holds it, crediting that agent as the finisher, as `transition` already does for a covering actor.

### Linking a ticket to its work

A workspace `workspace setup <ticket>` created knows its ticket through its `IssueRecord`. That record is the link: a workspace node belongs to the ticket its record names. No new field on the work todos, no parent or blocking edge from them to the ticket's todo.

- **Summary by default.** Lists, `plan` and hook injections show a ticket's todo with the count of open todos in its workspace's `repo.branch` subtree. On a machine that does not hold the worktree, the record is absent and the count is left out.
- **Detail on request.** A flag on `list` and `plan`, which an agent passes like a person, expands a ticket into its workspace's todos.
- **The ready fence.** `ready` and `plan` run in a workspace whose record names a ticket offer that ticket's todo and its workspace's subtree, in place of the workspace node alone. The ticket's todo sits on the repository node, above the workspace, so without this it is never offered. `[todo] ready_outside_ticket = true` still widens the offer to the whole repository, ticket todos included.

### Freshness

devkit records each repository's last sync time. When `list`, `plan`, `ready`, or a hook injection reads the store and that time is older than a configured age, it starts a sync. `devkit todo sync` syncs on demand, beside the taskchampion sync it already runs.

At hook time the sync runs in the background only: hooks answer within their deadlines and a verdict never waits on the network. One lock covers a repository's sync, so parallel sessions' hooks start at most one fetch, and a failed sync sets a hold before the next attempt, as the taskchampion background sync does (`src/bin/devkit/todo/sync.rs`). A failed sync warns, and every read uses what the store holds. The default age is the builder's call.

### Status moves on todo events

`[ticket.events]` gains two events for a ticket's todo:

- its todo becomes in progress
- its todo becomes completed

Each takes `from` and `to` like the existing events and does nothing without config. For a board with a "Done" status, completion would be `to = "Done"`. Event names are the builder's call, in the style of `setup`, `start` and `pr_open`; `IssueEvent::ALL`, the CLI's event argument and `TicketEventsConfig::get` match events exhaustively and grow with them.

**Firing.** After the store commits a status change on a todo that has a ticket, by any actor but `tracker`, devkit spawns a detached `devkit ticket event <name> --issue <key> --dir <checkout>`, as the `start` event's hook trigger does today (`src/bin/devkit/hook/issue_event.rs`). The checkout is the repository the change was made from. A change made outside that repository, where no checkout resolves the ticket's config, fires nothing and warns. A failed move warns and never undoes the todo change.

Changes sync makes, as `tracker`, never fire, so sync and events cannot move a ticket back and forth.

When a ticket's todo counts as done is the person's workflow, not devkit's: an agent, an orchestrator, or a deploy script that sees the PR merge marks it done, and the completion event fires on that. If the tracker closes the ticket first, the next sync completes the todo and fires nothing.

### Dry run

`devkit todo sync --dry-run` prints every change a sync would make (todos added, statuses changed, edges and parents set) and writes nothing.

## Left to the builder

- Config key names and placement, within the shapes above.
- The ticket todo's text format.
- The default sync age.
- Event names for the todo events.
- Whether out-of-rules blockers are noted on a todo.
