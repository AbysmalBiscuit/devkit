# Todo tracking for agents

Issue: [#211](https://github.com/AbysmalBiscuit/devkit/issues/211). Follow-ups: [#212](https://github.com/AbysmalBiscuit/devkit/issues/212) (taskwarrior backend), [#213](https://github.com/AbysmalBiscuit/devkit/issues/213) (online backends), [#214](https://github.com/AbysmalBiscuit/devkit/issues/214) (agentic tracking umbrella).

## Problem

Agents do not track their work unless told to. Claude Code's task tools are off by default in a local install, and in a cloud session an agent uses them only when the prompt asks. Where an agent does track, the list lives inside the harness: a resumed session cannot see where the last one stopped, and the person running the agents cannot watch work advance across sessions.

alacritree solves part of this with a task backend over taskwarrior and a hook that injects the lists. That hook is a Windows binary reached through WSL interop, and taskwarrior is a separate install, so neither exists in a headless environment such as a cloud session. The devkit plugin does run there.

## Goal

devkit owns agent todo lists: one store, one CLI, and the hooks that keep agents writing to it.

- A todo list is readable by a resumed session, by the person after the run, by the agent during the run, and by anyone watching work across sessions.
- An agent tracks multi-step work without being told to in the prompt. The injected context asks it to, and its native task or plan tool is mirrored when it uses one.
- Parallel sessions keep separate lists. Sub-agents share their parent's list and claim todos from it, so two never work on the same one.
- It works with nothing installed beyond devkit.
- alacritree's tab can read and write the same lists through its existing command backend.

## Out of scope

- Backends other than the built-in store. Taskwarrior is #212; online trackers are #213.
- MCP actions for todos, and exposing todos as harness-native tools. Agents use the CLI or their native tool, which is mirrored.
- Cursor. pabal parses its payloads, but what its task tools send is unknown.
- Due dates, priorities, tags, recurrence, dependencies, annotations and sync.
- Pruning completed todos.
- Changes to alacritree. Its migration to the command backend happens in that repository.

## Naming

The feature is `todo`: crate `devkit-todo`, CLI `devkit todo`, config `[todo]`, types `Todo` and `TodoStore`. `task` is taken: `[tasks]` in `devkit.toml` holds devrun's canned commands, `devrun task <name>` runs them, and the session brief lists them as tasks. Two meanings of one word in an agent's context would make it guess.

## Design

### Model (`devkit-todo`)

```rust
pub struct Todo {
    pub id: String,
    pub description: String,
    pub status: Status,
    pub parent: Option<String>,
    pub order: Option<i64>,
    pub project: Option<String>,
    pub entry: Option<String>,
    pub modified: Option<String>,
}

pub enum Status {
    Pending,
    InProgress { by: Holder },
    Completed { by: Option<Holder> },
    Cancelled { by: Option<Holder> },
}

pub enum StatusKind { Pending, InProgress, Completed, Cancelled }

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
```

- `id` is opaque to callers: a short number in the built-in store, a uuid in taskwarrior, an issue key online.
- `project` is the node the todo belongs to (see Nodes). Absent is the global list.
- `entry` and `modified` are RFC 3339 UTC, so they sort as text.
- `parent` naming a todo the listing does not hold leaves the todo at the top level.
- `Completed` and `Cancelled` record who finished the todo, for observability. `None` means unknown: a backend that does not record it, or a todo finished before a holder existed.
- `Cancelled` is a soft delete that keeps the record, the way taskwarrior's `deleted` status does. Agents drop todos by cancelling them.
- `Purge` removes a todo for good. It exists for a description that must disappear, such as a pasted secret. The CLI refuses it to an agent caller (see CLI).
- `TodoStore` is a `dyn` trait, as `Tracker` and `Forge` are. A backend that cannot apply an edit atomically documents the race.

`SetStatus` carries the target kind and the actor, never a finished `Status`, so the stored holder is always derived by one rule.

#### Holders and claims

A holder is `S` for a session's main agent and `S/a` for its sub-agent `a`, the format the lock registry already uses (`src/bin/devkit/hook/payload.rs`). A human acting through the CLI holds as `human`.

`h` covers `a` when `h == a`, when `a` starts with `h/`, or when `h` is `human`. A session covers its own sub-agents, and a human can reset any claim.

One pure function decides every status change, and every backend calls it before writing:

```rust
pub fn transition(current: &Status, to: StatusKind, actor: &Holder)
    -> Result<Option<Status>, Claimed>;
```

- `Ok(None)` means nothing changes.
- From `InProgress { by: a }`, any change by an actor that does not cover `a` fails with `Claimed { by: a }`.
- To `InProgress` from `InProgress { by: a }`, by an actor that covers `a`, is a no-op. A sub-agent's claim survives the parent session repeating it.
- To `Completed` or `Cancelled` from `InProgress { by: a }`, by an actor that covers `a`, records `by: Some(a)`. Otherwise it records `by: Some(actor)`.
- To `Pending` drops the holder.
- To the current kind from `Pending`, `Completed` or `Cancelled` is a no-op.

`ReleaseAll { holder }` moves each `InProgress { by: a }` that `holder` covers back to `Pending`. `human` never releases in bulk.

#### Nodes

A node names where a todo list lives. Nodes nest on `.`:

| Place | Node |
|---|---|
| global | `global` |
| project | `<repo>` |
| workspace | `<repo>.<branch>` |
| agent session | `<repo>.<branch>.<harness>-<session>` |

`<repo>` is the directory name of the main checkout, so every linked worktree shares it, with a bare repository's `.git` suffix stripped. `<branch>` is the checkout's branch, or its directory name when the head names no branch. Outside any repository the place is global. These are the rules of alacritree's `tasks::facts::place_from`.

A segment's `.`, `/` and `\` become `-`, so a branch never invents a level. `<harness>` is `claude` or `codex`. The format matches alacritree's `alacritree_tasks::scope::node` byte for byte, so both tools address the same lists.

`Filter` matches nodes exactly or by subtree. A subtree `r` covers `r` and `r.main.claude-1`, never `r-web`.

An agent sees its own session node and every node above it, never a sibling session's node. Sub-agents use their parent's session node.

### Built-in store

- **Location.** `state_dir()/todo/todos.json`, guarded by `state_dir()/todo/todo.lock`. Lists are not per checkout: global and project lists span checkouts, removing a worktree must not delete the record of its work, and observability reads one store.
- **Writes.** Every access goes through `store::with_lock_strict`. An unreadable file aborts the call and stays on disk untouched, rather than being replaced by an empty document.
- **Document.** `{ version, next_id, todos: BTreeMap<id, Todo> }`. `Document::salvage` recovers each todo that still parses through `salvage_map`.
- **Ids.** `next_id` hands out sequential numbers under the lock, so agents type `devkit todo done 17`.
- **Order.** `add` without an order places the todo after its last sibling. Orders leave gaps of 1024, as alacritree's do, so a move rarely renumbers siblings.

### CLI: `devkit todo`

| Verb | Behavior |
|---|---|
| `scope` | Prints the caller's node |
| `list [--node N \| --subtree N \| --all] [--json]` | Defaults to the nodes the caller sees. `--json` prints alacritree's task shape |
| `add <text> [--parent ID] [--order N] [--node N]` | Prints the new id. The node defaults to `scope` |
| `start`, `stop`, `done`, `undone`, `cancel <id>` | `SetStatus` to `InProgress`, `Pending`, `Completed`, `Pending`, `Cancelled` |
| `describe <id> <text>` | `Describe` |
| `move <id> [--parent ID] [--order N]` | `Move`, or `Reorder` when only the order changes |
| `purge <id>` | `Purge`, refused when `caller::caller()` is `Agent` |
| `context [--guide full\|none] [--if-changed]` | The hook context block (see Context injection) |

- **Actor.** An agent's actor is its harness session id from the environment. A human's is `human`. The pre-tool-use hook supplies a sub-agent's actor (see Claim attribution).
- **Errors.** A claim conflict names the holder. An unknown id says so.
- **`--json` compatibility.** `InProgress` prints as `started: true`, and `parent`, `order`, `project`, `entry` and `modified` keep alacritree's names. `Cancelled` is left out of the JSON listing, as alacritree leaves out deleted tasks. alacritree's command template sets `DEVKIT_CALLER=human` on its `delete` arguments, so its tab can purge.
- **Not a security boundary.** An agent can set `DEVKIT_CALLER` or edit the store file. The purge gate stops habitual use, and the agent guide never mentions purge.

### Config

```toml
[todo]
backend = "builtin"
```

`backend` is an enum, so the schema rejects an unknown value, as `[tracker] kind` does. #211 ships `builtin` only. The setting usually lives in the personal layer, and a repository can override it through the normal layer merge. The field's doc comment is its schema description, the type carries a doctest example, and `schema/devkit-config.json` is regenerated.

### Context injection

The hooks rule allows stdout only from `pre-tool-use`, so context comes from a separate command, as `devkit brief` and `devkit rules context` do. `devkit todo context` reads the hook payload from stdin for the session, `agent_id` and cwd, and prints nothing on any failure.

| Event | Command |
|---|---|
| `SessionStart` (startup, resume, clear) | `devkit todo context` |
| `PostCompact` | `devkit todo context` |
| `SubagentStart` | `devkit todo context` |
| `UserPromptSubmit` | `devkit todo context --guide none --if-changed` |

Codex sends both `PostCompact` and a `SessionStart` with the `compact` source after compaction. The command runs on `PostCompact` only, so the block lands once.

**Rendering.** For each visible node, deepest first:

- Pending and in-progress todos in full, in tree order: `- [ ] text (17)`, and `- [ ] text (17, in progress: a1)` when another holder has it.
- Completed and cancelled todos collapsed to one line, such as `12 done, 1 cancelled`.
- A node with nothing open is left out.

**`--if-changed`** compares a digest of the rendered block with the last one injected for the session, kept under `state_dir()/todo/digests/`, and prints nothing when it matches. Every injection records its digest, so the first prompt after session start does not repeat an unchanged list.

**The agent guide** precedes the lists under `--guide full`. It names the node the agent writes to and says:

- track any work of three or more steps before starting it;
- with a built-in task or plan tool, use it, and devkit mirrors it; otherwise use `devkit todo add`, `start`, `done` and `cancel`;
- a sub-agent claims a todo with `start` before working on it;
- ids are the numbers shown.

The guide does not depend on the backend.

### Claim attribution

A sub-agent's shell carries its parent's session id, so `devkit todo start 17` run by sub-agent `a1` cannot tell it apart from its parent. The pre-tool-use hook knows: the payload carries `agent_id`, and `payload.holder()` gives `S/a1`. The lock registry attributes shell writes the same way.

When the shell guard's analysis finds `devkit todo start|stop|done|undone|cancel <id>` in a sub-agent's command, the hook applies the `SetStatus` itself with actor `S/a1`. A `Claimed` result denies the command and names the holder. A store error allows it. The command then runs with actor `S`, which covers `S/a1`, so `transition` leaves the sub-agent's record in place.

### Release

The existing `subagent-stop` verb applies `ReleaseAll` for `S/a`. The `session-end` verb applies it for `S`, which covers every `S/a`. A crashed agent's todos return to pending rather than showing as in progress forever. Neither verb writes stdout.

### Native tool capture

`devkit hook post-tool-use` mirrors the harness's own task and plan tools into the store. It writes no stdout. A store failure or a `Claimed` result is logged and skipped, since the native tool has already run.

Payloads, as recorded from Claude Code 2.1.288:

```json
{"tool_name":"TaskCreate","tool_input":{"subject":"alpha","description":"alpha"},
 "tool_response":{"task":{"id":"1","subject":"alpha"}}}
{"tool_name":"TaskUpdate","tool_input":{"taskId":"1","status":"in_progress"},
 "tool_response":{"success":true,"taskId":"1","statusChange":{"from":"pending","to":"in_progress"}}}
```

Codex's `update_plan` takes `{ explanation?, plan: [{ step, status }] }` with `pending`, `in_progress` and `completed` (`codex-rs/protocol/src/plan_tool.rs`). Its default `post_tool_use_payload` covers function tools, so `PostToolUse` fires for it.

| Tool | Mapping |
|---|---|
| Claude `TaskCreate` | `add` on the session node with `subject` as the description; records the native id |
| Claude `TaskUpdate` | Looks up the native id. `pending`, `in_progress`, `completed` and `deleted` become `SetStatus` to `Pending`, `InProgress`, `Completed` and `Cancelled` with the payload's holder as actor. A changed `subject` becomes `Describe`. Other fields are ignored |
| Codex `update_plan`, Claude `TodoWrite` | Each call resends the whole list. Steps are matched to the previous list by exact text: a new step is added, a missing step is cancelled, a changed status becomes `SetStatus`, and a moved step becomes `Reorder` |

- **Native id map.** `(harness, session, native id) -> todo id`, kept apart from the todos in `state_dir()/todo/native.json` under its own `store` lock, so every backend gets capture. A crash between `add` and the map write leaves a todo whose later native updates are skipped.
- **Reused ids.** A `TaskCreate` whose native id is already mapped replaces the mapping. A resumed session that numbers from 1 again cannot edit the previous run's todos.
- **List-replace snapshots.** The previous list per session is kept beside the map. A renamed step reads as a cancel and an add.
- **Matchers.** Claude's `PostToolUse` matcher gains `TaskCreate|TaskUpdate|TodoWrite`. Codex's gains `update_plan`.
- `TaskCreated` and `TaskCompleted` hook events are not used. No event marks a task in progress, so `PostToolUse` has to carry capture, and those events would only duplicate it.

### Documentation

Behavior an agent needs goes in a new skill reference, `plugin/skills/using-devkit/references/todo.md`. Flag meanings stay in clap help and the config key's meaning stays on its field.

## Testing

TDD throughout; each item's failing test comes first.

- **`transition`.** A table over every current status, target kind and actor relation: same holder, covering session, sibling sub-agent, `human`.
- **Nodes and filters.** The node test vectors from `alacritree_tasks::scope`, so the two stay byte-identical, and the subtree filter's `r-web` case.
- **Store.** Concurrent `add`s from parallel processes all land with unique ids. A corrupt `todos.json` aborts the call and stays on disk.
- **CLI.** `purge` is refused with `DEVKIT_CALLER=agent` and allowed with `human`. `list --json` parses as alacritree's task shape.
- **Hooks.** Driven through `devkit hook` with real payloads on stdin:
  - a sub-agent's Bash `devkit todo start` records `S/a1`;
  - a sibling's later `start` on the same todo is denied, naming `S/a1`;
  - `subagent-stop` returns its claims to pending;
  - the recorded `TaskCreate` and `TaskUpdate` payloads produce the matching todos;
  - an `update_plan` sequence produces adds, a cancel and a reorder.
- **Context.** A `SessionStart` payload yields an `additionalContext` envelope with the guide and the condensed lists. A repeated `--if-changed` prints nothing.
- **Evals.**
  - A text case, `evals/todo-guide`, checks that agents understand the guide.
  - A scenario, `evals/scenarios/todo-unprompted`, gives a multi-step prompt that never mentions todos and checks that the agent added todos and finished them.

  The scenario is the success measure for agents tracking without being told.

## Open questions

- Whether a Claude Code sub-agent shares its parent's native task list is undocumented. If it does not, native ids collide between parent and sub-agent, and the native id map must key on the holder rather than the session. A probe with a sub-agent settles it before capture is built.
- Whether Codex's `SubagentStart` passes added context to the sub-agent is unverified. Claude Code's documentation says it does.
