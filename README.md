# devkit

Coordination for running many local dev sessions at once, human and agent, as one `devkit` binary plus an optional `devkitd` daemon.

Three things that lean on each other:

- Registries for what parallel sessions contend for. Allocated ports, advisory file locks over a shared checkout, running dev servers, and version-correct library checkouts, each one visible to every other session on the machine.
- `issue`, an issue-to-PR workflow over git worktrees. Tracker-agnostic, from setup through review request to cleanup.
- Agent wiring: an MCP server, a session-brief hook, and a skill, so coding agents drive the same registries you do.

The engine is project-agnostic. Every project-specific detail lives in `devkit.toml`.

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-installer.sh | sh
```

```powershell
irm https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-installer.ps1 | iex
```

From a clone instead: `cargo install --path .`

Running `devkit` once installs the old command names (`portm`, `devrun`, `issue`, `lockm`, `docm`, `devkit-mcp`) as hardlinks beside it, so `docm list` and `devkit docs list` are the same command. See [docs/install.md](docs/install.md) for prebuilt targets, feature flags, the hardlink rules, and where state lives.

## Commands

| Command | What it does |
|---|---|
| `devkit ports` / `portm` | Shared port registry. Reserves before anything binds, so concurrent callers never collide. |
| `devkit run` / `devrun` | Starts and supervises dev servers, with `--role both` for an issue-vs-baseline A/B. Also runs canned `[tasks]`. |
| `devkit issue` / `issue` | Issue lifecycle: worktree setup, PR checkout, triage, cleanup, review requests, dashboard. |
| `devkit locks` / `lockm` | Advisory file locks, so parallel sessions in one checkout don't edit the same files. |
| `devkit docs` / `docm` | Version-correct local library checkouts, resolved from your own lockfiles. |
| `devkit mcp` / `devkit-mcp` | The MCP server exposing ports, locks, devrun, and issue triage to coding agents. |
| `devkit auth` / `devkit doctor` | Store a Linear or Slack credential; report where every credential resolves from. |
| `devkit brief` | Compact project orientation for a session-start hook. |

`-h` is the authoritative flag list under every condition; `--help` matches it at a terminal, but answers with the command tree when piped. [docs/commands.md](docs/commands.md) carries what `--help` cannot: resolution rules, the TTY gates, and the reasoning behind them.

## Coding agents

devkit ships a plugin (the `using-devkit` skill, session hooks, and the MCP server) and the `devkit-mcp` server on its own for hosts without plugins. The plugin installs the binary for you on first session.

```sh
claude plugin marketplace add AbysmalBiscuit/devkit
claude plugin install devkit@devkit
```

See [docs/agents.md](docs/agents.md) for the MCP action list and the setup for Claude Code, Codex, Cursor, and Zed.

## Configuration

Config is layered. Every `devkit.toml` from the filesystem root down to the cwd is merged, with `~/.config/devkit/config.toml` as the base layer beneath them all. Deeper files win per value. Each directory may also carry an untracked `devkit.local.toml` that overrides the `devkit.toml` beside it.

The config is personal: worktree paths, your app catalog, teammate handles. Keep it out of version control.

```sh
mkdir -p ~/.config/devkit
$EDITOR ~/.config/devkit/config.toml
```

[docs/configuration.md](docs/configuration.md) is the full reference, with a sanitized example to copy. `devkit schema init` points a config at the JSON Schema so your editor validates it.

## Shell completions

```sh
devkit completions --all fish > ~/.config/fish/completions/devkit.fish
```

bash, zsh, fish, elvish, nushell, and powershell. `--all` emits one file covering every command name. Per-shell details are in [docs/completions.md](docs/completions.md).

## Requirements

`git` and an authenticated `gh` are required. Everything else is optional:

- `doppler`, only if an app's `launch` wraps its command in `doppler run`
- `$LINEAR_API_KEY` authenticates every Linear lookup: issue titles and summaries, the dashboard's issue timeline, and the issue state `issue status`/`issue end` gate on. It also makes Linear the tracker of any project that does not name one, so a project on GitHub should set `[tracker] kind` rather than rely on detection
- `$LINEAR_WORKSPACE` enables clickable Linear issue links in `issue status`
- `$SLACK_TOKEN` lets `issue review` post the reviewer message directly; without it the command emits a `SlackIntent` JSON object

Each of these resolves env-first, then from `~/.config/devkit/secrets.toml`. Run `devkit auth <linear|slack>` to store them, or `devkit doctor` to check them.

GitHub authenticates separately and devkit stores nothing: `$GH_TOKEN`, then `$GITHUB_TOKEN`, then `gh auth token`, so `gh auth login` alone is enough. `devkit auth github` reports which of the three is in effect and whose account it belongs to. The GitHub tracker uses the same chain.

## Troubleshooting

Recoverable failures print the full error context chain. On a panic, the binary prints a bug report with the location and a backtrace. For a backtrace on either, set `RUST_BACKTRACE=1`.
