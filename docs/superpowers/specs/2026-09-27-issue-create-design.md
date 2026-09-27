# Issue creation with enforced templates

Issue: [#163](https://github.com/AbysmalBiscuit/devkit/issues/163)

## Problem

Agents file issues through `gh issue create`, the Linear MCP (`save_issue`) or the GitHub MCP (`issue_write`). devkit never sees those calls, so nothing holds an issue to a project's format. A cloud agent often has the Linear MCP but no Linear token, so a devkit verb that writes through the Linear API cannot be the only path.

## Goal

A project declares an issue template once, in `devkit.toml`, and every issue an agent creates in that project comes out of it with the template's required fields filled:

- through `devkit issue create` on GitHub, which renders and creates in one step;
- through an MCP create call, which the pre-tool-use hook allows only when its title and body are exactly what `devkit issue render` produced in the same agent session.

Enforcement reuses the `[templates.variables]` `required` markings and `check_required`, the machinery `issue pr create` already enforces `pr_title` and `pr_body` with. No second definition of the template exists anywhere.

## Out of scope

- Creating Linear issues through the Linear API. Linear issues are created through the Linear MCP, gated by the hook.
- Cursor MCP calls. pabal does not model Cursor, and Cursor reports MCP calls through a separate `beforeMCPExecution` event.
- Labels, assignees, projects, and more than one issue template per project.
- A devkit MCP action for issue creation. Agents run the CLI through their shell tool.
- Changes to the `Tracker` trait, which stays read-only.

## Design

### Templates

Two keys join `[templates]`, next to `pr_title` and `pr_body`:

- `issue_title`: title of an issue rendered by `issue render` or created by `issue create`. `{{ input }}` is the `--title` argument. Defaults to `{{ input }}`. The rendered title must not be empty.
- `issue_body`: body of the same issue. `{{ input }}` is the `--body` argument, and `issue_title` is the rendered title. Defaults to `{{ input }}`.

Both render with the `[templates.variables]` defaults and `--arg` overrides, as `pr_title` and `pr_body` do. Before rendering, `check_required` runs over both templates, so a declared variable that a template reads and whose `required` marking binds the caller must arrive as `--arg`. The refusal names each missing arg with its description.

Example:

```toml
[templates]
issue_body = """
{{ input }}

## Acceptance criteria
{{ acceptance }}
"""

[templates.variables]
acceptance = { required = "agents", description = "observable outcomes that mean the issue is done" }
```

A variable read inside `{% if %}` counts as read whatever the condition, so it is required on every issue. That is acceptable with one template per project.

### `devkit issue render`

```
devkit issue render --title TEXT [--body TEXT] [--arg KEY=VALUE ...]
```

Works under every tracker and needs no credential.

1. Runs `check_required` over `issue_title` and `issue_body` and renders both.
2. Writes a receipt for the rendered pair (below).
3. Prints `{"title": "...", "body": "..."}` to stdout. JSON keeps the strings exact: an agent copies them into an MCP call whose arguments are JSON strings as well.

When no harness session id is set (a human at a terminal), it renders and prints but writes no receipt, and says so on stderr. Humans never pass through the hook, so no fallback id exists. When the start directory is not inside a git checkout, it fails before rendering, since a receipt has nowhere to go.

### `devkit issue create`

```
devkit issue create --title TEXT [--body TEXT] [--arg KEY=VALUE ...]
```

GitHub only.

1. Refuses under a Linear or `none` tracker, naming `devkit issue render` plus the tracker's MCP as the way to create the issue.
2. Renders exactly as `issue render` does, without a receipt.
3. Runs `gh issue create --title <title> --body <body>` against `[github] issues_repo` through `cmd::gh_capture`.
4. Prints the issue URL and number.

The tracker selection reuses `issue::tracker::select`, and the repository comes from `github::Repos`, so the command talks to the same repository `issue setup` reads.

### Receipts

A receipt records that `issue render` produced a given title and body in a given agent session.

- **Location.** `<checkout>/.devkit/issue-receipts/<session>/<digest>`, where `<checkout>` is the git checkout root of the start directory. The file is empty; its name is the record. `gitignore::write_self_ignore` keeps `.devkit/` untracked.
- **Session.** The CLI takes it from `HARNESS_SESSION_VARS` (`CLAUDE_CODE_SESSION_ID`, `CODEX_SESSION_ID`), the same values the hook payload carries as `session_id`. When two variables hold different ids, it writes one receipt under each. A session id containing anything other than ASCII letters, digits, `-` and `_` is rejected, since it becomes a path component.
- **Digest.** `harness_log::redact::digest` (SHA-256 hex) of `normalize(title) + "\0" + normalize(body)`. `normalize` converts CRLF to LF, strips trailing whitespace from each line, and trims the whole string. A missing title or body field in an MCP call hashes as the empty string.
- **Concurrency.** Each receipt is its own file named by its content, so parallel agents and subagents in one session never write the same file and need no lock. A receipt only unlocks the exact text it was made from, so one subagent's receipt cannot authorize another's different text.
- **Lifetime.** The `session-end` hook deletes `<checkout>/.devkit/issue-receipts/<session>/` for the payload's checkout. Receipts are not consumed when a call is allowed, because an allowed call can still fail at the MCP server and be retried. A receipt left in another checkout (an agent that rendered after `cd`-ing elsewhere) goes when that worktree is removed.

Subagents are not scoped separately. Claude Code gives a subagent's shell the same environment as its parent, with no agent id, so the CLI cannot tell subagents apart. Content addressing already keeps them from interfering.

### Hook enforcement of MCP creates

A new table under `[harness]`, one entry per MCP tool that creates issues:

```toml
[harness.issue_create.linear]
servers = ["*linear*"]
tools   = ["save_issue"]
absent  = ["id"]
title   = "title"
body    = "description"

[harness.issue_create.github]
servers = ["github"]
tools   = ["issue_write"]
equals  = { method = "create" }
title   = "title"
body    = "body"
```

`IssueCreateRule` fields:

- `servers`: globs matched case-insensitively against pabal's MCP server name (`claude.ai Linear` under Claude Code). Empty matches any server.
- `tools`: exact MCP tool names.
- `absent`: input keys that must be missing or null for the call to count as a create. `save_issue` updates when `id` is present.
- `equals`: input keys that must equal the given string for the call to count as a create. `issue_write` creates when `method` is `create`.
- `title`, `body`: the input keys holding the issue's title and body.
- `enabled`: `false` turns off an entry a parent layer declared, as with `harness.commands`.

`pre_tool_use` gains a third branch. After the edit check, a `Tool::Mcp` payload goes to `issue_create::guard`:

1. Load the `[harness]` section. When no enabled entry matches the call's server, tool, `absent` and `equals`, allow without output.
2. Resolve the session from the payload and the checkout from the payload's cwd. Either missing denies.
3. Digest the call's title and body fields. When `<checkout>/.devkit/issue-receipts/<session>/<digest>` exists, allow. Otherwise deny.

The deny reason tells the agent to run `devkit issue render --title ... [--body ...]` and then pass its `title` and `body` output unchanged as the call's `<title key>` and `<body key>`. It lists the `--arg`s the templates require of an agent, with their descriptions, computed the way `check_required` computes them with nothing supplied. When a receipts directory exists for the session but no digest matches, the reason says the text differs from what was rendered.

Once an entry matches, every error (session, checkout, IO) denies. The table is an opt-in enforcement rule, and one that fails open enforces nothing. A config that fails to load, or no devkit config at all, allows: no entry can be known to match, and denying there would block every MCP tool in a project whose `devkit.toml` has a typo. That half fails open like the shell guard.

Manifests:

- Claude Code: the `PreToolUse` matcher becomes `Edit|MultiEdit|Write|NotebookEdit|Bash|PowerShell|mcp__.*`.
- Codex: the matcher becomes `apply_patch|Write|Edit|Bash|mcp__.*`. Codex emits `PreToolUse` for MCP tools under `mcp__<server>__<tool>` (`codex-rs/core/src/tools/handlers/mcp.rs`, `pre_tool_use_payload`).

Every MCP call now starts a `devkit hook` process. Dispatch happens on the payload's tool before any config load, as it does for edits, and a project with no `issue_create` entries allows right after loading config.

### Shell bypass

`gh issue create` run by an agent is refused with an existing `[harness.commands]` rule. No code is needed:

```toml
[harness.commands.gh-issue-create]
programs = ["gh"]
args     = ["issue", "create"]
reason   = "file issues with `devkit issue create` so the issue template applies"
```

`devkit issue create` runs `gh` as its own child process, which the hook never sees, so the rule does not block it.

### devkit's own config

devkit's `devkit.toml` ships both `[harness.issue_create]` entries above (`linear` and `github`) and the `gh-issue-create` command rule, so this repository enforces its own issue path. It leaves `issue_title` and `issue_body` at their defaults. Issues filed here still go through `issue render` or `issue create`, and a template can be added later without touching the rules.

## Documentation

- Doc comments on `issue_title`, `issue_body` and `IssueCreateRule` fields, which become schema descriptions. A `devkit.toml` doctest on `IssueCreateRule`. `schema/devkit-config.json` regenerated.
- Clap help for `issue render` and `issue create`.
- `plugin/skills/using-devkit/references/issues.md` gains a section on filing issues: render then send for MCP trackers, `issue create` for GitHub, and the two config tables that enforce it.

## Testing

Tests come first and drive the real entry points.

Hook, through `devkit hook pre-tool-use --harness claude-code` with a `save_issue` payload in a temp checkout carrying an `issue_create` entry:

- no receipt: denied, and the reason names `devkit issue render` and each required arg;
- after `devkit issue render` run with `CLAUDE_CODE_SESSION_ID` set to the payload's session: allowed;
- the same text with CRLF line endings and trailing spaces: allowed;
- one word of the body changed: denied, and the reason says the text differs;
- a receipt under another session id: denied;
- a call carrying `id`: allowed with no receipt;
- an MCP tool no entry matches: allowed with no output;
- an `issue_write` payload with `method = "update"`: allowed; with `method = "create"` and no receipt: denied.

Session end, through `devkit hook session-end`: the session's receipts directory is gone and another session's remains.

`issue render`: a missing required arg is refused by name; the JSON output round-trips into a payload the hook allows; a run with no harness session writes no receipt.

`issue create`, against `tests/common/ghfake.rs`: a missing required arg is refused before `gh` runs; success passes the rendered title and body to `gh issue create`; a Linear tracker is refused with the render-then-MCP pointer.

Manifests: `tests/hook_manifests.rs` asserts both matchers route `mcp__` tools to `pre-tool-use`.

Config: the `IssueCreateRule` doctest parses both example entries; `enabled = false` in a child layer turns an entry off.
