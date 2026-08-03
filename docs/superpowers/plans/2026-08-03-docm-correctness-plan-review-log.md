# Plan Review Log: docm correctness implementation plan

Adversarial review of `docs/superpowers/plans/2026-08-03-docm-correctness.md` — the
implementation plan for the approved spec
`docs/superpowers/specs/2026-08-03-docm-correctness-design.md`.

Claude is the builder and orchestrator; Codex is a read-only critic. Codex advises,
Claude decides — every rejected finding is logged with its reason.

This is a *separate* loop from the spec review. The spec's own seven-round log lives at
`docs/superpowers/specs/2026-08-03-docm-correctness-review-log.md`; its design decisions
are settled and out of scope here. This loop asks a narrower question: handed to an
engineer with zero context for this codebase, does this plan produce a correct
implementation of that spec?

**Configuration:** `MAX_ROUNDS=5`, model `gpt-5.6-sol`, reasoning effort `high`,
sandbox `read-only` (inherited by every reply).

Started at plan commit `c7313ae`.

---

## Round 1 — Codex (`gpt-5.6-sol`, effort `high`)

> The plan is not implementable as written. It contains correctness failures, deadlocks, incomplete migration logic, and commit boundaries that cannot pass their stated gates.
>
> 1. **crucial — Ref and library path handling is internally broken.** Task 1's `checkout_dir` encodes `release/2.x` as `release~2.x`, then `validate_checkout` rejects every `~`; scoped refs therefore hard-error. It also omits required case/Unicode collision checks, and prune/doctor enumerate encoded `@scope~pkg` names without decoding them before `LibCache::new`, which rejects the tilde.
> Fix: Validate the raw ref before encoding, validate the encoded component separately, implement host-folded collision keys, and decode cache-root directory names before logical-library operations.
>
> 2. **crucial — The locking design still loses manifest updates and deadlocks.** Task 4 makes manifest writes atomic but never locks their read-modify-write cycle, so concurrent adds for different libraries race through different library locks and overwrite each other. Task 11 then takes a library lock and calls `resolve::resolve`, which Task 5 says takes the same non-reentrant lock, deadlocking both `add` and `sync`; `rm` is never placed under that lock, and prune's fresh registry recheck remains an empty sketch. The proposed `fs2` dependency also does not exist in the workspace despite the plan claiming it does.
> Fix: Use the existing `fd-lock` mechanism for a separate manifest lock, introduce a `resolve_locked` inner API, lock every same-library mutation, and implement prune's fresh snapshot/replan under the library lock.
>
> 3. **crucial — Task 6 leaves the highest-risk feature as an under-tested placeholder.** No parser code is supplied, while the five tests omit Cargo disambiguation, npm ancestor lookup, pnpm aliases, Bun workspace-qualified packages, and the required dependency classes. The uv test uses `[[package.dev-dependencies]]` instead of the specified table and never connects the app package to either `httpx` version, so it does not prove marker-fork handling.
> Fix: Provide concrete parsers and real-format fixtures covering every format rule in spec tests 6, 18, and 24, including all negative and ambiguity cases.
>
> 4. **crucial — The 0.12.x upgrade and ref-less global migration are not specified enough to preserve shipped state.** Task 8 gives only prose for rename collision preflight, worktree repair failure recovery, and cache-wide encoded-name collision detection; an existing `@scope~pkg` target makes the proposed `@scope/pkg` rename ambiguous. Task 11 cannot safely infer whether a ref-less merged entry is global or project-owned because current `Discovered` records no per-entry provenance.
> Fix: Define a preflighted, collision-safe migration state machine with rollback/re-materialization, add entry provenance to manifest discovery, and test real legacy global, project, scoped, and target-collision states.
>
> 5. **crucial — Several required behaviors and commit boundaries are unverified or immediately fail.** Task 5's `resolve_ref` never performs the spec's one-fetch retry, so an upstream ref missing locally falsely errors. Task 3 removes `TagPattern::NameDash*` while existing `tests/cache.rs` still uses it; Task 7's test calls nonexistent `current_for_row`, changes `Data::record` without updating its many unstaged callers, and leaves the decisive multiprocess race test empty. Tests for moved/deleted tags, tag-to-branch replacement, branch movement, add ABA, and `rm` versus `add` are absent.
> Fix: Add the missing fetch/retry and complete every regression harness before assigning commit boundaries, updating and staging all affected call sites at the task that changes each interface.
>
> VERDICT: REVISE

### Claude's response — accepted, in full

Every falsifiable claim was checked against source before being accepted. All
five clusters held up, and three were outright errors in the plan rather than
matters of degree:

| Claim | Verified against | Verdict |
|---|---|---|
| `fs2` is not a workspace dependency | `Cargo.toml:69` declares `fd-lock = "4"`; no `fs2` anywhere | **True** — and the plan's own "no new third-party dependency" constraint made the claim self-contradictory |
| `checkout_dir` rejects every ref with a `/` | The plan's own code: `encode` then `validate_checkout`, which bails on `~` | **True** — found independently while reading the plan back |
| `TagPattern::NameDash` still live | `crates/devkit-docs/tests/cache.rs:83` | **True** — Task 3's commit boundary would not compile |
| Enumeration feeds encoded names to `LibCache::new` | `refs.rs:187-192`, `docm.rs:364` | **True** — prune breaks the moment `new` validates |
| `Discovered` has no per-entry provenance | `manifest.rs:113-118` — only `project_devkit_toml` | **True** |
| Spec requires a one-fetch retry | Spec line 156: "a miss triggers exactly one fetch, then a retry, then a hard error" | **True** — the plan's `resolve_ref` had none |
| Spec requires case/Unicode folding checks | Spec lines 70-71, 659, 780 | **True** — the plan had none |
| `Data::record` callers unstaged | `resolve.rs:113` plus seven in `refs.rs`'s test module | **True** |

Changes made:

1. **Names.** `validate_checkout` → `validate_ref`, taking the *raw* ref and
   encoding after validation; `validate_lib` likewise takes the logical name.
   Added `names::fold_key` and `cache::create_dir_exact`, which creates the
   directory and then requires the parent listing to contain the exact bytes
   requested — that detects case folding *and* NFC/NFD folding by observation,
   with no Unicode dependency. Added `LibCache::from_dir` for names read from
   disk, and converted both enumeration call sites to it.
2. **Locks.** Rewrote Task 4 on `fd-lock`. Added `locks::with_manifest` (the
   manifest is one file shared by every library, so a per-library lock cannot
   serialize its read-modify-write) and `locks::with_lib_dir` (the upgrade pass
   must lock directories whose names do not validate). Split `resolve` into a
   lock-taking wrapper and a `resolve_locked` inner layer; `add`, `sync` and the
   race test's child all call the inner one. Documented non-reentrancy and the
   lock order in Global Constraints. Put `rm` under the library lock.
3. **Parsers.** Replaced Task 6's prose requirements with concrete code for the
   dispatcher, JS lockfile selection, bun, pnpm, npm, and a shared
   `from_package_array` covering cargo and uv. Tests went from 5 to 10, adding
   cargo member disambiguation, npm nearest-nested resolution, pnpm aliases and
   dev-dependency classes. Rewrote the uv fixture, which had declared no
   dependency edge and so proved nothing about fork handling.
4. **Migration.** Task 8 became a four-phase preflight-then-act state machine:
   survey, then a no-mutation conflict pass that refuses the whole run on an
   existing target or a fold collision, then per-library renames verified by
   `rev-parse` rather than trusting `worktree repair`'s exit code, then origin
   backfill. Added target-collision and already-migrated tests.
5. **Boundaries.** Task 3 now updates `tests/cache.rs:83` in its own commit.
   Task 7 renames `current_for_row` → `row_dirname` and enumerates all eight
   `Data::record` call sites. Added the fetch-retry to `resolve_ref` with tests
   for a newly published tag and a force-moved one. Added `origin_file`
   provenance to `LibEntry` so `sync` cannot backfill a ref into a committed
   project manifest. Filled in both previously-stubbed rollback tests and the
   prune race test's parent body.

Nothing was rejected this round.

**Loop note.** The user raised the global Codex effort default to `xhigh`
mid-round. `codex-reply` inherits model, effort and sandbox from the round-1
call and accepts no overrides, so round 2 starts a fresh thread at `xhigh`. One
round in, that costs little: a new critic should read the revised plan anyway,
and round 1's findings are recorded here.

---

## Round 2 — Codex (`gpt-5.6-sol`, effort `xhigh`, fresh thread)

Effort was raised mid-loop, and `codex-reply` inherits it from the thread's
first call, so round 2 opened a new thread pointed at the revised plan and this
log.

> The revised plan still has material correctness failures and cannot be implemented safely as written.
>
> 1. **crucial — Name validation still permits cache aliasing and escape.** Task 1 never calls `create_dir_exact`; `validate_lib("..")` escapes the cache root, `REGISTRY.JSON` bypasses the reserved stem on folding hosts, Task 2 returns on invalid `a~b` before naming colliding `a/b`, and `version_worktrees` silently returns empty instead of hard-erroring without `repo.git`.
> Fix: Reject dot components and folded reserved names, validate all manifest names together using folded keys, route every directory creation through `create_dir_exact`, and make malformed-library enumeration return `Result::Err`.
>
> 2. **crucial — Task 6 does not correctly resolve importer graph edges.** Cargo dependency `"serde 1.0.210 (registry+…)"` is reduced to `"serde"`, so another locked serde version causes a false ambiguity despite the edge identifying the exact package; undeclared errors omit required dependents, competing-lockfile errors omit resolved versions, and the Cargo/uv entry points plus JSONC scanner remain unfinished code.
> Fix: Resolve Cargo and uv dependency records by full identity/version/source, implement every parser completely, and test duplicate package versions, dependent attribution, optional/dev groups, and competing-lockfile version diagnostics.
>
> 3. **crucial — The decisive concurrency test cannot exercise the claimed window.** Task 5 puts the registry commit inside `resolve_locked`, but Task 7 says to call `resolve_locked`, pause before that commit, and commit afterward; additionally, lock-file existence is observable before the child acquires the lock, while the fixed 200 ms hold can expire before prune runs. ABA add, `rm` versus `add`, and the real CLI transaction remain untested.
> Fix: Add a deterministic test-only barrier immediately after materialization inside `resolve_locked`, signal only after the library lock is held, and add separate-process tests for prune, ABA rollback, and `rm`/`add`.
>
> 4. **crucial — Task 8 can strand shipped 0.12.1 worktrees.** On repair failure it may delete the moved directory while leaving `repo.git/worktrees/*` administration behind, then explicitly defers re-materialization despite the spec requiring it; no rollback protects earlier renames, and origin backfill is not stated to run under the library lock. The test checks only clean status and `origin.is_some()`, so a changed HEAD or wrong origin passes.
> Fix: Capture every HEAD before renaming, use the target library lock, repair or fully unregister and recreate each worktree before release, roll back/resume partial migrations, and assert exact HEAD, origin, metadata preservation, and repair-failure recovery.
>
> 5. **crucial — Multiple task boundaries cannot pass their stated gate, and several RED tests fail for the wrong reason.** Task 4 calls nonexistent `devkit_common::paths::config_dir`; Task 6 omits `anyhow::Context` and leaves existing Cargo fixtures without a member manifest/package row; Task 7 calls `plan_for_cache` with an unspecified new arity; Task 9's no-tag fixture fails during importer selection, while its moved-tag test never fetches; Task 10 does not instruct updating `cmd_add` for the new `detect(..., cwd)` signature. Branch movement, deleted-tag, tag-to-branch, two-project legacy-prune, and CLI output/JSON/doctor regressions are also absent.
> Fix: Supply compile-ready code and fixtures for every task, enumerate and stage every changed caller, and prove each regression RED for its intended behavior before the task's full gate.
>
> VERDICT: REVISE

### Claude's response — accepted with two reasoned exceptions

The `xhigh` pass went materially deeper than round 1, and most of what it found
was in code added *during* round 1's revision — the strongest argument for the
loop. Two findings were already fixed before this round returned, having been
caught while re-reading the new code: `paths::config_dir` does not exist (the
accessors in `crates/devkit-common/src/paths.rs` return `PathBuf`, not `Result`,
and there is no config accessor at all — `manifest::global_docs_path` is the
real one), and pnpm's `imp.get(c)` passed `&&str` where `Index` needs `&str`.

**Accepted:**

1. **Names.** Added `reject_traversal` — `validate_lib("..")` really did encode
   to `..` and `cache_root.join("..")` leaves the cache, which every deletion
   path downstream trusts it does not. Folded the reserved-stem check through
   `fold_key`, so `REGISTRY.JSON` is caught on a folding host. Rewrote Task 2's
   validation to collect problems across all entries rather than returning at
   the first — its own test demands an error naming both `a~b` and `a/b`, and
   the fail-fast version could only ever name one. Added Step 5c wiring
   `create_dir_exact` into `ensure_clone`, `write_meta` and the post-`worktree
   add` check; without that the function added in round 1 was dead code.
2. **Cargo/uv edges.** Replaced `dep_name` with `dep_edge`, which keeps the
   version cargo writes into the edge (`"serde 1.0.210 (registry+…)"`) and uv
   writes as `{ name, version }`. A lockfile holding two `serde` versions was
   being reported as an unresolvable fork even though the edge named exactly
   one. Added the matching test. `undeclared` now carries (version, dependent)
   pairs, and the competing-lockfile error resolves each candidate so it can
   print what each file would have returned. Wrote out `cargo`, `uv` and
   `json5_ish` in full, with a JSONC test covering a `//` inside a URL string.
3. **The race test.** This was the sharpest finding. The plan had the child
   pausing "before committing the registry row" while Task 5 put that commit
   inside `resolve_locked` — the child could not do what it was told. Worse,
   `locks::hold` creates the lock file *before* acquiring the lock, so polling
   for the file proved nothing, and the 200 ms hold was the fixed-interval sleep
   `AGENTS.md` forbids. Replaced with a `DEVKIT_DOCS_BARRIER` rendezvous inside
   `resolve_locked` (same env-driven style as `DEVKIT_DAEMON_HEALTH_PROBE_SECS`)
   that signals only after materialization under the held lock. Added the
   `rm`-versus-`add` cross-process test.
4. **Migration.** Added phase 2b capturing every HEAD before any rename, made
   the repair check compare against that commit rather than trusting `worktree
   repair`'s exit code, and made an unrepairable worktree get fully
   unregistered — including the `git worktree prune` that `remove_dir_all`
   alone leaves behind — and rebuilt before the lock is released. Deferring the
   rebuild to `resolve` was wrong because prune runs first and would misread a
   registered-but-absent checkout. Put phase 4 under the same lock. Both tests
   now assert the exact HEAD and the exact origin.
5. **Boundaries.** Fixed Task 9's Cargo fixture, which lacked `app`'s own
   `[[package]]` row and so failed in importer selection — RED for the wrong
   reason. Specified `plan_for_cache`'s new signature and `Removal`. Told
   Task 10 to update `cmd_add` for `detect(..., cwd)`. Added deleted-tag and
   tag-to-branch tests.

**Rejected, with reasons:**

- **"Make `version_worktrees` return `Err` when `repo.git` is absent."** This is
  the prune path. A stray directory must make prune do *less*, never fail —
  hard-erroring here turns one malformed entry into a total prune outage, and
  prune is what reclaims disk. The real defect Codex is pointing at is silence,
  not leniency, so `plan_for_cache` now reports every skipped entry and `devkit
  doctor` lists them. Safe behaviour kept; invisibility removed.
- **"Roll back earlier renames on a partial migration."** Each `fs::rename` is
  atomic, phase 2 proves every target free before anything moves, and `run` is
  idempotent — so a crash leaves a part-migrated cache that the next invocation
  *finishes*. Rollback would add a second failure path over the one that already
  recovers by re-running, and an unwind that itself crashes is strictly worse
  than a resumable forward pass. Documented the resumability instead.

Also corrected the moved-tag test, which asserted a behaviour the design does
not have: `resolve` reads the local repo and does not fetch for a ref it already
holds, so a force-moved tag is invisible to it. The test now pins the real
boundary — unchanged on `resolve`, followed after a fetch — rather than
asserting a network call on the hot path.

---
