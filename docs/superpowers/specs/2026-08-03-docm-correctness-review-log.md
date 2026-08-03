# Spec Review Log: docm correctness

Started 2026-08-03. MAX_ROUNDS=5. MODEL=gpt-5.6-sol, EFFORT=high, sandbox=read-only.
Spec under review: `docs/superpowers/specs/2026-08-03-docm-correctness-design.md`.

Claude is the builder and arbiter; Codex is a read-only adversarial critic. No code
is written during this loop.

## Round 1 — Codex

> The spec is not safe to implement yet. Its "provably correct or hard error" guarantee still fails under ordinary concurrency, monorepo, migration, and Windows cases.
>
> 1. **crucial — Shared-cache races remain knowingly open.** The spec accepts in-place branch mutation and only "narrows" prune's race (`design.md:81`, `:253`). Concurrent resolves can also race clone creation, overwrite each other's `meta.toml` maps, or lose manifest updates; a failed `add` rollback can overwrite another successful `add`. Current mutations have no cache/manifest lock (`cache.rs:68`, `:107`, `:165`; `manifest.rs:187`, `:219`). A reader can receive commit A, then read a mixture of A and B while `sync` checks out B in place.
>    Fix: Define lock ordering and atomic writes for manifests, per-library clone/fetch/meta/worktree operations, and prune; add session leases or immutable branch snapshots so live paths are never mutated or removed.
>
> 2. **crucial — "Highest satisfying" still guesses rather than resolving the workspace's installed package.** A workspace declaring `^1.0` can resolve 1.1 while the lockfile also contains nested 1.9; both satisfy, so §3 selects 1.9 despite the importer mapping to 1.1. The explicit "no match → warn and use highest" and transitive "use highest" rules are knowingly unprovable (`spec:100`, `:110`). The alias suffix rule keeps a scoped alias such as key `@compat/h3` → spec `h3@…`, and `uv.lock` can contain multiple versions under environment forks, contradicting `spec:116`. Tag probing also tries generic `v{version}` before the package-specific tag (`spec:139`), while an existing cached generic pattern bypasses the new forms entirely (`resolve.rs:152`).
>    Fix: Resolve through each lockfile's workspace/importer dependency graph, hard-error when it cannot identify one installed package, parse aliases structurally, support marker-aware uv forks, and prefer package-specific tags before cached or generic patterns.
>
> 3. **crucial — Registry identity and 0.12.x migration are internally inconsistent.** Rows remain keyed by `(project, lib)` (`refs.rs:59`), but two workspaces in one monorepo can legitimately resolve the same library to different commits; whichever resolves last overwrites the other and makes prune reclaimable. Separately, §7 says `current_version` computes the new dirname during prune while also claiming `default` survives until each project's first successful resolve (`spec:242`, `:248`). A prune before resolution would retarget every legacy row from its manifest and delete the only materialized `default`. Ref-less global git entries also get only a recurring hard error or one-off escape hatch, not a durable migration.
>    Fix: Key references by workspace holder plus library, preserve empty-commit legacy rows at `default` until an atomic successful materialization, and provide a durable global ref-inference migration while continuing to reject ref-less project policy.
>
> 4. **crucial — Matching HEAD does not prove repository or working-tree correctness.** Existing caches are reused solely because `<lib>/repo.git` exists (`cache.rs:107`), and the proposed metadata records no repository identity (`spec:67`). A global entry using upstream A and a project overlay using fork B therefore share one bare repo; both can report `status ok` against A. Likewise, edited tracked files or untracked replacements pass because HEAD still matches. Finally, "tags are immutable" is false: the existing forced tag fetch (`cache.rs:137`) accepts force-moved tags, but the spec does not decide whether that is corruption or an accepted repin. `info` also omits the actual origin URL, hiding the first failure.
>    Fix: Make canonical repository identity part of the cache key or reject conflicting origins, verify a clean worktree as well as HEAD, define force-moved-tag policy, and expose expected/actual origin in `info` and `list`.
>
> 5. **crucial — The path/ref rules are not portable and omit existing control-name collisions.** `meta.toml` is a valid Git ref but collides with the per-library metadata file (`cache.rs:69`); Windows-valid Git refs such as `CON`, `NUL`, `a|b`, and `a<b` cannot be directory names despite Windows being a required CI target (`AGENTS.md:143`). Scoped library names are also joined verbatim (`cache.rs:87`), so `@scope/pkg` becomes nested cache structure and prune sees `@scope` as the library. Finally, `rev-parse <ref>^{commit}` leaves short branch/tag ambiguity and option-looking refs unspecified (`spec:72`).
>    Fix: Add an injective library-name encoding, reserve every cache control name, hard-error refs not representable on the host filesystem, detect case/Unicode-normalization collisions, and resolve refs with option termination plus explicit branch/tag ambiguity checks.
>
> VERDICT: REVISE

### Claude's response

Substantially right. Accepted almost all of it; finding 5 surfaced a live data-loss
bug in shipped 0.12.1 that neither the design nor the field report caught.

**Accepted and specified:**

- **Scoped library names corrupt the cache (5).** `cache.rs:87` joins the lib name
  onto the cache root, so `@hey-api/client-fetch` nests. `refs.rs:187` then reads
  `@hey-api` as a library and `client-fetch` as an unreferenced worktree, and
  deletes it. The reporting agent registered exactly that name. Library names now
  take the same `/` → `~` encoding as refs. New §1.1, sequenced first in
  Delivery, test 12.
- **Importer-graph selection (2).** "Highest satisfying a declared range" is
  replaced by resolving the workspace's own importer entry — pnpm `importers`,
  bun `workspaces` plus key namespacing, npm `packages` path nesting. A lockfile
  that cannot identify exactly one installed version is a hard error. This kills
  the nested-1.9-beats-1.1 case, and dissolves the scoped-alias hole in the
  key-suffix rule as a side effect. Rewritten §3.
- **Reference rows keyed by workspace (3).** My own §3 change created this: two
  workspaces sharing one lockfile legitimately resolve different versions, and a
  `(project, lib)` key made the second overwrite the first, leaving prune free to
  reclaim a live checkout. Now keyed by workspace directory. §7.
- **Legacy rows pin `default` until materialized (3).** A prune before the first
  post-upgrade resolve would otherwise retarget every legacy row and delete the
  only materialized checkout. Rows with an empty `commit` keep `default` until an
  atomic successful materialization retargets them. §7.
- **Durable migration for ref-less global git entries (3).** A permanent hard
  error was a papercut with no exit. `sync` now infers and records the default
  branch for global entries, matching the new `add` behaviour; `--project`
  entries still refuse. §7.
- **Repository identity (4).** `ensure_clone` returns early on directory
  existence alone, so a project overlay pointing at a fork silently reuses
  upstream's bare repo and reports `status ok` against the wrong repository.
  Origin is now recorded in `meta.toml`, a mismatch is a hard error, and `info`
  prints it. §2, §6.
- **Force-moved tags (4).** "Tags are immutable" was an assumption, not a
  guarantee — `cache.rs:137` fetches `--force --tags`. Now an explicit policy:
  a tag whose commit changed re-points its directory and reports the move loudly.
  §2.
- **Portability and control names (5).** Reserved cache control names
  (`repo.git`, `meta.toml`), host-filesystem representability checks including
  the Windows-invalid characters git permits (`|`, `<`, `>`, `"`) and reserved
  device names, and Unicode-normalization collisions. §1.
- **`rev-parse` hardening (5).** `--end-of-options`, fully-qualified
  `refs/tags/` / `refs/heads/` lookup, and an explicit error when a name is both
  a branch and a tag. §2.
- **Atomic writes and a per-library lock (1).** `meta.toml` and both manifests
  become write-temp-then-rename; clone/fetch/materialize/meta operations for one
  library run under a per-library flock, reusing the pattern `refs.rs` already
  uses. New §9.
- **uv environment forks (2).** Python multi-version resolution is a hard error
  rather than silent highest-wins. §3.
- **Tag pattern ordering (2).** Package-specific forms now precede the generic
  `v{version}`, and a cached `tag_pattern` is a hint that is re-probed rather
  than a short circuit — otherwise every library resolved before this change
  keeps its old generic pattern forever. §4.

**Rejected, with reasons:**

- **Session leases / eliminating the shared-cache race entirely (1).** Already
  declared out of scope, and the rejection stands. The per-library lock and
  atomic writes close the corruption class Codex actually demonstrated (lost
  meta/manifest updates, racing clones). What remains is a branch pin's directory
  moving under a concurrent reader, which requires an explicit branch pin, an
  explicit `sync`, and a reader mid-flight. Leases are a different design with
  their own liveness failures (a crashed agent holding a checkout against prune),
  and buying them here would double the scope of a correctness fix.
- **Verifying a clean worktree on every resolve (4).** Accepted in substance,
  moved off the hot path: `git status --porcelain` on a large checkout (next.js)
  costs hundreds of milliseconds on every `docm info`, and a user editing files
  inside the cache is not a failure mode this design is defending against. The
  check moves to `devkit doctor`. §6.

## Round 2 — Codex

Five crucial defects, all accepted. Verbatim critique:

1. **Importer rules still return wrong values or reject direct dependencies.** pnpm importer `version` is a peer-qualified snapshot locator (`8.0.2(chai@6.2.2)`, `3.20.0(@types/node@25.5.0)`), not a version. The table omits pnpm `optionalDependencies`, bun `devDependencies`, uv dependency groups. For package-lock, falling back to root `node_modules/<pkg>` can select a package hoisted for another workspace unless declaration is verified in `packages[workspace-path]` first.
2. **The locking protocol does not close the claimed command-level races.** Prune is not required to take the per-library lock, so resolve can materialize, wait for the registry, then record a path prune just deleted. "Rollback under the same lock" does not prevent clobbering if the lock is released during materialization and rollback restores a stale whole-file pre-image.
3. **The 0.12.x migration remains internally inconsistent.** Legacy `@scope/pkg` caches make the new `repo.git` assertion fail on the parent `@scope` directory. Existing `meta.toml` files lack `origin` with no bootstrap rule. A legacy row keyed by lockfile root cannot be retargeted by an upsert keyed by a nested workspace, so the old `default` row survives indefinitely.
4. **Library-name encoding is not injective.** The proof imports a git-ref restriction into unconstrained `LibEntry.name` and URL-derived names: `a/b` and `a~b` both encode to `a~b`. The ref encoding stays valid; the library claim does not.
5. **`status ok` can still be false under normal git states.** Fetch uses `--prune` but not `--prune-tags`, so a tag deleted upstream stays locally resolvable forever. The qualified-ref escape is undefined for an already-qualified `refs/tags/v1`. Moving cleanliness to doctor is unacceptable while `info` still prints `status ok` for a dirty checkout; doctor is not part of the agent lookup workflow.

Codex also noted the crash-hostage argument against leases is overstated, since leases can expire — conceded — while agreeing the scope argument holds, and that the global/project split, package-first tag probing and force-moved-tag policy are sound.

### Claude's response

All five accepted, including the cleanliness check I rejected in round 1.

- **Injectivity (4).** The strongest finding: I proved `/` → `~` injective for refs because git forbids `~` in ref names, then reused that proof for library names, which have no such constraint. `~` in a library name is now a hard error at `add` time, which makes the domain match the assumption. §1.1.
- **Importer details (1).** Per-format rules added: strip pnpm peer qualifiers and follow alias locators, enumerate every direct-dependency class per format, and verify npm declaration in `packages.<workspace-path>` before walking `node_modules` upward. §3, test 18.
- **Locking (2).** Prune now takes the per-library lock across recheck and deletion, which closes the resolve/prune race outright rather than narrowing it — no leases needed, and the out-of-scope note is narrowed to in-flight readers only. Rollback becomes entry-scoped and byte-identical-guarded, because `add` genuinely cannot hold the manifest lock across a network clone. §7, §9, test 23.
- **Migration (3).** An explicit locked upgrade pass renames nested `@scope/pkg` caches and bootstraps `origin` from `remote.origin.url`; retargeting a legacy row is redefined as retirement, since a row keyed by the lockfile directory can never be matched by an upsert keyed by a nested workspace. §7, tests 19–20.
- **`status ok` (5).** Reversing my round-1 rejection: `info` prints the claim, agents read `info` and not `doctor`, and a truth claim that can be false is the defect this design exists to remove. The clean check moves onto every path-returning resolution as a hard error, with `--untracked-files=no` to bound the cost. Also `--prune-tags` on sync, and a qualified-ref rule that uses a `refs/`-prefixed ref verbatim. §2, tests 21–22.

## Round 3 — Codex

Five defects, all accepted. Two of them were contradictions left behind by my own
round-2 edits.

1. **§3 cannot choose the authoritative JS lockfile.** A real checkout carries both `pnpm-lock.yaml` (h3 → 1.15.10) and `package-lock.json` (h3 → 1.15.7) with no selection or disagreement rule. The uv field is also factually wrong: dependency groups serialize as `[package.dev-dependencies]`, not `dependency-groups`.
2. **The scoped-cache upgrade breaks every linked worktree.** Git stores absolute reciprocal paths in the worktree `.git` file and in `repo.git/worktrees/<name>/gitdir`; after a rename, status, checkout and removal all address the old path.
3. **Entry-byte equality does not give rollback ownership.** Add A writes entry E and fails; concurrent add B writes an identical E and succeeds; A removes B's registration. Remove/re-add is the same ABA.
4. **A deleted tag can silently become a branch pin.** Metadata stores only `{raw_ref, commit}`, so if `v1` resolves as a tag, upstream deletes it and creates branch `v1`, `--prune-tags` drops the tag and the next resolution accepts the branch — the promised hard error never fires.
5. **`status ok` still ignores citable source.** `--untracked-files=no` lets an untracked `docs/fake.md` or `src/new.rs` be read and cited exactly like tracked source.

Codex confirmed the prune/library-lock reasoning is correct *provided resolve holds
the per-library lock through its registry commit*, and specified the window test 23
must actually cover. It also flagged two editorial contradictions: §6 still claimed
cleanliness was deliberately off the resolve path, and Risks still called the
prune/resolve race merely narrowed.

### Claude's response

All five accepted; nothing rejected this round.

- **Lockfile authority (1).** `packageManager` selects; exactly one lockfile is used
  when it is absent; disagreement without `packageManager` is a hard error naming
  both files and both versions. Today's code merges all three JS parsers' output,
  which is worse than picking wrong. uv's table name corrected. §3, test 24.
- **Worktree repair (2).** The upgrade pass now runs `git worktree repair` for every
  migrated worktree and verifies HEAD, re-materializing any that cannot be repaired.
  This would have been an implementation-time discovery at best. §7, test 25.
- **Rollback ABA (3).** Fixed by scope rather than comparison: the per-library lock
  is acquired before the manifest insertion and held through materialization and
  rollback, so a second add for the same library cannot interleave. §9, test 26.
- **Ref namespace (4).** `meta.toml` now records `resolved_ref` — the canonical
  `refs/tags/v1` — and a resolution whose namespace differs from the recorded one is
  a hard error until the manifest pin changes. §2, test 27.
- **Untracked files (5).** My justification was flatly wrong; untracked files are
  citable. The check drops `--untracked-files=no`. §2 step 5, test 22.
- Both editorial contradictions removed (§6, Risks 5).
