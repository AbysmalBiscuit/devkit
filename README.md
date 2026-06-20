# devkit

A Rust workspace of four binaries that coordinate local development for a monorepo. Devkit provides a flock'd port registry (with an optional supervisor daemon), a supervised dev-app runner with baseline A/B comparison, and a single `issue` command covering the whole issue lifecycle (setup, triage, cleanup, PR status, dashboard, review). All project-specific details live in `devkit.toml`; the engine itself is project-agnostic.

## Binaries

### `portman` — Port Registry

Maintains a shared port registry so concurrent callers never collide on port allocation. State lives in `~/.claude/state/devkit/ports.json`, guarded by an advisory file lock. Reservation rows are written before any process binds, which prevents the allocation race across concurrent callers.

```
portman status                                     # table of reserved/live ports
portman alloc --holder <path> [--role issue|baseline] <apps…>
portman release --holder <path> [--role …]
portman prune                                      # remove stale reservations
```

### `devrun` — Supervised Dev Servers

Launches and supervises dev servers for one or more apps. Apps not explicitly named are auto-detected by diffing `git diff <baseline_ref>...HEAD`. When any webapp is selected, `api` is added automatically and `FOUNDRY_API_BASE_URL` is wired to the local api port. Servers are launched under `doppler run -c dev_local`, bypassing each app's own `dev` script. `--role both` runs the issue branch and a fresh `origin/staging` baseline side-by-side on separate ports for direct A/B comparison.

```
devrun up [apps…] [--role issue|baseline|both] [--env K=V] [--env-file F] [--dry-run]
devrun down [--role …]
devrun status [--all]
devrun logs <app> [-f]
```

### `issue` — Issue Lifecycle

One command covering the whole issue lifecycle. Global `-C/--dir` and `--config` flags sit on `issue` itself, before the subcommand (e.g. `issue -C ~/Git/acme/monorepo status`).

```
issue setup --issue <ID> --slug <slug> --apps <a,b> [--dry-run]
issue status [ids…]                           # read-only triage table (also the bare `issue`)
issue end [ids…] [-y] [--force] [--pr-only] [--clean-worktree]
issue prs [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache]
issue dashboard [--bucket auto|day|week|month] [--chart bar|line] [--mode absolute|proportional] [--all-roles] [--author <email>] [--no-plots]
issue review "<message>" --to <alias> [--reviewer <gh>] [--base <branch>] [--pr-title <t>] [--pr-body <b>] [--no-push]
```

- **`setup`** — mechanical start of a Linear issue: creates a worktree off the baseline ref, symlinks env files, runs `bun install`, reserves ports via the registry, and prints a JSON summary.
- **`status`** (the default when you run bare `issue`) — triage table of every issue worktree. A worktree is FINISHED only when its PR is MERGED, its Linear issue is Done, and the working tree is clean.
- **`end`** — removes FINISHED worktrees. `--pr-only` ignores the Linear gate; `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree guard; `-y` skips confirmation.
- **`prs`** — at-a-glance GitHub PR triage: your open PRs and PRs awaiting your review, with a per-repo diff cache that renders `old → new` for anything changed since the last run.
- **`dashboard`** — the at-a-glance triage + PR tables, plus terminal timelines of your Linear issues by status, PRs opened/merged, and commits over time (`--chart bar` or `line`). `--no-plots` shows only the tables.
- **`review`** — pushes the current branch, opens or reuses its PR, adds a reviewer, and sends the reviewer a Slack message with the PR link. Never force-pushes. With `$SLACK_TOKEN` set it posts directly; otherwise it emits a `SlackIntent` JSON object for an agent to forward. `--to` names a `[people]` alias from the config.

## Configuration

Config discovery order (first match wins):

1. `--config <path>` flag
2. `$DEVKIT_CONFIG` environment variable
3. `./devkit.toml` (walks up to the filesystem root)
4. `~/.config/devkit/config.toml`

App `path` and `doppler_project` are normally inferred from the monorepo's `doppler.yaml`; individual `[apps.<name>]` sections may override them. The `doppler_config` value must not be `prd` — devkit guards against accidentally running against production secrets.

App conventions are config-driven, not hardcoded:

- `provides_url = true` marks the app that serves the URL other apps consume (the API). Consumer apps name that variable in their own `url_env`; `devrun` wires it to the provider's local port and auto-includes the provider when a consumer is run.
- `prep_env = { KEY = "value" }` is written to `<app>/.env.local` during `issue setup`.
- `defaults.apps_dir` (default `apps`) is the repo-relative directory apps live under; it drives path inference and diff-based app detection.

### Setting up your config

The config is personal (worktree paths, app catalog, teammate handles, local
secrets) and is **not** distributed — keep it out of version control. See
[`docs/configuration.md`](docs/configuration.md) for the full config reference
and a sanitized example. Copy that example to your config location and edit it:

```sh
mkdir -p ~/.config/devkit
$EDITOR ~/.config/devkit/config.toml   # paste & adapt the example from docs/configuration.md
```

## Install

Install all four binaries (`portman`, `devrun`, `issue`, `devkit-portd`) into
`~/.cargo/bin` with one command:

```sh
cargo install --path .
```

This builds with default features, which include the supervisor daemon
(`devkit-portd`) used by `devrun up --supervise`. To skip the daemon, build a
lean set with `cargo install --path . --no-default-features` (omits
`devkit-portd` and `devrun`'s `--supervise` support).

Or just build into `target/release` without installing:

```sh
cargo build --release
```

## Shell completions

The CLIs generate their own completion scripts via a `completions <shell>`
subcommand (bash, zsh, fish, elvish, powershell). For example:

```sh
issue completions zsh   > ~/.zfunc/_issue
devrun completions zsh  > ~/.zfunc/_devrun
portman completions zsh > ~/.zfunc/_portman
# bash:
issue completions bash > ~/.local/share/bash-completion/completions/issue
```

## State & Cache Locations

| Data | Path |
|---|---|
| Port registry | `~/.claude/state/devkit/ports.json` |
| Server logs | `~/.claude/state/devkit/logs/` |
| PR diff cache (`issue prs`) | `$XDG_CACHE_HOME/devkit/pr-status/` (or `~/.cache/devkit/pr-status/`) |

## Requirements

**Required:**

- `git`
- `gh` (GitHub CLI, authenticated)
- `doppler`
- `bun`

**Optional:**

- `$LINEAR_API_KEY` — enables the Linear issue-Done gate in `issue status`/`issue end` and the issue timeline in `issue dashboard`
- `$LINEAR_WORKSPACE` — enables clickable Linear issue links in `issue status`
- `$SLACK_TOKEN` — lets `issue review` post the reviewer message directly (otherwise it emits a `SlackIntent` JSON object)

## Troubleshooting

Recoverable failures print the full error context chain. On a panic, the binary
prints a bug report with the location and a backtrace. For a backtrace on either,
set `RUST_BACKTRACE=1`.
