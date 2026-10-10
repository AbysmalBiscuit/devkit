# `ticket`: write tracker tickets and move their status

`ticket` (`devkit ticket`) renders, creates and edits the tracker tickets a workspace works on, moves their status, and draws the dashboard. It acts on the **current working directory's worktree** where a verb needs one (`-C/--dir <path>` overrides). `-C` and `--config` go on `ticket` itself, before the verb, or on the verb.

```sh
ticket render --title T [--body B] [--arg k=v] [--arg-file k=path]
ticket create --title T [--body B] [--arg k=v] [--arg-file k=path]
ticket edit <number|URL> --body B [--title T] [--arg k=v] [--arg-file k=path]
ticket event <setup|start|pr_open> [ID|URL]
ticket dashboard [--chart bar|line] [--bucket B] [--mode M] [--aggregate cumulative|period] [--all-roles] [--author gh] [--no-plots] [--no-cache]
```

`ticket pr <verb>` runs `devkit pr <verb>` (`references/pr.md`).

## `render`, `create` and `edit`: write a ticket

Ticket titles and bodies come from the `ticket_title` and `ticket_body` templates; `issue_title` and `issue_body`, their old names, still load. `{{ input }}` is `--title` or `--body`, and the body template also sees the rendered title as `ticket_title` and as `issue_title`. A `[templates.variables]` entry either template reads and marks `required` must be passed as `--arg`. A missing one is refused by name, with its description.

- **GitHub:** `ticket create` renders and runs `gh issue create` against `issues_repo`, then prints the issue URL. Under any other tracker it refuses and points at `ticket render`.
- **GitHub, an existing issue:** `ticket edit <number|URL> --body B` renders the body and runs `gh issue edit` to replace it. `--title T` also renders and replaces the title; without it the issue keeps its title, which the body template sees as `ticket_title`. Under any other tracker it refuses and points at `ticket render`.
- **A tracker MCP (Linear's `save_issue`, GitHub's `issue_write`):** run `ticket render`, which prints `{"title": ..., "body": ...}`, and pass both strings unchanged as the MCP call's title and body fields. Line endings and trailing whitespace may differ, but any other edit is denied as text that differs from the render.
- **Through a tracker MCP, rewriting an existing ticket's title or body** goes through `ticket render` too. An update that only changes state, labels or assignee needs no render. Linear's `patch` edits the body in place, so it is always refused: render the whole new body and pass it as `description`.

A denied MCP call names the render command and the `--arg`s it needs. The render has to happen in the same agent session as the call, because its receipt is filed under the session id. Receipts live under `.devkit/issue-receipts/` in the repository's main worktree, so a render in any worktree of the repository counts, and session end deletes that session's receipts.

Enforcement is configuration. `[harness.issue_tools.<name>]` entries name the MCP servers and tools that write tickets and enforce as soon as they exist. `devkit schema` documents their keys. Refusing a bare `gh issue create` takes a `[harness.commands]` rule with `programs = ["gh"]` and `args = ["issue", "create"]`, and a bare `gh issue edit --body` one with `args = ["issue", "edit", "**", "--body*"]`; like every command rule they need `[harness] enforce_commands = true`. `ticket create` and `ticket edit` run their own `gh`, which the rules never see.

## Status events

`[ticket.events]` moves the ticket's tracker status when devkit acts on it: a Linear workflow state, or on GitHub the single-select field of the Projects v2 project `[github] project` names. Each event moves the ticket only from a status its `from` lists, and a ticket already at `to` is left alone. `[issue.events]`, its old name, still loads. With neither, devkit reads and writes no tracker status. `devkit schema` documents the keys.

| Event | Fires | Where |
|---|---|---|
| `setup` | as `workspace setup`'s last step, after the `after_worktree_create` hooks | the worktree it creates, when it names a tracker issue |
| `start` | in the background, when the first agent session starts in the worktree | `workspace setup` worktrees only, never `devkit pr checkout` ones, so a reviewer's session cannot pull a ticket back from review |
| `pr_open` | when `devkit pr create` opens the PR or finds one already open | any worktree whose record names a tracker issue |

Each event fires once per worktree. The worktree's `.devkit/issue.toml` records it before the tracker is asked, and a failed move keeps that record, so a tracker that is down costs one missed move rather than a retry on every session. `workspace end` removes the record, and the marks go with it. `setup` and `pr_open` print a failed move as a warning and the command still succeeds. The background `start` run appends its outcome or error to `.devkit/issue-event.log` in the worktree.

`ticket event <event> [ID|URL]` reruns one event by hand, for the worktree's own ticket when no id is given: it retries a failed move or shows its error, whatever the record says. It prints what it did: the move, that the ticket is already there, or that its status is not in `from`. `devkit doctor`'s `issue_events` row lists each transition. On GitHub it also checks the project, its status field and every status name, which proves the token can read the project but not that it can write: a token with only `read:project` passes the row and fails on the first move. On Linear it only lists them, since a team's states are known once an issue is read, so an unknown name surfaces on the first move.

On GitHub the token needs the `project` scope. Without it each move fails naming the remedy: `gh auth refresh -s project` for a token `gh` supplies, or reissuing the variable it came from (`GH_TOKEN`, say) with `project` for a token from the environment.

## `dashboard`

The workspace triage and PR tables plus terminal timelines. `--chart bar|line`, `--bucket` (default `auto`), `--mode` (default `absolute`) and `--aggregate cumulative|period` shape the plots; `--all-roles` widens beyond your own, `--author <gh>` targets someone else; `--no-plots` shows only tables, `--no-cache` forces a fresh fetch. Timeline fetches are cached under `~/.cache/devkit/dashboard` for a few minutes; the live triage panel never is.
