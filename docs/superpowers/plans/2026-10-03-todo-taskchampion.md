# Todo Lists in Taskchampion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `backend = "taskchampion"` keeps todos in an embedded taskchampion replica that stays local or syncs to a directory or a sync server, with credentials from the environment, Doppler or the secrets file.

**Architecture:** A new `devkit-todo-taskchampion` crate implements `TodoStore` over a taskchampion 2.0 replica, using #212's `schema` for the task shape. Syncs run only in `devkit todo sync` processes: writers spawn one detached and return, and three callers wait a bounded time for fresh lists. The binary's `Store` enum gains the variant, and an environment variable can select it without a config file.

**Tech Stack:** Rust 2024, taskchampion `~2.0.2` (`server-sync`, `bundled`), ureq 2 with `proxy-from-env`, ambassador, fd-lock through `devkit_common::store`.

**Spec:** `docs/superpowers/specs/2026-10-03-todo-taskchampion-design.md`. Builds on `docs/superpowers/specs/2026-10-03-todo-taskwarrior-design.md` and its plan, `docs/superpowers/plans/2026-10-03-todo-taskwarrior.md`.

**Precondition:** #212 is merged and this branch is rebased onto `main`. Every task below consumes #212's `Store` enum, `TodoConfig`, `devkit_todo_taskwarrior::schema`, `store::with_file_lock`, `short_id`, prefix resolution and the `contract_tests!` macro. Before Task 1, check that each exists under the name #212's plan gives it. Where #212 named something differently, use #212's name throughout this plan.

## Global Constraints

- No `Box<dyn TodoStore>`. `Store` is the ambassador enum; `TodoStore` itself does not change.
- taskchampion is `~2.0.2` with `default-features = false, features = ["server-sync", "bundled"]`.
- devkit never interrupts a sync it started. A sync runs in its own `devkit todo sync` process, which no devkit code kills.
- No hook waits on the network. A hook waits at most 2 seconds for the replica lock and then fails open: it injects, attributes, captures or releases nothing, and prints nothing.
- No credential value appears in an error, a log line, `devkit doctor` output or a Doppler failure message.
- The variable names are exactly `DEVKIT_TODO_BACKEND`, `DEVKIT_TODO_SYNC_URL`, `DEVKIT_TODO_SYNC_CLIENT_ID` and `DEVKIT_TODO_SYNC_SECRET`. Their secrets-file keys are the lowercase spellings.
- The wait bounds are 20 seconds for each waiting caller. A failed sync blocks background attempts for 60 seconds. The `session-end` hook timeout is 25 seconds.
- Help text stays ASCII. Conventional Commits. Run `cargo nextest run --workspace --no-fail-fast`, `cargo test --workspace --doc`, `cargo clippy --workspace --all-targets -- -D warnings` and `devrun task fmt` before each commit.

## Review Focus

1. **A write made while a sync is uploading:** it must reach the server through that sync's rerun, never wait for the next unrelated write. Pinned in Task 4.
2. **The first SessionStart in a new container against an unreachable server:** the injection must arrive within the 20-second bound, from the empty local replica, with a background sync left running. Pinned in Task 5.
3. **`DEVKIT_TODO_BACKEND=taskchampion` with taskwarrior configured in the home config:** the environment wins, and `devkit doctor` says so. Pinned in Task 3.
4. **A `data_dir` that cannot be created**, such as a read-only path: the CLI error names the path, and hooks stay silent. Pinned in Task 2.
5. **Credentials with surrounding whitespace or a trailing newline** (common in Doppler and container env files): the client id still parses as a UUID, and the URL and secret are used trimmed. Pinned in Task 1.

---

### Task 1: Credentials with an optional Doppler step

**Files:**
- Modify: `crates/devkit-common/src/secrets.rs`
- Modify: `src/bin/devkit/doctor.rs` (exhaustive `Source` match)

**Interfaces:**
- Produces:

```rust
pub enum Source { Env, Doppler, File, Unset }

pub struct DopplerScope { pub project: String, pub config: Option<String> }

/// Each name resolved env -> Doppler (only with `doppler`, one call for every
/// name the environment lacks) -> secrets file. Values are trimmed.
pub fn resolve_many(names: &[&str], doppler: Option<&DopplerScope>) -> Vec<(Option<String>, Source)>;
```

`Secrets` gains `devkit_todo_sync_url`, `devkit_todo_sync_client_id` and `devkit_todo_sync_secret` in the struct, `get` and `set`.

- [ ] **Step 1: Write the failing tests** in `secrets.rs`. `resolve_many` delegates to a private `resolve_many_with(names, doppler, program: &OsStr)`, and the tests call that with the path of a fake `doppler` script in a temp dir.

```rust
#[test] fn env_beats_doppler_beats_file()              // A in env, B in fake doppler JSON, C in file: sources Env, Doppler, File
#[test] fn doppler_runs_once_for_every_missing_name()  // fake doppler appends to a call log; two missing names -> one call
#[test] fn no_scope_never_runs_doppler()               // doppler None with a fake that fails loudly: no call logged
#[test] fn a_failing_doppler_falls_through_silently()  // fake exits 1 printing "secret-value-xyz" on stderr: the result is File/Unset and nothing returned contains the stderr
#[test] fn values_are_trimmed()                        // env "  abc\n" -> "abc"
#[test] fn the_new_keys_round_trip_through_the_file()  // store_at(path, "devkit_todo_sync_secret", ..) then load
```

Doppler's call is `doppler secrets get <NAMES...> --json --no-exit-on-missing-secret --attempts 1 --timeout 5s --project <p> [--config <c>]`. Without `--no-exit-on-missing-secret`, one missing name fails the whole call. The defaults of 5 attempts and a 10-second timeout per attempt are too slow for a store open. stdout is `{"<NAME>": {"computed": "<value>" | null, "note": ..., ...}, ...}` (Doppler CLI 3.76.6, `pkg/printer/enclave.go:282-302`). A `null` computed value, or an absent name, counts as unresolved. The fake prints that shape.

- [ ] **Step 2: Run** `cargo nextest run -p devkit-common secrets`. Expected: FAIL to compile.
- [ ] **Step 3: Implement**, with `Source::Doppler` handled in `doctor.rs`'s match, printed as `doppler`.
- [ ] **Step 4: Run** `cargo nextest run -p devkit-common secrets && cargo nextest run --workspace --no-fail-fast`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(secrets): resolve credentials through doppler`

---

### Task 2: `devkit-todo-taskchampion`, local replica

**Files:**
- Create: `crates/devkit-todo-taskchampion/{Cargo.toml, src/lib.rs, src/store.rs}`, `crates/devkit-todo-taskchampion/tests/taskchampion.rs`
- Modify: workspace `Cargo.toml` (`members`, `[workspace.dependencies]` for the new crate and `taskchampion = { version = "~2.0.2", default-features = false, features = ["server-sync", "bundled"] }`), `AGENTS.md` crate table
- Modify: `crates/devkit-common/src/store.rs` (`with_file_lock_for`)

**Interfaces:**
- Consumes: `devkit_todo::{TodoStore, Todo, NewTodo, Edit, Filter, transition, one_line, ORDER_GAP, short_id}`, `devkit_todo_taskwarrior::schema::{GLOBAL_PROJECT, ...}`, #212's prefix resolution (move `resolve_prefix` into `devkit_todo` if #212 left it private to the taskwarrior crate), `store::with_file_lock`.
- Produces:

```rust
pub fn devkit_common::store::with_file_lock_for<T>(lock_path: &Path, wait: Duration, f: impl FnOnce() -> Result<T>) -> Result<T>;
// polls try-lock every 50 ms; past `wait` it errors "todo store busy"

pub struct TaskchampionStore { /* data_dir, lock_wait: Option<Duration>, target: Option<SyncTarget> */ }
impl TaskchampionStore {
    pub fn at(data_dir: PathBuf) -> Self;                  // no target, waits for the lock
    pub fn with_lock_wait(self, wait: Duration) -> Self;  // hooks pass 2 s
    pub fn with_target(self, target: SyncTarget) -> Self;
    pub fn target(&self) -> Option<&SyncTarget>;
    pub fn data_dir(&self) -> &Path;
}
pub enum SyncTarget { Dir(PathBuf), Server { url: String, client_id: uuid::Uuid, secret: Vec<u8> } }
impl TodoStore for TaskchampionStore { ... }
```

`SyncTarget`'s `Debug` prints `Server { url, client_id: .., secret: <redacted> }`.

- [ ] **Step 1: Write the failing tests**

```rust
devkit_todo::contract_tests!(|| { let d = tempfile::tempdir().unwrap(); let s = TaskchampionStore::at(d.path().join("tc")); (d, s) });
#[test] fn global_todos_use_the_global_project()      // add project None; raw replica task has project "global"
#[test] fn a_task_with_no_project_never_lists()       // insert a task straight through taskchampion without a project: Filter::all() is empty
#[test] fn entry_and_modified_are_rfc3339()           // both parse with chrono::DateTime::parse_from_rfc3339
#[test] fn a_held_lock_fails_after_the_wait()         // hold <data_dir>/devkit.lock in another thread; with_lock_wait(200ms).list() errors "todo store busy" in < 1 s
#[test] fn an_uncreatable_data_dir_names_the_path()   // data_dir under a read-only temp dir: the error contains the path
#[test] fn secret_is_redacted_in_debug()              // format!("{:?}", SyncTarget::Server{..}) has no secret bytes
```

- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo-taskchampion`. Expected: FAIL to compile.
- [ ] **Step 3: Implement `with_file_lock_for`**, then the store per the spec's Store section. `add` uses `Replica::new_task(Status::Pending, one_line(description))` and then `set_value` for `project`, `subof` and `order`. Status writes follow `schema`'s mapping, via `set_status`, `start`, `stop` and `set_value("holder", ..)`. `Purge` uses `get_task_data` and `TaskData::delete`, then `commit_operations`. `ReleaseAll` walks `all_tasks()` for pending tasks with a `start`. Every method opens the replica inside the lock.
- [ ] **Step 4: Run** `cargo nextest run -p devkit-todo-taskchampion`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): keep todos in a taskchampion replica`

---

### Task 3: Selecting the backend

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`TodoBackend::Taskchampion`, `TaskchampionConfig`, `TodoConfig.taskchampion`, doctest), `schema/devkit-config.json`
- Modify: the binary's `Store` module from #212 (`src/bin/devkit/todo/store.rs`), `src/bin/devkit/doctor.rs`
- Modify: workspace `Cargo.toml` (`ureq` features gain `"proxy-from-env"`)
- Test: `tests/todo_taskchampion.rs` (new), `tests/common/todoenv.rs`

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces:

```rust
pub struct TaskchampionConfig { pub data_dir: Option<String>, pub server_dir: Option<String>, pub doppler_project: Option<String>, pub doppler_config: Option<String> }

pub(crate) enum BackendSource { Env, Config, Default }
/// `DEVKIT_TODO_BACKEND` over `[todo] backend`. An unknown value is an error naming the variable and the accepted spellings.
pub(crate) fn effective_backend(config: Option<&TodoConfig>, env: Option<&str>) -> Result<(TodoBackend, BackendSource)>;
/// The spec's Sync target order; `server_dir` wins and skips credentials.
pub(crate) fn sync_target(config: &TaskchampionConfig) -> Result<Option<SyncTarget>>;
```

`Store::{for_cli, for_hook}` from #212 route through `effective_backend`. The taskchampion arm builds the store with `data_dir` (default `devkit_todo::state_dir().join("taskchampion")`) and `sync_target`. `for_hook` adds `with_lock_wait(2 s)`, turns a `sync_target` error into no target, and turns an unknown `DEVKIT_TODO_BACKEND` into the built-in store.

- [ ] **Step 1: Write the failing tests**
  - Config doctest: the `[todo.taskchampion]` table parses, and an unknown key is an error.
  - Unit tests on `effective_backend`: env over config; config over default; an unknown env value's error names `DEVKIT_TODO_BACKEND`.
  - Unit tests on `sync_target`: `server_dir` wins over credentials that resolve; the three server credentials give `Server`; two of three gives an error naming the missing one; a client id of `"nope"` gives an error naming `DEVKIT_TODO_SYNC_CLIENT_ID`; `" <uuid>\n"` parses.
  - `tests/todo_taskchampion.rs`:

```rust
#[test] fn the_env_selects_taskchampion_with_no_config()   // no devkit.toml, DEVKIT_TODO_BACKEND=taskchampion: `todo add one` prints 8 hex chars; the replica dir exists under XDG_STATE_HOME/devkit/todo/taskchampion
#[test] fn the_env_beats_the_home_config()                 // home config backend = "builtin"; env taskchampion: the todo lands in the replica
#[test] fn doctor_reports_the_backend_and_its_source()      // `devkit doctor` output has a "todo backend" row with "taskchampion" and "environment"; credential rows show sources and no values
#[test] fn a_misspelled_backend_fails_the_cli_and_not_hooks() // DEVKIT_TODO_BACKEND=taskchampio: `todo list` fails naming the variable; post-tool-use TaskCreate exits 0, prints nothing, lands in the built-in store
```

- [ ] **Step 2: Run** `cargo nextest run -E 'binary(todo_taskchampion)' && cargo test -p devkit-config --doc`. Expected: FAIL.
- [ ] **Step 3: Implement**, regenerate the schema (`DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit-config`), and add `proxy-from-env` to the workspace `ureq` features.
- [ ] **Step 4: Run the full gate.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): select the taskchampion backend`

---

### Task 4: `devkit todo sync` and background sync after writes

**Files:**
- Create: `src/bin/devkit/todo/sync.rs`
- Modify: `crates/devkit-todo-taskchampion/src/lib.rs` (`sync_once`), `src/bin/devkit/todo/mod.rs` (the `Sync` verb, spawn after writes), `src/bin/devkit/hook/todo.rs` (spawn after captures and releases), `src/bin/devkit/todo/store.rs`
- Test: `crates/devkit-todo-taskchampion/tests/sync.rs`, `tests/todo_taskchampion.rs`

**Interfaces:**
- Produces:

```rust
impl TaskchampionStore {
    /// One full `Replica::sync(server, false)` under the replica lock. Never called with a deadline.
    pub fn sync_once(&self) -> anyhow::Result<()>;
}
// CLI: `devkit todo sync [--background]`; `--background` is hidden from help.
impl Store {
    pub(crate) fn spawn_sync(&self);                                 // no-op without a taskchampion target
    pub(crate) fn sync(&self, wait: Option<Duration>) -> SyncOutcome; // waits for a `devkit todo sync` child up to `wait`
}
pub(crate) enum SyncOutcome { Done, Failed(String), StillRunning, NoTarget }
```

`devkit todo sync` runs this algorithm. `<d>` is `data_dir`:

```text
(writers create <d>/sync.pending before spawning; this command only clears it)
background and (<d>/sync.failed younger than 60 s): exit 0
take <d>/sync.lock (background: try once, exit 0 if held; foreground: wait)
loop:
    remove <d>/sync.pending
    result = store.sync_once()
    on error: write <d>/sync.failed (mtime is the timestamp); report; break
    on success: remove <d>/sync.failed
    if <d>/sync.pending does not exist: break
release <d>/sync.lock
if <d>/sync.pending exists and the last attempt succeeded: start over from "take"
```

Each successful write in the CLI and in hooks creates `<d>/sync.pending`, then `spawn_sync`. That spawns `std::env::current_exe()` with `todo sync --background`, null stdio, and on Unix `process_group(0)`. On Windows it uses `creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)`.

- [ ] **Step 1: Write the failing tests**

```rust
// crates/devkit-todo-taskchampion/tests/sync.rs
#[test] fn two_replicas_meet_through_a_directory()   // A.add; A.sync_once; B.sync_once; B lists it
#[test] fn claims_cross_replicas()                   // A starts as "S"; sync both; B reads InProgress{by:"S"}
#[test] fn an_interrupted_sync_rolls_back()          // build two versions on the server dir; in a child process, open B and kill it mid-sync via a server_dir made unreadable after the first version is read (or a test-only hook in sync_once that panics after the first applied version); B lists what it listed before; B.sync_once then succeeds
// tests/todo_taskchampion.rs
#[test] fn add_returns_before_an_unanswering_server() // a TcpListener that accepts and never replies, set as DEVKIT_TODO_SYNC_URL: `todo add` exits in < 1 s; a `todo sync --background` process is alive afterwards
#[test] fn a_failed_sync_holds_off_background_attempts() // a listener that accepts and closes at once, counting accepts: `todo add`, poll until <d>/sync.failed exists; a second `todo add` and its child's exit leave the accept count unchanged
#[test] fn writes_during_a_sync_ride_its_rerun()      // directory target; hold <d>/sync.lock from the test, `todo add` twice (each sets sync.pending; their background children exit); release and run `todo sync`; replica B sees both after its own sync
#[test] fn todo_sync_reports_failure_and_exits_zero()  // unreachable server: stderr has "devkit todo: sync failed:" and no credential value
```

Poll for spawned processes and files rather than sleeping (`AGENTS.md`).

- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo-taskchampion && cargo nextest run -E 'binary(todo_taskchampion)'`. Expected: FAIL.
- [ ] **Step 3: Implement `sync_once`, the verb, `spawn_sync`, and the write-path calls.** The write path covers `add`, `apply` from the CLI, native capture and `release`. For `an_interrupted_sync_rolls_back`, first try to stall the sync from outside: a `server_dir` whose second version cannot be read. If taskchampion offers no such seam, add a `test-support` feature to the crate under which `sync_once` panics after its first applied version when `DEVKIT_TODO_SYNC_FAILPOINT` is set. Release builds never read the variable.
- [ ] **Step 4: Run** the same commands. Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): sync taskchampion replicas in the background`

---

### Task 5: Waiting for fresh lists

**Files:**
- Modify: `src/bin/devkit/todo/mod.rs` (`context` at `SessionStart`, `list --sync`), `src/bin/devkit/hook/todo.rs` (`release` for `session-end`), `plugin/hooks/hooks.json`, `plugin/hooks/hooks-codex.json`
- Test: `tests/todo_taskchampion.rs`, `tests/todo_context.rs`

**Interfaces:**
- Consumes: `Store::sync(Some(Duration::from_secs(20)))` from Task 4.
- Produces: `ListArgs.sync: bool` (`--sync`, help: "Sync the todo store before listing; waits up to 20 seconds").

Waiting means spawning the same `devkit todo sync` child and polling its exit for the bound. A child still running at the bound keeps running; the caller reads the local replica.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn session_start_pulls_before_injecting()       // replica A has a todo on proj.main, synced to a dir; a fresh state dir with the same server_dir: SessionStart context lists it
#[test] fn session_start_is_bounded_when_the_server_hangs() // unanswering listener: SessionStart context returns within 22 s with the guide, and a sync child is still alive
#[test] fn user_prompt_context_never_syncs()            // directory target on a fresh state dir: after a UserPromptSubmit context run, <d>/sync.lock does not exist (only a sync creates it)
#[test] fn list_sync_pulls_and_plain_list_does_not()    // B's `todo list` misses A's todo; `todo list --sync` shows it
#[test] fn session_end_pushes_the_release()             // a claimed todo, session-end hook, then replica B (after its own sync) reads it Pending
#[test] fn the_session_end_timeout_allows_the_push()    // jq over both hook manifests: every session-end command has timeout 25
```

- [ ] **Step 2: Run** `cargo nextest run -E 'binary(todo_taskchampion) | binary(todo_context)'`. Expected: FAIL.
- [ ] **Step 3: Implement**, and set `"timeout": 25` on the `session-end` hook in both manifests.
- [ ] **Step 4: Run the full gate.** Expected: PASS.
- [ ] **Step 5: Commit** `feat(todo): pull lists at session start and push at session end`

---

### Task 6: Wire compatibility with taskwarrior, and documentation

**Files:**
- Create: `crates/devkit-todo-taskchampion/tests/taskwarrior_compat.rs`
- Modify: `plugin/skills/using-devkit/references/todo.md`

- [ ] **Step 1: Write the test**, skipping exactly as #212's taskwarrior tests do when `task` is missing:

```rust
#[test] fn taskwarrior_reads_a_synced_replica() // store: add "ship" on proj.main, add child "spec" under it, start child as "S/a", sync_once to <dir>/server;
                                                 // private taskwarrior with rc sync.local.server_dir=<dir>/server: `task sync` then `task export`;
                                                 // the child has project "proj.main", status "pending", a start, holder "S/a", subof = parent uuid, order 1024
#[test] fn a_devkit_replica_reads_taskwarrior_edits() // after that, `task <child uuid> done`, `task sync`; store.sync_once; get(child) is Completed
```

- [ ] **Step 2: Run** `cargo nextest run -p devkit-todo-taskchampion taskwarrior_compat`. Expected on a machine with taskwarrior 3.4: PASS. If it fails, stop and report the field that differs. Do not adjust the assertions to match.
- [ ] **Step 3: Write the reference section** the spec's Documentation lists. It includes the secrets-file lines to add by hand:

```toml
devkit_todo_sync_url = "https://..."
devkit_todo_sync_client_id = "<uuid>"
devkit_todo_sync_secret = "<secret>"
```

  It also includes a taskwarrior profile for reading the lists:

```text
TASKRC=~/.taskrc-agents TASKDATA=~/.task-agents task sync
# ~/.taskrc-agents
sync.server.url=<url>
sync.server.client_id=<uuid>
sync.encryption_secret=<secret>
```

  Add the cross-replica claim caveat, and the cost of sharing credentials with one's everyday taskwarrior. Give no counts and no version numbers beyond what the spec fixes.
- [ ] **Step 4: Commit** `test(todo): pin taskchampion and taskwarrior compatibility` and `docs(todo): document the taskchampion backend` as two commits.

---

## Unresolved

- How to stall a taskchampion sync partway for `an_interrupted_sync_rolls_back`. Task 4 tries an external stall first and falls back to a test-only failpoint.
