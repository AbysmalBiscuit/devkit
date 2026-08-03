# docm correctness: make wrong answers loud

**Date:** 2026-08-03
**Status:** approved design, pending implementation
**Source:** `scratch/docm_issues.md` — a field report from an agent that resolved
four libraries for SWE-10805 and got three of them wrong on the first attempt.

## Problem

docm's failure mode is not that it is unhelpful. It is that its wrong answers are
indistinguishable from its right ones. An agent runs `docm info`, gets an
authoritative-looking version and path, reads source from a different version
than the project runs, and cites it with confidence.

Six defects, all confirmed against the code:

| # | Defect | Cause |
|---|---|---|
| 1 | `info` reports a version the checkout does not have | `resolve.rs:57` materializes every `--ref` pin into a dir named `default`; `cache.rs:165` returns an existing dir without checking HEAD |
| 2 | Monorepo version selection picks the highest, not the applicable, version | `lockfiles.rs:74`; aliased bun entries (`"h3-v2": ["h3@2.0.1-rc.20"]`) counted as the base package at `lockfiles.rs:144` |
| 3 | Registry probe order silently wins on name collisions | `lookup.rs:40` probes crates.io → npm → PyPI regardless of the project |
| 4 | Scoped monorepo tags never match | `tags.rs:28` strips the scope: `@hey-api/client-fetch` → `client-fetch@0.13.1` |
| 5 | `remove` is not an alias for `rm` | `docm.rs:51` |
| 6 | Every git-ecosystem checkout lives at `<lib>/default` | same root cause as 1 |

Issues 1 and 6 are one bug. A checkout directory whose name is independent of
its content cannot be verified by eye, cannot be safely shared between
concurrent sessions, and silently survives a re-pin.

## Governing principle

Every answer docm gives is either provably correct or a hard error. No silent
fallback, no warning that scrolls past among success lines, no state where the
manifest and the disk disagree.

## Design

### 1. Checkout naming

A checkout directory is named for the ref that produced it, with `/` replaced by
`~`:

```
h3/v1.15.11
next/v14.2.4
openapi-ts/@hey-api~client-fetch@0.13.1
nitropack/v2.13.4                          # lockfile 2.13.4 → tag v2.13.4
some-lib/5f72330a1b2c3d4e5f6071829304a5b6c7d8e9f0
```

`~` is illegal in a git ref name, so the mapping is injective: no valid ref can
produce a `~`, and `release~2.x` can only have come from `release/2.x`. No hash
suffix, no collision table, and the transform is reversible.

These cases are hard errors rather than silent renames:

- a ref longer than 255 bytes (filesystem component limit)
- a ref whose dirname would collide with a cache control name. The reserved set
  is `repo.git` and `meta.toml`; both are valid git ref names. Any control file
  added later joins the set.
- a ref not representable on the host filesystem. Git permits characters Windows
  forbids — `|`, `<`, `>`, `"` are all legal in a ref name — and Windows also
  reserves device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`,
  with or without an extension). The check is host-specific, so a ref that works
  on Linux can fail on Windows; the error names the offending character or name.
- a collision under case folding or Unicode normalization — `V1.0` vs `v1.0` on
  macOS/Windows, or NFC vs NFD forms of the same visual ref. Detected by
  comparing the incoming ref against the `raw_ref` recorded in `meta.toml`, and
  reported as "two refs differ only by case/normalization; pin explicitly".

The `default` dirname is retired. Nothing shares a directory any more.

### 1.1 Library names

Library names take the same encoding, and this fixes a live data-loss bug rather
than merely preparing for the new layout.

`LibCache::new` (`cache.rs:87`) joins the library name onto the cache root, so a
scoped name nests: `@hey-api/client-fetch` becomes
`docs/@hey-api/client-fetch/`. Prune then reads the cache root
(`refs.rs:187`), sees `@hey-api` as a library, asks `version_worktrees()` for its
contents, and gets back `client-fetch` — which it treats as an unreferenced
worktree and deletes. The field report registered exactly that name. This is
reachable in shipped 0.12.1 with `docm add @types/node` or any scoped package.

Library directories therefore become `@hey-api~client-fetch/`, with the same
reserved-name and representability checks and the same hard errors.

**The injectivity argument does not transfer, and needs its own rule.** `/` → `~`
is injective for *refs* because git forbids `~` in a ref name. A library name is
not a ref — it comes from `LibEntry.name`, a registry package name, or a URL
leaf, none of which are constrained that way — so `a/b` and `a~b` would both
encode to `a~b`. A `~` in a library name is therefore a hard error at `add`
time, which restores injectivity by making the domain match the assumption. No
real package name on crates.io, npm, or PyPI contains `~`; the error names the
character and suggests `--package` to set a different registry name.

`version_worktrees()` gains an assertion that it is reading a directory
containing `repo.git`, so a mis-scan fails loudly instead of proposing
deletions.

### 2. Identity and verification

`meta.toml` gains an `origin` field and a `worktrees` map: dirname →
`{ raw_ref, commit }`.

**Repository identity.** `ensure_clone` (`cache.rs:107`) returns early when
`<lib>/repo.git` exists, without checking that it was cloned from the URL now
being asked for. A global entry on upstream and a project overlay on a fork
therefore share one bare repo, and both report `status ok` against whichever was
cloned first. `meta.toml` records the origin the clone was made from; a
resolution whose entry names a different repo is a hard error naming both URLs,
not a silent reuse. `info` and `list` print the origin so the mismatch is visible
before it misleads anyone.

Every resolution:

1. selects a ref (§3)
2. resolves the ref to a commit against the **local** bare repo — no network; a
   miss triggers exactly one fetch, then a retry, then a hard error. The lookup
   is option-terminated (`git rev-parse --verify --end-of-options`) so a ref that
   looks like an option cannot be misparsed, and qualification depends on the
   shape of the ref:
   - a ref already starting with `refs/` is used verbatim — prefixing
     `refs/tags/` onto `refs/tags/v1` would produce a nonexistent ref
   - a 40-character hex string is tried as a raw object name
   - anything else is tried as `refs/tags/<ref>` and `refs/heads/<ref>`. Matching
     both is a hard error demanding the qualified form, rather than silently
     taking git's precedence order.
3. materializes `<lib>/<slug(ref)>` at that commit if absent
4. if the directory exists, compares its HEAD to the resolved commit; a mismatch
   re-points the worktree (`git checkout --detach <commit>`) and reports
   `status repaired`
5. verifies the worktree is **clean** — `git status --porcelain
   --untracked-files=no`. A tracked file modified inside the cache is a hard
   error naming the files, because source read from a dirty checkout is not the
   released source, and `status ok` would otherwise be a false claim. Untracked
   files are ignored: they add nothing an answer could be cited from.
6. records `(workspace, lib, ref, version, commit, dirname)` in the reference
   registry

Step 5 costs a `git status` on every path-returning resolution — hundreds of
milliseconds on a large checkout. An earlier draft moved it to `devkit doctor`
to avoid that, which was wrong: `info` prints `status ok`, agents read `info` and
not `doctor`, and a truth claim that can be false is the exact defect this design
exists to remove. `doctor` keeps a deeper sweep across every materialized
checkout; `info` pays for the claim it makes.

A SHA pin is immutable, so step 4 is a pure corruption check for it. A *branch*
pin keeps a stable dirname while its commit moves; that directory does change
under a concurrent reader when `sync` fetches. This is accepted rather than
special-cased: branch checkouts only exist when someone explicitly asks for one,
and `info`'s `commit` line makes the movement visible.

**Tags are not immutable, and the spec must not assume they are.** `cache.rs:137`
fetches `--force --tags`, which accepts an upstream tag that was deleted and
recreated at a different commit. Policy: a tag whose recorded commit no longer
matches the one it resolves to is treated as a re-pin, not corruption — the
directory is re-pointed and the move is reported on stderr and in `info`:

```
docm: tag v1.15.11 moved 5f72330a1b2c… → 9e41a08b7d3f… upstream; h3/v1.15.11 re-pointed
```

Reported rather than silently accepted, because a moved release tag means any
answer cited from the old checkout was cited from source that no longer exists
under that name.

**Deleted tags must also disappear locally.** `cache.rs:137` fetches `--prune`
but not `--prune-tags`, so a tag deleted upstream stays resolvable in the local
bare repo forever — and would keep resolving, and keep reporting `status ok`,
for a release that has been withdrawn. Sync fetches with `--prune-tags`, and a
pinned ref that vanishes upstream becomes a hard error naming the pin.

`git describe --tags` is used nowhere. Issue 4 established that it names the
nearest *reachable* tag, which in a package monorepo is routinely a different
package's release.

### 3. Version selection

Selection order, first hit wins:

1. **Manifest `ref`** — used verbatim.
2. **Git-ecosystem entries** always have a ref (§6.1). There is no implicit
   fallback state. A git entry already in a manifest without a `ref` — written
   by 0.12.x — is a hard error naming the entry and suggesting
   `docm add <name> --ref <branch>`, or `--allow-default-branch` for a one-off.
3. **Importer-graph resolution** for js/rust/python entries. Resolve the version
   the workspace *installs*, not a version that merely satisfies its declared
   range. Selecting the highest candidate satisfying a range is still a guess: a
   workspace declaring `^1.0` installs 1.1, but a nested copy of 1.9 elsewhere in
   the lockfile also satisfies `^1.0` and would win.

   Every supported lockfile records the resolved version per importer, so the
   answer is a lookup rather than a search:

   | Lockfile | Importer entry |
   |---|---|
   | `pnpm-lock.yaml` | `importers.<workspace-path>.{dependencies,devDependencies,optionalDependencies}.<pkg>.version` |
   | `package-lock.json` | `packages.<workspace-path>` must declare `<pkg>`; the version then comes from the nearest `node_modules/<pkg>` walking up from `<workspace-path>` |
   | `bun.lock` | `workspaces.<workspace-path>.{dependencies,devDependencies,optionalDependencies,peerDependencies}` names the dep, then `packages` key `<workspace-name>/<pkg>` if present, else the hoisted `<pkg>` |
   | `Cargo.lock` | the member's `[[package]]` entry's `dependencies` list, resolved against the lock's package set |
   | `uv.lock` | the member's `[[package]]` entry's `dependencies`, plus its `dependency-groups` and optional-dependency tables |

   The workspace path comes from walking up from CWD to the nearest manifest
   (`package.json`, or `Cargo.toml` / `pyproject.toml` carrying a dependency
   table), taken relative to the lockfile's directory.

   Three format details that a naive reading of the table gets wrong:

   - **A pnpm `version` is a snapshot locator, not a version.** Real entries
     carry peer qualifiers — `8.0.2(chai@6.2.2)`, `3.20.0(@types/node@25.5.0)` —
     and an alias locator can name a different package entirely. Strip the
     parenthesised suffix and follow an alias locator to its package identity
     before treating what remains as a version.
   - **Every direct-dependency class counts**, not just `dependencies`. The
     table above enumerates them per format; a package declared only in
     `devDependencies` or a uv `dependency-group` is still a direct dependency of
     that workspace.
   - **npm resolution must verify declaration first.** Falling straight back to
     the hoisted root `node_modules/<pkg>` can return a copy hoisted for a
     *different* workspace. Confirm `packages.<workspace-path>` declares the
     package, then resolve by walking `node_modules` upward the way node does.

   **This removes the need for semver range matching, and with it the
   `node-semver` dependency argued for earlier in this design.** Ranges are never
   compared, because the lockfile already recorded which version won.

   Two outcomes are hard errors rather than guesses:

   - the workspace does not declare the package (it is transitive, so its version
     depends on which dependent pulled it). The error lists each candidate
     version with the dependent that requires it, and suggests `--ref`.
   - the importer maps to more than one version — including `uv.lock`
     environment forks, where marker-split resolutions legitimately record
     several versions of one package. Python does not get the "one version per
     workspace" exemption the previous draft gave it.

   **Aliases dissolve.** An npm alias appears as a distinct key whose spec names
   a different package (`"h3-v2": ["h3@2.0.1-rc.20", …]` — the case in the field
   report). Because resolution now starts from the name the workspace *declared*
   and looks up that key, an alias is only ever selected when the workspace
   declared the alias. No suffix heuristic is needed, which also closes the hole
   in the previous rule, where a scoped alias key like `@compat/h3` → spec `h3`
   ended with `/h3` and would have been wrongly kept as a nested copy.

4. **Tag lookup** turns the selected version into a ref (§4). No tag is a hard
   error (§6).

Every resolution states which file decided it, on stderr:

```
docm: next 14.2.4 — apps/lab-os installs it (bun.lock; 4 other versions present)
```

### 4. Tag patterns

`tags::apply` gains the unstripped scoped forms, and the probe order changes:
**package-specific patterns are tried before the generic ones.**

| Order | Pattern | Example |
|---|---|---|
| 1 | `{package}@{version}` | `@hey-api/client-fetch@0.13.1` |
| 2 | `{leaf}@{version}` | `client-fetch@0.13.1` |
| 3 | `{package}-v{version}` | `@hey-api/client-fetch-v0.13.1` |
| 4 | `{leaf}-v{version}` | `client-fetch-v0.13.1` |
| 5 | `{leaf}-{version}` | `client-fetch-0.13.1` |
| 6 | `v{version}` | `v1.15.11` |
| 7 | `{version}` | `1.15.11` |

The previous draft kept `v{version}` first, which is wrong in exactly the repo
shape this issue came from: a package monorepo that tags both
`@hey-api/client-fetch@0.13.1` and a repo-wide `v0.13.1` would resolve to the
repo-wide tag — a different package's release at the same version number.

**The cached `tag_pattern` becomes a hint, not a short circuit.** `resolve.rs:152`
currently applies the cached pattern and returns on the first hit, so every
library resolved before this change keeps its old generic pattern forever and
never sees the new package-specific forms. Resolution now probes in order and
uses the cache only to skip ahead to a previously-successful pattern *after*
higher-priority patterns miss; a `meta.toml` written by 0.12.x has its cached
pattern discarded on first read.

### 5. Failure modes

Silent degradation is replaced by hard errors:

- **No tag for the selected version.** Exits non-zero listing every pattern
  tried and the version it tried them for. Opt back in with a CLI-global
  `--allow-default-branch`.
- **Ambiguous ecosystem at `add` time.** `add` without `--eco` probes all three
  registries rather than stopping at the first hit. Two or more hits is an error
  naming each repo URL and demanding `--eco`. Probe *order* is biased by markers
  found walking up from CWD (`bun.lock` / `package-lock.json` / `pnpm-lock.yaml`
  / `package.json` → js first; `Cargo.toml` → rust; `pyproject.toml` / `uv.lock`
  → python) so the reported ecosystem matches the project when only one hits.
- **Unrepairable HEAD mismatch.** Exits non-zero printing expected and actual
  commits and the path.
- **Case-only ref collision, oversized ref, `repo.git` dirname.** As §1.

### 6. CLI surface

- `add` resolves and materializes before reporting success; any failure restores
  the manifest to its prior content, so a failed add leaves nothing behind. Its
  success output echoes ecosystem, repo, ref, commit and path — a wrong-ecosystem
  registration is visible at a glance.

#### 6.1 `add <git-url>` without `--ref`

The behaviour differs by destination manifest, because the two manifests have
different owners.

**Global** (`~/.config/devkit/docs.toml`, machine-owned and machine-written):
resolve the repo's default branch and write it into the manifest as an explicit
ref. The value is derived from remote `HEAD`, not invented, and no entry is ever
left in an unpinned state. The success output names it as inferred and moving:

```
registered h3 (git) -> https://github.com/unjs/h3
  ref       main (inferred default branch; moves on `docm sync`)
  commit    5f72330a1b2c3d4e5f6071829304a5b6c7d8e9f0
  path      ~/.local/share/devkit/docs/h3/main
  manifest  ~/.config/devkit/docs.toml
```

**`--project`** (a repo-committed `devkit.toml`, hand-maintained shared policy):
hard error. An inferred `main` committed to a repo reads as a deliberate team
decision to every person and agent that later sees it.

```
error: --project needs an explicit --ref for a git URL entry

devkit.toml is shared policy — an inferred default branch would read as a
team decision. Take the ref from this project's dependency or release
policy; don't guess `main`. Then rerun:

    docm add <url> --project --ref <tag|branch|sha>
```

A ref-less entry that permanently *tracks* the default branch is deliberately
not offered. It has no friction and no visible staleness, so it would silently
re-resolve to a newer commit on every sync while continuing to look healthy —
the design's founding bug, re-issued as a feature.
- `sync` becomes fetch → re-resolve → materialize → verify → record, replacing
  the blanket `sync_default` call at `docm.rs:301`. It no longer exists to
  re-point a shared directory, because there is no shared directory.
- `rm` gains `remove` and `delete` aliases.
- `--allow-default-branch` is a clap-global flag.
- `info` prints `ref`, `version`, `commit`, `status` alongside the existing
  fields, and exits non-zero on `status mismatch`:

```
name     h3
repo     https://github.com/unjs/h3
ref      v1.15.11
version  1.15.11
commit   5f72330a1b2c3d4e5f6071829304a5b6c7d8e9f0
status   ok
path     ~/.local/share/devkit/docs/h3/v1.15.11
docs     docs
src      src
notes    apps/api installs 1.15.11; bun.lock also carries h3-v2 (2.0.1-rc.20) as an alias for nitropack
```

`repo` is the origin the bare clone was actually made from, not the manifest's
declared URL — those differing is the failure §2 exists to catch, so printing the
manifest value would hide it.

- `list` gains the ref → commit mapping and the origin per lib, so the state file
  is the index agents read rather than inferring anything from a path.
- `devkit doctor` gains a docs row that verifies each materialized checkout is
  *clean* (`git status --porcelain`), not merely at the right commit. This is
  deliberately not on the resolve path: the check costs hundreds of milliseconds
  on a large checkout, and it would run on every `docm info` to defend against a
  failure — someone editing files inside the cache — that no other part of this
  design assumes.

### 7. Reference registry and prune

`RefRow` grows `ref` and `commit` beside the existing `version`, both
`#[serde(default)]` so rows written by 0.12.x still parse; an empty `commit`
marks a legacy row.

**Rows are keyed by workspace, not by project.** `Data::record` (`refs.rs:60`)
keys on `(project, lib)`, where `project` is the directory holding the lockfile.
Importer-graph resolution (§3) makes that key wrong: two workspaces under one
monorepo share a lockfile directory but legitimately install different versions
of the same library, so the second to resolve would overwrite the first's row —
and prune would then reclaim a checkout the first workspace is actively reading.
The key becomes the workspace directory that resolution actually used.

`refs::current_version` (`refs.rs:162`) currently returns the literal `"default"`
for every ref-bearing and git-ecosystem entry. It must compute the same dirname
the resolver would, or prune mis-plans against the new layout. The unconditional
`d != "default"` exemption at `refs.rs:143` goes away with it — an unpinned
checkout is protected by its own reference row like everything else.

**Migration.** The previous draft claimed migration needs no command, and was
internally inconsistent: if `current_version` computes the *new* dirname for a
legacy row, then a `docm prune` run before that project has re-resolved sees
`default` referenced by nobody and deletes the only materialized checkout. So:

- a row with an empty `commit` is a legacy row, and `current_version` returns
  `default` for it — it keeps protecting the old directory
- a legacy row is retargeted only by a *successful* materialization of the new
  directory, which writes `ref` and `commit` in the same registry commit
- a `default` directory is therefore reclaimed exactly when the last project
  referencing it has re-resolved, whatever order prune and resolve run in

Retargeting is a *retirement*, not an upsert. A legacy row is keyed by the
lockfile directory while its replacement is keyed by a nested workspace, so an
upsert would never match it and the legacy row — and its `default` directory —
would survive forever. When a workspace-keyed row for a library commits, any
legacy row for the same library whose key is an ancestor directory of that
workspace is dropped in the same registry commit.

**An upgrade pass handles what cannot migrate lazily.** A 0.12.x cache already
on disk holds two shapes the new code cannot simply reinterpret: nested scoped
library directories (`@scope/pkg/`), which would trip the `repo.git` assertion on
the parent `@scope` directory rather than being reclaimed, and `meta.toml` files
with no `origin` field. On first run against such a cache, under the per-library
lock:

- a nested `@scope/pkg/` directory containing `repo.git` is renamed to
  `@scope~pkg/`; a `@scope/` directory left empty afterwards is removed
- a missing `origin` is bootstrapped from the bare repo's own
  `git config remote.origin.url`, which is ground truth for where it was cloned
  from, rather than assumed from the manifest

**Ref-less global git entries get a durable migration, not a permanent error.**
§3.2 hard-errors on a git entry with no `ref`, which for entries written by
0.12.x is a papercut with no exit. `docm sync` infers the repo's default branch
and records it in the *global* manifest, exactly as `add` now does (§6.1);
`--project` entries still refuse, for the same reason `add --project` refuses.
The hard error names `docm sync` as the fix.

**Prune takes the per-library lock**, and holds it across its live-registry
recheck and the deletion itself. Re-checking under the registry lock alone is not
enough: a resolve can materialize a directory, then wait on the registry lock,
and have prune delete that directory before the resolve's row is recorded. Since
materialization already runs under the per-library lock (§9), prune holding the
same lock makes the two mutually exclusive and closes the window at
`docm.rs:361` — no session leases required. What leases would additionally buy
is protection for a reader holding a path it has already been given, which stays
out of scope.

### 9. Concurrent access

Several agent sessions share one cache root, and today nothing serializes them.
Two classes of damage are in scope:

**Lost updates.** `write_meta` (`cache.rs:75`) and both manifest writers
(`manifest.rs:187`, `:219`) are read-modify-write over a whole file with no lock,
so two concurrent `docm add` calls silently drop one entry, and two concurrent
resolutions of the same library can drop a `layouts` entry or a cached tag
pattern. Fix: every manifest and `meta.toml` write becomes
write-temp-then-rename, and the read-modify-write runs under a lock on the
target file.

**Rollback is entry-scoped, not a pre-image restore.** `add` cannot hold the
manifest lock across its whole transaction, because materialization clones over
the network and holding a lock across a network call is what `AGENTS.md` already
forbids for the port registry. The lock is therefore released while
materializing — so restoring a whole-file pre-image afterwards would silently
revert any add that landed in the interval. Rollback instead re-takes the lock
and removes the specific entry it wrote, and only if that entry is still
byte-identical to what it wrote. Anything else means another writer has touched
it since, and the rollback is abandoned with a warning rather than guessing.

**Racing materialization.** Two resolutions of the same library can race
`ensure_clone` into the same directory, or race two `git worktree add` calls for
the same dirname. Fix: clone, fetch, worktree materialization and `meta.toml`
updates for one library run under a per-library advisory lock. This is the
pattern `refs.rs` already uses for the reference registry, so it is an existing
mechanism applied to a second target rather than new machinery. Lock ordering is
per-library lock before registry lock, never the reverse, and no network call
happens while the registry lock is held — the invariant `AGENTS.md` already
states for the port registry.

Explicitly **not** solved: a reader holding a path while a branch pin is
re-pointed by a concurrent `sync`, or while prune removes a directory the reader
is mid-read on. Both need session leases, which are out of scope (§Out of scope).

### 8. SKILL.md

`skills/docs/SKILL.md:25` tells agents the path is "version-matched to the
current project's lockfile". That sentence is why the reporting agent did not
verify. Corrections:

- describe resolution as workspace-manifest-and-lockfile based, and point at
  `commit` as the proof of what is checked out
- state that any stderr line from docm is a hard stop until explained, not
  context to relay conditionally
- require `--notes` on every `add`, recording which workspace pinned the version
- drop the "run `docm sync` after `add`" workaround the report proposed — `add`
  materializes now, so it is no longer true that add leaves a stale checkout
- when a checkout looks wrong for reasons docm cannot see (an upstream repo
  whose root manifest is decoupled from its release tags, as nitro's is),
  compare against the installed package under `node_modules`, which is ground
  truth for what actually runs

## Delivery

One PR, one release, about two days. Splitting the resolution-accuracy fixes
into their own PR would ship a release where scoped tags resolve correctly while
checkouts are still silently stale — a partial upgrade nobody wants to be on.

Sequencing inside the branch, one commit per task:

1. **Library-name encoding** (§1.1) — first and landable alone. It fixes a
   data-loss bug reachable in shipped 0.12.1, independent of everything else.
2. **Tag patterns and probe order** must precede the hard errors. Making a
   missing tag fatal while `tags::apply` still strips the scope would hard-fail
   scoped lookups that ought to succeed.
3. **Atomic writes, the per-library lock, and prune taking it** (§9) — before
   anything that adds new writers to `meta.toml`.
4. **Ecosystem probing, `rm` aliases** — independent, land any time.
5. **Ref-named directories, repository identity, commit verification** — the
   core change.
6. **Importer-graph selection** (§3).
7. **Hard-error failure modes** — last, once every path that should succeed does.
8. **Prune, registry keying, the upgrade pass and legacy-row retirement,
   `add`/`sync`/`info` surface, SKILL.md.**

## Testing

TDD, RED first, `cargo test --workspace` is the gate. The fixture repo in
`tests/common/mod.rs` gains a scoped tag, a branch, and a second version to
support these.

1. **Re-pin does not return a stale checkout.** Resolve pin A, change the
   manifest to pin B, resolve without syncing: different path, HEAD equals B's
   commit, A's directory untouched. This is issue 1 as a regression test.
2. **Slug injectivity.** `release/2.x` and `release~2.x` never map to one
   directory; scoped tag round-trips; over-length, `repo.git` and case-collision
   refs each error with their specific message.
3. **HEAD repair.** Corrupt a checkout with `git checkout --detach <other>`,
   resolve, assert `status repaired` and a correct HEAD.
4. **Branch pin moves.** Advance a branch upstream, `sync`, assert the same
   dirname now sits at the new commit and `info` reports it.
5. **Alias entries are never selected.** A bun.lock holding
   `"h3-v2": ["h3@2.0.1-rc.20"]` beside `"h3": ["h3@1.15.11"]` resolves to
   1.15.11 — the reported case verbatim. Plus the scoped-alias shape
   `"@compat/h3": ["h3@2.0.1"]`, which the previous suffix rule would have
   wrongly kept.
6. **Importer-graph selection.** Five `next` versions in one lockfile,
   `apps/lab-os` installing 14.2.4: resolution from inside `apps/lab-os` picks
   14.2.4 and names the deciding file. A nested copy that satisfies the declared
   range but is not what the workspace installs is *not* selected — the case
   that defeats range matching. A workspace that does not declare the package
   hard-errors listing candidates and their dependents. A `uv.lock` with a
   marker-split fork recording two versions hard-errors.
7. **Two workspaces, one lockfile.** Resolving library L from workspace A and
   from workspace B to different versions leaves two live reference rows, and a
   prune between them deletes neither.
8. **Scoped tag resolution and probe order.** `@hey-api/client-fetch@0.13.1`
   matches the full-name pattern, not the leaf — and wins over a repo-wide
   `v0.13.1` present in the same repo. A `meta.toml` carrying a 0.12.x cached
   pattern does not suppress the new forms.
9. **Ecosystem collision refuses.** A stubbed registry where a name hits both
   crates.io and npm errors and names both URLs; a js-marker CWD reports js when
   only npm hits.
10. **No-tag is a hard error.** Non-zero exit listing patterns tried;
    `--allow-default-branch` restores the old behaviour.
11. **Prune migration.** Two live projects referencing `default`; migrate one,
    prune, assert `default` survives; migrate the second, prune, assert it is
    reclaimed. Plus the inverse order: prune *before* any re-resolution leaves
    every legacy `default` intact.
12. **Scoped library names.** `docm add @types/node` produces a single
    `@types~node/` directory; a prune with it registered proposes no deletions.
    This is the 0.12.1 data-loss regression — RED against the current code by
    seeding `docs/@scope/pkg/` and asserting prune wants to delete it.
13. **Repository identity.** A lib whose manifest URL changes to a fork errors
    instead of reusing the existing bare clone.
14. **Force-moved tag.** Recreate a tag at a new commit upstream, `sync`, assert
    the directory re-points and the move is reported.
15. **Unrepresentable and reserved refs.** `a|b` and `NUL` error on Windows;
    `meta.toml` and `repo.git` as refs error everywhere; a case-only and an
    NFC/NFD collision each error.
16. **`add` rollback under concurrency.** A failing add leaves the manifest
    byte-identical, and cannot revert a concurrent successful add.
17. **Library-name injectivity.** `a/b` and `a~b` cannot both be registered —
    the `~` name errors at `add` time.
18. **Lockfile format details.** A pnpm peer-qualified locator
    (`8.0.2(chai@6.2.2)`) yields version `8.0.2`; a pnpm alias locator follows to
    the aliased package; a dep declared only in `devDependencies` /
    `optionalDependencies` / a uv `dependency-group` resolves; and an npm
    workspace that does *not* declare a package does not silently receive
    another workspace's hoisted copy.
19. **Upgrade pass.** A seeded 0.12.x cache with `@scope/pkg/repo.git` and an
    `origin`-less `meta.toml` migrates to `@scope~pkg/` with origin recovered
    from `remote.origin.url`, and the emptied `@scope/` is removed.
20. **Legacy row retirement.** A legacy row keyed by the lockfile directory is
    dropped when the first workspace-keyed row for the same library commits, so
    its `default` directory becomes reclaimable.
21. **Deleted upstream tag.** A tag removed upstream stops resolving after
    `sync` and produces a hard error naming the pin, rather than resolving from
    a stale local ref.
22. **Dirty checkout.** A tracked file modified inside a checkout makes `info`
    exit non-zero naming the file, instead of reporting `status ok`.
23. **Prune versus in-flight materialization.** A prune running concurrently
    with a resolve of the same library cannot delete the directory that resolve
    just materialized.

## Risks

1. **`add` gets slower and needs the network** — it now clones and materializes.
   Mitigated by it being a rare, interactive command, and by the failure being
   the point: a registration that cannot materialize is not a registration.
2. **Hard errors break existing entries.** Any lib currently relying on the
   default-branch fallback starts failing until re-pinned or given
   `--allow-default-branch`. Intended, but it will surface at the worst moment
   for someone mid-task.
3. **Five lockfile formats now need importer-graph parsing**, not just a version
   scan — the largest single piece of work here, and the one most likely to meet
   a lockfile shape the parsers do not expect. Mitigated by every unhandled shape
   being a hard error rather than a guess, and by dropping the `node-semver`
   dependency the previous draft required: resolving through the importer graph
   means ranges are never compared.
4. **Dirname churn, twice.** Lockfile-resolved checkouts move from `2.13.4` to
   the tag `v2.13.4`, and scoped library directories move from `@scope/pkg` to
   `@scope~pkg`. Prune reclaims the old ones; the cost is one refetch each.
5. **The prune/resolve race is narrowed, not closed** (§9).

## Out of scope

- Session leases that would protect a reader holding a path it has *already*
  been given — against a branch pin re-pointed by a concurrent `sync`, or a
  directory removed by a concurrent prune mid-read. The reference registry
  tracks workspaces, not sessions. §9 closes the lost-update,
  racing-materialization and resolve-versus-prune classes with the per-library
  lock; only in-flight readers remain, and covering them means expiring leases,
  their own liveness failures, and roughly doubling the scope of a correctness
  fix.
- Any change to the docs cache location or the global/project manifest split.

## Settled by review

- **`--allow-default-branch` is a flag only**, with no environment-variable
  equivalent. An escape hatch that a skill can set once stops appearing in the
  transcript, which is how the fallback became invisible in the first place.
- **Nothing outside this repo reads `<lib>/default` by path.** Agents receive the
  path from `docm info` / `docm path`; a sweep found the string only in the
  archived 2026-07-10 implementation plan and in `devkit-docs`' own tests.
  Retiring the name strands nothing.

- **`add <git-url>` without `--ref` splits by manifest** (§6.1): inferred and
  recorded globally, refused under `--project`. Demanding an explicit `--ref`
  everywhere would not buy knowledge — an agent under time pressure types
  `--ref main` or copies the branch out of the error text, producing the same
  value while making it look deliberate.

## Unresolved questions

None outstanding.
