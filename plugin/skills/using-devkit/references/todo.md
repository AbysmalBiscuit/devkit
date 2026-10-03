# Todo lists: `devkit todo`

devkit keeps agent todo lists in a store outside any checkout, so a resumed session, a later session and the person running the agents all read the same lists. At session start, after compaction, when a sub-agent starts, and on each prompt whose lists changed, a hook injects the lists you can see. Track any work of three or more steps there before you start it.

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

When other sessions on your workspace left pending todos, the injected context says how many and names the `devkit todo list --subtree` command that shows them. It does so at session start, and on each later prompt while your own list has nothing open. To continue one of them, claim it with `devkit todo start <id>`. It stays on its own node.

## Ids and claims

Ids are in parentheses in the injected lists and in `devkit todo list`. `devkit todo add` prints the new one.

`devkit todo start <id>` claims a todo for you. A todo another agent has in progress shows `in progress: <name>`, and starting, finishing or dropping it fails naming the holder: pick another todo. A sub-agent claims a todo with `start` before working on it, so two sub-agents never work on the same one. A sub-agent may take over a todo its own session started, and its session starting that todo again leaves the sub-agent's claim in place. When an agent or sub-agent ends, the todos it still has in progress return to pending. In Claude Code a sub-agent's `devkit todo` commands act as that sub-agent; in Codex they act as its session, so Codex sub-agents do not exclude each other.

## Native task and plan tools

When your harness gives you a task or plan tool (Claude Code's `TaskCreate` and `TaskUpdate`, Codex's `update_plan`), use it: devkit mirrors each call into your session's node, attributed to you. Without one, use `devkit todo add`, `start`, `done` and `cancel`. Edit a mirrored todo through the native tool that made it, so the two stay in step.

Codex offers `update_plan` only when its config sets `[tools.update_plan] enabled = true`. Codex also runs the plugin's hooks only after they are trusted, so approve them when it asks. Until then, nothing mirrors and no lists are injected.

## Dropping a todo

`devkit todo cancel <id>` drops a todo and keeps the record, marked cancelled, so the person running the agents can see what was abandoned. Cancelled todos are counted rather than listed.

`devkit todo purge` deletes a record for good, for text that must disappear such as a pasted secret. It needs a person at a terminal and refuses an agent; cancel instead.

## Backends

The built-in store is the default: one file in devkit's state directory. `[todo] backend = "taskwarrior"` keeps the todos in the local taskwarrior instead, through taskwarrior 3's `task` program, all under one root project. Set it in `~/.config/devkit/config.toml`, not in a repository's `devkit.toml`: a committed value breaks every machine without `task`, cloud sessions included. `[todo.taskwarrior] path` names another `task` program, and `[todo.taskwarrior] project` another root. Agents use `devkit todo` or their native tool on either backend.

On taskwarrior:

- A todo's id is its task's uuid, shown as the first 8 characters. Any prefix of 8 or more that names one task works wherever an id does.
- Todos live under the root project, `devkit` unless configured: a global todo on `devkit` itself, any other node on `devkit.<node>`, such as `devkit.repo.main`. A task outside the root is never a todo: no list shows it, `--all` included, and no claim or release touches it, so your own projects and unfiled tasks stay out.
- alacritree's taskwarrior tab reads its nodes as top-level projects, so it does not show todos under the root.
- A task started outside devkit, with `task start` or in alacritree, counts as held by a person when it has no `holder`, so no agent takes it over. A task that still carries an agent's `holder` stays that agent's claim, even after someone stops and restarts it with `task`.
- devkit records who holds a todo in a `holder` attribute. To see it in your own `task` reports, add these lines to your taskrc:

  ```text
  uda.holder.type=string
  uda.holder.label=Holder
  ```
