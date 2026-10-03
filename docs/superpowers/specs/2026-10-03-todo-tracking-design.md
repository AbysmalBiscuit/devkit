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

- Backends other than the built-in store, and the config that would choose between backends. Taskwarrior is #212, which adds `[todo] backend` along with its second value; online trackers are #213.
- MCP actions for todos, and exposing todos as harness-native tools. Agents use the CLI or their native tool, which is mirrored.
- Cursor. pabal parses its payloads, but what its task tools send is unknown.
- Due dates, priorities, tags, recurrence, dependencies, annotations and sync.
- Pruning completed todos.
- Changes to alacritree. Its migration to the command backend happens in that repository.

## Naming

The feature is `todo`: crate `devkit-todo`, CLI `devkit todo`, config `[todo]` once a second backend needs one, types `Todo` and `TodoStore`. `task` is taken: `[tasks]` in `devkit.toml` holds devrun's canned commands, `devrun task <name>` runs them, and the session brief lists them as tasks. Two meanings of one word in an agent's context would make it guess.

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

A holder is `S` for a session's main agent and `S/a` for its sub-agent `a`, the format the lock registry already uses. A human acting through the CLI holds as `human`. `devkit-todo` defines `Holder` and `covers()`. The hook's own `Holder` lives in the `devkit` binary (`src/bin/devkit/hook/payload.rs`) and converts at the hook edge.

`h` covers `a` when `h == a`, when `a` starts with `h/`, or when `h` is `human`. A session covers its own sub-agents, and a human can reset any claim.

One pure function decides every status change, and every backend calls it before writing:

```rust
pub fn transition(current: &Status, to: StatusKind, actor: &Holder)
    -> Result<Option<Status>, Claimed>;
```

- `Ok(None)` means nothing changes.
- To `InProgress` from `InProgress { by: a }`, by an actor that covers `a`, is a no-op. A sub-agent's claim survives the parent session repeating it.
- To `InProgress` from `InProgress { by: a }`, by an actor that `a` covers, hands the claim down: it records `by: actor`. A parent that started a todo and then delegates it does not lock its own sub-agent out. A person's claim is never handed down: `human` covers every holder, so this rule alone would let any agent take it.
- Any other change from `InProgress { by: a }`, by an actor that does not cover `a`, fails with `Claimed { by: a }`. Siblings always conflict.
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

A segment's `.`, `/` and `\` become `-`, so a branch never invents a level. `<harness>` is `claude` or `codex`.

In a hook, the harness comes from `--harness` and the session from the payload. In the CLI, both come from the environment: `CODEX_SESSION_ID` gives `codex`, otherwise `CLAUDE_CODE_SESSION_ID` gives `claude`. Codex wins when both are set, as in alacritree's `scope::session_from_env`. This differs on purpose from `devkit-locks`, which calls two different values ambiguous (`crates/devkit-locks/src/ident.rs`), because a node must be byte-identical to alacritree's.

The format matches alacritree's `alacritree_tasks::scope::node` byte for byte, so both tools address the same lists.

`Filter` matches nodes exactly or by subtree. A subtree `r` covers `r` and `r.main.claude-1`, never `r-web`.

An agent sees its own session node and every node above it, never a sibling session's node. Sub-agents use their parent's session node.

### Built-in store

- **Location.** `state_dir()/todo/todos.json`, guarded by `state_dir()/todo/todo.lock`. Lists are not per checkout: global and project lists span checkouts, removing a worktree must not delete the record of its work, and observability reads one store.
- **Writes.** Every access goes through `store::with_lock_strict`, which loads through `try_load`. An unreadable file, or one whose schema no longer parses, aborts the call with `try_load`'s message and stays on disk untouched. Nothing is salvaged automatically: losing a list silently is worse than a call that fails and names the file. `Document::salvage` is implemented because the trait requires it, but this path never calls it.
- **Document.** `{ version, next_id, todos: BTreeMap<u64, Todo> }`. Keys are numbers, so `10` sorts after `9`. Ids are stringified at the trait edge.
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
- **`--json` compatibility.** `InProgress` prints as `started: true`, and `parent`, `order`, `project`, `entry` and `modified` keep alacritree's names. `Cancelled` is left out of the JSON listing, as alacritree's taskwarrior backend leaves out deleted tasks. alacritree's `delete` command maps to `devkit todo cancel`, so a todo deleted in its tab disappears from the tab and stays on record.
- **Purge needs a terminal.** alacritree runs its commands with no terminal and its command templates carry no environment, so `caller()` classifies it as an agent and it cannot purge. That is intended: purge is for a person at a shell.
- **Not a security boundary.** An agent can set `DEVKIT_CALLER` or edit the store file. The purge gate stops habitual use, and the agent guide never mentions purge.

### Context injection

The hooks rule allows stdout only from `pre-tool-use`, so context comes from a separate command, as `devkit brief` and `devkit rules context` do. `devkit todo context` reads the hook payload from stdin for the session, `agent_id` and cwd, and prints nothing on any failure.

| Event | Command |
|---|---|
| `SessionStart` (startup, resume, clear) | `devkit todo context` |
| `PostCompact` | `devkit todo context` |
| `SubagentStart` | `devkit todo context` |
| `UserPromptSubmit` | `devkit todo context --guide none --if-changed` |

Each line passes `--harness`, as the other hook commands do: `claude-code` in `plugin/hooks/hooks.json`, `codex` in `hooks-codex.json`. Codex sends both `PostCompact` and a `SessionStart` with the `compact` source after compaction. The command runs on `PostCompact` only, so the block lands once.

**Rendering.** For each visible node, deepest first:

- Pending and in-progress todos in full, in tree order: `- [ ] text (17)`, and `- [ ] text (17, in progress: a1)` when another holder has it.
- Completed and cancelled todos collapsed to one line, such as `12 done, 1 cancelled`.
- A node with nothing open is left out.

**`--if-changed`** compares a digest of the rendered lists, never the guide, with the last one injected for the holder, and prints nothing when it matches. Every injection records its digest, so the first prompt after session start does not repeat an unchanged list, even though session start also printed the guide. The digest file is keyed on a hash of the full holder, as `rules::fired_path` keys its fired-set (`src/bin/devkit/hook/rules.rs`), under `state_dir()/todo/digests/`. A sub-agent's injection never suppresses its parent's.

**The agent guide** precedes the lists under `--guide full`. It names the node the agent writes to and says:

- track any work of three or more steps before starting it;
- with a built-in task or plan tool, use it, and devkit mirrors it; otherwise use `devkit todo add`, `start`, `done` and `cancel`;
- a sub-agent claims a todo with `start` before working on it;
- ids are the numbers shown.

The guide does not depend on the backend.

### Claim attribution

A sub-agent's shell carries its parent's session id, so `devkit todo start 17` run by sub-agent `a1` cannot tell it apart from its parent. The pre-tool-use hook knows: the payload carries `agent_id`, and `payload.holder()` gives `S/a1`. The lock registry attributes shell writes the same way.

When the shell guard's analysis finds `devkit todo start|stop|done|undone|cancel <id>` in a sub-agent's command, the hook applies the `SetStatus` itself with actor `S/a1`. The command then runs with actor `S`, which covers `S/a1`, so `transition` leaves the sub-agent's record in place.

- **Ordering.** The todo stage runs last, after every other block source in the shell path has decided, and only when none of them blocks. A command that the guard or the write gate denies never leaves a claim behind.
- **Gating.** The stage runs whenever the payload names a sub-agent, whatever the command guard, the write gate and logging are set to. When all three are off, the shell path returns early without analysing the command (`src/bin/devkit/hook/shell.rs`). The todo stage still runs in that case, since no other source can block, so attribution works in a checkout without harness enforcement.
- **Verdict.** A `Claimed` result is one more block reason, naming the holder. A store error allows the command, and the claim lands as `S`.

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
| Codex `update_plan`, Claude `TodoWrite` | Each call resends the whole list, which is diffed against the holder's previous list (see List-replace diffing) |

- **Native id map.** `(harness, holder, native id) -> todo id`, kept apart from the todos in `state_dir()/todo/native.json` under its own `store` lock, so every backend gets capture. `TaskCreate` records under the session's holder, because a Claude Code sub-agent shares its parent's native task ids. `TaskUpdate` looks up its own holder first, then the session's holder, so updates work whether or not a sub-agent shares its parent's native list. A crash between `add` and the map write leaves a todo whose later native updates are skipped.
- **Reused ids.** A `TaskCreate` whose native id is already mapped for that holder replaces the mapping. A resumed session that numbers from 1 again cannot edit the previous run's todos.
- **List-replace snapshots.** The previous list is kept per holder, beside the map. Each agent context keeps its own list, so a sub-agent's first call never reads as cancelling its parent's steps.

#### List-replace diffing

Matching is positional within equal text, so repeated steps such as two "run tests" stay distinct:

1. Walk the new list in order. Pair each step with the first unpaired previous step that has the same text.
2. An unpaired previous step is cancelled. An unpaired new step is added.
3. A paired step whose status changed gets `SetStatus`.
4. Every step whose index changed gets `Reorder` to `index * 1024`.

A renamed step reads as a cancel and an add.
- **Matchers.** Claude's `PostToolUse` matcher gains `TaskCreate|TaskUpdate|TodoWrite`. Codex's gains `update_plan`.
- `TaskCreated` and `TaskCompleted` hook events are not used. No event marks a task in progress, so `PostToolUse` has to carry capture, and those events would only duplicate it.

### Documentation

Behavior an agent needs goes in a new skill reference, `plugin/skills/using-devkit/references/todo.md`. Flag meanings stay in clap help and the config key's meaning stays on its field.

## Testing

TDD throughout; each item's failing test comes first.

- **`transition`.** A table over every current status, target kind and actor relation: same holder, covering session, a sub-agent taking over its parent's claim, sibling sub-agent, `human`.
- **Nodes and filters.** The node test vectors from `alacritree_tasks::scope`, so the two stay byte-identical, and the subtree filter's `r-web` case.
- **Store.** Concurrent `add`s from parallel processes all land with unique ids. Id `10` lists after `9`. A corrupt `todos.json` aborts the call and stays on disk.
- **CLI.** `purge` is refused with `DEVKIT_CALLER=agent` and allowed with `human`. `list --json` parses as alacritree's task shape. `scope` picks `codex` when both session variables are set.
- **Hooks.** Driven through `devkit hook` with real payloads on stdin:
  - a sub-agent's Bash `devkit todo start` records `S/a1`, including in a checkout with every harness gate off;
  - a sibling's later `start` on the same todo is denied, naming `S/a1`;
  - a compound command that the write gate denies leaves no claim;
  - `subagent-stop` returns its claims to pending;
  - the recorded `TaskCreate` and `TaskUpdate` payloads produce the matching todos;
  - an `update_plan` sequence with a repeated step produces adds, a cancel and reorders, and keeps both repeats;
  - a sub-agent's first list-replace call leaves its parent's todos untouched.
- **Context.** A `SessionStart` payload yields an `additionalContext` envelope with the guide and the condensed lists. A following `UserPromptSubmit` with `--if-changed` and unchanged lists prints nothing. A `SubagentStart` injection does not suppress the parent's next one.
- **Evals.**
  - A text case, `evals/todo-guide`, checks that agents understand the guide.
  - A scenario, `evals/scenarios/todo-unprompted`, gives a multi-step prompt that never mentions todos and checks that the agent added todos and finished them.

  The scenario is the success measure for agents tracking without being told.

## Probes before capture

Native capture is built from recorded payloads, as `TaskCreate` and `TaskUpdate` already are. Each headless probe below runs before the capture code it informs, and its payloads become test fixtures:

- **`TodoWrite`.** The payload shape is unrecorded.
- **`TaskUpdate` with `status: "deleted"`.** Not yet observed.
- **Sub-agents.** Whether a Claude Code sub-agent's native task ids are its own or shared with its parent, and whether a Codex sub-agent's payload carries the root session id. Keying on the holder with a session fallback works either way; the probe confirms it.
- **`SubagentStart` context in Codex.** Whether added context reaches the sub-agent is unverified. Claude Code's documentation says its own does.
