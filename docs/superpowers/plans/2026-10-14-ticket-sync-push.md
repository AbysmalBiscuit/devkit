# Two-way Ticket Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A local change to a ticket's todo (start, done, cancel, parent, blocking) reaches the ticket when the repository opts in, and a change that would overwrite a tracker change the last sync did not see stops and asks first.

**Architecture:** Pure verdict functions in `devkit-todo` decide write, already-there, no-move or conflict from the baseline (`synced_status`, or the store's relation), the tracker's current value and the event's explicit `from`. The `devkit todo` verbs read the ticket, ask the verdict, write through the tracker writers (piece 2's relation writers, the status writer, a new GitHub close) and only then commit the store edit. New tickets (`ticket create`, `todo promote`) get their todo from a one-ticket run of piece 1's sync.

**Tech Stack:** Rust 2024 workspace, `gh` GraphQL through `github::Api`, Linear GraphQL through `linear::post_graphql`, `tests/common/ghfake.rs` and `tests/common/linearfake.rs` (piece 2) for end-to-end tests.

**Spec:** `docs/superpowers/specs/2026-10-14-ticket-sync-push-design.md`. Read it first; this plan argues from it.

## Prerequisites

This plan starts after these are merged, and uses their interfaces by the names their plans give:

- #356 (piece 1 plan tasks 9 to 12): `IssueEvent::TodoStart`/`TodoDone`, `fire_for` in `src/bin/devkit/todo/`, its "run from its repository" check.
- #355: Linear `find_tickets`, `tickets`, `status_names`.
- #363 (piece 2 plan tasks 1 to 5): `FieldDecl`, `Place::{Parent, BlockedBy}`, the field checks, `GithubFieldWriter` and its parent and blocked-by mutation builders, `ticket create` with fields.
- #364 (piece 2 plan tasks 6 and 7): `LinearFieldWriter`, `create_id`, `linearfake`, Linear `ticket create`.

If Linear (#355, #364) lags, do the GitHub half of each task and leave the Linear half as a follow-up ticket; every task below names its Linear part separately.

## Global Constraints

- No tracker status name in code: every status comes from `[ticket.events]` `to`/`from` or the `[ticket.sync]` status map.
- An unset `from`, or `"*"`, waives nothing for a push. Only named `from` entries waive a conflict.
- No hook path waits on the network: pushes run in the `devkit todo` verbs only, never in `Store::apply` or anything `todo/queue.rs` or the native-tool mirror reaches.
- A push writes the tracker first and commits the store edit second. On a conflict or a failed write, nothing changes on either side.
- Only todos with a `TicketRef`, and only edges joining two ticket todos, ever reach the tracker.
- `ticket create` and `todo promote` work whatever the opt-in list holds; only the refusals depend on `parent` pushing.
- Help text stays ASCII. `schema/devkit-config.json` is regenerated with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- Gate before each commit: `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings`, `devrun task fmt`.

## Review Focus

1. **A GitHub repository that opts into `status` without `[github] project`.** `done` must still close the issue; only the project move needs the project. Pinned in Task 5 by `done_closes_without_a_project`.
2. **A human caller with no terminal on stdin** (a piped `DEVKIT_CALLER=human`). A conflict must fail like an agent's, never block on a prompt. Pinned in Task 5 by `a_conflict_without_a_terminal_fails_instead_of_prompting`.
3. **A ticket todo whose ticket no longer reads back** (deleted, transferred, or outside the token's reach). The verb fails naming the key, and the todo is unchanged. Pinned in Task 5 by `an_unreadable_ticket_fails_and_changes_nothing`.
4. **A command mixing step todos and ticket todos** (`devkit todo done step ticket`). Steps commit at once whatever happens to the ticket. Pinned in Task 5 by `steps_commit_even_when_a_ticket_conflicts`.
5. **`--top` or `unblock` when the tracker already has no such link.** That is already-there, not an error. Pinned in Task 6 by `unblock_of_a_link_the_tracker_lacks_is_already_there`.

---

### Task 1: Config: the opt-in list and the cancelled event

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`TicketSyncConfig`, `TicketEventsConfig`, `IssueEvent`)
- Modify: `src/bin/devkit/issue/event.rs` (`EventArg`)
- Modify: `schema/devkit-config.json` (regenerated)

**Interfaces:**
- Produces:
  - `pub enum PushKind { Status, Parent, Blocking }`, serde `snake_case`, and `TicketSyncConfig.push: Vec<PushKind>` (default empty, key `push` under `[ticket.sync]`), with `pub fn pushes(&self, kind: PushKind) -> bool`.
  - `IssueEvent::TodoCancel`, configured as `[ticket.events.todo_cancel]`; `IssueEvent::ALL`, `TicketEventsConfig::get` and `EventArg` gain it.

- [ ] **Step 1: Write the failing tests** in `crates/devkit-config`: `push_defaults_to_empty`, `push_parses_status_parent_blocking`, `an_unknown_push_kind_is_a_parse_error`, `todo_cancel_event_parses`, and a doctest on `TicketSyncConfig` showing `push = ["status", "blocking"]` beside a rule.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-config push todo_cancel`. Expected: FAIL to compile.
- [ ] **Step 3: Implement** the type, the field and the event; regenerate the schema.
- [ ] **Step 4: Run** the same command, then `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit-config`. Expected: PASS, schema diff shows `push` and `todo_cancel`.
- [ ] **Step 5: Commit** `feat(config): opt ticket pushes in and add todo_cancel`.

---

### Task 2: The verdicts

**Files:**
- Create: `crates/devkit-todo/src/push.rs` (pure, no IO)
- Modify: `crates/devkit-todo/src/lib.rs` (`pub mod push;`)

**Interfaces:**
- Consumes: `devkit_common::tracker::tickets::{TicketStatus, CloseReason}` and `devkit_common::tracker::status::same_status` (`ticket.rs` already imports from there); `devkit_config::EventTransition`; `ticket::status_name`.
- Produces:
  - `pub enum LocalStatus { Start, Done, Cancel }`.
  - `pub enum Verdict { Write, Already, NoMove, Conflict { current: String, baseline: String } }`.
  - `pub fn status_verdict(local: LocalStatus, baseline: Option<&str>, current: &TicketStatus, event: Option<&EventTransition>, github: bool) -> Verdict`. `baseline` and the rendered `current` use `ticket::status_name` spelling.
  - `pub fn parent_verdict(baseline: Option<&str>, current: Option<&str>, target: Option<&str>) -> Verdict` (keys).
  - `pub fn block_verdict(baseline_has: bool, current_has: bool, other_relation: Option<&str>, add: bool) -> Verdict`.

Rules, from the spec's "The conflict check" and "Status equality":

| Case | Verdict |
|---|---|
| current equals the change's end state (Linear: `to`; GitHub `Done`: `closed:completed`; GitHub `Cancel`: `closed:not_planned`) | `Already` |
| current equals baseline, and the event's `from` names it or has no named entries | `Write` |
| current equals baseline, but named `from` entries exist and none matches | `NoMove` |
| current differs from baseline and a named `from` entry matches it | `Write` |
| anything else | `Conflict` |
| GitHub, no event configured: open issue | `Write` (the close) |
| GitHub, no event configured: closed with the other reason | `Conflict` |
| parent: current equals target | `Already`; current equals baseline: `Write`; else `Conflict` |
| block: current already as wanted | `Already`; Linear `other_relation` is some: `Conflict`; else `Write` |

`"*"` is not a named entry.

- [ ] **Step 1: Write the failing tests**, one per table row, named for the row: `already_at_to_is_already`, `unmoved_ticket_writes`, `unmoved_ticket_outside_named_from_does_not_move`, `tracker_move_inside_named_from_writes`, `tracker_move_with_star_from_conflicts`, `tracker_move_outside_from_conflicts`, `github_closed_completed_is_already_for_done`, `github_closed_not_planned_conflicts_for_done`, `github_open_without_event_writes`, `parent_third_ticket_conflicts`, `parent_already_set_is_already`, `block_with_another_linear_relation_conflicts`, `unblock_of_missing_link_is_already`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo push`. Expected: FAIL to compile.
- [ ] **Step 3: Implement** the three functions in `push.rs`.
- [ ] **Step 4: Run** the same command. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): decide ticket pushes against the last sync`.

---

### Task 3: Tracker writes for pushes

**Files:**
- Modify: `crates/devkit-common/src/tracker/status.rs` (`StatusWriter::set_status`)
- Modify: `crates/devkit-common/src/tracker/github_status.rs`, `linear_status.rs` (implement it; GitHub `close_issue`)
- Create: `crates/devkit-common/src/tracker/relations.rs`
- Modify: `crates/devkit-common/src/tracker/github_fields.rs`, `linear_fields.rs` (remove-mutation builders beside piece 2's add builders)
- Test: unit tests over recorded fixtures in each file, fixtures under `crates/devkit-common/src/tracker/fixtures/`

**Interfaces:**
- Consumes: piece 2's parent and blocked-by mutation builders in `github_fields.rs` and `linear_fields.rs`; `github::Api::graphql_or_gh`; `linear::post_graphql`.
- Produces:
  - `StatusWriter::set_status(&self, id: &str, to: &str) -> Result<()>`: writes `to` with no `from` check (the verdict already decided). A `to` the tracker lacks is the same error `move_status` gives.
  - `pub fn close_issue(api: &Api, repo: &Repo, number: u64, reason: CloseReason) -> Result<()>` in `github_status.rs`: `closeIssue(input: {issueId, stateReason: COMPLETED | NOT_PLANNED})`. Needs no `[github] project`.
  - `pub trait RelationWriter` with `set_parent(&self, child: &str, parent: Option<&str>, replace: bool) -> Result<()>`, `set_blocked(&self, blocked: &str, blocker: &str, add: bool) -> Result<()>`, `other_relation(&self, a: &str, b: &str) -> Result<Option<String>>`; enum `Relations { Github(..), Linear(..) }` delegating it, and `pub fn relations_for(kind: TrackerKind, repos: &Repos) -> Result<Relations>`.
  - GitHub: `addSubIssue(issueId: parent, subIssueId: child, replaceParent: replace)`, `removeSubIssue`, `addBlockedBy(issueId: blocked, blockingIssueId: blocker)`, `removeBlockedBy`. `other_relation` is always `None`.
  - Linear: `issueUpdate(id: child, input: {parentId})` (null to remove), `issueRelationCreate(type: blocks, issueId: blocker, relatedIssueId: blocked)`, `issueRelationDelete` of that relation, `other_relation` reads the pair's relations of any other type in either direction.

Linear facts, checked against its API with scratch issues (record them in `relations.rs`'s module doc): a pair holds one relation, and `issueRelationCreate` on a pair that has one succeeds and replaces it, whatever the type or direction; a `blocks` relation still reads as `blocks` after the blocker is completed. So `other_relation` returns the pair's relation, read from both issues' `relations` and `inverseRelations`, when it is anything but `blocks` from the blocker to the blocked, and `set_blocked` never sends a create over a pair `other_relation` reports.

- [ ] **Step 1: Write the failing tests:** `set_status_writes_without_a_from_check`, `close_issue_sends_state_reason_not_planned`, `close_issue_needs_no_project`, `add_sub_issue_sends_replace_parent_false`, `remove_sub_issue_names_both_ids`, `blocked_by_add_and_remove_name_the_blocker`, `linear_blocks_runs_from_blocker_to_blocked`, `linear_parent_null_removes_it`, `linear_other_relation_finds_related_in_either_direction`, `linear_other_relation_counts_a_reversed_blocks`, `linear_resolved_blocks_is_not_another_relation`.
- [ ] **Step 2: Run** `cargo nextest run -p devkit-common relations set_status close_issue`. Expected: FAIL to compile.
- [ ] **Step 3: Implement.** Confirm `closeIssue`, `removeSubIssue` and `removeBlockedBy` input names with `gh api graphql` introspection before recording fixtures.
- [ ] **Step 4: Run** the same command. Expected: PASS.
- [ ] **Step 5: Commit** `feat(tracker): write status, close, parent and blockers for pushes`.

---

### Task 4: One-ticket sync

**Files:**
- Modify: `src/bin/devkit/todo/tickets.rs`
- Test: `tests/todo_ticket_push.rs` (new); move `issue`, `graphql`, `two_issues`, `two_issues_in` from `tests/todo_ticket_sync.rs` into `tests/common/tickets.rs` and use them from both files

**Interfaces:**
- Consumes: `ticket::plan`, `tracker.tickets`, `tracker.status_names` (piece 1).
- Produces: `pub(crate) fn sync_one(store: &impl TodoStore, cwd: &Path, node: &str, key: &str) -> Result<Option<Todo>>`: fetches the one ticket, plans it against the node's existing todos, applies the plan as `tracker`, and returns its todo. `None` when the repository has no `[ticket.sync]` rules. An unmapped status is the same error a full sync gives.

- [ ] **Step 1: Write the failing tests:** `sync_one_adds_the_ticket_under_its_synced_parent`, `sync_one_leaves_other_tickets_alone` (the fake records one ticket read, not a rule query), `sync_one_without_rules_does_nothing`.
- [ ] **Step 2: Run** `cargo nextest run --test todo_ticket_push sync_one`. Expected: FAIL.
- [ ] **Step 3: Implement** `sync_one` by sharing `sync_tickets`'s plan-and-apply half; `sync_tickets` calls the same helper.
- [ ] **Step 4: Run** `cargo nextest run --test todo_ticket_push --test todo_ticket_sync`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): sync one ticket on demand`.

---

### Task 5: Status pushes from `start`, `done` and `cancel`

**Files:**
- Create: `src/bin/devkit/todo/push.rs` (the IO half: read, verdict, write, prompt, report)
- Modify: `src/bin/devkit/todo/mod.rs` (`Start`, `Done`, `Cancel` gain `--overwrite-tracker`; route ticket todos through `push.rs`)
- Modify: #356's `fire_for` (skip todos `push.rs` handled)
- Modify: `plugin/skills/using-devkit/references/todo.md`, `ticket.md`
- Test: `tests/todo_ticket_push.rs`

**Interfaces:**
- Consumes: Tasks 1 to 4; #356's repository check from `fire_for` (extract it as `pub(crate) fn ticket_checkout(cwd: &Path, todo: &Todo) -> Option<PathBuf>` and use it from both); `devkit_common::caller::caller()`; `writer_for`, `close_issue`.
- Produces: `pub(crate) fn push_status(store: &Store, cwd: &Path, todo: &Todo, local: LocalStatus, overwrite: bool) -> Result<Pushed>` with `pub(crate) enum Pushed { Wrote(String), Already, NotMoved(String), Local }` (`Local`: not opted in, no event and not GitHub, or outside the repository, which also prints `devkit todo: ticket <key> not moved: run from its repository`).

Flow per id, in command order: a step todo commits as today. A ticket todo with `status` pushing, inside its repository: read the ticket (`tracker.tickets(&[key])`), `status_verdict`, then:
- `Write`: GitHub `Done`/`Cancel` close first, then `set_status(to)` when an event is configured; record the written status with `Edit::RecordSync`; commit the status edit.
- `Already`: record, commit.
- `NoMove`: commit, report `<key> is <current>, not in [ticket.events.<event>] from`.
- `Conflict`: with `--overwrite-tracker`, write as `Write`. Else, when `caller()` is human and stdin is a terminal, prompt `<key>: local <change>, tracker <current>, last synced <baseline>. Write over the tracker? [y/N]`; yes writes, no runs `sync_one` for the key and leaves the todo as the tracker has it. Otherwise exit 1 with `devkit todo: <key> changed on the tracker since the last sync: tracker <current>, last synced <baseline>, local <change>. Ask the user; rerun with --overwrite-tracker to write over it.`
- A read or write error stops the command with the key named and the todo unchanged.

On a stop, the message lists the ids already done and those not done. Without `status` pushing, #356's background firing runs unchanged.

- [ ] **Step 1: Write the failing tests**, GitHub project via `ghfake`, `[ticket.sync] push = ["status"]`, `[ticket.events.todo_done] to = "Done"`, a synced ticket todo for 65 with `synced_status` `"Todo"`:
  - `done_closes_then_moves_and_records_the_status`: mutation log has `closeIssue` with `COMPLETED` before the field set; the todo is completed with `synced_status` `"Done"`.
  - `done_closes_without_a_project` (Review Focus 1): no `[github] project`, no event; `closeIssue` runs and the command succeeds.
  - `cancel_closes_as_not_planned`.
  - `a_tracker_move_conflicts_for_an_agent`: tracker shows `"Backlog"`; exit 1, stderr has the three values and `--overwrite-tracker`, no mutation, todo still pending.
  - `a_tracker_move_inside_named_from_writes`: `from = ["Todo", "In review"]`, tracker `"In review"`.
  - `overwrite_tracker_writes_over_a_conflict`.
  - `a_conflict_without_a_terminal_fails_instead_of_prompting` (Review Focus 2): `DEVKIT_CALLER=human`, stdin not a tty.
  - `an_unreadable_ticket_fails_and_changes_nothing` (Review Focus 3).
  - `steps_commit_even_when_a_ticket_conflicts` (Review Focus 4): `done <step> <ticket>`: the step is completed, the ticket todo is not, stderr names which.
  - `done_from_another_repository_commits_and_warns`.
  - `without_push_done_fires_the_background_event` (#356's path untouched).
  - Linear, via `linearfake`: `linear_done_sets_the_state_by_name`.
- [ ] **Step 2: Run** `cargo nextest run --test todo_ticket_push`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement** `push.rs` and wire the three verbs; extract `ticket_checkout`.
- [ ] **Step 4: Update the references:** `todo.md` gets a "Ticket todos that push" section (what pushes, the conflict message and what an agent does with it: ask the user, then `--overwrite-tracker` only on their word), `ticket.md` documents `push` and `todo_cancel`.
- [ ] **Step 5: Run** the gate. Expected: green.
- [ ] **Step 6: Commit** `feat(todo): push ticket status changes and stop on conflicts`.

---

### Task 6: Parent and blocking pushes, and the refusals

**Files:**
- Modify: `src/bin/devkit/todo/push.rs`, `src/bin/devkit/todo/mod.rs` (`Move`, `Block`, `Unblock`, `Add`; `--overwrite-tracker` on `Move`, `Block`, `Unblock`)
- Modify: `plugin/skills/using-devkit/references/todo.md`
- Test: `tests/todo_ticket_push.rs`

**Interfaces:**
- Consumes: Tasks 1 to 3 and 5 (`ticket_checkout`, the conflict prompt and message).
- Produces: `pub(crate) fn push_parent(store, cwd, child: &Todo, parent: Option<&Todo>, overwrite: bool) -> Result<Pushed>` and `pub(crate) fn push_block(store, cwd, blocked: &Todo, blocker: &Todo, add: bool, overwrite: bool) -> Result<Pushed>`.

Rules, from the spec's "Whose config", "Parent", "Blocking" and "Making tickets":
- An edge or parent change between two ticket todos made outside their repository is refused, opted in or not: `devkit todo: <key> and <key> are tickets of <repo>; change their link from a checkout of it, or the next sync reverts it.`
- Inside the repository but not opted in for that kind: refused the same way, naming the `push` key, since the next sync would revert it.
- Opted in: read both tickets, verdict, write through `RelationWriter` (`replace` only with `--overwrite-tracker`), commit the store edit.
- Tickets on two trackers: the edge stays local.
- With `parent` pushing: `todo add --parent <ticket todo>`, and `todo move <non-ticket> --parent <ticket todo>`, are refused naming `ticket create` and `todo promote`; `todo move <ticket todo> --parent <non-ticket>` is refused naming why. Children already under a ticket todo are untouched.

- [ ] **Step 1: Write the failing tests:** `move_under_a_ticket_makes_a_sub_issue`, `move_top_removes_the_tracker_parent`, `a_third_parent_on_the_tracker_conflicts`, `overwrite_sends_replace_parent_true`, `block_between_tickets_adds_blocked_by`, `unblock_of_a_link_the_tracker_lacks_is_already_there` (Review Focus 5), `a_relation_edit_outside_the_repository_is_refused`, `a_relation_edit_without_the_opt_in_is_refused`, `add_under_a_ticket_is_refused_naming_promote`, `move_a_step_under_a_ticket_is_refused`, `a_step_blocked_by_a_ticket_stays_local`, Linear: `linear_block_with_a_related_pair_conflicts`.
- [ ] **Step 2: Run** `cargo nextest run --test todo_ticket_push`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement** and wire the verbs.
- [ ] **Step 4: Update** `todo.md` with the refusals and what to run instead.
- [ ] **Step 5: Run** the gate. Expected: green.
- [ ] **Step 6: Commit** `feat(todo): push ticket parents and blockers`.

---

### Task 7: `todo promote`, and a todo for every created ticket

**Files:**
- Modify: `src/bin/devkit/issue/create.rs` (expose the create flow; run `sync_one` after a create)
- Modify: `src/bin/devkit/todo/mod.rs` (`Promote`)
- Modify: `plugin/skills/using-devkit/references/todo.md`, `ticket.md`
- Test: `tests/todo_ticket_push.rs`, `tests/issue_create.rs`

**Interfaces:**
- Consumes: Task 4 `sync_one`; piece 2's create flow and field checks; `Edit::{Block, Unblock, Describe}`, cancel.
- Produces:
  - `pub(crate) fn create(dir: &Path, config: Option<&Path>, title: &str, body: Option<&str>, vars: &VarArgs, fields: &FieldArgs) -> Result<Created>` in `create.rs`, `pub(crate) struct Created { pub key: String, pub url: String }`; the `ticket create` verb calls it, prints the URL, then runs `sync_one` when the repository has rules and reports a sync error after the URL.
  - `Promote { id: String, #[arg(long)] ticket: Option<String>, body, --arg, --none, --arg-file as ticket create }`.

Promote, from the spec: refuse a todo that already has a ticket. Without `--ticket`, `create` with the todo's text as title and the given body and fields (the parent field's level decides when it is missing, and the message names the workspace's ticket from its issue record and that ticket's parent as likely values). With `--ticket`, read that ticket instead of creating. Then `sync_one` adds the ticket's todo on the repository node; every todo blocked by the old todo is unblocked from it and blocked by the new one; the old todo's own blockers are copied to the new one; the old todo's text becomes the ticket's key and it is cancelled. Its children stay where they are.

- [ ] **Step 1: Write the failing tests:** `promote_creates_the_ticket_and_rewires_edges`, `promote_keeps_the_old_todos_children_in_place`, `promote_cancels_the_old_todo_naming_the_key`, `promote_ticket_links_an_existing_ticket_without_creating`, `promote_without_a_required_parent_names_the_likely_parents`, `promote_of_a_ticket_todo_is_refused`, `ticket_create_adds_its_todo_when_the_repository_syncs`, `ticket_create_prints_the_url_when_the_sync_fails`.
- [ ] **Step 2: Run** `cargo nextest run --test todo_ticket_push --test issue_create`. Expected: the new tests FAIL.
- [ ] **Step 3: Implement** `create`, the `sync_one` call after it, and `Promote`.
- [ ] **Step 4: Update** `todo.md` (when to promote, `--ticket` as the recovery after a create whose response was lost) and `ticket.md` (a created ticket gets its todo).
- [ ] **Step 5: Run** the gate. Expected: green.
- [ ] **Step 6: Commit** `feat(todo): promote a todo to a ticket`.
