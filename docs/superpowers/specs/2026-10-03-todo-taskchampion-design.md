# Todo lists in taskchampion, local or synced

Issue: [#213](https://github.com/AbysmalBiscuit/devkit/issues/213). Builds on [#211](https://github.com/AbysmalBiscuit/devkit/issues/211) (`docs/superpowers/specs/2026-10-03-todo-tracking-design.md`) and [#212](https://github.com/AbysmalBiscuit/devkit/issues/212) (`docs/superpowers/specs/2026-10-03-todo-taskwarrior-design.md`), whose `[todo]` table, `Store` enum, `schema` module, file lock helper and contract suite this spec extends. #212 merges first; this branch rebases onto it.

## Problem

A cloud session's todos live in a container that is deleted when the session ends. The built-in store dies with it, and the taskwarrior backend needs a `task` program the container does not have. Nobody outside the session can watch its work advance, and a later session cannot see where the last one stopped.

## Goal

`backend = "taskchampion"` keeps todos in a taskchampion replica that devkit embeds, so it needs no program beyond devkit. The replica can stay local or sync to a directory or a server, and a person reads the synced lists with taskwarrior.

- It works in a container that has only devkit and a few environment variables.
- Sync credentials come from environment variables, from Doppler when configured, or from devkit's secrets file.
- A sync failure never loses a write, and no hook waits on the network.
- The CLI, context injection, claim attribution, release and native capture behave as on the other backends.

## Out of scope

- Sync to GCP or AWS buckets, and taskchampion's git sync. Their dependencies outweigh their use here.
- Pointing `data_dir` at taskwarrior's own database. That couples devkit to the on-disk format of whatever taskwarrior is installed; the synced lists are read through a sync instead (see Reading the lists).
- Running or deploying a sync server.
- Moving todos between backends.
- Two machines claiming the same todo between syncs (see Claims across replicas).
- A command that writes the sync credentials into the secrets file. The file is edited by hand; the reference shows the lines.

## Design

### Crate

`devkit-todo-taskchampion` holds `TaskchampionStore`. It depends on `devkit-todo`, on `devkit-todo-taskwarrior` for `schema` (attribute names, status mapping, the global project), and on:

```toml
taskchampion = { version = "~2.0.2", default-features = false, features = ["server-sync", "bundled"] }
```

- Taskwarrior 3.4.2, the version on the person's machine, pins taskchampion `=2.0.2` (`src/taskchampion-cpp/Cargo.toml`). `~2.0.2` stays on that line. A bump is a deliberate change that reruns the wire-compatibility test (see Testing).
- `server-sync` brings the remote server client and its encryption. `bundled` compiles SQLite in. Directory sync (`ServerConfig::Local`) has no feature gate.
- The workspace `ureq` dependency gains the `proxy-from-env` feature. taskchampion 2.0.2's client is a ureq agent built without proxy settings, and Cargo feature unification turns proxy-from-env on inside it, so `HTTPS_PROXY` and `NO_PROXY` are honoured, as some cloud sandboxes require for egress. devkit's own HTTP calls gain the same behaviour.

The binary's `Store` enum gains `Taskchampion(TaskchampionStore)`, and `TodoBackend` gains `Taskchampion`. `TodoStore` does not change.

### Choosing the backend

`DEVKIT_TODO_BACKEND`, when set, overrides `[todo] backend`, with the same spellings. A container sets it next to the sync credentials, and the person's own machine keeps what their home config says. `$DEVKIT_CONFIG` cannot do this job: it replaces every config layer, the project's `devkit.toml` included, and the container still needs the project's config.

An unknown value is an error in the CLI and the built-in store in a hook, the same rule as a config that fails to load. `devkit doctor` gains a `todo backend` row giving the effective backend and where it came from (environment, config, default), so a misspelled value in a deploy shows up.

### Config

```toml
[todo]
backend = "taskchampion"

[todo.taskchampion]
data_dir = "/path"         # the replica; default state_dir()/todo/taskchampion
server_dir = "/path"       # sync to a directory instead of a server
doppler_project = "devkit" # read the server credentials from Doppler
doppler_config = "dev"     # optional; Doppler's own default when absent
```

Every key is optional, and each doc comment becomes its schema description. `TodoConfig`'s doctest gains the table.

### Sync target

Decided in this order:

1. **Directory.** `server_dir` is set: `ServerConfig::Local`. Credentials are not read.
2. **Server.** `DEVKIT_TODO_SYNC_URL`, `DEVKIT_TODO_SYNC_CLIENT_ID` and `DEVKIT_TODO_SYNC_SECRET` all resolve: `ServerConfig::Remote`.
3. **None.** The replica stays local.

Only the `devkit todo sync` process resolves the target. Some but not all server credentials resolving, or a client id that is not a UUID, fails it with an error naming the variables, never their values. Every other caller decides whether to start a sync from what is named, without resolving a credential: `server_dir`, `doppler_project`, or any of the three variables in the environment or the secrets file. So no hook or writer waits on Doppler.

### Credentials

Each of the three names resolves through, in order:

1. **The environment.** A container sets them at deploy time; launching the harness under `doppler run` also puts them here.
2. **Doppler**, only when `doppler_project` is set. One `doppler secrets get DEVKIT_TODO_SYNC_URL DEVKIT_TODO_SYNC_CLIENT_ID DEVKIT_TODO_SYNC_SECRET --json --no-exit-on-missing-secret --attempts 1 --timeout 5s --project <p> [--config <c>]` call resolves all three; a name it lacks, or one whose `computed` value is null, counts as unresolved. Any failure, including a missing `doppler`, falls through, and its message is dropped, so a value can never reach a log.
3. **The secrets file**, `~/.config/devkit/secrets.toml`, under `devkit_todo_sync_url`, `devkit_todo_sync_client_id` and `devkit_todo_sync_secret`, the lowercase spelling `secrets::resolve` already maps environment names to.

Changes to `devkit_common::secrets`:
- `Secrets` gains the three fields, in `get` and `set`.
- `Source` gains `Doppler`, which `devkit doctor`'s exhaustive match handles.
- A new `resolve_many(names: &[&str], doppler: Option<&DopplerScope>) -> Vec<(Option<String>, Source)>` makes at most one Doppler call. Existing callers keep `resolve`, so no other token gains a subprocess.

`devkit doctor` gains one row per credential when the backend is taskchampion and no `server_dir` is set, giving the source and never the value.

### Store

- **Opening.** A write or a sync opens the replica with `StorageConfig::OnDisk { taskdb_dir: data_dir, create_if_missing: true, access_mode: ReadWrite }`. A read opens it with `create_if_missing: false, access_mode: ReadOnly`, and a replica not created yet reads as empty.
- **Locking.** Every write and every sync holds the file lock at `<data_dir>/devkit.lock`. Reads take no lock: SQLite's write-ahead log, which taskchampion turns on, gives a read-only connection the last committed state while another process writes or syncs, so a read never waits behind a sync. taskchampion opens an immediate write transaction for every operation and holds one across a whole sync, and rusqlite's 5-second busy timeout is shorter than a sync, so devkit serializes its own processes instead of letting one fail. The sync's transaction is why devkit cannot simply let writers in during a sync: a second connection's write would wait on SQLite and fail at its busy timeout. A CLI write waits for the lock, bounded past one sync attempt against a silent server, then fails naming the lock. Hook writes, captures and releases, go through a queue beside the replica in arrival order, so none overtakes another. A hook appends its write, then applies the queue, waiting at most a second for the lock; a write still blocked stays queued, and the next CLI write or sync applies it, the sync pushing it on a rerun. `store::with_file_lock_for(lock_path, wait, f)` gives the bounded wait.
- **Adding.** `add` uses `Replica::new_task(Status::Pending, description)`, which stamps `entry` and `modified`, then `set_value` for `project`, `subof` and `order`.
- **Mapping.** Taken from `schema`: status, `start`, `holder`, `subof`, `order`, and `project:global` for the global list. A task with no project is never a devkit todo. Writes use `Task::set_status`, which sets and clears `end` as taskwarrior does, plus `start`, `stop` and `set_value`. `entry` and `modified` are read with `get_entry` and `get_modified` and printed as RFC 3339.
- **Claims.** `SetStatus` reads the task, runs `transition` and commits the result under the lock, so the claim rule is exact for every process on the machine.
- **Ids.** Full uuids, shown and resolved by prefix exactly as #212 specifies, through the same `short_id` and the same ambiguity error.
- **Purge.** `TaskData::delete` through `commit_operations`; the next sync removes the task from every replica.
- **Release.** `ReleaseAll` walks the replica's pending tasks.

### Sync

taskchampion runs a whole sync as one SQLite transaction, and the server may accept an uploaded version before the client commits. A sync that devkit abandoned would roll back and redo its work, so devkit never interrupts a sync it started. Its storage and server types are not `Send`, so a sync runs in its own process.

- **`devkit todo sync`** opens the replica, takes the lock, runs `Replica::sync(server, avoid_snapshots: false)` to completion, and reports the outcome. `avoid_snapshots: false` lets a devkit replica upload a snapshot when the server asks for one, so a later container's first sync starts from it rather than replaying every version. A directory server never stores snapshots, so a new replica on one replays the whole history.
- **After every write**, when a sync target exists, the writer spawns `devkit todo sync --background` with null stdio in its own process group, then returns. The write is already committed locally.
- **Coalescing.** `--background` exits at once when another sync holds `<data_dir>/sync.lock`, or when the last sync failed less than 60 seconds ago. Each write first sets `<data_dir>/sync.pending`. A sync clears that marker before it runs and runs again if the marker is set when it finishes. One running sync therefore carries every write made while it ran, and a server that is down costs one attempt a minute.
- **Waiting for fresh lists.** Two callers wait for a sync instead of detaching, each for at most a bound. When the bound passes, the sync keeps running on its own and the caller reads the local replica:

  | Caller | Bound |
  |---|---|
  | `devkit todo context` for `SessionStart`, so a new container pulls the lists before injecting them | 20 s |
  | `devkit todo list --sync` | 20 s |

  Machines with no sync target return at once, as before.
- **Session end.** The release in `session-end` is local, and its push is a background sync like any write's. Some harnesses cap a `SessionEnd` hook's run, Codex at 3 seconds, so the hook never waits on the network, and both manifests keep its timeout at 3 seconds.
- **Nowhere else.** `UserPromptSubmit`, `PostCompact` and `SubagentStart` injections and plain `devkit todo list` read the local replica, which holds this machine's own writes.
- **Failure text.** `devkit todo sync` and `list --sync` print `devkit todo: sync failed: <reason>; changes are saved and sync with the next write` to stderr and exit 0. The reason is taskchampion's error, which carries no credential. Hooks print nothing.

The `Store` enum gets an inherent `sync(&self, wait: Option<Duration>)`, and the binary's write path calls `spawn_sync` after each successful write. Both are no-ops for the built-in and taskwarrior backends and for a replica with no target.

### Claims across replicas

Two machines syncing to one server each run the claim rule against their own replica. Two sessions on different machines can both start a todo between syncs, and the later sync wins. Sub-agents share their session's machine, so the rule holds for them. The reference says this.

### Reading the lists

The server stores the data encrypted, so a person reads it through a replica. The reference shows a taskwarrior profile for that: a separate `TASKRC` and `TASKDATA` holding the same URL, client id and secret (`sync.server.url`, `sync.server.client_id`, `sync.encryption_secret`). `task sync` then `task project:<repo>` shows the agents' lists, and alacritree's tab can read the same profile. Pointing one's everyday taskwarrior at the same server also works, at the cost of handing every container read and write access to all of one's tasks; the reference states that cost.

### Failure

The rule from #211 holds: a hook never changes a verdict or writes stdout because a store failed. A replica that fails to open makes the hooks inject, attribute and release nothing, and fails the CLI with taskchampion's error and the `data_dir` path.

### Documentation

`plugin/skills/using-devkit/references/todo.md`'s backend section gains taskchampion: when to choose it; `DEVKIT_TODO_BACKEND` for containers; the three variable names and their resolution order, including the secrets-file lines to add by hand; that `HTTPS_PROXY` is honoured; when it syncs and that a down server costs one attempt a minute; `devkit todo sync`; the profile for reading the lists; and the cross-replica claim caveat.

## Testing

- **Contract suite.** `devkit-todo-taskchampion` runs #212's contract suite against a replica in a temp dir with no sync target. Nothing is skipped: the crate needs no external program.
- **Directory sync.** Two stores on separate `data_dir`s share one `server_dir`. After `devkit todo sync` on each, a todo added on one lists on the other, and a claim made on one shows as `InProgress` on the other.
- **Wire compatibility.** Where `task` exists, a store syncs to a `server_dir`, and a private taskwarrior with `sync.local.server_dir` pointing there runs `task sync` and `task export`. The todo shows with its project, status, `holder`, `subof` and `order`. It skips the way #212's taskwarrior tests do.
- **Writers never wait on the network.** With a server URL pointing at a listener that accepts and never answers, `devkit todo add` returns in under 1 second and leaves a `devkit todo sync --background` child running. A second `add` within 60 seconds of a failed sync spawns no new attempt.
- **Interrupted sync.** A `devkit todo sync` killed mid-run leaves the replica listing the same todos as before it started, and the next sync completes.
- **Coalescing.** Writes made while a sync holds `sync.lock` are carried by that sync's rerun: after it exits, a second replica sees all of them.
- **Hook lock budget.** With the lock held by another process, a post-tool-use capture returns within about a second, exits 0 and prints nothing, and the next write applies it. During a sync stalled at the server, a concurrent `todo add` and a hook capture both reach the server once the sync ends.
- **Credentials.** The environment beats Doppler, which beats the file, using a fake `doppler` on PATH that prints JSON or fails. Doppler is never run without `doppler_project`. `server_dir` wins over credentials that resolve. A half-set server config names the missing variables, and a non-UUID client id names its variable, and neither prints a value.
- **Backend override.** `DEVKIT_TODO_BACKEND=taskchampion` with no config file at all: `devkit todo add` writes to the replica under the isolated state dir, and `devkit doctor` reports the backend as coming from the environment.
