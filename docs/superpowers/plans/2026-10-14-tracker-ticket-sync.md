# Tracker Ticket Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `devkit todo sync` brings the tickets a repository's rules select into the todo store, one todo per ticket, and a ticket todo's start and completion move the ticket's tracker status through `[ticket.events]`.

**Architecture:** The todo model gains a ticket reference and a `tracker` holder; every backend stores both and resolves a ticket to one todo. The tracker seam gains read-only ticket queries for GitHub and Linear. A pure planner in `devkit-todo` diffs fetched tickets against the store into edits; the `devkit` binary runs it on demand, in the background when the last sync is stale, and fires the new todo events after a ticket todo's status changes.

**Tech Stack:** Rust 2024 workspace; taskchampion, tokio-postgres, Supabase PostgREST, `gh` (REST and GraphQL), Linear GraphQL; `uuid` v5; nextest.

**Spec:** `docs/superpowers/specs/2026-10-14-tracker-ticket-sync-design.md`. Read it before any task; every task argues from it.

## Global Constraints

- The work ships as three PRs, one per ticket: tasks 1 to 5, 7 and 8 (sync on demand, GitHub); task 6 (Linear); tasks 9 to 12 (refresh, todo events, the ticket fence, docs). The second and third start once the first has merged.
- The spec and this plan are committed on branch `ticket-sync-design`. The first PR's branch starts from `origin/ticket-sync-design`, so it carries them to `main`; the later PRs start from `main`.
- TDD: the failing test comes first, and fails for the reason the task names.
- Gates before every commit: `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt`.
- Sync never writes to the tracker. The only tracker writes are `[ticket.events]` moves, through the existing `StatusWriter`.
- devkit names no tracker status in code; every status name comes from config. Status names match case-insensitively, ignoring surrounding whitespace, as `EventTransition` names do.
- Hooks: no `hook` verb waits on the network; tracker work from a hook is spawned detached (`devkit_common::sys::spawn_background`).
- Tests drive `gh` through `tests/common/ghfake.rs`; tracker unit tests use `tracker::fake::FakeTracker` or canned JSON. Test scratch comes from `tempfile`. Tests that wait on a spawned process poll for the expected state.
- A config key's meaning is the doc comment on its field; regenerate `schema/devkit-config.json` with `DEVKIT_UPDATE_SCHEMA=1 cargo test` whenever config types change. A config type's `devkit.toml` example is a doctest on that type.
- Match `StatusKind`, `IssueEvent` and `Role` exhaustively.
- Conventional Commits, imperative, at most 50 characters in the subject.

## Review Focus

1. **A ticket in a status the map does not name** (a team adds "Blocked"): the sync fails naming "Blocked" and writes nothing; it never silently skips the ticket. Pinned in Task 7.
2. **An agent started a ticket todo whose tracker status has not moved:** the next sync leaves it in progress under the agent. Pinned in Task 7.
3. **Two syncs racing on one ticket** (two sessions, or two taskchampion replicas): one todo results. Pinned in Task 2 (contract) and Task 3 (Postgres).
4. **A GitHub issue outside the project, or a closed issue whose field still says "In progress":** the first maps through `""`, the second through its close reason. Pinned in Task 5 and Task 7.
5. **A hook reading a stale store while the tracker hangs:** the hook answers within its deadline, from the store as it is, and a failed sync holds off retries. Pinned in Task 9.

---

### Task 1: The `tracker` holder

**Files:**
- Modify: `crates/devkit-todo/src/holder.rs`
- Modify: `crates/devkit-todo/src/contract.rs` (new cases, registered in `contract_tests!`)
- Modify: `crates/devkit-todo-postgres/src/database.rs` (`devkit.todo_covers`)

**Interfaces:**
- Produces: `Holder::tracker() -> Holder` (id `"tracker"`), `Holder::is_tracker(&self) -> bool`. `covers` returns true for `tracker` acting on any holder; `is_human()` stays false for it.

- [ ] **Step 1: Write the failing tests.** In `holder.rs` tests, `tracker_covers_every_holder`: `Holder::tracker().covers(&Holder::new("S/a"))`, `.covers(&Holder::human())`, and `!Holder::tracker().is_human()`. In `contract.rs`, `the_tracker_hands_its_claim_to_any_actor`: a todo set `InProgress` as `tracker`, then `InProgress` as `"S"`, ends `InProgress { by: "S" }`; and `the_tracker_finishes_an_agents_claim`: a todo in progress by `"S"`, set `Completed` as `tracker`, ends `Completed { by: Some("S") }`.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p devkit-todo -p devkit-todo-builtin -E 'test(tracker)'`. Expected: FAIL with `Claimed`.
- [ ] **Step 3: Implement.** `covers` adds `|| self.is_tracker()`. `devkit.todo_covers(actor, other)` returns true when `actor = 'tracker'`, beside its `human` case.
- [ ] **Step 4: Run the tests.** The step 2 command plus `cargo nextest run -p devkit-todo-taskchampion`. Expected: PASS. Postgres runs in CI's `postgres` job, or locally after `crates/devkit-todo-postgres/testdb/up.sh`.
- [ ] **Step 5: Commit.** `feat(todo): add a tracker holder that covers all`

### Task 2: Ticket references on todos (model, builtin, taskchampion)

**Files:**
- Modify: `crates/devkit-todo/src/lib.rs` (`Todo`, `NewTodo`, `Edit`, new `TicketRef`, new `ticket_todo_uuid`)
- Modify: `crates/devkit-todo/src/contract.rs`
- Modify: `crates/devkit-todo-builtin/src/lib.rs`
- Modify: `crates/devkit-todo-taskchampion/src/schema.rs`, `src/store.rs`
- Modify: `Cargo.toml` (workspace `uuid` gains feature `v5`); `crates/devkit-todo/Cargo.toml` (depends on `uuid`)
- Modify: every `NewTodo { .. }` and `Todo { .. }` literal the compiler names

**Interfaces:**
- Produces:
  - `pub struct TicketRef { pub tracker: String, pub key: String, pub synced_status: Option<String> }` (serde; `Clone, Debug, PartialEq, Eq`). `tracker` is `"github"` or `"linear"`; `key` is the tracker's id (`"65"`, `"SWE-123"`); `synced_status` is the status name as last synced (Task 7 defines its spelling).
  - `Todo.ticket: Option<TicketRef>` (`#[serde(default)]`); `NewTodo.ticket: Option<TicketRef>`.
  - `Edit::RecordSync { id: String, synced_status: String }`: sets `ticket.synced_status`. On a todo with no ticket it is the error `todo <id> has no ticket`.
  - `pub fn ticket_todo_uuid(root: &str, tracker: &str, key: &str) -> uuid::Uuid`: v5 over `Uuid::NAMESPACE_OID` and the bytes `root \0 tracker \0 key`; `root` is `""` for stores without one.
  - Contract: `add` of a `NewTodo` whose ticket (tracker, key) already has a todo returns that todo's id and changes nothing.

- [ ] **Step 1: Write the failing tests**, the contract cases registered in `contract_tests!`:
  - `a_ticket_reference_round_trips`: add with `ticket: Some(TicketRef { tracker: "github", key: "65", synced_status: Some("Todo") })`; `get` and `list` return it unchanged.
  - `adding_a_ticket_twice_returns_one_todo`: two adds, same tracker and key, different descriptions, return one id; `list(&Filter::all())` holds one todo with the first description.
  - `record_sync_sets_the_synced_status`: `RecordSync { synced_status: "Done" }` then `get` shows `Some("Done")`; on a todo without a ticket it errors.
  - In `lib.rs` tests, `ticket_todo_uuid_is_stable`: equal inputs give equal uuids; a different root, tracker or key gives a different one.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p devkit-todo -p devkit-todo-builtin -p devkit-todo-taskchampion -E 'test(ticket) | test(record_sync)'`. Expected: compile errors, then FAIL.
- [ ] **Step 3: Implement the model** in `lib.rs`.
- [ ] **Step 4: Implement builtin.** The document stores `ticket`; `add` finds a todo with the same tracker and key under the file lock it already holds and returns it.
- [ ] **Step 5: Implement taskchampion.** Properties `ticket_tracker`, `ticket_key`, `ticket_status` in `schema.rs` beside `subof`; `add` with a ticket creates the task at `ticket_todo_uuid("", tracker, key)`, and returns that uuid unchanged when the task already exists.
- [ ] **Step 6: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 7: Commit.** `feat(todo): record the ticket a todo came from`

### Task 3: Ticket references on Postgres and Supabase

**Files:**
- Modify: `crates/devkit-todo-postgres/src/database.rs` (schema script), `src/store.rs` (`COLUMNS`, row mapping, `add`, `apply`)
- Modify: `crates/devkit-todo-supabase/src/store.rs` (`COLUMNS`, `add`, `apply`), `src/api.rs` (older-database tolerance)
- Test: the Task 2 contract cases, through each crate's `contract_tests!`; `tests/todo_postgres.rs`

**Interfaces:**
- Consumes: `TicketRef`, `Edit::RecordSync`, `ticket_todo_uuid` (Task 2).
- Produces: columns `ticket_tracker text`, `ticket_key text`, `ticket_status text` on `devkit.todos`. `devkit.todo_add` gains `p_id uuid DEFAULT NULL`, `p_ticket_tracker text DEFAULT NULL`, `p_ticket_key text DEFAULT NULL`, `p_ticket_status text DEFAULT NULL`, inserts with `ON CONFLICT (id) DO NOTHING`, and returns the id. New `devkit.todo_record_sync(p_root text, p_id text, p_status text)`.

- [ ] **Step 1: Write the failing test.** In `tests/todo_postgres.rs`, `racing_ticket_adds_make_one_todo`: two threads, each with its own `PostgresStore` on one root, add the same ticket 20 times each; the root ends with one todo. The Task 2 contract cases are the rest of the RED.
- [ ] **Step 2: Run them to see them fail.** Run `crates/devkit-todo-postgres/testdb/up.sh`, export the variables it prints, then `cargo nextest run -p devkit-todo-postgres -p devkit-todo-supabase --test todo_postgres -E 'test(ticket) | test(record_sync)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** Columns by `ALTER TABLE devkit.todos ADD COLUMN IF NOT EXISTS`, as `blocked_by` was. The store passes `p_id = ticket_todo_uuid(root, tracker, key)` for a ticket todo. Every SQL function returning a `devkit.todos` row returns the new columns through `COLUMNS`. Supabase reads a refusal naming a `ticket_` column as an older database, as it does for `blocked_by`: listing still works, and adding a ticket todo fails naming `devkit todo schema update`.
- [ ] **Step 4: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(todo): store ticket references in Postgres`

### Task 4: Sync and todo-event config

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`TicketConfig`, `TicketEventsConfig`, `IssueEvent`, new `TicketSyncConfig`, `MatchRule`, `Assignee`, `ProjectRole`, `MappedStatus`)
- Modify: `src/bin/devkit/issue/event.rs` (`EventArg`)
- Modify: `schema/devkit-config.json` (regenerated)

**Interfaces:**
- Produces:
  - `TicketConfig.sync: TicketSyncConfig`, read from `[ticket.sync]`.
  - `pub struct TicketSyncConfig { pub rules: Vec<MatchRule>, pub statuses: BTreeMap<String, MappedStatus>, pub max_age_secs: u64 }`; `max_age_secs` defaults to 600. Sync is on when `rules` is non-empty.
  - `pub struct MatchRule { pub assignee: Option<Assignee>, pub project_role: Option<ProjectRole>, pub labels: Vec<String> }`, `deny_unknown_fields`.
  - `pub enum Assignee { Me, Nobody, User(String) }`, spelled `"me"`, `"none"`, or a user name.
  - `pub enum ProjectRole { Lead, Member }`, spelled `"lead"`, `"member"`; member matches lead or member.
  - `pub enum MappedStatus { Pending, InProgress, Completed, Cancelled }`, `snake_case`.
  - `TicketSyncConfig::lookup(&self, name: &str) -> Option<MappedStatus>`, matching names case-insensitively and trimmed.
  - `IssueEvent::TodoStart`, `IssueEvent::TodoDone`, configured as `[ticket.events.todo_start]` and `[ticket.events.todo_done]`; `EventArg` gains both.

- [ ] **Step 1: Write the failing tests.** A doctest on `TicketSyncConfig` parses the example below and asserts `rules.len() == 2`, `lookup(" in progress ") == Some(MappedStatus::InProgress)`, and `events.get(IssueEvent::TodoDone).unwrap().to == "Done"`. A unit test, `an_unknown_rule_key_is_a_parse_error`.

```toml
[ticket.sync]
max_age_secs = 900

[[ticket.sync.rules]]
assignee = "me"

[[ticket.sync.rules]]
assignee = "none"
project_role = "lead"

[ticket.sync.statuses]
"Backlog" = "pending"
"Todo" = "pending"
"In Progress" = "in_progress"
"In Review" = "in_progress"
"Done" = "completed"
"Canceled" = "cancelled"

[ticket.events.todo_done]
to = "Done"
```

- [ ] **Step 2: Run them to see them fail.** `cargo test -p devkit-config --doc TicketSyncConfig && cargo nextest run -p devkit-config`. Expected: FAIL.
- [ ] **Step 3: Implement the types**, each field's doc comment stating its meaning. Grow `IssueEvent::ALL`, `as_str`, `TicketEventsConfig::get` and `EventArg` exhaustively.
- [ ] **Step 4: Regenerate the schema** with `DEVKIT_UPDATE_SCHEMA=1 cargo test`, then run the step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(config): add ticket sync rules and todo events`

### Task 5: GitHub ticket queries

**Files:**
- Create: `crates/devkit-common/src/tracker/tickets.rs`
- Modify: `crates/devkit-common/src/tracker/mod.rs` (trait methods; `pub mod tickets`)
- Modify: `crates/devkit-common/src/tracker/github.rs`, `fake.rs`

**Interfaces:**
- Consumes: `MatchRule`, `Assignee`, `ProjectRole` (Task 4).
- Produces, in `tracker::tickets`:
  - `pub struct SyncedTicket { pub key: String, pub title: String, pub status: TicketStatus, pub blocked_by: Vec<String>, pub parent: Option<String>, pub team: Option<String> }`; `blocked_by` and `parent` hold ticket keys; `team` is the Linear team id, `None` on GitHub, read by `status_names`.
  - `pub enum TicketStatus { Named(String), Closed(CloseReason) }` and `pub enum CloseReason { Completed, NotPlanned }`. Linear yields `Named` always; GitHub yields `Closed` for a closed issue, and `Named(option)` or `Named("")` for an open one.
  - On `Tracker`: `fn find_tickets(&self, rules: &[MatchRule], closed_names: &[String]) -> Result<Vec<SyncedTicket>>`, `fn tickets(&self, keys: &[String]) -> Result<Vec<SyncedTicket>>`, `fn status_names(&self, tickets: &[SyncedTicket]) -> Result<Vec<String>>`. The default bodies return an error naming the tracker kind as unable to sync tickets.
  - `FakeTracker::with_tickets(Vec<SyncedTicket>)` and `with_status_names(Vec<&str>)`; the fake's `find_tickets` returns its tickets minus those named in `closed_names`.

- [ ] **Step 1: Write the failing tests** in `github.rs`, against canned GraphQL answers: `open_issue_in_project_maps_its_field_option` (`Named("In progress")`), `open_issue_outside_project_is_the_empty_name` (`Named("")`), `closed_issue_maps_by_reason` (`stateReason` `COMPLETED` and `NOT_PLANNED`), `blocked_by_and_parent_are_issue_numbers`, `assignee_me_queries_the_viewer`, `labels_must_all_match`, `project_role_is_refused_on_github` (error naming `project_role`), `status_names_are_field_options_plus_empty`.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p devkit-common -E 'test(github::)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** One GraphQL search per rule through `cmd::gh_json_in`, scoped to `[github] issues_repo` with `is:issue is:open`, paginated to the end. Per issue it reads `number`, `title`, `state`, `stateReason`, the parent issue's number, the numbers of the issues blocking it, and the `[github] status_field` value on its `[github] project` item. Results are deduplicated by key. Confirm the sub-issue and issue-dependency field names against GitHub's GraphQL schema (`devkit:docs`, or `gh api graphql` introspection) before writing the query. `tickets(keys)` reads the same fields by number in one aliased query. Without `[github] project`, every open issue is `Named("")`.
- [ ] **Step 4: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(tracker): list GitHub tickets for sync`

### Task 6: Linear ticket queries

**Files:**
- Modify: `crates/devkit-common/src/tracker/linear.rs`

**Interfaces:**
- Consumes: `tracker::tickets` and the three trait methods (Task 5).
- Produces: Linear's `find_tickets`, `tickets` and `status_names`.

- [ ] **Step 1: Write the failing tests**, against canned GraphQL:
  - `rule_filter_is_built_from_conditions`: assignee `me` gives `assignee: { isMe: { eq: true } }`; `none` gives `assignee: { null: true }`; `project_role = "lead"` gives `project: { lead: { isMe: { eq: true } } }`; `"member"` matches the lead or `members: { some: { isMe: { eq: true } } }`; each label gives a `labels: { some: { name: { eqIgnoreCase: "<label>" } } }` clause.
  - `closed_names_are_excluded_by_state_name`.
  - `blocks_relations_become_blocked_by_on_the_blocked_issue`.
  - `parent_is_the_parent_identifier`.
  - `status_names_are_the_union_of_the_teams_states`.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p devkit-common -E 'test(linear::)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** One `issues(filter:)` query per rule, paginated to the end, selecting `identifier`, `title`, `state { name }`, `team { id }`, `parent { identifier }`, and the issue's relations of type `blocks` in which it is the blocked side. Verify the relation direction and the filter field names against Linear's schema before writing the query. `status_names` reads `states { nodes { name } }` for each distinct team of the tickets.
- [ ] **Step 4: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(tracker): list Linear tickets for sync`

### Task 7: The sync planner

**Files:**
- Create: `crates/devkit-todo/src/ticket.rs`
- Test: `crates/devkit-todo/tests/ticket.rs`

**Interfaces:**
- Consumes: `Todo`, `TicketRef`, `Edit`, `Holder::tracker()` (Tasks 1 and 2); `TicketSyncConfig::lookup`, `MappedStatus` (Task 4); `SyncedTicket`, `TicketStatus` (Task 5).
- Produces:
  - `pub fn plan(tracker: &str, node: &str, existing: &[Todo], fetched: &[SyncedTicket], known_names: &[String], config: &TicketSyncConfig) -> Result<SyncPlan>`.
  - `pub struct SyncPlan { pub adds: Vec<NewTodo>, pub edits: Vec<PlannedEdit> }`. `PlannedEdit` is an `Edit` whose ids are ticket keys, resolved to todo ids after the adds land: `SetStatus` as `tracker`, `RecordSync`, `Describe`, `Move`, `Block`, `Unblock`.
  - `pub fn todo_text(tracker: &str, key: &str, title: &str) -> String`: `"SWE-123 Fix the plate loader"` on Linear, `"#65 Add sync"` on GitHub.
  - `pub fn status_name(status: &TicketStatus) -> String`: the option or state name for `Named`, and `closed:completed` or `closed:not_planned` for `Closed`. This is the spelling `synced_status` stores.
  - `SyncPlan::render(&self) -> String`: one line per change, for `--dry-run`.

- [ ] **Step 1: Write the failing tests** in `tests/ticket.rs`:
  - `a_new_ticket_becomes_one_todo_on_the_node`: text from `todo_text`, mapped status, `synced_status` set.
  - `an_unmapped_status_fails_naming_it`: a ticket in `"Blocked"`; the error contains `Blocked` and nothing is planned.
  - `a_mapped_name_the_tracker_lacks_fails`: the map names `"Doing"` and `known_names` lacks it.
  - `an_agents_start_survives_an_unmoved_ticket`: an existing todo in progress by `"S"`, synced `"Todo"`, fetched `"Todo"`; no `SetStatus` planned.
  - `a_moved_ticket_moves_its_todo_as_tracker`: synced `"Todo"`, fetched `"Done"`; `SetStatus Completed` by `tracker`, then `RecordSync "Done"`.
  - `a_closed_github_issue_maps_by_reason`: `Closed(NotPlanned)` plans `Cancelled`.
  - `blocking_links_between_synced_tickets_become_edges`: a link to an unsynced key is not an edge.
  - `a_sub_issue_nests_under_its_parent_ticket`.
  - `local_children_and_edges_are_left_alone`: an existing child todo and a blocker that is not a ticket todo stay; a ticket-to-ticket edge the fetch no longer reports is unblocked.
  - `a_renamed_ticket_is_described_again`.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p devkit-todo --test ticket`. Expected: FAIL (module missing).
- [ ] **Step 3: Implement `plan`.** Validate everything before planning any change. `Named(n)` maps through `config.lookup(n)`, `Closed(Completed)` to `Completed`, `Closed(NotPlanned)` to `Cancelled`. A status edit is planned only when `status_name` differs from the todo's `synced_status`. Only edges between two ticket todos are the tracker's to add or remove.
- [ ] **Step 4: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(todo): plan ticket sync from tracker tickets`

### Task 8: `devkit todo sync` runs the ticket sync

**Files:**
- Create: `src/bin/devkit/todo/tickets.rs`
- Modify: `src/bin/devkit/todo/mod.rs` (`Sync` gains `--dry-run`; `mod tickets`)
- Test: `tests/todo_ticket_sync.rs`

**Interfaces:**
- Consumes: `plan`, `SyncPlan` (Task 7); `find_tickets`, `tickets`, `status_names` (Tasks 5 and 6); `tracker::select`.
- Produces:
  - `pub(crate) fn sync_tickets(store: &Store, cwd: &Path, dry_run: bool) -> Result<SyncReport>`: resolves the repository and its `repo`-scope node; fetches rule matches, excluding names the map sends to completed or cancelled, plus `tickets()` for every ticket todo on the node still pending or in progress; plans; applies adds before edits. `pub(crate) struct SyncReport { pub added: usize, pub changed: usize }`.
  - The last-sync time at `<state dir>/ticket-sync/<repo>.last` (RFC 3339), written after a successful, non-dry sync.
  - `devkit todo sync` runs today's taskchampion sync, then `sync_tickets` when `[ticket.sync]` has rules. `--dry-run` prints `SyncPlan::render` and writes nothing.

- [ ] **Step 1: Write the failing tests** in `tests/todo_ticket_sync.rs`: a GitHub project through `ghfake` serving open issues 65 (blocked by 66) and 66, rules `assignee = "me"`, statuses for `""` and the project options.
  - `sync_adds_one_todo_per_ticket_with_edges`: two todos on the repo node; 65's todo is blocked by 66's.
  - `a_second_sync_adds_nothing`.
  - `dry_run_prints_and_writes_nothing`: stdout names both tickets; the store stays empty.
  - `an_unmapped_status_fails_and_writes_nothing`: exit non-zero, stderr names the status.
  - `sync_without_rules_queries_no_tickets`: `gh.calls()` shows no ticket query.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run --test todo_ticket_sync`. Expected: FAIL.
- [ ] **Step 3: Implement** in `tickets.rs`, against `TodoStore` alone.
- [ ] **Step 4: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(todo): sync tickets with devkit todo sync`

### Task 9: Refresh on read

**Files:**
- Modify: `src/bin/devkit/todo/tickets.rs` (`spawn_if_stale`)
- Modify: `src/bin/devkit/todo/mod.rs` (`list`, `plan`, `ready` call it), `src/bin/devkit/hook/todo.rs` (the list injection calls it)
- Test: `tests/todo_ticket_sync.rs`

**Interfaces:**
- Consumes: `sync_tickets` and the last-sync file (Task 8).
- Produces: `pub(crate) fn spawn_if_stale(cwd: &Path, config: &TicketSyncConfig)`. When rules exist, the last sync is older than `max_age_secs` or absent, and no hold is active, it spawns `devkit todo sync --tickets-only --background` detached and returns at once. The background run takes `<state dir>/ticket-sync/<repo>.lock` and exits when another run holds it. A failure writes `<repo>.failed` and holds retries for `max_age_secs`, as `todo/sync.rs`'s failed marker does. `--tickets-only` and `--background` are hidden flags on `Sync`.

- [ ] **Step 1: Write the failing tests:**
  - `a_stale_store_starts_one_background_sync`: two `devkit todo list` calls in a row, with no last-sync file; once the spawned run finishes, `gh.calls()` shows one ticket query.
  - `a_fresh_store_starts_none`.
  - `a_failed_sync_holds_off_the_next`: `ghfake` failing GraphQL; the second `list` starts no query.
  - `a_hook_injection_answers_while_the_tracker_hangs`: the injection hook returns within 3 seconds with the stored lists while `gh` hangs.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run --test todo_ticket_sync -E 'test(stale) | test(fresh) | test(holds) | test(hangs)'`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(todo): refresh tickets when the sync is stale`

### Task 10: Fire `todo_start` and `todo_done`

**Files:**
- Create: `src/bin/devkit/todo/ticket_event.rs`
- Modify: `src/bin/devkit/todo/store.rs` (the path every `SetStatus` passes through)
- Test: `tests/todo_ticket_sync.rs`

**Interfaces:**
- Consumes: `StatusChange` from `apply`, `Todo.ticket`, `Holder::is_tracker` (Tasks 1 and 2); `IssueEvent::TodoStart`, `TodoDone` (Task 4); `devkit ticket event` (existing).
- Produces: `pub(crate) fn fire_for(changes: &[StatusChange], actor: &Holder, store: &Store, cwd: &Path)`. For each change into `InProgress` or `Completed` on a todo with a ticket, when `actor` is not `tracker`, it spawns detached `devkit ticket event todo_start|todo_done --issue <key> --dir <checkout root of cwd> --log-file <state dir>/ticket-event.log`. When `cwd` is in no checkout, or in a checkout of another repository than the todo's node, it fires nothing and prints `devkit todo: ticket <key> not moved: run from its repository` on stderr.

- [ ] **Step 1: Write the failing tests**, with GitHub project 3 via `ghfake`, `[ticket.events.todo_done] to = "Done"`, and a synced ticket todo for 65:
  - `done_on_a_ticket_todo_moves_the_ticket`: after `devkit todo done <id>`, poll until `ghfake` records the status mutation to `Done`.
  - `a_sync_completing_a_ticket_fires_nothing`: the tracker closes 65; after `devkit todo sync`, no mutation is recorded.
  - `done_from_outside_the_repository_warns`: run from a temp dir; no mutation, and stderr names the ticket.
  - `a_plain_todo_fires_nothing`.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run --test todo_ticket_sync -E 'test(fires) | test(moves_the_ticket) | test(outside)'`. Expected: FAIL.
- [ ] **Step 3: Implement**, with one call site in `Store`, so the CLI and the native-tool mirror both fire.
- [ ] **Step 4: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(todo): move tickets when their todo changes`

### Task 11: The ticket fence, summary and detail

**Files:**
- Modify: `crates/devkit-todo/src/layout.rs`, `crates/devkit-todo/src/render.rs`
- Modify: `src/bin/devkit/todo/mod.rs` (`offered_here`; `--expand` on `list` and `plan`), `src/bin/devkit/hook/todo.rs` (the injection's summary)
- Test: `crates/devkit-todo/tests/layout.rs`, `crates/devkit-todo/tests/render.rs`, `tests/todo_ticket_sync.rs`

**Interfaces:**
- Consumes: `Todo.ticket` (Task 2); `record::read_on(..)` and `IssueRecord::tracker_issue()` (existing).
- Produces:
  - `Layout::offered` gains `ticket_todo: Option<&str>`, the id of the ticket todo the caller's workspace record names; that todo is offered beside the workspace subtree.
  - `render::render_lists` and `render_plan` take `open_work: &HashMap<String, usize>` (ticket todo id to the count of open todos in its workspace subtree) and print `(<n> open)` after a ticket todo; with `expand`, the workspace's todos print indented beneath it.
  - `devkit todo list --expand` and `devkit todo plan --expand`.

- [ ] **Step 1: Write the failing tests:** in `tests/layout.rs`, `a_workspace_for_a_ticket_is_offered_its_ticket_todo`; in `tests/render.rs`, `a_ticket_todo_shows_its_open_work_count` and `expand_lists_the_workspace_todos_under_the_ticket`; in the binary tests, `ready_in_a_ticket_workspace_offers_the_ticket` (a `workspace setup` record for 65 and a synced ticket todo: `devkit todo ready` lists it) and `ready_without_a_record_is_unchanged`.
- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p devkit-todo --test layout --test render && cargo nextest run --test todo_ticket_sync -E 'test(ready)'`. Expected: FAIL.
- [ ] **Step 3: Implement.** The binary computes the count from the workspace's `repo.branch` subtree listing. A machine with no record for the ticket passes no count, and the line renders without one.
- [ ] **Step 4: Run the tests.** The step 2 command. Expected: PASS.
- [ ] **Step 5: Commit.** `feat(todo): offer and summarise a workspace's ticket`

### Task 12: Reference docs and the agent eval

**Files:**
- Modify: `plugin/skills/using-devkit/references/todo.md` (a "Tickets" section: what sync brings in, the `tracker` holder, `--expand`, the todo events)
- Modify: `plugin/skills/using-devkit/references/ticket.md` (`todo_start`, `todo_done`)
- Modify: the todo eval case's `questions.json` under `evals/` (one question: does a ticket todo an agent started survive a sync while the ticket has not moved; `proof` names `an_agents_start_survives_an_unmoved_ticket`)

- [ ] **Step 1: Write the eval question first**, with its expected answer and proof.
- [ ] **Step 2: Write the reference sections**: the behaviour an agent needs, as rules. Config meanings stay in the doc comments.
- [ ] **Step 3: Run the free checks.** `cargo nextest run --test docs_links --test docs_examples --test skills`. Expected: PASS. The billed eval (`devrun task eval --arg eval_case=<the todo case>`) runs only if Lev asks.
- [ ] **Step 4: Commit.** `docs(todo): document ticket sync for agents`
