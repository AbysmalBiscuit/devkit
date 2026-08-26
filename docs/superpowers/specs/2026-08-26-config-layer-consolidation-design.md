# One config layer stack, main checkout included

Give devkit a single answer to "which config files apply here", teach it the
main checkout, and let a linked worktree resolve a project's config without
carrying a copy of it.

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

### Three resolvers disagree about which files count

Layer resolution is implemented three times, each reading a different file set:

| Reader | Files | Walk | Merge |
| --- | --- | --- | --- |
| `devkit-config::discover` | `devkit.toml` + `devkit.local.toml` | yes, with `[config] root` cutoff | deep, with provenance |
| `devkit-locks::hook` (`hook.rs:144`, `:173`) | `<root>/devkit.toml`, `~/.config/devkit/config.toml` | none | boolean OR |
| `devkit-docs::manifest` (`manifest.rs:169`) | `devkit.toml` | yes, no cutoff | field-wise per lib |

So `[harness]` ignores `devkit.local.toml` and ignores every ancestor
directory, and `[docs]` ignores `devkit.local.toml` and ignores the root
cutoff. Both surprise a reader who has learned how `[apps]` resolves.

The harness divergence has a concrete cost. Because `harness_enabled` reads
`<checkout root>/devkit.toml` and nothing else, a tree that wants write
enforcement declared by the project rather than by the personal config needs
that file at every worktree root, on top of whatever carries the rest of the
config.

### `common` depends on `config`, so `config` cannot reach git

`devkit-config` is a leaf crate. `devkit-common`, which owns the git wrappers
in `cmd`, depends on it. The dependency exists for three symbols:

- `expand_tilde` (`config/lib.rs:968`), used by `common/src/gitignore.rs:2` and
  `devkit-ports/src/strays/mod.rs:228`. A path primitive in the wrong crate.
- `TrackerKind` (`config/lib.rs:185`), re-exported at
  `common/src/tracker/mod.rs:8`. The `Tracker` trait lives in `common`; its
  vocabulary type does not.
- `GithubConfig` (`config/lib.rs:174`), a parameter type at
  `common/src/github.rs:352`, `:377`, `:410`. `build` destructures it into
  `issues_repo` and `pr_repo` immediately.

None is a real dependency of a primitives crate on a config crate, and together
they are what stops config resolution from calling git.

### The same ancestor walk, written five ways

`Path::ancestors()` is used correctly at `docs/importers.rs:241`, `:437`,
`:760` and `locks/lib.rs:419`. It is also hand-rolled as
`while let Some(d) = dir` at `locks/lib.rs:45`, `config/lib.rs:722`,
`docs/manifest.rs:169`, `docs/resolve.rs:75`, and `docs/lockfiles.rs:38`.
Three of those five are `ancestors().find(...)` spelled long.

## Design

Five changes, sequenced so each lands green on its own. Steps 0 and 1 are what
make step 3 small.

### 0. Ancestor walks, and one home for the checkout root

Replace the five hand-rolled loops with `Path::ancestors()`. No helper is added
to `devkit-common`: the standard library already provides the iterator, and a
`walk_up` wrapper would be a third spelling of it in a codebase that has two.

Move `find_root_from` (`locks/lib.rs:39`) into `devkit-common::paths`. It
answers "which checkout am I in", which is a path primitive, and it currently
sits where `devkit-config` cannot reach it. `devkit-locks` keeps its callers
and re-exports nothing.

`devkit-docs::resolve::project_root` (`resolve.rs:75`) is deleted rather than
moved. It wants the nearest ancestor holding a `devkit.toml`, which is what the
layer discovery of step 2 already reports.

### 1. Invert the `common` / `config` dependency

`devkit-common` becomes a true leaf: shared primitives, no internal
dependencies. `devkit-config` depends on it.

- `expand_tilde` moves to `devkit-common::paths` beside `find_root_from`.
  `devkit-config` calls it from there.
- `TrackerKind` moves to `devkit-common::tracker`, beside the trait it
  describes. `devkit-config`'s `TrackerConfig` (`lib.rs:211`) references it
  across the new edge. Its `JsonSchema` derive travels with it, so the schema
  is unchanged.
- `GithubConfig` stays in `devkit-config`. `Repos::resolve` and `build` take
  `issues_repo: Option<&str>` and `pr_repo: Option<&str>` instead of the
  struct, and the two callers that have a `GithubConfig` destructure at the
  call site.

Nothing about behavior changes. The gate is that `cargo test --workspace` and
`cargo clippy --workspace --all-targets -- -D warnings` stay green with
`devkit-common`'s `[dependencies]` holding no `devkit-*` entry.

### 2. One layer discovery, three consumers

`devkit-config` exposes the ordered layer paths as a public function. Shape:

```rust
/// Config layer paths that apply at `start`, lowest precedence first.
pub fn layer_paths(start: &Path) -> Result<Vec<PathBuf>>;
```

`discover` keeps its private table-reading behavior and is expressed in terms
of it, so there is one definition of which files count, in what order, with
what cutoff.

`devkit-locks::hook` and `devkit-docs::manifest` call `layer_paths` and keep
their own deserialize targets. `HarnessProbe` and the `[docs]` extraction
genuinely differ and stay where they are. Only the file set is shared, because
that is the part with no reason to differ.

`harness_enabled` and `global_harness_enabled` collapse into one function over
the layer list. `resolve_enforcement` keeps its `DEVKIT_ENFORCE_WRITES`
override and its precedence (`hook.rs:183`), and `enforcement_enabled` keeps
its signature, so `src/bin/lockm.rs:168` is untouched.

Layer-collection policy stays in `devkit-config`. It is config knowledge, not a
primitive, and pushing it into `devkit-common` would put the file names in a
crate that has no opinion about them.

### 3. The main-checkout layer

`layer_paths` gains one source: the main checkout of the git repository the
start directory belongs to.

Resolution, in order:

1. `find_root_from(start)` gives the checkout root.
2. If `<root>/.git` is a directory, this is already the main checkout and
   nothing is added.
3. Otherwise ask git: `git rev-parse --path-format=absolute --git-common-dir`,
   and take the parent of the result.

Step 3 spawns git rather than parsing the `.git` file by hand. Measured at
1.0ms against 0.69ms for a bare process spawn on this machine, so the cost is
the spawn, not git. Delegating means devkit does not carry its own model of
git's worktree layout, and `src/bin/issue/end.rs:80` already asks the same
question the same way.

**Precedence.** The main checkout's `devkit.toml` then `devkit.local.toml` sit
directly below the current checkout's own two files and above everything the
upward walk found. Path depth already implies that order: the main checkout is
more specific to this repository than any ancestor directory, and less specific
than the worktree you are standing in. A worktree keeps the ability to override
by dropping its own file, so per-branch divergence stays available as a
deliberate act rather than an accident of when the worktree was created.

**Dedupe.** When the start directory is the main checkout, the walk already
found those files. They are contributed once.

**Cutoff.** `[config] root = true` cuts off the main-checkout layer the same
way it cuts off the home config.

**Degradation.** Config resolution must not start failing because git is
absent. `devkit brief` is required to stay silent outside a devkit project and
the `lockm` hook must never block a write. A git invocation that fails for any
reason adds no layer and is not an error.

**Edge cases.** A `--separate-git-dir` clone and a submodule both produce a
common dir whose parent is not a checkout. The resulting path holds no config
files, so the layer is empty and nothing is added. A bare main worktree behaves
the same way. These are absorbed by the "the file is not there" path rather
than special-cased.

### 4. Path anchoring, per key

`resolve_path_key` (`lib.rs:940`) anchors a relative `[defaults]` path to the
directory of the layer that declared it. Read from a worktree, a main-checkout
layer needs two different anchors, because its three path keys mean two
different things:

| Key | Means | Anchor |
| --- | --- | --- |
| `worktree_root` | machine layout | declaring layer's directory |
| `baseline_path` | machine layout | declaring layer's directory |
| `doppler_yaml` | a file inside the repository being worked on | the consuming checkout |

`baseline_path = "."` in a main-checkout layer must resolve to the main
checkout. `doppler_yaml = "doppler.yaml"` must resolve to the worktree that is
reading it, so that a branch adding an app to the map takes effect without
merging first. A single rule cannot give both.

For every other layer the two anchors coincide, so this changes nothing except
for main-checkout layers.

**Docstrings.** The doc comments on `Defaults` (`lib.rs:222`, `:229`, `:234`)
generate the published JSON Schema's hover text. They currently say only
"`~` is expanded" and have never mentioned layer-relative anchoring at all,
which was already a gap before this change. Each of the three gains its
anchoring rule, and `worktree_root` and `baseline_path` say explicitly that
they anchor to the declaring layer even when that layer is the main checkout.
Regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`.

`docs/configuration.md:109` says these types live in
`crates/devkit-ports/src/config.rs`. They live in
`crates/devkit-config/src/lib.rs`. Fix it in the same change.

## Behavior changes

Each is intended. Listed so none of them arrives as a surprise.

- A linked worktree resolves the main checkout's config. Where a worktree
  currently carries a copy, the copy still wins, because it sits higher in the
  stack.
- `[docs]` starts honoring `devkit.local.toml` and the `[config] root = true`
  cutoff.
- `[harness]` starts honoring the full walk and `devkit.local.toml`, so a
  harness declaration no longer has to sit at the checkout root.
- devkit's own worktrees under `devkit-worktrees/` begin resolving
  `devkit/devkit.toml`, so they pick up its `[harness]`, `[github]`, and
  `[tracker]`. They run without all three today. This is the bug being fixed,
  and it lands on this repository first.
- The `lockm` PreToolUse hook goes from two flat file reads to an ancestor walk
  plus a git invocation. It runs once per agent tool call.

## Non-goals

- `issue sync-includes` (`docs/superpowers/plans/2026-08-26-issue-sync-includes.md`)
  is unaffected and still needed. It covers the rest of `worktree_include`:
  `CLAUDE.local.md`, `.env.local`, hook scripts. This change only removes
  config from the set of files that must be synced.
- No new config key. The main checkout is derived, not declared. A key naming
  it would have to live in the file the worktree cannot yet see.
- No caching of the git call. The hook is a fresh process per invocation, so a
  process-lifetime cache would not help it, and 1ms does not warrant a
  cross-process one.

## Gate

`cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all`, on each step independently.

Step 1 additionally requires `devkit-common/Cargo.toml` to contain no
`devkit-*` dependency. Step 4 requires the committed
`schema/devkit-config.json` to match, which `cargo test` already enforces.

## Unresolved

None. The scope question (worktrees need zero devkit files, harness included),
the resolution mechanism (derive from git, no config key), and the anchoring
rule (per key, by what the key means) are settled.
