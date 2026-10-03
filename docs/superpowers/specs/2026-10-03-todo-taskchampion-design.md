# Todo lists in taskchampion, local or synced

Issue: [#213](https://github.com/AbysmalBiscuit/devkit/issues/213). Builds on [#211](https://github.com/AbysmalBiscuit/devkit/issues/211) (`docs/superpowers/specs/2026-10-03-todo-tracking-design.md`) and [#212](https://github.com/AbysmalBiscuit/devkit/issues/212) (`docs/superpowers/specs/2026-10-03-todo-taskwarrior-design.md`), whose `[todo]` table, `Store` enum, `schema` module and contract suite this spec extends. #212 merges first; this branch rebases onto it.

## Problem

A cloud session's todos live in a container that is deleted when the session ends. The built-in store dies with it, and the taskwarrior backend needs a `task` program the container does not have. Nobody outside the session can watch its work advance, and a later session cannot see where the last one stopped.

## Goal

`backend = "taskchampion"` keeps todos in a taskchampion replica that devkit embeds, so it needs no program beyond devkit. The replica can stay local or sync to a server, and a person reads the synced lists with taskwarrior.

- It works in a container that has only devkit and a few environment variables.
- Sync credentials come from environment variables, Doppler, or devkit's secrets file, in that order.
- A sync failure never loses a write and never blocks an agent for long.
- The CLI, context injection, claim attribution, release and native capture behave as on the other backends.

## Out of scope

- Sync to GCP or AWS buckets, and taskchampion's git sync. Their dependencies outweigh their use here.
- Pointing `data_dir` at taskwarrior's own database. That couples devkit to the on-disk format of whatever taskwarrior is installed; reading the synced lists goes through a sync instead (see Reading the lists).
- Running or deploying a sync server.
- Moving todos between backends.
- Two machines claiming the same todo between syncs (see Claims across replicas).

## Design

### Crate

`devkit-todo-taskchampion` holds `TaskchampionStore`. It depends on `devkit-todo`, on `devkit-todo-taskwarrior` for `schema` (attribute names, status mapping, the global project), and on:

```toml
taskchampion = { version = "~2.0.2", default-features = false, features = ["server-sync", "bundled"] }
```

- `~2.0.2` matches the taskchampion inside taskwarrior 3.4, the version the replicas on the person's machine run. A minor bump is a deliberate change with its own sync test (see Testing).
- `server-sync` brings the remote server client and its encryption. `bundled` compiles SQLite in, so the binary needs no system library.
- Local directory sync (`ServerConfig::Local`) needs no feature.

The binary's `Store` enum gains `Taskchampion(TaskchampionStore)`, and `TodoBackend` gains `Taskchampion`.

### Choosing the backend

`DEVKIT_TODO_BACKEND`, when set, overrides `[todo] backend`, with the same spellings. A container sets it next to the sync credentials, so it needs no config file, and the person's own machine keeps whatever their home config says. An unknown value is an error in the CLI and the built-in store in a hook, the same rule as a config that fails to load.

### Config

```toml
[todo]
backend = "taskchampion"

[todo.taskchampion]
data_dir = "/path"         # the replica; default state_dir()/todo/taskchampion
server_dir = "/path"       # sync to a directory instead of a server
doppler_project = "devkit" # optional: Doppler scope for the sync credentials
doppler_config = "dev"     # optional
```

Every key is optional, and each doc comment becomes its schema description. `TodoConfig`'s doctest gains the table.

### Sync target

Exactly one of:

| Target | When |
|---|---|
| Directory | `server_dir` is set. Uses `ServerConfig::Local`. |
| Server | `DEVKIT_TODO_SYNC_URL`, `DEVKIT_TODO_SYNC_CLIENT_ID` and `DEVKIT_TODO_SYNC_SECRET` all resolve. Uses `ServerConfig::Remote`. |
| None | Neither. The replica stays local. |

- Setting `server_dir` while any server credential resolves is an error naming both.
- Some but not all server credentials resolving is an error naming the missing ones.
- A client id that is not a UUID is an error naming the variable.

In the CLI these are errors. In a hook the store still opens, without sync, and says nothing.

### Credentials

Each of the three names resolves through, in order:

1. **The environment.** A container sets them at deploy time.
2. **Doppler.** `doppler secrets get <NAME> --plain`, with `--project` and `--config` when the config names them, else Doppler's own scope for the working directory. Reached only when the variable is unset and `doppler` is on PATH. A Doppler failure, including a secret Doppler does not hold, falls through to the next step.
3. **The secrets file**, `~/.config/devkit/secrets.toml`, under `devkit_todo_sync_url`, `devkit_todo_sync_client_id` and `devkit_todo_sync_secret`.

`devkit_common::secrets` gains this as `resolve_with_doppler(name: &str, scope: &DopplerScope) -> Option<String>`. Existing callers keep `resolve`, so no other token gains a subprocess. The Doppler step costs a process per devkit invocation that opens the store, so the reference recommends the environment, or launching the harness under `doppler run`, which puts the values in the environment.

`devkit doctor` gains one row per credential when the backend is taskchampion, giving the source each resolved from (environment, Doppler, file, or missing) and never the value.

### Store

- **Opening.** Each call opens the replica with `StorageConfig::OnDisk { taskdb_dir: data_dir, create_if_missing: true, access_mode: ReadWrite }` and holds a devkit file lock at `<data_dir>/devkit.lock` from open to the end of its sync. taskchampion's SQLite takes an immediate write lock with no busy timeout, so devkit serializes its own processes rather than letting a second one fail.
- **Mapping.** Taken from `schema`: status, `start`, `holder`, `subof`, `order`, `project:global` for the global list, and a task with no project is never a devkit todo. Writes use `Task::set_status`, `start`, `stop` and `set_value`. `entry` and `modified` are read with `get_entry` and `get_modified` and printed as RFC 3339.
- **Claims.** `SetStatus` reads the task, runs `transition`, and writes the result under the lock, so the claim rule is exact for every process on the machine.
- **Ids.** Full uuids, shown and resolved by prefix exactly as #212 specifies, through the same `short_id` and the same ambiguity error.
- **Purge.** `TaskData::delete` removes the task from the replica, and the next sync removes it everywhere.
- **Release.** `ReleaseAll` walks the replica's pending tasks, since the replica is small and local.

### When it syncs

- **After every write.** `add` and `apply` sync once their operations are committed, while still holding the lock.
- **Before a read that starts a session.** `TodoStore` gains `fn sync(&self) -> anyhow::Result<()>`, a no-op by default. `devkit todo context` calls it for the `SessionStart` event, so a new container pulls the lists before injecting them, and `devkit todo list` calls it so a person or agent asking for the list sees other sessions' work.
- **Nowhere else.** `UserPromptSubmit` and `PostCompact` injections read the local replica, which holds this session's own writes.

### Sync failure and deadline

taskchampion's client waits up to 10 seconds to connect and 60 to read, too long for a hook.

- Each sync runs on its own thread, and the caller waits at most 5 seconds for it.
- A sync that times out or fails never fails the call: the operations are already committed locally and go up with the next sync.
- Once a sync in a process has timed out, that process skips every later sync, so a command touching several todos waits once.
- A short-lived process that exits while its sync thread runs ends that thread. taskchampion applies each server version in a SQLite transaction, so an interrupted sync leaves the replica at the last whole version.
- The CLI prints `devkit todo: sync failed: <reason>; changes are saved and sync with the next write` to stderr. Hooks print nothing.

### Claims across replicas

Two machines syncing to one server each run the claim rule against their own replica. Two sessions on different machines can both start a todo between syncs, and the later sync wins. Sub-agents share their session's machine, so the rule holds for them. The reference says this.

### Reading the lists

The synced data is encrypted on the server, so a person reads it through a replica. The reference shows a taskwarrior profile for that: a separate `TASKRC` and `TASKDATA` holding the same sync URL, client id and secret. `task sync` then `task project:<repo>` shows the agents' lists, and alacritree's tab can read the same profile. Sharing credentials with one's everyday taskwarrior also works, at the cost of handing every container read and write access to all of one's tasks; the reference states that cost.

### Failure

The rule from #211 holds: a hook never changes a verdict or writes stdout because a store failed. A replica that fails to open makes the hooks inject, attribute and release nothing, and fails the CLI with taskchampion's error and the `data_dir` path.

### Documentation

`plugin/skills/using-devkit/references/todo.md`'s backend section gains taskchampion: when to choose it, the three variable names and their resolution order, `DEVKIT_TODO_BACKEND` for containers, the sync timing, the profile for reading the lists, and the cross-replica claim caveat.

## Testing

- **Contract suite.** `devkit-todo-taskchampion` runs #212's contract suite against a replica in a temp dir, with no sync target. Nothing is skipped: the crate needs no external program.
- **Directory sync.** Two stores on separate `data_dir`s with one `server_dir`: a todo added on one lists on the other after its `sync`; a claim made on one shows as `InProgress` on the other.
- **Wire compatibility.** Where `task` exists, a store syncs to a `server_dir`, and a private taskwarrior with `sync.local.server_dir` pointing there runs `task sync` and `task export`: the todo shows with its project, status, `holder`, `subof` and `order`. This test pins that taskchampion `~2.0.2` and the installed taskwarrior read each other's data. It skips the way #212's taskwarrior tests do.
- **Deadline.** A server URL pointing at a listener that accepts and never answers: `add` returns within 6 seconds, the todo lists locally, and a second `add` in the same process does not wait again.
- **Credentials.** Environment beats Doppler beats file, with a fake `doppler` on PATH that prints a value or fails; a half-set server config names the missing variables; `server_dir` together with server credentials is an error; a non-UUID client id names the variable.
- **Backend override.** `DEVKIT_TODO_BACKEND=taskchampion` with no config file at all: `devkit todo add` writes to the replica under the isolated state dir.
- **Hooks.** A broken sync config in a hook still captures a native `TaskCreate` into the local replica, exits 0 and prints nothing.

## Unresolved

- Whether taskchampion 2.0.2's remote client honours `HTTPS_PROXY`, which some cloud sandboxes require for egress. Its 3.x docs describe proxy configuration; 2.0.2's ureq client may not read the variable. The plan probes it, and if it does not, the reference says the sync server's host must be reachable directly.
