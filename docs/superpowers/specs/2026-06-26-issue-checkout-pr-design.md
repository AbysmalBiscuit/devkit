# Issue checkout-pr design

**Status:** Approved 2026-06-26
**Scope:** A new `issue checkout-pr` subcommand that creates a worktree with an
existing PR branch checked out, resolving the PR from a GitHub PR number/URL or a
Linear issue ID/URL.

## Goal

Get someone else's (or your own) PR branch into an isolated worktree in one
command, addressing the PR however you happen to have it: a GitHub PR number, a
GitHub URL, a Linear issue ID, or a Linear URL. The command resolves the input to
a single concrete PR, places the worktree via a config template, and checks the
PR branch out fork-safely. Unlike `issue setup`, it does **not** create a new
branch — it checks out one that already exists.

## Command surface

```
issue checkout-pr <PR_LINEAR_ID_URL> [WORKTREE_PATH] [--setup] [--apps web,api] [--config <path>]
```

| Arg / flag | Meaning |
|---|---|
| `<PR_LINEAR_ID_URL>` | `#3340` \| `3340` \| `PREFIX-3340` \| github PR URL \| linear issue URL |
| `[WORKTREE_PATH]` | optional; overrides the config-resolved placement |
| `--setup` | also run the per-app prep+install pipeline (default: checkout only) |
| `--apps web,api` | apps to prep; only meaningful with `--setup` |
| `--config <path>` | config path, mirroring the other subcommands |

## Identifier routing

A `resolve` step turns the raw input into one concrete GitHub PR before any
worktree work:

| Input shape | Routed as |
|---|---|
| `#3340` | GitHub PR 3340 |
| matches `^[A-Za-z]+-\d+$` (e.g. `ENG-3340`) | Linear ID → its PR |
| `github.com/.../pull/N` URL | GitHub PR N |
| `linear.app/.../issue/PREFIX-N` URL | Linear ID → its PR |
| bare `3340` (digits only, no `#`) | **probe both sides**, then disambiguate |
| bare `3340`, Linear key absent | GitHub PR (Linear never consulted) |
| `PREFIX-3340` / linear URL, Linear key absent | error — cannot resolve a Linear id without a key, and a `PREFIX-N` is not a numeric PR |

The `^[A-Za-z]+-\d+$` test reuses the existing `worktree::find_id` notion of a
Linear identifier (`LETTERS-DIGITS`). URL host (`github.com` vs `linear.app`)
decides URL inputs.

### Bare-number disambiguation

For a digits-only input with a Linear key present, both lookups run:

1. GitHub: does PR 3340 exist? (`gh pr view 3340`)
2. Linear: do any issues have `number == 3340`? (across all teams)

Then:

- only the PR hits → use it, no prompt.
- only Linear hits → use that issue → resolve its PR, no prompt.
- both hit → **prompt** the user to choose (one option per candidate; a Linear
  number that exists in several teams contributes one option per team).
- neither hits → error: *no PR or Linear issue found for `3340`*.

**The "both hit" prompt is TTY-gated.** With no interactive terminal (an agent),
a genuine collision is an error that names the disambiguating forms: *ambiguous
`3340` — rerun as `#3340` (GitHub PR) or `PREFIX-3340` (Linear)*. This matches
the toolkit's existing convention of TTY-gating ambiguous actions
(cross-worktree `devrun down`). The single-hit and no-hit paths never prompt, so
they work head-less.

## Linear → PR resolution

The `linear` module gains one query. Linear exposes a PR's link to an issue as a
GitHub *attachment*; the suggested git branch is `issue.branchName`.

```rust
pub struct LinearPr {
    pub pr_url: String,   // github PR URL from the attachment
    pub number: u64,      // parsed from the URL
}

/// Resolve the GitHub PR attached to a Linear issue, plus the issue title.
pub fn issue_pr(id: &str, key: &str) -> Result<(Option<LinearPr>, String)>
```

- GraphQL: fetch the issue by `team.key` + `number` (the existing split in
  `linear.rs`), selecting `title`, `branchName`, and `attachments` filtered to
  GitHub PR URLs (`url ~ github.com/.../pull/`). The title feeds templating.
- If the issue resolves but has **no** GitHub PR attachment, `issue_pr` returns
  `(None, title)` and the caller **errors out**: *Linear issue `ENG-42` has no
  associated PR to check out*. (`checkout-pr` only checks out PRs; it never falls
  back to creating a branch — that is `issue setup`'s job.)
- Number lookup for the bare-`3340` probe (`number == N` across teams) is a
  second small query returning the matching `(id, title)` candidates.

## Worktree creation and checkout

Once `resolve` yields a PR number (and, when known, the Linear id/title):

1. **Fetch PR metadata** for templating: `gh pr view <n> --json number,title,headRefName`.
2. **Resolve the worktree dir.** `WORKTREE_PATH` if given, else
   `{worktree_root}/{rendered checkout_worktree_dir}` (see Config).
3. **Create + checkout, fork-safely:**
   ```
   git worktree add --detach <path> <baseline_ref>     # in the monorepo
   gh pr checkout <n>                                   # cwd = <path>
   ```
   `gh pr checkout` is used rather than a manual `git fetch
   refs/pull/<n>/head` + `git worktree add <branch>` because it handles PRs from
   forks and sets up push tracking (adding the fork remote when needed), so the
   worktree behaves like a normal branch. The PR's own branch name is kept — the
   template governs only the **directory**, never the branch.
4. **Write the record** (see Scope).
5. **`--setup` only:** run the same per-app prep+setup loop as `issue setup`.

The monorepo path, `worktree_root`, and `baseline_ref` come from config exactly
as `issue setup` resolves them.

## Config schema

One new template beside `worktree_dir` in the existing `[templates]` table:

```toml
[templates]
checkout_worktree_dir = "{{ pr_number }}-{{ pr_title }}{% if linear_id %}_[{{ linear_id }}]{% endif %}"
```

Added as `pub checkout_worktree_dir: Option<String>` on `Templates`, resolved at
the render site with the built-in default below (`None` → default).

### Built-in default

```
{{ pr_number }}-{{ pr_title }}{% if linear_id %}_[{{ linear_id }}]{% endif %}
```

Renders `3340-fix-login` for a plain PR, and `3340-fix-login_[ENG-42]` when the
PR was reached through a Linear issue.

### Context variables

| Variable | Source | When PR-only |
|---|---|---|
| `pr_number` | resolved PR number | present |
| `pr_title` | PR title, **slugified** | present |
| `linear_id` | Linear issue id (e.g. `ENG-42`) | `""` (empty) |
| `linear_title` | Linear issue title, **slugified** | `""` (empty) |

`pr_title`/`linear_title` are slugified before substitution (lowercase,
runs of non-alphanumeric → single `-`, trimmed) because they name a directory.
`linear_*` are empty strings on the PR-only path, so the `{% if linear_id %}`
guard in the default drops the suffix. Strict-undefined still applies: a template
referencing an unknown variable errors loudly.

## Scope and record

Default is **checkout only**, but the worktree always participates in the normal
lifecycle: `checkout-pr` writes `<worktree>/.devkit/issue.toml` so `issue
status` and `issue end` recognise it.

```toml
issue = "ENG-42"        # Linear id if resolved via Linear, else the id parsed
                        # from the PR head ref (worktree::find_id), else "UNKNOWN"
slug  = "fix-login"     # slugified PR title
apps  = []              # or --apps when --setup is given
```

`--setup` then runs the existing per-app prep-file render + setup-command loop
from `setup.rs`, so the worktree is immediately runnable (e.g. to test the PR).
Without `--setup`, the command stops after writing the record — you run
install/dev yourself.

## Error handling

- Linear issue with no PR attachment → *Linear issue `<id>` has no associated PR
  to check out*.
- Bare number, no hit on either side → *no PR or Linear issue found for `<n>`*.
- Bare number, collision, no TTY → *ambiguous `<n>` — rerun as `#<n>` or
  `PREFIX-<n>`*.
- Linear input (`PREFIX-N` / linear URL) with no Linear key → error telling the
  user to set `LINEAR_API_KEY` (we cannot resolve a Linear id to a PR without
  it). A bare number with no key is simply treated as a GitHub PR.
- `gh pr checkout` failure (PR closed with deleted branch, network) → surfaced
  with `.context()` naming the PR.
- Worktree path already exists → the underlying `git worktree add` error is
  surfaced as-is.

## Test plan

TDD; `cargo test --workspace` is the merge gate.

| Unit | Tests |
|---|---|
| `resolve` routing | each row of the routing table → correct PR/Linear branch; `#N`/`PREFIX-N`/URL/bare-N classification; Linear-absent forces GitHub |
| Bare-number disambiguation | PR-only → PR; Linear-only → resolved PR; both → prompt list (incl. multi-team); neither → error; no-TTY collision → error |
| `linear::issue_pr` | parses a captured GraphQL fixture: PR attachment present → `Some`; no attachment → `None` + title; multi-team number lookup |
| `checkout_worktree_dir` render | PR-only drops the `_[…]` suffix; Linear path includes it; title slugification; strict-undefined on unknown var |
| Slugify helper | spaces/punctuation/case → clean slug; idempotent |
| Record | writes `issue`/`slug`/`apps`; `issue` falls back to head-ref id then `UNKNOWN`; `--apps` populates only with `--setup` |

Pure-function tests (`resolve`, slugify, template render, fixture parsing) are
unit tests; the `gh`/`git worktree`/`gh pr checkout` paths follow the existing
`setup`/`review` test patterns behind the established command seams.

[minijinja]: https://docs.rs/minijinja
