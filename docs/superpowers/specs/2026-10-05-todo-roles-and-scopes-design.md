# Todo roles and scopes

Issue: #240

## Problem

devkit files every todo at one fixed layout: `global`, `<repo>`, `<repo>.<branch>` and `<repo>.<branch>.<harness>-<session>`, under the root `[todo] project` (`devkit` by default). Two things follow.

- alacritree's task tab reads the same layout as top-level projects and never matches anything under `devkit.`, so todos devkit writes never show there, even when both tools share one taskchampion replica. The root also collides with a repository named `devkit`: alacritree's `devkit.main` reads, under devkit's root, as a repository named `main`.
- Every agent writes to the one node its session names. A manager agent whose plan belongs to the whole worktree, and the worker sub-agents it spawns, all land on the same session list. A workflow cannot say "the manager's plan lives at the workspace, each worker keeps its own list", and a software factory or rig built later on devkit has no way to describe its roles.

A snapshot of the shared replica at the time of writing shows the gap. Nearly every agent-written task sits at a bare alacritree node, a large share of them at `<repo>.<branch>` from manager agents filing plan rows, and only a handful under devkit's root. Personal projects (`ideas`, `followup`) sit beside them with no root to fence them off.

## Behaviour

### Scopes

A **scope** is a named node template with a parent. The scopes form one tree, and a role writes to one scope.

```toml
[todo.scopes]
global    = { node = "global" }
repo      = { node = "{repo}",                                      parent = "global" }
workspace = { node = "{repo}.{branch}",                             parent = "repo" }
session   = { node = "{repo}.{branch}.{harness}-{session}",         parent = "workspace" }
agent     = { node = "{repo}.{branch}.{harness}-{session}.{agent}", parent = "session" }
```

These five are the defaults, and they write bare nodes alacritree reads. A layer may add scopes or redefine one; `[todo.scopes]` merges key by key across layers.

**Placeholders** are a fixed set:

| Placeholder | Value | Kind |
|---|---|---|
| `{repo}` | the repository name `node::place_of` gives | open |
| `{branch}` | the branch, or the worktree directory on a detached head | open |
| `{harness}` | `claude` or `codex` | closed |
| `{session}` | the harness's session id | open |
| `{agent}` | the sub-agent id, the `a` of holder `S/a` | open |

Every value is sanitized as now (`node::sanitize`: `.`, `/` and `\` become `-`), so a value never adds a level.

**Template grammar.** A template is segments joined by `.`. A segment is literal text and placeholders, non-empty, with no `.`, `{` or `}` in its literal text. Parsing the config rejects:

- an unknown placeholder;
- a placeholder used twice in one template;
- a segment holding more than one open placeholder (`{harness}-{session}` is fine: `{harness}` is closed, so the segment reads back unambiguously);
- a template made only of open placeholders that has no `{repo}` (it could never be anchored, below).

Each rule keeps a template readable back: given a project string, devkit can tell whether it came from a template and recover the placeholder values.

**The tree.** Parents must form one tree. Its root's template holds no placeholders, so it always fills. A cycle, a missing parent, or a second root is a parse error.

**Filling.** A caller fills its scope's template from its own facts. When a placeholder has no value (a main agent has no `{agent}`, a person at a terminal has no `{session}`, a directory outside any repository has no `{repo}`), the caller walks up `parent` until a scope fills. The root always does.

### Roles

```toml
[todo.roles.manager]
scope = "workspace"

[todo.roles.implementer]
scope = "agent"
parent = "manager"
agent_types = ["implementer"]
hold_pending = true
```

| Key | Meaning |
|---|---|
| `scope` | the scope the role writes to. Required. |
| `parent` | the role this one works under. Drives role suggestions and validation, never authority over claims. |
| `agent_types` | sub-agent types (`agent_type` in a SubagentStart payload) that take this role on spawn. |
| `hold_pending` | whether the stop hold counts pending todos on the role's node. Defaults below. |

Two roles are built in and always exist: `main`, scope `session`, and `subagent`, scope `session`. A layer may redefine either. A role naming a missing scope or role, a cycle through `parent`, or one `agent_type` claimed by two roles is a parse error.

**Which role a caller has**, first match wins:

1. A role recorded for its exact holder (`S` or `S/a`).
2. A sub-agent whose `agent_type` a role lists. `devkit hook subagent-start` records that role for the sub-agent's holder, so later calls match step 1.
3. Any other sub-agent: `subagent`.
4. Anyone else: `main`. A person at a terminal resolves to `main` and, with no `{session}`, fills up to `workspace`, which is where a person's todos land today.

A recorded role that no longer exists in config falls back to step 2 onward, with one warning on stderr. A todo command never fails over it.

**`devkit todo role [<name>]`.** With a name, records that role for the caller's own holder. There is no argument naming another holder. A name that is neither built in nor in `[todo.roles]` is an error listing the valid names. A caller with no session (a person at a terminal) is refused: there is nothing to record against. With no name, prints the caller's role and the node it resolves to.

**The record** is one JSON file in devkit's todo state directory, keyed by holder, each entry holding the role and when it was recorded, written under a file lock. Entries survive the session's end, so a resumed conversation keeps its role. Each write drops entries older than 30 days.

**The nudge.** The first time a caller with no recorded or `agent_types` role writes a todo (a native `TaskCreate` or `TodoWrite`, or a `devkit todo add` shell command), the pre-tool-use hook adds context naming the roles it could take: for a sub-agent, the roles whose `parent` is its session's role; for a main agent, the configured roles with no `parent`. The context names `devkit todo role <name>`. With no candidates there is no nudge. A holder is nudged once; the record of who was nudged sits beside the role record. Ignoring the nudge costs nothing: the caller keeps `subagent` or `main`.

### Writing

- `devkit todo add` writes to the caller's node, the one `devkit todo scope` prints.
- `--scope <name>` on `add` fills the named scope with the caller's own facts, walking up as in Filling. A worker files a finding for its manager with `--scope workspace`.
- `--node <node>` still writes to a raw node.
- Native `TaskCreate` and `TodoWrite` mirror to the caller's node.

### Reading

**Which tasks are devkit's.** A task is a devkit todo when its project reads back through a scope template. A node read through a template made only of open placeholders (`{repo}`, `{repo}.{branch}`) also needs its repository **anchored**: some task in the store reads back through a template with literal text or `{harness}` and has the same `{repo}`. `global` is literal and always counts. In practice a repository with at least one session list is anchored; a personal project like `ideas` has none and stays out. The fence applies where devkit scans the whole store: `list --all` and `list --subtree`. Every other path addresses exact nodes built from the caller's own facts.

**A caller's default listing**, deepest first:

- its own node;
- each ancestor scope's node, filled with its own facts;
- every node below its own node whose `{session}` is the caller's session: its own sub-agents' lists. A manager at `workspace` sees its workers' `agent` lists; a sibling session's lists stay out, as now.

`--scope <name>` on `list` lists the named scope's node alone. `--node`, `--subtree` and `--all` are unchanged.

**Claims** are unchanged: a holder covers its own claims and its sub-agents' (`Holder::covers`). Role `parent` grants nothing here.

### Stop hold

`devkit hook stop` and `devkit hook subagent-stop` hold an agent on its own claims anywhere, as now, plus the pending todos on its own node when its role's `hold_pending` is true.

`hold_pending` defaults to whether the role's node is the caller's own: true when the scope's template names the caller's deepest identity, `{agent}` for a sub-agent and `{session}` for a main agent. With the built-ins this keeps today's behaviour: `main` at `session` is held on its session's pending todos, and `subagent` at `session` is not. A worker at `agent` is held on its own list; a manager at `workspace` is held only on what it claimed, unless its role sets `hold_pending = true`. The rest of the hold (one reminder per unchanged list, what never blocks) is unchanged.

### Context

The session-start block and `devkit todo context` name the caller's role and node: "Role `manager`, writing to `monorepo.swe-123-fix`." The nudge, when due, is added in the same voice.

### Config changes

- `[todo] project` is removed. Parsing it is an error that names `[todo.scopes]` as the replacement.
- The Postgres backend used `[todo] project` as a tenant column, not a node prefix. It moves to `[todo.postgres] root`, default `devkit`, with the same meaning.
- `[todo.scopes]` and `[todo.roles]` merge key by key across layers and may live in a repository's `devkit.toml`, since they describe the workflow, not the machine.

Existing todos under `devkit.*` are not moved. They stop appearing in devkit's lists; `task project:devkit` still shows them.

## Design

### devkit-config

`TodoConfig` loses `project` and gains `scopes: BTreeMap<String, ScopeConfig>` and `roles: BTreeMap<String, RoleConfig>`, each defaulting to the tables above. `PostgresConfig` gains `root`. Validation (grammar, tree, role references, `agent_types` uniqueness) runs at parse time, so `devkit doctor` and every command report a broken layout the same way. The example `devkit.toml` and every parse error are doctests on the types.

### devkit-todo

`node` keeps `Place`, `SessionRef`, `sanitize`, `place_of` and `session_from_env`, and replaces `node`, `visible_nodes` and the four fixed levels with a `Layout` built from the config:

- `Template::parse` and `Template::read(project) -> Option<Values>`;
- `Layout::fill(scope, &Facts) -> String`, walking up on a missing value;
- `Layout::visible(&Facts, role) -> Filter`, the default listing;
- `Layout::is_devkit(project, anchored_repos)`, the fence.

`Facts` gathers repo, branch, harness, session and agent once per call. A `roles` module resolves a caller's role, reads and writes the record, and decides the nudge. `hold::open_for` takes the node and `hold_pending` from the resolved role instead of computing the session node itself. `Filter` gains a match for "below this node with this `{session}`".

### Backends

Taskchampion stores the filled node as the task's project with no root. The builtin and Postgres stores keep nodes as they do now; Postgres scopes its rows by `[todo.postgres] root`. The taskchampion hook queue's `Capture` and `Release` lose their `root`.

This design assumes the taskwarrior CLI backend is gone and taskchampion defaults its `data_dir` to `TASKDATA`, then the taskrc's `data.location`, so the bare default lands in the replica taskwarrior and alacritree read. That change is its own issue and lands first.

### CLI and hooks

- `todo/mod.rs` resolves `Facts` and the role once and passes the `Layout` down; `scope`, `add`, `list` and `context` stop calling `node::node` and `visible_nodes` directly.
- `todo role` is a new subcommand.
- `hook/todo.rs` resolves the caller's node through the same `Layout`.
- `hook subagent-start` records the `agent_types` role. It writes no stdout.
- `hook pre-tool-use` adds the nudge context.

## Testing

Failing test first, through the real entry points.

- **Config:** doctests for the default tables, each parse error listed above, `[todo] project` naming its replacement, and `[todo.postgres] root` defaulting to `devkit`.
- **Templates:** fill and read back each default template; walk-up on each missing fact; a sanitized value never adds a level.
- **CLI, on a taskchampion replica in a temp dir:**
  - with no config, a session's `devkit todo add` lands at a bare `<repo>.<branch>.<harness>-<id>`;
  - after `devkit todo role manager`, it lands at `<repo>.<branch>`;
  - a sub-agent's `devkit todo add --scope workspace` lands at the workspace node;
  - `list --all` lists an anchored repository's repo-level todo and leaves out an `ideas` task;
  - a manager's default listing shows its sub-agent's `agent` list and not a sibling session's;
  - `devkit todo role nope` lists the valid names; a person at a terminal is refused.
- **Hooks, through payloads:**
  - `subagent-start` with a listed `agent_type` records the role;
  - the nudge appears at a role-less sub-agent's first todo write and not at its second;
  - `stop` and `subagent-stop` hold per `hold_pending`, including the built-in defaults matching today's behaviour.
- **Contract suite:** unchanged, run against every backend.

## Evals

The session-start block and the nudge are text agents act on. A scenario under `evals/scenarios/` configures a manager and two worker roles, spawns a role-less sub-agent, and checks that it runs `devkit todo role` with a suggested name and files its todos on its `agent` node. It runs once before the PR.

## Proof for #240

- A session-scoped todo devkit added shows in alacritree's task tab under its repository (screenshot).
- `devkit schema` shows `[todo.scopes]` and `[todo.roles]` (command and output).

## Out of scope

- Moving existing `devkit.*` todos to bare nodes.
- Any change to alacritree. Its tab shows an `agent` node as a separate session section beside its parent, not nested; nesting belongs in alacritree's `tree::sections` as its own issue.
- Role authority over claims, and roles for people.
- Reading taskrc sync settings.
