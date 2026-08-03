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
