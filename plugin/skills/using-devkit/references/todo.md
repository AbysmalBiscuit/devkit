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

`devkit todo start <id>` claims a todo for you. A todo another agent has in progress shows `in progress: <name>`, and starting, finishing or dropping it fails naming the holder: pick another todo. A sub-agent claims a todo with `start` before working on it, so two sub-agents never work on the same one. A sub-agent may take over a todo its own session started, and its session starting that todo again leaves the sub-agent's claim in place. When an agent or sub-agent ends, the todos it still has in progress return to pending. In Claude Code and Codex, a sub-agent's `devkit todo` commands run in Bash act as that sub-agent. Run in any other shell, such as PowerShell (Codex's shell on Windows), they act as its session, so those sub-agents do not exclude each other.

## Stopping with open todos

In Claude Code and Codex, ending your turn while you have open todos is refused once: the stop hook sends you back with the list. Open todos are the pending todos on your own session's node and the todos you have in progress on any node. A sub-agent's are only the todos it has in progress. Finish each one, or cancel one that no longer applies (`devkit todo cancel <id>`, or delete it in your task tool).

Before you stop to ask the user something:

- With a clear recommendation, take it and say so in your final report.
- Without one, ask a sub-agent on a bigger model and take its answer.
- Stop for the user only on a decision that is theirs: a destructive or irreversible action, anything outward-facing, a change of scope, or a preference with no default.

Ending your turn again with the list unchanged goes through, so that is how you stop for such a decision. A change to the list, a new prompt or a compaction re-arms the reminder. `[todo] hold_stop = false` turns it off.

## Native task and plan tools

When your harness gives you a task or plan tool (Claude Code's `TaskCreate` and `TaskUpdate`, Codex's `update_plan`), use it: devkit mirrors each call into your session's node, attributed to you. Without one, use `devkit todo add`, `start`, `done` and `cancel`. Edit a mirrored todo through the native tool that made it, so the two stay in step.

Codex offers `update_plan` only when its config sets `[tools.update_plan] enabled = true`. Codex also runs the plugin's hooks only after they are trusted, so approve them when it asks. Until then, nothing mirrors and no lists are injected.

## Dropping a todo

`devkit todo cancel <id>` drops a todo and keeps the record, marked cancelled, so the person running the agents can see what was abandoned. Cancelled todos are counted rather than listed.

`devkit todo purge` deletes a record for good, for text that must disappear such as a pasted secret. It needs a person at a terminal and refuses an agent; cancel instead.

## Activity

devkit records each subagent run and each stretch a todo spends in progress, whether or not the harness log is on: in the todo database on the postgres backend, and beside the todo state on every other. `devkit activity` reports them over a date range, grouped by session, agent type and todo, and `--json` gives the same report as JSON. `devkit activity -h` gives the range's defaults.

- A run starts at the subagent's start hook and ends at its stop. Without a stop it ends at its session's end. Without either, it ends as `lost` at the agent's last hook once the agent has been silent past a backstop, so no run stays open for good. Parallel runs, two of one type included, are told apart by agent id.
- A run whose payload names no agent type, such as a Claude Code fork, stores none and reports as `subagent`.
- An interval starts when a holder claims a todo and ends `completed`, `cancelled`, `released` (stopped, or released when its holder ended), or `handed` when another holder takes it over, whose own interval starts there.
- Only what devkit sees is recorded. A todo started with `task start` or in alacritree has no interval, and Cursor runs are not recorded because its manifest wires no subagent start.

## Backends

The built-in store is the default: one file in devkit's state directory. `[todo] backend = "taskwarrior"` keeps the todos in the local taskwarrior instead, through taskwarrior 3's `task` program, all under one root project. Set it in `~/.config/devkit/config.toml`, not in a repository's `devkit.toml`: a committed value breaks every machine without `task`, cloud sessions included. `[todo.taskwarrior] path` names another `task` program, and `[todo] project` another root. Agents use `devkit todo` or their native tool on either backend.

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

### Taskchampion

`[todo] backend = "taskchampion"` keeps the todos in a taskchampion replica that devkit embeds, so it needs no program beyond devkit. Choose it for a container or cloud session, whose todos would otherwise be deleted with it, or to watch a session's lists from another machine. The replica holds the same tasks as the taskwarrior backend, under the same `[todo] project` root, with the same ids. It lives in devkit's state directory unless `[todo.taskchampion] data_dir` names another. taskwarrior 3.5 or later reads a synced replica's lists.

`DEVKIT_TODO_BACKEND=taskchampion` chooses it without a config change, so a container sets it next to the sync credentials and keeps loading the project's `devkit.toml`. It accepts the same values as `[todo] backend` and wins over it. `devkit doctor` shows the backend in effect and where it came from.

The replica syncs to the first of these that applies, or stays local:

1. `[todo.taskchampion] server_dir`, a directory shared with other replicas.
2. A taskchampion sync server, when `DEVKIT_TODO_SYNC_URL`, `DEVKIT_TODO_SYNC_CLIENT_ID` (a UUID) and `DEVKIT_TODO_SYNC_SECRET` all resolve. Some of them without the rest makes `devkit todo sync` fail, naming the missing ones.

Each credential resolves from the environment first, then from Doppler when `[todo.taskchampion] doppler_project` (and optionally `doppler_config`) is set, then from `~/.config/devkit/secrets.toml`. The file takes these lines, added by hand:

```toml
devkit_todo_sync_url = "https://..."
devkit_todo_sync_client_id = "<uuid>"
devkit_todo_sync_secret = "<secret>"
```

`devkit doctor` shows where each one resolved from, never its value. The sync client goes through `HTTPS_PROXY`, or `HTTP_PROXY` for an `http://` server, and ignores `NO_PROXY` when either is set. With neither set, it uses `ALL_PROXY` and honours `NO_PROXY`. It trusts the platform certificate store, or `SSL_CERT_FILE` and `SSL_CERT_DIR` when set, without the bundled Mozilla roots devkit's other requests add.

When it syncs:

- After every write, in a background `devkit todo sync` process. The write is saved locally first, so a failed sync loses nothing, and no hook waits on the network. After a failure, background syncs hold off before the next attempt, for the time `devkit todo sync -h` states, so a server that is down costs an occasional attempt rather than one per write.
- At session start, before the lists are injected, so a new container sees where the last one stopped. `devkit todo list --sync` does the same. Each waits a bounded time, which `devkit todo list -h` states; past that, the sync carries on by itself and you get what this machine has.
- A session's end releases its claims locally and leaves the push to a background sync, so a harness that caps the hook's run cannot cut it short.
- `devkit todo sync` syncs now. It reports a failure and exits 0.

Other injections and plain `devkit todo list` read the local replica, which holds this machine's own writes.

Two machines claiming the same todo between syncs both succeed, and the later sync wins: the claim rule holds on one machine, sub-agents included, not across them.

The server keeps the lists encrypted, so a person reads them through a replica of their own. A separate taskwarrior profile holding the same URL, client id and secret keeps those tasks apart from your own:

```text
TASKRC=~/.taskrc-agents TASKDATA=~/.task-agents task sync
# ~/.taskrc-agents
sync.server.url=<url>
sync.server.client_id=<uuid>
sync.encryption_secret=<secret>
```

Then `task project:devkit.<repo>` shows the agents' lists. Pointing your everyday taskwarrior at the same server also works, but hands every container that holds the credentials read and write access to all of your tasks.

### Postgres

`[todo] backend = "postgres"` keeps the todos in a Postgres database that every machine's agents share. Choose it over taskchampion when many agents on many machines claim from the same lists: the database is the one authority, so a claim holds across machines, and of any number of agents claiming one todo at once exactly one gets it and the rest are refused naming the holder. Choose taskchampion when agents work alone or on one machine, when they must keep writing offline, or when you want to read the lists with taskwarrior.

Every hook and command talks to the database, so a machine without it reaches no lists: a hook that cannot reach it injects, attributes and releases nothing, and a command fails naming the database. A hook bounds each call to the database, and once one call fails to connect or gets no answer in time, the hook's later calls fail at once, so an unreachable or stalled database costs a hook one wait. A session's end gets a single budget for all of its database work, the release and the activity records together, so it finishes inside the short time harnesses give that hook. The activity log lives in the same database, so `devkit activity` on any machine reports the whole swarm.

The connection URL comes from `DEVKIT_TODO_DATABASE_URL`, resolved like the sync credentials: the environment first, then Doppler when `[todo.postgres] doppler_project` (and optionally `doppler_config`) is set, then `~/.config/devkit/secrets.toml`:

```toml
devkit_todo_database_url = "postgres://..."
```

Every hook resolves it, so a machine running many agents does better with the URL in its environment or the secrets file than in Doppler. `DEVKIT_TODO_BACKEND=postgres` chooses the backend without a config change. `devkit doctor` shows the backend, where the URL resolved from, and whether the database answers, never the URL itself.

devkit creates its tables in a `devkit` schema on first use, so the role in the URL needs to create a schema once; afterwards it only reads and writes those tables. Todos live under the `[todo] project` root, so several roots share one database without seeing each other's lists. Ids are uuids, shown and accepted as 8-character prefixes, as on taskchampion. The connection is encrypted whenever the server offers TLS, without verifying its certificate, as libpq does by default; `sslmode=disable` in the URL turns it off.

On Supabase, use the transaction pooler, which suits short-lived clients such as hooks. Copy its URL from the project's Connect panel, under Transaction pooler:

```text
postgres://postgres.<project-ref>:<password>@aws-0-<region>.pooler.supabase.com:6543/postgres
```

devkit uses nothing the transaction pooler lacks: no prepared statement outlives its transaction, and no session setting, `LISTEN` or advisory lock is used. The direct connection (`db.<project-ref>.supabase.co:5432`) works too, but each hook then holds one of the database's own connections while it runs. Any other transaction-mode pooler, such as PgBouncer, works the same way.
