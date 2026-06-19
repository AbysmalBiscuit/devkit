# devkit

A Rust workspace of five binaries that coordinate local development for a monorepo. Devkit provides a flock'd port registry (no daemon), a supervised dev-app runner with baseline A/B comparison, mechanical issue setup, and worktree/PR triage. All project-specific details live in `devkit.toml`; the engine itself is project-agnostic.

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

### `issue-prep` — Issue Setup

Performs the mechanical steps required to start work on a Linear issue: creates a worktree off `origin/staging`, symlinks env files, runs `bun install`, reserves ports via `portman`, and prints a JSON summary of the resulting setup.

```
issue-prep --issue <ID> --slug <slug> --apps <a,b> [--dry-run]
```

### `issue-end` — Worktree Triage

Identifies and cleans up finished issue worktrees. A worktree is considered FINISHED only when its PR is MERGED, its Linear issue is Done, and the working tree is clean.

```
issue-end status                              # default: show triage table
issue-end clean [ids…] [-y] [--force] [--pr-only]
issue-end clean --clean-worktree <sel…>
```

### `pr-status` — PR Triage

Renders an at-a-glance GitHub PR triage view with two tables: your open PRs and PRs currently awaiting your review. A per-repo diff cache tracks values between runs and renders `old → new` for anything that changed since the last invocation.

```
pr-status [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache]
```

## Configuration

Config discovery order (first match wins):

1. `--config <path>` flag
2. `$DEVKIT_CONFIG` environment variable
3. `./devkit.toml` (walks up to the filesystem root)
4. `~/.config/devkit/config.toml`

App `path` and `doppler_project` are normally inferred from the monorepo's `doppler.yaml`; individual `[apps.<name>]` sections may override them. The `doppler_config` value must not be `prd` — devkit guards against accidentally running against production secrets.

App conventions are config-driven, not hardcoded:

- `provides_url = true` marks the app that serves the URL other apps consume (the API). Consumer apps name that variable in their own `url_env`; `devrun` wires it to the provider's local port and auto-includes the provider when a consumer is run.
- `prep_env = { KEY = "value" }` is written to `<app>/.env.local` during `issue-prep`.
- `defaults.apps_dir` (default `apps`) is the repo-relative directory apps live under; it drives path inference and diff-based app detection.

### Example setup

```sh
mkdir -p ~/.config/devkit
cp configs/example.toml ~/.config/devkit/config.toml
```

## Install

Build all five binaries:

```sh
cargo build --release            # all five binaries into target/release
```

Or install them into `~/.cargo/bin`:

```sh
cargo install --path crates/portman
cargo install --path crates/devrun
cargo install --path crates/issue-prep
cargo install --path crates/issue-end
cargo install --path crates/pr-status
```

## State & Cache Locations

| Data | Path |
|---|---|
| Port registry | `~/.claude/state/devkit/ports.json` |
| Server logs | `~/.claude/state/devkit/logs/` |
| PR status cache | `$XDG_CACHE_HOME/devkit/` (or `~/.cache/devkit/`) |

## Requirements

**Required:**

- `git`
- `gh` (GitHub CLI, authenticated)
- `doppler`
- `bun`

**Optional:**

- `$LINEAR_API_KEY` — enables the Linear issue-Done gate in `issue-end`
- `$LINEAR_WORKSPACE` — enables clickable Linear issue links in `issue-end status`

## Troubleshooting

Recoverable failures print the full error context chain. On a panic, the binary
prints a bug report with the location and a backtrace. For a backtrace on either,
set `RUST_BACKTRACE=1`.
