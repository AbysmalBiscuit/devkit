# devkit for coding agents

devkit ships two things for agents:

- a plugin bundling the `using-devkit` skill, the session and write hooks, and the `devkit-mcp` server; and
- the `devkit-mcp` server on its own, for hosts without a plugin system.

Either way the binaries must be on your `PATH`. The plugin's MCP entry and every config below invoke `devkit-mcp` by name. See [install.md](install.md).

Running `devkit --help` through a pipe, which is what a tool call does, prints
the whole command tree rather than the top-level list. One call is enough to
learn every verb. Reach for a specific command's `-h` (or `DEVKIT_HELP=terse`)
when you need its flags, arguments, and gates, which the tree does not carry.

## The MCP server

`devkit-mcp` (equivalently, `devkit mcp`) exposes devkit's port and file-lock coordination to MCP-capable coding agents over stdio. It presents two tools:

- `devkit_describe`: list the available actions, or fetch one action's argument schema (`{"action": "locks.acquire"}`).
- `devkit_call`: invoke an action, e.g. `{"action": "locks.acquire", "args": {"root": "/path/to/repo", "paths": ["src/a.rs"]}}`.

To keep a plugin's skill and hooks without the server's tools, set `[mcp] enabled = false` in `~/.config/devkit/config.toml`. The server then lists no tools; `devkit schema` describes when the setting takes effect.

### Actions

`ports.{status,alloc,release,prune}` and `locks.{acquire,check,release,status,prune}`. Pass `root` (the project path) on every lock call and on `ports.alloc`/`ports.release`. For locks, `holder` is a session identity detected from the coding-agent session, falling back to `$DEVKIT_SESSION` or a per-process id. For ports, `holder` defaults to `root`, the worktree path the registry uses to track liveness. Either can be overridden per call.

The `devrun` actions are `devrun.status` (tracked servers for a worktree, or `all`), `devrun.up` (start servers, **non-blocking**: returns each server `starting`, so poll `devrun.status` for readiness), `devrun.down` (stop and release a worktree's servers), and `devrun.logs` (tail a tracked app's log). All take `root` (the worktree); `up` is `issue`-role only and starts servers under a running `devkitd` when present, else detached.

Two read-only `issue` actions round it out. `issue.status` lists the issue worktrees for a directory (`root`, default `.`; optional `ids` filter) with each one's PR state, tracker state, and a finished/not-finished verdict; a draft PR's `pr` object carries `is_draft: true` while its legacy `pr_state` field still reads `OPEN`, so a consumer that needs to distinguish a draft reads the flag rather than the state string. `issue.prs` triages your GitHub PRs (`mine`, `reviews`, neither set means both; optional `repo`); a draft's row carries the action `draft` in both sections regardless of any standing review request. Both return structured JSON with the verdicts and next-action labels pre-computed. They never mutate; `issue review`, `issue end`, and `issue pr` stay CLI-only.

The gates differ from the CLI's on purpose. `devrun reap` is never exposed; `ports.strays` is the read-only half of stray handling. `devrun.down` takes one holder, the `root` the caller passes, and that root must be the worktree the server itself started in, resolved once at startup from its working directory and never from anything a caller sends. Any other path is refused, so naming another worktree cannot stand in for the CLI's terminal prompt, and a server started outside a repository stops nothing. The CLI's own-baseline exception is absent too: `devrun.up` runs the issue role only, so an MCP session never starts baseline servers. Read-only actions are unaffected, and `devrun.status` still reports every worktree under `all`.

## Plugin bootstrap

The plugin installs `devkit` for you. Claude Code, Codex, and Cursor have no install-time hook, so a session-start hook checks for `devkit` and, when it is missing, runs the [dist](install.md#prebuilt-binaries) installer for the GitHub release matching the plugin's own version — so the binary stays in lockstep with the hooks and MCP server that drive it. The `devkit brief` hook that runs right after creates the `lockm`, `devkit-mcp`, and other old-name links automatically, so the plugin's other hooks and its MCP entry find them on `PATH` without a separate install step. It re-runs on plugin update, when the version moves.

Two cases where it stays out of the way. Binaries already on `PATH` that the hook did not install (`cargo install`, a distro package, a source build) are never overwritten; the hook records them as externally managed and leaves upgrades to you. And `DEVKIT_NO_BOOTSTRAP=1` disables it outright.

A failed install (offline, say) never blocks the session. It warns and does not retry until you resolve it or delete `${XDG_STATE_HOME:-~/.local/state}/devkit/bootstrap-failed`.

On Windows the hook runs under Git Bash when it is present and under PowerShell otherwise, so it does not depend on a bash being installed. Both paths resolve the same state directory, so gaining Git Bash later does not reinstall.

## What the hooks do

Everything a harness sends enters one verb family, `devkit hook <event>`, and the payload's own `tool_name` picks the path inside it. `pre-tool-use` is what makes lock coordination automatic, so an agent working in a shared checkout does not hand-call `lockm acquire` before an edit.

| devkit verb | What devkit does |
|---|---|
| `pre-tool-use` | Guards the command, claims the write targets it can resolve, records the attempt. |
| `post-tool-use`, `post-tool-use-failure` | Records the outcome. |
| `session-end` | Releases the session's claims, records, sweeps the log. |
| `subagent-stop` | Releases the subagent's claims, records. |
| `session-start`, `subagent-start` | Records the frame. |
| `permission-request`, `permission-denied` | Records what the harness asked about, or what its own classifier blocked. |
| `stop`, `stop-failure`, `pre-compact`, `post-compact`, `cwd-changed` | Records the turn or context boundary. |
| `worktree-create`, `worktree-remove` | Records the change. |
| `user-prompt-submit` | Records, behind its own fidelity key. |

Every verb except `pre-tool-use`, `session-end` and `subagent-stop` is record-only: with logging off each is a process spawn that reads the global config, learns logging is off, and exits. `devkit brief` still runs alongside `session-start`, `post-compact` and `cwd-changed` rather than being replaced by them. `devkit rules context` runs alongside `session-start` too, and again alongside `post-compact` on hosts that resume with a fresh SessionStart: it carries the repository's own must-severity rules when a rule index exists, and stays silent otherwise, so a session opens already knowing what governs the checkout as a whole.

Each manifest is a translation table with no logic in it. Every command carries `--harness <name>`, so identity never depends on guessing which fields a vendor sends this release; without it, devkit infers the harness from the payload's shape.

`pre-tool-use` answers on stdout with a denial, a warning, or nothing, and never runs the command itself. Its write stage fails closed: an unresolved write, a registry error, or a registry that does not answer within 2 seconds is a denial. Its command guard fails open.

| devkit verb | Claude Code | Codex | Cursor |
|---|---|---|---|
| `pre-tool-use` | `PreToolUse` | `PreToolUse` | `beforeShellExecution` |
| `post-tool-use` | `PostToolUse` | `PostToolUse` | `postToolUse` |
| `post-tool-use-failure` | `PostToolUseFailure` | | `postToolUseFailure` |
| `session-start` | `SessionStart` | `SessionStart` | `sessionStart` |
| `session-end` | `SessionEnd` | `SessionEnd` | `sessionEnd` |
| `subagent-start` | `SubagentStart` | `SubagentStart` | |
| `subagent-stop` | `SubagentStop` | `SubagentStop` | `subagentStop` |
| `permission-request` | `PermissionRequest` | `PermissionRequest` | |
| `permission-denied` | `PermissionDenied` | | |
| `stop` | `Stop` | `Stop`, `Interrupt` | `stop` |
| `stop-failure` | `StopFailure` | | |
| `pre-compact` | `PreCompact` | `PreCompact` | `preCompact` |
| `post-compact` | `PostCompact` | `PostCompact` | |
| `cwd-changed` | `CwdChanged` | | `workspaceOpen` |
| `worktree-create` | `WorktreeCreate` | | |
| `worktree-remove` | `WorktreeRemove` | | |
| `user-prompt-submit` | `UserPromptSubmit` | `UserPromptSubmit` | |

A blank cell is an event that harness does not send, or one devkit leaves unwired.

Check the vendor before adding a verb rather than recalling it:

- Claude Code: <https://code.claude.com/docs/en/hooks>, which carries the per-event payloads and the exit-code table.
- Codex: <https://developers.openai.com/codex/config-schema.json>, and `codex-rs/protocol/src/protocol.rs` plus `codex-rs/config/src/hook_config.rs` in `openai/codex` behind it.
- Cursor: <https://cursor.com/docs/hooks>. Not the `cursor-hooks` npm schema, which lags the product.

Cursor reads a decision from the stdout of `preToolUse`, `subagentStart` and `beforeSubmitPrompt`, and devkit answers an allow with nothing. Empty stdout is a proven allow only on `beforeShellExecution`, so the guard rides that event, and the other three stay unwired until a captured Cursor payload shows silence is an allow there too. Cursor's `postToolUse` and `postToolUseFailure` match `Shell`, the calls the guard saw. Its Tab completions edit files through `afterTabFileEdit`, which is post-only, so a Tab edit cannot be lock-guarded; that gap is noted rather than solved.

**Exit codes are part of the contract.** No verb in the family ever exits 2. Exit 2 blocks the tool call on Claude Code `PreToolUse` and sets `should_block` on Codex, and clap exits 2 for a usage error, so an unrecognised verb would otherwise deny every command an agent ran. A usage error, an unknown verb and a panic all exit 1 with a message on stderr. And only `pre-tool-use` writes to stdout: `UserPromptSubmit` appends a hook's stdout to the prompt, and `Stop` and `PermissionRequest` honour a JSON decision.

`devkit harness shell` and `lockm hook <event>` stay as hidden aliases for at least one release. An installed manifest can outlive the binary beside it, so the session-start bootstrap also probes for `devkit hook` once per plugin version and reports a binary too old for the manifests it is being asked to answer.

Both claim paths run only where `[harness] enforce_writes` is on, resolved from the env var, the project layers, or the global config. Everywhere else they exit without effect and nothing is locked.

A conflict surfaces as a denied tool call naming the holder. That is the signal to edit a different file or wait, never to `--force` past a live holder. A manual `lockm acquire` still has one use in an enforced checkout: a coarse claim over a whole subtree you are churning through, since a directory lock covers everything under it.

A shell write is claimed when devkit can resolve its target statically. What happens to the rest, and which hosts get which stage, is in the `using-devkit` skill's [config reference](../skills/using-devkit/references/config.md#harness-write-enforcement).

## Harness logging

Off by default. With `[harness.log] enabled = true` in the global config, each verb writes one JSONL record of what the agent tried and what devkit decided: the command at the configured fidelity, a summary of the analysis, and the full text of every block and warning message. `devkit hook-log path` prints the directory; `devkit hook-log prune` sweeps it; `devkit doctor`'s `harness_log` row reports what is actually in force. The keys, and which of them only a global config may set, are in `devkit schema` and the [config reference](../skills/using-devkit/references/config.md#harnesslog).

Records land under `<dir>/<YYYY-MM-DD>/<session_id>[-<agent_id>].jsonl`. `devkit hook-log prune` (with `--dry-run` to report only) deletes whole files and never rewrites one, so a reader never sees a partial record: first any day directory whose UTC name is older than `max_age_days`, then oldest first until the total is under `max_bytes`. It never touches the current UTC day, a file modified within the last hour (writers take no lock, so this is what separates a sweep from a live session), or anything while another pruner holds `prune.lock`. A sweep that cannot reach the cap without crossing one of those says so and stops. Cohort analysis of a corpus stays offline, in `crates/devkit-command/examples/corpus_probe.rs` behind the `corpus` feature.

## Claude Code

Installing the plugin registers the skill, the hooks, and the MCP server in one step. The plugin manifest points at `.mcp.json`, so enabling the plugin starts the server automatically.

```sh
claude plugin marketplace add AbysmalBiscuit/devkit   # or a local path to this repo
claude plugin install devkit@devkit
```

Or in a session, same arguments (`/plugin` alone opens the interactive browser):

```
/plugin marketplace add AbysmalBiscuit/devkit
/plugin install devkit@devkit
```

Restart Claude Code so the hooks load, then run `/mcp` to confirm the `devkit` server is active and `devkit_describe`/`devkit_call` are listed.

For the MCP server alone, with no skill or hooks: the repo ships `.mcp.json` at the root, so opening this repo in Claude Code registers the `devkit` server project-scoped.

## Codex

```sh
codex plugin marketplace add AbysmalBiscuit/devkit    # or a local path / git URL
codex plugin add devkit@devkit
```

Codex registers the `using-devkit` skill natively from the plugin manifest, so it is announced in every fresh session, and starts the bundled `devkit` MCP server. Confirm with `codex plugin list` and `codex mcp list`.

For the MCP server alone: the repo ships `.codex/config.toml` with `[mcp_servers.devkit]`, registering it project-scoped. Project MCP servers load only in trusted projects, so trust this repo when Codex prompts.

## Cursor

Cursor has no git-repo plugin install from the CLI. Install the plugin from the Customize panel in the sidebar, or, for a team, from Dashboard -> Plugins -> Team Marketplaces -> Add Marketplace -> Import from Repo (`AbysmalBiscuit/devkit`). For local development, symlink the checkout:

```sh
ln -s "$(pwd)" ~/.cursor/plugins/local/devkit
```

For the MCP server alone: the repo ships `.cursor/mcp.json`, the same `mcpServers` shape as Claude Code's, registering it project-scoped.

## Zed and generic MCP clients

No plugin manifest exists for these. Register `devkit-mcp` as a stdio MCP server in the host's own config. The command is just `devkit-mcp`, on `PATH` once `devkit` has run once or after `devkit install-links`. Point the agent at `AGENTS.md` for context; Zed reads `AGENTS.md` directly.

After wiring up any host, confirm `devkit_describe` and `devkit_call` appear.
