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

### Five ways to ask which checkout this is

"Which checkout am I in" is answered by five different mechanisms that do not
agree with each other:

| Mechanism | Answers | Call sites |
| --- | --- | --- |
| `git rev-parse --show-toplevel` | current worktree root | `brief.rs:371`, `:502`, `devrun/main.rs:238`, `info.rs:46`, `portm.rs:59`, `review/request.rs:280`, `review/finish.rs:189` |
| `git rev-parse --git-common-dir` | main checkout's git dir | `end.rs:80`, `:121` |
| `find_root_from` | nearest ancestor holding `.git`, no git call | `locks/lib.rs:39` |
| parsing the `.git` file | linked-worktree detection | `docs/upgrade.rs:480`, `:485` |
| `git worktree list --porcelain` | every worktree of the repo | `common/worktree.rs:93` |

Every one of the subprocess spellings goes through `cmd::capture`
(`cmd.rs:5`), which inherits the environment and has no timeout. So an ambient
`GIT_DIR` redirects `issue end`'s common-dir lookup to an unrelated repository
today, and any git that fails to return blocks its caller indefinitely.

The ancestor walk itself is written five ways too. `Path::ancestors()` is used
correctly at `docs/importers.rs:241`, `:437`, `:760` and `locks/lib.rs:419`, and
hand-rolled as `while let Some(d) = dir` at `locks/lib.rs:45`,
`config/lib.rs:722`, `docs/manifest.rs:169`, `docs/resolve.rs:75`, and
`docs/lockfiles.rs:38`.

## Design

Four changes, sequenced so each lands green on its own.

### 0. One git module

Every git invocation in the workspace moves behind one module,
`devkit_common::git`. Git is already a required dependency (`devkit doctor`
checks for it), so this shells out — it does not add a git library and does not
reimplement anything git already answers.

The module owns spawning, because git needs a spawn policy that generic
`cmd::capture` must not have:

- **Sanitized environment.** `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_WORK_TREE`, and
  `GIT_INDEX_FILE` are stripped from every invocation. An ambient value for any
  of them silently redirects git to a different repository, which for devkit
  means loading a stranger's `[apps] launch` and `[tasks] run` as executable
  config. `cmd::capture` cannot strip them, because `gh` and `doppler` need
  their environments intact.
- **A timeout.** `cmd::capture` uses `Command::output()`, which blocks until the
  child exits. `enforcement_enabled` runs in the PreToolUse write hook, so a git
  that never returns blocks the write. Failing open cannot rescue a call that
  does not come back.

Its surface is the questions the workspace already asks, each answered by the
git command that answers it:

```rust
/// `rev-parse --show-toplevel` — the checkout containing `start`.
pub fn checkout_root(start: &Path) -> Result<PathBuf>;

/// The main checkout of `start`'s repository — the first entry of
/// `worktree list --porcelain`, which git documents as the main worktree.
/// `None` when that entry is `bare`, or when it is `start`'s own checkout.
pub fn main_checkout(start: &Path) -> Result<Option<PathBuf>>;

/// `worktree list --porcelain`.
pub fn worktrees(start: &Path) -> Result<Vec<Worktree>>;

/// `rev-parse --abbrev-ref HEAD`.
pub fn branch(start: &Path) -> Result<String>;

/// `git <args>` with the sanitized environment and the timeout — the escape
/// hatch for operations without a named function here.
pub fn run(args: &[&str], cwd: &Path) -> Result<String>;
```

Every call site in the problem table moves onto it. `find_root_from`
(`locks/lib.rs:39`) becomes a call to `checkout_root`, with `devkit-locks`
re-exporting the name so its own callers are untouched. `end.rs:80` and `:121`
call `main_checkout`, which is what closes their `GIT_DIR` hole.
`common::worktree::discover` calls `worktrees`. The seven `--show-toplevel`
sites call `checkout_root`. `docs::upgrade`'s hand-rolled `.git` inspection
(`upgrade.rs:480`, `:485`) goes away in favor of asking git.

**`devkit-config` does not call git.** It stays a leaf crate with no internal
dependencies, and receives the main checkout as a parameter — see step 1. This
is what lets the module live in `devkit-common` without inverting the
`common` → `config` edge.

`devkit-docs::resolve::project_root` (`resolve.rs:73`) is not a git question and
stays. It asks for the nearest ancestor holding a `devkit.toml`, and its answer
is the reference-registry workspace key (`resolve.rs:248`), which
`pins::project_keys` (`pins.rs:119`) needs to differ per worktree. It is
rewritten with `ancestors()` and otherwise left alone. The other four
hand-rolled ancestor loops become `Path::ancestors()` too; no walking helper is
added, because the standard library already is one.

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
override and its precedence (`hook.rs:183`).

**`enforcement_enabled` takes the payload CWD, not the checkout root.** Today
`src/bin/lockm.rs:159` collapses the CWD to a root with `find_root_from` before
calling it. Handing that root to `project_layers` would discard every layer
between the root and the directory the write came from, so a nested
`devkit.toml` — and a nested `[config] root` cutoff — would be invisible to the
harness while the resolver's own tests passed. The signature changes to take the
CWD and derive the root internally, and `lockm.rs:159` stops pre-resolving it.
That reordering is also what makes the env short-circuit real: with
`find_root_from` in the caller, an ancestor walk already ran before
`DEVKIT_ENFORCE_WRITES` was consulted. It is checked first, before any
filesystem work.

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

`project_layers` gains one source: the main checkout of the repository the
start directory belongs to. It does not resolve that itself — the caller passes
what `devkit_common::git::main_checkout` returned, so the git question is asked
in exactly one place and `devkit-config` keeps no git knowledge.

```rust
pub fn project_layers(start: &Path, main_checkout: Option<&Path>) -> Result<Vec<Layer>>;
```

`config::resolve` grows the same parameter. It has two production callers
(`devkit-ports/src/load.rs:17`, `src/bin/devkit/brief.rs:465`), both in crates
that already depend on `devkit-common`; everything else reaches config through
`load::load`. The harness and the docs manifest resolve it the same way, from
crates that also already depend on `devkit-common`.

Passing `None` means "no main-checkout layer" and is what every caller does
when git fails, when the start directory is not in a repository, or when it is
already the main checkout. Config resolution must not start failing because git
is absent: `devkit brief` is required to stay silent outside a devkit project,
and the `lockm` hook must never block a write. A git invocation that fails for
any reason yields `None`, not an error.

**Edge cases are git's answer, not devkit's inference.** Deriving the main
checkout as the parent of `--git-common-dir` cannot distinguish a real main
worktree from a bare repository at `/x/.git` or a `--separate-git-dir=/x/.git`
clone whose working tree is elsewhere: all three yield `/x`, and `/x/devkit.toml`
would then contribute executable `[apps]` and `[tasks]` entries from a directory
that is not a checkout of this repository. `worktree list --porcelain` avoids the
inference entirely — it names the main worktree directly and marks a bare one
`bare` — so submodules, `--separate-git-dir`, and bare mains need no
special-casing and no `.git` naming assumption.

**Precedence**, lowest first: home config (in `discover` only), ancestor layers
outermost-first, the main checkout's `devkit.toml` then `devkit.local.toml`,
then the checkout root's own two files, then any nested layer between the
checkout root and `start`, deepest last. The main checkout sits above every
ancestor because it is more specific to this repository than any directory
containing it, and below the checkout you are standing in so a worktree can
always override by dropping its own file.

**Cutoff.** `[config] root = true` is a positional barrier: it drops every layer
*lower in precedence* than the layer declaring it, and the home config with
them. No per-kind rule. This matches what `discover` does today — stop walking
outward, keep everything nearer `start` (`lib.rs:718`) — and it reads the same
from any directory. An inner ancestor marked root drops the outer ancestors and
leaves the main and checkout layers alone; a root marked in the main checkout
drops the ancestors, which is what makes a repository boundary isolate its
linked worktrees instead of isolating only the main checkout.

**Dedupe.** Canonicalized paths are the dedupe *key* only. `Layer.path` keeps
the original spelling of the highest-precedence occurrence, and that occurrence's
`kind` is the one that survives. A worktree nested beneath the main checkout
finds those files during the upward walk, and a symlink spells the same file
two ways; both contribute once. Returning the canonical path instead would
defeat the write restriction above — a checkout `devkit.toml` symlinked to the
main checkout's file would keep `LayerKind::Checkout` while pointing at the main
checkout, authorizing exactly the cross-worktree `docm --project` write the kind
exists to prevent. Dedupe runs before the cutoff, so a barrier is evaluated at
one position rather than at whichever duplicate came first.

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
  plus one `git rev-parse`. It runs on `Edit|MultiEdit|Write|NotebookEdit`
  (`hooks/hooks.json:44`), so the cost lands on writes specifically, measured at
  1.0 ms against 0.69 ms for a bare process spawn. The `DEVKIT_ENFORCE_WRITES`
  short-circuit removes all of it when the env decides, and the module's timeout
  bounds it when git misbehaves.
- Every git invocation loses `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_WORK_TREE`, and
  `GIT_INDEX_FILE` from its environment. A workflow that deliberately sets one
  of these to steer devkit stops working; none is known to exist, and the
  redirect they enable is the hole this closes.
- devkit's own worktrees under `devkit-worktrees/` begin resolving
  `devkit/devkit.toml`, so they pick up its `[harness]`, `[github]`, and
  `[tracker]`. They run without all three today. This is the bug being fixed,
  and it lands on this repository first.

## Non-goals

- **The `common` / `config` dependency inversion is out of scope, and is not
  needed.** An earlier draft made it the enabler for git-based resolution.
  Injecting the main checkout instead (step 2) keeps `devkit-config` a leaf, so
  the git module lives in `devkit-common` with no edge to invert. Doing it
  anyway would be a large unrelated refactor — `Repos::resolve` has eleven call
  sites and `expand_tilde` seven production consumers — and `devkit-common` is
  not a primitives crate today: it carries HTTP, GitHub, Slack, UI, tracing,
  storage, supervision, and the optional daemon client, all of which
  `devkit-config` would inherit. If that boundary is worth fixing, it deserves
  its own spec.
- **No git library.** Git is already a required dependency, so the module shells
  out. Adding `gix` or `git2` would answer questions git already answers, and
  `git2` would put a C build in front of the Windows CI job.
- `issue sync-includes` (`docs/superpowers/plans/2026-08-26-issue-sync-includes.md`)
  is unaffected and still needed. It covers the rest of `worktree_include`:
  `CLAUDE.local.md`, `.env.local`, hook scripts. This change only removes
  config from the set of files that must be synced.
- No new config key. The main checkout is derived, not declared. A key naming
  it would have to live in the file the worktree cannot yet see.
- No caching. The hook is a fresh process per invocation, so a process-lifetime
  cache would not help it, and 1 ms does not warrant a cross-process one.

## Gate

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all`, on each step independently.

Step 0 requires a test that an ambient `GIT_DIR`, `GIT_COMMON_DIR`, and
`GIT_WORK_TREE` change no answer the module gives, and one that a hanging git is
bounded by the timeout rather than blocking its caller. It also requires that no
`Command::new("git")`, `cmd::git`, or `capture("git", …)` remains outside the
module — greppable, so it can be a test.

Step 1 requires an end-to-end hook test run from a directory *below* a nested
harness declaration, which is the case the current caller cannot pass.

Step 2 requires a test per resolution outcome: a linked worktree resolves its
main checkout; a main checkout yields `None`; submodule, `--separate-git-dir`
with the git dir named `.git`, and bare main each yield `None`; a git failure
yields `None` and not an error. Precedence, the positional cutoff from each kind
of layer, and dedupe each get a test — including a worktree nested beneath its
own main checkout, and a `Checkout` layer symlinked to the main checkout's file
driven through `docm --project`.

Step 3 requires the committed `schema/devkit-config.json` to match, which
`cargo test` already enforces.

## Unresolved

None. The scope question (worktrees need zero devkit files, harness included),
the resolution mechanism (one git module in `devkit-common`, shelling out, with
the main checkout injected into config rather than resolved there), the sharing
boundary (project layers only), and the anchoring rule (by
key kind) are settled.
