# Configurable slug caps

**Date:** 2026-08-30
**Status:** proposed

## Problem

A worktree directory name devkit generates can be long enough to push paths
inside the worktree past Windows' 260-character ceiling, at which point
third-party CLIs fail to delete the tree. `git config core.longpaths true` fixes
git itself and reaches nothing else.

`issue setup` does cap its derived slug, but against the wrong number.
`slug_budget` (`src/bin/devkit/issue/setup.rs:277`) subtracts the branch
template's fixed cost from `ui::BRANCH_DISPLAY_MAX`, a constant whose stated job
is the width the `issue status` branch column renders before eliding
(`crates/devkit-common/src/ui.rs:103`). That is a display fit. It has no
relationship to a path limit, and it is not configurable.

Two things make the cap loose in practice:

- The default branch template is `{{ prefix }}{{ slug }}`
  (`crates/devkit-config/src/lib.rs:389`), with no `{{ issue }}` in it. A
  GitHub tracker's numeric issue id therefore costs the budget nothing, so
  the title slug gets nearly the whole 46 characters. A project that spells a
  Linear id into the branch spends ten or so characters there and gets visibly
  shorter slugs from the same code.
- `worktree_dir` defaults to `{{ slug }}`
  (`crates/devkit-config/src/lib.rs:390`), so the directory inherits whatever
  the branch got. With `worktree_root` at
  `C:/Users/Lev/Git/lev/devkit-worktrees/` (38 characters) plus a 42-character
  directory, roughly 180 characters remain for everything inside the repository.

`issue checkout-pr` has no cap at all. `slugify(&meta.title)` goes straight into
the `checkout_worktree_dir` context (`src/bin/devkit/issue/checkout.rs:338`), so
a long PR title becomes a long directory name with nothing standing in the way.

There is a third defect behind both. `slug_budget` measures the template's fixed
cost by rendering once with a one-character slug and subtracting 1. That is only
correct when `{{ slug }}` appears exactly once. A template that omits it gets a
budget that constrains a variable it never renders; a template that uses it
twice gets a budget that overflows.

## What already works (verified, no change needed)

- **`cap` shortens on a word boundary and never leaves a trailing dash**
  (`src/bin/devkit/issue/slug.rs:68`). It hard-cuts only a first word longer
  than the whole budget. Nothing about it needs to change.
- **`setup` builds one context object and reuses it** for the branch template,
  the `worktree_dir` template, `prep_apps`, and the `after_worktree_create`
  hooks (`src/bin/devkit/issue/setup.rs:334`). A variable added there reaches
  every consumer.
- **`checkout-pr` never constructs a branch name.** It runs
  `git worktree add --detach` and then `gh pr checkout`
  (`src/bin/devkit/issue/checkout.rs:388`), so the branch is the PR's head ref
  from the remote. Only the directory name is devkit's to cap.
- **`devkit-config` is the leaf crate.** `devkit-common` already depends on it,
  so a constant can move down without a cycle.
- **An explicit `--slug` is documented as verbatim** (`docs/commands.md:94`) and
  stays that way.

## Design

### 1. Three config keys under `[templates]`

```toml
[templates]
branch_max                = 46   # default 46
worktree_dir_max          = 24   # default 24
checkout_worktree_dir_max = 46   # default 46
```

Each is `Option<usize>` with an accessor falling back to a `DEFAULT_*` constant,
matching how every other key in `Templates` already works.

`branch_max` names the length `setup` fits a rendered branch into.
`worktree_dir_max` and `checkout_worktree_dir_max` name the length each command
fits a rendered worktree directory name into.

### 2. `short_slug`, a new variable for `setup`

`slug` keeps its current meaning exactly: the title slug capped to `branch_max`
minus the branch template's fixed cost, with the existing `MIN_SLUG` floor.

`short_slug` is the same title slug capped to `worktree_dir_max` minus the
`worktree_dir` template's fixed cost. It joins the shared context at
`setup.rs:334`, so it is available to the branch template, the `worktree_dir`
template, `prep_apps`, and hooks.

The default `worktree_dir = "{{ slug }}"` does not change, so this is inert
until a project opts in:

```toml
[templates]
worktree_dir = "{{ short_slug }}"
```

For issue #142, "Group sync-includes file lists in the output", with
`branch_prefix = "lev/"`:

```
branch: lev/group-sync-includes-file-lists-in-the
dir:    group-sync-includes-file
```

The branch slug gets 42 characters (46 less the four `lev/` costs) and the
directory gets 24, and `cap` drops whole words to land under each.

Two variables are warranted here because one title slug feeds two names with
unrelated limits. A git ref has no length problem; a directory does.

`short_slug` shortens an explicit `--slug` as well. `slug` stays verbatim
because a slug you typed is a decision, but being shorter is the entire purpose
of `short_slug`, so a caller opting into it opts into that too.

### 3. `checkout-pr` caps its existing title variables

No new variables. `checkout_worktree_dir_max` caps whichever of `pr_title` and
`linear_title` the `checkout_worktree_dir` template renders. When a template
renders both, they split the budget evenly.

`checkout-pr` builds exactly one name from its title slugs, so there is nothing
for a second variable to distinguish. The default of 46 brings the command to
parity with what `setup` already enforces, and it applies on upgrade with no
configuration:

```
before: 142-group-sync-includes-file-lists-in-the-output_[ENG-1234]
after:  142-group-sync-includes-file-lists_[ENG-1234]
```

The budget there is 46 minus the fixed text the default template renders around
the title, which is `142-` and `_[ENG-1234]`.

### 4. One measurement function

`slug::budget` replaces `slug_budget` and serves all three sites:

```rust
pub(crate) fn budget(
    template: &str,
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    names: &[&str],
    max: usize,
    floor: Option<usize>,
) -> Result<usize>
```

It renders the template twice against a real context, with every name in `names`
set to a one-character value and then to a two-character value. The length
difference is the total number of occurrences across those names. The
one-character render's length minus that count is the fixed cost, and the
returned per-name budget is `(max - fixed) / count`. A count of zero means the
template renders none of the named variables, and the budget is `usize::MAX`.

`floor` carries the overflow policy, because the two kinds of limit want
opposite answers when the fixed text does not leave room. `Some(n)` clamps the
result up to `n` and never fails. `None` returns an error naming the template,
its fixed cost, and the configured maximum.

Probing against the real context rather than a synthetic one matters for the
default `checkout_worktree_dir`, whose `{% if linear_id %}` branch changes the
fixed cost depending on whether a tracker id resolved.

Call sites:

| Command | Template | Names | Max | Floor |
|---|---|---|---|---|
| `setup` | `branch` | `slug` | `branch_max` | `Some(MIN_SLUG)` |
| `setup` | `worktree_dir` | `short_slug` | `worktree_dir_max` | `None` |
| `checkout-pr` | `checkout_worktree_dir` | `pr_title`, `linear_title` | `checkout_worktree_dir_max` | `None` |

### 5. Overflow behaves differently per limit

`branch_max` keeps the `MIN_SLUG = 12` floor (`setup.rs:268`) and degrades. A
git ref has no hard length limit, 46 is a column width, and an over-long branch
is elided by the status table. This is today's behavior and it stays.

Both directory limits hard-error when the template's fixed text leaves no room,
naming the template, its fixed cost, and the configured maximum. A limit on a
filesystem path that silently does not hold is the defect this design exists to
fix. `issue setup --dry-run` reaches the error before any worktree is created,
and the realistic trigger is a hand-written template carrying more literal text
than its own configured maximum.

### 6. The 46 stops being duplicated

`ui::BRANCH_DISPLAY_MAX` is one constant doing two jobs, deliberately, per its
own doc comment. Splitting it into a config default and a display width would
put the same number in two crates.

Instead `DEFAULT_BRANCH_MAX` is defined in `devkit-config` and
`ui::BRANCH_DISPLAY_MAX` becomes a re-export of it. Every existing call site,
including `triage.rs:17`, is untouched, and the two values cannot drift.

The status table keeps eliding at 46 regardless of `branch_max`. A project that
raises `branch_max` gets longer branches and an ellipsis in `issue status`,
which is what an elision is for. Threading config into table rendering buys
nothing.

## What this does not do

- No platform-dependent defaults. `devkit.toml` travels with a checkout, so a
  value that means one thing on Windows and another on Linux would be a trap.
- No path-length budget that subtracts `worktree_root` from a 260-character
  ceiling. It self-tunes, but the reserve for in-repository paths is still a
  guessed number, and the character caps here are enough to solve the reported
  problem.
- No cap on branch names in `checkout-pr`. They come from the remote.
- No warning when an explicit `--slug` exceeds `branch_max`. Verbatim means
  verbatim.

## Testing

- `budget` at zero, one, and two occurrences of a single name, and across two
  names in one template.
- `budget` measured against a template with a conditional block, confirming the
  fixed cost reflects the real context.
- The hard error for `worktree_dir_max` and for `checkout_worktree_dir_max` when
  fixed text fills the budget.
- `setup` producing a branch and a directory of different lengths from one
  title, which is the behavior a unit test on `cap` alone would miss.
- `setup` unchanged when `worktree_dir` is left at its default.
- `checkout-pr` capping a long PR title in the directory name under the default
  template while the head ref is untouched.
- The committed `schema/devkit-config.json` regenerated with
  `DEVKIT_UPDATE_SCHEMA=1 cargo test`.

## Files

| File | Change |
|---|---|
| `crates/devkit-config/src/lib.rs` | three `Templates` keys, three `DEFAULT_*` constants, three accessors |
| `crates/devkit-common/src/ui.rs` | `BRANCH_DISPLAY_MAX` becomes a re-export of `devkit_config::DEFAULT_BRANCH_MAX` |
| `src/bin/devkit/issue/slug.rs` | `budget` |
| `src/bin/devkit/issue/setup.rs` | drop `slug_budget`, compute `short_slug`, extend the shared context |
| `src/bin/devkit/issue/checkout.rs` | cap `pr_title` and `linear_title` |
| `schema/devkit-config.json` | regenerated |
| `docs/configuration.md` | the three keys and `short_slug` |
| `docs/commands.md` | the slug paragraph for `setup`, and the `checkout-pr` section |
