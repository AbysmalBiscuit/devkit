<p align="center">
    <img width="160" alt="devkit logo" src="plugin/assets/icon.svg">
</p>

<h1 align="center">devkit</h1>

<p align="center">
    Run a fleet of coding agents on one machine without collisions.
</p>

devkit is one `devkit` binary, plus an optional `devkitd` daemon, that gives every session on a machine, human or agent, the same view of what they share:

- Registries for what parallel sessions contend for. Allocated ports, advisory file locks over a shared checkout, supervised dev servers, and version-correct library checkouts, each one visible to every other session.
- An issue-to-PR workflow over workspaces, each a branch's git worktree, its ports and its PR. Tracker-agnostic, from setup through review request to cleanup, split across `workspace`, `devkit pr` and `ticket`.
- Todo lists that agents and their subagents share and claim, kept locally or in a Postgres database that several machines claim against.
- Agent wiring for Claude Code, Codex and Cursor: an MCP server and a plugin whose hooks can claim a lock on every write, guard shell commands, and inject the project's rules as files are edited.

The engine is project-agnostic. Every project-specific detail lives in `devkit.toml`.

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-installer.sh | sh
```

```powershell
irm https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-installer.ps1 | iex
```

From a clone instead: `cargo install --path .`

Running `devkit` once installs the old command names (`ticket`, `workspace`, `portm`, `devrun`, `lockm`, `docm`, `devrules`, `devkit-mcp`) as hardlinks beside it, so `docm list` and `devkit docs list` are the same command. See [docs/install.md](docs/install.md) for prebuilt targets, feature flags, the hardlink rules, and where state lives.

## Commands

| Command | What it does |
|---|---|
| `devkit ports` / `portm` | Shared port registry. Reserves before anything binds, so concurrent callers never collide. |
| `devkit run` / `devrun` | Starts and supervises dev servers, with `--role both` for an issue-vs-baseline A/B. Also runs canned `[tasks]`. |
| `devkit workspace` / `workspace` | A branch's worktree, ports and PR as one workspace: setup, triage, cleanup. |
| `devkit pr` | The workspace's PR: open, mark ready, check out to review, request or finish review, list your PRs. |
| `devkit ticket` / `ticket` | Tracker tickets: render, create and edit from templates, move their status, dashboard. |
| `devkit locks` / `lockm` | Advisory file locks, so parallel sessions in one checkout don't edit the same files. |
| `devkit docs` / `docm` | Version-correct local library checkouts, resolved from your own lockfiles. |
| `devkit mcp` / `devkit-mcp` | The MCP server exposing ports, locks, devrun, workspace triage, and templates to coding agents. |
| `devkit auth` / `devkit doctor` | Store a Linear or Slack credential; report where every credential resolves from. |
| `devkit brief` | Compact project orientation for a session-start hook. |
| `devkit rules` / `devrules` | Query the rule index that write-time rule injection reads. |
| `devkit template` | Render a project's templates, custom or built-in, for the caller to deliver itself. |
| `devkit config` / `devkit schema` | Print the merged config and where each value came from; print or install the config's JSON Schema. |

`-h` always prints the full flag list. `--help` matches it at a terminal but prints the whole command tree when piped, so one call shows an agent every verb; `--help --full` asks for the tree anywhere, and `DEVKIT_HELP=terse|full` pins either view. The behavior behind the flags (resolution rules, TTY gates, what refuses and why) is in the [`using-devkit` skill's references](plugin/skills/using-devkit/references/), which is where an agent reads it.

## Coding agents

devkit ships a plugin (the `using-devkit` skill, session hooks, and the MCP server) and the `devkit-mcp` server on its own for hosts without plugins. The plugin installs the binary for you on first session.

```sh
claude plugin marketplace add AbysmalBiscuit/devkit
claude plugin install devkit@devkit
```

See [docs/agents.md](docs/agents.md) for the MCP action list and the setup for Claude Code, Codex, Cursor, Antigravity, and Zed.

## Configuration

Config is layered. Every `devkit.toml` from the filesystem root down to the cwd is merged, with `~/.config/devkit/config.toml` as the base layer beneath them all. Deeper files win per value. Each directory may also carry an untracked `devkit.local.toml` that overrides the `devkit.toml` beside it.

The config is personal: worktree paths, your app catalog, teammate handles. Keep it out of version control.

```sh
mkdir -p ~/.config/devkit
$EDITOR ~/.config/devkit/config.toml
```

`devkit schema init` points a config at the JSON Schema, so your editor validates it and shows every key's description on hover; with no config there yet, it writes a commented starter. [docs/configuration.md](docs/configuration.md) covers layering, secrets and editor setup, with a sanitized example to copy.

## Shell completions

```sh
devkit completions --all fish > ~/.config/fish/completions/devkit.fish
```

bash, zsh, fish, elvish, nushell, and powershell. `--all` emits one file covering every command name. Per-shell details are in [docs/completions.md](docs/completions.md).

## Requirements

`git` and an authenticated `gh` are required. Everything else is optional:

- `doppler`, only if an app's `launch` wraps its command in `doppler run`
- `$LINEAR_API_KEY` authenticates every Linear lookup: issue titles and summaries, the dashboard's issue timeline, and the issue state `workspace status`/`workspace end` gate on. It also makes Linear the tracker of any project that does not name one, so a project on GitHub should set `[tracker] kind` rather than rely on detection
- `$LINEAR_WORKSPACE` enables clickable Linear issue links in `workspace status`
- `$SLACK_TOKEN` lets `devkit pr review` post the reviewer message directly; without it the command emits a `SlackIntent` JSON object

Each of these resolves env-first, then from `~/.config/devkit/secrets.toml`. Run `devkit auth <linear|slack>` to store them, or `devkit doctor` to check them.

GitHub authenticates separately and devkit stores nothing: `$GH_TOKEN`, then `$GITHUB_TOKEN`, then `gh auth token`, so `gh auth login` alone is enough. `devkit auth github` reports which of the three is in effect and whose account it belongs to. The GitHub tracker uses the same chain.

## Troubleshooting

Recoverable failures print the full error context chain. On a panic, the binary prints a bug report with the location and a backtrace. For a backtrace on either, set `RUST_BACKTRACE=1`.
