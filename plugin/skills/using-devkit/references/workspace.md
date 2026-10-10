# `workspace`: set up, report on and retire a workspace

`workspace` (`devkit workspace`) acts on the **current working directory's worktree** by default (`-C/--dir <path>` overrides). `cd` into the right worktree first. `-C` and `--config` go on `workspace` itself, before the verb, or on the verb.

Every verb works from the primary checkout, which git resolves: the main worktree when you stand in a linked one, otherwise the checkout root. The directory's name does not matter.

```sh
workspace setup <ID|URL> [--slug <slug>] [--apps a,b] [--summary|--no-summary] [--dry-run] [--no-gitignore]
workspace setup --slug <slug> [--apps a,b] [--dry-run] [--no-gitignore]
workspace setup --here <ID|URL> [--slug <slug>] [--summary|--no-summary] [--dry-run] [--no-gitignore]
workspace status [ids...]                                 # read-only triage (also the bare `workspace`)
workspace end [ids...] [-y] [--force] [--pr-only] [--clean-worktree] [--no-preserve]
workspace sync-includes [selectors...] [--overwrite [--all]] [-y] [--dry-run]
```

The workspace's PR is `devkit pr`'s (`references/pr.md`), and the tracker ticket it may close is `ticket`'s (`references/ticket.md`).

## `setup`: start a workspace

Creates a worktree off the baseline ref, symlinks env files, runs the per-app setup commands, adds `.devkit/` (the per-worktree record and cache directory), `*.local` and `*.local.*` (local config layers such as `devkit.local.toml`) to the global gitignore, as `devkit install` does, and prints a JSON summary to stdout. An agent's stdout is not a terminal, so JSON is what you get; a person at a terminal sees the same fields as a table.

```json
{ "issue": "ENG-123", "worktree": "/abs/path/to/worktree", "branch": "lev/eng-123-fix-auth" }
```

Read `worktree` to know where to `cd`. Under `--summary` the object carries a fourth key, `summary`, holding the summary file's path. Under an explicit `--summary` with `--dry-run` it also carries `summary_text`, the text a fresh summary file gets (a real run leaves an existing file as it is), so a launcher can take an issue's handoff without creating a worktree; no file is written.

### Binding the checkout you are in

Where you must work in the checkout you were given, on the branch it has, and may create no worktree, `workspace setup --here <ID>` binds that checkout to the issue instead. It writes the issue record naming the issue and the branch checked out, and the summary under `--summary`, so `devkit pr create` renders the issue's closing line as it would in a worktree. It creates no branch or worktree, runs no `after_worktree_create` hooks, and takes no `--apps`. It refuses on the default branch, which `origin/HEAD` or `defaults.baseline_ref` names, on a detached HEAD, in a checkout already bound to a different issue, and in one `devkit pr checkout` made for review. Run again for the same issue, it refreshes the record. The JSON's `worktree` is the checkout itself. The binding holds for that branch alone: switched to another branch, the checkout's record is ignored, so `devkit pr create` renders no closing line for the issue, and `devkit pr status`, `ticket event` and the `start` event pass it over until the bound branch is checked out again.

Setup reserves no ports: `devrun up` allocates them when the worktree's servers start. A fresh worktree has no diff to auto-detect from, so name apps explicitly: `devrun up web api`.

The branch is created with no upstream. git would otherwise track the baseline's remote branch (`origin/main`), where a plain `git push` refuses on the name mismatch. With `push.autoSetupRemote` set, the first push creates `origin/<branch>` and tracks it; without it, push with `-u origin <branch>`. Each `[hooks] after_worktree_create` command runs last, after the result is printed, and a failing hook only warns.

`{{ short_slug }}` is the slug shortened again to `templates.worktree_dir_max`, for a `worktree_dir` template that must stay shorter than the branch. On Windows that keeps paths under the 260-character limit other tools still enforce. It shortens an explicit `--slug` too.

| Flag | Meaning |
|---|---|
| `<ID>` / `--issue <ID>` | Issue id or URL the tracker recognises: a Linear `ENG-123` or `linear.app` URL, a GitHub issue number or an issue URL in the project's `issues_repo`. Drives the branch name and summary. Omit it for work with no tracker issue: `--slug` is then required, `issue` is left out of the output, templates see an empty `{{ issue }}`, and `--summary` is refused. `workspace status` shows the issue as `NONE`, and `workspace end` finishes it on a merged PR and a clean tree. |
| `--slug <slug>` | Short kebab slug rendered into the branch and worktree dir name (`lev/eng-123-<slug>`). Omit it and the slug comes from a pasted Linear URL's own `.../issue/<ID>/<title-slug>` path, else from the issue's title as the tracker reports it, which needs that tracker's credential. A leading copy of the issue id is stripped so the branch does not repeat it. A *derived* slug is then shortened on a word boundary to fit the 46-char width `workspace status` prints. The budget is measured against your own `branch` template, so a longer `branch_prefix` takes from the slug. A slug you pass is used verbatim, however long. |
| `--apps <a,b>` | Comma-separated apps to bootstrap: writes each one's prep files, runs its setup commands. Omit for a worktree with no per-app setup. |
| `--summary` | Also write a markdown summary file. A tracker that keeps its own summary supplies the file verbatim: under GitHub that is the issue body, so a handoff written into the issue comes back down as the summary. Otherwise, and for a GitHub issue with an empty body, the file is the issue's tracker facts (url, parent, project, state, assignee, priority, estimate, labels) and its description verbatim, then empty `## Summary` and `## Pointers` headings to fill in. A tracker with no equivalent of a field leaves it empty, as GitHub does for parent, project, priority, and estimate. Default path `.devkit/ISSUE_SUMMARY_<ID>.md` inside the worktree (with `--here`, the checkout), untracked like the rest of `.devkit/`, so it goes when the worktree is removed unless a `[preserve]` pattern copies it out; `templates.issue_summary_path` and `templates.issue_summary` override placement and body, and a relative path there is taken from `worktree_root`, beside the worktree, and refused where there is none. Needs the tracker's credential. An existing file is left byte-for-byte and its path still reported. The fetch runs before the worktree is created, so an unknown issue fails clean. `workspace end` removes the recorded file when it cleans the worktree up. `defaults.issue_summary = true` makes this the default. Under `--dry-run` the resolved path is reported without the file being written. Only an explicit `--summary` together with `--dry-run` adds `summary_text`, the text a fresh file gets; `defaults.issue_summary = true` with a plain `--dry-run` reports `summary` but no `summary_text`. |
| `--no-summary` | Skip the summary file for this run, whatever `defaults.issue_summary` says. |

`devkit pr checkout` is the review-side counterpart: it builds a workspace around someone else's PR, and `workspace status`, `workspace end` and `workspace sync-includes` treat it like any other.

## Triage and teardown

- **`status`** (also the bare `workspace`): read-only triage table of every workspace. A worktree is FINISHED only when its PR is merged with nothing committed past the PR's head, its issue has reached a completed state in the tracker, and the tree is clean. A project that *declares* no tracker has no state to wait for and is decided by the merged PR and clean tree alone; a tracker that answers nothing for the issue holds the verdict open instead. An input that cannot be read (a corrupt `.devkit/issue.toml`, a `git status` that fails, a merged PR head this clone never fetched) makes the verdict unknown, which holds the worktree like any other reason. `devkit pr status --json` and the MCP `issue.status` action report it as `"verdict": "unknown"`, beside `"finished"` and `"held"`. Where GitHub refuses GraphQL, each PR and issue state is read over REST instead, and a PR on a fork's branch is found only when the worktree recorded it.
- **`end`**: removes FINISHED worktrees. `--pr-only` ignores the tracker-state and issue-id gates (finished = PR merged + clean, even on a branch carrying no issue id); `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree guard, never a tree whose status could not be read; `-y` skips confirmation. A worktree's files are copied out before it is removed, one destination per `[preserve.<name>]` entry, which is how an agent's scratch, notes and session memory outlive the worktree. A copy that fails warns and the removal goes ahead, unless the entry sets `required = true`: then the worktree stays, and the run exits non-zero once every selection is handled. `--no-preserve` skips the copying. Which files go where is configuration, not a flag: `devkit schema` covers the patterns, the destination templates, and the symlink and collision rules. A `devkit.toml` that exists but fails to load makes every run refuse, since `[preserve]` cannot be read; `--no-preserve` is the way through. `[hooks] before_worktree_remove` runs inside each worktree about to go, after the copying and before any removal starts; it is where a session tied to the worktree gets closed. After the removals, `after_worktree_remove` runs per removed worktree, then `after_end` once. A worktree that named a baseline gives it up: servers under a baseline nobody else names are stopped first, and the baseline is reclaimed after the removal, both best-effort. `devrun baseline prune --force` reclaims one whose servers are still running.
- **`sync-includes`**: re-copies the `defaults.worktree_include` files from the primary checkout into worktrees that already exist, the list `setup` and `devkit pr checkout` backfill at creation time. Reach for it when that list gains an entry after a worktree was made. Selectors match as `devkit pr status`'s do; omit them to sync every worktree. The primary checkout is the source and never a target. Files the worktree already has are left alone and named in a warning; `--overwrite` replaces them instead, prompting once per worktree, and declining that prompt still copies what the worktree is missing. Those files are untracked ones git cannot restore, so `--overwrite` needs a scope, either selectors or `--all`, and `-y` answers the prompt (it does nothing without `--overwrite`). `--dry-run` writes nothing. Symlinks are reproduced as links and counted separately. File lists are grouped by top-level directory and show the first few per group; `-v` names every file. It resolves no PR number, so it makes no network call.

At a terminal, `workspace status` renders live on stderr: cells fill in with spinners, and step-driven commands keep each finished step as a timed line. Piped or redirected output is unaffected.

With no forge (`references/pr.md`), the finished verdict depends on why there is none:

- `kind = "none"` declares the project has no PRs. In place of a merged PR, a worktree needs its commits on a remote-tracking branch, since `workspace end` deletes the branch. The PR column reads `pushed` or `unpushed`.
- A forge devkit failed to find holds every worktree unfinished, with the reason in the PR column.
