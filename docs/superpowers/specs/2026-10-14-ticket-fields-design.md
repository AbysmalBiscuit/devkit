# Ticket fields

## Problem

Tickets agents file come out missing metadata: no labels, no priority, no estimate, no project. `/to-tickets` tells an agent to fill every relevant field, but it is a skill a person runs, and only that run follows it. A ticket filed mid-task with `ticket create`, through a tracker MCP, or by a workflow stage skips it.

Filling fields is also expensive where it does happen. devkit's `ticket create` writes only a title and a body, and only on GitHub. The skill runs its own discovery script for valid values, then three to five extra calls per ticket: labels, assignee and milestone on the issue, then adding it to a project and setting Priority, Size and the rest on its project item. Linear tickets go through the Linear MCP's `save_issue`, which devkit can gate but not fill.

## Goal

Each repository declares, per tracker, which ticket fields exist, their types and values, and how strongly each must be filled. devkit then:

- writes every declared field itself, wherever the tracker keeps it, from `ticket create` and `ticket edit` on GitHub and on Linear;
- checks the same fields on tracker MCP writes, since some agents prefer the MCP;
- tells an agent what a ticket needs when it asks.

This is the second of three pieces. The first, tracker ticket sync (`2026-10-14-tracker-ticket-sync-design.md`), is one-way. The third, two-way sync of body and metadata, writes through this design's field declarations.

## Non-goals

- Conditions that look at anything beyond the ticket's other fields and who is filing it. Richer policy belongs in scripts and skills users write.
- Creating labels, milestones, options or any other metadata in the tracker.
- Tickets filed in a tracker's web UI.
- Two-way sync (piece 3).
- Changing `/to-tickets`. It lives outside this repository and can move to `ticket fields` and `ticket create` once they ship.
- Ticket status. `[ticket.events]` owns status moves, and no field declares it.
- Several fields sharing one place, such as priority and area both kept in GitHub labels. Each place belongs to at most one field.

## Anti-goals

Each of these is a failure even if the feature otherwise works:

- **A mismap.** A value written to the wrong field or the wrong option.
- **A silent miss.** A write that succeeds while a field it was given stays unset. Every write is read back.
- **A guess.** A stale or unknown id or value turned into a near match. It is an error naming the field and the value.
- **An unsatisfiable `error`.** A field at `error` that no path an agent is allowed to take can set.
- **A duplicate.** A create that half-failed, retried into a second ticket.

## Design

### Declarations

Fields live in `devkit.toml` under `[ticket.fields.<tracker>.<field>]`, where `<tracker>` is `github` or `linear`.

A declaration holds:

- `type`: `string`, `number`, `date`, `enum`, `user` or `issue`, and whether it takes a list (labels, blocked-by).
- `where`: the place on the tracker the value is written. The set of places is fixed per tracker in code, because each needs its own write.
  - **GitHub:** the issue's labels, assignees, milestone, type, parent and blocked-by; membership of `[github] project`; and a field of that project's items, named by field id. The project number lives in `[github] project` alone.
  - **Linear:** the `issueCreate`/`issueUpdate` inputs (team, labels, assignee, priority, estimate, cycle, project, project milestone, due date, parent) and blocked-by relations.
- `values`, for `enum`: an array of `{ name, id }`. The name is what agents pass and the id is what devkit writes.
- `level` and `when`: see Levels.
- `default`: the value `ticket create` writes when none is given. See Writing fields.
- `description`: shown to agents with the field, as `[templates.variables]` descriptions are.
- `allow_none`: whether an agent may mark the field deliberately empty (see Explicit none). True when unset. A Linear team field is never allowed none.

An illustration only. Key names and layout are the builder's:

```toml
[ticket.fields.github.priority]
type        = "enum"
where       = { project_field = "PVTSSF_..." }
values      = [{ name = "P0", id = "f75ad846" }, { name = "P1", id = "47fc9ee4" }, { name = "P2", id = "98236657" }]
default     = "P2"
level       = "error"
description = "P0 drop everything, P1 this cycle, P2 backlog"

[ticket.fields.github.milestone]
type   = "enum"
where  = "milestone"
values = [{ name = "v1", id = "v1" }]
level  = "none"
when   = [{ if = { project = "*" }, level = "warning" }]
```

Values are listed in the toml so that checking and writing a field costs no lookup. `ticket fields discover` writes them (below).

**Load-time errors.** Each is a config error at load, naming the field:

- A declaration whose `where` is the status (`[github] status_field` on GitHub, the workflow state on Linear). The error points at `[ticket.events]`.
- An `enum` that can reach `error` (by `level` or any `when` entry) with no `values`, since no value could ever satisfy it.
- A merged declaration lacking `type` or `where`. The error names the layer that set its `level`.
- Two declarations with the same `where`.

**Layering.** Only the table for the tracker the repository uses applies. A global config can declare `level`, `when`, `default` and `description` for a field across every repository on a tracker, and a repository's `devkit.toml` overrides them key by key. `type`, `where` and `values` are facts about one destination and normally live only in the repository's file. `values` is an array, so a higher layer replaces it whole and a stale id cannot survive from a lower one. *(My call.)*

**Linear team scope.** A Linear label or cycle belongs to a team. A value may record the teams it is valid in, and a value used with a ticket of another team is refused, never swapped for a same-named one. A Linear ticket needs a team, so `ticket create` on Linear with no team field declared, or none given and no default, is refused and names the missing declaration. *(My call.)*

### Passing values: `--arg`

Field values come in through the `--arg name=value` flag that `ticket create`, `ticket edit` and `ticket render` already take for template variables, whose names `parse_args` already checks against the declared set. Fields and template variables share one namespace. Each of these is a config error at load:

- a name declared both as a field and as a template variable;
- a field named after one of the keys the issue render contexts supply, which are `ISSUE_CONTEXT_KEYS` in `src/bin/devkit/issue/render.rs`.

Field values are visible to the `ticket_body` and title templates, so a body or a local-file ticket can show them without a second declaration. Fields are left out of the derived required rule in `required.rs`. A declared field that a template reads but the caller did not give renders as the empty string, and only the field's level decides whether the command is refused. *(My call.)*

A list takes comma-separated values (`--arg labels=area:todo,enhancement`). A `user` takes a `[people]` alias. An empty value (`--arg milestone=`) is refused, and the refusal points at `--none`.

### Explicit none

An agent marks a field deliberately empty with `--none <field>` on `ticket create` and `ticket edit`, repeatable. It is a flag of its own, not a value, so it cannot collide with an enum value or a label named `none`.

- **On a create**, a field marked none counts as filled: it satisfies `error`, silences `warning`, and nothing is written for it.
- **On an edit**, `--none` is how a field is cleared. A field whose level for the resulting ticket is `error` can be cleared only this way and only when it allows none.
- **A field with `allow_none = false`** refuses `--none`, so at `error` it needs a real value.
- **Nothing is stored.** Trackers have no place to record "deliberately empty", so after the write the field reads as unset like any other. The mark matters only to the check at the moment of writing.
- **Templates** read a field marked none as the empty string.
- **Messages** about a missing `error` field name `--none <field>` beside the `--arg` when the field allows none.

On an MCP write, a mapped key present with an explicit `null` is the same mark. A tool whose input schema refuses `null` cannot express it, and its agent uses `ticket create` or `ticket edit`. *(My call.)*

### People

A `user` value is a `[people]` alias. `Person` gains a `linear` user id beside `github`, and `slack` becomes optional so a person with only tracker accounts can be declared. `--arg assignee=lev` writes `github` on GitHub and `linear` on Linear. A person with no identity on the tracker in use is an error naming the alias and the missing key.

### Levels

A field's level is one of `error`, `warning`, `info` or `none`. These are the existing `devkit_config::Severity` (`info`, `warning`, `error`, in `crates/devkit-config/src/harness.rs`) plus `none` below it. A level says whether a missing field is mentioned and whether it blocks. It never lets a wrong value through.

| Level | `ticket create` | `ticket edit` | Tracker MCP create | `ticket fields` |
|---|---|---|---|---|
| `error` | refused before any write, naming every missing field | a clear is refused unless the field allows none | denied, naming every missing field | listed |
| `warning` | written; each missing field reported after the write | a clear is reported after the write | allowed; each missing field reported with the allow | listed |
| `info` | written; nothing said | nothing said | allowed; nothing said | listed |
| `none` | written if given; nothing said | nothing said | allowed; nothing said | listed only if a `when` entry can raise it |

On a create, "missing" means no value given, no explicit none, and, on `ticket create`, no default. An edit names only the fields it changes, so on an edit nothing is missing: the only thing a level can refuse there is a clear.

A message about a missing field or a refused clear gives the field's name, its description, its valid values, and the `--arg` that sets it. The wording is the same on every path, and it reuses `required.rs`'s hint style, which already names a missing `--arg` and its description.

**Conditional levels.** `when` is a list of `{ if, level }` entries. The first whose conditions all hold sets the level, and `level` is the fallback. A condition tests:

- another field on the same ticket: set (`"*"`), unset (`""`), or one of several values;
- who is filing: `agent` or `human`, from `devkit_common::caller`.

Nothing else. A condition naming an undeclared field is a config error.

**Which value a condition sees** depends on the path:

- **`ticket create`:** the values given, plus defaults.
- **`ticket edit`:** the ticket's current fields, read before anything is written, with the `--arg`s given laid over them.
- **MCP create:** the call's input.
- **MCP update:** see The MCP check.

**Invalid values** are refused at every level, `none` included. That covers an `enum` value not in `values`, a `user` alias not in `[people]`, a `number` or `date` that does not parse, and an `issue` reference the tracker's `issue_ref` rejects.

### `ticket fields`

Prints each declared field of the tracker in use: name, type, level, `when` entries, values, default and description. The level shown is evaluated for this caller and for any `--arg` values given, so an agent can see what a particular ticket needs. A field is left out only when its `level` and every `when` entry are `none`. `--json` for scripts. It reads config only and makes no tracker request.

### `ticket fields discover`

Reads the tracker once and writes the `[ticket.fields.<tracker>]` declarations into the repository's `devkit.toml`:

- **Run by a human** it prints a diff of the change and asks for confirmation. `-y`/`--yes` skips the prompt.
- **Run by an agent** it writes without asking.
- Either way the change shows in `git diff` before anyone commits it.

It reads the merged config first, so it knows what every layer already declares. What it touches:

- **Refreshes** `type`, `where` and `values` for every field any layer declares, and writes nothing else for those fields. Values new in the tracker are added; values gone from it are dropped and reported.
- **Keeps** `level`, `when`, `default` and `description` wherever the person set them.
- **Adds** a field no layer declares with `level = "info"`, so nothing starts refusing tickets unannounced. A field a person does not want is set to `level = "none"`, not deleted, because a deleted one comes back on the next run. *(My call.)*
- **Skips** the status field: `[github] status_field` and Linear's workflow state.
- **Preserves** comments and layout, editing the file in place (`toml_edit`).
- **Reports, does not write,** tracker users it cannot match to a `[people]` alias, and the identity keys to add. `[people]` usually lives in global config.

What it reads:

- **GitHub:** the issues repository's labels, milestones, assignable users and issue types, and the fields and options of `[github] project`.
- **Linear:** the teams the authenticated user belongs to, unless `--team` names them, with their labels, cycles, projects, project milestones, priority scale and estimate settings, and the workspace's users.

A stored id that has gone stale shows up as a rejected write. devkit reports the field and value and says to re-run `discover`. It never looks the value up again and retries on its own.

### Writing fields: `ticket create` and `ticket edit`

Both commands work on GitHub and on Linear. On Linear, devkit writes through the GraphQL API with `LINEAR_API_KEY`, as `[ticket.events]` status moves already do (`crates/devkit-common/src/tracker/linear_status.rs`). On GitHub, project membership and project item fields use the mutations `crates/devkit-common/src/tracker/github_status.rs` already sends. The title and body still come from the templates, unchanged.

The order of a create:

1. Resolve every `--arg`, apply defaults, evaluate levels, and validate values. A missing `error` field or an invalid value stops here, and nothing is written.
2. Create the ticket with every field the create call itself accepts.
3. Set the rest: GitHub project membership and project item fields, blocked-by and parent relations, Linear relations.
4. Read the ticket back and compare every field written.
5. Print the ticket URL, then report missing `warning`-level fields.

**Failure after the ticket exists.** If step 3 or 4 fails, the URL is still printed, along with each field that did not take, and the command exits non-zero. The fix is `ticket edit` on that ticket, since a retried `ticket create` would file a duplicate.

**Failure that may or may not have created it.** If step 2 fails without a definite "not created", such as a timeout or a dropped connection, devkit can't tell whether a ticket exists:

- **On Linear**, devkit generates the issue's id before creating it and passes it to `issueCreate`. A retry of the same create then collides on that id rather than filing a second ticket. Whether Linear's input still accepts a client-supplied id is for the builder to verify.
- **On GitHub**, there is no such key. The error says the create may have gone through, and that the tracker must be searched for the title before retrying.

`ticket edit`:

- **Reads first.** It reads the ticket before step 1, and evaluates conditions against the current fields with the given `--arg`s laid over them.
- **Field-only edits.** With only field `--arg`s and no `--body` or `--title`, it writes those fields and leaves the title and body alone. Today `--body` is required. *(My call.)*
- **Exactly what is named.** It writes only the fields named and never applies a `default`.
- **Clears.** A field is cleared with `--none`. A clear of a field whose level for the resulting ticket is `error` and that does not allow none is refused.
- **Lists replace.** A list field is set to exactly the values given, and the read-back compares the whole set. An add or remove syntax is the builder's option. *(My call.)*

### The MCP check

The pre-tool-use MCP path (`src/bin/devkit/hook/mcp.rs`) already matches tracker MCP writes against `[harness.issue_tools]` and requires a matching `ticket render` receipt for the title and body. Each entry gains:

- a `tracker` key naming the tracker it writes to;
- a map from field names to the tool's input keys, for example `labels = "labels"` or `assignee = "assignee"`.

**When the check runs.** The hook never resolves or detects a tracker, since detection reads secrets and spawns git. The entry's `tracker` key selects `[ticket.fields.<tracker>]`. The field check runs when all three of these hold:

- that table is declared;
- `[tracker] kind`, if set, agrees with it;
- on GitHub, `[github] issues_repo`, if set, matches the call's `owner`/`repo`.

Any other matched call gets the receipt check alone. On a match the hook loads the full config, for `[ticket.fields]` and `[people]`, and a config error denies with that error. *(My call.)*

After the receipt check passes, a matched call is checked like this:

- **Create:** every field whose level is `error` must be present under its mapped key. A field with no mapped key can't be set by that tool, so it always counts as missing; an `error` on a GitHub project field therefore always denies `issue_write`, and the denial points at `ticket create`. A `default` does not count here, because devkit writes nothing on this path, so the denial names the default as the value to pass.
- **Update:** the call is a partial write, so only the fields it carries are checked, for validity and for clears. A carried `null` is a clear, judged as `--none` is on `ticket edit`. The hook can't fetch the ticket. When the level of a field being cleared depends on a `when` condition that names a field the update doesn't carry, the clear is judged at the highest level any entry could give. *(My call.)*
- **Values:** an MCP value is tracker-native. It matches a declared value by name (case-insensitively) or by id, and a `user` matches by alias, by tracker login or by id. Anything else is refused. A team-scoped Linear value is accepted if it is valid in any team, because the tracker resolves a name within the ticket's own team and an id is unique across the workspace. *(My call.)*
- **Warning:** a missing `warning`-level field allows the call and attaches the report to the allow. When the harness has no way to attach context to an allow, the report is dropped.

The check reads config and the payload only, so a hook verdict never waits on the tracker. Once a rule matches, any failure denies, as the receipt check already does.

## Global constraints

- Hook rules from `AGENTS.md`: no `hook` verb exits 2, only `pre-tool-use`, `stop` and `subagent-stop` write stdout, and logging never changes a verdict.
- A hook verdict needs no network.
- Each fact has one home. A field key's meaning is the doc comment on its config type, with a `devkit.toml` example as a doctest, and `schema/devkit-config.json` is regenerated. Behavior an agent needs goes in `plugin/skills/using-devkit/references/ticket.md`.
- Help text stays ASCII.
- Repository-scoped `gh` calls go through `cmd::gh_json_in` / `cmd::gh_capture`. Tokens resolve through `devkit_common::secrets`.

## Left to the builder

- Config key names and layout within the shapes above, including how `where` names a place.
- How list values are written in `--arg` beyond the comma rule, and how a value containing a comma is escaped.
- The `ticket fields` text layout.
- Whether `ticket render` evaluates levels too, for agents that render and then call the MCP.
- The order of writes inside step 3 on each tracker.
