# devkit CLI reference

Full command/flag reference for the devkit binaries. `SKILL.md` covers the
coordination *discipline*; this file is the lookup table. Every user-facing CLI
also has `--help` on each subcommand and a `completions <shell>` subcommand.

Global on `issue` and `devrun`: `-C/--dir <path>` and `--config <file>` go **before**
the subcommand (e.g. `issue -C ~/Git/acme/monorepo status`).

## `portm` — port registry

Hands out dev-server ports without collisions. A reservation row is written *before*
any process binds, which is what prevents two concurrent callers grabbing the same
port. Usually `devrun`/`issue` drive it for you; call it directly to inspect or
hand-manage ports.

```sh
portm status                                          # table of reserved/live ports (this project)
portm alloc --holder <path> [--role issue|baseline] <apps…>
portm release --holder <path> [--role …]
portm prune                                           # drop stale reservations
```

- The **holder is the worktree root path**, not a session token — a worktree's ports
  auto-reclaim when its directory disappears (e.g. `git worktree remove`). Get the
  current root with `git rev-parse --show-toplevel`.
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
devrun logs <app> [-f]                                # print or follow one app's log
devrun config show [--origin] [--json]                # effective merged config
devrun config apps [--json]                           # list configured apps
```

**`up`** — default `--role issue`. `--role both` runs the issue branch and a fresh
baseline side-by-side on separate ports for A/B comparison. `--supervise` hands
servers to the daemon so they restart on crash. `--dry-run` prints the launch plan
without starting. It reuses the ports `issue setup` already reserved.

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
mutually exclusive with the column filters. `--older-than` accepts `90s`/`30m`/`2h`/`1d`.

## `issue` — issue lifecycle

```sh
issue setup --issue <ID> --slug <slug> [--apps a,b] [--dry-run] [--no-gitignore]
issue status [ids…]                                   # read-only triage (also the bare `issue`)
issue info [selector] [--json] [--cache-only]         # one worktree's PR number + Linear id
issue end [ids…] [-y] [--force] [--pr-only] [--clean-worktree]
issue prs [-m|--mine] [-r|--reviews] [-R owner/repo]
issue dashboard [--chart bar|line] [--no-plots] [--no-cache]
issue review "<message>" --to <alias> [--reviewer <gh>] [--base <branch>] [--pr-title T] [--pr-body B] [--no-push]
```

### `issue setup`

Mechanical start of a Linear issue. Creates a worktree off the baseline ref, symlinks
env files, runs the per-app setup commands (e.g. `bun install`), reserves ports, and
**prints a JSON summary to stdout**:

```json
{ "issue": "ENG-123", "worktree": "/abs/path/to/worktree", "branch": "lev/eng-123-fix-auth", "ports": { "web": 4101, "api": 4001 } }
```

Read `worktree` to know where to `cd`; `ports` are already reserved for `devrun up`.

| Flag | Meaning |
|---|---|
| `--issue <ID>` | Linear issue id; drives the branch name and summary. **Required.** |
| `--slug <slug>` | short kebab slug rendered into the branch and worktree dir name (e.g. `lev/eng-123-<slug>`). **Required.** |
| `--apps <a,b>` | comma-separated apps to set up; omit to use the config default. |
| `--dry-run` | print what it would do without creating the worktree or reserving ports. |
| `--no-gitignore` | skip updating the global gitignore (normally ensures devkit artifacts like `ISSUE_*.md` are ignored). |

### `issue review`

Ship the branch for review. Pushes the current branch (**never force-pushes**), opens
or reuses its PR, adds a reviewer, and Slack-messages them the PR link plus your body.
With `$SLACK_TOKEN` set it posts directly; otherwise it emits a `SlackIntent` JSON
object for an agent to forward.

| Arg / flag | Meaning |
|---|---|
| `[BODY]` | positional Slack message body; fills the `slack` template's `{{ input }}`. |
| `--to <alias>` | a `[people]` alias from the config — the Slack recipient. **Required.** |
| `--reviewer <gh>` | GitHub handle to request review from on the PR. |
| `--base <branch>` | PR base branch (defaults to the configured baseline). |
| `--pr-title T` / `--pr-body B` | override the PR title/body. |
| `--no-push` | open/update the PR without pushing first. |

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
- **`end`** — removes FINISHED worktrees. `--pr-only` ignores the Linear gate;
  `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree
  guard; `-y` skips confirmation.
- **`prs`** — GitHub PR triage of your open PRs and PRs awaiting your review.
- **`dashboard`** — the triage + PR tables plus terminal timelines; `--no-plots` shows
  only tables, `--no-cache` forces a fresh fetch.

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
