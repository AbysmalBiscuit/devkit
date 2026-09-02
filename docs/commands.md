# devkit command reference

Every command below is a `devkit` subcommand and is also reachable under its own short name through a hardlink `devkit` creates beside itself. `docm list` and `devkit docs list` are the same command. See [install.md](install.md) for how those links are made and kept current.

## Two help views

Every command answers help in two shapes, and which one you get depends on
where stdout points.

| Spelling | At a terminal | Piped or redirected |
|---|---|---|
| `<cmd> -h` | terse | terse |
| `<cmd> --help` | terse | full command tree |
| `<cmd> --help --full` | full command tree | full command tree |
| `<cmd> help` | terse | full command tree |
| `<cmd> help --full` | full command tree | full command tree |

Terse is one line per direct subcommand. The full tree descends through every
level, one line per command, so a coding agent reading help through a pipe
learns the whole surface in a single call instead of one call per group.

`-h` is terse under every condition and is the stable view to reach for. Set
`DEVKIT_HELP=terse` or `DEVKIT_HELP=full` to pin the choice regardless of where
output goes.

The tree is scoped, never cascading: `devkit issue --help` descends through
`issue` and stops. A command with no subcommands keeps clap's own rendering of
its flags and arguments, since a tree cannot carry those — one that does have
subcommands loses its own options block in the tree.

Each command's own `-h` is the authoritative flag list under every condition; `--help` matches it only at a terminal. This file carries what `--help` cannot: the resolution rules, the gates, and the reasons.

- [`portm`: port registry](#portm-port-registry)
- [`devrun`: supervised dev servers](#devrun-supervised-dev-servers)
- [`issue`: issue lifecycle](#issue-issue-lifecycle)
- [`lockm`: file locks](#lockm-file-locks)
- [`devkit`: setup and diagnostics](#devkit-setup-and-diagnostics)
- [`docm`: library docs](#docm-library-docs)
- [Timing](#timing)

## `portm`: port registry

Maintains a shared port registry so concurrent callers never collide on port allocation. State lives in `~/.local/state/devkit/ports.json`, guarded by an advisory file lock. Reservation rows are written before any process binds, which prevents the allocation race across concurrent callers.

```
portm status                                     # table of reserved/live ports
portm alloc <apps…> [--holder <path>] [--role issue|baseline]    # alias: reserve
portm release [apps…] [--holder <path>] [--role …]
portm prune                                      # remove stale reservations
```

`--holder` defaults to the current worktree's root (`git rev-parse --show-toplevel`). `release` with app names frees only those apps' reservations; it never signals processes (`devrun down` is stop-and-release).

## `devrun`: supervised dev servers

Launches and supervises dev servers for one or more apps. Apps not explicitly named are auto-detected by diffing `git diff <baseline_ref>...HEAD`. When any webapp is selected, `api` is added automatically and `FOUNDRY_API_BASE_URL` is wired to the local api port. Each app's `launch` command is run verbatim with `{{ port }}` substituted; wrap it in `doppler run` in the config if the app needs Doppler-injected secrets. `--role both` runs the issue branch and a fresh `origin/staging` baseline side-by-side on separate ports for direct A/B comparison.

```
devrun up [apps…] [--role issue|baseline|both] [--env K=V] [--env-file F] [--supervise] [--dry-run]
devrun down [--role …] [--all | --others | --holder <path>] [--app …] [--older-than …] [--batch] [selector]
devrun status [--all]
devrun reap [--all]
devrun logs <app> [--role …] [-f]

devrun task [<name>] [--env K=V] [--env-file F] [--dry-run]
```

- **`up`**: defaults to `--role issue` and allocates ports from the live registry at start time, so a worktree needs no reservation up front. `--supervise` hands the servers to `devkitd`, which restarts them on crash within the crash-loop budget. `--dry-run` prints the launch plan without starting anything: under `--role baseline` it reports the baseline directory a real run would use, without bootstrapping one and without repinning the worktree, so the reported path need not exist yet.
- **`status`**: lists tracked servers for this worktree (`--all` for every worktree). Each row carries the app's rendered URL, clickable where the terminal supports it and is wide enough to hold the link without crowding out the other columns. Below the tracked table it shows an **untracked (outside the registry)** section: dev servers detected listening on a configured app's port band, or matching a configured app's launch signature, that the registry doesn't own — i.e. started outside `devrun up`. Read-only; reaping is a separate command.
- **`reap`**: kills dev servers found running outside devrun (this worktree by default; `--all` reaches every worktree). It prints the matched process trees, then **requires an interactive terminal** and a confirmation before sending SIGTERM (escalating to SIGKILL). There is no `--yes`/`--force` bypass — with no terminal it refuses and kills nothing, so an agent (no PTY) can never reap. Detection is also available read-only to agents via the `ports.strays` MCP action; killing is not.
- **`baseline list`**: the baseline directory as it is on disk (columns: baseline, sha, state, size, referenced by). It enumerates the directory rather than asking git, so a tree git has no registration for is still listed — which is the state an operator most needs to see, and what the state column names: `registered` (a worktree git knows about), `orphaned` (marked, but this repository has no registration for it), `unmarked` (no `.devkit/baseline.toml`, so not devkit's to touch) or `unreadable` (a marker that can be neither read nor ruled out). Sizes count hidden files. When some worktree's own `.devkit/issue.toml` cannot be read, the listing says so on stderr: while that is true no baseline is provably unreferenced, so a sweep reclaims nothing whatever the referencer column shows.
- **`baseline prune`**: removes every baseline no worktree's `.devkit/issue.toml` names any more, one pass under the baseline directory's lock. A `registered` baseline goes through `git worktree remove`; an `orphaned` one is reclaimed as a plain directory, but only once nothing stands behind its `.git` — a git directory that resolves, or one that can be neither read nor ruled out, means the tree may be somebody else's checkout and it is left alone. A directory carrying no marker is named and left where it is: devkit cannot prove it created it. The refusals: running servers (waived by `--force`), modified *tracked* files (waived by `--discard-edits`, which discards them with the tree — untracked prep files and installed dependencies are expected and never count), a `git worktree lock`ed tree, and the directory the caller is standing in. The two waivers are separate on purpose: getting past somebody's edit must not also switch off the running-servers gate, and neither waives the marker, the lock, the cwd or the orphan check. One baseline's refusal does not abandon the sweep, and a run that refused any exits non-zero. `--dry-run` puts every slot through the same gates on the same inputs and stops before carrying the verdict out; what it cannot know is whether a removal it reports would succeed, or whether the answers still hold when a real sweep runs. It holds no per-slot lock, so it never waits on a bootstrap in progress.
- **`task`**: run a canned `[tasks]` oneshot or sequence (no name lists them).

### `devrun down` scope

Stops servers and releases their ports. By default it stops every server in the current worktree, plus those under the baseline this worktree is the sole referencer of — a baseline nobody else names is this worktree's own. Reaching another worktree, or a baseline another worktree also names, requires an explicit scope flag and a confirmation read from a terminal.

| Command | Effect |
|---|---|
| `devrun down` | stop all servers in this worktree and its own baseline |
| `devrun down --role baseline` | this worktree's own baseline only |
| `devrun down api` | this worktree and its own baseline, fuzzy-match `api` across columns |
| `devrun down --all` | every server, every worktree (one batch prompt) |
| `devrun down --others` | every server in every *other* worktree |
| `devrun down --others api` | `api` in other worktrees (per-worktree prompts) |
| `devrun down --holder ../wt/feat-x` | one specific worktree |
| `devrun down --all --app api --older-than 1h` | precise filter, all worktrees |

A positional selector substring-matches across `HOLDER`/`APP`/`PORT`/`ROLE`/`PID` and is mutually exclusive with the column filters (`--app`, `--port`, `--role`, `--pid`, `--listening`/`--not-listening`, `--older-than`). `--older-than` accepts `90s`, `30m`, `2h`, `1d` (bare number = seconds). Any selection that reaches a holder other than the current worktree or its own baseline prints a preview and prompts; with no interactive terminal it is refused. `--all`/`--batch` collapse the per-worktree prompts into one.

## `issue`: issue lifecycle

One command covering the whole issue lifecycle. Global `-C/--dir` and `--config` flags sit on `issue` itself, before the subcommand (e.g. `issue -C ~/git/acme/api status`).

Every subcommand works from the primary checkout, which it resolves through git: the main worktree when you are standing in a linked one, otherwise the checkout root of where you are. The directory's name does not matter. Run `issue` from inside the repository, or point it at one with `-C`.

```
issue setup <ID|URL> [--slug <slug>] [--apps <a,b>] [--summary|--no-summary] [--no-gitignore] [--dry-run]  # id also accepted as --issue <ID>
issue status [ids…]                           # read-only triage table (also the bare `issue`)
issue pr [status] [selector] [--json] [--cache-only] # one worktree's PR number + issue id (defaults to current; also the bare `issue pr`)
issue pr create [--draft|--ready] [--to <alias>] [--base <branch>] [--pr-title <t>] [--pr-body <b>] [--no-push] [--pr <URL|number>] [--arg k=v]
issue pr ready [--to <alias>] [--no-push] [--pr <URL|number>]
issue pr checkout <PR_ISSUE_URL> [WORKTREE_PATH] [--setup [--apps a,b]]
issue end [ids…] [-y] [--force] [--pr-only] [--clean-worktree]
issue sync-includes [selectors…] [--overwrite [--all]] [-y] [--dry-run] [-v]
issue prs [-m|--mine] [-r|--reviews] [-R owner/repo] [--no-cache] [--batch-size <N>] [--retries <N>]
issue dashboard [--bucket auto|day|week|month] [--chart bar|line] [--mode absolute|proportional] [--all-roles] [--author <email>] [--no-plots] [--no-cache]
issue review request [<body>] [--to <alias|#channel>] [--pr <URL|number>] [--no-push] [--no-notify] [--arg k=v]
issue review finish  [<body>] [--pr <number>] [--to <alias|#channel>] [--arg k=v]
```

### `setup`

Mechanical start of an issue. Creates a worktree off the baseline ref, symlinks env files, runs `bun install`, and prints the issue, worktree, and branch — as a labelled table when stdout is a terminal, so the path can be double-clicked out of the line, and as JSON otherwise for a script or an agent to parse.

The issue is whatever this project's tracker recognises: a Linear id (`ENG-1234`) or Linear URL, a GitHub issue number or a GitHub issue URL in the project's issues repository.

Without `--slug`, a Linear URL supplies the slug from its own `…/issue/<ID>/<title-slug>` path and no lookup happens; otherwise the tracker is asked for the issue's title and that is slugified, which needs the tracker's credential. Either way a leading copy of the issue id is stripped so the branch template does not repeat it, and the derived slug is shortened on a word boundary to fit `templates.branch_max` (default 46, the width `issue status` prints). The budget is measured by rendering your own `branch` template, so a longer `branch_prefix` or issue id takes from the slug rather than overflowing. A slug passed to `--slug` is used verbatim.

`{{ short_slug }}` is the same slug shortened again to fit `templates.worktree_dir_max` (default 24). Render it instead of `{{ slug }}` in `worktree_dir` when a worktree path needs to be shorter than the branch name, which on Windows is what keeps paths inside the worktree under the 260-character ceiling that third-party tools still enforce. It shortens an explicit `--slug` too: `slug` is verbatim because a slug you typed is a decision, while being shorter is the entire purpose of `short_slug`.

`--summary` additionally writes a markdown file holding the issue's tracker facts and description, followed by empty `## Summary` and `## Pointers` headings for whoever works the issue to fill in. Its path and body come from `templates.issue_summary_path` and `templates.issue_summary`; the default path is `ISSUE_SUMMARY_<ID>.md` under `worktree_root`, beside the worktree rather than inside it, so the notes outlive `git worktree remove`. An existing file is never overwritten — by the second setup it holds investigation the scaffold cannot reproduce — and its path is reported either way. `issue end` removes the file the worktree's record names as it cleans the worktree up, so the notes live exactly as long as the issue does. Set `defaults.issue_summary = true` to make this the default and `--no-summary` to skip it for one run.

The tracker fetch happens before anything is created, so a missing credential or an unknown issue fails with no worktree left behind, and it reuses the same round trip the slug lookup already needed. It does not reserve ports — `devrun up` allocates them dynamically when the worktree's servers start.

Each `[hooks] after_worktree_create` command runs last, in the new worktree's root, after the result has been reported. A hook that cannot render, cannot spawn, or exits non-zero warns and the rest still run: the worktree is already usable, so a missing program must not fail the command that created it. `docs/configuration.md` covers the table.

### `pr status`

Shows one worktree's PR number and issue id (current worktree, or a SELECTOR). Bare `issue pr` means `issue pr status`, the way bare `issue` means `issue status`. The optional selector is an issue id, branch, worktree basename, or path; omit it for the current worktree. `--json` emits a single machine-readable object (the `IssueWorktree` struct, with `pr_number`/`issue_id` for scripts). `--cache-only` skips the network — the PR number comes from the per-worktree cache at `<worktree>/.devkit/pr.json`, and the STATE and VERDICT columns render as `—`. A live run writes the PR through to that cache, which `git worktree remove` deletes with the worktree.

### `pr create`

Pushes the branch and opens this branch's PR, printing its URL. It posts no Slack — announcing the PR is `issue review request`.

```sh
issue pr create                                  # state from defaults.pr_create_state
issue pr create --ready --to igor                # open ready, with a reviewer
issue pr create --draft --pr-title "Fix login"   # open a draft under an explicit title
```

- `--draft` and `--ready` are mutually exclusive and decide the state for this run; without either, `defaults.pr_create_state` does.
- A branch that already has a PR reuses it instead of opening a second one: `--to` reviewers are added and everything else is left alone. A reused PR keeps the draft state it has, so a `--draft`/`--ready` that contradicts it is reported as ignored, naming the command that moves it (`issue pr ready`, or `gh pr ready --undo`).
- `--to <alias>` (repeatable) adds the `[people]` aliases that carry a `github` handle as GitHub reviewers. An alias without one warns and is skipped.
- `--base <branch>` overrides `defaults.pr_base`. `--pr-title` and `--pr-body` supply the `{{ input }}` of the `pr_title` and `pr_body` templates; the rendered title must not be empty. The body template is rendered only when a PR is actually opened, so a `pr_body` reading `{{ issue }}` cannot fail a run that only reuses one.
- `--no-push` opens the PR without pushing the branch first. `--pr <URL|number>` acts on that PR for this run, recording it — the way a worktree bound to the wrong PR is rebound. `--arg key=value` (repeatable) overrides a variable declared in `[templates.variables]`.
- Whichever PR the run ends on, its head commit must be this worktree's `HEAD`: a reused PR is checked before it is touched, and a newly opened one straight after, since that can only be checked once it exists. A failure there leaves the PR open on GitHub and says so.

### `pr ready`

Marks this branch's PR ready for review, printing its URL. Like `pr create` it pushes first and posts no Slack.

```sh
issue pr ready
issue pr ready --to igor        # add a reviewer in the same run
```

- A PR that is already ready is left untouched: nothing is flipped, the run says so on stderr and still exits zero. Running it twice is the same as running it once.
- `--to <alias>` (repeatable) adds GitHub reviewers, the same as `pr create`'s.
- `--no-push` and `--pr <URL|number>` behave as they do on `pr create`, and the head-commit check gates every mutation the same way.
- A branch with no PR is an error naming `issue pr create`; a merged or closed PR is refused for what it is.

`defaults.require_pr_reviewer` guards every path that leaves a PR ready for review: `issue pr create --ready`, `issue pr ready`, and the draft flip `issue review request` makes when it notifies. Each refuses unless a human reviewer other than the PR's own author is on it — one already there, whether the request is still pending or they have already reviewed, or one `--to` adds in the same run. A review the author submitted on their own PR does not count, so commenting on your own work does not open the gate. The refusal comes before the flip, so a gated run leaves the PR a draft. Opening a draft is never gated, and neither is a run against a PR that was already ready before it started: neither one leaves a PR ready that was not. The PR's own reviewer list is fetched only when it can still change the verdict, so a run with the gate off, or one whose `--to` already names a human, spends no lookup on it.

### `pr checkout`

Checks out an existing PR branch into a new worktree, unlike `setup`, which creates a new branch. The target may be:

- a GitHub PR number (`#3340`);
- a bare number (`3340`), probed against the PRs and against this project's tracker, prompting on a real collision in a TTY and erroring if ambiguous without one — on a GitHub project a bare number is always a PR, since issues and PRs share one numbering;
- an issue id the tracker recognises (`ENG-3340`), whose attached PR is used; an issue with no attached PR is an error;
- a GitHub PR URL, or an issue URL the tracker recognises.

The worktree directory is named by the `templates.checkout_worktree_dir` template (variables: `pr_number`, `pr_title`, `linear_id`, `linear_title`; titles are slugified; `linear_*` are empty on the PR-only path and otherwise carry whichever tracker's id and title resolved, the names being historical); the default renders e.g. `3340-fix-login`, or `3340-fix-login_[ENG-42]` when reached through an issue. `pr_title` and `linear_title` are shortened on a word boundary to fit `templates.checkout_worktree_dir_max` (default 46); a template rendering both splits the budget between them. Pass `[WORKTREE_PATH]` to override the placement. The PR's own branch name is kept, not devkit's to shorten — `gh pr checkout` takes it from the remote — the template governs only the directory.

Add `--setup [--apps a,b]` to also run the per-app prep pipeline, exactly as `issue setup` does. The worktree gets a `.devkit/issue.toml` record so `issue status`/`issue end` recognise it. Its result prints as a table on a terminal and as JSON otherwise, exactly as `setup` does. `after_worktree_create` fires here too, with or without `--setup`, because the event names the new worktree rather than the command that made it.

### `status`, `end`, `sync-includes`

- **`status`** (the default when you run bare `issue`): triage table of every issue worktree. A worktree is FINISHED when its PR is MERGED, its working tree is clean, and its issue has reached a completed state in the tracker. A project with no tracker has no state to wait for, so the merged PR and the clean tree decide it alone; a tracker that has no state for the issue — an id it does not know, or an API it could not reach — holds the verdict open rather than promoting the worktree.
- **`end`**: removes FINISHED worktrees, preserving each `[preserve.<name>]` entry's files first. `--pr-only` ignores the tracker-state and issue-id gates (finished = PR merged + clean), so a worktree carrying no issue id still qualifies; `--clean-worktree` targets explicit selections; `--force` overrides the dirty-tree guard; `-y` skips confirmation; `--no-preserve` skips preservation for the run. A `devkit.toml` that exists but fails to load makes every run refuse — even in a project with no `[preserve]` table, since the table cannot be read without loading the config — and `--no-preserve` is the way through; an absent `devkit.toml` is not a failure. A `required` entry whose preservation fails keeps that worktree, its branch, and its summary intact, and the command exits non-zero once every selected worktree has been handled. Once every removal has finished and the stale worktree entries are pruned, each `[hooks] after_worktree_remove` command runs for every worktree actually removed, then each `[hooks] after_end` command runs once — both in the main repository root, since the worktree they describe is gone. A run that removed nothing runs neither, and a failing hook warns without changing the exit status.
- **`sync-includes`**: re-copies the `defaults.worktree_include` files from the primary checkout into worktrees that already exist, the same list `setup` and `pr checkout` backfill at creation time. Use it when a file is added to that list after a worktree already exists. `selectors` match by issue id, branch, worktree basename, or path, same as `pr status`; omit them to sync every worktree. It deliberately does not resolve a PR number the way `end` and `pr status` do, since that needs a network call this command has no reason to make. By default it copies files the worktree is missing and warns about, but leaves alone, any file the worktree already has. A matched symlink is reproduced as a symlink pointing at the same target rather than being followed, so its contents are never duplicated into the worktree; links are counted and reported separately from copied files. On Windows this needs Developer Mode or administrator rights, and a refused link warns and is skipped. `--overwrite` opts into replacing those, prompting once per worktree with the list of files it would clobber; declining that prompt falls back to the default behaviour for that worktree, so the files it is missing are still copied. Because the files being replaced are untracked ones git cannot restore, `--overwrite` needs a scope: one or more selectors, or `--all` for every worktree in the repository, other sessions' included. `-y` answers the prompt without asking and does nothing on its own. `--dry-run` reports what would happen and writes nothing. Every list of files it prints is grouped by top-level directory and names only the first few from each, so an include reaching a build or asset cache does not bury the rest of the run; `--verbose` (`-v`) names every file instead.

### `prs` and `dashboard`

- **`prs`**: GitHub PR triage of your open PRs and PRs awaiting your review, with a per-repo diff cache that renders `old → new` for anything changed since the last run. The three searches (authored, review-requested, reviewed-by) run concurrently, each paged at `--batch-size` (default 25) and followed to exhaustion, so the table is complete however many PRs are open. Lower the batch size if GitHub answers a page with HTTP 504 — the per-PR check and review selections are what make a page expensive. `--retries <N>` (default 0) re-attempts a failed page with backoff.
- **`dashboard`**: the triage + PR tables, plus terminal timelines of the issues assigned to you by status, PRs opened/merged, and commits over time (`--chart bar` or `line`). The issue timeline comes from whichever tracker this project uses. The timeline fetches (tracker + GitHub) are cached under `~/.cache/devkit/dashboard` for a few minutes so reruns are fast; the live triage/PR panel is never cached. `--no-plots` shows only the tables; `--no-cache` forces a fresh fetch.

### `review request`

Push the branch, request review on this branch's PR, and Slack the reviewers. It opens no PR: a branch that has none is an error naming `issue pr create`, and a merged or closed PR is refused for what it is.

```sh
issue review request "ready for a look" --to igor
issue review request --to igor --to '#eng' --arg team=infra   # body optional; channel + people
issue review request                                          # re-ping the PR's existing reviewers
issue review request --no-notify                              # push, add reviewers, tell nobody
```

- `--to <alias|#channel>` (repeatable). People are added as GitHub reviewers (those with a `github` handle) and Slacked; `#channels` are Slack-only. Omit `--to` to re-request and Slack the PR's current human reviewers.
- A run that notifies marks a draft PR ready for review first, since asking a human to look at a draft is incoherent. `defaults.require_pr_reviewer` gates that flip exactly as it gates `issue pr ready`, and a refusal leaves the PR a draft.
- `--no-notify` sends no Slack, never falls back to the PR's current reviewers, and leaves draft state alone. It prints the PR URL instead. Combined with `--to` it still adds those GitHub reviewers, just without the Slack.
- `--pr <URL|number>` acts on that PR for this run: a pasted GitHub PR URL keeps its own repository, a bare number means `pr_repo`. Since the command records whichever PR it acted on, this is also how a worktree bound to the wrong PR is rebound — the recovery for a superseded PR, where two PRs share a head branch and the branch lookup is ambiguous. Otherwise the PR comes from the worktree's record, and failing that from its branch.
- `--no-push` requests the review against the branch as GitHub already has it, without pushing first.
- `--arg key=value` (repeatable) overrides a variable declared in `[templates.variables]`.

The `pr_title` the Slack template renders is the PR's own title, read from GitHub, not a locally rendered one — the command is not creating the PR and has no title of its own to offer.

Everything that can refuse the run happens before the draft flip: the recipients are resolved, and the reviewer gate judged, before a draft is ever marked ready for review. A run that ends up with nobody to notify leaves a draft a draft.

Whichever way the PR is resolved, its head commit must be this worktree's `HEAD` or the command refuses it: a branch name is shared across forks and does not prove the PR carries this work, and a wrongly bound PR that later merges would let `issue end` delete a branch whose commits never landed. A squash- or rebase-merged PR still matches, since the comparison is against the branch head the PR carries rather than the commit that landed on the base.

### `review finish`

Announce over Slack that you finished reviewing. Posts nothing to GitHub.

```sh
issue review finish "LGTM, merging after CI"          # inside the PR's worktree → notifies the author
issue review finish --pr 1234 --to lev                # from anywhere, explicit PR + recipient
```

- Resolves the PR from `--pr <number>`, else the worktree's record, else the current branch. `--pr` applies to that run only and never rewrites the record.
- No head-commit check here, unlike `issue review request`: this is the reviewer's command, run in a worktree `issue pr checkout` built, where `HEAD` falls behind the moment the author pushes again.
- Defaults to notifying the PR author; `--to` overrides (repeatable, people or `#channels`).
- `--arg key=value` as above.

Templates: `review_request` and `review_finish` under `[templates]`. Per-recipient render fields: `name` (alias or channel), `slack_id` (user id, empty for channels), plus `pr_url`, `pr_title`, `input` (and `author` for finish). See [configuration.md](configuration.md#templates).

### Live rendering

On a TTY, `issue` and `issue pr status` draw the triage table immediately and fill in each cell with an animated braille spinner as git, GitHub, and tracker data land. `issue prs` shows the previous run's tables dimmed with a fetch spinner below noting they are as of the last run (stale-while-revalidate), then swaps the fresh tables in place — the two renders are line-for-line parallel, so the screen does not shift. The step-driven commands (`pr checkout`, `setup`, `end`, `review`) keep every completed step on screen as a numbered `✓` log line with its elapsed time. All of this live rendering goes to stderr and is TTY-gated — stdout, piped output, and redirected output are unaffected.

## `lockm`: file locks

Advisory locks on paths so parallel sessions sharing one checkout (where per-session worktrees are too expensive) don't edit the same files at once. A flock-guarded registry of claims keyed by path, the file-level twin of `portm`. Locks are exclusive and overlap by path component, so locking a directory conflicts with locking a file inside it.

```
lockm acquire <paths…> [--as <id>] [--note <msg>] [--ttl <secs>] [--json]
lockm release <paths…> [--as <id>]        # or: release --all
lockm check   <paths…> [--json]           # read-only: would acquire succeed?
lockm status  [--all] [--json]            # alias: list
lockm prune
```

Sessions identify themselves by (in priority order) `--as <id>`, `$DEVKIT_SESSION`, `$TMUX_PANE` (zero-config and unique per tmux pane), the controlling tty, or the parent pid. Conflicts fail fast: `acquire`/`check` exit `1` and report who holds the path. Locks expire after their TTL (default 30 min, `--ttl 0` disables) or when a recorded anchor pid dies; `release` frees them explicitly. For non-interactive agent sessions, pass a stable `--as`/`$DEVKIT_SESSION` so acquire and release agree.

## `devkit`: setup and diagnostics

Configures and diagnoses the toolkit itself. `auth` validates a Linear or Slack credential against the live API and stores it in `~/.config/devkit/secrets.toml` (`0600`); `doctor` reports where each credential resolves from and whether it is valid. Tokens always resolve env-first, so a shell export or Doppler-injected var still wins.

~~~
devkit auth <linear|slack> [--token <value>]   # validate + store; prompts (no echo) by default
devkit auth github                              # report the GitHub identity devkit would use
devkit doctor [--json]                          # check configured credentials
devkit config [show] [--origin] [--json]        # the resolved config for this directory
devkit config apps [--json]                     # the configured apps
devkit config tasks [--json]                    # the configured tasks
devkit brief [--pins-only] [--if-changed] [--additional-context]   # compact project brief
devkit schema                                   # JSON Schema for devkit.toml, for editor validation
devkit schema init [PATH]                       # point a devkit.toml at that schema (starter if absent)
devkit completions <shell>
devkit install-links [--force]                  # (re)create the old-name hardlinks
~~~

- **`auth`**: prompts for the token without echo (or reads `--token`/piped stdin), validates it, and saves it. For Linear it also stores the workspace slug derived from the API, so issue links work without setting `LINEAR_WORKSPACE`.
- **`auth github`**: reports, and stores nothing. devkit keeps no GitHub credential of its own because `gh auth login`, `GH_TOKEN` and `GITHUB_TOKEN` already cover it. It prints the identity behind the token devkit would send and which of the three supplied it, then lists `gh`'s own accounts separately below — those can differ from the token's identity, and the token's is the one devkit uses. A `--token` passed here is refused rather than quietly discarded, since accepting one would suggest devkit had stored it.
- **`config`**: prints the resolved config for the current directory, as TOML, opening with the layer files it was merged from as comments in lowest-to-highest precedence order. A bare `devkit config` is `devkit config show`. `--origin` annotates each value with the file it was resolved from (or `# (default)` for serde defaults) and, where several layers set it, the layers it overrode and what each held. `--json` emits a bare config object; `--origin --json` emits `{ "config": …, "layers": [...], "origins": { "dotted.path": "file" }, "overrides": { "dotted.path": [{ "file": …, "value": … }] } }`. Flags may be spelled on either side of the subcommand.
- **`config apps`**: lists the configured apps from the merged config (columns: name, port, path, url, provides_url, url_env, launch). `--json` emits a structured array. A pure config readout with no live readiness — for running state use `devrun status`.
- **`config tasks`**: lists the configured `[tasks]` from the merged config (columns: name, kind, app, description) — the same listing as a bare `devrun task`. `--json` emits a structured array.
- **`doctor`**: one row per credential — source (`env`/`file`/`unset`) and live validity. Exits non-zero when a credential that *is* set fails validation, or when a `devkit.toml` exists that does not load — the `config` row carries the cause, since a config that fails to deserialize makes every other devkit command fail the same way. Having no `devkit.toml` at all is reported without complaint. Also warns when the installed binaries are older than the newest devkit plugin checkout in `~/.claude/plugins/cache` (skewed binaries make agents follow docs for features the binaries lack), when servers run outside devrun, when the docs cache holds unreferenced checkouts, and — one row per old name — when a shim is missing or a name is held by something other than devkit; a shim problem is always a warning and never changes `doctor`'s exit code. The `tracker` row names the tracker `issue` talks to here and how devkit arrived at it — `[tracker] kind` or detection — and warns when devkit fell back to no tracker, which holds every issue-state gate closed.

### `brief`

Prints a compact orientation for the current checkout: configured apps, the `[tasks]` table, this worktree's live servers, and the versions this checkout's lockfiles pin for each registered library. The two halves are independent. A checkout with no devrun setup still gets the library table, and a checkout that evidences no registered library still gets the rest. It prints nothing when neither applies, so a hook can call it from any repository — but a `devkit.toml` that exists and does not load is a fault, not an absence, and is reported with the cause instead.

Every section earns its place: a project with no apps is not told about `devrun up` or `portm`, and one with no tasks is not told about `devrun task`. `[brief]` suppresses a section the checkout does have (`apps`, `tasks`, `locks`); see [configuration.md](configuration.md#brief).

The library table answers for the directory it runs in. At a workspace root — a workspace container that declares nothing of its own — it rolls up the members its lockfile names, one row per version they resolve, so a session started there sees `kysely 0.28.17 — bun.lock (apps/api, packages/db-types, +5)` and, where members disagree, both versions with the workspaces holding them. A library the reference registry records a checkout for under this project shows too, even with no lockfile evidence, and is flagged when the checkout's version is not the one the lockfile names — that gap is where an agent reads one version while the project builds another. Roll-up covers the JS lockfiles, which name their members; cargo and uv keep theirs in a manifest, so those resolve per workspace as before.

`--pins-only` emits just the library table. `--if-changed` prints nothing when this session already received the same brief, keyed on the `session_id` in the hook's stdin JSON. A full brief records itself against that key, so the first `--if-changed` after one stays silent. `--pins-only` does not record: it carries only the library table, and a full brief is still owed. The plugin runs all three: `SessionStart` (full), `PostCompact` (`--pins-only`), and `CwdChanged` (`--if-changed`). `--additional-context` wraps whichever of those a run emits in the JSON envelope Codex and Cursor read a hook's context from; Claude Code injects plain stdout and takes the brief without it.

## `docm`: library docs

Version-correct local library checkouts backing the `devkit:docs` skill. Register a library once; every lookup resolves the version the requesting workspace's own manifest and lockfile pin and materializes a checkout for it under `~/.local/share/devkit/docs/`, named for the exact ref it holds rather than a bare version (`h3/v1.15.11`, `openapi-ts/@hey-api~client-fetch@0.13.1` — `/` encodes as `~`). `docm info`'s `commit` field is the proof of what a checkout actually has, not the printed `version` string.

```sh
docm add tokio                    # registry lookup (crates.io/npm/PyPI)
docm add https://github.com/godotengine/godot --ref 4.3-stable
docm add react --project          # write to this repo's devkit.toml [docs]
docm add zod --eco js             # name the ecosystem instead of probing for it
docm add h3 --src-dir src --docs-dir docs   # override the detected checkout layout
docm add tokio --notes "async runtime we copy patterns from"  # shown by info/list
docm list                         # merged catalog: name, ecosystem, ref, origin
docm list --project               # only what this checkout evidences; --json emits {pins, dropped}
docm info tokio                   # path + version + layout map + notes
docm path tokio                   # just the checkout path
docm sync                         # fetch, re-resolve, re-materialize, verify
docm rm tokio                     # drop from the manifest (aliases: remove, delete)
docm forget tokio                 # release this project's reference to it
docm prune                        # drop checkouts no live project references
```

Global manifest: `~/.config/devkit/docs.toml`. Per-project overlay: `[[docs.libs]]` entries in `devkit.toml` (same fields; partial entries override the global entry field-by-field).

### Version resolution

Resolution order: a manual `ref` pin, then the requesting workspace's own dependency graph (`Cargo.lock`, `pnpm-lock.yaml`, `package-lock.json`, `bun.lock`, `uv.lock`) matched against the repo's git tags.

Only a registry install resolves this way: a version number identifies upstream's code just when the lockfile says it came from the registry the repo publishes to, so a git, path, workspace, link or archive dependency is refused by name and needs `--ref`. A remote tarball is judged by the spec that declares it rather than by the row it installed, because npm records the same `resolved` URL for a tarball fetched from the registry host as for an ordinary version range.

When nothing pins a version — no tag matches the lockfile's version, no importer manifest is found, the ecosystem is ambiguous, or a lockfile's own state conflicts with itself — `docm` fails with the specific cause and the fix rather than silently checking out the default branch. Pass `--allow-default-branch` (a global flag, valid on `add`, `sync`, `path`, and `info`) to opt into that checkout for one run instead.

### References and pruning

Resolving a library from a project records a *reference*: the project root, the library, and the checkout it received. A reference holds that checkout against `docm prune`, and puts the library in the project's `docm list --project` table even when nothing here declares it — which is how a `--ref` pin, or a library since dropped from the manifest, keeps appearing for a checkout that never used it. `docm forget <lib>` releases this project's reference and leaves the checkout for `prune` to reclaim once nothing references it.

### Reserved names

A library or ref name cannot collide with the cache's own control files: `registry` (and anything starting `registry.`) is reserved at the cache root for the reference registry, `manifest` is reserved for the lock the cache takes while editing a manifest, and `repo.git`/`meta.toml` are reserved inside each library's own directory. Register a package whose name is reserved under a different one with `docm add <other-name> --package <package>`. A library already registered under a reserved name cannot be removed with `docm rm`, which fails the same check: delete its entry from the manifest holding it and delete its cache directory by hand.

### Migrating a 0.12.x cache

A cache built by devkit 0.12.x has its layout migrated the first time any `docm` command runs against it: nested scoped library directories (`@scope/pkg/`) are renamed to the new encoding and their worktrees repaired, and legacy entries keep protecting their existing checkout until the library re-resolves under the new layout. `docm prune` then reclaims what the migration leaves behind, including retired `default` checkouts once nothing references them any longer.

A 0.12.x `meta.toml` is not migrated. Three of the five tag patterns 0.12.x could record — `name-dash`, `name-dash-v` and `name-at` — no longer parse, and guessing which of the current patterns they meant would resolve a wrong git tag and serve wrong docs. Every `docm` command against such a cache fails instead, naming each `meta.toml` it cannot read in one run: delete those files and run `docm` again. The origin, layouts, tag pattern and commit records they hold are all re-derived.

## `devkit-mcp`: agent access

`devkit-mcp` (equivalently `devkit mcp`) serves the port and file-lock registries, the server-lifecycle facade and issue triage to MCP-capable coding agents over stdio. The actions, their arguments, and which of them mutate are in [agents.md](agents.md), alongside the plugin and per-host wiring.

The gates differ from the CLI's on purpose: `devrun reap` is never exposed, and `devrun.down` takes one holder and stops that holder's servers, with none of the CLI's cross-worktree prompts. `ports.strays` is the read-only half of stray handling.

That holder is the `root` the caller passes, and the handler does not check it against the calling session, so `devrun.down` stops whatever holder it is given — including a baseline directory, whose path `devrun.status` with `all` reports. The CLI's terminal gate is not a property of the MCP surface, and an agent host that needs one enforces it by not granting `devrun.down`.

## Timing

`issue` and `devrun` accept a global `--timing` flag that prints a per-operation breakdown of subprocess and network IO to stderr on exit:

- `--timing` (or `--timing=summary`) — a table of ops (`git fetch`, `github REST`, `linear graphql`, …) with count / total / max / p50, plus a headline showing wall time, IO-busy time, serial sum, and the concurrency factor the parallel fan-outs achieve.
- `--timing=trace` — additionally lists every op with its start offset, thread, and full command line.
- `--timing-log <FILE>` — streams one JSON record per op (`op`, `detail`, `start_ms`, `dur_ms`, `thread`) for comparing runs.

`DEVKIT_TIMING=summary|trace` enables the summary/trace form without the flag. stdout (tables, `--json`) is never affected.

    issue status --timing
