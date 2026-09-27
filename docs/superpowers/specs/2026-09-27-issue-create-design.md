# Issue creation with enforced templates

Issue: [#163](https://github.com/AbysmalBiscuit/devkit/issues/163)

## Problem

Agents file issues through `gh issue create`, the Linear MCP (`save_issue`) or the GitHub MCP (`issue_write`). devkit never sees those calls, so nothing holds an issue to a project's format. A cloud agent often has the Linear MCP but no Linear token, so a devkit verb that writes through the Linear API cannot be the only path.

## Goal

A project declares an issue template once, in `devkit.toml`, and every issue title and body an agent writes in that project comes out of it with the template's required fields filled:

- through `devkit issue create` on GitHub, which renders and creates in one step;
- through an MCP call that creates an issue or rewrites its title or body, which the pre-tool-use hook allows only when that text is exactly what `devkit issue render` produced in the same agent session.

Enforcement reuses the `[templates.variables]` `required` markings and `check_required`, the machinery `issue pr create` already enforces `pr_title` and `pr_body` with. No second definition of the template exists anywhere.

## Threat model

This is a guardrail for agents that would otherwise forget the template, not a sandbox. An agent set on bypassing it can: write a receipt file itself, reach the tracker API through `gh api`, `curl` or another client, or call the tracker's MCP through a proxy MCP that exposes it under another server and tool name. The design leaves those paths open.

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
2. Resolves the session ids (below). With none (a human at a terminal), skips to step 4 and says on stderr that no receipt was written. Humans never pass through the hook, so no fallback id exists.
3. Refuses before writing anything when any session id is invalid. Resolves the receipt store (below), failing when the start directory is in no git checkout, and writes the receipts.
4. Prints `{"title": "...", "body": "..."}` to stdout. JSON keeps the strings exact: an agent copies them into an MCP call whose arguments are JSON strings as well.

### `devkit issue create`

```
devkit issue create --title TEXT [--body TEXT] [--arg KEY=VALUE ...]
```

GitHub only.

1. Refuses under a Linear or `none` tracker, naming `devkit issue render` plus the tracker's MCP as the way to create the issue.
2. Renders exactly as `issue render` does, without receipts.
3. Runs `gh issue create --title <title> --body <body>` against `[github] issues_repo` through `cmd::gh_capture`.
4. Prints the issue URL and number.

The tracker selection reuses `issue::tracker::select`, and the repository comes from `github::Repos`, so the command talks to the same repository `issue setup` reads.

### Receipts

A receipt records that `issue render` produced a given title, or a given body, in a given agent session. Each render writes two: one for its title and one for its body.

- **Location.** `<store>/.devkit/issue-receipts/<session>/title-<hex>` and `.../body-<hex>`, where `<store>` is the repository's main worktree, or the checkout root when there is none. Every worktree of one repository shares the store, because a harness reports the directory its session started in, not the worktree the agent rendered from. The files are empty; their names are the record. `gitignore::write_self_ignore` keeps `.devkit/` untracked.
- **Digest.** `<hex>` is the SHA-256 of the normalized text as bare lowercase hex. `harness_log::redact::digest` prefixes `sha256:`, and a colon is not a legal NTFS filename character, so the receipt module takes the hex without that prefix. `normalize` converts CRLF to LF, strips trailing whitespace from each line, and trims the whole string.
- **Session.** The CLI takes it from `HARNESS_SESSION_VARS` (`CLAUDE_CODE_SESSION_ID`, `CODEX_SESSION_ID`), the same values the hook payload carries as `session_id`. When two variables hold different ids, it writes the receipts under each.
- **Session id as a path.** A session id is valid only when it is non-empty and every character is an ASCII letter, digit, `-` or `_`. The CLI refuses an invalid one. The pre-tool-use hook denies a matched call whose payload session id is invalid, and the session-end hook skips it.
- **Concurrency.** Each receipt is its own file named by its content, so parallel agents and subagents in one session never write the same file and need no lock.
- **Per-field receipts.** A call's title and body are checked separately, so a call may pair the title of one render with the body of another. Both still came out of the template, so this is accepted.
- **Lifetime.** Receipts are not consumed when a call is allowed, because an allowed call can still fail at the MCP server and be retried. The `session-end` hook deletes `<store>/.devkit/issue-receipts/<session>/` for the payload's repository. A session that never fires `session-end` (a killed cloud agent) or rendered in another repository leaves its directory behind, so `issue render` first deletes every session directory in its store whose modification time is older than seven days.

Subagents are not scoped separately. Claude Code gives a subagent's shell the same environment as its parent, with no agent id, so the CLI cannot tell subagents apart. Content addressing already keeps them from interfering.

### Hook enforcement of MCP writes

A new table under `[harness]`, one entry per MCP tool that creates or edits issues:

```toml
[harness.issue_tools.linear]
servers    = ["*linear*"]
tools      = ["save_issue"]
absent     = ["id"]
title      = "title"
body       = "description"
body_patch = ["patch"]

[harness.issue_tools.github]
servers = ["*github*"]
tools   = ["issue_write"]
equals  = { method = "create" }
title   = "title"
body    = "body"
```

`IssueToolRule` fields:

- `servers`: patterns matched case-insensitively against pabal's MCP server name (`claude.ai Linear` under Claude Code), with the single-argument `*` rule `CommandRule.args` uses: `*` matches any run of characters. Empty matches any server.
- `tools`: exact, case-sensitive MCP tool names. Empty matches nothing.
- `absent`: input keys that must be missing or null for the call to count as a create. `save_issue` updates when `id` is present.
- `equals`: input keys that must equal the given string for the call to count as a create. `issue_write` creates when `method` is `create`.
- `title`, `body`: the input keys holding the issue's title and body.
- `body_patch`: input keys that edit the body without carrying it whole. A call carrying one is denied, since no receipt can vouch for the result. Linear's `save_issue` edits a description in place through `patch`.
- `enabled`: `false` turns off an entry a parent layer declared, as with `harness.commands`.

An entry enforces on its own presence. Unlike `harness.commands`, it does not depend on `enforce_commands`.

A call **matches** an entry when its server and tool match. A matched call is checked as follows:

- **Create** (every `absent` key missing and every `equals` pair holding): the title and the body each need a receipt. A missing title or body field is checked as the empty string, so a Linear create that leaves `description` empty to take a Linear-side `template` is denied.
- **Update** (anything else): a `body_patch` key denies. Otherwise each of the title and body fields the call carries needs a receipt, and fields it omits are not checked. An update touching neither (state, labels, assignee) is allowed.

Dispatch in `pre_tool_use` becomes: edit, then MCP, then shell. Every `Tool::Mcp` payload takes the MCP branch and returns from it, so none reaches `shell::guard`. The MCP branch:

1. Merges the `[harness.issue_tools]` tables from the config layers at the payload's cwd, the way the command guard merges its rules. An entry that fails to parse is skipped, so a typo in one entry cannot block every MCP tool.
2. Finds the first enabled entry the call matches. None allows without output.
3. From here every failure denies: an invalid or missing session id, no checkout at the payload's cwd, a `.devkit` that is a file, a `tool_input` that is not a JSON object, an IO error, or a panic. The branch runs under `catch_unwind` with a `matched` flag, the pattern `shell::guard` uses with its `write_stage` flag, so a panic after a match denies and a panic before one allows. The table is an opt-in enforcement rule, and one that fails open enforces nothing.
4. Checks the receipts under `<store>/.devkit/issue-receipts/<session>/`.

The deny reason tells the agent to run `devkit issue render --title ... [--body ...]` and then pass its `title` and `body` output unchanged as the call's `<title key>` and `<body key>`. It lists the `--arg`s the templates require of an agent, with their descriptions, computed the way `check_required` computes them with nothing supplied. That list loads the full config, so only a missing-receipt denial builds it. When the session's receipts directory exists but a field has no matching receipt, the reason names that field and says its text differs from what was rendered. A `body_patch` denial says to render the whole new body instead.

The MCP branch writes no harness log record.

Manifests:

- Claude Code: the `PreToolUse` matcher becomes `Edit|MultiEdit|Write|NotebookEdit|Bash|PowerShell|mcp__.*`.
- Codex: the matcher becomes `apply_patch|Write|Edit|Bash|mcp__.*`. Codex emits `PreToolUse` for MCP tools under `mcp__<server>__<tool>` (`codex-rs/core/src/tools/handlers/mcp.rs`, `pre_tool_use_payload`) and applies a hook block to them like any other tool (`codex-rs/core/src/tools/registry.rs`).

Every MCP call now starts a `devkit hook` process. Dispatch happens on the payload's tool before any config load, and a project with no `issue_tools` entries allows right after loading config.

### Shell bypass

`gh issue create` run by an agent is refused with an existing `[harness.commands]` rule:

```toml
[harness.commands.gh-issue-create]
programs = ["gh"]
args     = ["issue", "create"]
reason   = "file issues with `devkit issue create` so the issue template applies"
```

Command rules fire only when `[harness] enforce_commands = true` resolves for the checkout, so a project needs both. `devkit issue create` runs `gh` as its own child process, which the hook never sees, so the rule does not block it.

### devkit's own config

devkit's `devkit.toml` ships both `[harness.issue_tools]` entries above (`linear` and `github`) and the `gh-issue-create` command rule. It already sets `enforce_commands = true`. It leaves `issue_title` and `issue_body` at their defaults. Issues filed here still go through `issue render` or `issue create`, and a template can be added later without touching the rules.

## Documentation

- Doc comments on `issue_title`, `issue_body` and `IssueToolRule` fields, which become schema descriptions. A `devkit.toml` doctest on `IssueToolRule`. `schema/devkit-config.json` regenerated.
- Clap help for `issue render` and `issue create`.
- `plugin/skills/using-devkit/references/issues.md` gains a section on filing issues: render then send for MCP trackers, `issue create` for GitHub, rewriting a body through `render`, and the two config tables that enforce it, including the `enforce_commands` requirement.

## Testing

Tests come first and drive the real entry points.

Hook, through `devkit hook pre-tool-use --harness claude-code` with MCP payloads in a temp checkout carrying the two shipped `issue_tools` entries:

- `save_issue` create with no receipt: denied, and the reason names `devkit issue render` and each required arg;
- after `devkit issue render` run with `CLAUDE_CODE_SESSION_ID` set to the payload's session: allowed;
- the same text with CRLF line endings and trailing spaces: allowed;
- one word of the body changed: denied, and the reason names the body as differing;
- a receipt under another session id: denied;
- a payload session id of `../x`: denied;
- `save_issue` update carrying `id` and state only: allowed with no receipt;
- `save_issue` update carrying `id` and an unrendered `description`: denied; with a rendered one: allowed;
- `save_issue` update carrying `patch`: denied;
- an MCP tool no entry matches: allowed with no output, and no shell-guard denial;
- `issue_write` with `method = "create"` and no receipt: denied; `method = "update"` with no title or body: allowed;
- a server named `claude.ai GitHub` matches the `*github*` entry.

The same `save_issue` create, once denied and once allowed, through `--harness codex` with a Codex-shaped payload.

Session end, through `devkit hook session-end`: the session's receipts directory is gone, another session's remains, and a payload session id of `..` deletes nothing.

`issue render`: a missing required arg is refused by name; the JSON output round-trips into a payload the hook allows; a run with no harness session writes no receipt and succeeds outside a checkout; receipt filenames contain no `:`; a session directory older than seven days is deleted.

`issue create`, against `tests/common/ghfake.rs`: a missing required arg is refused before `gh` runs; success passes the rendered title and body to `gh issue create`; a Linear tracker is refused with the render-then-MCP pointer.

Manifests: `tests/hook_manifests.rs` asserts both matchers route `mcp__` tools to `pre-tool-use`.

Config: the `IssueToolRule` doctest parses both shipped entries; `enabled = false` in a child layer turns an entry off.
