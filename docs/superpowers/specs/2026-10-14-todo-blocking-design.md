# Todo blocking and plans

## Problem

A large feature set breaks into todos that depend on each other. Today devkit records no relation between todos, so a manager or a person planning the work writes "blocked by X" into descriptions and leaves each agent to work out the order. Linear and git-based trackers, where most plans start, offer no dependency-aware "what can run now" or critical path either.

The person running the agents wants to record blocking relations once, in the terminal, and have devkit answer two questions:

- **What can be done now?** The frontier: open todos whose blockers are all closed and nobody holds. This is what a manager reads before dispatching implementers.
- **How does the plan unfold?** Batches of todos that can run in parallel, and the critical path through them, so a person or a senior agent can see how long the chain is and where to put agents.

## Who uses it

Flows centre on one manager and its team per ticket. A ticket is a workspace (`<repo>.<branch>`). The manager's role writes at workspace scope, and implementers write at agent scope, as `[todo.roles]` already allows.

- **Planning**, by the person or a senior agent: lays out todos and their blocking edges through the CLI.
- **The manager** dispatches. It reads the frontier, hands todos to implementers (one or many in parallel, its choice), and learns when finished work unblocks more.
- **Implementers** claim what they are handed, finish it, and report back. They do not pick up newly unblocked work on their own.

## Goals

- Record "A is blocked by B" between any two todos, on any node, on every backend.
- A live frontier: a todo becomes ready the moment its last open blocker closes, whatever batch a forecast put it in. Dispatch is never held to fixed batches.
- A plan view for people in a terminal: batches, critical path, and a cap on parallel agents.
- Agents see blocking state in the lists hooks already inject, without learning a new view.
- A manager is never offered ready work from outside its own ticket, unless config allows it.

## Non-goals

- Estimates, durations, dates, Gantt charts. Every todo counts as one step.
- A graphical board view (boxes per batch, see "Follow-ups").
- Importing a plan from a file. Edges are entered through the CLI, one todo at a time, as in Taskwarrior.
- Edge kinds other than "blocks" (relates-to, duplicates).
- Exposing blocking through the MCP server, which has no todo tools today.

## Anti-goals

- A frontier that leaks other tickets' work into a manager's context, pulling it into a ticket it does not own.
- A heavy project-management layer. This is a graph with a few queries over it.
- Graph logic duplicated per backend. Backends store edges; one implementation computes everything else.

## Behaviour

### The model

Each todo carries a set of **blockers**: ids of todos it waits on. The edge lives on the blocked todo only, as Taskwarrior and taskchampion store it (`dep_<uuid>` on the dependent task). "What does X block" is derived by scanning, never stored.

A blocker is **open** while it is pending or in progress. A completed, cancelled or missing blocker (purged, or never existed on this backend) counts as closed. This is taskchampion's rule (`taskchampion-3.1.0/src/replica.rs`, the dependency map), so devkit and a person's own `task ready` agree on the taskchampion backend.

A todo is **blocked** while any blocker is open, and **ready** when it is pending, unblocked and unheld.

An edge that would close a cycle is refused, with an error that names the cycle. Edges may cross nodes and tickets freely. The check reads the store before writing, so two writers racing on Postgres can still close a cycle. Reads tolerate that: todos on a cycle are never ready, and `plan` lists them under a `cycle:` line instead of dropping them.

### Recording edges

Taskwarrior-style, CLI only:

- `devkit todo add "<text>" --blocked-by <id>[,<id>...]` adds a todo with its blockers.
- `devkit todo block <id> --by <id>[,<id>...]` adds blockers to an existing todo.
- `devkit todo unblock <id> --by <id>[,<id>...]` removes them.

Ids are accepted in every form other commands accept (short uuid prefixes on uuid backends) and stored resolved to the full id.

### Seeing what is ready

- `devkit todo ready` prints the frontier for the caller's ticket: ready todos, critical-path todos first, then by how many todos each unblocks.
- The injected lists and `devkit todo list` gain blocking state in the parentheses each line already has: `(<id>, blocked by <id>, <id>)` while blocked, and `(<id>, ready)` once a todo that had blockers is unblocked. A todo with no edges is shown as today, so lists without plans do not change. A blocker on a node the listing does not show is named with its node.
- `devkit todo done <id>` and `devkit todo cancel <id>` print `unblocked: <id> ...` for every todo the change made ready. `[todo] print_unblocked` (default `true`) turns this off. For an implementer this is information only; the manager learns of it through its next injection, which the changed lists trigger.

### Ticket fence

By default the frontier offered to a caller (`ready`, the injected `ready` notes, `print_unblocked`'s list, and the todos `plan` covers) contains only todos inside the caller's ticket: the workspace node its identity fills, plus the nodes below it that the caller's default listing already shows (its own session's and its sub-agents' nodes, per `Layout::visible`). Other sessions' nodes on the same checkout stay out, so their private native-tool steps never enter a manager's frontier. Blocking edges still cross tickets, and reading a blocker's status follows the edge wherever it points. A todo outside the ticket that is blocked by work inside it is never offered.

`[todo] ready_outside_ticket` (default `false`) widens the frontier to the caller's repository: the `<repo>` node and every workspace under it, still excluding other sessions' nodes. It never reaches other repositories. A caller outside any repository has no ticket and sees the frontier of its own default listing.

A sub-agent sees `ready` notes only on todos on its own node. On parent nodes, such as the manager's plan at workspace scope, it sees `blocked by` notes but no `ready` ones, and `print_unblocked` tells it nothing beyond the ids, so implementers are not invited to pick up work the manager dispatches.

### Plans

`devkit todo plan` prints the ticket's open todos as batches:

```text
$ devkit todo plan --agents 3
devkit.feat-blocking: 12 todos, 1 done, 1 in progress, 10 open
critical path: b41c77e2 -> 9c31f6d2 -> e27d8a51 -> f30a6c29 (4 batches minimum)

batch 1  ready now
  > 8a2f4d10  Frontier, batches and critical path      in progress: claude-3f1a.implementer
  o b41c77e2  Contract-suite tests for blocking edges  *  ready
  o d0e5a913  devkit todo block / unblock                 ready

batch 2
  o 9c31f6d2  Postgres: blocked_by uuid[]              *  after b41c77e2
  ...
```

- Batch 1 is the frontier, plus todos already in progress. Batch *n* is every open todo whose open blockers all sit in earlier batches.
- The **critical path** is the longest chain of open todos by count. It is the minimum number of batches the plan needs, however many agents run.
- `--agents N` caps each batch at N, in-progress todos included. Todos with the longest remaining chain go first; the rest move to later batches.
- `--mermaid` prints the same batches as Mermaid `flowchart LR` source, one subgraph per batch, the critical path styled, for pasting into PRs, issues and docs.
- Batches are a forecast that assumes equal durations. The command recomputes them from current statuses on every run.

The mockup page used during design (`https://claude.ai/artifact/9ENRAyoaMGyYaDvn28DUuQ`) shows views B (this command), C (annotated lists) and D (Mermaid) on a sample plan.

### Claims and the stop hold

- `devkit todo start` on a blocked todo is refused, naming its open blockers. `--force` starts it anyway, for a person who knows better. The refusal lives in the CLI verb, not in the store's `apply`: the native mirror, `TakeOver` and the sub-agent claim check are unchanged, since by the time the mirror sees a native status change the harness has already made it.
- A blocked pending todo does not count toward the stop hold: an agent cannot finish work that waits on someone else's.

### Native task tools

When a native tool call carries blocking edges (Claude Code's `TaskUpdate` takes fields for them), the mirror records them as `Block`/`Unblock` edits on the mirrored todos. Tools that carry none are mirrored as today.

## Design

### devkit-todo

- `Todo` gains `blocked_by: Vec<String>`, serde-defaulted to empty so stored documents and older records read back unchanged. `NewTodo` does not: `add --blocked-by` adds the todo, then applies a `Block` edit, so each backend has one way to write edges.
- `Edit` gains `Block { id, by: Vec<String> }` and `Unblock { id, by: Vec<String> }`. Adding an edge that already exists, or removing one that does not, is a no-op. The cycle check runs before the edit is applied, over the store's listing.
- A new `graph` module holds the pure functions every caller shares: open and ready predicates, frontier, batches with an optional cap, critical path, cycle detection, and "what did this change unblock". They take the listed todos plus a lookup for blockers outside the listing. Hand-rolled Kahn's algorithm and a longest-path pass over its order; no graph crate.
- `render` gains the blocking notes on list lines, the plan text and the Mermaid source.

### Backends

Each backend stores a todo's blockers with the todo. The contract suite gains cases for edges: add, remove, idempotence, survival across status changes, and reading blockers back through `list` and `get`.

| Backend | Storage |
|---|---|
| builtin | `blocked_by` in each todo's JSON |
| taskchampion | one `dep_<uuid>` property per blocker, the attribute Taskwarrior reads |
| postgres | a `blocked_by uuid[]` column on `devkit.todos`, defaulting to empty, changed by new write functions that `devkit todo schema update` creates |
| supabase | the same column through the existing selects, and the same functions through the Data API |

An upgraded binary meeting an unmigrated database follows the existing precedent for functions a newer devkit adds. On Postgres, the first call that needs the column or a new function creates it. On Supabase, reads keep working and return todos with no edges. The store selects named columns today, so naming `blocked_by` in a select needs a fallback when PostgREST rejects the missing column; the mechanism is the builder's choice. An edge write fails with the error naming `devkit todo schema update`, as missing functions do today. Existing write functions keep their signatures, so older binaries on other machines keep working against a migrated database; edges arrive through new functions.

The array over a join table: the edge is a property of the todo on every other backend, `list` stays one query (one request on Supabase), edits run under the row lock the write functions already take, and the graph is computed in Rust. A join table pays off for edge kinds or server-side graph queries, both out of scope.

### Config

`[todo] print_unblocked` (bool, default `true`) and `[todo] ready_outside_ticket` (bool, default `false`), each documented on its field so the schema carries it.

### CLI, hooks, docs

- New verbs `block`, `unblock`, `ready`, `plan`; `add --blocked-by`; `start --force`; the `unblocked:` line on `done` and `cancel`.
- The injected context renders blocking notes through the same `render` change.
- `plugin/skills/using-devkit/references/todo.md` gains a section on blocking, the frontier, the ticket fence and plans.

## Testing

TDD, failing test first, through the real entry points:

- Contract-suite edge cases, run against all four backends (Postgres and Supabase where their test databases are configured).
- `graph` unit tests: frontier with multi-blocker todos (a todo blocked by 1 and 2 stays blocked when 1 finishes, a todo blocked by 1 alone becomes ready), batches with and without a cap, critical path, cycle refusal, closed and missing blockers.
- CLI tests driving the `devkit` binary: `block` then `ready`, `done` printing `unblocked:`, `start` refused on a blocked todo, the ticket fence hiding another workspace's ready todo, `plan` and `--mermaid` output.
- A hook test that the injected list shows `blocked by` and `ready` notes.
- On taskchampion, a test that the stored property is `dep_<uuid>`, so Taskwarrior reads it.

## Evals

The injected lists and `todo.md` are agent-facing text. Extend the text eval for the todo reference with questions on blocking, the frontier and the fence, and consider a scenario where a manager must dispatch only ready todos from its own ticket.

## Follow-ups

- View A: a terminal board with one column of boxes per batch.
- Estimates on todos, a weighted critical path, and a Gantt chart.
- Plan-file import.
- Blocking through the MCP server.

## Left to the builder

- Exact flag spellings beyond those above, and the plan's column layout and glyphs.
- Whether `ready` takes `--node`/`--subtree` like `list`.
- Postgres function names and signatures.
