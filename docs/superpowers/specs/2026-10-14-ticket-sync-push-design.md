# Two-way ticket sync

## Problem

Piece 1 (`2026-10-14-tracker-ticket-sync-design.md`) brings a repository's tickets into the todo store, one todo per ticket, with blocking links as todo edges and sub-issues as children. It only reads: the one write is a `[ticket.events]` status move, fired in the background and best-effort. Everything else an agent changes on a ticket's todo stays local, and the next sync reverts the relations to the tracker's.

So the edits Lev asks agents for most still go through hand-written skill steps, `gh` calls and the Linear MCP: making sub-issues (by far the most frequent), marking tickets done or cancelled, and adding blocking links "so i know in what order to do htings". Each workflow repeats those steps, and trackers without automation (no Actions, no rules) get none of it.

## Goal

When a person or an agent changes a ticket's todo locally (its status, its parent, its blocking edges), the change reaches the ticket, if the repository opts in. A change that would overwrite something the tracker changed since the last sync stops and asks first.

This is piece 3 of three, and a first slice of it: status, parent and blocking. It is meant to be used and corrected before later slices add more.

## Non-goals

- **Comments, including a closing reason.** A todo has no comments, so there is nothing local to sync from. The written "why" behind a Done stays with the agent or workflow step that posts it today.
- **Title, body, labels, assignee and other fields.** Later slices (see the end). `ticket edit` keeps writing them meanwhile.
- **Promoting a todo by guessing.** No heuristic on a todo's size, text or depth decides whether it is a ticket.
- **Reproducing workcell's progress comment or its ask-the-human channel.** Those are a workflow tool's, and stay in it.
- **A pending-push queue.** A change that cannot reach the tracker fails; see Failures.
- Tracker webhooks, a background poller, or any tracker push into devkit (piece 1's non-goals stand).

## Anti-goals

Each of these is a failure even if the sync otherwise works:

- **A tracker change silently overwritten.** Trackers move fields on their own (Linear moved a ticket to In Review when a PR linked it; a GitHub project workflow set a closed issue back to In progress), and people edit tickets in the UI while agents work. A local change never lands over a tracker value devkit has not seen.
- **A step reaching the tracker unasked.** An agent's working todos become tickets only through an explicit `todo promote`, and never become links or status moves.
- **A duplicate ticket.** A retried create or promote does not make a second ticket.
- **A write the repository did not opt into.**
- **A relation silently replaced.** Adding "related" on Linear once replaced an existing "blocked by" in an agent's session, so a write never lands over another relation between the same pair, and a GitHub sub-issue never takes a new parent without the override.
- **A local relation change the next sync reverts.**

## Design

### Opt-in

Per repository, in its `devkit.toml`, beside piece 1's `[ticket.sync]` rules: a list of what pushes, any of `status`, `parent` and `blocking`. Empty by default, so a repository that only syncs keeps piece 1's behaviour, except that a relation edit between two ticket todos is refused rather than reverted by the next sync (see Whose config). Key name and placement are the builder's call; the config type's doctest and `devkit schema` document it.

### What pushes

Only **ticket todos**, the todos that carry a `TicketRef`. Of their edges, only those joining two ticket todos: a blocking edge or parent link between a ticket todo and any other todo stays local, as piece 1 already leaves it.

The workspace todos under a ticket (an agent's steps, the harness task-tool mirror) live on the workspace node and are linked to the ticket only through the workspace's issue record, so nothing here touches them.

**Entry points.** The `devkit todo` verbs, and the `todo` shim that calls them. The MCP server has no todo tools, and the native-tool mirror only ever creates and changes todos without a ticket. The foreground push runs in those verbs, never in the store's apply path that the hooks share, so no hook waits on the tracker.

**Whose config.** A write lands on one ticket: the one whose status changes, the child gaining or losing a parent, the ticket gaining or losing a blocker. That ticket's repository's config decides whether it pushes, resolved from the working directory the way #356's events do; devkit has no lookup from a todo's node back to a checkout.

- **Parent and blocking** changes between two ticket todos are allowed only from a checkout of their repository, whatever the opt-in, and refused elsewhere, because piece 1's next sync would revert them. This also applies to a repository that has not opted in: its ticket-to-ticket relation edits are refused instead of silently reverting later.
- **Status** changes on a ticket todo made outside its repository commit locally and warn that the ticket did not move, as #356 does.
- An edge between tickets on two different trackers stays local.

### Status

A status change on a ticket todo pushes when `status` is in the opt-in list and `[ticket.events]` configures an event for it:

| Local change | Event |
|---|---|
| `devkit todo start` | piece 1's in-progress event (#356) |
| `devkit todo done` | piece 1's completed event (#356) |
| `devkit todo cancel` | a new cancelled event |

The event's `to` names the tracker status, so no status name lives in code. `stop` and `undone` have no event and stay local; Lev's history has no reopen requests.

**GitHub.** An issue's open state is separate from its project status. With `status` pushing, `done` also closes the issue as completed and `cancel` closes it as not planned (`closeIssue` with `stateReason`), whether or not an event is configured. The close comes first, since it is the meaningful write and a project workflow may set the field on close; the event, when configured, then moves the project status. This lifts piece 1's "no new write path closes or reopens issues" for opted-in repositories only.

**Several todos in one command.** `devkit todo done a b c` pushes each ticket todo in turn. A conflict or failure stops at that todo: earlier todos stand on both sides, later ones are untouched, and the message names which were done and which were not.

**Foreground.** With `status` pushing, the event runs before the command returns, not in the detached background firing #356 builds, because the conflict check below needs the tracker's answer. A ticket todo's status change therefore waits on the network, about a second, which Lev accepted. Step todos stay local and instant. Without `status` in the list, #356's background, best-effort firing is unchanged.

### Parent

With `parent` pushing:

- `devkit todo move <ticket todo> --parent <ticket todo>` makes the first ticket a sub-issue of the second: a GitHub sub-issue, a Linear `parentId`.
- `--top` removes the ticket's tracker parent.
- Moving a ticket todo under a todo that is not a ticket todo is refused, naming why.
- Moving a todo that is not a ticket todo under a ticket todo is refused, as `todo add --parent` is (below).
- Children of a ticket todo that are not ticket todos and already exist when a repository opts in stay as they are, local, as piece 1 leaves them. The refusals guard new links only. (My call.)

On GitHub, `addSubIssue` is sent with `replaceParent: false`; only the override flag (see The conflict check) sends `true`. `removeSubIssue` undoes it.

### Blocking

With `blocking` pushing, `devkit todo block` and `unblock` between two ticket todos add or remove the tracker's blocking link. `todo block A --by B` is GitHub's `addBlockedBy(issueId: A, blockingIssueId: B)` and Linear's `issueRelationCreate(type: blocks, issueId: B, relatedIssueId: A)`; unblock sends `removeBlockedBy` or deletes that relation. `todo add --blocked-by` naming a ticket todo, on a step, stays local.

### Tracker writes

The parent and blocking writes are piece 2's parent and blocked-by place writers. Piece 3 adds the read before the write and the store edit after it, not a second set of tracker calls. A relation changed through `ticket edit` reaches the store on the next sync, as any tracker change does.

### Making tickets

With `parent` pushing, a todo under a ticket todo is a sub-issue, so it is made as one:

- **`devkit todo add --parent <ticket todo>` is refused.** The message names `ticket create` and `todo promote`. An agent cannot make a half-filled sub-issue by accident.
- **`ticket create`** makes the sub-issue through piece 2's parent field place (`--arg <parent field>=<key>`), with piece 2's field checks, so it gets its metadata. No separate `--parent` flag: the parent is a field like any other, and a repository that wants sub-issues declares it. (Builder check; my call.)
- **The new ticket's todo.** After a create or promote in a repository with `[ticket.sync]` rules, devkit runs the ticket sync for that one ticket inline, which adds the todo (or re-keys the promoted one) with piece 1's status mapping and parent. An unmapped status is reported as a sync error, and the ticket's URL is still printed. No second todo-creation path.
- **`devkit todo promote <id>`** turns a todo into a ticket when a step turns out to be real work. It takes the same body and field arguments as `ticket create` and runs the same checks, the parent included. The ticket's title is the todo's text. Afterwards exactly one todo stands for the ticket, on the repository node with the ticket's derived id, and every edge that pointed at the old todo points at it. The old todo's children stay where they are, on the workspace node, with their edges: work under a ticket links to it through the workspace record, as piece 1 has it, never as children of the ticket's todo. The old todo is cancelled, its text replaced by the ticket's key, so the record shows what it became. How the store gets there (re-keying, or a new todo plus rewired edges) is the builder's call.
- **`devkit todo promote <id> --ticket <key>`** links the todo to a ticket that already exists, with the same re-keying and no create. It is the recovery when a create's response was lost, and the way to say a step is really ticket X.
- **Promote has no default parent.** A workspace's steps link to their ticket only through its issue record, so they have no ticket-todo ancestor to default to, and Lev asks for both shapes: a child of the current ticket, and a "sibling subissue" under the current ticket's parent. When the parent field is declared and not given, the field's level decides, as for any field, and the message names the workspace's ticket and that ticket's parent as the likely values. (Builder check; my call.)
- **`ticket create` and `todo promote` work whatever the opt-in list holds.** They are explicit tracker creates, like `ticket create` today. Only the refusals above depend on `parent` pushing. (My call.)

A retried create or promote must not make a second ticket: both reuse piece 2's duplicate handling (a client id on Linear, a search-before-retry message on GitHub), and on GitHub `promote --ticket` takes the ticket the search finds.

### The conflict check

Before writing, devkit reads the ticket's current value and compares it with the baseline:

- **Status:** the baseline is the todo's `synced_status`, as the last sync read it. The current status is read the way sync maps it (GitHub close reason or project field, Linear workflow state). A current status that differs from the baseline is a conflict unless it is in an **explicit** `from` list of the event about to fire: the person already declared moving from there fine, and trackers move tickets on their own (Linear to In Review when a PR links). So with `from = ["Todo", "In progress", "In review"]`, In review to Done passes, while a ticket someone closed or sent back to Backlog stops. An unset `from`, or `"*"`, waives nothing for a push, so the default config still stops on every tracker change. A current status outside `from` that equals the baseline keeps #356's behaviour: the todo change commits locally, and the command reports that the ticket did not move.
- **Parent:** the baseline is the ticket todo's current parent in the store (a ticket todo or none), since piece 1 makes the store mirror the tracker for relations between ticket todos. A tracker parent that is a third ticket is a conflict.
- **Blocking:** the baseline is whether the store holds the edge. The only conflict is a Linear relation of another type between the pair, or a `blocks` running the other way. A Linear pair holds one relation: `issueRelationCreate` on a pair that already has one succeeds and silently replaces it, so writing `blocks` over a `related` pair would delete the `related` link. A `blocks` relation still reads as `blocks` after the blocker is completed, so a resolved blocker is not a conflict. Both were checked against Linear's API with scratch issues while this was written.

Outcomes:

- **Current equals baseline, or a status in the explicit `from`:** write, record the new `synced_status` (for status), commit the local change.
- **Current already equals the local change:** nothing to write; record it and commit. A retry after a write whose response was lost lands here.
- **Anything else is a conflict.** Nothing is written on either side.
  - **A person at a terminal** sees the local change, the tracker's current value and the baseline, and confirms (write over the tracker) or declines (drop the local change and take the tracker's value).
  - **An agent** gets a non-zero exit whose message names the three values, tells it to ask the user, and names the override flag. The flag (something like `--overwrite-tracker`, the name is the builder's) writes the local change anyway.

Who counts as a person follows devkit's existing caller detection.

A push records the status it wrote as `synced_status`. devkit's other moves (`setup`, `start`, `pr_open`) leave it alone, so `synced_status` keeps piece 1's meaning; a repository whose events move tickets through statuses lists those statuses in the next event's `from`.

**Status equality.** On Linear, "already equals the local change" means the exact status the event's `to` names. On GitHub the closed state is the status for `done` and `cancel`: an issue closed as completed (for `done`) or not planned (for `cancel`) already equals the local change whatever the project field says, and only the field write remains, gated by the event's `from` as today. An open issue is compared by its project field. Without a configured event, the close is gated by the baseline alone: it proceeds when the issue is open, counts as done when closed with the same reason, and is a conflict when closed with another.

### Failures

- **The tracker cannot be reached, or the write fails:** the command fails and nothing changes locally. The guard makes a rerun safe.
- **The write lands and the store commit fails:** the next sync sees the ticket moved and brings the todo along.

### Sync after a push

A push updates the baseline, so the next sync finds tracker and store agreeing and changes nothing. Piece 1's rule that a todo's status changes only when the ticket's moved still holds.

## Sequencing

- #356, because status pushing builds on its todo events.
- #355, for Linear reads, and #364, for Linear creates with fields.
- #363, for piece 2's field checks and parent field that `ticket create` and `todo promote` use.

A GitHub-only first cut is possible if Linear lags.

## Later slices

To be decided after using the first slice:

- Labels as todo tags, mapped through piece 2's field declarations.
- Title (`devkit todo describe`) and body, once a todo can hold a body.
- Assignee.
- On trackers with no automation, sync moving a ticket whose last open blocker closed (Backlog to Ready), guarded by `from` like any event.

## Left to the builder

- The opt-in key's name and placement.
- The cancelled event's name, in the style of #356's.
- The override flag's name.
- How `todo promote` re-keys the todo.
