# `issue` — issue lifecycle

`issue` acts on the **current working directory's worktree** by default (`-C/--dir <path>` overrides), and `issue review` ships the branch checked out there. `cd` into the right worktree first. `-C` and `--config` go on `issue` itself, before the subcommand.

Every subcommand works from the primary checkout, which git resolves: the main worktree when you stand in a linked one, otherwise the checkout root. The directory's name does not matter.

```sh
issue setup <ID|URL> [--slug <slug>] [--apps a,b] [--summary|--no-summary] [--dry-run] [--no-gitignore]
issue setup --slug <slug> [--apps a,b] [--dry-run] [--no-gitignore]
issue render --title T [--body B] [--arg k=v] [--arg-file k=path]
issue create --title T [--body B] [--arg k=v] [--arg-file k=path]
issue status [ids…]                                   # read-only triage (also the bare `issue`)
issue pr [status] [selector] [--json] [--cache-only]  # also the bare `issue pr`
issue pr create [--draft|--ready] [--to <alias>] [--base <branch>] [--pr-title T] [--pr-body B] [--attach <file>[#alt]] [--no-push] [--pr <URL|number>] [--arg k=v] [--arg-file k=path]
issue pr render [--pr-title T] [--pr-body B] [--arg k=v] [--arg-file k=path]
issue pr ready [--to <alias>] [--no-push] [--pr <URL|number>]
issue pr checkout <target> [<worktree-path>] [--setup] [--apps a,b]
issue end [ids…] [-y] [--force] [--pr-only] [--clean-worktree] [--no-preserve]
issue sync-includes [selectors…] [--overwrite [--all]] [-y] [--dry-run]
issue prs [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache] [--batch-size N] [--retries N]
issue dashboard [--chart bar|line] [--bucket B] [--mode M] [--aggregate cumulative|period] [--all-roles] [--author gh] [--no-plots] [--no-cache]
issue review request ["<message>"] [--to <alias|#channel>] [--pr <URL|number>] [--no-push] [--no-notify] [--arg k=v] [--arg-file k=path]
issue review finish ["<message>"] [--to <alias|#channel>] [--pr <n>] [--arg k=v] [--arg-file k=path]
issue event <setup|start|pr_open> [ID|URL]
```

## `setup` — start an issue

Creates a worktree off the baseline ref, symlinks env files, runs the per-app setup commands, adds `.devkit/` (the per-worktree record and cache directory) to the global gitignore, and prints a JSON summary to stdout. An agent's stdout is not a terminal, so JSON is what you get; a person at a terminal sees the same fields as a table.

```json
{ "issue": "ENG-123", "worktree": "/abs/path/to/worktree", "branch": "lev/eng-123-fix-auth" }
```

Read `worktree` to know where to `cd`. Under `--summary` the object carries a fourth key, `summary`, holding the summary file's path.

Setup reserves no ports: `devrun up` allocates them when the worktree's servers start. A fresh worktree has no diff to auto-detect from, so name apps explicitly: `devrun up web api`.

The branch is created with no upstream. git would otherwise track the baseline's remote branch (`origin/main`), where a plain `git push` refuses on the name mismatch. With `push.autoSetupRemote` set, the first push creates `origin/<branch>` and tracks it; without it, push with `-u origin <branch>`. Each `[hooks] after_worktree_create` command runs last, after the result is printed, and a failing hook only warns.

`{{ short_slug }}` is the slug shortened again to `templates.worktree_dir_max`, for a `worktree_dir` template that must stay shorter than the branch. On Windows that keeps paths under the 260-character limit other tools still enforce. It shortens an explicit `--slug` too.

| Flag | Meaning |
|---|---|
| `<ID>` / `--issue <ID>` | Issue id or URL the tracker recognises — a Linear `ENG-123` or `linear.app` URL, a GitHub issue number or an issue URL in the project's `issues_repo`. Drives the branch name and summary. Omit it for work with no tracker issue: `--slug` is then required, `issue` is left out of the output, templates see an empty `{{ issue }}`, and `--summary` is refused. `issue status` shows the issue as `NONE`, and `issue end` finishes it on a merged PR and a clean tree. |
| `--slug <slug>` | Short kebab slug rendered into the branch and worktree dir name (`lev/eng-123-<slug>`). Omit it and the slug comes from a pasted Linear URL's own `…/issue/<ID>/<title-slug>` path, else from the issue's title as the tracker reports it, which needs that tracker's credential. A leading copy of the issue id is stripped so the branch does not repeat it. A *derived* slug is then shortened on a word boundary to fit the 46-char width `issue status` prints — the budget is measured against your own `branch` template, so a longer `branch_prefix` takes from the slug. A slug you pass is used verbatim, however long. |
| `--apps <a,b>` | Comma-separated apps to bootstrap: writes each one's prep files, runs its setup commands. Omit for a worktree with no per-app setup. |
| `--summary` | Also write a markdown summary file. A tracker that keeps its own summary supplies the file verbatim: under GitHub that is the issue body, so a handoff written into the issue comes back down as the summary. Otherwise, and for a GitHub issue with an empty body, the file is the issue's tracker facts (url, parent, project, state, assignee, priority, estimate, labels) and its description verbatim, then empty `## Summary` and `## Pointers` headings to fill in. A tracker with no equivalent of a field leaves it empty, as GitHub does for parent, project, priority, and estimate. Default path `ISSUE_SUMMARY_<ID>.md` under `worktree_root`, beside the worktree so it survives `git worktree remove`; `templates.issue_summary_path` and `templates.issue_summary` override placement and body. Needs the tracker's credential. An existing file is left byte-for-byte and its path still reported. The fetch runs before the worktree is created, so an unknown issue fails clean. `issue end` removes the recorded file when it cleans the worktree up. `defaults.issue_summary = true` makes this the default. Under `--dry-run` the resolved path is reported without the file being written. |
| `--no-summary` | Skip the summary file for this run, whatever `defaults.issue_summary` says. |

## `render` and `create`: file an issue

Issue titles and bodies come from the `issue_title` and `issue_body` templates. `{{ input }}` is `--title` or `--body`, and the body template also sees the rendered `issue_title`. A `[templates.variables]` entry either template reads and marks `required` must be passed as `--arg`. A missing one is refused by name, with its description.

- **GitHub:** `issue create` renders and runs `gh issue create` against `issues_repo`, then prints the issue URL. Under any other tracker it refuses and points at `issue render`.
- **A tracker MCP (Linear's `save_issue`, GitHub's `issue_write`):** run `issue render`, which prints `{"title": ..., "body": ...}`, and pass both strings unchanged as the MCP call's title and body fields. Line endings and trailing whitespace may differ, but any other edit is denied as text that differs from the render.
- **Rewriting an existing issue's title or body** goes through `issue render` too. An update that only changes state, labels or assignee needs no render. Linear's `patch` edits the body in place, so it is always refused: render the whole new body and pass it as `description`.

A denied MCP call names `devkit issue render` and the `--arg`s it needs. The render has to happen in the same agent session as the call, because its receipt is filed under the session id. Receipts live under `.devkit/issue-receipts/` in the repository's main worktree, so a render in any worktree of the repository counts, and session end deletes that session's receipts.

Enforcement is configuration. `[harness.issue_tools.<name>]` entries name the MCP servers and tools that write issues and enforce as soon as they exist. `devkit schema` documents their keys. Refusing a bare `gh issue create` takes a `[harness.commands]` rule with `programs = ["gh"]` and `args = ["issue", "create"]`, and like every command rule it needs `[harness] enforce_commands = true`. `issue create` runs its own `gh`, which the rule never sees.

## `pr checkout` — review someone else's work

Checks out an **existing** PR into a new worktree, the review-side counterpart of `setup`. The target is `#3340`, `3340`, an issue id the tracker recognises (`PREFIX-3340`, whose linked PR is used), a PR URL on the project's forge, or an issue URL the tracker recognises.

A bare `3340` is probed against both the PRs and the tracker's issues. A real collision prompts at a terminal and is an error without one. On a GitHub project, where issues and PRs share one numbering, it is always the PR. An issue with no attached PR is an error.

The optional second positional overrides the worktree path (default: `templates.checkout_worktree_dir`, e.g. `3340-fix-login`). The PR's own branch name is kept. `--setup` also runs the per-app setup commands; `--apps a,b` narrows which apps that covers. The worktree gets a `.devkit/issue.toml` record so `issue status` and `issue end` recognise it, and `after_worktree_create` fires with or without `--setup`. Prints `pr`, `worktree`, and `branch`: JSON to a pipe, a table to a terminal.

Where GitHub refuses GraphQL, as in a Claude Code cloud session, the PR's head is fetched from `origin` as `refs/pull/<n>/head` instead of through `gh pr checkout`, and its branch gets no upstream, so pushing to a fork's branch needs that remote added by hand. An issue id finds only a PR whose description closes the issue (`Closes`, `Fixes` or `Resolves`), not one linked only through the issue's Development sidebar.

## `pr create` and `pr ready` — open the PR

`pr create` pushes the branch (**never force-pushes**) and opens its PR, printing the URL; a branch that already has one reuses it and keeps its draft state. `--draft`/`--ready` decide the state for the run, `defaults.pr_create_state` when neither is passed. `pr ready` flips a draft to ready and is a no-op on a PR that is already ready. Both take `--to <alias>` to add reviewers on the forge, and neither posts to Slack.

- A `--draft`/`--ready` that contradicts a reused PR's state is reported as ignored, naming `issue pr ready`, or converting it to a draft on the forge.
- A `--to` alias with no `github` handle warns and is skipped.
- `--pr <URL|number>` acts on that PR and records it, which is how a worktree bound to the wrong PR is rebound. `--no-push` skips the push.
- Whichever PR the run ends on, its head commit must be this worktree's `HEAD`. A reused PR is checked before it is touched, a new one straight after it opens and before any reviewer is requested on it; a failure there leaves the new PR open and says so.
- `pr ready` on a branch with no PR is an error naming `issue pr create`; a merged or closed PR is refused.
- Where GitHub refuses GraphQL, as in a Claude Code cloud session, `pr create` still finds the branch's PR and opens one over REST, ready or draft as asked, but finds no PR on a fork's branch, refuses to open one when `origin` is another repository than the PR repository, and cannot `--attach`. `pr ready` and `review request`'s draft flip need GraphQL, so open the PR ready there.

### `pr render`: the PR text without opening it

`pr render` takes `pr create`'s `--pr-title`, `--pr-body` and template arguments and prints `{"title": ..., "body": ...}`, byte for byte what `pr create` would send from this worktree, issue line included. It touches no forge and runs no proof check. Use it when the PR is opened some other way, and pass both strings unchanged. A required argument missing is refused by name before anything renders. Inside an agent session it records a receipt of each string under `.devkit/pr-receipts/` in the repository's main worktree, kept apart from issue receipts; outside one it says no receipt was written. Session end deletes that session's receipts.

A forge MCP tool that opens or edits PRs (the GitHub MCP server's `create_pull_request` and `update_pull_request`) is gated on those receipts once a `[harness.pr_tools.<name>]` entry names it, the same way `[harness.issue_tools]` gates issue writes: a create, or an update that rewrites the title or body, is denied naming `devkit issue pr render` unless the text matches a render from the same session; an update touching neither is allowed, and an in-place body patch is always denied. An issue render never vouches for a PR, nor a PR render for an issue.

### `--attach`: images and video in the PR body

`pr create --attach <file>[#alt]` uploads an image or video into the body of the PR it opens, through `gh pr create --attach`. It is repeatable, and alt text for an image follows `#`. A markdown image or link in the body whose destination is the same path gets the uploaded URL in its place, and an attachment the body does not reference is appended. A bare path is not a reference: it stays as written, and the file is appended. Embed a video like an image, alone in its paragraph: gh swaps the whole embed for the bare URL, which is what renders as a player. A video embedded mid-paragraph, or written as a plain link without the `!`, stays a link.

```sh
issue pr create --pr-title 'feat(login): show the error state' \
  --attach './.devkit/proof/after.png#The login error state' \
  --attach ./.devkit/proof/demo.mp4 \
  --pr-body '![after](./.devkit/proof/after.png)

![demo](./.devkit/proof/demo.mp4)'
```

- Paths are relative to the working directory, or to `-C` when given. A `#` inside a filename that exists is part of the path, not the start of alt text.
- A missing file, and any forge but GitHub, are refused before the push.
- A branch whose PR already exists is refused before that PR is touched, because a rerun would add another copy of each file. Attach to it with `gh pr edit <n> --attach <file>`.
- It needs gh 2.99.0 or later. An older gh fails with its own error after the push, and no PR is opened.
- gh enforces GitHub's file count, type and size limits. On a private repository only people with access can see the attachments.
- If some uploads fail, gh still opens the PR with the ones that succeeded but exits non-zero, so the run reports a failure. The next `pr create` without `--attach` finds that PR and records it.

`defaults.require_pr_reviewer` refuses any run that would leave a PR ready with no human reviewer other than the PR's own author: `pr create --ready`, `pr ready`, and `review request`'s draft flip. A pending request, a submitted review, or a `--to` in the same run all count; the author's own review does not. The refusal comes before the flip, so the PR stays a draft. Opening a draft is never gated, and neither is a PR that was already ready.

### Proof for every item

When `defaults.pr_proof_variable` names a template variable (say `proof`), an agent's `pr create` in an `issue setup` worktree reads the issue from the tracker and refuses, before the push, a proof that skips an item of its `Done when` section. A `Done when` or `Acceptance criteria` section counts, under a heading or a bold label like `**Acceptance criteria:**`, matched in any case. The refusal lists each missing item with its number.

Items are numbered in the order the issue lists them. Answer item `n` on a line of the proof that starts with `n.`, `n)` or `n:`, so the wording can be yours:

```sh
issue pr create --pr-title 'feat(login): show the error state' --arg proof='1. test `login_error_renders`
2. test `retry_clears_error`
3. `devkit issue pr create -h` prints the new flag'
```

- A numbered line indented two columns or more belongs to the entry above it and answers nothing.
- Not gated: a human caller, a worktree with no issue record, one set up with only `--slug` or checked out from a PR that names no issue, and an issue with no such section.
- An issue the tracker cannot return is refused, since the items cannot be checked.
- The check runs on every agent run, a reused PR included, because it comes before the push that finds the PR. Whether the evidence holds is left to review.

## `review request` — ship for review

Pushes the branch, requests the reviewers on the PR, and Slack-messages them the PR link plus your body. With `$SLACK_TOKEN` set it posts directly; otherwise it emits a `SlackIntent` JSON object for an agent to forward.

It opens no PR: a branch with none is an error naming `issue pr create`. Ship with the two commands in order.

```sh
issue pr create
issue review request "Auth fix ready, please review session handling." --to bob
```

A run that notifies marks a draft ready for review first; `--no-notify` leaves draft state alone.

| Arg / flag | Meaning |
|---|---|
| `[BODY]` | Positional Slack body; fills the `review_request` template's `{{ input }}`. |
| `--to <alias\|#channel>` | **Repeatable.** A `[people]` alias — which carries both `slack` and an optional `github`, so one flag sets reviewer *and* recipient — or a literal `#channel`. |
| `--pr <URL\|number>` | Act on this PR for this run. A pasted PR URL on the project's forge keeps its own repository; a bare number means `[forge] repo`. The command records whichever PR it acted on, so this is how a worktree bound to the wrong PR is rebound, including the superseded case where the old and new PRs share a head branch and the branch lookup is ambiguous. Without it the PR comes from the worktree's record, and failing that from its branch. |
| `--no-notify` | Send no Slack and leave draft state alone. Pins targets to what `--to` resolved to, possibly none, instead of falling back to the PR's current reviewers. |
| `--arg k=v` | **Repeatable.** Override a declared template variable. |
| `--arg-file k=path` | **Repeatable.** The same, from a file's contents, or stdin for `k=-`. |

With no `--to`, it resolves the PR's current human reviewers and notifies them; `--no-notify` suppresses that and prints the PR URL instead. Everything that can refuse the run (recipients, the reviewer gate) is settled before a draft is marked ready, so a run with nobody to notify leaves a draft a draft. The Slack template's `pr_title` is the PR's own title from the forge.

However the PR was resolved, its head commit must equal this worktree's `HEAD` or the command refuses. A branch name is shared across forks and does not prove the PR carries this work. A squash- or rebase-merged PR still matches, since the comparison is against the branch head the PR carries. Under `--no-push`, a branch ahead of its remote fails this check.

## `review finish` — announce you reviewed

Announces over Slack that you finished reviewing. `--to` (repeatable) defaults to the PR author. The PR comes from `--pr <n>`, else the worktree's record, else the branch; `--pr` applies to that run and rewrites nothing.

No head-commit check here: this is the reviewer's command, run in a worktree `pr checkout` built, where `HEAD` falls behind as soon as the author pushes again. `[BODY]` fills the `review_finish` template's `{{ input }}`.

## Status events

`[issue.events]` moves the issue's tracker status when devkit acts on the issue: a Linear workflow state, or on GitHub the single-select field of the Projects v2 project `[github] project` names. Each event moves the issue only from a status its `from` lists, and an issue already at `to` is left alone. With no `[issue.events]`, devkit reads and writes no tracker status. `devkit schema` documents the keys.

| Event | Fires | Where |
|---|---|---|
| `setup` | as `issue setup`'s last step, after the `after_worktree_create` hooks | the worktree it creates, when it names a tracker issue |
| `start` | in the background, when the first agent session starts in the worktree | `issue setup` worktrees only, never `issue pr checkout` ones, so a reviewer's session cannot pull an issue back from review |
| `pr_open` | when `issue pr create` opens the PR or finds one already open | any worktree whose record names a tracker issue |

Each event fires once per worktree. The worktree's `.devkit/issue.toml` records it before the tracker is asked, and a failed move keeps that record, so a tracker that is down costs one missed move rather than a retry on every session. `issue end` removes the record, and the marks go with it. `setup` and `pr_open` print a failed move as a warning and the command still succeeds. The background `start` run appends its outcome or error to `.devkit/issue-event.log` in the worktree.

`issue event <event> [ID|URL]` reruns one event by hand, for the worktree's own issue when no id is given: it retries a failed move or shows its error, whatever the record says. It prints what it did: the move, that the issue is already there, or that its status is not in `from`. `devkit doctor`'s `issue_events` row lists each transition. On GitHub it also checks the project, its status field and every status name, which proves the token can read the project but not that it can write: a token with only `read:project` passes the row and fails on the first move. On Linear it only lists them, since a team's states are known once an issue is read, so an unknown name surfaces on the first move.

On GitHub the token needs the `project` scope. Without it each move fails naming the remedy: `gh auth refresh -s project` for a token `gh` supplies, or reissuing the variable it came from (`GH_TOKEN`, say) with `project` for a token from the environment.

## Triage and teardown

- **`status`** (also the bare `issue`) — read-only triage table of every issue worktree. A worktree is FINISHED only when its PR is merged with nothing committed past the PR's head, its issue has reached a completed state in the tracker, and the tree is clean. A project that *declares* no tracker has no state to wait for and is decided by the merged PR and clean tree alone; a tracker that answers nothing for the issue holds the verdict open instead. An input that cannot be read (a corrupt `.devkit/issue.toml`, a `git status` that fails, a merged PR head this clone never fetched) makes the verdict unknown, which holds the worktree like any other reason. `pr status --json` and the MCP `issue.status` action report it as `"verdict": "unknown"`, beside `"finished"` and `"held"`. Where GitHub refuses GraphQL, each PR and issue state is read over REST instead, and a PR on a fork's branch is found only when the worktree recorded it.
- **`pr status`** (also the bare `issue pr`) — one worktree's PR number and issue id. The optional selector is an issue id, branch, worktree basename, or path; omit it for the current worktree. `--json` emits a single `IssueWorktree` object (scripts read `.pr_number` / `.issue_id`). `--cache-only` skips the network: the PR number comes from `<worktree>/.devkit/pr.json` and the tracker columns render as `—`. A live run writes the PR through to that cache, which `git worktree remove` deletes with the worktree.
- **`end`** — removes FINISHED worktrees. `--pr-only` ignores the tracker-state and issue-id gates (finished = PR merged + clean, even on a branch carrying no issue id); `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree guard, never a tree whose status could not be read; `-y` skips confirmation. A worktree's files are copied out before it is removed, one destination per `[preserve.<name>]` entry, which is how an agent's scratch, notes and session memory outlive the worktree. A copy that fails warns and the removal goes ahead, unless the entry sets `required = true`: then the worktree stays, and the run exits non-zero once every selection is handled. `--no-preserve` skips the copying. Which files go where is configuration, not a flag: `devkit schema` covers the patterns, the destination templates, and the symlink and collision rules. A `devkit.toml` that exists but fails to load makes every run refuse, since `[preserve]` cannot be read; `--no-preserve` is the way through. `[hooks] before_worktree_remove` runs inside each worktree about to go, after the copying and before any removal starts; it is where a session tied to the worktree gets closed. After the removals, `after_worktree_remove` runs per removed worktree, then `after_end` once. A worktree that named a baseline gives it up: servers under a baseline nobody else names are stopped first, and the baseline is reclaimed after the removal, both best-effort. `devrun baseline prune --force` reclaims one whose servers are still running.
- **`sync-includes`** — re-copies the `defaults.worktree_include` files from the primary checkout into worktrees that already exist, the list `setup` and `pr checkout` backfill at creation time. Reach for it when that list gains an entry after a worktree was made. Selectors match as `pr status`'s do; omit them to sync every worktree. The primary checkout is the source and never a target. Files the worktree already has are left alone and named in a warning; `--overwrite` replaces them instead, prompting once per worktree, and declining that prompt still copies what the worktree is missing. Those files are untracked ones git cannot restore, so `--overwrite` needs a scope, either selectors or `--all`, and `-y` answers the prompt (it does nothing without `--overwrite`). `--dry-run` writes nothing. Symlinks are reproduced as links and counted separately. File lists are grouped by top-level directory and show the first few per group; `-v` names every file. It resolves no PR number, so it makes no network call.
- **`prs`**: triage of your open PRs and PRs awaiting your review. The authored, review-requested and reviewed-by searches run concurrently and page to exhaustion, and a per-repo cache renders `old -> new` for anything changed since the last run. The repository is `[forge] repo`, defaulting to the `origin` remote; `-R owner/repo` overrides it for one run. `--no-cache` forces a fresh fetch. On a repo with many open PRs the forge can return HTTP 504: lower `--batch-size` (PRs per search page, 1–100) and raise `--retries` (extra attempts per page with backoff, 0–10).
- **`dashboard`** — the triage and PR tables plus terminal timelines. `--chart bar|line`, `--bucket` (default `auto`), `--mode` (default `absolute`) and `--aggregate cumulative|period` shape the plots; `--all-roles` widens beyond your own, `--author <gh>` targets someone else; `--no-plots` shows only tables, `--no-cache` forces a fresh fetch. Timeline fetches are cached under `~/.cache/devkit/dashboard` for a few minutes; the live triage panel never is.

At a terminal, `issue`, `issue pr status` and `issue prs` render live on stderr: cells fill in with spinners, `prs` shows the last run's tables dimmed until fresh ones swap in, and step-driven commands keep each finished step as a timed line. Piped or redirected output is unaffected.

## Forges

Every PR command talks to one forge: GitHub (github.com or GitHub Enterprise Server), GitLab (gitlab.com or self-managed), or Forgejo and Gitea (codeberg.org or self-hosted). `[forge] kind` picks it, `host` names a self-hosted instance, and `repo` is the repository PRs go to. With no `[forge] kind`, the `origin` host decides: github.com, gitlab.com and codeberg.org are recognized, and any other host means no forge, since a self-hosted instance could be any of them. `devkit doctor`'s `forge` row shows which forge resolved and why.

With no forge, every command that reads or writes a PR (`pr create`, `pr ready`, `pr checkout`, `review`, `prs`) fails and names `[forge] kind`. Worktrees, ports, locks and docs work as before. The finished verdict depends on why there is no forge:

- `kind = "none"` declares the project has no PRs. In place of a merged PR, a worktree needs its commits on a remote-tracking branch, since `issue end` deletes the branch. The PR column reads `pushed` or `unpushed`.
- A forge devkit failed to find holds every worktree unfinished, with the reason in the PR column.

Credentials resolve per forge, from the environment first and then `secrets.toml`:

| Forge | Token | Writes go through |
|---|---|---|
| GitHub | `GH_TOKEN`/`GITHUB_TOKEN` for github.com, `GH_ENTERPRISE_TOKEN`/`GITHUB_ENTERPRISE_TOKEN` for another host, then `gh auth token --hostname <host>` | `gh`, which also serves every read when no token resolves |
| GitLab | `GITLAB_TOKEN` | the REST API |
| Forgejo | `FORGEJO_TOKEN`, then `GITEA_TOKEN` | the REST API |

GitHub reads that batch go over GraphQL, one request per batch. Where GitHub refuses GraphQL outright with HTTP 403, as in a Claude Code cloud session, they fall back to REST, one request per item; each command above says what that costs it. A 403 whose message is GitHub's rate limit is not a refusal: it is reported as an error, with no REST fallback. `DEVKIT_NO_GRAPHQL=1` or `[github] no_graphql = true` skips GraphQL altogether, so every read with a REST path goes straight to it, and `issue prs`, which has none, fails naming the switch. `DEVKIT_NO_GRAPHQL=0` turns GraphQL back on over the config key.

A GitLab personal access token needs the `api` scope. A fine-grained one needs Merge Request create, read and update, Approval Configuration, Project, Pipeline and Job read on the project, and User read on the User tab. A Forgejo token needs access to all repositories, since a token limited to specific ones cannot read the user, with `repository` read and write, `issue` read and `user` read.

GitLab and Forgejo mark a draft with a title prefix (`Draft:` and `WIP:`), which `pr ready` removes. Neither reports line counts cheaply, so `issue dashboard`'s PR additions and deletions stay zero there.

`[github] pr_repo` moved to `[forge] repo`. A config that still carries it loads, and every PR command names the new key.
