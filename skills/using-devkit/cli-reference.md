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
issue setup <ID|URL> [--slug <slug>] [--apps a,b] [--summary|--no-summary] [--dry-run] [--no-gitignore]
issue checkout-pr <target> [<worktree-path>] [--setup] [--apps a,b]
issue status [ids…]                                   # read-only triage (also the bare `issue`)
issue info [selector] [--json] [--cache-only]         # one worktree's PR number + issue id
issue end [ids…] [-y] [--force] [--pr-only] [--clean-worktree]
issue sync-includes [selectors…] [--overwrite [--all]] [-y] [--dry-run]
issue prs [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache] [--batch-size N] [--retries N]
issue dashboard [--chart bar|line] [--bucket B] [--mode M] [--all-roles] [--author gh] [--no-plots] [--no-cache]
issue review request ["<message>"] [--to <alias|#channel>] [--pr <URL|number>] [--base <branch>] [--pr-title T] [--pr-body B] [--no-push] [--no-notify] [--arg k=v]
issue review finish ["<message>"] [--to <alias|#channel>] [--pr <n>] [--arg k=v]
```

### `issue setup`

Mechanical start of an issue, on whichever tracker the project uses. Creates a worktree off the baseline ref, symlinks
env files, runs the per-app setup commands (e.g. `bun install`), and **prints a JSON
summary to stdout** — an agent's stdout is not a terminal, so JSON is what you get; a
person at a terminal sees the same fields as a table instead:

```json
{ "issue": "ENG-123", "worktree": "/abs/path/to/worktree", "branch": "lev/eng-123-fix-auth" }
```

Under `--summary` the object carries a fourth key, the summary file's path:

```json
{ "issue": "ENG-123", "worktree": "…", "branch": "…", "summary": "/abs/ISSUE_SUMMARY_ENG-123.md" }
```

Read `worktree` to know where to `cd`. Setup does not reserve ports — `devrun up`
allocates them dynamically when the worktree's servers start.

| Flag | Meaning |
|---|---|
| `<ID>` / `--issue <ID>` | issue id or issue URL the tracker recognises — a Linear `ENG-123` or `linear.app` URL, a GitHub issue number or an issue URL in the project's `issues_repo` (positional or flag); drives the branch name and summary. **Required.** |
| `--slug <slug>` | short kebab slug rendered into the branch and worktree dir name (e.g. `lev/eng-123-<slug>`). Omit it and the slug comes from a pasted Linear URL's own `…/issue/<ID>/<title-slug>` path, or failing that from the issue's title as the tracker reports it, which needs that tracker's credential. A leading copy of the issue id is stripped so the branch does not repeat it. A **derived** slug is then shortened on a word boundary so the branch fits the 46-char width `issue status` prints — the budget is measured against your own `branch` template, so a longer `branch_prefix` takes from the slug. A slug you pass here is used verbatim, however long. |
| `--apps <a,b>` | comma-separated apps to bootstrap: writes each one's prep files and runs its setup commands. Omit for a worktree with no per-app setup. |
| `--summary` | also write a markdown summary file: the issue's tracker facts (url, parent, project, state, assignee, priority, estimate, labels — a tracker with no equivalent of a field leaves it empty, as GitHub does for parent, project, priority and estimate) and its description verbatim, then empty `## Summary` and `## Pointers` headings to fill in. Default path `ISSUE_SUMMARY_<ID>.md` under `worktree_root` — beside the worktree, so it survives `git worktree remove`; `templates.issue_summary_path` and `templates.issue_summary` override placement and body. Needs the tracker's credential. An existing file is left byte-for-byte and its path still reported. The fetch runs before the worktree is created, so an unknown issue fails clean. `issue end` removes the recorded file when it cleans the worktree up. `defaults.issue_summary = true` makes this the default. |
| `--no-summary` | skip the summary file for this run, whatever `defaults.issue_summary` says. |
| `--dry-run` | print what it would do without creating the worktree. Reports the resolved `summary` path under `--summary` without writing it. |
| `--no-gitignore` | skip updating the global gitignore (normally adds `.devkit/`, the per-worktree record and cache directory). |

### `issue checkout-pr`

Check out an **existing** PR into a new worktree — the review-side counterpart of
`setup`. The target is `#3340`, `3340`, an issue id the tracker recognises
(`PREFIX-3340`, whose linked PR is used), a GitHub PR URL, or an issue URL the tracker
recognises. A bare `3340` is probed against both the PRs and the tracker's issues, so
on a GitHub project — where issues and PRs share one numbering — it is always the PR.
The optional second positional overrides the worktree path (default: the
config-resolved placement). `--setup` also runs the per-app setup commands;
`--apps a,b` narrows which apps that covers. Prints `pr`, `worktree`, and `branch` — JSON
to a pipe, a table to a terminal.

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
| `--pr <URL\|number>` | act on this PR for this run. A pasted GitHub PR URL keeps its own repository; a bare number means `pr_repo`. The command records whichever PR it acted on, so this is how a worktree bound to the wrong PR is rebound — including the superseded case, where the old and new PRs share a head branch and the branch lookup is ambiguous. Without it the PR comes from the worktree's record, and failing that from its branch. |
| `--base <branch>` | PR base branch (defaults to the configured baseline). |
| `--pr-title T` / `--pr-body B` | override the PR title/body. |
| `--no-push` | open/update the PR without pushing first. |
| `--no-notify` | pin targets to what `--to` resolved to — possibly none — instead of falling back to the PR's current reviewers. |
| `--arg k=v` | **repeatable.** Override a declared template variable. |

On an existing PR with no `--to`, it resolves the PR's current human reviewers and
notifies them; `--no-notify` is how you suppress that.

However the PR was resolved, its head commit must equal this worktree's `HEAD` or the
command refuses it — a branch name is shared across forks and does not prove the PR
carries this work. A squash- or rebase-merged PR still matches, since the comparison is
against the branch head the PR carries. Under `--no-push` a branch ahead of its remote
fails this check.

**`issue review finish`** announces over Slack that you finished reviewing. `--to`
(repeatable) defaults to the PR author. The PR comes from `--pr <n>`, else the
worktree's record, else the branch; `--pr` applies to that run and rewrites nothing.
There is no head-commit check here: this is the reviewer's command, run in a worktree
`checkout-pr` built, where `HEAD` falls behind as soon as the author pushes again.
`[BODY]` fills the `review_finish` template's `{{ input }}`.

### Other `issue` subcommands

- **`status`** (also bare `issue`) — read-only triage table of every issue worktree.
  A worktree is FINISHED only when its PR is merged, its issue has reached a completed
  state in the tracker, and the tree is clean. A project that declares no tracker has no
  state to wait for and is decided by the merged PR and the clean tree alone; a tracker
  that answers nothing for the issue holds the verdict open instead.
- **`info`** — one worktree's PR number and issue id. The optional selector is an
  issue id, branch, worktree basename, or path; omit it for the current worktree.
  `--json` emits a single `IssueWorktree` object (scripts read `.pr_number` /
  `.issue_id`). `--cache-only` skips the network: the PR number comes from the
  per-worktree cache at `<worktree>/.devkit/pr.json` and the tracker columns render as
  `—`. A live run writes the PR through to that cache, which `git worktree remove`
  deletes with the worktree.
- **`end`** — removes FINISHED worktrees. `--pr-only` ignores the tracker-state and
  issue-id gates (finished = PR merged + clean, even on a branch carrying no issue id);
  `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree
  guard; `-y` skips confirmation.
- **`sync-includes`** — re-copies the `defaults.worktree_include` files from the
  monorepo into worktrees that already exist, the list `setup` and `checkout-pr`
  backfill at creation time; reach for it when that list gains an entry after a
  worktree was made. Selectors match as `info`'s do; omit them to sync every
  worktree. The monorepo is the source and never a target. Files the worktree
  already has are left alone and named in a warning. `--overwrite` replaces them
  instead, prompting once per worktree; declining that prompt still copies what
  the worktree is missing. Those files are untracked ones git cannot restore, so
  `--overwrite` needs a scope — one or more selectors, or `--all` for every
  worktree — and `-y` answers the prompt (it does nothing without `--overwrite`).
  `--dry-run` writes nothing.
- **`prs`** — GitHub PR triage of your open PRs and PRs awaiting your review. The
  repository is `[github] pr_repo`, defaulting to the `origin` remote; `-R owner/repo`
  overrides it for one run. `--no-cache` forces a fresh fetch. On a repo with many open PRs GitHub can return
  HTTP 504: lower `--batch-size` (PRs per search page, 1–100) and raise `--retries`
  (extra attempts per page with backoff, 0–10).
- **`dashboard`** — the triage + PR tables plus terminal timelines. `--chart bar|line`,
  `--bucket` (default `auto`) and `--mode` (default `absolute`) shape the plots;
  `--all-roles` widens beyond your own, `--author <gh>` targets someone else;
  `--no-plots` shows only tables, `--no-cache` forces a fresh fetch.

## `devkit` — toolkit setup & diagnostics

```sh
devkit auth <linear|slack> [--token <value>]   # validate + store a credential
devkit auth github                              # report the GitHub identity devkit would use
devkit doctor [--json]                          # check configured credentials + diagnostics
devkit brief [--pins-only|--if-changed]         # compact project brief
devkit schema                                   # JSON Schema for devkit.toml, to stdout
devkit schema init [<path>]                     # point a devkit.toml at the published schema
```

**`auth github`** reports; it stores nothing. devkit keeps no GitHub credential of its
own, because `gh auth login`, `GH_TOKEN` and `GITHUB_TOKEN` already cover it — the
resolution order being `GH_TOKEN`, `GITHUB_TOKEN`, then `gh auth token`. The command
prints the identity behind the token devkit would send and names which of the three
supplied it, then lists `gh`'s own accounts below. Those two can differ, and the
token's identity is the one devkit uses. A `--token` passed here is refused rather than
silently discarded.

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
