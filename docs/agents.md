# devkit for coding agents

devkit ships two things for agents:

- a plugin bundling the `using-devkit` skill, session-start hooks, and the `devkit-mcp` server; and
- the `devkit-mcp` server on its own, for hosts without a plugin system.

Either way the binaries must be on your `PATH`. The plugin's MCP entry and every config below invoke `devkit-mcp` by name. See [install.md](install.md).

## The MCP server

`devkit-mcp` (equivalently, `devkit mcp`) exposes devkit's port and file-lock coordination to MCP-capable coding agents over stdio. It presents two tools:

- `devkit_describe`: list the available actions, or fetch one action's argument schema (`{"action": "locks.acquire"}`).
- `devkit_call`: invoke an action, e.g. `{"action": "locks.acquire", "args": {"root": "/path/to/repo", "paths": ["src/a.rs"]}}`.

### Actions

`ports.{status,alloc,release,prune}` and `locks.{acquire,check,release,status,prune}`. Pass `root` (the project path) on every lock call and on `ports.alloc`/`ports.release`. For locks, `holder` is a session identity minted from `$DEVKIT_SESSION` (or a per-process id). For ports, `holder` defaults to `root`, the worktree path the registry uses to track liveness. Either can be overridden per call.

The `devrun` actions are `devrun.status` (tracked servers for a worktree, or `all`), `devrun.up` (start servers, **non-blocking**: returns each server `starting`, so poll `devrun.status` for readiness), `devrun.down` (stop and release a worktree's servers), and `devrun.logs` (tail a tracked app's log). All take `root` (the worktree); `up` is `issue`-role only and starts servers under a running `devkitd` when present, else detached.

Two read-only `issue` actions round it out. `issue.status` lists the issue worktrees for a directory (`root`, default `.`; optional `ids` filter) with each one's PR state, tracker state, and a finished/not-finished verdict. `issue.prs` triages your GitHub PRs (`mine`, `reviews`, neither set means both; optional `repo`). Both return structured JSON with the verdicts and next-action labels pre-computed. They never mutate; `issue review` and `issue end` stay CLI-only.

## Plugin bootstrap

The plugin installs `devkit` for you. Claude Code, Codex, and Cursor have no install-time hook, so a session-start hook checks for `devkit` and, when it is missing, runs the [dist](install.md#prebuilt-binaries) installer for the GitHub release matching the plugin's own version — so the binary stays in lockstep with the hooks and MCP server that drive it. The `devkit brief` hook that runs right after creates the `lockm`, `devkit-mcp`, and other old-name links automatically, so the plugin's other hooks and its MCP entry find them on `PATH` without a separate install step. It re-runs on plugin update, when the version moves.

Two cases where it stays out of the way. Binaries already on `PATH` that the hook did not install (`cargo install`, a distro package, a source build) are never overwritten; the hook records them as externally managed and leaves upgrades to you. And `DEVKIT_NO_BOOTSTRAP=1` disables it outright.

A failed install (offline, say) never blocks the session. It warns and does not retry until you resolve it or delete `${XDG_STATE_HOME:-~/.local/state}/devkit/bootstrap-failed`.

On Windows the hook runs under Git Bash when it is present and under PowerShell otherwise, so it does not depend on a bash being installed. Both paths resolve the same state directory, so gaining Git Bash later does not reinstall.

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

Cursor has no git-repo plugin install from the CLI. Install the plugin from the Customize panel in the sidebar, or, for a team, from Dashboard → Plugins → Team Marketplaces → Add Marketplace → Import from Repo (`AbysmalBiscuit/devkit`). For local development, symlink the checkout:

```sh
ln -s "$(pwd)" ~/.cursor/plugins/local/devkit
```

For the MCP server alone: the repo ships `.cursor/mcp.json`, the same `mcpServers` shape as Claude Code's, registering it project-scoped.

## Zed and generic MCP clients

No plugin manifest exists for these. Register `devkit-mcp` as a stdio MCP server in the host's own config. The command is just `devkit-mcp`, on `PATH` once `devkit` has run once or after `devkit install-links`. Point the agent at `AGENTS.md` for context; Zed reads `AGENTS.md` directly.

After wiring up any host, confirm `devkit_describe` and `devkit_call` appear.
