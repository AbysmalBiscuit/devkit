# `ticket`: write tracker tickets and move their status

`ticket` (`devkit ticket`) renders, creates and edits the tracker tickets a workspace works on, moves their status, and draws the dashboard. It acts on the **current working directory's worktree** where a verb needs one (`-C/--dir <path>` overrides). `-C` and `--config` go on `ticket` itself, before the verb, or on the verb.

```sh
ticket render --title T [--body B] [--arg k=v] [--arg-file k=path]
ticket create --title T [--body B] [--arg k=v] [--arg-file k=path] [--none FIELD]
ticket edit <number|ID|URL> [--body B] [--title T] [--arg k=v] [--arg-file k=path] [--none FIELD]
ticket fields [--arg k=v] [--json]
ticket event <setup|start|pr_open|todo_start|todo_done|todo_cancel> [ID|URL]
ticket dashboard [--chart bar|line] [--bucket B] [--mode M] [--aggregate cumulative|period] [--all-roles] [--author gh] [--no-plots] [--no-cache]
```

`ticket pr <verb>` runs `devkit pr <verb>` (`references/pr.md`).

## `render`, `create` and `edit`: write a ticket

Ticket titles and bodies come from the `ticket_title` and `ticket_body` templates; `issue_title` and `issue_body`, their old names, still load. `{{ input }}` is `--title` or `--body`, and the body template also sees the rendered title as `ticket_title` and as `issue_title`. A `[templates.variables]` entry either template reads and marks `required` must be passed as `--arg`. A missing one is refused by name, with its description.

- **GitHub:** `ticket create` renders and runs `gh issue create` against `issues_repo`, then prints the issue URL.
- **Linear:** `ticket create` renders and files the ticket through Linear's API with `LINEAR_API_KEY`, then prints its URL. The ticket's team is a field: declare one with `where = "team"`, and pass it as `--arg` or give it a `default`, or the create is refused before any request.
- **GitHub, an existing issue:** `ticket edit <number|URL> --body B` renders the body and runs `gh issue edit` to replace it. `--title T` also renders and replaces the title; without it the issue keeps its title, which the body template sees as `ticket_title`. Field `--arg`s and `--none` write those fields (see Ticket fields), and an edit with only those leaves the title and body alone. An edit with no field `--arg` or `--none` reads the issue's fields only when the templates it renders read one, so it needs GraphQL and the `project` scope only then.
- **Linear, an existing ticket:** `ticket edit <ID|URL>` (`ENG-42`) works the same way through Linear's API, always reading the ticket first.
- **A tracker MCP (Linear's `save_issue`, GitHub's `issue_write`):** run `ticket render`, which prints `{"title": ..., "body": ...}`, and pass both strings unchanged as the MCP call's title and body fields. Line endings and trailing whitespace may differ, but any other edit is denied as text that differs from the render.
- **Through a tracker MCP, rewriting an existing ticket's title or body** goes through `ticket render` too. An update that only changes state, labels or assignee needs no render. Linear's `patch` edits the body in place, so it is always refused: render the whole new body and pass it as `description`.

A denied MCP call names the render command and the `--arg`s it needs. The render has to happen in the same agent session as the call, because its receipt is filed under the session id. Receipts live under `.devkit/issue-receipts/` in the repository's main worktree, so a render in any worktree of the repository counts, and session end deletes that session's receipts.

Enforcement is configuration. `[harness.issue_tools.<name>]` entries name the MCP servers and tools that write tickets and enforce as soon as they exist. `devkit schema` documents their keys. Refusing a bare `gh issue create` takes a `[harness.commands]` rule with `programs = ["gh"]` and `args = ["issue", "create"]`, and a bare `gh issue edit --body` one with `args = ["issue", "edit", "**", "--body*"]`; like every command rule they need `[harness] enforce_commands = true`. `ticket create` and `ticket edit` run their own `gh`, which the rules never see.

## Ticket fields

A repository declares the fields its tickets carry under `[ticket.fields.<tracker>.<field>]`, `github` or `linear`: each field's type, the place on the tracker it is written, its values, and its level. `devkit schema` documents the keys. A field is passed like a template variable, `--arg <field>=<value>`, and fields and `[templates.variables]` share that one namespace. A list field takes comma-separated values (`--arg labels=bug,enhancement`); spaces around each value and repeats are dropped. An enum value is matched by name, ignoring case. A `user` value is a `[people]` alias, which needs the tracker's identity key (`github` or `linear`) under its `[people]` entry.

Run `ticket fields` before filing a ticket. It lists each field that can matter, with its type, its values, its default, its description, and its level for you and for any `--arg` values you pass to it (`ticket fields --arg project=devkit` shows what a ticket in that project needs). It reads config only. `--json` prints the same list for scripts.

| Level | `ticket create` without the field | `ticket edit` clearing it |
|---|---|---|
| `error` | refused before anything is written, naming every missing field | refused unless the field allows `--none` |
| `warning` | filed, then each missing field is reported | done, then reported |
| `info`, `none` | filed, nothing said | done, nothing said |

A field's `when` entries can raise or lower its level depending on another field of the same ticket or on who is filing (`caller = "agent"`). A value that is not one the field takes is refused at every level, `none` included; the refusal lists the valid values. The body and title templates see each field as text: its values' names joined by `, `, or the empty string when it has none. A field the templates read is never required by the template rule; only its level decides.

`ticket create` writes every field it is given, and every default for a field it is not given, on the issue (and on GitHub on its item in `[github] project`), then reads the issue back and compares each field. It prints the issue URL first. When a field did not take (a write the tracker refused, a token without the `project` scope, a read-back that differs), it names each field after the URL and exits non-zero: the issue exists, so fix it with `ticket edit <number|ID> --arg <field>=...`. On GitHub never rerun `ticket create`, which would file a second issue; when `gh issue create` itself fails, the issue may exist anyway, so search for its title before retrying. On Linear a create passes an id derived from its team, title and body, so rerunning the same `ticket create` after a timeout or a failure returns the ticket the first run filed, with a note that it already existed. Two deliberate tickets with the same team, title and body are therefore one ticket.

On Linear a value restricted to some teams (a label or cycle whose declared value lists `teams`) is refused, before any request, for a ticket of another team; it is never swapped for a same-named value of that team. Priority and estimate take whole numbers.

`ticket edit` reads the issue first, judges `when` conditions on its current fields with the given ones laid over them, and writes only the fields named, never a default. A list field is set to exactly the values given: `--arg labels=a` on an issue labelled `a, b` removes `b`. It reads the issue back the same way. Moving a Linear ticket to another team can make Linear drop values bound to the old team, such as labels, cycle or project; the edit then warns about each declared field the move changed, naming the value it held, and still succeeds. Set the ones the new team takes again with `ticket edit`.

A tracker MCP write is held to the same levels once its `[harness.issue_tools]` entry names a `tracker` and maps field names to the tool's input keys under `fields`, unless `[tracker] kind` names another tracker or, on GitHub, the call's `owner`/`repo` is not `[github] issues_repo`. After its render check passes, a create missing an `error` field is denied naming the field, its values and the input key that sets it; a field the entry maps to no key cannot be set by that tool, so at `error` every create through it is denied, and `ticket create` is the way to file it. A `default` is not applied on this path; the denial names it as the value to pass. A missing `warning` field allows the call with a report. An update is judged only on the fields it carries. A key set to `null` marks its field deliberately empty, as `--none` does. Values are matched as the tracker names them: an enum value by name or id, a user by `[people]` alias or tracker login, a list as an array of strings, a `number` field as a JSON number or a string. A value of any other JSON type is refused, never converted. The check reads config and the call alone, and a `devkit.toml` that does not load denies the call.

`--none <field>` marks a field deliberately empty. On a create it counts as filled and nothing is written; on an edit it clears the field. A field with `allow_none = false` refuses `--none`, so at `error` it needs a real value; a Linear team field never allows it. An empty value, `--arg milestone=`, is refused and points at `--none` when the field allows it.

## Status events

`[ticket.events]` moves the ticket's tracker status when devkit acts on it: a Linear workflow state, or on GitHub the single-select field of the Projects v2 project `[github] project` names. Each event moves the ticket only from a status its `from` lists, and a ticket already at `to` is left alone. `[issue.events]`, its old name, still loads. With neither, devkit reads and writes no tracker status. `devkit schema` documents the keys.

| Event | Fires | Where |
|---|---|---|
| `setup` | as `workspace setup`'s last step, after the `after_worktree_create` hooks | the worktree it creates, when it names a tracker issue |
| `start` | in the background, when the first agent session starts in the worktree | `workspace setup` worktrees only, never `devkit pr checkout` ones, so a reviewer's session cannot pull a ticket back from review |
| `pr_open` | when `devkit pr create` opens the PR or finds one already open | any worktree whose record names a tracker issue |
| `todo_start` | in the background, when the todo `devkit todo sync` brought in for the ticket becomes in progress | a checkout of the ticket's repository; from anywhere else the ticket is not moved and a warning names it |
| `todo_done` | in the background, when that todo becomes completed | as `todo_start` |
| `todo_cancel` | when that todo becomes cancelled by `devkit todo cancel`, only with `"status"` in `[ticket.sync] push` | as `todo_start` |

`setup`, `start` and `pr_open` each fire once per worktree. The worktree's `.devkit/issue.toml` records it before the tracker is asked, and a failed move keeps that record, so a tracker that is down costs one missed move rather than a retry on every session. `workspace end` removes the record, and the marks go with it. `setup` and `pr_open` print a failed move as a warning and the command still succeeds. The background `start` run appends its outcome or error to `.devkit/issue-event.log` in the worktree.

`todo_start` and `todo_done` fire on every change into that status, by an agent, a person or a harness's task tool, and never on a change sync makes as the `tracker` holder, so sync and the events cannot move a ticket back and forth. A harness task names its ticket as `references/todo.md` describes. Their background run skips the move once the todo has left the status that fired it, so a todo started and finished quickly ends with the ticket moved for `todo_done`. It appends its outcome or error to `ticket-event.log` in devkit's state directory, and a failed move leaves the todo as it is.

With `push = ["status"]` under `[ticket.sync]`, `devkit todo start`, `done` and `cancel` fire `todo_start`, `todo_done` and `todo_cancel` in the foreground instead, stop on a ticket the tracker moved since the last sync, and on GitHub close the issue on `done` and `cancel`; `references/todo.md` covers it.

`ticket event <event> [ID|URL]` reruns one event by hand, for the worktree's own ticket when no id is given: it retries a failed move or shows its error, whatever the record says. It prints what it did: the move, that the ticket is already there, or that its status is not in `from`. `devkit doctor`'s `issue_events` row lists each transition. On GitHub it also checks the project, its status field and every status name, which proves the token can read the project but not that it can write: a token with only `read:project` passes the row and fails on the first move. On Linear it only lists them, since a team's states are known once an issue is read, so an unknown name surfaces on the first move.

On GitHub the token needs the `project` scope. Without it each move fails naming the remedy: `gh auth refresh -s project` for a token `gh` supplies, or reissuing the variable it came from (`GH_TOKEN`, say) with `project` for a token from the environment.

## `dashboard`

The workspace triage and PR tables plus terminal timelines. `--chart bar|line`, `--bucket` (default `auto`), `--mode` (default `absolute`) and `--aggregate cumulative|period` shape the plots; `--all-roles` widens beyond your own, `--author <gh>` targets someone else; `--no-plots` shows only tables, `--no-cache` forces a fresh fetch. Timeline fetches are cached under `~/.cache/devkit/dashboard` for a few minutes; the live triage panel never is.
