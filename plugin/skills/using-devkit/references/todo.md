# Todo lists: `devkit todo`

devkit keeps agent todo lists in a store outside any checkout, so a resumed session, a later session and the person running the agents all read the same lists. At session start, after compaction, when a sub-agent starts, and on each prompt whose lists changed, a hook injects the lists you can see. Track any work of three or more steps there before you start it.

## Where your todos live

A list belongs to a node. Named scopes fill node templates with your identity. The default scopes are:

| Node | Holds |
|---|---|
| `global` | todos outside any repository |
| `<repo>` | the project's todos, shared by every worktree |
| `<repo>.<branch>` | the workspace's todos |
| `<repo>.<branch>.<harness>-<session>` | one agent session's todos |
| `<repo>.<branch>.<harness>-<session>.<agent>` | one sub-agent's todos |

`<repo>` is the main checkout's directory name, so every linked worktree shares it. `<branch>` is the checkout's branch, or its directory name on a detached head. A `.`, `/` or `\` inside a name becomes `-`. `devkit todo scope` prints your own node, and `devkit todo add` writes there unless told otherwise.

Your role picks the scope you write to. The built-in `main` and `subagent` roles share the session scope. A workflow can configure a manager at workspace scope and workers at agent scope. `devkit todo role` prints your role and node; `devkit todo role <name>` selects a configured role for your own holder. The selection survives resume, and records the checkout it was made in: your later todo commands and native task writes from outside any repository resolve against that checkout, not `global`. Without a recorded checkout, a todo added from outside any repository goes on `global`, with a notice on stderr. A sub-agent type listed by a role takes it at spawn. With no selection or matching type, the first todo write suggests available roles once. Choose with the command it names, or keep the built-in role.

Scopes and roles come from `[todo.scopes]` and `[todo.roles]`, merged by name across config layers. `devkit schema` describes their templates and validation. When a scope needs an identity you lack, it walks up its configured parent until a node fills. Use `devkit todo add --scope workspace` to file a worker's finding for its manager; `devkit todo list --scope workspace` reads that scope alone. Raw `--node` paths still address the exact node you name.

Your default listing shows your own node, its configured ancestors and descendant nodes belonging to your session. A manager sees its workers; a sibling session's lists stay out. Store-wide `--all` and `--subtree` scans only show nodes that read back through a scope template. Open repo/workspace templates also need an anchor: a task with that repository identity at a template containing literal text or the harness. A repository's session task anchors its repo and workspace lists; a personal project such as `ideas` without an anchor stays out. Exact-node reads have no scan fence.

When other sessions on your workspace left pending todos, the injected context says how many and names the `devkit todo list --subtree` command that shows them. It does so at session start, and on each later prompt while your own list has nothing open. To continue one of them, claim it with `devkit todo start <id>`. It stays on its own node.

## Ids and claims

Ids are in parentheses in the injected lists and in `devkit todo list`. `devkit todo add` prints the new one.

`devkit todo start <id>` claims a todo for you. A todo another agent has in progress shows `in progress: <name>`, and starting, finishing or dropping it fails naming the holder: pick another todo. Starting it succeeds only when the holder's session has ended or gone silent, as below. A sub-agent claims a todo with `start` before working on it, so two sub-agents never work on the same one. A sub-agent may take over a todo its own session started, and its session starting that todo again leaves the sub-agent's claim in place. When an agent or sub-agent ends, the todos it still has in progress return to pending. A harness can reclaim a session without ending it, as Claude Code does with cloud sessions, and then its claims stay. So `devkit todo start` takes over a todo whose holder's session has ended, or has fired no hook for longer than the backstop, the time an idle session's file locks last: the old claim ends as `handed` and yours begins. A holder whose session fired a hook within that time keeps the todo, and so does a person. Nothing is released on silence alone, so a session idle on a long command loses a claim only when someone starts that todo. Takeover reads the activity log below, so on a taskchampion replica a claim made on another machine, whose sessions this machine's log never saw, is never taken over. In Claude Code and Codex, a sub-agent's `devkit todo` commands run in Bash act as that sub-agent. Run in any other shell, such as PowerShell (Codex's shell on Windows), they act as its session, so those sub-agents do not exclude each other.

## Stopping with open todos

In Claude Code and Codex, a sub-agent ending its turn while it has open todos is refused once: the stop hook sends it back with the list. A main agent's stop is refused the same way only when the hold mode is `always`. In Claude Code, a main agent that ends its turn while background work it started is still running (a background sub-agent, a `run_in_background` command, a Monitor) or a session wakeup is scheduled is not refused, so ending the turn to wait for that work is fine; its next stop with nothing in flight is refused as usual. Your own claims on any node count. Pending todos on your role's node count when `hold_pending` is true. By default they count when the effective scope names your deepest identity: session for a main agent, agent for a sub-agent. Built-in main agents count session pending todos; built-in sub-agents count only claims. A worker at agent scope counts its pending list; a manager at workspace scope counts only claims unless its role overrides the flag. Role parents affect suggestions, never claim authority. Finish each open todo, or cancel one that no longer applies (`devkit todo cancel <id>`, or delete it in your task tool).

Before you stop to ask the user something:

- With a clear recommendation, take it and say so in your final report.
- Without one, ask a sub-agent on a bigger model and take its answer.
- Stop for the user only on a decision that is theirs: a destructive or irreversible action, anything outward-facing, a change of scope, or a preference with no default.

Ending your turn again with the list unchanged goes through, so that is how you stop for such a decision. A change to the list, a new prompt or a compaction re-arms the reminder.

The hold mode decides whose stop is held: `always` holds every agent, `subagents` (the default) only sub-agents, and `never` none. `devkit todo hold <always|subagents|never>` sets it for your session and its sub-agents until the session ends, and `devkit todo hold --clear` drops that setting; with no argument it prints the mode in effect and where it came from. Run it when your user asks to silence the reminders (`never`), or to be held to your todos while they step away (`always`). Without a session's setting, `DEVKIT_TODO_HOLD_STOP` decides, then `[todo] hold_stop`; both take the same spellings, and both read `true` as `always` and `false` as `never`. Unattended launchers, such as cloud sessions, set `always`.

## Native task and plan tools

When your harness gives you a task or plan tool (Claude Code's `TaskCreate` and `TaskUpdate`, Codex's `update_plan`), use it: devkit mirrors each call into your role's node, attributed to you. Without one, use `devkit todo add`, `start`, `done` and `cancel`. Edit a mirrored todo through the native tool that made it, so the two stay in step.

Codex offers `update_plan` only when its config sets `[tools.update_plan] enabled = true`. Codex also runs the plugin's hooks only after they are trusted, so approve them when it asks. Until then, nothing mirrors and no lists are injected.

## Dropping a todo

`devkit todo cancel <id>` drops a todo and keeps the record, marked cancelled, so the person running the agents can see what was abandoned. Cancelled todos are counted rather than listed.

`devkit todo purge` deletes a record for good, for text that must disappear such as a pasted secret. It needs a person at a terminal and refuses an agent; cancel instead.

## Activity

devkit records each subagent run and each stretch a todo spends in progress, whether or not the harness log is on: in the todo database on the postgres and supabase backends, and beside the todo state on every other. `devkit activity` reports them over a date range, grouped by session, agent type and todo, and `--json` gives the same report as JSON. `devkit activity -h` gives the range's defaults.

- A run starts at the subagent's start hook and ends at its stop. Without a stop it ends at its session's end. Without either, it ends as `lost` at the agent's last hook once the agent has been silent past a backstop, so no run stays open for good. Parallel runs, two of one type included, are told apart by agent id.
- Every hook a session fires marks it seen, its main agent's included. Its start is recorded as a `session_start` row, so a session appears in every range it overlaps, whenever its last hook came. The report shows each session as `ended` once its end arrives, `silent since <time>` once it has fired no hook for longer than the backstop, and `active` otherwise. A session the harness reclaimed without an end, such as a Claude Code cloud session, reads as silent, and so does one idle on a long command.
- A run whose payload names no agent type, such as a Claude Code fork, stores none and reports as `subagent`.
- Every stop is recorded, with its agent type when the payload names one. A stop with no recorded start makes no run. It is usually Claude Code's turn-end agent, which Claude Code runs after each turn without firing a start hook; a start whose record was dropped also leaves one. `devkit activity` reports no run for it, and counting runs from the raw `subagent_start` and `subagent_stop` rows means counting starts, not stops.
- An interval starts when a holder claims a todo and ends `completed`, `cancelled`, `released` (stopped, or released when its holder ended), or `handed` when another holder takes it over, a sub-agent from its session or anyone from an ended or silent one, whose own interval starts there.
- Only what devkit sees is recorded. A todo started with `task start` or in alacritree has no interval, and Cursor runs are not recorded because its manifest wires no subagent start.

## Backends

The built-in store is the default: one file in devkit's state directory. Agents use `devkit todo` or their native tool on every backend.

### Taskchampion

`[todo] backend = "taskchampion"` keeps the todos in a taskchampion replica that devkit embeds, so it needs no program beyond devkit. Choose it to share a replica with Taskwarrior, to keep a container's todos after it stops, or to watch a session's lists from another machine. Taskwarrior 3.5 or later can read and edit the same tasks:

- A todo's id is its task's uuid, shown as the first 8 characters. Any prefix of 8 or more that names one task works wherever an id does.
- Todos use bare project nodes that alacritree's task tab reads: a global todo on `global`, and a session todo on `<repo>.<branch>.<harness>-<session>`. Store-wide scans apply the configured template fence described above. Existing prefixed todos are not moved; `task project:devkit` still reads them.
- A task started outside devkit, with `task start` or in alacritree, counts as held by a person when it has no `holder`, so no agent takes it over. A task that still carries an agent's `holder` stays that agent's claim, even after someone stops and restarts it with `task`.
- devkit records who holds a todo in a `holder` attribute. To see it in your own `task` reports, add these lines to your taskrc:

  ```text
  uda.holder.type=string
  uda.holder.label=Holder
  ```

The replica directory resolves from `[todo.taskchampion] data_dir`, then `TASKDATA`, then the taskrc's `data.location`, then `taskchampion` under devkit's todo state directory. The taskrc resolves from `TASKRC`, then `~/.taskrc`, then `$XDG_CONFIG_HOME/task/taskrc`, with `~/.config/task/taskrc` used when `XDG_CONFIG_HOME` is unset or empty. Includes are followed in order, later assignments win, and a leading `~` or `$NAME` in taskrc paths and values expands. A malformed taskrc or missing include warns and falls back to devkit's state directory. Explicit `data_dir` and `TASKDATA` bypass taskrc reading. `devkit doctor` shows the replica directory and its source.

`DEVKIT_TODO_BACKEND=taskchampion` chooses it without a config change, so a container sets it next to the sync credentials and keeps loading the project's `devkit.toml`. It accepts the same values as `[todo] backend` and wins over it. `devkit doctor` shows the backend in effect and where it came from.

Replicas discovered through `TASKDATA` or taskrc stay local, even when a devkit sync target is configured. Set `[todo.taskchampion] data_dir` explicitly to allow devkit to sync that replica, including all tasks it holds. Local writes and lists still work without that opt-in; `list --sync` reports the refusal and lists local todos. The default replica under devkit's state directory can sync without `data_dir`.

An eligible replica syncs to the first of these that applies, or stays local:

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

Then `task project:<repo>` shows the agents' lists. Pointing your everyday taskwarrior at the same server also works, but hands every container that holds the credentials read and write access to all of your tasks.

### Postgres

`[todo] backend = "postgres"` keeps the todos in a Postgres database that every machine's agents share. Choose it over taskchampion when many agents on many machines claim from the same lists: the database is the one authority, so a claim holds across machines, and of any number of agents claiming one todo at once exactly one gets it and the rest are refused naming the holder. Choose taskchampion when agents work alone or on one machine, when they must keep writing offline, or when you want to read the lists with taskwarrior.

Every hook and command talks to the database, so a machine without it reaches no lists: a hook that cannot reach it injects, attributes and releases nothing, and a command fails naming the database. A hook bounds each call to the database, and once one call fails to connect or gets no answer in time, the hook's later calls fail at once, so an unreachable or stalled database costs a hook one wait. A session's end gets a single budget for all of its database work, the release and the activity records together, so it finishes inside the short time harnesses give that hook. The activity log lives in the same database, so `devkit activity` on any machine reports the whole swarm.

The connection URL comes from `DEVKIT_TODO_DATABASE_URL`, resolved like the sync credentials: the environment first, then Doppler when `[todo.postgres] doppler_project` (and optionally `doppler_config`) is set, then `~/.config/devkit/secrets.toml`:

```toml
devkit_todo_database_url = "postgres://..."
```

A URL Doppler gives is kept in devkit's state directory, readable only by you, and hooks reuse it for a while instead of asking Doppler each time. Every `devkit todo` command and `devkit doctor` ask Doppler afresh and refresh the kept copy, and a hook that fails to connect with it, refused or rejected, drops it, so the next hook picks up a rotated credential or a moved database. A session's end never asks Doppler: it uses the kept URL however old, and with none kept and no URL in the environment or the secrets file it releases nothing. `DEVKIT_TODO_BACKEND=postgres` chooses the backend without a config change. `devkit doctor` shows the backend, where the URL resolved from, and whether the database answers, never the URL itself.

devkit creates its tables in a `devkit` schema on first use, with a function for each write, so a claim is atomic even from a client that sends each request as a transaction of its own. A database that already has the schema gains a function a newer devkit adds when a postgres call first needs it; `devkit todo schema update` creates every table and function the database lacks at once, and prints each one it created. Run it after upgrading devkit. The role in the URL needs to create a schema once, and to create in it again when a newer devkit adds a function; otherwise it only reads those tables and calls those functions. `[todo.postgres] root` separates tenants sharing one database. It defaults to `devkit` and does not prefix node names. Ids are uuids, shown and accepted as 8-character prefixes, as on taskchampion.

The connection always uses TLS and verifies the server's certificate: against the Mozilla roots bundled into devkit, the platform's certificate store, and the PEM file `[todo.postgres] ca_file` names, if any. A server that offers no TLS, or a certificate none of them vouches for, fails the connection rather than falling back to plaintext, whatever `sslmode` the URL gives, `prefer` included. Only `sslmode=disable` in the URL connects in plaintext, for a database on the same machine or network.

On Supabase, use the transaction pooler, which suits short-lived clients such as hooks. Copy its URL from the project's Connect panel, under Transaction pooler:

```text
postgres://postgres.<project-ref>:<password>@aws-0-<region>.pooler.supabase.com:6543/postgres
```

Supabase signs its database certificates with its own CA. Download it from the project's database settings, under SSL Configuration, and name the file:

```toml
[todo.postgres]
ca_file = "~/.config/devkit/supabase-ca.crt"
```

Set `ca_file` in `~/.config/devkit/config.toml`. A project's `devkit.toml` cannot name one: devkit ignores it there, so a repository you clone cannot add a CA your connection trusts.

devkit uses nothing the transaction pooler lacks: no prepared statement or advisory lock outlives its transaction, and no session setting or `LISTEN` is used. The direct connection (`db.<project-ref>.supabase.co:5432`) works too, but each hook then holds one of the database's own connections while it runs. Any other transaction-mode pooler, such as PgBouncer, works the same way.

### Supabase

`[todo] backend = "supabase"` reaches the postgres backend's todos over a Supabase project's HTTPS Data API, for a machine that can make HTTP requests but cannot open a Postgres connection, such as a Claude Code cloud session, whose sandbox lets only HTTP(S) out through a proxy. Each write calls the database function the postgres backend calls, so a claim is atomic across machines on either backend, and a todo another agent holds is refused naming the holder. Reads select from the tables. Hooks bound each request as they bound each database call, and an API that gets no answer costs a hook one wait. Activity records go to the same database through its functions, so `devkit activity` on either backend reports machines on both, and a session's end fits its release and records into the one budget the postgres backend gives it.

The API URL is the project's, `https://<project-ref>.supabase.co`. It comes from `DEVKIT_TODO_SUPABASE_URL`, else `[todo.supabase] url` in `~/.config/devkit/config.toml`. A project's `devkit.toml` cannot name it: devkit ignores it there, so a repository you clone cannot send your key elsewhere. `[todo.supabase] root` names the tenant, as `[todo.postgres] root` does; set the same root on every machine that shares the lists. `DEVKIT_TODO_BACKEND=supabase` chooses the backend without a config change.

The key comes from `DEVKIT_TODO_SUPABASE_KEY`, resolved and kept like the postgres backend's URL: the environment, then Doppler when `[todo.supabase] doppler_project` is set, then `devkit_todo_supabase_key` in `~/.config/devkit/secrets.toml`. A kept key the API answers 401 to is dropped. devkit sends the key as both `apikey` and `Authorization: Bearer`, so a key in devkit works only where the gateway accepts the role's JWT as `apikey`, such as self-hosted PostgREST or a gateway configured that way. A hosted Supabase project needs the role's JWT as the bearer token and a separate project key as `apikey`, two values devkit cannot send; there, configure no key in devkit and use a credential proxy. The key is optional: with none, requests carry neither header, so a credential proxy that attaches them, such as a claude.ai cloud environment's API credentials for the project's host, keeps the key out of the sandbox.

Requests go through `HTTPS_PROXY` when it is set, and honour `NO_PROXY`. They trust the Mozilla roots bundled into devkit and the platform's certificate store, or `SSL_CERT_FILE` and `SSL_CERT_DIR` when set, which is where a cloud session's proxy CA is found.

`devkit doctor` shows the backend, the API URL and where it resolved from, where the key resolved from but never the key, and whether the API answers.

Set up the project once:

1. Create devkit's schema from a machine that can connect to the database, on the postgres backend: `DEVKIT_TODO_BACKEND=postgres devkit todo schema update`, with `DEVKIT_TODO_DATABASE_URL` naming the project's database. Run it again after upgrading devkit, which creates any function the newer version adds; until then, the API answers a call to that function with an error naming this step. On the supabase backend the command is refused, since the API cannot change the schema.
2. In the SQL editor, create a role that reaches devkit's schema and nothing else. The functions run with the caller's rights, so the role writes the todo and activity tables itself. Run it as the role that created the schema, so the default privilege covers the functions a later devkit adds:

   ```sql
   CREATE ROLE devkit_agent NOLOGIN;
   GRANT devkit_agent TO authenticator;
   GRANT USAGE ON SCHEMA devkit TO devkit_agent;
   GRANT SELECT, INSERT, UPDATE, DELETE ON devkit.todos, devkit.seen TO devkit_agent;
   GRANT SELECT, INSERT ON devkit.activity TO devkit_agent;
   GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA devkit TO devkit_agent;
   ALTER DEFAULT PRIVILEGES IN SCHEMA devkit GRANT EXECUTE ON FUNCTIONS TO devkit_agent;
   ```

   `anon` and `authenticated` get no usage on the schema, so the project's public keys reach nothing in it.
3. Expose the schema to the Data API: under the project's Data API settings, add `devkit` to the exposed schemas. On a self-hosted project, add it to PostgREST's `db-schemas`.
4. Requests run as the role their bearer token names. Sign a JWT whose `role` claim is `devkit_agent` with the project's JWT secret. On a hosted project, configure no key in devkit and have the credential proxy send the JWT as the bearer token, with a key the project's gateway accepts as `apikey`. Where the gateway accepts the JWT as `apikey` too, the JWT can be devkit's own key instead.

The API answers a schema it does not serve, a function or table it cannot find, and a permission the role lacks with an error that names the step to revisit. `devkit todo schema update` tells the API to reload its schema cache, so the API finds the functions it created at once. After creating a function by hand, `NOTIFY pgrst, 'reload schema'` does the same.
