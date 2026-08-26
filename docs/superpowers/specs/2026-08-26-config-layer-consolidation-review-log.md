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
