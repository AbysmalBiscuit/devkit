# Todo lists: `devkit todo`

devkit keeps agent todo lists in its own store, outside any checkout, so a resumed session, a later session and the person running the agents all read the same lists. At session start, after compaction, when a sub-agent starts, and on each prompt whose lists changed, a hook injects the lists you can see. Track any work of three or more steps there before you start it.

## Where your todos live

A list belongs to a node. Nodes nest on `.`:

| Node | Holds |
|---|---|
| `global` | todos outside any repository |
| `<repo>` | the project's todos, shared by every worktree |
| `<repo>.<branch>` | the workspace's todos |
| `<repo>.<branch>.<harness>-<session>` | one agent session's todos |

`<repo>` is the main checkout's directory name, so every linked worktree shares it. `<branch>` is the checkout's branch, or its directory name on a detached head. A `.`, `/` or `\` inside a name becomes `-`. `devkit todo scope` prints your own node, and `devkit todo add` writes there unless told otherwise.

You see your own node and every node above it, never another session's. A sub-agent shares its session's node, so it sees and writes the same list as the agent that started it.

## Ids and claims

Ids are the numbers in parentheses in the injected lists and in `devkit todo list`. `devkit todo add` prints the new one.

`devkit todo start <id>` claims a todo for you. A todo another agent has in progress shows `in progress: <name>`, and starting, finishing or dropping it fails naming the holder: pick another todo. A sub-agent claims a todo with `start` before working on it, so two sub-agents never work on the same one. A sub-agent may take over a todo its own session started, and its session starting that todo again leaves the sub-agent's claim in place. When an agent or sub-agent ends, the todos it still has in progress return to pending.

## Native task and plan tools

When your harness gives you a task or plan tool (Claude Code's `TaskCreate` and `TaskUpdate`, Codex's `update_plan`), use it: devkit mirrors each call into your session's node, attributed to you. Without one, use `devkit todo add`, `start`, `done` and `cancel`. Edit a mirrored todo through the native tool that made it, so the two stay in step.

Codex offers `update_plan` only when its config sets `[tools.update_plan] enabled = true`. Codex also runs the plugin's hooks only after they are trusted, so approve them when it asks. Until then, nothing mirrors and no lists are injected.

## Dropping a todo

`devkit todo cancel <id>` drops a todo and keeps the record, marked cancelled, so the person running the agents can see what was abandoned. Cancelled todos are counted rather than listed.

`devkit todo purge` deletes a record for good, for text that must disappear such as a pasted secret. It needs a person at a terminal and refuses an agent; cancel instead.
