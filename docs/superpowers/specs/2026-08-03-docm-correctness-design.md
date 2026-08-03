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

Three cases are hard errors rather than silent renames:

- a ref longer than 255 bytes (filesystem component limit)
- a ref whose dirname would be the literal `repo.git`
- a case-only collision on a case-insensitive filesystem — detected by comparing
  the incoming ref against the `raw_ref` recorded in `meta.toml`, reported as
  "two refs differ only by case; pin explicitly"

The `default` dirname is retired. Nothing shares a directory any more.

### 2. Identity and verification

`meta.toml` gains a `worktrees` map: dirname → `{ raw_ref, commit }`.

Every resolution:

1. selects a ref (§3)
2. resolves `git rev-parse <ref>^{commit}` against the **local** bare repo — no
   network; a miss triggers exactly one fetch, then a retry, then a hard error
3. materializes `<lib>/<slug(ref)>` at that commit if absent
4. if the directory exists, compares its HEAD to the resolved commit; a mismatch
   re-points the worktree (`git checkout --detach <commit>`) and reports
   `status repaired`
5. records `(project, lib, ref, version, commit, dirname)` in the reference
   registry

Tag and SHA pins are immutable, so step 4 is a corruption check for them. A
*branch* pin keeps a stable dirname while its commit moves; that directory does
change under a concurrent reader when `sync` fetches. This is accepted rather
than special-cased: branch checkouts only exist when someone explicitly asks for
one, and `info`'s `commit` line makes the movement visible.

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
3. **Workspace-aware lockfile resolution** for js/rust/python entries:
   1. walk up from CWD to the nearest workspace manifest — `package.json` (js),
      `Cargo.toml` carrying a dependency table (rust)
   2. read the declared range for the package from `dependencies`,
      `devDependencies`, `peerDependencies` (js) or `[dependencies]`,
      `[dev-dependencies]`, `[build-dependencies]` (rust, following
      `workspace = true` to the workspace root's `[workspace.dependencies]`)
   3. take the lockfile candidates for the package (alias-filtered, below)
   4. pick the highest candidate satisfying the declared range — `node-semver`
      for js ranges, the `semver` crate for Cargo ranges
   5. no candidate satisfies the range → warn naming both, use the highest
   6. no declared range (the package is transitive) → use the highest and list
      every candidate
4. **Tag lookup** turns the selected version into a ref (§4). No tag is a hard
   error (§6).

Python (`uv.lock`) keeps lockfile-highest selection: uv resolves one version per
workspace, so the multi-version failure does not arise. Revisit if it does.

Every multi-version resolution states the choice and its reason on stderr:

```
docm: lockfile holds 5 versions of next; apps/lab-os/package.json pins ^14.2.0 → 14.2.4
```

**Alias filtering.** A bun.lock `packages` entry maps a key to a spec array whose
first element is `name@version`. For an npm alias the key is the alias and the
spec is the real package: `"h3-v2": ["h3@2.0.1-rc.20", …]`. The parser currently
reads only the spec, so the alias inflates the base package's candidate list —
and, being a v2 prerelease, wins `highest()`. Rule: keep an entry only when the
key equals the spec's package name or ends with `/` + that name. `@app/portal/kysely`
→ spec `kysely` ends with `/kysely`, kept (a nested copy). `@scope/kysely` →
spec `@scope/kysely`, equal, kept. `h3-v2` → spec `h3`, neither, dropped.

### 4. Tag patterns

`tags::apply` gains the unstripped scoped forms alongside the existing leaf
forms, probed in this order:

| Pattern | Example |
|---|---|
| `v{version}` | `v1.15.11` |
| `{version}` | `1.15.11` |
| `{package}@{version}` | `@hey-api/client-fetch@0.13.1` |
| `{leaf}@{version}` | `client-fetch@0.13.1` |
| `{package}-v{version}` | `@hey-api/client-fetch-v0.13.1` |
| `{leaf}-{version}` | `client-fetch-1.15.11` |
| `{leaf}-v{version}` | `client-fetch-v1.15.11` |

The full-name forms precede their leaf equivalents so a repo publishing both
resolves to the more specific tag. The matched pattern stays cached in
`meta.toml` as it is today.

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
ref      v1.15.11
version  1.15.11
commit   5f72330a1b2c3d4e5f6071829304a5b6c7d8e9f0
status   ok
path     ~/.local/share/devkit/docs/h3/v1.15.11
docs     docs
src      src
notes    apps/api pins ^1.15.5; bun.lock also carries h3-v2 (2.0.1-rc.20) as an alias for nitropack
```

- `list` gains the ref → commit mapping per lib, so the state file is the index
  agents read rather than inferring anything from a path.

### 7. Reference registry and prune

`RefRow` grows `ref` and `commit` beside the existing `version`, both
`#[serde(default)]` so rows written by 0.12.x still parse; an empty `commit`
marks a row as needing re-resolution.

`refs::current_version` (`refs.rs:162`) currently returns the literal `"default"`
for every ref-bearing and git-ecosystem entry. It must compute the same dirname
the resolver would, or prune mis-plans against the new layout. The unconditional
`d != "default"` exemption at `refs.rs:143` goes away with it — an unpinned
checkout is protected by its own reference row like everything else.

Migration needs no command: the first resolve after upgrading retargets that
project's row to the new dirname, orphaning the old `default` directory for the
next `docm prune`. A `default` directory survives exactly as long as some project
still references it.

Prune's delete candidates are re-checked against the live registry under the
lock immediately before removal, narrowing the existing window at `docm.rs:361`
where a worktree materialized after the snapshot could be deleted by a
concurrent prune. This narrows the race rather than closing it; closing it needs
session leases, which are out of scope.

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

1. **Tag patterns and alias filtering** must precede the hard errors. Making a
   missing tag fatal while `tags::apply` still strips the scope would hard-fail
   scoped lookups that ought to succeed.
2. **Ecosystem probing, `rm` aliases** — independent, land any time.
3. **Ref-named directories and commit verification** — the core change.
4. **Workspace-aware selection** — depends on alias filtering being in place.
5. **Hard-error failure modes** — last, once every path that should succeed does.
6. **Prune, registry, `add`/`sync`/`info` surface, SKILL.md.**

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
5. **Alias filtering.** A bun.lock holding `"h3-v2": ["h3@2.0.1-rc.20"]` beside
   `"h3": ["h3@1.15.11"]` resolves to 1.15.11 — the reported case verbatim.
6. **Workspace-aware selection.** Five `next` versions in one lockfile,
   `apps/lab-os/package.json` pinning `^14.2.0`, resolution from inside
   `apps/lab-os` picks 14.2.4 and names the deciding file. Resolution from a
   workspace with no declared range picks the highest and lists candidates.
7. **Scoped tag resolution.** `@hey-api/client-fetch@0.13.1` matches the
   full-name pattern, not the leaf.
8. **Ecosystem collision refuses.** A stubbed registry where a name hits both
   crates.io and npm errors and names both URLs; a js-marker CWD reports js when
   only npm hits.
9. **No-tag is a hard error.** Non-zero exit listing patterns tried;
   `--allow-default-branch` restores the old behaviour.
10. **Prune migration.** Two live projects referencing `default`; migrate one,
    prune, assert `default` survives; migrate the second, prune, assert it is
    reclaimed.
11. **`add` rollback.** An add whose resolution fails leaves the manifest byte-identical.

## Risks

1. **`add` gets slower and needs the network** — it now clones and materializes.
   Mitigated by it being a rare, interactive command, and by the failure being
   the point: a registration that cannot materialize is not a registration.
2. **Hard errors break existing entries.** Any lib currently relying on the
   default-branch fallback starts failing until re-pinned or given
   `--allow-default-branch`. Intended, but it will surface at the worst moment
   for someone mid-task.
3. **`node-semver` is a new third-party dependency** (v2.2.0, ~950k downloads,
   published 2025-02). Accepted over a hand-rolled matcher because silently
   mishandling `>=` or `||` is the exact failure class this work exists to
   eliminate.
4. **Dirname churn.** Lockfile-resolved checkouts move from `2.13.4` to the tag
   `v2.13.4`. Prune reclaims the old directories; the cost is one refetch each.
5. **The prune/resolve race is narrowed, not closed.**

## Out of scope

- Session leases that would let a concurrent agent hold a checkout against
  prune. The reference registry tracks projects, not sessions.
- Workspace-aware selection for Python.
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
