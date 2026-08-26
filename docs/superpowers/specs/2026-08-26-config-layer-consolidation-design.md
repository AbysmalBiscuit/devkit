# One project layer stack, main checkout included

Give devkit a single answer to "which of this project's config files apply
here", teach it the main checkout, and let a linked worktree resolve a
project's config without carrying a copy of it.

## Problem

### A worktree cannot see its own repository's config

`discover` (`crates/devkit-config/src/lib.rs:706`) builds the layer stack by
walking up from the working directory and prepending `~/.config/devkit/config.toml`.
A linked worktree is a sibling of the main checkout, not a descendant, so the
walk never reaches it. `~/Git/adaptyv/swe-10285-…` walks through
`~/Git/adaptyv` and `~/Git` and never sees `~/Git/adaptyv/monorepo/devkit.toml`.

The workaround is to copy the config into each worktree through
`defaults.worktree_include`. That copy runs only at creation time
(`src/bin/issue/setup.rs:432`, `src/bin/issue/checkout.rs:423`) and
`copy_file` returns early when the destination exists
(`crates/devkit-common/src/worktree.rs:158`), so every existing worktree keeps
whatever it was born with. A repository with sixty worktrees holds sixty
independently stale copies of its own app and task catalog.

### Three resolvers disagree about which project files count

Layer resolution is implemented three times, each reading a different file set:

| Reader | Project files | Walk | Merge |
| --- | --- | --- | --- |
| `devkit-config::discover` | `devkit.toml` + `devkit.local.toml` | yes, with `[config] root` cutoff | deep, with provenance |
| `devkit-locks::hook` (`hook.rs:144`) | `<root>/devkit.toml` | none | boolean OR |
| `devkit-docs::manifest` (`manifest.rs:169`) | `devkit.toml` | yes, no cutoff | field-wise per lib |

So `[harness]` ignores `devkit.local.toml` and ignores every ancestor
directory, and `[docs]` ignores `devkit.local.toml` and ignores the root
cutoff. Both surprise a reader who has learned how `[apps]` resolves.

The harness divergence has a concrete cost. Because `harness_enabled` reads
`<checkout root>/devkit.toml` and nothing else, a tree that wants write
enforcement declared by the project rather than by the personal config needs
that file at every worktree root, on top of whatever carries the rest of the
config.

**Only the project layers are shared.** The three readers also disagree about
their non-project inputs, and those disagreements are deliberate:

- `discover` treats `--config` / `$DEVKIT_CONFIG` as the **sole** layer
  (`lib.rs:705`), suppressing the walk entirely.
- The harness treats `$DEVKIT_CONFIG` as **one global input**, OR'd with the
  checkout's own opt-in (`hook.rs:161`, `:183`).
- Docs reads a separate global catalog at `~/.config/devkit/docs.toml`
  (`manifest.rs:130`), not the config file at all.

A shared resolver that returned one flat list of every applicable file could
not preserve all three. It would either make `$DEVKIT_CONFIG` suppress harness
enforcement and the project docs catalog, or stop it being a sole-layer
override. Neither is acceptable, so the shared piece is the project layers
only.

### The same ancestor walk, written five ways

`Path::ancestors()` is used correctly at `docs/importers.rs:241`, `:437`,
`:760` and `locks/lib.rs:419`. It is also hand-rolled as
`while let Some(d) = dir` at `locks/lib.rs:45`, `config/lib.rs:722`,
`docs/manifest.rs:169`, `docs/resolve.rs:75`, and `docs/lockfiles.rs:38`.
Three of those five are `ancestors().find(...)` spelled long.

## Design

Four changes, sequenced so each lands green on its own.

### 0. Ancestor walks, and one home for the checkout root

Replace the five hand-rolled loops with `Path::ancestors()`. No helper is added
to `devkit-common`: the standard library already provides the iterator, and a
`walk_up` wrapper would be a third spelling of it in a codebase that has two.

Move `find_root_from` (`locks/lib.rs:39`) into `devkit-config`, beside the
layer discovery of step 1. "Which checkout am I in" is the first question layer
discovery asks, and `devkit-config` must be able to answer it without reaching
across the workspace. `devkit-locks` gains a `devkit-config` dependency — which
step 1 requires anyway — and re-exports the function so its existing callers
(lock context construction, root lookup, write normalization, `lockm`) are
untouched.

`devkit-docs::resolve::project_root` (`resolve.rs:73`) **stays.** It is not
provenance: its return value becomes the reference-registry workspace key
(`resolve.rs:248`), and `pins::project_keys` (`pins.rs:119`) relies on each
linked worktree having its own identity. Resolving it through an inherited
main-checkout layer would collapse every worktree of a repository onto one key.
It is rewritten with `ancestors()` and otherwise left alone.

### 1. One project-layer discovery, three consumers

`devkit-config` exposes the ordered **project** layer paths as a public
function. Global files and explicit overrides are deliberately excluded; each
reader composes those itself.

```rust
/// Ordered project config layers that apply at `start`, lowest precedence
/// first. Excludes the home config, `--config` / `$DEVKIT_CONFIG`, and any
/// reader's own global catalog — callers compose those themselves.
pub fn project_layers(start: &Path) -> Result<Vec<Layer>>;

pub struct Layer {
    pub path: PathBuf,
    pub kind: LayerKind,
}

pub enum LayerKind {
    /// Found by walking up from `start`, above the checkout root.
    Ancestor,
    /// The current checkout's own file.
    Checkout,
    /// Inherited from this repository's main checkout. Read-only: never a
    /// mutation target, never a project identity.
    MainCheckout,
}
```

`discover` keeps its private table-reading behavior, its sole-layer override,
and its home-config base, and calls `project_layers` for the middle of the
stack. `devkit-locks::hook` and `devkit-docs::manifest` call it too and keep
their own deserialize targets, their own merge rules, and their own global
inputs. `HarnessProbe` and the `[docs]` extraction genuinely differ and stay
where they are.

`harness_enabled` becomes a function over the project layers. `global_harness_enabled`
stays exactly as it is, OR'd in **outside** the layer stack, so that
`[config] root = true` in a repository cannot switch off machine-wide write
enforcement. `resolve_enforcement` keeps its `DEVKIT_ENFORCE_WRITES`
override and its precedence (`hook.rs:183`), and `enforcement_enabled` keeps
its signature, so `src/bin/lockm.rs:168` is untouched. The env override
short-circuits before any filesystem work: when `DEVKIT_ENFORCE_WRITES` decides
the answer, no layer is collected and no file is read.

`docm --project` writes to `Discovered::project_devkit_toml` (`docm.rs:262`),
so that field is restricted to a `Checkout` or `Ancestor` layer and **never**
resolves to a `MainCheckout` one. Without that restriction, `docm add --project`
from a worktree would edit the main checkout's config. When a worktree has no
config of its own, `--project` fails as it does today rather than silently
retargeting.

Layer-collection policy stays in `devkit-config`. It is config knowledge, not a
primitive, and pushing it into `devkit-common` would put the file names in a
crate that has no opinion about them.

### 2. The main-checkout layer

`project_layers` gains one source: the main checkout of the git repository the
start directory belongs to, resolved by reading `.git` directly. No subprocess.

```
root = find_root_from(start)
if <root>/.git is a directory        → root is the main checkout; add nothing
if <root>/.git is a file:
    read it; require a single `gitdir: <path>` line
    resolve <path> against <root> when relative
    require it to end with `worktrees/<name>`; strip both components → common
    require common.file_name() == ".git"
    candidate = common.parent()
    require canonicalize(<candidate>/.git) == canonicalize(common)
    → candidate is the main checkout
any failed requirement → no layer, no error
```

**Why not `git rev-parse --git-common-dir`.** It answers a different question
and answers it unsafely. `cmd::capture` (`cmd.rs:5`) inherits the environment,
so an ambient `GIT_DIR` or `GIT_COMMON_DIR` redirects the answer to an
unrelated repository, whose `devkit.toml` would then contribute executable
`[apps]` and `[tasks]` entries. `capture` also has no timeout, and this code
runs in the PreToolUse write hook: a `git` that never returns blocks the write,
and failing open cannot help a call that does not come back. Reading one file
has neither problem, and the validation above is stricter than git's answer.

**Edge cases fall out of the validation rather than being special-cased.** A
submodule's gitdir has no `worktrees/` component. A `--separate-git-dir` clone
produces a common dir not named `.git`. A bare main worktree fails the same
name check. Each adds no layer.

**Precedence**, lowest first: home config (in `discover` only), ancestor layers
outermost-first, the main checkout's `devkit.toml` then `devkit.local.toml`,
then the checkout root's own two files, then any nested layer between the
checkout root and `start`, deepest last. The main checkout sits above every
ancestor because it is more specific to this repository than any directory
containing it, and below the checkout you are standing in so a worktree can
always override by dropping its own file.

**Cutoff.** `[config] root = true` applies per layer kind. In a `Checkout` or
`Ancestor` layer it cuts off everything below it, main-checkout layer included.
In a `MainCheckout` layer it is ignored: that layer is already the bottom of
the project stack, and honoring it there would let the main checkout suppress
the home config for every worktree at once.

**Dedupe.** Layers are deduplicated by canonicalized path, not by string. A
worktree nested beneath the main checkout finds those files during the upward
walk, and a symlinked path spells the same file differently; both contribute
once, at the highest precedence position they occupy.

### 3. Path anchoring by what a key means

`resolve_path_key` (`lib.rs:940`) anchors every relative `[defaults]` path to
the directory of the layer that declared it. That single rule is wrong for one
of the three keys once a layer can be inherited, because the keys mean two
different kinds of thing:

| Key | Kind | Anchor |
| --- | --- | --- |
| `worktree_root` | host path | declaring layer's directory |
| `baseline_path` | host path | declaring layer's directory |
| `doppler_yaml` | repository-relative | the consuming checkout root |

A host path names a location on this machine, and the layer that declared it is
the only thing that gives it meaning. A repository-relative path names a file
inside whichever checkout is reading it: `doppler_yaml = "doppler.yaml"` must
resolve to the worktree doing the work, so a branch that adds an app to the map
takes effect without merging first, and `baseline_path = "."` in a main-checkout
layer must still resolve to the main checkout.

This is a rule about the key, not about the layer, so it applies uniformly and
a declaration keeps its meaning when moved between layers.

**It changes existing behavior**, not only main-checkout behavior. A relative
`doppler_yaml` in the home config currently resolves against
`~/.config/devkit/`; under this rule it resolves against the checkout. The same
goes for one declared in an ancestor layer. Both are listed below.

**Docstrings.** The doc comments on `Defaults` (`lib.rs:222`, `:229`, `:234`)
generate the published JSON Schema's hover text. They currently say only
"`~` is expanded" and have never mentioned layer-relative anchoring at all,
which was already a gap before this change. Each of the three gains its kind
and its anchoring rule. Regenerate with
`DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`.

`docs/configuration.md:109` says these types live in
`crates/devkit-ports/src/config.rs`. They live in
`crates/devkit-config/src/lib.rs`. `configuration.md:169` documents the old
blanket anchoring rule. Fix both in the same change.

## Behavior changes

Each is intended. Listed so none of them arrives as a surprise.

- A linked worktree resolves the main checkout's config. Where a worktree
  currently carries a copy, the copy still wins, because it sits higher in the
  stack.
- `[docs]` starts honoring `devkit.local.toml` and the `[config] root = true`
  cutoff. `docm --project` keeps writing only to a file in the current
  checkout or above it.
- `[harness]` starts honoring the full walk and `devkit.local.toml`, so a
  harness declaration no longer has to sit at the checkout root. The global
  opt-in stays outside the layer stack and outside the root cutoff.
- A relative `doppler_yaml` declared in the home config or an ancestor layer
  changes anchor, from the declaring directory to the consuming checkout.
  A relative `worktree_root` or `baseline_path` is unaffected.
- **A tracked `devkit.toml` in the main checkout changes what every linked
  worktree executes when the main checkout switches branches.** The main
  checkout is git topology, not a trust boundary: its `[apps] launch`,
  `[tasks] run`, per-app setup commands, and `after_worktree_create` hooks now
  reach worktrees whose own diff shows no such change. This does not give an
  untrusted PR branch a new opening — a worktree's own `devkit.toml` already
  outranks the inherited one — but it does mean an edit made in one place takes
  effect in many. Config that must not move under a branch switch belongs in
  the main checkout's untracked `devkit.local.toml`, which is what the layer is
  primarily for.
- The `lockm` PreToolUse hook goes from two flat file reads to an ancestor walk
  plus one `.git` read. It runs on `Edit|MultiEdit|Write|NotebookEdit`
  (`hooks/hooks.json:44`), so the cost lands on writes specifically. The
  `DEVKIT_ENFORCE_WRITES` short-circuit removes all of it when the env decides.
- devkit's own worktrees under `devkit-worktrees/` begin resolving
  `devkit/devkit.toml`, so they pick up its `[harness]`, `[github]`, and
  `[tracker]`. They run without all three today. This is the bug being fixed,
  and it lands on this repository first.

## Non-goals

- **The `common` / `config` dependency inversion is out of scope.** It was in
  an earlier draft as the enabler for git-based resolution; reading `.git`
  directly needs nothing from `devkit-common`, so the inversion is now an
  unrelated refactor. It is also larger than that draft claimed: `Repos::resolve`
  has eleven call sites, not two, and `expand_tilde` has seven production
  consumers, not two. And `devkit-common` is not a primitives crate today — it
  carries HTTP, GitHub, Slack, UI, tracing, storage, supervision, and the
  optional daemon client — so pointing `devkit-config` at all of it would trade
  one awkward edge for a heavier one. If the boundary is worth fixing, the shape
  is a small leaf crate for path and repository primitives with `devkit-config`
  and `devkit-common` as siblings, and that deserves its own spec.
- `issue sync-includes` (`docs/superpowers/plans/2026-08-26-issue-sync-includes.md`)
  is unaffected and still needed. It covers the rest of `worktree_include`:
  `CLAUDE.local.md`, `.env.local`, hook scripts. This change only removes
  config from the set of files that must be synced.
- No new config key. The main checkout is derived, not declared. A key naming
  it would have to live in the file the worktree cannot yet see.
- No caching. The hook is a fresh process per invocation, so a process-lifetime
  cache would not help it, and one file read does not warrant a cross-process
  one.

## Gate

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all`, on each step independently.

Step 2 requires tests for each resolution outcome: linked worktree resolves its
main checkout; main checkout adds nothing; submodule, `--separate-git-dir`, and
bare main each add nothing; a malformed `.git` file adds nothing and does not
error; an ambient `GIT_DIR` pointing elsewhere changes no result. Precedence,
per-kind cutoff, and canonicalized dedupe each get a test, including a worktree
nested beneath its own main checkout.

Step 3 requires the committed `schema/devkit-config.json` to match, which
`cargo test` already enforces.

## Unresolved

None. The scope question (worktrees need zero devkit files, harness included),
the resolution mechanism (parse and validate `.git`, no subprocess, no config
key), the sharing boundary (project layers only), and the anchoring rule (by
key kind) are settled.
