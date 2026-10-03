# Todo Lists in Taskwarrior Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `[todo] backend = "taskwarrior"` keeps every devkit todo in the local taskwarrior through the `task` program, with the CLI and hooks unchanged on either backend.

**Architecture:** `TodoStore` becomes an ambassador delegatable trait with a new `get`. The built-in store moves to its own crate, a new `devkit-todo-taskwarrior` crate holds the `schema` mapping and a store that shells out to `task`, and the binary dispatches through one `Store` enum built from config. A contract test suite in `devkit-todo` pins what every backend owes.

**Tech Stack:** Rust 2024, ambassador 0.5, serde/serde_json, chrono, fd-lock (through `devkit_common::store`), taskwarrior 3.

**Spec:** `docs/superpowers/specs/2026-10-03-todo-taskwarrior-design.md`. Background: `docs/superpowers/specs/2026-10-03-todo-tracking-design.md`.

## Global Constraints

- Each backend is its own crate. Only the binary names every backend crate.
- No `Box<dyn TodoStore>` and no `&dyn TodoStore`: the binary dispatches through `#[derive(Delegate)] #[delegate(TodoStore)] enum Store`, and code that uses a store takes `&impl TodoStore`.
- `TodoStore` has no `Send + Sync` bound.
- The native map and context digests stay at `state_dir()/todo/` for every backend.
- Hooks never change a verdict or write stdout because a store failed; a `task` failure in a hook is silent.
- `task` is only ever spawned with `rc.confirmation=off`, the three UDA overrides, stdin null, and full uuids for writes.
- A global todo is `project:global` in taskwarrior; a task with no project is never a devkit todo.
- Text an agent reads shows a uuid id as its first 8 characters; `--json`, the store and the native map keep full ids.
- Help text stays ASCII. Conventional Commits. Run `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings` and `devrun task fmt` before each commit.

## Review Focus

1. **No config anywhere** (`NoConfig`, the normal state in a cloud session): the CLI must use the built-in store, not fail. Pinned in Task 6.
2. **A short id given to `purge`** must forget the native map entry under the full uuid, or later native updates point at a purged todo. Pinned in Task 6.
3. **The person's own taskwarrior data**: an unfiled task, or one in an unrelated project, must never list under `--all`, be released, or be claimed. Pinned in Task 5.
4. **A description that looks like taskwarrior syntax** (`project:x`, `+tag`, `due:tomorrow`, backslashes, a leading `-`) must be stored verbatim. Pinned in Task 5.
5. **A config that exists but fails to parse**: the CLI errors naming the file; a hook falls back to the built-in store silently. Pinned in Task 6.

---

### Task 1: `get`, delegatable trait, shared state paths, contract suite

**Files:**
- Modify: `crates/devkit-todo/Cargo.toml` (add `ambassador.workspace = true`; feature `test-support = []`)
- Modify: `crates/devkit-todo/src/lib.rs` (trait, `state_dir`, `digest_path`, `short_id`)
- Modify: `crates/devkit-todo/src/builtin.rs` (implement `get`; drop its `digest_path` and `default_dir`)
- Create: `crates/devkit-todo/src/contract.rs` (behind `#[cfg(feature = "test-support")]`)
- Modify: `crates/devkit-todo/tests/builtin.rs` (keep builtin-only tests; invoke the contract macro)
- Modify: `src/bin/devkit/todo.rs`, `src/bin/devkit/hook/todo.rs`, `tests/common/todoenv.rs` (call sites of the moved paths)

**Interfaces:**
- Produces:
  - `#[ambassador::delegatable_trait] pub trait TodoStore { fn list(&self, filter: &Filter) -> anyhow::Result<Vec<Todo>>; fn get(&self, id: &str) -> anyhow::Result<Option<Todo>>; fn add(&self, todo: NewTodo) -> anyhow::Result<String>; fn apply(&self, edit: &Edit) -> anyhow::Result<()>; }`
  - `pub fn devkit_todo::state_dir() -> PathBuf` (`paths::state_dir().join("todo")`)
  - `pub fn devkit_todo::digest_path(holder: &Holder) -> PathBuf` (`state_dir().join("digests").join(render::digest(holder))`)
  - `pub fn devkit_todo::short_id(id: &str) -> &str`: the first 8 characters when `id` is a 36-character uuid with 4 dashes, else `id`
  - `devkit_todo::contract::*` case functions, each `pub fn <case>(store: &impl TodoStore)`, and `#[macro_export] macro_rules! contract_tests { ($make:expr) => { ... } }` that expands to one `#[test]` per case. `$make` is a closure returning `(G, S)` where `G` is a guard held for the test (a `TempDir`) and `S: TodoStore`. A second arm, `(skip_unless $make:expr)`, takes a closure returning `Option<(G, S)>` and makes each generated test return early on `None`, for backends whose program may be absent.

- [ ] **Step 1: Write the contract cases in `contract.rs`**

Move every backend-agnostic test from `tests/builtin.rs` into a case, rewritten to use only ids `add` returns. Cases and their assertions:

```rust
pub fn add_then_list_round_trips(s: &impl TodoStore)       // project Some("r.main") and None both read back; Pending; entry and modified are Some
pub fn add_without_order_appends_after_the_last_sibling(s) // orders 1024, 2048; first child 1024
pub fn descriptions_are_one_line(s)                        // "a\n\tb  c" -> "a b c"; Describe "x\r\ny" -> "x y"
pub fn set_status_goes_through_transition(s)               // start by "S"; start by "S/b" fails with root cause Claimed{by:"S"}; start by "S/a" hands down to "S/a"; done by "S" records Completed{by:Some("S/a")}
pub fn undone_drops_the_holder(s)                          // done then Pending -> Status::Pending
pub fn cancel_keeps_the_record(s)                          // cancelled todo still lists as Cancelled{by:Some(actor)}
pub fn release_all_returns_covered_claims_to_pending(s)    // "S" releases "S" and "S/a", not "T"; human releases nothing
pub fn purge_removes_the_record(s)                         // get -> None after Purge
pub fn unknown_ids_are_refused(s)                          // apply Describe on "99999999" errors "no todo 99999999"; get -> Ok(None)
pub fn get_returns_the_todo(s)                             // get(id) == the listed todo
pub fn move_reorder_and_relocate(s)                        // Move under parent w/o order lands after new siblings; Reorder sets order; Relocate to None reads back None
pub fn filters_match_exact_and_subtree(s)                  // todos on "r", "r.main", "r-web": Subtree("r") lists 2, Exact("r") lists 1
```

- [ ] **Step 2: Wire the builtin test file to the suite and run it**

`tests/builtin.rs` keeps `ids_are_sequential_and_sort_numerically`, `a_corrupt_file_aborts_and_stays` and `concurrent_adds_all_land`, and adds `devkit_todo::contract_tests!(|| { let d = tempfile::tempdir().unwrap(); let s = BuiltinStore::at(d.path().to_path_buf()); (d, s) });`. `[dev-dependencies]` enables `devkit-todo = { path = ".", features = ["test-support"] }`.

Run: `cargo nextest run -p devkit-todo`
Expected: FAIL to compile: `get` not found, `contract` missing.

- [ ] **Step 3: Implement the trait change, `get` on `BuiltinStore` (key lookup; a non-numeric id is `Ok(None)`), `state_dir`, `digest_path`, `short_id`, and update the call sites**

`short_id` gets unit tests in `lib.rs`: a uuid shortens to its first 8 characters, `"17"` and a 36-character non-uuid stay whole.

- [ ] **Step 4: Run the suite**

Run: `cargo nextest run -p devkit-todo && cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit** `refactor(todo): add get and a backend contract suite`

---

### Task 2: Move the built-in store to `devkit-todo-builtin`

**Files:**
- Create: `crates/devkit-todo-builtin/Cargo.toml`, `crates/devkit-todo-builtin/src/lib.rs` (the contents of `builtin.rs`)
- Move: `crates/devkit-todo/tests/builtin.rs` to `crates/devkit-todo-builtin/tests/builtin.rs`
- Delete: `crates/devkit-todo/src/builtin.rs`
- Modify: workspace `Cargo.toml` (`members`, `[workspace.dependencies] devkit-todo-builtin = { path = "crates/devkit-todo-builtin" }`, root package dependency), `crates/devkit-todo/Cargo.toml` (description drops "and the built-in store"), `AGENTS.md` crate table (one row each for `devkit-todo`, `devkit-todo-builtin`, `devkit-todo-taskwarrior`; the last is added in Task 4)
- Modify: every `devkit_todo::BuiltinStore` import in `src/` and `tests/`

**Interfaces:**
- Consumes: Task 1's trait, `state_dir`, contract macro.
- Produces: `devkit_todo_builtin::BuiltinStore` with `at(dir: PathBuf) -> Self` and `open() -> Self` (`at(devkit_todo::state_dir())`). Same on-disk files as before.

- [ ] **Step 1: Move the files and fix imports**
- [ ] **Step 2: Run** `cargo nextest run --workspace --no-fail-fast`. Expected: PASS with the same test count as after Task 1.
- [ ] **Step 3: Commit** `refactor(todo): move the built-in store to its own crate`

---

### Task 3: `[todo]` config

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`pub todo: TodoConfig` on `Config`; new types)
- Modify: `schema/devkit-config.json` (regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test`)

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TodoBackend { #[default] Builtin, Taskwarrior }

#[derive(Debug, Default, Clone, JsonSchema, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TodoConfig { pub backend: TodoBackend, pub taskwarrior: TaskwarriorConfig }

#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TaskwarriorConfig { pub path: String } // default "task"
```

- [ ] **Step 1: Write the doctest on `TodoConfig`** in the style of `ForgeConfig`'s: parsing `[todo]\nbackend = "taskwarrior"\n[todo.taskwarrior]\npath = "/opt/task"` gives `TodoBackend::Taskwarrior` and `"/opt/task"`; `Config::parse("")` gives `Builtin` and `"task"`; `backend = "jira"` and an unknown key under `[todo]` are errors.
- [ ] **Step 2: Run** `cargo test -p devkit-config --doc TodoConfig`. Expected: FAIL, type missing.
- [ ] **Step 3: Add the types with doc comments.** `backend`'s comment says where the todos live, that `taskwarrior` needs taskwarrior 3's `task`, and that the key belongs in `~/.config/devkit/config.toml` because a committed value breaks every machine without `task`, cloud sessions included. `path`'s comment: "The program to run. Its own name is looked up on PATH; any other value runs as written."
- [ ] **Step 4: Regenerate the schema and run** `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit-config && cargo test -p devkit-config --doc`. Expected: PASS, `schema/devkit-config.json` changed.
- [ ] **Step 5: Commit** `feat(config): add the [todo] backend table`

---

### Task 4: `devkit-todo-taskwarrior::schema`

Pure conversion, no process spawning, so every test runs everywhere.

**Files:**
- Create: `crates/devkit-todo-taskwarrior/Cargo.toml` (deps: `anyhow`, `serde`, `serde_json`, `chrono`, `devkit-todo`, `devkit-common`; dev: `tempfile`, `devkit-todo` with `test-support`)
- Create: `crates/devkit-todo-taskwarrior/src/lib.rs`, `src/schema.rs`
- Modify: workspace `Cargo.toml`, `AGENTS.md` crate table

**Interfaces:**
- Produces (`pub mod schema`):

```rust
pub const UDAS: [(&str, &str); 6] = [
    ("uda.subof.type", "uuid"), ("uda.subof.label", "Sub of"),
    ("uda.order.type", "numeric"), ("uda.order.label", "Order"),
    ("uda.holder.type", "string"), ("uda.holder.label", "Holder"),
];
pub const GLOBAL_PROJECT: &str = "global";

/// A task as `task export` prints it.
#[derive(Deserialize)] pub struct Exported { pub uuid: String, pub description: String, pub status: String,
    pub start: Option<String>, pub holder: Option<String>, pub subof: Option<String>,
    #[serde(default, deserialize_with = ...)] pub order: Option<i64>, pub project: Option<String>,
    pub entry: Option<String>, pub modified: Option<String> }

impl Exported { pub fn into_todo(self) -> Option<Todo>; }
pub fn status_args(status: &Status, started: bool) -> Vec<String>;
pub fn project_arg(project: Option<&str>) -> String;          // "project:global" or "project:<node>"
pub fn escaped(description: &str) -> String;                   // each '\' doubled
pub fn rfc3339(taskwarrior_date: &str) -> Option<String>;      // "20261003T120000Z" -> "2026-10-03T12:00:00Z"
```

`status_args` returns the words after `<uuid>` for each row of the spec's Writes table: `["modify","status:pending","start:","end:","holder:"]`, `["modify","status:pending","start:now","holder:S"]` (no `start:now` when `started`), `["done","holder:S"]`, `["delete","holder:S"]`, and for `Completed`/`Cancelled` with `by: None` no `holder:` word.

- [ ] **Step 1: Write the failing unit tests in `schema.rs`**

```rust
#[test] fn pending_with_start_and_holder_is_in_progress()       // into_todo -> InProgress{by:"S/a"}
#[test] fn pending_with_start_and_no_holder_is_held_by_human()   // InProgress{by:"human"}
#[test] fn completed_and_deleted_carry_their_holder()            // Completed{by:Some("S")}, Cancelled{by:None}
#[test] fn recurring_and_unknown_statuses_are_not_todos()        // "recurring" -> None
#[test] fn global_project_reads_as_none_and_no_project_is_not_a_todo() // project "global" -> project None; project absent -> None (not a todo)
#[test] fn fractional_order_rounds()                             // 1024.4 -> 1024
#[test] fn dates_convert_to_rfc3339()                            // "20261003T120000Z" -> "2026-10-03T12:00:00Z"; garbage -> None
#[test] fn status_args_cover_every_status()                      // the five vectors above
#[test] fn backslashes_double()                                  // r"a\b" -> r"a\\b"
```

- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo-taskwarrior`. Expected: FAIL to compile.
- [ ] **Step 3: Implement `schema.rs`.** A status string other than `pending`, `completed` or `deleted` is not a todo; `entry` and `modified` go through `rfc3339`.
- [ ] **Step 4: Run** `cargo nextest run -p devkit-todo-taskwarrior`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): add the taskwarrior task mapping`

---

### Task 5: `TaskwarriorStore`

**Files:**
- Create: `crates/devkit-todo-taskwarrior/src/store.rs`, `src/cli.rs`
- Modify: `crates/devkit-common/src/store.rs` (expose the file lock)
- Create: `crates/devkit-todo-taskwarrior/tests/taskwarrior.rs`, `tests/common/mod.rs` (private taskwarrior)

**Interfaces:**
- Consumes: `schema` (Task 4), `contract_tests!` (Task 1), `devkit_todo::{transition, one_line, ORDER_GAP, state_dir}`.
- Produces:
  - `pub fn devkit_common::store::with_file_lock<T>(lock_path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T>`: the exclusive advisory lock `with_lock_via` already takes, factored out so `with_lock_via` calls it.
  - `pub struct TaskwarriorStore` with `pub fn new(program: impl Into<String>) -> Self` (lock at `devkit_todo::state_dir().join("taskwarrior.lock")`), `pub fn with_env(self, env: Vec<(String, String)>) -> Self`, `pub fn with_lock_at(self, path: PathBuf) -> Self`.
  - `impl TodoStore for TaskwarriorStore`.
  - `pub fn devkit_todo_taskwarrior::version(program: &str) -> Option<(u32, u32)>`, used by tests to skip.

- [ ] **Step 1: Write the private-taskwarrior helper and the failing tests**

`tests/common/mod.rs`: `pub fn private() -> Option<(TempDir, TaskwarriorStore)>` returns `None`, after printing `skipping: taskwarrior 3 not found`, when `version("task")` is missing or below 3. Otherwise a temp dir with an empty `taskrc` and `data/`, a store built with `with_env([("TASKRC", ..), ("TASKDATA", ..)])` and `with_lock_at(dir/"taskwarrior.lock")`. A `task(dir, args) -> String` helper runs `task` against the same files for setup and read-back.

`tests/taskwarrior.rs`:

```rust
devkit_todo::contract_tests!(skip_unless common::private);
#[test] fn attribute_syntax_in_a_description_is_verbatim()   // "project:x +tag due:tomorrow -- -lead" and r"a\b" round-trip through add and Describe
#[test] fn a_hand_started_task_is_held_by_human()            // `task add project:r.main -- t` + `task <uuid> start`; get -> InProgress{by:"human"}; agent start -> Claimed
#[test] fn unfiled_and_unrelated_tasks_stay_out()            // `task add -- personal` (no project): Filter::all() lists none; ReleaseAll{"S"} leaves an unrelated active task started
#[test] fn a_short_id_resolves_and_an_ambiguous_one_fails()  // get(first 8 chars) -> the todo; two uuids sharing 8 chars is not constructible, so test resolve_prefix() directly with a fixed list: "abcd" (<8) -> None, a unique 8-prefix -> Some, a shared prefix -> Err naming both uuids
#[test] fn a_missing_program_names_the_config_key()          // TaskwarriorStore::new("/nonexistent/task").list(..) error contains "[todo.taskwarrior] path"
#[test] fn global_todos_use_the_global_project()             // add project None; `task export` shows "project":"global"
```
- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo-taskwarrior`. Expected: FAIL to compile.
- [ ] **Step 3: Implement `cli.rs`**, ported from `alacritree_taskwarrior::Cli::{one_shot, run, add}`: prefix every call with `rc.<key>=<value>` for each of `schema::UDAS` and `rc.confirmation=off`; stdin null; retry on stderr containing `database is locked` or `sqlite_busy` after 50, 150 and 400 ms; `ErrorKind::NotFound` becomes `taskwarrior not found: <path> ([todo.taskwarrior] path)`; any other failure carries `task`'s stderr. `add` passes `rc.verbose=new-uuid` and parses `Created task <uuid>.`
- [ ] **Step 4: Implement `store.rs`**
  - `list`: `(<nodes>) (status:pending or status:completed or status:deleted) export`, nodes per spec Lists (`project.any:` for a subtree of `""`), mapped through `Exported::into_todo`. An empty filter lists nothing without running `task`.
  - `get`: an id shorter than 8 characters is `Ok(None)`; otherwise run `<id> export` (taskwarrior matches a uuid prefix given as a filter word), drop what `into_todo` rejects, and pass the remaining uuids to `fn resolve_prefix(id: &str, uuids: &[String]) -> anyhow::Result<Option<String>>`, whose ambiguity error is `todo id <id> is ambiguous: <uuid>, <uuid>`.
  - `apply`: resolve the id through `get` (`no todo <id>` when `None`), then write by full uuid per the spec's Writes table. `SetStatus`, `add` and `Move` without an order, and `ReleaseAll` run inside `with_file_lock`. `Purge` runs `delete` first unless the todo is `Cancelled`, then `purge`. `ReleaseAll` exports `+ACTIVE`, keeps todos whose holder `holder` covers, and writes one `modify start: holder:` naming every kept uuid; a human holder releases nothing.
  - `add`: `one_line` the description; order from the siblings' max plus `ORDER_GAP` when absent; `parent` must exist (`no todo <parent>`).
- [ ] **Step 5: Run** `cargo nextest run -p devkit-todo-taskwarrior`. Expected: PASS where taskwarrior 3 is installed; every `task` test prints the skip line elsewhere.
- [ ] **Step 6: Probe the spec's open question.** In the private taskwarrior with `journal.time=on`, run `task <uuid> modify start:now` and `task <uuid> start`; record in the crate docs whether only the latter annotates. No code change unless `modify` misbehaves.
- [ ] **Step 7: Commit** `feat(todo): keep todos in taskwarrior`

---

### Task 6: Dispatch in the binary

**Files:**
- Create: `src/bin/devkit/todo/store.rs` (convert `src/bin/devkit/todo.rs` into `src/bin/devkit/todo/mod.rs`)
- Modify: `src/bin/devkit/todo/mod.rs`, `src/bin/devkit/hook/todo.rs`, `crates/devkit-todo/src/render.rs`, root `Cargo.toml` (deps `ambassador`, `devkit-todo-builtin`, `devkit-todo-taskwarrior`)
- Modify: `tests/common/todoenv.rs` (`Proj::with_home_config(toml: &str) -> Self`)
- Create: `tests/todo_taskwarrior.rs`

**Interfaces:**
- Consumes: Tasks 1 to 5.
- Produces:

```rust
#[derive(Delegate)]
#[delegate(TodoStore)]
pub(crate) enum Store { Builtin(BuiltinStore), Taskwarrior(TaskwarriorStore) }

impl Store {
    pub(crate) fn from_config(config: &TodoConfig) -> Self;
    /// CLI: no config anywhere is the built-in store; a config that fails to load is an error.
    pub(crate) fn for_cli(cwd: &Path) -> anyhow::Result<Self>;
    /// Hooks: any config failure is the built-in store.
    pub(crate) fn for_hook(checkout: &Checkout, cwd: &Path) -> Self;
}
```

`for_cli` uses `devkit_common::config::resolve(None, cwd)` and tells `NoConfig` apart with `downcast_ref::<devkit_config::NoConfig>()`. `for_hook` uses `resolve_in(checkout, None, cwd)`.

- [ ] **Step 1: Write the failing tests**

`tests/todo_taskwarrior.rs`, each test skipping as in Task 5 when taskwarrior 3 is missing. The home config sets `backend = "taskwarrior"`, and `TASKRC`/`TASKDATA` point into the test's temp dir:

```rust
#[test] fn the_cli_keeps_todos_in_taskwarrior()   // `todo add one` prints 8 hex chars; `todo start <short>` then `todo done <short>`; `task export` shows status "completed", holder "agent" (or the session holder), project "proj.main.claude-s1"
#[test] fn lists_render_short_ids()                // `todo list` contains "- [ ] one (<8 chars>)"; `todo list --json` carries the full uuid
#[test] fn purge_by_short_id_forgets_the_native_mapping() // map a native id to the todo via a TaskCreate post-tool-use payload, purge by short id as human, the native map no longer holds the full uuid
```

In `tests/todo_cli.rs`:

```rust
#[test] fn no_config_anywhere_uses_the_builtin_store() // isolated home, no devkit.toml: `todo add one` prints "1"
#[test] fn a_broken_config_is_an_error_in_the_cli()    // home config "[todo]\nbackend = 3": `todo list` fails, stderr names the file
```

In `tests/todo_hooks.rs`:

```rust
#[test] fn a_broken_config_leaves_hooks_on_the_builtin_store() // same broken config; `hook post-tool-use` with a TaskCreate payload exits 0 with empty stdout and the todo lands in the built-in store
```

- [ ] **Step 2: Run** `cargo nextest run -E 'binary(todo_taskwarrior) | binary(todo_cli) | binary(todo_hooks)'`. Expected: FAIL.
- [ ] **Step 3: Implement `Store` and switch every call site**: `todo::run`, `context`, `hook::todo::{release, check_claims, capture}`. Every helper that took `&BuiltinStore` takes `&impl TodoStore`; `Capture` becomes `Capture<'a, S: TodoStore> { store: &'a S, .. }`. `check_claims` calls `store.get(&id)` per edited id instead of listing every todo. `purge` resolves the id with `get` and forgets the full id. `add` prints `short_id(&id)`.
- [ ] **Step 4: Render short ids.** `render.rs` prints `short_id(&todo.id)` inside the parentheses. Update the render unit tests that pin a uuid id.
- [ ] **Step 5: Run the full gate** (`cargo nextest run --workspace --no-fail-fast`, doctests, clippy). Expected: PASS.
- [ ] **Step 6: Commit** `feat(todo): choose the todo backend from config`

---

### Task 7: CI and documentation

**Files:**
- Modify: `.github/workflows/ci.yml` (test job)
- Modify: `plugin/skills/using-devkit/references/todo.md`

- [ ] **Step 1: Install taskwarrior on macOS in CI.** In the `test` job add a step before `Test workspace`: `if: matrix.os == 'macos-latest'`, `run: brew install task && task --version`. Ubuntu's `ubuntu-latest` packages ship taskwarrior 2.6, below the floor, so the Linux job skips the `task` tests; confirm with `apt-cache policy taskwarrior` output in the PR description rather than adding a build from source.
- [ ] **Step 2: Add a "Backends" section to `references/todo.md`**: the built-in store is the default; `backend = "taskwarrior"` in `~/.config/devkit/config.toml` keeps todos in taskwarrior 3, on the same project names alacritree uses; ids show as the first 8 characters of the uuid and any unique prefix of 8 or more works; a task started with `task start` outside devkit counts as held by a person; the two taskrc lines from the spec to see `holder` in one's own reports. No counts, no version numbers beyond the floor.
- [ ] **Step 3: Verify** `devkit schema | jq '.properties.todo'` shows both keys with their descriptions.
- [ ] **Step 4: Commit** `ci: run the taskwarrior tests on macos` and `docs(todo): document the taskwarrior backend` as two commits.

---

## Unresolved

- Whether Ubuntu's runner can get taskwarrior 3 without building from source. The plan settles for macOS coverage; say so if Linux CI coverage matters enough to build it.
