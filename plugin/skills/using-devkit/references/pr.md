# `devkit pr`: open, ship and review a workspace's PR

`devkit pr` acts on the PR of the **current working directory's worktree** by default (`-C/--dir <path>` overrides), and `devkit pr review request` ships the branch checked out there. `cd` into the right worktree first. `-C` and `--config` go on `pr` itself, before the verb, or on the verb. `pr` has no short name of its own, since coreutils owns `pr`; `ticket pr <verb>` runs the same verbs.

```sh
devkit pr [status] [selector] [--json] [--cache-only]  # also the bare `devkit pr`
devkit pr create [--draft|--ready] [--to <alias>] [--base <branch>] [--pr-title T] [--pr-body B] [--attach <file>[#alt]] [--no-push] [--pr <URL|number>] [--arg k=v] [--arg-file k=path]
devkit pr render [--pr-title T] [--pr-body B] [--arg k=v] [--arg-file k=path]
devkit pr ready [--to <alias>] [--no-push] [--pr <URL|number>]
devkit pr checkout <target> [<worktree-path>] [--setup] [--apps a,b]
devkit pr list [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache] [--batch-size N] [--retries N]
devkit pr review request ["<message>"] [--to <alias|#channel>] [--pr <URL|number>] [--no-push] [--no-notify] [--arg k=v] [--arg-file k=path]
devkit pr review finish ["<message>"] [--to <alias|#channel>] [--pr <n>] [--arg k=v] [--arg-file k=path]
```

## `checkout`: review someone else's work

Checks out an **existing** PR into a new worktree, the review-side counterpart of `workspace setup`. The target is `#3340`, `3340`, an issue id the tracker recognises (`PREFIX-3340`, whose linked PR is used), a PR URL on the project's forge, or an issue URL the tracker recognises.

A bare `3340` is probed against both the PRs and the tracker's issues. A real collision prompts at a terminal and is an error without one. On a GitHub project, where issues and PRs share one numbering, it is always the PR. An issue with no attached PR is an error.

The optional second positional overrides the worktree path (default: `templates.checkout_worktree_dir`, e.g. `3340-fix-login`). The PR's own branch name is kept. `--setup` also runs the per-app setup commands; `--apps a,b` narrows which apps that covers. The worktree gets a `.devkit/issue.toml` record so `workspace status` and `workspace end` recognise it, and `after_worktree_create` fires with or without `--setup`. Prints `pr`, `worktree`, and `branch`: JSON to a pipe, a table to a terminal.

Where GitHub refuses GraphQL, as in a Claude Code cloud session, the PR's head is fetched from `origin` as `refs/pull/<n>/head` instead of through `gh pr checkout`, and its branch gets no upstream, so pushing to a fork's branch needs that remote added by hand. A local branch with the head's name at any other commit is left alone and the checkout fails naming it, since it may hold unpushed work. An issue id finds only a PR whose description closes the issue (`Closes`, `Fixes` or `Resolves`), not one linked only through the issue's Development sidebar, and it accepts such a PR into any base branch, where GraphQL counts only PRs into the default branch.

## `create` and `ready`: open the PR

`create` pushes the branch (**never force-pushes**) and opens its PR, printing the URL; a branch that already has one reuses it and keeps its draft state. `--draft`/`--ready` decide the state for the run, `defaults.pr_create_state` when neither is passed. `ready` flips a draft to ready and is a no-op on a PR that is already ready. Both take `--to <alias>` to add reviewers on the forge, and neither posts to Slack.

- A `--draft`/`--ready` that contradicts a reused PR's state is reported as ignored, naming `devkit pr ready`, or converting it to a draft on the forge.
- A `--to` alias with no `github` handle warns and is skipped.
- `--pr <URL|number>` acts on that PR and records it, which is how a worktree bound to the wrong PR is rebound. `--no-push` skips the push.
- Whichever PR the run ends on, its head commit must be this worktree's `HEAD`. A reused PR is checked before it is touched, a new one straight after it opens and before any reviewer is requested on it; a failure there leaves the new PR open and says so.
- `ready` on a branch with no PR is an error naming `devkit pr create`; a merged or closed PR is refused.
- Where GitHub refuses GraphQL, as in a Claude Code cloud session, `create` still finds the branch's PR and opens one over REST, ready or draft as asked, but finds no PR on a fork's branch, refuses to open one when `origin` is another repository than the PR repository, and cannot `--attach`. `ready` and `review request`'s draft flip need GraphQL, so open the PR ready there.

### `render`: the PR text without opening it

`render` takes `create`'s `--pr-title`, `--pr-body` and template arguments and prints `{"title": ..., "body": ...}`, byte for byte what `create` would send from this worktree, issue line included. It touches no forge and runs no proof check. Use it when the PR is opened some other way, and pass both strings unchanged. A required argument missing is refused by name before anything renders. Inside an agent session it records a receipt of each string under `.devkit/pr-receipts/` in the repository's main worktree, kept apart from ticket receipts; outside one it says no receipt was written. Session end deletes that session's receipts.

A forge MCP tool that opens or edits PRs (the GitHub MCP server's `create_pull_request` and `update_pull_request`) is gated on those receipts once a `[harness.pr_tools.<name>]` entry names it, the same way `[harness.issue_tools]` gates ticket writes (`references/ticket.md`): a create, or an update that rewrites the title or body, is denied naming `devkit pr render` unless the text matches a render from the same session; an update touching neither is allowed, and an in-place body patch is always denied. A ticket render never vouches for a PR, nor a PR render for a ticket.

### `--attach`: images and video in the PR body

`create --attach <file>[#alt]` uploads an image or video into the body of the PR it opens, through `gh pr create --attach`. It is repeatable, and alt text for an image follows `#`. A markdown image or link in the body whose destination is the same path gets the uploaded URL in its place, and an attachment the body does not reference is appended. A bare path is not a reference: it stays as written, and the file is appended. Embed a video like an image, alone in its paragraph: gh swaps the whole embed for the bare URL, which is what renders as a player. A video embedded mid-paragraph, or written as a plain link without the `!`, stays a link.

```sh
devkit pr create --pr-title 'feat(login): show the error state' \
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
- If some uploads fail, gh still opens the PR with the ones that succeeded but exits non-zero, so the run reports a failure. The next `create` without `--attach` finds that PR and records it.

`defaults.require_pr_reviewer` refuses any run that would leave a PR ready with no human reviewer other than the PR's own author: `create --ready`, `ready`, and `review request`'s draft flip. A pending request, a submitted review, or a `--to` in the same run all count; the author's own review does not. The refusal comes before the flip, so the PR stays a draft. Opening a draft is never gated, and neither is a PR that was already ready.

### Proof for every item

When `defaults.pr_proof_variable` names a template variable (say `proof`), an agent's `create` in a `workspace setup` worktree reads the issue from the tracker and refuses, before the push, a proof that skips an item of its `Done when` section. A `Done when` or `Acceptance criteria` section counts, under a heading or a bold label like `**Acceptance criteria:**`, matched in any case. The refusal lists each missing item with its number.

Items are numbered in the order the issue lists them. Answer item `n` on a line of the proof that starts with `n.`, `n)` or `n:`, so the wording can be yours:

```sh
devkit pr create --pr-title 'feat(login): show the error state' --arg proof='1. test `login_error_renders`
2. test `retry_clears_error`
3. `devkit pr create -h` prints the new flag'
```

- A numbered line indented two columns or more belongs to the entry above it and answers nothing.
- Not gated: a human caller, a worktree with no issue record, one set up with only `--slug` or checked out from a PR that names no issue, and an issue with no such section.
- An issue the tracker cannot return is refused, since the items cannot be checked.
- The check runs on every agent run, a reused PR included, because it comes before the push that finds the PR. Whether the evidence holds is left to review.

## `review request`: ship for review

Pushes the branch, requests the reviewers on the PR, and Slack-messages them the PR link plus your body. With `$SLACK_TOKEN` set it posts directly; otherwise it emits a `SlackIntent` JSON object for an agent to forward.

It opens no PR: a branch with none is an error naming `devkit pr create`. Ship with the two commands in order.

```sh
devkit pr create
devkit pr review request "Auth fix ready, please review session handling." --to bob
```

A run that notifies marks a draft ready for review first; `--no-notify` leaves draft state alone.

| Arg / flag | Meaning |
|---|---|
| `[BODY]` | Positional Slack body; fills the `review_request` template's `{{ input }}`. |
| `--to <alias\|#channel>` | **Repeatable.** A `[people]` alias, which carries both `slack` and an optional `github`, so one flag sets reviewer *and* recipient, or a literal `#channel`. |
| `--pr <URL\|number>` | Act on this PR for this run. A pasted PR URL on the project's forge keeps its own repository; a bare number means `[forge] repo`. The command records whichever PR it acted on, so this is how a worktree bound to the wrong PR is rebound, including the superseded case where the old and new PRs share a head branch and the branch lookup is ambiguous. Without it the PR comes from the worktree's record, and failing that from its branch. |
| `--no-notify` | Send no Slack and leave draft state alone. Pins targets to what `--to` resolved to, possibly none, instead of falling back to the PR's current reviewers. |
| `--arg k=v` | **Repeatable.** Override a declared template variable. |
| `--arg-file k=path` | **Repeatable.** The same, from a file's contents, or stdin for `k=-`. |

With no `--to`, it resolves the PR's current human reviewers and notifies them; `--no-notify` suppresses that and prints the PR URL instead. Everything that can refuse the run (recipients, the reviewer gate) is settled before a draft is marked ready, so a run with nobody to notify leaves a draft a draft. The Slack template's `pr_title` is the PR's own title from the forge.

However the PR was resolved, its head commit must equal this worktree's `HEAD` or the command refuses. A branch name is shared across forks and does not prove the PR carries this work. A squash- or rebase-merged PR still matches, since the comparison is against the branch head the PR carries. Under `--no-push`, a branch ahead of its remote fails this check.

## `review finish`: announce you reviewed

Announces over Slack that you finished reviewing. `--to` (repeatable) defaults to the PR author. The PR comes from `--pr <n>`, else the worktree's record, else the branch; `--pr` applies to that run and rewrites nothing.

No head-commit check here: this is the reviewer's command, run in a worktree `checkout` built, where `HEAD` falls behind as soon as the author pushes again. `[BODY]` fills the `review_finish` template's `{{ input }}`.

## Triage

- **`status`** (also the bare `devkit pr`): one worktree's PR number and issue id. The optional selector is an issue id, branch, worktree basename, or path; omit it for the current worktree. `--json` emits a single `IssueWorktree` object (scripts read `.pr_number` / `.issue_id`). `--cache-only` skips the network: the PR number comes from `<worktree>/.devkit/pr.json` and the tracker columns render as a placeholder dash. A live run writes the PR through to that cache, which `git worktree remove` deletes with the worktree.
- **`list`**: triage of your open PRs and PRs awaiting your review. The authored, review-requested and reviewed-by searches run concurrently and page to exhaustion, and a per-repo cache renders `old -> new` for anything changed since the last run. The repository is `[forge] repo`, defaulting to the `origin` remote; `-R owner/repo` overrides it for one run. `--no-cache` forces a fresh fetch. On a repo with many open PRs the forge can return HTTP 504: lower `--batch-size` (PRs per search page, 1 to 100) and raise `--retries` (extra attempts per page with backoff, 0 to 10).

At a terminal, `status` and `list` render live on stderr: cells fill in with spinners, and `list` shows the last run's tables dimmed until fresh ones swap in. Piped or redirected output is unaffected.

## Forges

Every PR command talks to one forge: GitHub (github.com or GitHub Enterprise Server), GitLab (gitlab.com or self-managed), or Forgejo and Gitea (codeberg.org or self-hosted). `[forge] kind` picks it, `host` names a self-hosted instance, and `repo` is the repository PRs go to. With no `[forge] kind`, the `origin` host decides: github.com, gitlab.com and codeberg.org are recognized, and any other host means no forge, since a self-hosted instance could be any of them. `devkit doctor`'s `forge` row shows which forge resolved and why.

With no forge, every command that reads or writes a PR (`create`, `ready`, `checkout`, `review`, `list`) fails and names `[forge] kind`. Worktrees, ports, locks and docs work as before. How `workspace status` decides a finished verdict without one is in `references/workspace.md`.

Credentials resolve per forge, from the environment first and then `secrets.toml`:

| Forge | Token | Writes go through |
|---|---|---|
| GitHub | `GH_TOKEN`/`GITHUB_TOKEN` for github.com, `GH_ENTERPRISE_TOKEN`/`GITHUB_ENTERPRISE_TOKEN` for another host, then `gh auth token --hostname <host>` | `gh`, which also serves every read when no token resolves |
| GitLab | `GITLAB_TOKEN` | the REST API |
| Forgejo | `FORGEJO_TOKEN`, then `GITEA_TOKEN` | the REST API |

GitHub reads that batch go over GraphQL, one request per batch. Where GitHub refuses GraphQL outright with HTTP 403, as in a Claude Code cloud session, they fall back to REST, one request per item; each command above says what that costs it. A 403 whose message is GitHub's rate limit is not a refusal: it is reported as an error, with no REST fallback. `DEVKIT_NO_GRAPHQL=1` or `[github] no_graphql = true` skips GraphQL altogether, so every read with a REST path goes straight to it, and `devkit pr list`, which has none, fails naming the switch. `DEVKIT_NO_GRAPHQL=0` turns GraphQL back on over the config key.

A GitLab personal access token needs the `api` scope. A fine-grained one needs Merge Request create, read and update, Approval Configuration, Project, Pipeline and Job read on the project, and User read on the User tab. A Forgejo token needs access to all repositories, since a token limited to specific ones cannot read the user, with `repository` read and write, `issue` read and `user` read.

GitLab and Forgejo mark a draft with a title prefix (`Draft:` and `WIP:`), which `ready` removes. Neither reports line counts cheaply, so `ticket dashboard`'s PR additions and deletions stay zero there.

`[github] pr_repo` moved to `[forge] repo`. A config that still carries it loads, and every PR command names the new key.
