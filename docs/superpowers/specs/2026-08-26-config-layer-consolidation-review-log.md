# Plan Review Log: config layer consolidation

Adversarial review of
`docs/superpowers/specs/2026-08-26-config-layer-consolidation-design.md`.

MAX_ROUNDS=5 · MODEL=gpt-5.6-sol · EFFORT=xhigh · Codex is read-only every round.

Focus set by the requester: major issues only, and verify against the graph that
the proposed architecture is actually cleaner than what it replaces.

## Round 1 — Codex

## Verdict

The spec needs revision. Drop Step 1 as written and redesign Step 2 around structured project-layer metadata, not a universal `Vec<PathBuf>`.

## High severity

1. A universal layer stack cannot preserve the three readers’ contracts.

What breaks: Full config treats `--config` and `DEVKIT_CONFIG` as sole-layer overrides. The harness treats `DEVKIT_CONFIG` as one global input ORed with checkout policy. Docs has a separate `~/.config/devkit/docs.toml` catalog. The proposed [`layer_paths(start)`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/docs/superpowers/specs/2026-08-26-config-layer-consolidation-design.md:110) cannot represent those differences.

If it follows [`devkit-config::discover`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-config/src/lib.rs:706), `DEVKIT_CONFIG` suppresses checkout harness and project docs. If it follows the current harness OR semantics in [`hook.rs`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-locks/src/hook.rs:183), explicit config stops being sole-layer. Applying `[config] root = true` to the harness also lets a repository suppress machine-wide write enforcement, even though the current marker only promises to drop the home config layer.

Why it matters: This changes safety policy and global docs behavior while claiming to share only file discovery.

Fix: Share only `project_layers(start)`, excluding global and explicit sources; let config, docs, and harness add their own global inputs and merge rules.

2. Docs read inheritance can turn into a write against the primary checkout.

What breaks: [`Discovered::project_devkit_toml`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-docs/src/manifest.rs:139) is not just provenance. [`docm --project`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/src/bin/docm.rs:262) writes to it. If a worktree has no config and discovery reports the inherited primary config, `docm add --project` and `docm rm --project` can modify the primary checkout from another worktree.

Deleting [`resolve::project_root`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-docs/src/resolve.rs:70) creates a second failure. Manual refs and fallback resolutions use that path as the reference-registry project identity. Substituting the declaring main-checkout layer would collapse every linked worktree onto one identity, contradicting the exact-worktree isolation enforced by [`pins::project_keys`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-docs/src/pins.rs:119).

Why it matters: A read-only inherited layer becomes a cross-worktree mutation target, and branch-specific docs references become conflated.

Fix: Carry `checkout_root`, writable project target, and declaring layer as separate fields; inherited main layers must never determine mutation targets or project identity.

3. `parent(git-common-dir)` is not a main-checkout resolver.

What breaks: It works for an ordinary linked worktree because the common directory is `<main>/.git`. For `--separate-git-dir`, it returns the external Git-storage parent. For a submodule, it points under the superproject’s `.git/modules`. For a bare repository, it probes the parent of the bare repository. The claim that these cases produce an empty layer is true only if those unrelated directories happen not to contain config files.

Ambient Git variables make this worse. I verified locally that `GIT_DIR` and `GIT_COMMON_DIR` redirect `--git-common-dir` to another repository, and `GIT_WORK_TREE` redirects `--show-toplevel`. [`cmd::capture`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-common/src/cmd.rs:5) inherits all three. A valid `devkit.toml` beside the redirected Git directory would be loaded.

Why it matters: Devkit can either import executable config from an unrelated directory or silently lose required config. The existing identical assumption in [`issue/end.rs`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/src/bin/issue/end.rs:78) is not validation; it is another caller that should use the same repaired repository model.

Fix: Resolve a verified `RepoContext` from sanitized Git metadata, validate that any candidate is an actual worktree for the same common directory, and represent bare or missing-main cases explicitly.

4. The hook design can block writes and silently weaken enforcement.

What breaks: The PreToolUse matcher runs only for write tools, not every tool call as the spec claims, but that makes latency more sensitive. The extra work lands exactly on writes. Current [`enforcement_enabled`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-locks/src/hook.rs:194) eagerly reads both config files even when `DEVKIT_ENFORCE_WRITES` already decides the answer. The proposed implementation adds another ancestor walk and an unbounded `Command::output`; [`capture`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-common/src/cmd.rs:5) has no timeout.

A 1 ms warm `rev-parse` measurement does not cover process startup, antivirus, network-mounted directories, a Git shim on `PATH`, config parsing, or tail latency on Windows. Failure-open does nothing if the subprocess never returns.

Why it matters: A write-safety hook that hangs blocks the write. A Git or layer error that collapses to “off” silently removes the safety policy.

Fix: Keep a minimal harness resolver, short-circuit the env override before filesystem work, preserve independent OR semantics, and never launch an unbounded child from PreToolUse.

5. A mutable primary checkout is not an implicit trust root for commands.

What breaks: Git’s primary worktree is topology, not a guarantee that it is on a trusted branch or even controlled by the current session. The new layer includes `devkit.local.toml` and all command-bearing tables. Besides `[apps].launch` and `[tasks].run`, [`issue setup`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/src/bin/issue/setup.rs:310) and [`issue checkout-pr`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/src/bin/issue/checkout.rs:309) can execute app setup commands and `after_worktree_create` hooks from the caller’s effective config.

An untrusted PR does not gain a new way to edit the primary config. Its own `devkit.toml` already has higher precedence and is the existing command-injection risk. The new risk is that changes outside the active checkout now alter executable behavior across every linked worktree without appearing in the current diff.

Why it matters: One checkout switch or edit changes the commands old branches execute.

Fix: Do not inherit executable tables from a mutable primary checkout without explicit repository trust; use an explicitly provisioned repo-common config or restrict the implicit layer to non-executable policy.

## Major severity

6. The dependency inversion relocates coupling and increases compile coupling.

[`devkit-config`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-config/Cargo.toml:1) is currently a small schema/loading crate. [`devkit-common`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-common/Cargo.toml:1) contains HTTP, GitHub, Slack, UI, tracing, storage, supervision, and optional daemon code. Making config depend on all of common does not create a clean foundation. It prevents any future common module from accepting `Config` without recreating a cycle, and moving `TrackerKind` with `JsonSchema` puts schema concerns into the integration crate.

The stated blast radius is also wrong if “two callers” means call sites. `Repos::resolve` has eleven production callers across `doctor`, issue setup/checkout/review/tracker/prs/dashboard, `devkit-issue`, and MCP. `expand_tilde` is also used by ports loading, devrun, setup, checkout, dashboard, strays, and gitignore.

Why it matters: The dependency graph looks acyclic while the semantic boundary gets worse.

Fix: Drop Step 1; extract a small leaf crate for repository/path primitives and shared vocabulary, with `devkit-config` and `devkit-common` as siblings.

7. The proposed path rule is source-dependent, not merely per-key.

The statement that the two anchors coincide for every non-main layer is false. They differ for the home config and any ancestor config today. Current behavior anchors every relative defaults path to the declaring file, as documented in [`configuration.md`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/docs/configuration.md:169) and implemented by [`resolve_path_key`](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-config/src/lib.rs:940).

Special-casing `doppler_yaml` only when it came from the primary checkout means identical TOML changes meaning when moved between layer types.

Why it matters: Future path keys will require remembering an undocumented source-specific exception, and moving a declaration can redirect filesystem access.

Fix: Model path semantics explicitly as repository paths versus host paths; repository-relative paths always anchor to `checkout_root`, while host paths always anchor to their declaring layer.

8. Precedence, cutoff, and dedupe are not specified tightly enough.

The spec does not define where the main layer sits when `start` is below the checkout root and nested configs exist. It also does not define what a root marker inside the main layer suppresses, versus one in the current root or a nested layer.

Dedupe only discusses standing in the primary checkout. A linked worktree can live beneath the primary checkout directory, causing the upward walk to find the same file before main-layer injection. Symlinked paths create another duplicate spelling.

Why it matters: Implementation choices will change which commands and policies win based on directory layout.

Fix: Define and test a complete order such as global, outer ancestors, main, checkout root, descendants; give every layer a source kind, define cutoff per source kind, and dedupe verified file identities.

## Verified scope

The graph from parent commit `cb67710` confirms that there are three effective-settings readers in the narrow sense: full config discovery, harness probing, and docs manifest discovery. The spec still misses downstream semantics that depend on discovery results.

- `expand_tilde` has production consumers beyond the two named in the dependency argument.
- `TrackerKind` is used throughout `devkit-common::tracker`, `devkit-issue`, and the issue/devkit binaries.
- `GithubConfig` reaches every `Repos::resolve` caller, not two call sites.
- `find_root_from` currently feeds lock context construction, root lookup, write normalization, and `lockm`.
- `resolve::project_root` has two direct resolution callers, while the separate `project_devkit_toml` field also controls pins and destructive `docm --project` mutations.

VERDICT: REVISE
### Claude's response

Verified all eight findings against source before revising. Every one held up
in whole or in part; the spec was rewritten rather than patched.

**Accepted in full.**

1. *A universal layer stack cannot preserve three contracts.* Confirmed:
   `discover` (`lib.rs:705`) makes `$DEVKIT_CONFIG` the sole layer, `hook.rs:165`
   makes it one OR'd input, docs reads a separate global catalog. The shared
   function is now `project_layers`, which excludes globals and explicit
   overrides; each reader composes its own. The `[config] root` point was the
   sharper half — a repository suppressing machine-wide write enforcement is a
   safety regression, so `global_harness_enabled` is OR'd in outside the layer
   stack and outside the cutoff.

2. *Docs read-inheritance becomes a write against the primary checkout.*
   Confirmed: `manifest_path` (`docm.rs:262`) returns `project_devkit_toml` as
   the `--project` mutation target. Layers now carry a `LayerKind` and that
   field never resolves to a `MainCheckout` one. The `project_root` deletion is
   withdrawn — `resolve.rs:248` uses its result as the reference-registry
   workspace key and `pins.rs:119` depends on per-worktree identity. It stays,
   rewritten with `ancestors()`.

3. *`parent(git-common-dir)` is not a main-checkout resolver.* Confirmed on the
   strongest angle: `cmd::capture` (`cmd.rs:5`) inherits the environment, so
   `GIT_DIR`/`GIT_COMMON_DIR` redirect the answer to an unrelated repository.
   The subprocess is gone. Resolution reads `.git` directly and validates the
   candidate against the common dir, which rejects submodule,
   `--separate-git-dir`, and bare-main cases by construction rather than by
   hoping those directories hold no config.

4. *The hook can block writes and silently weaken enforcement.* Confirmed on
   both mechanics: the matcher is `Edit|MultiEdit|Write|NotebookEdit`
   (`hooks/hooks.json:44`), not every tool call as the spec claimed, and
   `capture` has no timeout. Dropping the subprocess (finding 3) removes the
   hang. Added: `DEVKIT_ENFORCE_WRITES` short-circuits before any filesystem
   work, and the global opt-in keeps independent OR semantics.

7. *The path rule is source-dependent, not per-key.* Confirmed — the claim that
   the two anchors coincide for every non-main layer was false for the home
   config and every ancestor layer. Reframed as Codex proposed: keys are
   classified host-path or repository-relative, the rule attaches to the key,
   and the resulting change to relative `doppler_yaml` in home/ancestor layers
   is listed as a behavior change.

8. *Precedence, cutoff, dedupe underspecified.* Accepted. Full ordering written
   out, cutoff defined per layer kind, dedupe by canonicalized identity with a
   named test for a worktree nested beneath its own main checkout.

**Accepted, fix rejected.**

5. *A mutable primary checkout is not an implicit trust root.* The finding is
   right and the reasoning corrected mine: the untrusted-PR angle I worried
   about is not the real one, since a worktree's own file already outranks the
   inherited layer. The real hazard is that an edit or branch switch in one
   place changes what many worktrees execute, invisibly to their diffs.
   Restricting the layer to non-executable policy is rejected — inheriting
   `[apps]` and `[tasks]` is the feature, and without them the change delivers
   nothing. It is documented as a named behavior change instead, with the
   mitigation that an untracked `devkit.local.toml` does not move under a branch
   switch, which is how the motivating repository is already set up.

**Accepted, scope changed.**

6. *The dependency inversion relocates coupling.* The blast-radius numbers were
   simply wrong: `Repos::resolve` has eleven call sites and `expand_tilde` seven
   production consumers, against "two" each in the draft. I had counted
   signatures, not callers. The architectural objection also lands —
   `devkit-common` carries HTTP, Slack, UI, supervision, and the daemon client,
   so it is not a primitives crate to depend on. But the decisive point is that
   finding 3 dissolved the motivation: reading `.git` directly needs nothing
   from `devkit-common`, so the inversion stopped being an enabler and became an
   unrelated refactor. Step 1 is removed from the design and recorded as a
   non-goal, with Codex's sibling-leaf-crate shape noted as the right form if it
   is taken up separately.

Net: five steps became four, one step was deleted outright, and the hot path
lost its subprocess.

## Round 2 — Codex

_Reviewed `ffa2e12`. Between this round being launched and its critique landing, the design changed direction on the user's instruction: all git goes through one `devkit_common::git` module that shells out, and `devkit-config` no longer touches git at all (`82a7f76`). Finding 1's literal target — the hand-rolled `.git` parser — no longer exists, but its substance survived the rewrite and is answered below._

## Verdict

REVISE. The dependency structure is now sound, but Steps 1–2 still contain three correctness failures and one write-safety ambiguity.

## High severity

### 1. The `.git` validation does not prove that `candidate` is a main checkout

What breaks: In the proposed algorithm, once `common.file_name() == ".git"`, `candidate = common.parent()` makes `candidate/.git == common` by construction. The canonicalization comparison at [the spec’s lines 160–164](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/docs/superpowers/specs/2026-08-26-config-layer-consolidation-design.md:160) is therefore tautological.

Both cases the spec claims to reject can pass:

- A bare repository located at `/x/.git` with linked worktrees identifies `/x` as the supposed main checkout.
- `--separate-git-dir=/x/.git` does the same even though its real working tree is elsewhere.

Git permits the caller to choose the separate Git directory, while a bare clone makes its chosen directory the repository itself. The name `.git` is not proof of either layout. [Git clone documentation](https://git-scm.com/docs/git-clone), [Git repository-layout documentation](https://git-scm.com/docs/gitrepository-layout). Git’s own worktree implementation notes that a main worktree represented by a gitfile cannot reliably be recovered from another worktree. [Git worktree source](https://github.com/git/git/blob/master/worktree.c)

The current real layout also contains two validation inputs the design ignores: `worktrees/<id>/commondir` and its `gitdir` backlink. Consequently, a syntactically valid `.git` file pointing at an unrelated existing `.../.git/worktrees/<id>` also passes.

Why it matters: devkit can load executable `[apps]`, `[tasks]`, and setup hooks from `/x/devkit.toml`, even when `/x` is not a checkout of the current repository.

One-line fix: Do not land Step 2 until main-checkout identity is explicit or provable; at minimum validate `commondir` and the `gitdir` backlink, then either reject or document the still-ambiguous separate-git-dir-named-`.git` layout and test it.

### 2. The hook discards the start directory before calling `project_layers`

What breaks: The spec says `enforcement_enabled` keeps its signature and [the `lockm` call remains untouched](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/docs/superpowers/specs/2026-08-26-config-layer-consolidation-design.md:129). The existing caller first turns the payload CWD into the checkout root at [lockm.rs:159](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/src/bin/lockm.rs:159), then calls `enforcement_enabled(&root)` at [lockm.rs:168](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/src/bin/lockm.rs:168).

`project_layers(root)` cannot discover a nested `devkit.toml` or `devkit.local.toml` between the checkout root and the original CWD. Nested root cutoffs are also invisible. This directly contradicts the claimed full-walk behavior.

It also disproves the stronger environment-short-circuit claim: `find_root_from` has already performed an ancestor filesystem walk before `DEVKIT_ENFORCE_WRITES` is checked.

Why it matters: Step 1 can compile and its resolver tests can pass while the actual PreToolUse hook silently fails to enforce a nested project declaration.

One-line fix: Pass the original payload CWD to `enforcement_enabled`, let `project_layers` derive the checkout root, and add an end-to-end hook test launched below a nested harness layer.

### 3. The cutoff rule contradicts both precedence and existing `root` semantics

What breaks: The stack is ordered `Ancestor → MainCheckout → Checkout`, but [the cutoff rule](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/docs/superpowers/specs/2026-08-26-config-layer-consolidation-design.md:190) says:

- An `Ancestor` root removes the higher-precedence main layer.
- A `MainCheckout` root is ignored, leaving lower-precedence ancestors active.

That is backwards. Existing behavior stops walking upward at the root marker while retaining layers closer to `start`, as shown by [config discovery](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-config/src/lib.rs:718).

For example, with `outer ancestor → inner ancestor(root) → main → checkout`, the inner ancestor should remove only the outer ancestor. It must not suppress main or checkout. Conversely, a root marker in main should remove ancestor project layers; otherwise the same main config excludes ancestors in the main checkout but inherits them from every linked worktree.

Why it matters: A broad ancestor config can suppress the repository’s `[harness]`, while a repository boundary marked in main fails to isolate linked worktrees from outer executable config.

One-line fix: Deduplicate first, then apply `root` as a positional barrier that removes only earlier/lower-precedence project layers; compose each reader’s global inputs under an explicit separate rule.

## Major severity

### 4. Canonical dedupe leaves the docs write invariant implementation-dependent

What breaks: `Discovered::project_devkit_toml` is an actual mutation target at [docm.rs:262](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/src/bin/docm.rs:262). The spec says dedupe uses canonical paths but does not state whether `Layer.path` retains the original highest-precedence spelling or is replaced by the canonical path.

For a checkout `devkit.toml` symlinked to the main checkout’s file, the surviving kind is `Checkout`. If the canonical path is returned, the `LayerKind` filter authorizes a direct main-checkout write despite the claimed read-only invariant. Cutoff behavior also changes depending on whether dedupe happens before or after root evaluation.

Why it matters: A plausible implementation of “deduplicated by canonicalized path” defeats the exact write restriction the type was introduced to enforce.

One-line fix: Specify that canonicalization is used only as a dedupe key, preserve the original highest-precedence path, run dedupe before cutoff, and test a canonical alias across `Checkout` and `MainCheckout` through `docm --project`.

## What now holds

- The dependency inversion is genuinely gone rather than relocated. Adding direct `devkit-locks → devkit-config` and `devkit-docs → devkit-config` edges is acyclic; both currently reach config transitively through [devkit-common](/home/lev/Git/lev/devkit-worktrees/config-layer-consolidation/crates/devkit-common/Cargo.toml:8).
- Source and graph inspection confirm the three config parsers are still `devkit-config::discover`, the lock hook, and docs manifest discovery. `docs::resolve::project_root` is another `devkit.toml` path search, but it is project identity rather than config resolution and is correctly retained.
- No remaining move touches `expand_tilde`, `TrackerKind`, or `GithubConfig`. `find_root_from` is the only moved symbol; the re-export covers its existing lock-context, status, write-normalization, and `lockm` callers.
- Steps 0 and 3 are independently landable. Step 1 is not behaviorally green because of the hook caller, and Step 2’s proposed gate misses the two `.git`-named bare/separate layouts and invalid backpointers.

VERDICT: REVISE
### Claude's response

All four findings verified against source. All four accepted.

1. *The `.git` validation is tautological.* Correct, and the objection outlived
   the algorithm it was aimed at. `candidate = common.parent()` makes
   `candidate/.git == common` true by construction, so the comparison proved
   nothing — and the replacement in `82a7f76`, taking the parent of
   `rev-parse --git-common-dir`, inherits the identical ambiguity. A bare
   repository at `/x/.git` and a `--separate-git-dir=/x/.git` clone both yield
   `/x`, and `/x/devkit.toml` would contribute executable `[apps]` and `[tasks]`
   from a directory that is not a checkout of this repository.

   Rather than adding the `commondir` and `gitdir` backlink checks Codex
   proposed, `main_checkout` now takes the first entry of
   `worktree list --porcelain`, which git documents as the main worktree.
   Verified: from a linked worktree it names the main checkout, and a bare
   repository's first entry carries `bare`. That removes the path arithmetic
   and the `.git` naming assumption together, so submodule,
   `--separate-git-dir`, and bare mains stop being cases to reject and become
   answers git gives directly. The gate keeps a test for each.

2. *The hook discards the start directory.* Confirmed at `lockm.rs:159`:
   `find_root_from` collapses the payload CWD to a root before
   `enforcement_enabled(&root)` at `:168`. Handing that root to `project_layers`
   would hide every nested layer and nested cutoff between root and CWD, while
   the resolver's own tests passed — exactly the silent failure described. The
   signature now takes the CWD and derives the root internally. This also
   disproved my env short-circuit claim, since the ancestor walk was already
   running in the caller before `DEVKIT_ENFORCE_WRITES` was read; the order is
   now explicit. The gate gains an end-to-end hook test run below a nested
   harness declaration.

3. *The cutoff rule is backwards.* Confirmed against `lib.rs:718`: the existing
   marker stops the outward walk and keeps everything nearer `start`. My rule
   had an `Ancestor` root suppressing the higher-precedence main layer while a
   `MainCheckout` root did nothing, which is inverted on both halves, and
   Codex's second example is the sharper one — a repository boundary that
   isolates only the main checkout and not its linked worktrees is not a
   boundary. Replaced with a single positional barrier that drops every layer
   lower in precedence than the one declaring it, home config included. The
   per-kind rule is gone, which makes this shorter than what it replaced.

4. *Canonical dedupe leaves the write invariant implementation-dependent.*
   Accepted in full, including the attack path: a `Checkout` layer symlinked to
   the main checkout's file keeps `LayerKind::Checkout` while pointing at the
   main checkout, so returning the canonical path would authorize the exact
   cross-worktree `docm --project` write the kind was introduced to prevent.
   Canonicalization is now specified as the dedupe key only, the original
   highest-precedence spelling is preserved, and dedupe runs before cutoff so a
   barrier is evaluated at one position. Tested through `docm --project`.

Round 3 re-reviews the current spec rather than this one, since the git
direction changed underneath this round.
