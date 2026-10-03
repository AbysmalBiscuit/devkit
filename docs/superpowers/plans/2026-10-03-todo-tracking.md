# Todo Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** devkit keeps agent todo lists in its own store, injects them into agent context, attributes sub-agent claims, and mirrors the harnesses' native task and plan tools, all without anything installed beyond devkit.

**Architecture:** A new crate, `devkit-todo`, holds the model, the claim rule, node naming, the built-in JSON store, rendering and list diffing, all IO-light and unit-tested. The `devkit` binary adds a `devkit todo` subcommand and wires the hook verbs: context injection through `devkit todo context`, claim attribution in the pre-tool-use shell guard, release in `subagent-stop` and `session-end`, and native capture in `post-tool-use`.

**Tech Stack:** Rust 2024, `devkit_common::store` (flock-guarded JSON), `devkit_command::Analysis`, `pabal` 0.2 hook payloads, `chrono`, clap, nextest.

**Spec:** `docs/superpowers/specs/2026-10-03-todo-tracking-design.md`. Read it before any task; this plan argues from it.

## Global Constraints

- The feature is named `todo` everywhere: crate `devkit-todo`, CLI `devkit todo`, types `Todo`/`TodoStore`. Never `task`, which is devrun's.
- No `[todo]` config key in this issue.
- Node strings match alacritree's `alacritree_tasks::scope::node` byte for byte.
- Hook rules (AGENTS.md): no `hook` verb exits 2, and only `pre-tool-use` writes stdout. `devkit todo context` is a separate command, so it may print.
- Every failure in injection, capture and release is silent and leaves the verdict unchanged. Only a `Claimed` result in the shell guard denies.
- The store loads through `store::with_lock_strict` only.
- Help text stays ASCII. Errors use `anyhow` with `.context()`.
- Tests take scratch dirs from `tempfile` and isolate `HOME`/`XDG_STATE_HOME` with `tests/common/testenv.rs::isolated`.
- Commit through `devrun task commit --arg coauthors=... --arg commit_subject=... --arg files=...`. The gate before each commit is `cargo nextest run --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`; `devrun task fmt` runs before the final commit.

## Review Focus

1. `devkit todo` run outside any repository: `scope` prints `global` and `add` lands there. Pinned in Task 5.
2. `devkit todo` run by an agent caller with no harness session variable (a CI script): the actor is the literal holder `agent`, and the node is the workspace node. Pinned in Task 5.
3. A description containing newlines or tabs: stored and rendered as one line, with whitespace runs collapsed to single spaces. Pinned in Task 3.
4. A hook payload with no `session_id`: `todo context`, capture and release do nothing and print nothing. Pinned in Tasks 6 and 10.
5. An id that does not exist: every verb fails with `no todo <id>` and exit 1, and the store is untouched. Pinned in Task 5.

---

### Task 1: Crate, holders and the claim rule

**Files:**
- Create: `crates/devkit-todo/Cargo.toml`, `crates/devkit-todo/src/lib.rs`, `crates/devkit-todo/src/holder.rs`, `crates/devkit-todo/src/transition.rs`
- Modify: `Cargo.toml` (workspace `members` and `[workspace.dependencies]`)

**Interfaces:**
- Produces:
  - `pub struct Holder(String)` with `Holder::new(impl Into<String>)`, `Holder::human()`, `Holder::is_human(&self)`, `Deref<Target = str>`, `Display`, serde as a plain string.
  - `pub fn covers(&self, other: &Holder) -> bool`: true when equal, when `other` starts with `self/`, or when `self` is `human`.
  - `pub enum Status { Pending, InProgress { by: Holder }, Completed { by: Option<Holder> }, Cancelled { by: Option<Holder> } }` and `pub enum StatusKind { Pending, InProgress, Completed, Cancelled }`. Serde: internally tagged, `#[serde(tag = "status", rename_all = "snake_case")]`.
  - `Status::kind(&self) -> StatusKind`.
  - `pub struct Claimed { pub by: Holder }`, implementing `std::error::Error` with the message `todo is in progress by {by}`.
  - `pub fn transition(current: &Status, to: StatusKind, actor: &Holder) -> Result<Option<Status>, Claimed>` in `transition.rs`.

- [ ] **Step 1: Write the failing tests** in `transition.rs`, one `#[test]` per row:

```rust
fn h(s: &str) -> Holder { Holder::new(s) }
fn ip(s: &str) -> Status { Status::InProgress { by: h(s) } }

#[test] fn pending_to_in_progress_records_the_actor() {
    assert_eq!(transition(&Status::Pending, StatusKind::InProgress, &h("S")), Ok(Some(ip("S"))));
}
#[test] fn the_session_repeating_its_sub_agents_claim_keeps_the_sub_agent() {
    assert_eq!(transition(&ip("S/a1"), StatusKind::InProgress, &h("S")), Ok(None));
}
#[test] fn a_sub_agent_takes_over_its_parents_claim() {
    assert_eq!(transition(&ip("S"), StatusKind::InProgress, &h("S/a1")), Ok(Some(ip("S/a1"))));
}
#[test] fn a_sibling_conflicts() {
    assert_eq!(transition(&ip("S/a1"), StatusKind::InProgress, &h("S/a2")), Err(Claimed { by: h("S/a1") }));
    assert_eq!(transition(&ip("S/a1"), StatusKind::Completed, &h("S/a2")), Err(Claimed { by: h("S/a1") }));
    assert_eq!(transition(&ip("S/a1"), StatusKind::Pending, &h("S/a2")), Err(Claimed { by: h("S/a1") }));
}
#[test] fn finishing_under_a_covering_actor_credits_the_claimant() {
    assert_eq!(transition(&ip("S/a1"), StatusKind::Completed, &h("S")),
               Ok(Some(Status::Completed { by: Some(h("S/a1")) })));
}
#[test] fn finishing_unclaimed_credits_the_actor() {
    assert_eq!(transition(&Status::Pending, StatusKind::Cancelled, &h("S")),
               Ok(Some(Status::Cancelled { by: Some(h("S")) })));
}
#[test] fn human_overrides_any_claim() {
    assert_eq!(transition(&ip("S/a1"), StatusKind::Pending, &Holder::human()), Ok(Some(Status::Pending)));
}
#[test] fn same_kind_is_a_no_op_outside_in_progress() {
    let done = Status::Completed { by: Some(h("S")) };
    assert_eq!(transition(&done, StatusKind::Completed, &h("T")), Ok(None));
    assert_eq!(transition(&Status::Pending, StatusKind::Pending, &h("T")), Ok(None));
}
#[test] fn undone_drops_the_holder() {
    let done = Status::Completed { by: Some(h("S")) };
    assert_eq!(transition(&done, StatusKind::Pending, &h("S")), Ok(Some(Status::Pending)));
}
```

Add to `holder.rs`: `covers` is true for `("S","S")`, `("S","S/a1")`, `("human","X")`, and false for `("S/a1","S")`, `("S","S2")`, `("S/a1","S/a2")`.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run -p devkit-todo`
Expected: compile errors for the missing `transition`, `Holder` and `Status`.

- [ ] **Step 3: Implement `Holder`, `Status`, `StatusKind`, `Claimed` and `transition`**

Rules, in order:
1. From `InProgress { by: a }`:
   - to `InProgress` when `actor.covers(a)`: `Ok(None)`;
   - to `InProgress` when `a.covers(actor)`: hand over, `Ok(Some(InProgress { by: actor }))`;
   - any change when `!actor.covers(a)`: `Err(Claimed { by: a })`.
2. Otherwise, the same kind as the current status is `Ok(None)`.
3. To `Pending` gives `Pending`.
4. To `InProgress` gives `{ by: actor }`.
5. To `Completed` or `Cancelled` gives `by: Some(a)` when leaving `InProgress { by: a }`, else `Some(actor)`.

Crate deps: `anyhow`, `serde`, `serde_json`, `chrono`, `devkit-common`, `devkit-vcs`; dev-deps `tempfile`. Add `devkit-todo = { path = "crates/devkit-todo" }` to `[workspace.dependencies]` and the crate to `members`.

- [ ] **Step 4: Run them and confirm they pass**

Run: `cargo nextest run -p devkit-todo`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): add holders and the claim rule`.

---

### Task 2: Nodes and places

**Files:**
- Create: `crates/devkit-todo/src/node.rs`
- Modify: `crates/devkit-common/src/vcs.rs` (two accessors on `Checkout`)

**Interfaces:**
- Consumes: `devkit_vcs::{Worktree, DETACHED}`, `devkit_common::vcs::Checkout`.
- Produces:
  - `pub const GLOBAL: &str = "global"`
  - `pub enum Harness { Claude, Codex }`, with `prefix() -> &'static str` returning `"claude"` or `"codex"`. The crate does not depend on pabal; the binary maps `AnyHarness` to `Harness` in `src/bin/devkit/hook/todo.rs` (`fn harness_of(AnyHarness) -> Option<Harness>`, `None` for Cursor and Antigravity).
  - `pub struct SessionRef { pub harness: Harness, pub id: String }`
  - `pub enum Place { Global, Project { repo: String }, Workspace { repo: String, branch: String } }`
  - `pub fn sanitize(segment: &str) -> String`
  - `pub fn node(place: &Place, session: Option<&SessionRef>) -> String`
  - `pub fn session_from_env(get: impl Fn(&str) -> Option<String>) -> Option<SessionRef>`
  - `pub fn visible_nodes(place: &Place, session: Option<&SessionRef>) -> Vec<String>`, deepest first.
  - `pub fn place_from(main: Option<&Worktree>, here: Option<&Worktree>) -> Place`
  - `pub fn place_of(checkout: &Checkout) -> Place`
  - `pub struct Filter { pub nodes: Vec<NodeMatch> }`, `pub enum NodeMatch { Exact(String), Subtree(String) }`, and `Filter::matches(&self, project: Option<&str>) -> bool`.
  - On `Checkout`: `pub fn worktrees(&self) -> &[Worktree]` and `pub fn here(&self) -> Option<&Worktree>`.

- [ ] **Step 1: Write the failing tests**

Port alacritree's `scope.rs` tests verbatim, renaming `alacritree` to `devkit`. The vectors to keep:
- `node(&ws("alacritree","master"), Some(&codex("0199a"))) == "alacritree.master.codex-0199a"`
- `"my.repo"` / `"feat/v1.2"` / claude `"a.b/c"` gives `"my-repo.feat-v1-2.claude-a-b-c"`
- a session below a project is dropped
- codex wins over claude in `session_from_env`
- blank ids are no session

Port `a_subtree_keeps_other_repositories_out` and `a_task_without_a_project_is_global` from `alacritree_tasks/src/lib.rs`. Add:
- `visible_nodes(ws("r","main"), Some(claude "s")) == ["r.main.claude-s", "r.main", "r", "global"]`
- `place_from` with main `/x/devkit` and here branch `feat/a` gives `Workspace { repo: "devkit", branch: "feat/a" }`
- main `/x/devkit.git` with `bare: true` gives repo `"devkit"`
- here with branch `DETACHED` at `/x/wt-3` gives branch `"wt-3"`
- `None` main gives `Global`
- `Some(main)`, `None` here gives `Project`

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run -p devkit-todo node`
Expected: FAIL to compile, with the `node` module missing.

- [ ] **Step 3: Implement**

The bodies follow alacritree's `scope.rs` and `facts::place_from`. `place_of` passes `checkout.worktrees().first()` and `checkout.here()`. The two `Checkout` accessors read the existing `Resolved { worktrees, here }`.

- [ ] **Step 4: Run them and confirm they pass**

Run: `cargo nextest run -p devkit-todo node && cargo nextest run -p devkit-common vcs`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): name todo nodes the way alacritree does`.

---

### Task 3: The store trait and the built-in store

**Files:**
- Create: `crates/devkit-todo/src/builtin.rs`
- Modify: `crates/devkit-todo/src/lib.rs`
- Test: `crates/devkit-todo/tests/builtin.rs`

**Interfaces:**
- Consumes: Task 1 (`Status`, `StatusKind`, `Holder`, `transition`), Task 2 (`Filter`).
- Produces:

```rust
pub struct Todo { pub id: String, pub description: String, pub status: Status,
    pub parent: Option<String>, pub order: Option<i64>, pub project: Option<String>,
    pub entry: Option<String>, pub modified: Option<String> }
pub struct NewTodo { pub project: Option<String>, pub description: String,
    pub parent: Option<String>, pub order: Option<i64> }
pub enum Edit {
    SetStatus { id: String, to: StatusKind, actor: Holder },
    Describe { id: String, description: String },
    Move { id: String, parent: Option<String>, order: i64 },
    Reorder { id: String, order: i64 },
    ReleaseAll { holder: Holder },
    Purge(String),
}
pub trait TodoStore: Send + Sync {
    fn list(&self, filter: &Filter) -> anyhow::Result<Vec<Todo>>;
    fn add(&self, todo: NewTodo) -> anyhow::Result<String>;
    fn apply(&self, edit: &Edit) -> anyhow::Result<()>;
}
pub const ORDER_GAP: i64 = 1024;
pub fn one_line(text: &str) -> String;
pub struct BuiltinStore { dir: PathBuf }
impl BuiltinStore {
    pub fn at(dir: PathBuf) -> Self;   // files: dir/todos.json, dir/todo.lock
    pub fn default_dir() -> PathBuf;   // paths::state_dir().join("todo")
}
```

`apply` returns an error whose root cause downcasts to `Claimed` on a conflict, and the error `no todo {id}` for an unknown id.

- [ ] **Step 1: Write the failing tests** in `tests/builtin.rs`

```rust
#[test] fn ids_are_sequential_and_sort_numerically() // add 10 todos; list ids in order == "1".."10" with "10" last
#[test] fn add_without_order_appends_after_the_last_sibling() // orders 1024, 2048; a child of "1" gets 1024
#[test] fn descriptions_are_one_line() // "a\n\tb  c" stored as "a b c"
#[test] fn set_status_goes_through_transition() // S/a1 starts "1"; S/a2 start -> err.downcast_ref::<Claimed>().by == "S/a1"
#[test] fn release_all_returns_covered_claims_to_pending() // S/a1 and T hold; ReleaseAll{S} -> S/a1's pending, T's untouched
#[test] fn purge_removes_the_record() // list no longer contains it; Purge of a missing id -> "no todo 7"
#[test] fn unknown_ids_are_refused() // Describe{"99"} -> err contains "no todo 99"; file unchanged
#[test] fn a_corrupt_file_aborts_and_stays() // write "{not json" to todos.json; add() errs; file bytes unchanged
#[test] fn concurrent_adds_all_land()
    // 8 threads, each with its own BuiltinStore::at(same dir), add 25 todos each -> 200 todos, 200 distinct ids.
    // Each store opens its own lock file handle, so the threads contend on the lock as processes would.
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run -p devkit-todo --test builtin`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

`Doc { version: u32, next_id: u64, todos: BTreeMap<u64, Todo> }` implements `store::Document`:
- `label()` is `"todo store"`;
- `salvage` delegates to `salvage_map(raw, "todos", |k| k.parse().ok())` and is never called on this path.

Every method runs inside `store::with_lock_strict(&lock, &data, |doc| ...)`:
- `add` sets `entry` and `modified` with `chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)`.
- `ReleaseAll` skips a `human` holder.
- `list` excludes nothing by status. Callers filter.

- [ ] **Step 4: Run them and confirm they pass**

Run: `cargo nextest run -p devkit-todo`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): add the built-in todo store`.

---

### Task 4: Rendering, the guide and the list digest

**Files:**
- Create: `crates/devkit-todo/src/render.rs`

**Interfaces:**
- Consumes: `Todo`, `Status`, `visible_nodes`.
- Produces:
  - `pub fn render_lists(nodes: &[String], todos: &[Todo], viewer: &Holder) -> String`
  - `pub fn guide(own_node: &str) -> String`
  - `pub fn digest(lists: &str) -> String`: a 16-hex-digit stable hash. Use `std::hash::DefaultHasher` the way `rules::fired_path` does, over the bytes.

Exact guide copy (the eval in Task 12 grades it):

```text
Todo list, kept by devkit. Write your own todos to `{own_node}`.
- Track any work of three or more steps here before you start it.
- If you have a built-in task or plan tool, use it; devkit mirrors it here. Otherwise use `devkit todo add "<text>"` (prints the id), `devkit todo start <id>`, `devkit todo done <id>` and `devkit todo cancel <id>`.
- A sub-agent claims a todo with `devkit todo start <id>` before working on it.
- Ids are the numbers in parentheses.
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn open_todos_render_in_tree_order_under_their_node()
    // node "r.main": "1" pending, "2" child of "1" -> "## r.main\n- [ ] one (1)\n  - [ ] two (2)\n"
#[test] fn another_holders_claim_is_named() // InProgress{by:"S/a1"}, viewer "S/a2" -> "- [ ] x (3, in progress: a1)"
#[test] fn own_claim_reads_in_progress() // viewer "S/a1" -> "(3, in progress)"
#[test] fn finished_todos_collapse_to_a_count() // 2 completed + 1 cancelled + 1 pending -> line "2 done, 1 cancelled"
#[test] fn a_node_with_nothing_open_is_left_out()
#[test] fn the_guide_names_the_node() // guide("r.main.claude-s").contains("`r.main.claude-s`")
#[test] fn digest_is_stable() // digest("a") == digest("a"), != digest("b"), len 16
```

The name in `in progress: <name>` is the holder's last `/` segment.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run -p devkit-todo render`
Expected: FAIL to compile.

- [ ] **Step 3: Implement**

Tree order follows alacritree's `tree::rows`: siblings sort by `order`, then by `entry`; a parent missing from the listing puts its child at the top level. Port only the ordering that function needs, not the tab's view code.

- [ ] **Step 4: Run them and confirm they pass**

Run: `cargo nextest run -p devkit-todo render`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): render todo lists and the agent guide`.

---

### Task 5: `devkit todo` CLI

**Files:**
- Create: `src/bin/devkit/todo.rs`
- Modify: `src/bin/devkit/main.rs` (`mod todo;`, a `Cmd::Todo(todo::TodoCli)` variant, and dispatch), root `Cargo.toml` (`devkit-todo.workspace = true`)
- Test: `tests/todo_cli.rs`

**Interfaces:**
- Consumes: Tasks 1-4, `caller::caller()`, `Checkout::at`.
- Produces:
  - `pub fn run(cli: TodoCli) -> Result<()>`
  - `pub(crate) fn actor_from_env(caller: Caller, get: impl Fn(&str) -> Option<String>) -> Holder`, giving the session id for an agent with a session, `agent` for one without, and `human` for a human.
  - `pub(crate) fn store() -> BuiltinStore`, using `BuiltinStore::at(BuiltinStore::default_dir())`.
  - `pub(crate) fn alacritree_json(todo: &Todo) -> Option<serde_json::Value>`: `None` for `Cancelled`; `started` is `InProgress`; `status` is `"pending"` or `"completed"`; the keys are `id`, `description`, `status`, `started`, `parent`, `order`, `project`, `entry` and `modified`.

The verbs and flags are exactly the spec's CLI table, plus `context`, which Task 6 adds. `list` without flags uses `visible_nodes` for the caller's place and session. `--subtree` and `--node` build a `Filter`. `--all` lists every todo. Text output is `render_lists` with completed todos shown in full. `--json` prints an array of `alacritree_json`.

- [ ] **Step 1: Write the failing tests** in `tests/todo_cli.rs`

Every test runs `env!("CARGO_BIN_EXE_devkit")` through `testenv::isolated` and `scrub_identity`, in a `git init -b main` temp repo named `proj` unless stated otherwise.

```rust
#[test] fn add_prints_the_id_and_list_shows_it() // CLAUDE_CODE_SESSION_ID=s1: add "one" -> stdout "1\n"; list contains "- [ ] one (1)" under "## proj.main.claude-s1"
#[test] fn scope_prefers_codex() // both vars set -> "proj.main.codex-<codex id>\n"
#[test] fn outside_a_repository_everything_is_global() // cwd = bare temp dir: scope -> "global"; add lands there
#[test] fn an_agent_without_a_session_acts_as_agent() // DEVKIT_CALLER=agent, no session vars: start 1 -> list --json shows started; scope -> "proj.main"
#[test] fn a_sibling_start_is_refused_naming_the_holder() // DEVKIT_CALLER=human start ok; then CLAUDE_CODE_SESSION_ID=s2 start -> exit 1, stderr "in progress by human"
#[test] fn unknown_ids_fail_cleanly() // done 99 -> exit 1, stderr "no todo 99"
#[test] fn purge_needs_a_human() // DEVKIT_CALLER=agent purge 1 -> exit 1, stderr names "terminal"; DEVKIT_CALLER=human -> exit 0, gone
#[test] fn json_matches_alacritrees_task_shape() // cancel 2 -> absent; started todo has "started": true and "status": "pending"
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run --test todo_cli`
Expected: FAIL, with `todo` not a subcommand.

- [ ] **Step 3: Implement `todo.rs` and register it in `main.rs`**

The `purge` refusal message is: `devkit todo purge needs a person at a terminal; agents cancel instead (devkit todo cancel <id>)`.

- [ ] **Step 4: Run them and confirm they pass**

Run: `cargo nextest run --test todo_cli && cargo nextest run --test completions --test cli_ergonomics`
Expected: PASS. The completions and help tests catch non-ASCII help.

- [ ] **Step 5: Commit** with subject `feat(todo): add the devkit todo command`.

---

### Task 6: Context injection

**Files:**
- Modify: `src/bin/devkit/todo.rs` (the `context` verb), `plugin/hooks/hooks.json`, `plugin/hooks/hooks-codex.json`
- Test: `tests/todo_context.rs`, and update `tests/hook_manifests.rs` if it pins the command lists

**Interfaces:**
- Consumes: Task 4's `render_lists`, `guide` and `digest`, `crate::hook::payload::Payload`, and pabal's `AnyView` and `AddContext`.
- Produces: `devkit todo context --harness <claude-code|codex> [--guide full|none] [--if-changed]`. It reads the hook payload on stdin.

Behavior:
- The node comes from the payload's `cwd` (via `place_of`), its `session_id` and `--harness`. With no `session_id`, it prints nothing.
- The viewer is `payload.holder()`.
- The lists are `render_lists(visible_nodes(..), todos, viewer)`. With no lists and `--guide none`, it prints nothing.
- `--if-changed`: the digest file is `state_dir()/todo/digests/<16-hex hash of the full holder>`. When it equals `digest(lists)`, print nothing. Every run that prints writes the digest.
- Output goes through pabal: `SessionStart`, `SubagentStart` and `UserPromptSubmit` views use `add_context(text)`. Any other event, which includes `PostCompact`, prints the plain text.
- Any error prints nothing and exits 0.

Manifest lines to add after the existing commands of each event:

| File | Event | Command |
|---|---|---|
| `hooks.json` | `SessionStart` (startup, resume, clear) | `devkit todo context --harness claude-code` |
| `hooks.json` | `PostCompact` | `devkit todo context --harness claude-code` |
| `hooks.json` | `SubagentStart` | `devkit todo context --harness claude-code` |
| `hooks.json` | `UserPromptSubmit` | `devkit todo context --harness claude-code --guide none --if-changed` |
| `hooks-codex.json` | `SessionStart`, the `startup\|resume\|clear` entry only (not the `compact` one) | `devkit todo context --harness codex` |
| `hooks-codex.json` | `PostCompact`, `SubagentStart` | `devkit todo context --harness codex` |
| `hooks-codex.json` | `UserPromptSubmit` | `devkit todo context --harness codex --guide none --if-changed` |

- [ ] **Step 1: Write the failing tests** in `tests/todo_context.rs`. Seed todos with the CLI from Task 5.

```rust
#[test] fn session_start_injects_the_guide_and_lists()
    // payload {"hook_event_name":"SessionStart","session_id":"s1","cwd":<proj>,"source":"startup"}
    // stdout parses as JSON; hookSpecificOutput.additionalContext contains "Write your own todos to `proj.main.claude-s1`" and "- [ ] one (1)"
#[test] fn an_unchanged_list_is_not_repeated_on_the_next_prompt()
    // run SessionStart, then UserPromptSubmit --guide none --if-changed -> empty stdout; add a todo; rerun -> non-empty
#[test] fn a_sub_agents_injection_does_not_suppress_the_parent()
    // SubagentStart with agent_id a1, agent_type "general-purpose" -> non-empty; then parent UserPromptSubmit --if-changed after a change -> non-empty
#[test] fn no_session_means_silence() // payload without session_id -> empty stdout, exit 0
#[test] fn post_compact_prints_plain_text() // PostCompact payload -> stdout starts with "Todo list, kept by devkit."
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run --test todo_context`
Expected: FAIL

- [ ] **Step 3: Implement the verb and edit both manifests**

- [ ] **Step 4: Run the context tests and the manifest and plugin layout tests**

Run: `cargo nextest run --test todo_context --test hook_manifests --test plugin_layout`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): inject todo lists into agent context`.

---

### Task 7: Release on sub-agent stop and session end

**Files:**
- Create: `src/bin/devkit/hook/todo.rs` (`pub(crate) fn release(payload: &Payload, holder: Holder)`)
- Modify: `src/bin/devkit/hook/mod.rs` (the `SubagentStop` and `SessionEnd` arms, beside `edit::release_subagent` and `edit::release_session`), `src/bin/devkit/hook/mod.rs` module list
- Test: `tests/todo_hooks.rs`

**Interfaces:**
- Consumes: `payload.subagent_holder()`, `payload.session_holder()`, and `Edit::ReleaseAll`.
- Produces: `hook::todo::release`, and a `fn to_todo_holder(h: &payload::Holder) -> devkit_todo::Holder` used by Tasks 8 and 10.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn subagent_stop_returns_its_claims_to_pending()
    // seed "1" InProgress{S/a1}, "2" InProgress{S}; run `devkit hook subagent-stop --harness claude-code`
    // with {"session_id":"S","agent_id":"a1","agent_type":"general-purpose"} -> "1" pending, "2" untouched, stdout empty
#[test] fn session_end_releases_the_session_and_its_sub_agents() // both pending
#[test] fn a_fork_releases_nothing() // agent_id without agent_type -> no change
```

Seed claims through the store directly: the root package takes `devkit-todo` as a dependency (Task 5), so tests call `BuiltinStore::at(<state>/todo)` and apply `SetStatus` with actor `S/a1`.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run --test todo_hooks release`
Expected: FAIL

- [ ] **Step 3: Implement**

Release runs before the existing lock release in both arms. Errors are discarded.

- [ ] **Step 4: Run them and confirm they pass**

Run: `cargo nextest run --test todo_hooks --test hook_exit_codes`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): release claims when an agent ends`.

---

### Task 8: Claim attribution in the shell guard

**Files:**
- Modify: `src/bin/devkit/hook/todo.rs`, `src/bin/devkit/hook/shell.rs:267-326`
- Test: `tests/todo_hooks.rs`

**Interfaces:**
- Consumes: `devkit_command::Analysis` (`invocations[].program`, `.args`), and `transition` via `BuiltinStore::apply`.
- Produces:
  - `pub(crate) fn status_edits(analysis: &Analysis) -> Vec<(StatusKind, String)>`. For each invocation whose program basename is `devkit` and whose first two known args are `todo` and one of `start|stop|done|undone|cancel`, it yields the verb's kind and every following known id argument. Unknown values are skipped.
  - `pub(crate) fn attribute(payload: &Payload, analysis: &Analysis) -> Option<String>`. It returns a block reason only on `Claimed`, formatted `devkit todo: todo {id} is in progress by {by}; pick another todo`. Store errors return `None`.

Changes in `shell.rs`:
- The early return at `if !commands_on && !writes_on && !settings.enabled` also requires `payload.subagent_holder().is_none()`.
- After the write stage and before the `if !blocks.is_empty()` deny, add: `if blocks.is_empty() && let Some(reason) = todo::attribute(payload, &analysis) { blocks.push(reason) }`.
- `attribute` does nothing unless `payload.subagent_holder()` is `Some`.

- [ ] **Step 1: Write the failing tests**

```rust
// unit, in hook/todo.rs
#[test] fn finds_status_verbs_and_ids() // "devkit todo start 3 && devkit todo done 4 5" -> [(InProgress,"3"),(Completed,"4"),(Completed,"5")]
#[test] fn ignores_other_devkit_commands() // "devkit todo add x; devkit locks list" -> []
// e2e, tests/todo_hooks.rs: pre-tool-use with a Bash payload carrying agent_id/agent_type
#[test] fn a_sub_agents_start_is_recorded_as_the_sub_agent() // repo with no [harness] config; after the hook, "1" is InProgress{S/a1}; stdout empty
#[test] fn a_siblings_start_is_denied_naming_the_holder() // second hook as a2 -> deny envelope whose reason contains "in progress by S/a1"
#[test] fn a_command_the_write_gate_denies_leaves_no_claim()
    // project with [harness] enforce_writes = true and file "f" held by another holder (lockm acquire as "other");
    // command "echo x > f && devkit todo start 1" -> denied; "1" still pending
#[test] fn the_parent_command_run_after_attribution_keeps_the_sub_agent()
    // after the hook, run `devkit todo start 1` with CLAUDE_CODE_SESSION_ID=S -> exit 0, still InProgress{S/a1}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run --test todo_hooks attribution && cargo nextest run --bin devkit hook::todo`
Expected: FAIL

- [ ] **Step 3: Implement `status_edits`, `attribute` and the `shell.rs` changes**

- [ ] **Step 4: Run the attribution tests and every shell guard suite**

Run: `cargo nextest run --test todo_hooks --test harness_guard --test harness_shell_writes --test hook_edit_verdict`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): attribute sub-agent claims in the shell guard`.

---

### Task 9: Probes for native capture

**Files:**
- Create: `tests/fixtures/todo/claude-task-create.jsonl`, `claude-task-update.jsonl`, `claude-task-deleted.jsonl`, `claude-todowrite.jsonl`, `claude-subagent-tasks.jsonl`, `codex-update-plan.jsonl`, `tests/fixtures/todo/README.md`

**Interfaces:**
- Produces: recorded `PostToolUse` payloads that Tasks 10 and 11 use as test input, with `transcript_path` and `cwd` replaced by placeholders.

- [ ] **Step 1: Record the Claude payloads.** Run headless `claude -p` from a scratch dir. Pass `--settings` with a `PostToolUse` hook for matcher `TaskCreate|TaskUpdate|TodoWrite` that runs `sh -c 'cat; echo' >> <file>`, and set `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`. Use three prompts:
  - create two tasks, set one `in_progress` then `completed`, and set the other `deleted`;
  - delegate one task to a general-purpose sub-agent, which creates and completes its own task;
  - (with `TodoWrite` allowed) write a three-item list, then rewrite it with one item done and one removed.
- [ ] **Step 2: Record the Codex payload.** Run `codex exec` with the same capture hook on `PostToolUse` matcher `update_plan`, and a prompt that updates its plan three times with a repeated step text.
- [ ] **Step 3: Write `README.md`.** For each fixture, give the harness version (`claude --version`, `codex --version`) and the prompt. Then record the answers the spec left open:
  - whether `TaskUpdate` accepted `deleted`;
  - whether sub-agent task ids restart at 1;
  - whether the Codex sub-agent payload carried the root `session_id`.
- [ ] **Step 4: Check the spec against the answers.** If an answer contradicts the spec's capture table, stop and report it before Task 10.
- [ ] **Step 5: Commit** with subject `test(todo): record native task tool payloads`.

---

### Task 10: Capture `TaskCreate` and `TaskUpdate`

**Files:**
- Create: `crates/devkit-todo/src/native.rs`
- Modify: `src/bin/devkit/hook/todo.rs` (`pub(crate) fn capture(payload: &Payload)`), `src/bin/devkit/hook/mod.rs` (a `PostToolUse` arm calling `todo::capture(p)` before `record_only`), the `PostToolUse` matchers in `plugin/hooks/hooks.json` (add `TaskCreate|TaskUpdate|TodoWrite`) and `hooks-codex.json` (add `update_plan`)
- Test: `tests/todo_hooks.rs`

**Interfaces:**
- Consumes: Task 9 fixtures, `BuiltinStore`, `to_todo_holder`.
- Produces in `native.rs`:

```rust
pub struct NativeMap { dir: PathBuf }   // dir/native.json, dir/native.lock
impl NativeMap {
    pub fn at(dir: PathBuf) -> Self;
    pub fn record(&self, harness: Harness, holder: &Holder, native: &str, todo: &str) -> anyhow::Result<()>;
    /// Holder first, then the holder's session (text before the first `/`).
    pub fn lookup(&self, harness: Harness, holder: &Holder, native: &str) -> anyhow::Result<Option<String>>;
    pub fn snapshot(&self, harness: Harness, holder: &Holder) -> anyhow::Result<Vec<(String, String)>>; // (step text, todo id)
    pub fn set_snapshot(&self, harness: Harness, holder: &Holder, steps: Vec<(String, String)>) -> anyhow::Result<()>;
}
```

The document is `{ version, ids: BTreeMap<String, String>, snapshots: BTreeMap<String, Vec<(String, String)>> }`. It is keyed `"{prefix}\u{1f}{holder}\u{1f}{native}"` and `"{prefix}\u{1f}{holder}"`, and goes through `with_lock_strict`.

Mapping:
- `TaskCreate` adds on the session node, using `tool_input.subject`, and records `tool_response.task.id`.
- `TaskUpdate` looks up `tool_input.taskId`.
  - `status` maps through `pending`, `in_progress`, `completed` and `deleted` to `StatusKind`, with actor `payload.holder()`.
  - A `subject` that differs from the stored description becomes `Describe`.
- A missing session, an unknown native id, a store error or `Claimed` all return silently.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn lookup_falls_back_to_the_session() // native.rs unit: record under "S", lookup as "S/a1" -> Some
#[test] fn a_reused_native_id_replaces_the_mapping() // record ("S","1")->"4" then ("S","1")->"9"; lookup -> "9"
// e2e, feeding claude-task-create.jsonl then claude-task-update.jsonl lines to `devkit hook post-tool-use --harness claude-code`:
#[test] fn task_create_and_update_mirror_into_the_store() // todo "alpha" exists on node proj.main.claude-<sid>, Completed{by:<sid>}
#[test] fn deleted_becomes_cancelled() // from claude-task-deleted.jsonl
#[test] fn a_sub_agents_native_tasks_are_attributed_to_it() // from claude-subagent-tasks.jsonl: Completed{by:"<sid>/<agent>"}
#[test] fn capture_writes_nothing_to_stdout() // every run above: stdout empty, exit 0
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run -p devkit-todo native && cargo nextest run --test todo_hooks capture`
Expected: FAIL

- [ ] **Step 3: Implement `NativeMap`, `capture` for the two tools, the `PostToolUse` arm and the matchers**

- [ ] **Step 4: Run them and confirm they pass**

Run: `cargo nextest run -p devkit-todo && cargo nextest run --test todo_hooks --test hook_manifests`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): mirror Claude Code task tools`.

---

### Task 11: List-replace capture for `update_plan` and `TodoWrite`

**Files:**
- Create: `crates/devkit-todo/src/diff.rs`
- Modify: `src/bin/devkit/hook/todo.rs`
- Test: `tests/todo_hooks.rs`

**Interfaces:**
- Consumes: `NativeMap::snapshot`/`set_snapshot`, and Task 9's `codex-update-plan.jsonl` and `claude-todowrite.jsonl`.
- Produces:

```rust
pub struct Step { pub text: String, pub status: StatusKind }
pub enum Change { Add { index: usize }, Cancel { todo: String }, Status { todo: String, to: StatusKind }, Reorder { todo: String, order: i64 } }
/// `previous` is (text, todo id, status) in the prior order.
pub fn diff(previous: &[(String, String, StatusKind)], next: &[Step]) -> Vec<Change>;
```

The algorithm is the spec's List-replace diffing:
1. Pair each step with the first unpaired equal text.
2. Unpaired previous steps are cancelled; unpaired new steps are added.
3. A paired step whose status changed gets `Status`.
4. A step whose index changed gets `Reorder` to `index * ORDER_GAP`.

Capture applies `Add` first, recording each new id into the next snapshot, then the rest in order.

Field names: Codex `update_plan` reads `tool_input.plan[].step` and `.status`. `TodoWrite` uses whatever Task 9 recorded. Claude's `TodoWrite` and Codex's `update_plan` both use `pending`, `in_progress` and `completed`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn repeated_steps_stay_distinct()
    // prev [("run tests","1",P),("build","2",P),("run tests","3",P)], next [run tests(C), run tests(P)]
    // -> Status{"1",Completed}, Cancel{"2"}, Reorder{"3",1024}
#[test] fn new_steps_are_added_at_their_index() // prev [], next [a,b] -> [Add{0}, Add{1}]
#[test] fn a_rename_is_a_cancel_and_an_add()
#[test] fn update_plan_sequence_mirrors_into_the_store() // e2e over codex-update-plan.jsonl: final store matches the last plan, both repeats present
#[test] fn a_sub_agents_first_list_leaves_the_parent_alone() // parent plan of 2 steps, then a sub-agent update_plan of 1 step -> parent's 2 todos still pending
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo nextest run -p devkit-todo diff && cargo nextest run --test todo_hooks plan`
Expected: FAIL

- [ ] **Step 3: Implement `diff` and the capture branch for both tools**

- [ ] **Step 4: Run them and confirm they pass**

Run: `cargo nextest run -p devkit-todo && cargo nextest run --test todo_hooks`
Expected: PASS

- [ ] **Step 5: Commit** with subject `feat(todo): mirror list-replace plan tools`.

---

### Task 12: Skill reference and evals

**Files:**
- Create: `plugin/skills/using-devkit/references/todo.md`, `evals/todo-guide/{render.sh,preamble.md,questions.json}`, `evals/scenarios/todo-unprompted/{prompt.md,scenario.json,fixture/...}`
- Modify: `plugin/skills/using-devkit/SKILL.md` (one pointer line to `references/todo.md`, in the style of the existing reference pointers)
- Test: `tests/skills.rs` and `tests/docs_links.rs` (existing suites that check skill references and links)

**Interfaces:**
- Consumes: the guide from Task 4, and the CLI behavior from Tasks 5-11.

`references/todo.md` covers what an agent needs and cannot read from `--help`:
- where its todos live (the node rule);
- claiming as a sub-agent;
- that native tools are mirrored;
- that cancelling keeps a record;
- that purge is for people.

It cites no flag meanings, since clap help owns those.

`evals/todo-guide/render.sh` prints `devkit todo context --harness claude-code` for a seeded store. `questions.json` asks, with the answer each one proves and the test that pins it:
- which node to write to (`todo_context::session_start_injects_the_guide_and_lists`);
- what to run before working on a parent's todo as a sub-agent (`todo_hooks::a_sub_agents_start_is_recorded_as_the_sub_agent`);
- how to drop a todo (`todo_cli::json_matches_alacritrees_task_shape`);
- whether to use a built-in task tool when it has one (`todo_hooks::task_create_and_update_mirror_into_the_store`).

The `todo-unprompted` scenario gives a three-file refactor in `fixture/` and a prompt that never mentions todos. `scenario.json`:

```json
{"max_turns": 25, "checks": [
  {"id": "adds-todos", "call": "Bash|TaskCreate|TodoWrite", "match": "devkit todo add|subject|todos"},
  {"id": "finishes-todos", "call": "Bash|TaskUpdate|TodoWrite", "match": "devkit todo done|completed"}
]}
```

Check the `call` and `match` fields against `evals/lib/transcript.jq`, which documents the check kinds, and adjust them to its syntax.

- [ ] **Step 1: Write the reference and the `SKILL.md` pointer, then run the skill and link suites**

Run: `cargo nextest run --test skills --test docs_links --test plugin_layout`
Expected: PASS

- [ ] **Step 2: Write both eval cases and run each once at low reps**

Run: `EVAL_REPS=1 devrun task eval --arg eval_case=todo-guide` and `EVAL_REPS=1 evals/scenario.sh todo-unprompted new=.`
Expected: every question answered correctly, and both scenario checks pass. Both are billed; get the user's go-ahead before running them.

- [ ] **Step 3: Run the full gate**

Run: `devrun task verify`
Expected: fmt, clippy, nextest and doctests all pass.

- [ ] **Step 4: Commit** with subject `docs(todo): add the todo skill reference and evals`.
