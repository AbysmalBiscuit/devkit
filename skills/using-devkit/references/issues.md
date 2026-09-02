# `issue` — issue lifecycle

`issue` acts on the **current working directory's worktree** by default (`-C/--dir <path>` overrides), and `issue review` ships the branch checked out there. `cd` into the right worktree first.

```sh
issue setup <ID|URL> [--slug <slug>] [--apps a,b] [--summary|--no-summary] [--dry-run] [--no-gitignore]
issue status [ids…]                                   # read-only triage (also the bare `issue`)
issue pr [status] [selector] [--json] [--cache-only]  # also the bare `issue pr`
issue pr create [--draft|--ready] [--to <alias>] [--base <branch>] [--pr-title T] [--pr-body B] [--no-push] [--pr <URL|number>] [--arg k=v]
issue pr ready [--to <alias>] [--no-push] [--pr <URL|number>]
issue pr checkout <target> [<worktree-path>] [--setup] [--apps a,b]
issue end [ids…] [-y] [--force] [--pr-only] [--clean-worktree] [--no-preserve]
issue sync-includes [selectors…] [--overwrite [--all]] [-y] [--dry-run]
issue prs [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache] [--batch-size N] [--retries N]
issue dashboard [--chart bar|line] [--bucket B] [--mode M] [--all-roles] [--author gh] [--no-plots] [--no-cache]
issue review request ["<message>"] [--to <alias|#channel>] [--pr <URL|number>] [--no-push] [--no-notify] [--arg k=v]
issue review finish ["<message>"] [--to <alias|#channel>] [--pr <n>] [--arg k=v]
```

## `setup` — start an issue

Creates a worktree off the baseline ref, symlinks env files, runs the per-app setup commands, adds `.devkit/` (the per-worktree record and cache directory) to the global gitignore, and prints a JSON summary to stdout. An agent's stdout is not a terminal, so JSON is what you get; a person at a terminal sees the same fields as a table.

```json
{ "issue": "ENG-123", "worktree": "/abs/path/to/worktree", "branch": "lev/eng-123-fix-auth" }
```

Read `worktree` to know where to `cd`. Under `--summary` the object carries a fourth key, `summary`, holding the summary file's path.

Setup reserves no ports — `devrun up` allocates them when the worktree's servers start. A fresh worktree has no diff to auto-detect from, so name apps explicitly: `devrun up web api`.

| Flag | Meaning |
|---|---|
| `<ID>` / `--issue <ID>` | Issue id or URL the tracker recognises — a Linear `ENG-123` or `linear.app` URL, a GitHub issue number or an issue URL in the project's `issues_repo`. Drives the branch name and summary. **Required.** |
| `--slug <slug>` | Short kebab slug rendered into the branch and worktree dir name (`lev/eng-123-<slug>`). Omit it and the slug comes from a pasted Linear URL's own `…/issue/<ID>/<title-slug>` path, else from the issue's title as the tracker reports it, which needs that tracker's credential. A leading copy of the issue id is stripped so the branch does not repeat it. A *derived* slug is then shortened on a word boundary to fit the 46-char width `issue status` prints — the budget is measured against your own `branch` template, so a longer `branch_prefix` takes from the slug. A slug you pass is used verbatim, however long. |
| `--apps <a,b>` | Comma-separated apps to bootstrap: writes each one's prep files, runs its setup commands. Omit for a worktree with no per-app setup. |
| `--summary` | Also write a markdown summary file: the issue's tracker facts (url, parent, project, state, assignee, priority, estimate, labels) and its description verbatim, then empty `## Summary` and `## Pointers` headings to fill in. A tracker with no equivalent of a field leaves it empty, as GitHub does for parent, project, priority, and estimate. Default path `ISSUE_SUMMARY_<ID>.md` under `worktree_root`, beside the worktree so it survives `git worktree remove`; `templates.issue_summary_path` and `templates.issue_summary` override placement and body. Needs the tracker's credential. An existing file is left byte-for-byte and its path still reported. The fetch runs before the worktree is created, so an unknown issue fails clean. `issue end` removes the recorded file when it cleans the worktree up. `defaults.issue_summary = true` makes this the default. Under `--dry-run` the resolved path is reported without the file being written. |
| `--no-summary` | Skip the summary file for this run, whatever `defaults.issue_summary` says. |

## `pr checkout` — review someone else's work

Checks out an **existing** PR into a new worktree, the review-side counterpart of `setup`. The target is `#3340`, `3340`, an issue id the tracker recognises (`PREFIX-3340`, whose linked PR is used), a GitHub PR URL, or an issue URL the tracker recognises.

A bare `3340` is probed against both the PRs and the tracker's issues, so on a GitHub project — where issues and PRs share one numbering — it is always the PR.

The optional second positional overrides the worktree path (default: the config-resolved placement). `--setup` also runs the per-app setup commands; `--apps a,b` narrows which apps that covers. Prints `pr`, `worktree`, and `branch` — JSON to a pipe, a table to a terminal.

## `pr create` and `pr ready` — open the PR

`pr create` pushes the branch (**never force-pushes**) and opens its PR, printing the URL; a branch that already has one reuses it and keeps its draft state. `--draft`/`--ready` decide the state for the run, `defaults.pr_create_state` when neither is passed. `pr ready` flips a draft to ready and is a no-op on a PR that is already ready. Both take `--to <alias>` to add GitHub reviewers, and neither posts to Slack.

`defaults.require_pr_reviewer` refuses any run that would leave a PR ready with no human reviewer other than the PR's own author: `pr create --ready`, `pr ready`, and `review request`'s draft flip. A draft is never gated.

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
| `--pr <URL\|number>` | Act on this PR for this run. A pasted GitHub PR URL keeps its own repository; a bare number means `pr_repo`. The command records whichever PR it acted on, so this is how a worktree bound to the wrong PR is rebound, including the superseded case where the old and new PRs share a head branch and the branch lookup is ambiguous. Without it the PR comes from the worktree's record, and failing that from its branch. |
| `--no-notify` | Send no Slack and leave draft state alone. Pins targets to what `--to` resolved to, possibly none, instead of falling back to the PR's current reviewers. |
| `--arg k=v` | **Repeatable.** Override a declared template variable. |

With no `--to`, it resolves the PR's current human reviewers and notifies them; `--no-notify` suppresses that.

However the PR was resolved, its head commit must equal this worktree's `HEAD` or the command refuses. A branch name is shared across forks and does not prove the PR carries this work. A squash- or rebase-merged PR still matches, since the comparison is against the branch head the PR carries. Under `--no-push`, a branch ahead of its remote fails this check.

## `review finish` — announce you reviewed

Announces over Slack that you finished reviewing. `--to` (repeatable) defaults to the PR author. The PR comes from `--pr <n>`, else the worktree's record, else the branch; `--pr` applies to that run and rewrites nothing.

No head-commit check here: this is the reviewer's command, run in a worktree `pr checkout` built, where `HEAD` falls behind as soon as the author pushes again. `[BODY]` fills the `review_finish` template's `{{ input }}`.

## Triage and teardown

- **`status`** (also the bare `issue`) — read-only triage table of every issue worktree. A worktree is FINISHED only when its PR is merged, its issue has reached a completed state in the tracker, and the tree is clean. A project that *declares* no tracker has no state to wait for and is decided by the merged PR and clean tree alone; a tracker that answers nothing for the issue holds the verdict open instead.
- **`pr status`** (also the bare `issue pr`) — one worktree's PR number and issue id. The optional selector is an issue id, branch, worktree basename, or path; omit it for the current worktree. `--json` emits a single `IssueWorktree` object (scripts read `.pr_number` / `.issue_id`). `--cache-only` skips the network: the PR number comes from `<worktree>/.devkit/pr.json` and the tracker columns render as `—`. A live run writes the PR through to that cache, which `git worktree remove` deletes with the worktree.
- **`end`** — removes FINISHED worktrees. `--pr-only` ignores the tracker-state and issue-id gates (finished = PR merged + clean, even on a branch carrying no issue id); `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree guard; `-y` skips confirmation. A worktree's files are copied out before it is removed, one destination per `[preserve.<name>]` entry in the config — this is what keeps an agent's scratch, notes, or session memory once the worktree is gone. A copy that fails warns and the removal proceeds anyway, unless that entry sets `required = true`, which keeps the worktree instead. `--no-preserve` skips the copying. Which files go where is configuration, not a flag: `docs/configuration.md` covers the patterns, the destination templates, and the symlink and collision rules.
- **`sync-includes`** — re-copies the `defaults.worktree_include` files from the primary checkout into worktrees that already exist, the list `setup` and `pr checkout` backfill at creation time. Reach for it when that list gains an entry after a worktree was made. Selectors match as `pr status`'s do; omit them to sync every worktree. The primary checkout is the source and never a target. Files the worktree already has are left alone and named in a warning; `--overwrite` replaces them instead, prompting once per worktree, and declining that prompt still copies what the worktree is missing. Those files are untracked ones git cannot restore, so `--overwrite` needs a scope — one or more selectors, or `--all` — and `-y` answers the prompt (it does nothing without `--overwrite`). `--dry-run` writes nothing.
- **`prs`** — GitHub PR triage of your open PRs and PRs awaiting your review. The repository is `[github] pr_repo`, defaulting to the `origin` remote; `-R owner/repo` overrides it for one run. `--no-cache` forces a fresh fetch. On a repo with many open PRs GitHub can return HTTP 504: lower `--batch-size` (PRs per search page, 1–100) and raise `--retries` (extra attempts per page with backoff, 0–10).
- **`dashboard`** — the triage and PR tables plus terminal timelines. `--chart bar|line`, `--bucket` (default `auto`) and `--mode` (default `absolute`) shape the plots; `--all-roles` widens beyond your own, `--author <gh>` targets someone else; `--no-plots` shows only tables, `--no-cache` forces a fresh fetch.
