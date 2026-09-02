# Baseline worktrees: design

## Goal

Make the baseline a per-fork-point, content-addressed worktree that devkit
creates on demand and reclaims when nothing references it, and make every
`[defaults]` key optional so a personal config layer no longer breaks config
resolution outside a devkit project.

The baseline is the unmodified tree a worktree's changes are compared against.
Today it is one shared checkout pinned to the tip of a configured ref. That is
the wrong thing to compare against, and the sharing is unsafe.

## Scope

**In:** the meaning and resolution of the baseline; `defaults.baseline_ref`,
`defaults.baseline_path`, `defaults.baseline_dir`, `defaults.worktree_root`,
`defaults.branch_prefix`, and `defaults.pr_base`; the `[defaults]` requirement
itself; a `baseline` field on the worktree record and a marker file inside each
baseline; lazy baseline creation with the full worktree bootstrap; baseline
reclamation in `issue end`; a `devrun baseline` command group; a `devkit doctor`
row; the docs and JSON schema that follow.

### Non-goals

- **A refresh path.** A baseline pinned to a commit is immutable. Nothing
  fetches, resets, or updates one after creation.
- **Keeping the shared-tip mode.** The old behavior is removed rather than kept
  behind a flag. Nothing in the codebase wants "compare against whatever the
  remote tip is right now".
- **Stopping servers on the caller's behalf.** Deletion refuses while live
  registry rows hold the path, but nothing brings those servers down. `issue
  end` already removes a worktree without stopping its servers, letting the
  ports registry free the rows when the holder path vanishes.
- **New MCP actions.** `baseline list` and `baseline prune` are CLI-only.
- **An eviction policy.** Unreferenced baselines are removed by an explicit
  verb, not by a size or age budget.
- **Renaming `--role both`.** The name still fits.

## Background

### What exists

`defaults.baseline_ref` and `defaults.baseline_path` are both required keys.
`baseline_ref` has two unrelated consumers:

- `devrun up` with no apps named runs `git diff <baseline_ref>...HEAD --stat` to
  infer which apps changed (`src/bin/devkit/run/mod.rs:618`). Three dots means
  merge base, so this reading is fork-point relative and per worktree.
- `issue setup` and `issue checkout-pr` pass it to `git worktree add` as the
  commit a new worktree is cut from (`src/bin/devkit/issue/setup.rs:513`,
  `src/bin/devkit/issue/checkout.rs:421`).

`baseline_path` names a single checkout that `devrun up --role baseline` keeps
pinned at the tip of `baseline_ref` through `ensure_fresh`
(`src/bin/devkit/run/baseline.rs:9`): create if missing, else refuse a dirty
tree, else fetch and `git reset --hard`. It is called with the *calling
worktree* as its git repository, not the primary checkout. It is also one of the
two managed roots stray detection recognizes
(`crates/devkit-ports/src/strays/mod.rs:238`), and it is the ports-registry
holder for every baseline-role row.

### Why it needs replacing

The same config key means "the commit I forked from" for app detection and "the
current remote tip" for the baseline server. Both readings are defensible; one
key carrying both is not.

The shared checkout produces three failures that have nothing to do with each
other except the sharing:

1. A second worktree's `devrun up --role baseline` resets the tree under the
   first worktree's running server. The spawn is skipped because the pid is
   live, so a process started at one commit ends up serving another.
2. Any file in the baseline tree that git reports as modified or untracked makes
   `ensure_fresh` bail. That error propagates out of the group-building block in
   `cmd_up` before anything spawns, so one worktree's leftover build output
   blocks a different worktree's own issue servers.
3. The baseline holder is neither worktree's root, so a plain `devrun down`
   matches nothing (`Scope::Current` compares `holder == toplevel`,
   `crates/devkit-ports/src/registry.rs:758`) and stopping it requires
   `--holder`, which is TTY-gated (`src/bin/devkit/run/mod.rs:815`). This also
   makes `docs/commands.md:59` wrong today.

Separately, `[defaults]` requires four keys, and the requirement applies to the
merged config. A personal layer at `~/.config/devkit/config.toml` that sets one
of them opens the table for every directory on the machine, so config resolution
hard-errors wherever no project layer completes it. `devkit brief` runs from a
session-start hook and prints that error in any git repo that is not a devkit
project.

## Staging

One spec, two stages. Stage A stands alone and ships first; stage B depends on
it only for `baseline_dir`'s default.

- **Stage A, config relaxation.** Every `[defaults]` key optional, the derived
  `worktree_root`, the `pr_base` default, deleting the missing-`[defaults]`
  bail. It fixes `devkit brief` the day it lands.

  A carries two things that look like they belong to B, because without them A
  breaks working setups. First, `target()` from section 2: the moment
  `baseline_ref` may be empty, `issue setup` and `issue checkout-pr` would pass
  `""` to `git worktree add` (`src/bin/devkit/issue/setup.rs:513`,
  `src/bin/devkit/issue/checkout.rs:421`) and app detection would run
  `git diff ...HEAD`, whose failure `unwrap_or_default` swallows into "no apps
  detected in diff vs ". Second, a use-site error for an empty `baseline_path`,
  since `ensure_fresh("")` would otherwise run `git worktree add --detach ""`.
  `baseline_path` itself survives stage A unchanged; only its requiredness goes.

- **Stage B, the baseline redesign.** Everything else, including removing
  `baseline_path` and adding `baseline_dir`.

  Within B the two config changes are far apart. `baseline_dir` is added early,
  because bootstrap needs it; `baseline_path` is removed last, because
  `cmd_up`, `strays::managed_roots`, and the `schema init` starter each read it
  until their own change lands. Removing the field before those readers go
  leaves the workspace red across a run of commits.

Both stages are `feat!`: A flips the `pr_base` default, B removes
`baseline_path`. Each names its own change in a `BREAKING CHANGE` footer.

## Design

### 1. Config (stage A)

Every `[defaults]` key becomes optional. The table has no required fields, and
the bail in `resolve_with_home` that demands a `[defaults]` table whenever a
non-standalone section is present (`crates/devkit-config/src/lib.rs:962`) is
deleted along with the condition that motivated it.

| Key | Before | After |
|---|---|---|
| `worktree_root` | required | optional, derived default |
| `branch_prefix` | required | optional, defaults to empty |
| `baseline_ref` | required | optional, meaning changes |
| `baseline_path` | required | removed |
| `baseline_dir` | absent | optional, defaults to `<worktree_root>/_baselines` |
| `pr_base` | defaults to `staging` | defaults to `main` |

`worktree_root` defaults to the primary checkout's parent directory joined with
the checkout's own directory name and `_worktrees`. A primary checkout at
`~/Git/lev/devkit` derives `~/Git/lev/devkit_worktrees`. The underscore
separates the suffix from the project name, which commonly contains hyphens.
Git has no equivalent setting to inherit: as of 2.53 the only worktree config
keys are `worktree.guessRemote` and `worktree.useRelativePaths`, and `<path>` is
a required positional on `git worktree add`.

AGENTS.md and `docs/configuration.md:141` both document `<name>-worktrees` with
a hyphen, so both move to the underscore form as part of this stage. Devkit's
own `devkit.local.toml` keeps an explicit `worktree_root`, since renaming an
existing worktree directory is not something this change should do.

**Where the derivation lives.** `devkit-config` asks git nothing by design
(`crates/devkit-config/src/lib.rs:925`), and the derivation needs the primary
checkout. `devkit_common::config::resolve` already computes `main_checkout` and
`checkout_root` and passes both into `devkit_config::resolve`, so it computes
the default path too and passes it as one more parameter. `devkit-config` treats
it as the fallback for an unset key and stays git-free.

**When the derivation is unavailable.** `git::main_checkout` returns `None` for
a bare main worktree, and `checkout_root` errors from inside a bare directory.
In both cases `worktree_root` stays empty and the error surfaces at the point of
use, naming the key: `issue setup` cannot place a worktree, `devrun up --role
baseline` cannot place a baseline. Config resolution itself succeeds, which is
the whole point of stage A. A derived root that lands somewhere unwritable, such
as a primary checkout at `/repo` deriving `/repo_worktrees`, fails at creation
with an error naming `defaults.worktree_root` as the fix.

`baseline_ref`, when set, is the merge-base target rather than a checkout
target. The two consumers read it differently, deliberately. `issue setup` and
`issue checkout-pr` still cut a new worktree from the target's tip, because a
new branch belongs at the current head of what it will merge into. The baseline
takes the merge base of that target with the worktree's HEAD. Both use the same
resolution for the target itself, so an unset `baseline_ref` falls back to
`origin/HEAD` for setup as well.

`baseline_dir` resolves as `PathKind::Host`, the same as `worktree_root`, so a
relative value anchors to the directory of the layer that declared it.

**`baseline_path` migration.** A project layer that still sets it is a hard
error naming `baseline_dir` and stating that the value is now a container rather
than a checkout. The home layer warns once on stderr and ignores the value.
Erroring there would reproduce the exact `devkit brief` failure stage A exists
to remove, in every git repository on the machine, and the documented example at
`docs/configuration.md:572` sets the key. Provenance already records which layer
each leaf came from, so the distinction is available where the check runs.

### 2. Resolving the pin

```
target(cfg, repo):
    cfg.defaults.baseline_ref            if non-empty
    else `git symbolic-ref --short refs/remotes/origin/HEAD`
    else Err naming both `defaults.baseline_ref`
         and `git remote set-head origin -a`

pin(worktree, target) -> full sha:
    `git merge-base HEAD <target>`
```

No fetch. Merge base reads local refs, and a stale target gives the same answer
as a fresh one, because extending a branch with new commits does not move its
merge base with another branch. The pin changes when the worktree is rebased and
at no other time, which is the behavior a baseline wants in both directions: it
follows a rebase exactly once, and it does not chase the remote tip while work
continues.

That assumption has one hole worth naming: rebasing onto a ref fetched from a
different remote, `upstream/main` while `origin/main` is stale, leaves the merge
base at the stale tip. The baseline is then older than the true fork point and
`--stat` detection reports the target's own commits as changed apps. The fix is
to set `baseline_ref` to the remote actually being rebased onto.

`git merge-base` exits 1 with empty output when HEAD and the target share no
history, which `Git::output` reports as a bare failure with no stderr. An orphan
branch and a fork PR whose base was force-pushed both reach it, so the call is
wrapped with an error naming both refs and suggesting `baseline_ref`.

Rejected as an intermediate fallback: the branch's own `@{upstream}`. After a
push it is usually `origin/<same-branch>`, whose merge base with HEAD is the
branch's own pushed tip. Useless as a baseline, and wrong in a way that looks
correct.

### 3. Layout and identity

A baseline lives at `<baseline_dir>/<first 12 hex of sha>`, created with
`git worktree add --detach <path> <sha>` from the primary checkout. Twelve
characters is 48 bits against a few dozen directories per repository, and it
leaves Windows path headroom that a 40-character name would spend.

Every baseline carries a marker at `<baseline>/.devkit/baseline.toml`:

```toml
sha = "d13d90b724bf8a3c…"

[apps.api]
fingerprint = "9f2c…"
[apps.web]
fingerprint = "1a77…"
```

The marker is written last, after every bootstrap step, and it is what makes a
baseline complete. Each entry under `apps` records that the app was prepped and
what it was prepped from: a hash over that app's `prep_files`, `setup`, and the
`worktree_include` patterns. A bare list of names would go stale silently. A
project that adds a key to an app's `.env.local` would give issue worktrees the
new key and baselines the old one forever, because reuse would see the name
listed and skip it, and `issue sync-includes` reaches baselines no longer, since
it enumerates through the filtered `discover`
(`src/bin/devkit/issue/sync.rs`). With a fingerprint, the next
`devrun up` re-preps the app whose config moved and no new verb is needed.

The marker does three further jobs one file can do and `git rev-parse` cannot:

- **Completion.** A directory without a marker is a half-built baseline, whatever
  its HEAD says. Reuse requires the marker, so a bootstrap that died partway is
  finished on the next run instead of being accepted forever.
- **Identity.** Collisions compare the marker's sha. A candidate directory that
  is not a worktree at all reports an error from `git rev-parse`, not a
  mismatch, so keying identity on git alone would silently strand it.
- **Filtering.** The marker is how every worktree consumer tells a baseline from
  an issue worktree. See section 5.

Lookup and collision are one probe, and it classifies every state a slot can be
in. Try `<short>`, then `<short>_2`, `_3`, and so on:

| Slot | Meaning | Action |
|---|---|---|
| Marker parses, sha matches | this baseline | reuse |
| Marker parses, another sha | a real collision | next candidate |
| Directory exists, marker absent or unparseable | an interrupted bootstrap | force-remove and rebuild in place |
| Nothing there | free | create here |

The interrupted case is the common one and it needs its own row. `with_cleanup`
(`src/bin/devkit/issue/checkout.rs:293`) matches on `Err`, so it unwinds a
failed step but not a `SIGINT` during a long install. Leaving that directory
classified as "occupied" would strand it forever: the baseline moves to `_2`,
`baseline prune` reports rather than removes it, and because the worktree filter
keys on the marker, the half-built tree becomes an `UNKNOWN` row that
`issue end --clean-worktree` can delete. Rebuilding in place under the per-sha
lock is what makes an interrupted bootstrap self-heal.

Marker and record writes go through a temporary file plus rename, so a crash
leaves the previous state rather than a truncated file that parses as neither.

The worktree record stores both the full sha and the resolved path:

```toml
[baseline]
sha  = "d13d90b724bf8a3c…"
path = "/home/lev/Git/lev/devkit_worktrees/_baselines/d13d90b724bf"
```

The path is what `issue end` deletes and what the reference count is keyed on.
Storing only the sha would orphan every existing baseline the moment someone
changed `baseline_dir`.

The ports holder stays the baseline directory path, so `holder_alive` keeps
meaning "the directory exists" and removing a baseline frees its registry rows
with no additional code.

**Stopping baseline servers.** Because the holder is the baseline path, a plain
`devrun down` from the worktree that started those servers matches nothing
today. `build_selector` gains one rule, and `touches_foreign` gains the same one
or the narrowed selection still trips the gate: the baseline path named by the
current worktree's record is not foreign to that worktree *when that worktree is
its sole referencer*. A shared baseline stays foreign to everyone, so the
cross-worktree TTY gate still covers every case where stopping it affects
another worktree, which is what that invariant protects.

The check and the kill both happen under the per-sha lock. Computing the sole
referencer without it is a time-of-check race the design's own reuse path makes
reachable: between the check and the kill, another worktree's `devrun up` takes
the lock, writes its record, and, because `up` is idempotent for a live pid,
reports these servers as its own running baseline. The kill would then land
after that worktree was told its baseline was ready. Since `up` writes its
record under the same lock, holding it across the kill closes the window. The
cost is one `git worktree list` plus a record read per worktree, and `cmd_down`
already shells out to git for `toplevel`.

**Repinning stops what it abandons.** A rebase changes a worktree's merge base,
so its next `devrun up` writes a record naming a different baseline. The old
baseline's servers keep running: their pid is alive and their holder directory
still exists, so nothing reclaims them. The sole-referencer rule reads the
*current* record, so the moment the record is rewritten those servers become
foreign to the only worktree that would ever stop them, unreachable without
`--holder` and a TTY, unreachable from MCP, and enough to make `baseline prune`
refuse the directory forever. Every rebase in an agent session would leak a
server set and a checkout.

So repin is not a record rewrite alone. Before writing the new pin, `devrun up`
brings down the rows this holder started against the previous baseline path,
using the existing `bring_down_ports`. The old baseline is then referenceless
and portless, and prune can collect it.

**Stray attribution.** `strays::managed_roots` swaps its `baseline_path` entry
for `baseline_dir`. `attribute_holder` currently takes the first root that
prefixes a cwd (`crates/devkit-ports/src/strays/mod.rs:259`), and at the default
`baseline_dir` sits under `worktree_root`, so the existing rule would claim a
baseline stray for `<worktree_root>/_baselines` before any new rule ran. Root
matching therefore changes to longest-prefix-wins, and the baseline rule mirrors
the `worktree_root` one: holder is `baseline_dir` plus one path segment.

### 4. Lazy creation

Creation runs inside the group-building block of `cmd_up`, where `ensure_fresh`
is today (`src/bin/devkit/run/mod.rs:658`), which is *before* `resolve_ports`. A
bootstrap that installs dependencies can outlast `RESERVATION_GRACE_SECS`, so it
must not sit between an allocation and its bind.

Every path takes the per-sha lock, following the per-stem lock pattern in
`crates/devkit-docs/src/locks.rs`:

- **Marker present and its sha matches.** Prep any app the marker does not list
  or whose fingerprint has moved, update the marker, write the calling
  worktree's record, release.
- **Otherwise.** Bootstrap, write the marker, write the record, release. A
  directory present without a usable marker is force-removed first, per the
  probe table in section 3.

The reuse path takes the lock too. Skipping it lets a worktree find a baseline,
begin allocating ports, and have a concurrent `issue end` delete the tree before
any record names it, leaving servers running in a deleted directory.

Bootstrap order:

1. `git worktree prune` in the primary checkout, then
   `git worktree add --detach <path> <sha>`. The prune clears a registration
   left behind by a directory someone removed by hand, which otherwise fails
   every later creation for that sha with "missing but already registered".
2. `worktree_include` copies, through the existing parallel worklist at
   `crates/devkit-common/src/worktree.rs:211`.
3. `prep_files` and per-app prep for the apps being run.
4. `hooks.after_worktree_create`.
5. The marker.

Steps 1 through 4 run under `with_cleanup`, the pattern
`src/bin/devkit/issue/checkout.rs:428` already uses, so a failure removes the
partial worktree rather than leaving a directory for the next run to inherit.

**Reuse preps apps it has not prepped.** Step 3 preps the apps being run, and a
baseline created for `api` is later reused by a worktree running `web`. The
marker records which apps have been prepped, and reuse preps any that are
missing before releasing the lock. Without this, the second worktree's server
starts with no prep files and no `setup` run.

**Render context.** `prep_apps` renders `prep_files` over `issue`, `slug`,
`branch`, `app`, and `worktree`, and propagates render errors with `?`
(`src/bin/devkit/issue/setup.rs:100`). Under strict rendering
(`crates/devkit-common/src/template.rs:13`), a template naming `{{ issue }}`,
which a per-issue database name would, hard-fails baseline creation.

A baseline therefore renders with a stable synthetic identity rather than with
absent keys: `issue`, `slug`, and `branch` all take the value
`baseline-<short sha>`, plus `role = "baseline"` and `sha`. Existing templates
render unchanged, and the resources they name are per baseline: one shared
database for one shared tree, distinct across shas, which matches what the
directory already is. Issue-role creation gains the same `role` key set to
`"issue"`, so a project that wants to branch can.

This reverses an earlier decision to omit `issue` and `slug` so that hooks
referencing them would fail loudly. Loud failure is the right call for a hook,
which is fail-open and only warns, and the wrong call for prep, which aborts the
whole bootstrap. One context serving both means the prep constraint wins.

`role` and `sha` become reserved context keys. `template::render` merges
`[templates.variables]` *underneath* the context, so a context field of the same
name wins (`crates/devkit-common/src/template.rs:22`). A project that already
defines `variables.role` would have it shadowed with no diagnostic, in both
baseline and issue prep. Config loading therefore rejects a
`[templates.variables]` key that collides with a context key, and
`docs/configuration.md` lists the reserved names.

### 5. References and deletion

The reference set is derived, not stored. The enumeration is
`devkit_common::worktree::discover` (`crates/devkit-common/src/worktree.rs:65`)
over `git worktree list --porcelain`; the count reads each worktree's
`.devkit/issue.toml` and groups referencing worktrees by baseline path.

A stored registry was considered and rejected. It drifts the moment anything
bypasses devkit: a plain `git worktree remove` leaves a phantom reference that
keeps a baseline alive forever, and the fix for that is a liveness scan, which
is the derived approach with an extra file to maintain. The docs cache needs a
registry because its consumers are project roots anywhere on the machine and
nothing enumerates them. Baseline consumers are all worktrees of one repository,
and git enumerates them.

**Baselines are linked worktrees, so `discover` returns them.** Without a filter
each baseline becomes an `UNKNOWN` row in `issue status`, `issue info`, and
`issue dashboard`, `issue sync-includes --all` copies includes into every
baseline, and `issue end --clean-worktree <path>` removes one while its
referencers still point at it. `discover` therefore skips any worktree carrying
the marker file. The check is a filesystem stat with no config, which is what
keeps it available in `devkit-common`.

The stat reads through `metadata()` rather than `exists()`. `Path::exists` folds
every error into `false`, so a permission failure or an unreadable path would
classify a baseline as an issue worktree, which is the unsafe direction: an
`UNKNOWN` row, includes copied in, and `--clean-worktree` able to remove it. Any
error other than `NotFound` skips the worktree and warns.

**An unreadable record counts as a reference.** `record::read` returns `None` on
any deserialize failure (`crates/devkit-common/src/record.rs:45`), so a
truncated record is indistinguishable from a worktree that never had one. Read
as "no reference", a sibling's `issue end` would delete a baseline out from
under a worktree actively serving from it. The count therefore treats a record
that exists but does not parse as a reference and refuses the deletion, naming
the worktree. Atomic record writes make this rare; counting it as a reference
makes it safe when it happens.

**Deletion drops the caller's reference before counting.** This is the ordering
the protocol depends on:

```
drop_reference(repo, baseline_path, force):
    // the caller's own worktree is already gone by here
    take <baseline_dir>/.lock exclusively
    take the per-sha lock for baseline_path
    recompute referencers
    if none remain:
        refuse if live registry rows hold baseline_path, unless force
        git worktree remove --force <baseline_path>
        remove the directory
```

Counting before the caller's reference is gone leaks the baseline under the
common case rather than a rare one. `issue end` removes worktrees in parallel
threads within one process (`src/bin/devkit/issue/end.rs:441`), so ending two
worktrees that share a baseline has both threads observe two referencers, both
decline, and both then remove their own trees. Sequencing the caller's removal
first makes the last drop observe zero.

In `cleanup`, that means: read the record (which already happens, for the
summary path), `git worktree remove` the issue worktree, then call
`drop_reference` with the recorded baseline path. `issue end --force` covers the
live-rows refusal as well as the dirty-worktree one.

`git worktree remove` is always `--force` for a baseline. Without it, any
non-ignored untracked file refuses removal, and a baseline holds
`worktree_include` copies and rendered prep files by construction. A baseline
carries no user work, so there is nothing for the guard to protect, and leaving
it in reproduces the class of failure this design removes.

A partial removal is possible on Windows, where a file a live server holds open
aborts the delete midway. Git chooses its own deletion order, so the marker may
or may not survive. Either way the tree is recoverable: with the marker it is a
baseline the probe table reuses or rebuilds, without one it is an
interrupted bootstrap the probe table rebuilds in place. The live-rows refusal
is what keeps this rare, since it declines before the delete starts.

**Deletion at `issue end` is best-effort; prune is the guarantee.** A process
that dies between removing its worktree and calling `drop_reference`, or a
`drop_reference` that fails, leaves a baseline with zero referencers. Nothing is
corrupted and nothing is lost: `devrun baseline prune` collects it and the
`baseline_orphans` doctor row reports it in the meantime.

Lock order is dir-wide then per-sha; creation takes only the per-sha lock, so
there is no cycle.

**The per-sha wait is unbounded.** A bound would break the guarantee the lock
exists for: a worktree racing a ten-minute bootstrap is supposed to wait and
then find a finished tree, and with a timeout it gets an error instead. It is
worse for prune, which holds the dir-wide lock while every `issue end` on the
machine queues behind it, so a timed-out per-sha acquisition abandons a sweep
midway with the dir-wide lock contended.

Hooks run under the per-sha lock, so a hook that itself runs
`devrun up --role baseline` for the same sha would block on a lock its own
process holds, and an unbounded wait makes that a hang rather than an error.
Re-entrancy is prevented rather than detected: the hook runner sets an
environment marker naming the sha it is bootstrapping, and lock acquisition for
that sha inside that process fails immediately with an error naming the hook.
This is the name-based guard `crates/devkit-docs/src/locks.rs:31` uses for
control stems, not a timeout; `locks::hold` itself blocks unboundedly on
`.write()` at line 93.

### 6. New surface

- `devrun baseline list` reports each baseline: short sha, referencing
  worktrees, prepped apps, size on disk, and whether a live server holds it.
  Enumeration is `read_dir(baseline_dir)` rather than the worktree list, so a
  directory git no longer knows about is visible and reportable rather than
  invisible. Size walks go through `pool::jwalk_parallelism`, evaluated on the
  thread that builds the walk rather than inside `pool::install`.
- `devrun baseline prune [--dry-run] [--force]` removes every baseline with no
  referencer, and reports directories under `baseline_dir` that carry no marker
  rather than deleting them.
- `devkit doctor` gains a `baseline_orphans` row reporting count and total size.
  Read-only, shaped like the existing `devrun_strays` row, so agents get
  detection without a mutating path.

Not attached to `devrun reap`. Reap kills processes it does not track, which is
why it is TTY-gated with no bypass and never exposed on MCP; deleting a
rebuildable checkout is not in that risk class, and inheriting that gate would
stop an agent session from reclaiming disk for a safe operation.

### 7. Code removed

`ensure_fresh` and `head_at` in `src/bin/devkit/run/baseline.rs` are replaced by
pin resolution plus creation. With them go the cross-worktree `reset --hard`
under a running server, the dirty-tree abort that blocks an unrelated worktree's
servers, the fetch on the baseline path, and the shared-tip semantics that made
two worktrees contend for one directory.

## Invariants

Reserve-before-bind, the reservation grace, `down` without prune, the supervisor
deciding crash versus stop, `up` idempotence, and sequence re-resolution are all
unaffected, given that creation stays ahead of `resolve_ports` as section 4
requires.

The cross-worktree `down` gate narrows deliberately and only for a sole
referencer, evaluated and acted on under the per-sha lock, per section 3. The
MCP `devrun.down` handler stays root-scoped and gains nothing. Repin brings down
the baseline rows it abandons, so the narrowing never leaves a set of servers
that only a TTY can reach.

`with_lock` stays minimal in the ports registry. The new locks are separate
files. `drop_reference` does read the registry under the dir-wide lock, which is
a daemon round trip when `devkitd` runs, and creation holds the per-sha lock
across a full bootstrap. Both are accepted: they guard directory creation and
deletion, not the port allocation path.

Holder-as-path-existence is preserved.

## Migration

`baseline_path` in a project layer is a hard error naming `baseline_dir`; in the
home layer it warns and is ignored. Existing `_baseline` directories are the
user's to remove, and the message gives the `git worktree remove` invocation.

Records written before this change have no `baseline` table. The field is
`Option`, `#[serde(default, skip_serializing_if = "Option::is_none")]`, matching
how `summary` and `pr` already handle the same problem. A worktree with no
baseline record has no reference, and its next baseline run writes one.

`schema/devkit-config.json` regenerates with `DEVKIT_UPDATE_SCHEMA=1 cargo test`
and its drift test covers the result.

Each stage lands as `feat!` with a `BREAKING CHANGE` footer naming its own
change: the `pr_base` default in stage A, the `baseline_path` removal in
stage B.

## Testing

Pin resolution: `baseline_ref` wins over detection; detection falls back to
`origin/HEAD`; with neither, the error names both fixes; unrelated histories
produce the wrapped error rather than a bare exit 1; the merge base moves after
a rebase and does not move when the target advances.

Sharing and locking: two worktrees at one merge base resolve to one directory
and one creation; two threads contending for one slot lock serialize rather than
interleave, which is what makes a reuse and a deletion of the same baseline
mutually exclusive; two different slots do not block each other, and the
directory lock nests around a slot lock in that order.

Deletion ordering: two `issue end` threads sharing one baseline delete it
exactly once, which is the regression test for the leak this design's ordering
exists to prevent; live registry rows refuse without `--force`; a baseline
holding untracked prep output is still removed.

Completion and identity: each row of the probe table gets a case, including a
directory whose marker is absent and one whose marker is corrupt, both rebuilt
in place rather than skipped to `_2`; a marker holding another sha forces `_2`;
a marker written into a directory git has no registration for still lists; a
record write leaves no temp file behind, which is what pins the rename that
makes it atomic.

Bootstrap: `worktree_include`, prep files, and `after_worktree_create` all run
on creation; a `prep_files` template naming `{{ issue }}` renders against the
synthetic identity; reuse for a second app preps that app; reuse after an app's
`prep_files` changes re-preps it on fingerprint mismatch; the fingerprint is
pinned to a known digest, so a hash that shifts between toolchains fails rather
than silently re-prepping forever; a `[templates.variables]` key colliding with
`role` or `sha` is rejected at load, and an ordinary key is not.

Locks: two threads contending for one slot serialize, which is the cross-process
guarantee `flock` provides and the reason a locked function never calls another
locked function.

Repin: a rebase brings down the previous baseline's rows for this holder before
the record is rewritten, and the abandoned baseline is then prunable.

Reference safety: a worktree whose record exists but does not parse counts as a
referencer and blocks the deletion of every baseline, since a record that does
not parse cannot say which one it names.

Filtering: a baseline does not appear in `issue status`, `issue info`, or
`issue sync-includes --all`.

Config: `baseline_path` errors from a project layer and warns from the home
layer; `[defaults]` carrying only `branch_prefix` loads; a config with no
`[defaults]` at all loads with a non-standalone section present; the derived
`worktree_root` matches the primary checkout's sibling; a bare main worktree
leaves it empty and the error arrives at use.

Platforms: everything but stray attribution runs on Windows and macOS. Two
Windows behaviors are asserted rather than assumed: `git worktree remove` fails
while a server holds files open, which is what makes the live-rows refusal the
load-bearing guard there, and an orphaned baseline server has no `devrun status`
untracked row because `process_pass` is compiled out
(`crates/devkit-ports/src/strays/mod.rs:346`).

## Rejected alternatives

**Merge base against `pr_base`.** No new key and no new detection, and the
baseline would agree with the PR target by construction. Rejected because it
welds two concerns together: a repository that opens PRs into a release branch
but compares against `main` could not express both, and a stacked branch has no
way to name its parent.

**Recording the fork commit at `issue setup`.** Exact and detection-free.
Rejected because it goes stale after a rebase, silently, and is absent on every
existing worktree and every `checkout-pr` worktree. It also stores a commit
rather than a policy, so a wrong pin can only be fixed by editing a record.

**One baseline per worktree.** Simplest ownership, no reference counting.
Rejected on cost: N checkouts and N dependency installs, and two worktrees cut
from the same commit build the same tree twice, which is the common case right
after a rebase.

**Collecting a baseline on repin.** No accumulation, and one deletion rule.
Rejected because it gives `devrun up` a side effect of deleting directories, and
because a daily rebase would discard a fully built checkout that rebasing back
would want again.

**`issue end` not deleting at all.** Exactly one deleting code path. Rejected
because normal start-and-finish work would then grow disk without bound, and
`issue end` already knows the reference is gone.

**Identity from `git rev-parse HEAD` with no marker file.** Git already records
the commit, so a marker looked redundant. Rejected because HEAD cannot express
"this bootstrap finished", cannot distinguish a stray directory from a baseline
for the worktree-list filter, and reports an error rather than a mismatch for a
directory that is not a worktree.
