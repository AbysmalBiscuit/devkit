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
