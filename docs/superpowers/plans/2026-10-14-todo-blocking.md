# Todo Blocking and Plans Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record "A is blocked by B" between devkit todos on every backend, and answer what can run now (`ready`), how the plan unfolds (`plan`), and show blocking state in the lists agents already read.

**Architecture:** Each backend stores a todo's blockers on the todo (`blocked_by`). One pure `graph` module in `devkit-todo` computes everything else (open/blocked/ready, batches, critical path, cycles, what a change unblocked) from listed todos. The CLI, the hook-injected lists and the stop hold consume it; a ticket fence in `Layout` decides which todos a caller is offered.

**Tech Stack:** Rust 2024 workspace, clap, serde, anyhow, taskchampion 3.1, tokio-postgres, PostgREST (Supabase). No new crates.

**Spec:** `docs/superpowers/specs/2026-10-14-todo-blocking-design.md`. Read it before any task.

## Global Constraints

- Gate before every commit: `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt`.
- TDD: the failing test first, through the real entry point (the `devkit` binary for CLI behaviour, the hook for injected text).
- No new dependencies. Graph algorithms are hand-rolled (Kahn's algorithm plus a longest-path pass).
- Help text stays ASCII. Plan and list output use ASCII too (`>`, `o`, `*`, `->`).
- Match `StatusKind` exhaustively. A blocker is open while `Pending` or `InProgress`; `Completed`, `Cancelled` and missing count as closed.
- `anyhow` with `.context()`; test scratch from `tempfile`; CLI tests use `tests/common/todoenv.rs` (`Proj`).
- A config key's meaning is the doc comment on its field. Regenerate `schema/devkit-config.json` with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- Hooks never fail a verdict: errors in mirroring or rendering blocking state are logged and skipped.
- Conventional Commits, one logical change per commit, no attribution trailers.
- Work in a worktree (`workspace setup --slug todo-blocking`); the primary clone stays on `main`.

## Review Focus

1. A blocker given as a short id or uuid prefix is stored as the full id, and an unknown blocker id is refused with `no todo <id>` before anything is written. (Task 5)
2. A todo blocked by itself, or by a todo that already waits on it, is refused naming the cycle. (Task 5)
3. `done 1 2` where todo 3 is blocked by both prints `unblocked: 3` once, and `undone 1` blocks 3 again. (Task 5)
4. A purged blocker counts as closed: its dependent becomes ready and lists show no dangling `blocked by`. (Task 7)
5. Another session's mirrored native steps on the same checkout never appear in `ready` or `plan`. (Task 6)

---

### Task 1: Blocking edges in the model, builtin and taskchampion stores

**Files:**
- Modify: `crates/devkit-todo/src/lib.rs` (`Todo`, `Edit`)
- Modify: `crates/devkit-todo/src/contract.rs` (new cases, registered in `contract_tests!`)
- Modify: `crates/devkit-todo-builtin/src/lib.rs`
- Modify: `crates/devkit-todo-taskchampion/src/store.rs`
- Modify: `crates/devkit-todo-postgres/src/store.rs`, `crates/devkit-todo-supabase/src/store.rs` (compile only: `blocked_by: Vec::new()` in row conversion; `Block`/`Unblock` bail until Task 2)
- Test: `crates/devkit-todo-taskchampion/tests/taskchampion.rs`

**Interfaces:**
- Produces: `Todo::blocked_by: Vec<String>` (`#[serde(default)]`, full ids, sorted, no duplicates); `Edit::Block { id: String, by: Vec<String> }`; `Edit::Unblock { id: String, by: Vec<String> }`. Both return no `StatusChange`. Adding an existing edge or removing an absent one is a no-op; an unknown `id` is the error `no todo <id>`. The store does not check that blockers exist.

- [ ] **Step 1: Write the failing contract cases** in `contract.rs`, registered in the `@each` list:
  - `block_adds_blockers_once`: add a, b, c; `Block { id: c, by: [a, b] }`, then `Block { id: c, by: [a] }`; `get(c).blocked_by` is `[a, b]` sorted; `list(&Filter::all())` shows the same.
  - `unblock_removes_only_named_blockers`: after the above, `Unblock { id: c, by: [a, zz] }` leaves `[b]`.
  - `blockers_survive_status_changes`: block c on a, then start, complete and undo c; `blocked_by` unchanged.
  - `block_on_an_unknown_todo_is_refused`: `Block { id: "99999999", by: [a] }` errors containing `no todo 99999999`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo-builtin -p devkit-todo-taskchampion`. Expected: the four cases fail to compile (no `Edit::Block`).
- [ ] **Step 3: Add the field and edits** in `lib.rs`; builtin stores the field in its JSON document; postgres and supabase compile with an empty field and bail on the new edits.
- [ ] **Step 4: Taskchampion**: read blockers with `Task::get_dependencies()` into `Exported`, write with `Task::add_dependency` / `remove_dependency` inside `apply`. Add test `blockers_are_taskwarrior_dep_properties` in `tests/taskchampion.rs`: after `Block`, the raw task has a `dep_<blocker uuid>` key.
- [ ] **Step 5: Run** the command from Step 2. Expected: PASS.
- [ ] **Step 6: Commit** `feat(todo): store blocking edges on builtin and taskchampion`

### Task 2: Blocking edges on Postgres and Supabase

**Files:**
- Modify: `crates/devkit-todo-postgres/src/database.rs` (`SCHEMA`, `missing_schema`)
- Modify: `crates/devkit-todo-postgres/src/store.rs` (`COLUMNS`, `apply`)
- Modify: `crates/devkit-todo-supabase/src/store.rs`, `src/api.rs`
- Test: `crates/devkit-todo-postgres/tests/postgres.rs`, `crates/devkit-todo-supabase/tests/supabase.rs`

**Interfaces:**
- Consumes: `Edit::Block`/`Edit::Unblock`, `Todo::blocked_by` (Task 1).
- Produces: column `devkit.todos.blocked_by uuid[] NOT NULL DEFAULT '{}'`; functions `devkit.todo_block(p_root text, p_id text, p_by uuid[])` and `devkit.todo_unblock(p_root text, p_id text, p_by uuid[])`, each locking the row through `todo_lock` and keeping the array sorted and distinct. Existing function signatures do not change.

- [ ] **Step 1: Write the failing test** `a_database_from_before_blocking_gains_the_column` in `postgres.rs`, modelled on `a_database_from_before_the_functions_gains_them_and_keeps_its_todos`: create the pre-blocking schema (the existing old-schema SQL in that file, which lacks the column), add a todo, then `Block` through `PostgresStore`; the edge reads back and the old todo keeps its fields. The contract cases from Task 1 now run here too.
- [ ] **Step 2: Run** `devrun task test` with `DEVKIT_TEST_POSTGRES_URL`/`DEVKIT_TEST_POOLER_URL` from `crates/devkit-todo-postgres/testdb/up.sh`. Expected: FAIL (bail from Task 1, missing column).
- [ ] **Step 3: Implement**: `SCHEMA` gains `ALTER TABLE devkit.todos ADD COLUMN IF NOT EXISTS blocked_by uuid[] NOT NULL DEFAULT '{}'` and the two functions; `missing_schema` also matches `SqlState::UNDEFINED_COLUMN`; `COLUMNS` reads `blocked_by::text[]`; `apply` calls the functions.
- [ ] **Step 4: Supabase**: `COLUMNS` gains `blocked_by`; when PostgREST rejects the select because the column is missing, retry once with the old column list and return empty `blocked_by`. Edge writes call `todo_block`/`todo_unblock` over RPC; a missing function surfaces the existing error naming `devkit todo schema update`. Test `reads_without_the_blocking_column_list_todos_without_edges` in `supabase.rs`: drop the column on the test database, `list` succeeds.
- [ ] **Step 5: Run** the postgres and supabase suites as in Step 2 (Supabase also needs `DEVKIT_TEST_SUPABASE_URL`). Expected: PASS.
- [ ] **Step 6: Commit** `feat(todo): store blocking edges on postgres and supabase`

### Task 3: The graph module

**Files:**
- Create: `crates/devkit-todo/src/graph.rs` (`pub mod graph;` in `lib.rs`)
- Test: `crates/devkit-todo/tests/graph.rs`

**Interfaces:**
- Consumes: `Todo`, `Status`, `TodoStore`.
- Produces:
  - `pub struct Graph<'a>`, `Graph::new(todos: &'a [Todo]) -> Graph<'a>`
  - `Graph::open_blockers(&self, todo: &Todo) -> Vec<&'a Todo>`: blockers present in the graph and open, in id order
  - `Graph::is_blocked(&self, todo: &Todo) -> bool`
  - `Graph::is_ready(&self, todo: &Todo) -> bool`: pending, not blocked, not on a cycle
  - `Graph::unblocked_by(&self, closed: &[&str]) -> Vec<&'a Todo>`: ready todos listing any of `closed` as a blocker, each once, id order
  - `Graph::plan(&self, offered: impl Fn(&Todo) -> bool, cap: Option<NonZeroUsize>) -> Plan<'a>`
  - `pub struct Plan<'a> { pub batches: Vec<Vec<&'a Todo>>, pub critical: Vec<&'a Todo>, pub cycle: Vec<&'a Todo> }`, `Plan::frontier(&self) -> Vec<&'a Todo>` (batch 1 minus in-progress todos)
  - `pub fn cycle_through(todos: &[Todo], id: &str, by: &[String]) -> Option<Vec<String>>`: the ids of the cycle adding `by` to `id` would close, starting and ending at `id`
  - `pub fn with_blockers(store: &impl TodoStore, todos: Vec<Todo>) -> anyhow::Result<Vec<Todo>>`: appends each blocker the list lacks, fetched with `get` (absent ones skipped), one level deep

Plan rules, from the spec: plan members are open todos `offered` accepts. Batch 1 holds members in progress plus ready members. Batch n+1 holds members whose open blockers all sit in batches up to n; an open blocker outside the plan counts as finishing in batch 1. `cap` limits each batch's size, in-progress todos first and never moved; the rest are chosen by longest remaining chain, then by more dependents, then `order`, then id. `critical` is the longest chain of members by count, ties broken the same way, independent of `cap`. Members Kahn's algorithm never places go to `cycle`.

- [ ] **Step 1: Write the failing tests** in `tests/graph.rs`, building `Todo` values directly:
  - `a_todo_with_one_finished_blocker_is_ready_and_one_with_two_waits`: 1, 2 pending; 4 blocked by 1; 5 blocked by 1 and 2; complete 1; `is_ready(4)` true, `is_ready(5)` false, `unblocked_by(&["1"])` is `[4]`.
  - `in_progress_blockers_block_and_closed_or_missing_ones_do_not`: blockers in progress, cancelled, completed, and absent from the slice.
  - `batches_layer_by_open_blockers`: the spec's sample (A done; G in progress; B, H after A; C, D, E after B; F after E; I after G, H; J after G; K after H; L after F, I, J, K) gives batches `[G, B, H]`, `[C, D, E, I, J, K]`, `[F]`, `[L]` (compare as sets per batch) and critical `[B, E, F, L]`.
  - `a_cap_fills_batches_by_longest_chain`: same sample with cap 3: batch 1 `[G, B, H]`, batch 2 starts with E.
  - `cycle_members_are_reported_not_dropped`: 1 blocked by 2, 2 blocked by 1, 3 free; batches `[[3]]`, cycle `[1, 2]`, neither is ready.
  - `cycle_through_names_the_cycle`: 2 blocked by 1; `cycle_through(todos, "1", &["2"])` is `["1", "2", "1"]`; `cycle_through(todos, "1", &["1"])` is `["1", "1"]`; an acyclic edge gives `None`.
  - `frontier_puts_the_critical_path_first`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo --test graph`. Expected: FAIL to compile.
- [ ] **Step 3: Implement** `graph.rs` to the interfaces above.
- [ ] **Step 4: Run** the command from Step 2. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): compute frontier, batches and critical path`

### Task 4: Config keys and the ticket fence

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`TodoConfig`, its doctest), `crates/devkit-config/src/todo.rs` (`Input`, merge)
- Modify: `crates/devkit-todo/src/layout.rs`
- Modify: `schema/devkit-config.json` (regenerated)
- Test: `crates/devkit-todo/tests/layout.rs`

**Interfaces:**
- Produces:
  - `TodoConfig::print_unblocked: bool` (default `true`): whether `done` and `cancel` print the todos they made ready.
  - `TodoConfig::ready_outside_ticket: bool` (default `false`): whether the frontier reaches the caller's whole repository instead of its ticket.
  - `Layout::ticket_root(&self, facts: &Facts, wide: bool) -> Option<String>`: the `workspace` scope filled (or `repo` when `wide`); `None` when that scope does not fill as itself (walking up to a parent scope does not count).
  - `Layout::offered(&self, project: Option<&str>, root: &str, facts: &Facts) -> bool`: `project` is `root` or below it, and `!is_other_session(project, facts)`.

- [ ] **Step 1: Write the failing tests** in `tests/layout.rs`:
  - `the_ticket_is_the_workspace_node`: facts for `proj`/`main`/session `s1`: `ticket_root(.., false)` is `proj.main`, `(.., true)` is `proj`.
  - `outside_a_repository_there_is_no_ticket`: global place gives `None`.
  - `offered_covers_the_ticket_and_own_session_only`: `proj.main` and `proj.main.claude-s1.a1` are offered; `proj.main.claude-s2`, `proj`, `proj.other` and `global` are not.
  - Extend the `TodoConfig` doctest: `print_unblocked = false` and `ready_outside_ticket = true` parse; defaults are `true` and `false`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo --test layout` and `cargo test -p devkit-config --doc`. Expected: FAIL.
- [ ] **Step 3: Implement** the fields with doc comments (their text becomes the schema description) and the two `Layout` methods.
- [ ] **Step 4: Regenerate** the schema: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit --test config_schema`, then run Step 2's commands. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): add ticket fence and blocking config keys`

### Task 5: CLI for recording edges, claims and unblocked output

**Files:**
- Modify: `src/bin/devkit/todo/mod.rs`
- Test: create `tests/todo_blocking.rs`

**Interfaces:**
- Consumes: `Edit::Block`/`Unblock` (Task 1), `graph::{Graph, cycle_through, with_blockers}` (Task 3), `TodoConfig::print_unblocked` (Task 4).
- Produces CLI: `devkit todo add <text> --blocked-by <ids>`; `devkit todo block <id> --by <ids>`; `devkit todo unblock <id> --by <ids>`; `devkit todo start <ids> --force`; `done`/`cancel` print `unblocked: <short ids space-separated>` on stdout when the change made todos ready. `<ids>` are comma-separated (`value_delimiter = ','`).

Resolve `id` and every `--by` id through `store.get` before writing (full ids stored; unknown is `no todo <id>`). Before `Block`, run `cycle_through` over `store.list(&Filter::all())` and refuse with `blocking <id> on <ids> would close the cycle <a> -> <b> -> <a>`. `add --blocked-by` validates first, then adds and applies `Block`. `start` without `--force` refuses a blocked todo: `todo <id> is blocked by <ids>`, using `Graph` over `with_blockers` of the todo. After `done`/`cancel`, compute `unblocked_by` over the fenced listing from Task 4 so the line only names offered todos.

- [ ] **Step 1: Write the failing tests** in `tests/todo_blocking.rs` (builtin ids `1`, `2`, ...):
  - `block_records_edges_and_list_reads_them_back`: add 1, 2, 3; `block 3 --by 1,2`; `p.todo("3").blocked_by == ["1", "2"]`.
  - `add_with_blocked_by_records_the_edge`.
  - `an_unknown_blocker_is_refused_and_nothing_is_written`: `add x --blocked-by 9` fails with `no todo 9`, and no todo `x` exists.
  - `a_cycle_is_refused_naming_it`: `block 1 --by 1` and, after `block 2 --by 1`, `block 1 --by 2` fail; stderr names `1 -> 2 -> 1`.
  - `start_refuses_a_blocked_todo_unless_forced`.
  - `done_prints_what_it_unblocked_once`: 3 blocked by 1 and 2; `done 1` prints nothing about 3; `done 2` prints `unblocked: 3`; separately `done 1 2` prints `unblocked: 3` exactly once.
  - `undone_blocks_the_dependent_again`: after the above, `undone 1` then `start 3` is refused.
  - `print_unblocked_false_silences_the_line`: `[todo] print_unblocked = false` in the project's `devkit.toml`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit --test todo_blocking`. Expected: FAIL (unknown subcommands and flags).
- [ ] **Step 3: Implement** the verbs and flags in `todo/mod.rs` (`block` and `unblock` count as writes for sync).
- [ ] **Step 4: Run** the command from Step 2. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): add block, unblock and blocked-start refusal`

### Task 6: `devkit todo ready` and `devkit todo plan`

**Files:**
- Modify: `src/bin/devkit/todo/mod.rs`
- Modify: `crates/devkit-todo/src/render.rs`
- Test: `tests/todo_blocking.rs`, create `crates/devkit-todo/tests/render.rs`

**Interfaces:**
- Consumes: `Graph::plan`, `Plan::frontier`, `with_blockers` (Task 3); `Layout::ticket_root`, `Layout::offered`, `ready_outside_ticket` (Task 4).
- Produces:
  - CLI `devkit todo ready` (one line per frontier todo: short id, text, `*` when on the critical path) and `devkit todo plan [--agents <N>] [--mermaid]`.
  - `render::render_plan(root: &str, plan: &Plan) -> String`, in the spec's format: header `<root>: <n> open, <k> in progress`, `critical path: <id> -> <id> ... (<n> batches minimum)`, then `batch <n>` blocks (`ready now` on batch 1) of `  <mark> <short id>  <text>  [*] <note>` lines, mark `>` in progress and `o` otherwise, note `in progress: <holder>`, `ready`, or `after <ids>`; a `cycle: <ids>` line when the plan has one.
  - `render::render_mermaid(plan: &Plan) -> String`: `flowchart LR`, one `subgraph b<n>["batch <n>"]` per batch, nodes `t<short id>["<short id>: <text>"]` (`"` escaped as `#quot;`), one `-->` per edge between members, `classDef critical stroke-width:3px` and a `class` line for the critical path.

Offered todos: with a ticket root, `Layout::offered(project, root, facts)`; without one, `Layout::visible`. The listing for the graph is `store.list(&Filter::all())` fenced by `layout.fence`, then `with_blockers`.

- [ ] **Step 1: Write the failing tests**:
  - `render.rs`: `plan_text_names_batches_critical_path_and_cycle` and `mermaid_has_one_subgraph_per_batch_and_escapes_quotes`, with expected strings for a three-todo plan.
  - `todo_blocking.rs`: `ready_lists_the_frontier_critical_first`; `plan_with_agents_caps_batches`; `plan_mermaid_prints_flowchart_source`; `another_sessions_steps_are_not_offered` (session `s2` adds an edgeless todo on its own node; `ready` and `plan` as `s1` with the manager role never show it); `another_tickets_ready_todo_is_not_offered_by_default` (a ready todo on `proj.other`, shown only with `ready_outside_ticket = true`).
- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo --test render` and `cargo nextest run -p devkit --test todo_blocking`. Expected: FAIL.
- [ ] **Step 3: Implement** the two render functions and the two verbs.
- [ ] **Step 4: Run** Step 2's commands. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): add ready and plan views`

### Task 7: Blocking notes in lists, injected context and the stop hold

**Files:**
- Modify: `crates/devkit-todo/src/render.rs` (`render_lists`, `render_full`, `line`)
- Modify: `crates/devkit-todo/src/hold.rs` (`open_for`)
- Modify: `src/bin/devkit/todo/mod.rs` (`list`, `context`), `src/bin/devkit/hook/todo.rs` (stop hold caller)
- Test: `crates/devkit-todo/tests/render.rs`, `tests/todo_context.rs`, `tests/todo_hold.rs`

**Interfaces:**
- Consumes: `Graph` (Task 3), `Layout::offered` (Task 4).
- Produces:
  - `pub struct Notes<'a> { pub graph: &'a Graph<'a>, pub show_ready: &'a dyn Fn(&Todo) -> bool }`; `render_lists` and `render_full` take `notes: &Notes` as a new last parameter.
  - Line format: `- [ ] text (<id>, blocked by <id>, <id>)` while blocked, a blocker on a node not among the rendered nodes written `<id> on <node>`; `- [ ] text (<id>, ready)` when the todo has blockers, is ready, and `show_ready` accepts it; a todo with no blockers renders exactly as today.
  - `hold::open_for(todos, holder, pending_node, graph: &Graph)`: blocked pending todos are not open.

`show_ready` in `list` and `context`: offered by the ticket fence, and for a sub-agent (`facts.subagent`) only on its own node.

- [ ] **Step 1: Write the failing tests**:
  - `render.rs`: `blocked_and_ready_notes_render_in_the_parentheses`, `a_blocker_off_the_listing_is_named_with_its_node`, `todos_without_blockers_render_unchanged`.
  - `todo_context.rs`: `injected_lists_show_blocking_notes`; `a_subagent_sees_no_ready_note_on_its_managers_list`; `a_purged_blocker_leaves_its_dependent_ready` (purge as `DEVKIT_CALLER=human`).
  - `todo_hold.rs`: `a_blocked_pending_todo_does_not_hold_the_stop`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo --test render` and `cargo nextest run -p devkit --test todo_context --test todo_hold`. Expected: FAIL.
- [ ] **Step 3: Implement** `Notes`, the line notes, the `open_for` change, and build the graph (`with_blockers` over the listing) in `list`, `context` and the stop hook.
- [ ] **Step 4: Run** Step 2's commands, then the full gate. Expected: PASS; existing context and digest tests unchanged because edgeless lists render as before.
- [ ] **Step 5: Commit** `feat(todo): show blocking state in lists and the stop hold`

### Task 8: Mirror blocking from native task tools

**Files:**
- Modify: `src/bin/devkit/hook/todo.rs` (`task_update`)
- Test: `tests/todo_hooks.rs`

**Interfaces:**
- Consumes: `Edit::Block` (Task 1), `cycle_through` (Task 3).
- Produces: a Claude Code `TaskUpdate` carrying blockers adds the matching `Block` edits on the mirrored todos.

- [ ] **Step 1: Confirm the field names** Claude Code's `TaskUpdate` uses for blocking (expected `addBlockedBy` and `addBlocks`, arrays of task ids) from Claude Code's own documentation or tool schema, using the `devkit:docs` skill or the `claude-code-guide` agent. Record the source in the commit body. If `TaskCreate` carries them too, cover it the same way.
- [ ] **Step 2: Write the failing test** `task_update_blockers_are_mirrored` in `todo_hooks.rs`: create two native tasks through hook payloads, send a `TaskUpdate` with `addBlockedBy: [<first native id>]` for the second; the second mirrored todo's `blocked_by` holds the first's id. Add `a_mirrored_cycle_is_skipped`: the reverse edge is not recorded and the hook still exits 0 with no stdout.
- [ ] **Step 3: Run** `cargo nextest run -p devkit --test todo_hooks`. Expected: FAIL.
- [ ] **Step 4: Implement** in `task_update`: map native ids through `c.map.lookup`, skip unknown ones, check `cycle_through`, apply `Block`.
- [ ] **Step 5: Run** Step 3's command. Expected: PASS.
- [ ] **Step 6: Commit** `feat(hook): mirror native task blocking into todos`

### Task 9: Reference docs and the todo-guide eval

**Files:**
- Modify: `plugin/skills/using-devkit/references/todo.md`
- Modify: `evals/todo-guide/questions.json` (and its `render.sh`/`preamble.md` if the new text needs rendering)

**Interfaces:**
- Consumes: the behaviour of Tasks 1-8.

- [ ] **Step 1: Write a "Blocking and plans" section** in `todo.md`: recording edges (`add --blocked-by`, `block`, `unblock`), what open and ready mean, `ready`, `plan`, the `ready`/`blocked by` notes, the ticket fence and `ready_outside_ticket`, `print_unblocked`, that a sub-agent hands unblocked work back to its manager rather than starting it, `start --force` being for people, and how Taskwarrior reads the edges on the taskchampion backend. State behaviour, not counts.
- [ ] **Step 2: Add eval questions** to `evals/todo-guide/questions.json`, each with `proof` naming the test from Tasks 5-7 that pins the answer: which todo is ready after `done 1` with 4 blocked by 1 and 5 blocked by 1 and 2; whether a sub-agent should start a todo `done` reported as unblocked; whether `ready` shows another ticket's todo.
- [ ] **Step 3: Verify** with `cargo nextest run -p devkit --test docs_links --test docs_examples` (expected PASS). The billed eval run (`devrun task eval --arg eval_case=todo-guide`) is for the person, not the executor; note in the PR that it was not run.
- [ ] **Step 4: Commit** `docs(todo): document blocking, ready and plan`

## Follow-ups (file as tickets, do not build)

- Terminal board view (one column of boxes per batch).
- Estimates, weighted critical path, Gantt chart.
- Plan-file import.
- Blocking through the MCP server.
