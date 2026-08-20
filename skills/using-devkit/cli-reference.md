# devkit CLI reference

Full command/flag reference for the devkit binaries. `SKILL.md` covers the
coordination *discipline*; this file is the lookup table. Every user-facing CLI
also has `--help` on each subcommand and a `completions <shell>` subcommand.

`lockm` lives in `SKILL.md` (Lock command reference) — it is the collaboration
tool, not a lookup. `docm`, the version-correct library-docs cache, belongs to
the separate `docs` skill; go there rather than duplicating its flags here.

Global flags go **before** the subcommand (e.g. `issue -C ~/Git/acme/monorepo status`):

| Flag | Where |
|---|---|
| `-C/--dir <path>` | `issue`, `devrun`, `portm` |
| `--config <file>` | `issue`, `devrun` |
| `--timing[=trace]` | `issue`, `devrun` — IO timing to stderr (`--timing` = summary). Also `DEVKIT_TIMING`. |
| `--timing-log <FILE>` | `issue`, `devrun` — one JSON record per timed IO op |

## `portm` — port registry

Hands out dev-server ports without collisions. A reservation row is written *before*
any process binds, which is what prevents two concurrent callers grabbing the same
port. Usually `devrun`/`issue` drive it for you; call it directly to inspect or
hand-manage ports.

```sh
portm status                                          # table of reserved/live ports (this project)
portm alloc <apps…> [--holder <path>] [--role issue|baseline]    # alias: reserve
portm release [apps…] [--holder <path>] [--role …]    # no apps = everything the holder has
portm prune                                           # drop stale reservations
```

- The **holder is the worktree root path**, not a session token — a worktree's ports
  auto-reclaim when its directory disappears (e.g. `git worktree remove`). `--holder`
  defaults to the current worktree's root (`git rev-parse --show-toplevel`); pass it
  only to act on another worktree.
- `release` frees reservations in the registry; it does not stop processes
  (`devrun down` stops *and* releases).
- `portm status` shows the current project's registry (every worktree of it, since
  the holder is a path). There is no cross-project flag.

## `devrun` — supervised dev servers

Launches and supervises dev servers for the current worktree. Apps you don't name
are auto-detected by diffing against the baseline ref — so on a **fresh worktree with
no diff yet, name the apps explicitly**. Selecting a webapp pulls in `api`
automatically and wires its URL. Run from inside the worktree (or pass `-C <dir>`).

```sh
devrun up [apps…] [--role issue|baseline|both] [--env K=V] [--env-file F] [--supervise] [--dry-run]
devrun down [selector] [--role …] [--all|--others|--holder <path>] [--app …] [--older-than 30m]
devrun status [--all]                                 # tracked servers (this worktree, or all)
devrun reap [--all]                                   # kill servers running OUTSIDE devrun (needs a TTY)
devrun logs <app> [--role …] [-f]                     # print or follow one app's log
devrun config show [--origin] [--json]                # effective merged config
devrun config apps [--json]                           # list configured apps
devrun config tasks [--json]                          # list configured [tasks]
devrun task [<name>] [--env K=V] [--env-file F] [--dry-run]   # run a canned task (no name: list)
```

**`up`** — default `--role issue`. `--role both` runs the issue branch and a fresh
baseline side-by-side on separate ports for A/B comparison. `--supervise` hands
servers to the daemon so they restart on crash. `--dry-run` prints the launch plan
without starting. It allocates ports dynamically from the live registry at start time.

**`down`** — stops servers **and releases their ports** (prints `released ports {…}`).
Defaults to **this worktree only**. Reaching another worktree needs an explicit scope
flag *and* a terminal to confirm — an agent (no PTY) cannot stop another worktree's
servers.

| Command | Effect |
|---|---|
| `devrun down` | stop + release everything in this worktree |
| `devrun down --role baseline` | this worktree, baseline only |
| `devrun down api` | this worktree, fuzzy-match `api` |
| `devrun down --all` | every server, every worktree (one prompt) |
| `devrun down --others` | every server in every *other* worktree |
| `devrun down --holder ../wt/feat-x` | one specific worktree |

A bare positional selector substring-matches across holder/app/port/role/pid and is
mutually exclusive with the column filters, which are `--app`, `--port`, `--role`,
`--pid`, `--listening` / `--not-listening`, and `--older-than` (`90s`/`30m`/`2h`/`1d`).
`--batch` collapses cross-worktree confirmation into one prompt.

**`reap`** — kills dev servers running *outside* the registry (started by hand, or
orphaned). It **always requires an interactive terminal** and has no `--yes`/`--force`
bypass, so an agent cannot run it. Agents get detection only: the untracked section of
`devrun status`, `devkit doctor`'s `devrun_strays` row, and the `ports.strays` MCP
action. Ask the user to reap.

**`task`** — runs a canned `[tasks]` entry from the config: a **command** task in the
foreground (exit code propagated), or a **sequence** of `{ task = … }` / `{ up = … }`
steps in order, stopping at the first failure. The `--env`/`--env-file` overlay applies
to **every** step — command steps layer it above the task's `env`, `up` steps above the
app's `static_env`, same as `devrun up --env`. `--dry-run` prints each rendered plan
(with real resolved ports) without executing.

- A task with `require_live = ["<app>"]` refuses to run unless `<app>` has a
  **live server in this worktree** (a registry row with an alive pid); the
  error names the fix: `devrun up <app>`. Only `ports['<app>']` references arm
  the gate.
- A user `--env` override of a key **waives** both the gate and the port
  allocation for that key's references — an overridden value is taken verbatim.
  `run` argv references cannot be overridden and always arm their gate.
- Sequence command steps are **re-resolved immediately before each executes**
  (fresh ports, gate enforced then), so a step sees servers earlier steps
  started; the upfront pass only validates and feeds `--dry-run`.

## `issue` — issue lifecycle

```sh
issue setup <ID> [--slug <slug>] [--apps a,b] [--dry-run] [--no-gitignore]
issue checkout-pr <target> [<worktree-path>] [--setup] [--apps a,b]
issue status [ids…]                                   # read-only triage (also the bare `issue`)
issue info [selector] [--json] [--cache-only]         # one worktree's PR number + Linear id
issue end [ids…] [-y] [--force] [--pr-only] [--clean-worktree]
issue prs [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache] [--batch-size N] [--retries N]
issue dashboard [--chart bar|line] [--bucket B] [--mode M] [--all-roles] [--author gh] [--no-plots] [--no-cache]
issue review request ["<message>"] [--to <alias|#channel>] [--base <branch>] [--pr-title T] [--pr-body B] [--no-push] [--no-notify] [--arg k=v]
issue review finish ["<message>"] [--to <alias|#channel>] [--pr <n>] [--arg k=v]
```

### `issue setup`

Mechanical start of a Linear issue. Creates a worktree off the baseline ref, symlinks
env files, runs the per-app setup commands (e.g. `bun install`), and **prints a JSON
summary to stdout**:

```json
{ "issue": "ENG-123", "worktree": "/abs/path/to/worktree", "branch": "lev/eng-123-fix-auth" }
```

Read `worktree` to know where to `cd`. Setup does not reserve ports — `devrun up`
allocates them dynamically when the worktree's servers start.

| Flag | Meaning |
|---|---|
| `<ID>` / `--issue <ID>` | Linear issue id (positional or flag); drives the branch name and summary. **Required.** |
| `--slug <slug>` | short kebab slug rendered into the branch and worktree dir name (e.g. `lev/eng-123-<slug>`). Omit it to slugify the issue's Linear title, which needs a Linear key; a leading copy of the issue id is stripped so the branch does not repeat it. |
| `--apps <a,b>` | comma-separated apps to bootstrap: writes each one's prep files and runs its setup commands. Omit for a worktree with no per-app setup. |
| `--dry-run` | print what it would do without creating the worktree. |
| `--no-gitignore` | skip updating the global gitignore (normally ensures devkit artifacts like `ISSUE_*.md` are ignored). |

### `issue checkout-pr`

Check out an **existing** PR into a new worktree — the review-side counterpart of
`setup`. The target is `#3340`, `3340`, `PREFIX-3340`, a GitHub PR URL, or a Linear
issue URL. The optional second positional overrides the worktree path (default: the
config-resolved placement). `--setup` also runs the per-app setup commands;
`--apps a,b` narrows which apps that covers.

### `issue review`

Two subcommands, not one command.

**`issue review request`** ships the branch: pushes it (**never force-pushes**), opens
or reuses its PR, requests the reviewers, and Slack-messages them the PR link plus your
body. With `$SLACK_TOKEN` set it posts directly; otherwise it emits a `SlackIntent` JSON
object for an agent to forward.

| Arg / flag | Meaning |
|---|---|
| `[BODY]` | positional Slack body; fills the `review_request` template's `{{ input }}`. |
| `--to <alias\|#channel>` | **repeatable.** A `[people]` alias (which carries both `slack` and an optional `github`, so one flag sets reviewer *and* recipient) or a literal `#channel`. |
| `--base <branch>` | PR base branch (defaults to the configured baseline). |
| `--pr-title T` / `--pr-body B` | override the PR title/body. |
| `--no-push` | open/update the PR without pushing first. |
| `--no-notify` | pin targets to what `--to` resolved to — possibly none — instead of falling back to the PR's current reviewers. |
| `--arg k=v` | **repeatable.** Override a declared template variable. |

On an existing PR with no `--to`, it resolves the PR's current human reviewers and
notifies them; `--no-notify` is how you suppress that.

**`issue review finish`** announces over Slack that you finished reviewing. `--to`
(repeatable) defaults to the PR author. `--pr <n>` is required when you are not inside
the PR's worktree. `[BODY]` fills the `review_finish` template's `{{ input }}`.

### Other `issue` subcommands

- **`status`** (also bare `issue`) — read-only triage table of every issue worktree.
  A worktree is FINISHED only when its PR is merged, its Linear issue is Done, and the
  tree is clean.
- **`info`** — one worktree's PR number and Linear id. The optional selector is an
  issue id, branch, worktree basename, or path; omit it for the current worktree.
  `--json` emits a single `IssueWorktree` object (scripts read `.pr_number` /
  `.issue_id`). `--cache-only` skips the network: the PR number comes from the
  per-worktree cache at `<worktree>/.devkit/pr.json` and Linear renders as `—`. A live
  run writes the PR through to that cache, which `git worktree remove` deletes with the
  worktree.
- **`end`** — removes FINISHED worktrees. `--pr-only` ignores the Linear and issue-id
  gates (finished = PR merged + clean, even without a Linear-style branch name);
  `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree
  guard; `-y` skips confirmation.
- **`prs`** — GitHub PR triage of your open PRs and PRs awaiting your review.
  `--no-cache` forces a fresh fetch. On a repo with many open PRs GitHub can return
  HTTP 504: lower `--batch-size` (PRs per search page, 1–100) and raise `--retries`
  (extra attempts per page with backoff, 0–10).
- **`dashboard`** — the triage + PR tables plus terminal timelines. `--chart bar|line`,
  `--bucket` (default `auto`) and `--mode` (default `absolute`) shape the plots;
  `--all-roles` widens beyond your own, `--author <gh>` targets someone else;
  `--no-plots` shows only tables, `--no-cache` forces a fresh fetch.

## `devkit` — toolkit setup & diagnostics

```sh
devkit auth <linear|slack> [--token <value>]   # validate + store a credential
devkit doctor [--json]                          # check configured credentials + diagnostics
devkit brief [--pins-only|--if-changed]         # compact project brief
devkit schema                                   # JSON Schema for devkit.toml, to stdout
devkit schema init [<path>]                     # point a devkit.toml at the published schema
```

**`brief`** prints the current checkout's devkit orientation — configured apps, the
`[tasks]` table, this worktree's live servers, and registered library versions — and
prints **nothing** outside a devkit-managed project. A config that fails to load is
reported rather than swallowed, so a broken `devkit.toml` is diagnosable from the brief.
The plugin's `SessionStart` hook runs it so sessions start already knowing the project;
run it by hand to re-orient mid-session.

- `--pins-only` emits only the library-versions section — what a post-compaction
  re-injection wants, without respending the context compaction just reclaimed.
- The library-versions section answers for the directory it runs in. At a
  workspace root it rolls up the members the lockfile names, one row per version
  they resolve; where members disagree, both versions appear with the workspaces
  holding them, so an agent reads the right checkout for the app it is editing.
  A library the reference registry records a checkout for under this project
  shows even without lockfile evidence, sourced `resolved checkout`, and a
  checkout whose version is not the one the lockfile names is flagged
  `; checkout <version>`.
- `--if-changed` prints nothing when this session already received the same brief
  (it reads `session_id` from the hook's stdin JSON). Rejected with `--pins-only`:
  the watermark records the *whole* brief, so suppressing on it after emitting only
  the library table would tell the session it had seen a brief it never got.

Which sections appear is config-driven — `[brief]` has `enabled`, `pins`, `locks`,
`apps`, and `tasks` switches, all defaulting on. A section with nothing to report is
omitted whatever its switch says; a switch turned off suppresses the section even when
the checkout has something to put in it. Live servers this worktree holds are reported
regardless of the `apps` switch — a bound port is a fact about the machine.

**`schema`** prints the JSON Schema derived from the config types. **`schema init`**
prepends the taplo header directive (`#:schema <url>`, first line — *not* a
`# $schema = "…"` key) to the config at `<path>` (default `devkit.toml`), writing a
fully-commented starter when the file does not exist, and leaving a file that already
names a schema alone. See `docs/configuration.md`.

## Enforced mode — mechanics

When write enforcement is on (`SKILL.md` covers *when* and the short version), the
plugin's `PreToolUse` hook owns the lock protocol. Details:

- **Auto-acquire on first write.** Before the first `Edit`/`MultiEdit`/`Write`/
  `NotebookEdit` to a file, the hook locks it for the session. Later writes to the same
  file by the same session (or a sub-agent it delegates to) need no re-acquire.
- **Holder identity.** Top-level writes are held under the session id; sub-agent writes
  under `session_id/agent_id`. A parent holding a file implicitly covers its sub-agents.
- **A blocked write returns a deny** naming the holder:
  ```
  devkit write-harness: src/auth.rs (held by <holder>) — locked by another
  agent; coordinate or wait for it to finish
  ```
- **Automatic release.** Sub-agent locks release on `SubagentStop`; all session locks
  release on `SessionEnd` (normal, Ctrl-C, or error). A 30-min TTL backstops hard kills.
- **`Bash` writes are not covered** — only the structured write tools above.
- **Fail-open when off or when `lockm` is absent** — the hook exits without blocking and
  takes no locks. Install `lockm` via `cargo install --path .` to activate enforcement.
- **Fail-closed on registry errors** — if `lockm` is present but the registry errors
  (corruption, permissions), the hook denies the write rather than allowing it silently.
