# devkit

A Rust workspace of six binaries that coordinate local development for a monorepo. Devkit provides flock'd port and file-lock registries (both served from memory by an optional `devkitd` supervisor daemon), a supervised dev-app runner with baseline A/B comparison, and a single `issue` command covering the whole issue lifecycle (setup, triage, cleanup, PR status, dashboard, review). All project-specific details live in `devkit.toml`; the engine itself is project-agnostic.

## Binaries

### `portm`: Port Registry

Maintains a shared port registry so concurrent callers never collide on port allocation. State lives in `~/.local/state/devkit/ports.json`, guarded by an advisory file lock. Reservation rows are written before any process binds, which prevents the allocation race across concurrent callers.

```
portm status                                     # table of reserved/live ports
portm alloc --holder <path> [--role issue|baseline] <apps…>
portm release --holder <path> [--role …]
portm prune                                      # remove stale reservations
```

### `devrun`: Supervised Dev Servers

Launches and supervises dev servers for one or more apps. Apps not explicitly named are auto-detected by diffing `git diff <baseline_ref>...HEAD`. When any webapp is selected, `api` is added automatically and `FOUNDRY_API_BASE_URL` is wired to the local api port. Each app's `launch` command is run verbatim with `{port}` substituted; wrap it in `doppler run` in the config if the app needs Doppler-injected secrets. `--role both` runs the issue branch and a fresh `origin/staging` baseline side-by-side on separate ports for direct A/B comparison.

```
devrun up [apps…] [--role issue|baseline|both] [--env K=V] [--env-file F] [--dry-run]
devrun down [--role …] [--all | --others | --holder <path>] [--app …] [--older-than …] [--batch] [selector]
devrun status [--all]
devrun logs <app> [-f]
devrun config show [--origin] [--json]
devrun config apps [--json]
```

- **`config show`**: prints the effective merged config as TOML. `--origin` annotates each value with the file it was resolved from (or `# (default)` for serde defaults); `--json` emits JSON instead of TOML. `--origin --json` emits `{ "config": …, "origins": { "dotted.path": "file" } }`.
- **`config apps`**: lists the configured apps from the merged config (columns: name, port, path, provides_url, url_env, launch). `--json` emits a structured array. A pure config readout with no live readiness — for running state use `devrun status`.
- **`down`**: stops servers and releases their ports. By default stops every server in the current worktree. Reaching another worktree requires an explicit scope flag and a confirmation read from a terminal:

  | Command | Effect |
  |---|---|
  | `devrun down` | stop all servers in this worktree |
  | `devrun down --role baseline` | this worktree, baseline only |
  | `devrun down api` | this worktree, fuzzy-match `api` across columns |
  | `devrun down --all` | every server, every worktree (one batch prompt) |
  | `devrun down --others` | every server in every *other* worktree |
  | `devrun down --others api` | `api` in other worktrees (per-worktree prompts) |
  | `devrun down --holder ../wt/feat-x` | one specific worktree |
  | `devrun down --all --app api --older-than 1h` | precise filter, all worktrees |

  A positional selector substring-matches across `HOLDER`/`APP`/`PORT`/`ROLE`/`PID` and is mutually exclusive with the column filters (`--app`, `--port`, `--role`, `--pid`, `--listening`/`--not-listening`, `--older-than`). `--older-than` accepts `90s`, `30m`, `2h`, `1d` (bare number = seconds). Any selection that reaches outside the current worktree prints a preview and prompts; with no interactive terminal it is refused. `--all`/`--batch` collapse the per-worktree prompts into one.

### `issue`: Issue Lifecycle

One command covering the whole issue lifecycle. Global `-C/--dir` and `--config` flags sit on `issue` itself, before the subcommand (e.g. `issue -C ~/Git/acme/monorepo status`).

```
issue setup --issue <ID> --slug <slug> --apps <a,b> [--dry-run]
issue checkout-pr <PR_LINEAR_ID_URL> [WORKTREE_PATH] [--setup [--apps a,b]]
issue status [ids…]                           # read-only triage table (also the bare `issue`)
issue info [selector] [--json] [--cache-only] # one worktree's PR number + Linear id (defaults to current)
issue end [ids…] [-y] [--force] [--pr-only] [--clean-worktree]
issue prs [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache]
issue dashboard [--bucket auto|day|week|month] [--chart bar|line] [--mode absolute|proportional] [--all-roles] [--author <email>] [--no-plots] [--no-cache]
issue review "<message>" --to <alias> [--reviewer <gh>] [--base <branch>] [--pr-title <t>] [--pr-body <b>] [--no-push]
```

- **`setup`**: mechanical start of a Linear issue. Creates a worktree off the baseline ref, symlinks env files, runs `bun install`, and prints a JSON summary. It does not reserve ports — `devrun up` allocates them dynamically when the worktree's servers start.
- **`checkout-pr`**: checks out an existing PR branch into a new worktree (unlike `setup`, which creates a new branch). The target may be a GitHub PR number (`#3340`), a bare number (`3340`, probed against both GitHub and Linear and disambiguated — prompts on a real collision in a TTY, errors if ambiguous without one), a Linear id (`ENG-3340`, whose attached PR is used — an issue with no attached PR is an error), or a GitHub/Linear URL. The worktree directory is named by the `templates.checkout_worktree_dir` template (variables: `pr_number`, `pr_title`, `linear_id`, `linear_title`; titles are slugified; `linear_*` are empty on the PR-only path); the default renders e.g. `3340-fix-login`, or `3340-fix-login_[ENG-42]` when reached via Linear. Pass `[WORKTREE_PATH]` to override the placement. The PR's own branch name is kept; the template governs only the directory. Add `--setup [--apps a,b]` to also run the per-app prep pipeline, exactly as `issue setup` does. The worktree gets a `.devkit/issue.toml` record so `issue status`/`issue end` recognise it.
- **`status`** (the default when you run bare `issue`): triage table of every issue worktree. A worktree is FINISHED only when its PR is MERGED, its Linear issue is Done, and the working tree is clean.
- **`info`**: shows one worktree's PR number and Linear id. The optional selector is an issue id, branch, worktree basename, or path; omit it for the current worktree. `--json` emits a single machine-readable object (the `IssueWorktree` struct, with `pr_number`/`issue_id` for scripts). `--cache-only` skips the network — the PR number comes from the per-worktree cache at `<worktree>/.devkit/pr.json` and Linear state renders as `—`. A live run writes the PR through to that cache, which `git worktree remove` deletes with the worktree.
- **`end`**: removes FINISHED worktrees. `--pr-only` ignores the Linear gate; `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree guard; `-y` skips confirmation.
- **`prs`**: GitHub PR triage of your open PRs and PRs awaiting your review, with a per-repo diff cache that renders `old → new` for anything changed since the last run.
- **`dashboard`**: the triage + PR tables, plus terminal timelines of your Linear issues by status, PRs opened/merged, and commits over time (`--chart bar` or `line`). The timeline fetches (Linear + GitHub) are cached under `~/.cache/devkit/dashboard` for a few minutes so reruns are fast; the live triage/PR panel is never cached. `--no-plots` shows only the tables; `--no-cache` forces a fresh fetch.
- **`review`**: pushes the current branch, opens or reuses its PR, adds a reviewer, and sends the reviewer a Slack message with the PR link. Never force-pushes. With `$SLACK_TOKEN` set it posts directly; otherwise it emits a `SlackIntent` JSON object for an agent to forward. `--to` names a `[people]` alias from the config.

### `lockm`: File Locks

Advisory locks on paths so parallel sessions sharing one checkout (where per-session
worktrees are too expensive) don't edit the same files at once. A flock-guarded
registry of claims keyed by path, the file-level twin of `portm`. Locks are
exclusive and overlap by path component, so locking a directory conflicts with
locking a file inside it.

```
lockm acquire <paths…> [--as <id>] [--note <msg>] [--ttl <secs>] [--json]
lockm release <paths…> [--as <id>]        # or: release --all
lockm check   <paths…> [--json]           # read-only: would acquire succeed?
lockm status  [--all] [--json]
lockm prune
```

Sessions identify themselves by (in priority order) `--as <id>`, `$DEVKIT_SESSION`,
`$TMUX_PANE` (zero-config and unique per tmux pane), the controlling tty, or the
parent pid. Conflicts fail fast: `acquire`/`check` exit `1` and report who holds the
path. Locks expire after their TTL (default 30 min, `--ttl 0` disables) or when a
recorded anchor pid dies; `release` frees them explicitly. For non-interactive agent
sessions, pass a stable `--as`/`$DEVKIT_SESSION` so acquire and release agree.

### `devkit`: Setup & Diagnostics

Configures and diagnoses the toolkit itself. `auth` validates a Linear or Slack
credential against the live API and stores it in `~/.config/devkit/secrets.toml`
(`0600`); `doctor` reports where each credential resolves from and whether it is
valid. Tokens always resolve env-first, so a shell export or Doppler-injected var
still wins.

~~~
devkit auth <linear|slack> [--token <value>]   # validate + store; prompts (no echo) by default
devkit doctor [--json]                          # check configured credentials
devkit completions <shell>
~~~

- **`auth`**: prompts for the token without echo (or reads `--token`/piped stdin),
  validates it, and saves it. For Linear it also stores the workspace slug derived
  from the API, so issue links work without setting `LINEAR_WORKSPACE`.
- **`doctor`**: one row per credential — source (`env`/`file`/`unset`) and live
  validity. Exits non-zero only when a credential that *is* set fails validation.

## devkit-mcp (MCP server)

`devkit-mcp` exposes devkit's port and file-lock coordination to MCP-capable
coding agents over stdio. It presents two tools:

- `devkit_describe`: list the available actions, or fetch one action's argument
  schema (`{"action": "locks.acquire"}`).
- `devkit_call`: invoke an action, e.g.
  `{"action": "locks.acquire", "args": {"root": "/path/to/repo", "paths": ["src/a.rs"]}}`.

v1 actions: `ports.{status,alloc,release,prune}` and
`locks.{acquire,check,release,status,prune}`. Pass `root` (the project path) on
every lock call and on `ports.alloc`/`ports.release`. For locks, `holder` is a
session identity minted from `$DEVKIT_SESSION` (or a per-process id). For ports,
`holder` defaults to `root` (the worktree path the registry uses to track liveness).
Either can be overridden per call.

Phase-2 `devrun` actions: `devrun.status` (tracked servers for a worktree, or
`all`), `devrun.up` (start servers, **non-blocking**: returns each server
`starting`; poll `devrun.status` for readiness), `devrun.down` (stop + release
a worktree's servers), and `devrun.logs` (tail a tracked app's log). All take
`root` (the worktree); `up` is `issue`-role only and starts servers under a
running `devkitd` when present, else detached.

The MCP server also exposes two read-only `issue` actions: `issue.status` lists
the issue worktrees for a directory (`root`, default `.`; optional `ids` filter)
with each one's PR state, Linear state, and a finished/not-finished verdict;
`issue.prs` triages your GitHub PRs (`mine`, `reviews`, neither set means both;
optional `repo`). Both return structured JSON with the verdicts and next-action
labels pre-computed. They never mutate; `issue review`/`issue end` stay CLI-only.

Install with `cargo install --path .` (it builds alongside the other binaries),
then register it with your agent. The repo ships project-scoped registration for
three hosts, each pointing at the `devkit-mcp` command on your `PATH`:

- **Claude Code**: `.mcp.json` (also referenced by the bundled plugin manifest).
- **Cursor**: `.cursor/mcp.json` (same `mcpServers` shape).
- **Codex**: `.codex/config.toml` (`[mcp_servers.devkit]`; project MCP servers
  load only in trusted projects).

After installing, open the host in this repo and confirm `devkit_describe` and
`devkit_call` appear (`/mcp` lists active servers in Claude Code and Codex).

## Configuration

Config is layered. Every `devkit.toml` from the filesystem root down to the cwd is merged, with `~/.config/devkit/config.toml` as the lowest-precedence base layer beneath them all. Deeper files override shallower ones per value: tables merge key-by-key, while scalars and arrays replace wholesale. `devrun config show` prints the merged result; `--origin` traces each value to the file it came from.

Two escapes bypass the walk:

- `[config] root = true` in a `devkit.toml` stops the upward walk at that file and drops every shallower layer, including the home config — full isolation.
- `--config <path>` or `$DEVKIT_CONFIG` selects a single file verbatim, with no layering or home base.

App `path` is normally inferred from the monorepo's `doppler.yaml`; individual `[apps.<name>]` sections may override it with an explicit `path`. `launch` is run verbatim, so a Doppler wrapper lives in each app's `launch`; devkit refuses to start a Doppler launch whose config resolves to `prd`, so it can't run against production secrets.

App conventions are config-driven, not hardcoded:

- `provides_url = true` marks the app that serves the URL other apps consume (the API). Consumer apps name that variable in their own `url_env`; `devrun` wires it to the provider's local port and auto-includes the provider when a consumer is run.
- `prep_files` declares files written into an app's directory during `issue setup` (before its `setup` commands). Each is `{ path, content, overwrite }`; `content` is rendered as a minijinja template with the issue context (`prefix`, `issue`, `slug`, `apps`, `app`, `branch`, `worktree`), and existing files are kept unless `overwrite = true`.
- `defaults.apps_dir` (default `apps`) is the repo-relative directory apps live under; it drives path inference and diff-based app detection.

### Setting up your config

The config is personal (worktree paths, app catalog, teammate handles, local
secrets) and is **not** distributed; keep it out of version control. See
[`docs/configuration.md`](docs/configuration.md) for the full config reference
and a sanitized example. Copy that example to your config location and edit it:

```sh
mkdir -p ~/.config/devkit
$EDITOR ~/.config/devkit/config.toml   # paste & adapt the example from docs/configuration.md
```

### Templates

`issue setup` and `issue review` render five strings from optional minijinja
templates under `[templates]`. Each unset key falls back to a default that
matches the historical hardcoded output.

```toml
[templates]
branch       = "{{ prefix }}{{ issue }}-{{ slug }}"
worktree_dir = "{{ slug }}"
pr_title     = "{{ issue }}: {{ input }}"
pr_body      = "Closes {{ issue }}.\n\n{{ input }}"
slack        = "{{ pr_title }}\n{{ input }}\n{{ pr_url }}"

[templates.variables]            # constants; a context field of the same name wins
team = "platform"
```

| Key | Default | Context |
|---|---|---|
| `branch`, `worktree_dir` | `{{ prefix }}{{ slug }}`, `{{ slug }}` | `prefix`, `issue`, `slug`, `apps` |
| `checkout_worktree_dir` | `{{ pr_number }}-{{ pr_title }}` (or `{{ pr_number }}-{{ pr_title }}_[{{ linear_id }}]` via Linear) | `pr_number`, `pr_title`, `linear_id`, `linear_title` |
| `pr_title` | `{{ input }}` | review base + `input` = `--pr-title` |
| `pr_body` | `{{ input }}` | review base + `input` = `--pr-body`, `pr_title` |
| `slack` | `{{ input }} {{ pr_url }}` | review base + `input` = `body` arg, `pr_title`, `pr_url` |

Review base context: `branch`, `reviewer`, `to`, and `issue`/`slug`/`apps` from
the `.devkit/issue.toml` record `issue setup` writes in the worktree. `issue
setup` also adds `.devkit/` to your global gitignore (`--no-gitignore` skips it).
An undefined variable is an error (strict mode), so typos surface immediately.

## Install

Install all six binaries (`portm`, `devrun`, `issue`, `lockm`, `devkit`, `devkitd`)
into `~/.cargo/bin` with one command:

```sh
cargo install --path .
```

This builds with default features, which include the `devkitd` supervisor daemon.
`devkitd` serves both the port registry (`ports.sock`) and the lock registry
(`locks.sock`) from memory, writing through to the files, and is used by
`devrun up --supervise`. To skip the daemon, build a lean set with
`cargo install --path . --no-default-features` (omits `devkitd` and `devrun`'s
`--supervise` support).

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
portm completions zsh   > ~/.zfunc/_portm
lockm completions zsh   > ~/.zfunc/_lockm
# bash:
issue completions bash > ~/.local/share/bash-completion/completions/issue
```

## State & Cache Locations

| Data | Path |
|---|---|
| Port registry | `~/.local/state/devkit/ports.json` |
| Server logs | `~/.local/state/devkit/logs/` |
| File-lock registry | `~/.local/state/devkit/locks.json` |
| PR diff cache (`issue prs`) | `$XDG_CACHE_HOME/devkit/pr-status/` (or `~/.cache/devkit/pr-status/`) |

The state home honors `$XDG_STATE_HOME` (default `~/.local/state`). A legacy
`~/.claude/state/devkit` home is migrated automatically on first run.

## Requirements

**Required:**

- `git`
- `gh` (GitHub CLI, authenticated)

**Optional:**

- `doppler`: only if an app's `launch` wraps its command in `doppler run` (see [docs/configuration.md](docs/configuration.md))
- `$LINEAR_API_KEY`: enables the Linear issue-Done gate in `issue status`/`issue end` and the issue timeline in `issue dashboard`
- `$LINEAR_WORKSPACE`: enables clickable Linear issue links in `issue status`
- `$SLACK_TOKEN`: lets `issue review` post the reviewer message directly (otherwise it emits a `SlackIntent` JSON object)

Each of these resolves env-first, then from `~/.config/devkit/secrets.toml`. Run
`devkit auth <linear|slack>` to store them, or `devkit doctor` to check them.

## Troubleshooting

Recoverable failures print the full error context chain. On a panic, the binary
prints a bug report with the location and a backtrace. For a backtrace on either,
set `RUST_BACKTRACE=1`.
