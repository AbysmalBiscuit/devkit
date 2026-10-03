# Todo lists in taskwarrior

Issue: [#212](https://github.com/AbysmalBiscuit/devkit/issues/212). Builds on [#211](https://github.com/AbysmalBiscuit/devkit/issues/211) (`docs/superpowers/specs/2026-10-03-todo-tracking-design.md`). Follow-up: [#213](https://github.com/AbysmalBiscuit/devkit/issues/213) (taskchampion backend, local or synced).

## Problem

devkit keeps agent todo lists in its own JSON store. A person who already runs taskwarrior has a second place to look, and alacritree's tab, which reads taskwarrior, cannot see devkit's lists.

## Goal

`[todo] backend = "taskwarrior"` keeps every devkit todo in the local taskwarrior, through the `task` program, the way alacritree's backend does.

- The CLI, the context injection, claim attribution, release and native capture work unchanged on either backend.
- devkit's todos show in `task` reports, all under one root project (`devkit` by default), so the person's own projects never mix with them. alacritree's taskwarrior tab shows them only once alacritree reads the same root: its nodes are top-level projects today, so the tab does not see devkit's todos until it learns that root.
- Each backend lives in its own crate. The binary picks one through an enum, not a trait object.
- A machine with no config, such as a cloud session, keeps the built-in store.

## Out of scope

- The taskchampion backend and sync (#213).
- Running `task` inside WSL from a Windows host. On Windows, `task` is looked up like any program and reported missing when absent.
- Moving todos between backends.
- A `devkit todo setup` that writes the attribute declarations into the user's taskrc. devkit passes them on every call; a person who wants to filter on `holder` in their own reports declares it once (see Documentation).
- MCP actions for todos.

## Design

### Crates

| Crate | Holds |
|---|---|
| `devkit-todo` | `Todo`, `Status`, `Edit`, `NewTodo`, `TodoStore`, `transition`, holders, nodes, rendering, the native map, the list-replace diff, and the shared state directory |
| `devkit-todo-builtin` | `BuiltinStore`, moved out of `devkit-todo` unchanged |
| `devkit-todo-taskwarrior` | `TaskwarriorStore` and `schema`, the mapping between a todo and a taskwarrior task |

`schema` is public because #213's taskchampion backend writes the same tasks: its replica is a taskwarrior database, and a person reads it with `task`.

The native map and the context digests are not backend state. They stay at `state_dir()/todo/`, now named by `devkit_todo::state_dir()`, whichever backend is chosen.

### Dispatch

`TodoStore` becomes `#[ambassador::delegatable_trait]` and loses its `Send + Sync` bound. The binary holds one enum, the only code that names every backend crate:

```rust
#[derive(Delegate)]
#[delegate(TodoStore)]
pub enum Store {
    Builtin(BuiltinStore),
    Taskwarrior(TaskwarriorStore),
}

impl Store {
    pub fn from_config(config: &TodoConfig) -> Self;
}
```

This is the pattern `devkit_common::vcs::Vcs` already uses over `devkit_vcs::VersionControl`. Code that reads or writes todos takes `&impl TodoStore`, so tests pass a concrete store and nothing boxes.

### Trait change: `get`

```rust
/// The todo `id` names. A backend whose ids can be abbreviated resolves a
/// unique abbreviation; an ambiguous one is an error that names the matches.
fn get(&self, id: &str) -> anyhow::Result<Option<Todo>>;
```

The claim check in the shell guard reads the todos a command names. On the built-in store it listed every todo; on taskwarrior that list is the person's whole database, every project included. `get` reads one, and it resolves the short ids agents type (see Ids). `BuiltinStore` implements it as a key lookup.

### Config

```toml
[todo]
backend = "taskwarrior"   # "builtin" (default) | "taskwarrior"

[todo.taskwarrior]
path = "task"             # its own name is looked up on PATH; any other value runs as written
project = "devkit"        # the root project every todo is filed under
```

`TodoConfig` and `TaskwarriorConfig` live in `devkit-config`, with doc comments that become the schema, and a doctest example on `TodoConfig`. The schema description of `backend` says it belongs in `~/.config/devkit/config.toml`: a project that commits `backend = "taskwarrior"` breaks every machine without `task`, cloud sessions included.

The CLI resolves config from the working directory. A hook resolves it from the payload's `cwd`, as its other config reads do. A config that fails to load falls back to the built-in store in a hook, which stays silent, and is an error in the CLI.

### Mapping (`devkit-todo-taskwarrior::schema`)

| Todo | Task |
|---|---|
| `id` | `uuid` |
| `description` | `description` |
| `parent` | `subof` (UDA, type `uuid`) |
| `order` | `order` (UDA, type `numeric`), rounded to an integer on read |
| `project`, `None` | `project:<root>` |
| `project`, `Some(node)` | `project:<root>.<node>` |
| `entry`, `modified` | `entry`, `modified`, converted from taskwarrior's `20261003T120000Z` to RFC 3339 |
| `Pending` | `status:pending`, no `start` |
| `InProgress { by }` | `status:pending`, `start` set, `holder:<by>` |
| `Completed { by }` | `status:completed`, `holder:<by>` when known |
| `Cancelled { by }` | `status:deleted`, `holder:<by>` when known |

- `subof` and `order` are alacritree's attributes, so its tab nests and orders devkit's todos.
- `holder` (UDA, type `string`) is devkit's. alacritree ignores it.
- A pending task with `start` set and no `holder` was started outside devkit, by `task start` or alacritree. It reads as `InProgress { by: human }`, so no agent takes it over.
- `<root>` is `[todo.taskwarrior] project`, `devkit` by default. A global todo is written to the root itself and every other node below it; reading a task strips the root back off, so `Todo.project` is the bare node and the rest of devkit never sees the root.
- A task outside the root (neither the root nor below `<root>.`) is never a devkit todo: no list includes it, `--all` included, `get` does not resolve it, and no claim or release touches it. The person's unfiled tasks and their own projects, even one named like a node, stay out.
- Recurring templates and any status not in the table are not todos and never list.

### Running `task`

Ported from `alacritree_taskwarrior`:

- Every call passes `rc.confirmation=off` and an `rc.uda.<name>.type=` and `.label=` override for `subof`, `order` and `holder`, so a taskrc without them never folds `holder:` into a description. stdin is null.
- Every write addresses tasks by full uuid.
- A description goes after `--`, with each backslash doubled, since taskwarrior drops a single one even after `--`.
- `add` passes `rc.verbose=new-uuid` and reads the uuid from `Created task <uuid>.`.
- A failure whose stderr says the database is locked is retried after 50, 150 and 400 ms. Any other failure is an error carrying `task`'s stderr.
- A missing program is the error `taskwarrior not found: <path> ([todo.taskwarrior] path)`.

### Writes

| Edit | Command |
|---|---|
| `SetStatus` to `Pending` | `<uuid> modify status:pending start: end: holder:` |
| `SetStatus` to `InProgress { by }` | `<uuid> modify status:pending start:now holder:<by>`, leaving `start` alone when it is already set, so a handed-down claim keeps its start time |
| `SetStatus` to `Completed { by }` | `<uuid> done holder:<by>` |
| `SetStatus` to `Cancelled { by }` | `<uuid> delete holder:<by>` |
| `Describe` | `<uuid> modify -- <description>` |
| `Move` | `<uuid> modify subof:<parent or empty> order:<n>` |
| `Reorder` | `<uuid> modify order:<n>` |
| `Relocate` | `<uuid> modify project:<root>.<node>` |
| `ReleaseAll { holder }` | export `+ACTIVE` under the root, keep those whose holder `holder` covers, then `<uuids> modify start: holder:` |
| `Purge` | `<uuid> delete` unless already deleted, then `<uuid> purge` |

`SetStatus` reads the task, runs `transition`, and writes the status it returns, all under a devkit file lock at `state_dir()/todo/taskwarrior.lock`. The same lock covers `add` and `Move` without an order, which read siblings to place the todo after the last one, and `ReleaseAll`. Taskwarrior has no compare-and-swap, so the lock serializes devkit's own callers only: a person running `task start` between devkit's read and write can lose to devkit's write. The crate documents that race.

### Ids

Ids are full uuids everywhere a program reads them: the store, the native map and `--json`. Text an agent reads shows a uuid as its first eight characters, taskwarrior's `uuid.short`: the rendered lists and the uuid `devkit todo add` prints. The built-in store's numbers are shorter than eight and render as they are.

The CLI accepts any id it is given. `TaskwarriorStore::get` resolves a prefix of at least eight characters to the one uuid it starts; two matches are the error `todo id <prefix> is ambiguous: <uuid>, <uuid>`. Every write resolves first and then addresses the full uuid.

### Lists

`list` exports every task under the root, `(project.is:"<root>" or project:"<root>.") (status:pending or status:completed or status:deleted)`, and keeps the todos whose node passes `Filter::matches`, the rule the built-in store applies. A node never reaches a `task` filter: `sanitize` keeps quotes, git accepts a branch holding both `'` and `"`, and no taskwarrior quoting survives both. Placing a todo after its siblings matches their node the same way. The root is still a filter value, so `[todo.taskwarrior] project` may not hold `'` or `"`. A bare `project:<root>` would also match a sibling project such as `<root>-web`, which is why the root's subtree is spelled with the trailing dot.

### Failure

The rule from #211 holds: a hook never changes a verdict or writes stdout because a store failed. With `task` missing, broken or locked past its retries, the hooks inject nothing, attribute nothing and release nothing, and the CLI fails with the error above.

### Documentation

- `plugin/skills/using-devkit/references/todo.md` gains a short section on choosing a backend: where to set it, what `task` must be (taskwarrior 3), and the taskrc lines a person adds to see `holder` in their own reports:

  ```text
  uda.holder.type=string
  uda.holder.label=Holder
  ```

- The guide injected into agents keeps its instructions: agents use `devkit todo` or their native tool on either backend. One line changes: "Ids are the numbers in parentheses." becomes "Ids are in parentheses.", since a taskwarrior id is a hex uuid prefix, not a number.

## Testing

- **Contract tests.** `devkit-todo` exposes the behavior every backend owes as a test suite behind a `test-support` feature: add and list round-trips, parent and order, every `transition` outcome through `apply`, `ReleaseAll`, `Purge`, unknown ids, `get`. The built-in store's existing tests move into it, and `devkit-todo-builtin` and `devkit-todo-taskwarrior` each run it against their store. #213 runs it against taskchampion.
- **Taskwarrior specifics.** A private `TASKRC` and `TASKDATA` in a temp dir, so the user's data and hooks never run: attribute syntax and backslashes in descriptions stay verbatim, a hand-started task reads as held by `human`, unfiled tasks never list, `--all` leaves them out, a short id resolves and an ambiguous one fails naming both, a missing program names the config key.
- **Availability.** Each test that runs `task` first runs a real `task export` against its private taskwarrior. It returns early, silently, only when that fails because `task` is missing; any other failure fails the test, so a broken backend never passes as a skip. CI installs taskwarrior 3 on the macOS runner, where Homebrew ships it, so the suite runs on every push. The Linux and Windows runners have no `task`, so they skip these tests; Linux CI does not build taskwarrior from source.
- **Dispatch.** A CLI test with `[todo] backend = "taskwarrior"` in an isolated home config adds, starts and finishes a todo, then reads it back with `task export`.

## Unresolved

- Whether `start:now` through `modify` records a journal annotation when `journal.time` is on, as `task start` does. Harmless either way; the plan probes it.
