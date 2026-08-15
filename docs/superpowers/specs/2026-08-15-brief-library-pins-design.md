# Library pins in the brief: ambient versions that agree with docm

**Date:** 2026-08-15
**Status:** ready to implement. Survived five rounds of adversarial review
(transcript in `2026-08-15-brief-library-pins-review-log.md`); round 5 found no
blocking defects. The section renders as a table; `pins` is a library function
with two callers, the brief and a new `docm list --project`; delivery is three
milestones, of which only the first is unconditional.

What review changed, in descending order of how wrong the spec was:

- **Relevance cannot be read off `Outcome`.** Both obvious rules were broken in
  opposite directions — "not `Undeclared`" rendered every machine-wide
  registration in every repository, and "only `Version`" hid real dependencies,
  because a `ref` pin short-circuits resolution before the importer runs.
  Declaration evidence is now its own tri-state field, recorded before the
  resolution checks that can fail after it.
- **The batching refactor is not mechanical**, its equivalence test was
  circular, and it lands earlier than first planned: the relevance probe makes
  per-session cost scale with the accumulating global catalog, not with rows
  rendered.
- **There is a width budget after all** — `ui::table` applies a terminal width
  with a 100-column off-TTY fallback — and a size budget the renderer must
  impose itself, since wrapping is not truncation.

The cap, the evidence ranking, the conditional workspace suffix, and the
cache-materialization read remain gone; they rationed *rows* against a
constraint that does not exist. The bounds that replaced them are on bytes.
**Source:** `feat/docs-pin` (8 commits, pushed, unmerged). Built against
`main` at 0.12.1; the 0.13.0 docm correctness rework
(`2026-08-03-docm-correctness-design.md`) landed 85 commits underneath it and
invalidated half the branch. This spec is the re-implementation, not a rebase.

## Problem

An agent learns which version of a library a project pins exactly once — when
the `devkit:docs` skill runs — and then forgets. Nothing restates it. A long
session drifts back to training-set recall, and the drift is silent: the answer
keeps the skill's `file:line` citation format while sourcing facts from memory.
That failure was measured on the source branch: 5 fresh subagents per arm, 2/5
clean before a skill fix, 4/5 after.

`devkit brief` already injects project orientation at session start, on
directory change, and after compaction. It is the right carrier. The source
branch added a library-versions section to it and got the section wrong three
times in eight commits, each time the same way: **the brief asserted a version
that resolution would not actually serve.**

What 0.13.0 changed underneath that work:

| # | Change on main | Effect on the branch |
|---|---|---|
| 1 | Checkouts are named for the ref that produced them; `resolve` hard-errors when a recorded ref diverges (`resolve.rs:197-205`) | `72ecfde` (stale shared `default` worktree) is obsolete — the bug cannot occur |
| 2 | Resolution moved to the importer graph (`importers::select`); `lockfiles.rs` demoted to prune liveness checks (`lockfiles.rs:1-8`) | `ecf9730` (batched lockfile parsing) targets a module that is no longer the resolution path |
| 3 | Resolution is fail-closed — no silent default-branch fallback without `--allow-default-branch` (`resolve.rs:142,163,171`) | the branch's `"N unpinned → default branch"` clause is now false |
| 4 | `skills/docs/SKILL.md` rewritten with stronger provenance rules | `1ecd551` would revert main |

Three of eight commits survive contact (`856c165`, `9605089`, and the final
shape of `345b2a8`). The rest is redesign.

## Governing principle

The brief states only what it can prove from the filesystem, and never a
version that differs from the one resolution would select.

Two things follow, and both are load-bearing:

**One truth source.** `lockfiles.rs` is not a cheap version of
`importers.rs` — it answers a different question. `pnpm_versions`
(`lockfiles.rs:124`) collects every version key including transitive copies,
and `highest` (`lockfiles.rs:49`) picks the numerically largest with no regard
for what the workspace declares. `importers` exists because that is wrong: it
walks the importer graph, rejects transitive-only packages (`undeclared`,
`importers.rs:91`), rejects non-registry installs (`assert_registry_source`,
`importers.rs:1223`), and rejects npm aliases that install a different package
(`assert_npm_row_is_package`, `importers.rs:834`). A second parser in the brief
re-opens the shipped bug by construction, so there is no second parser.

**The claim is scoped to the lockfile, not the cache.** `resolve_locked` runs
`locate_tag` *after* `select` succeeds; that can fetch over the network, and it
returns `None` on a tag miss — `resolve_locked` is what turns that `None` into
the `bail!` (`resolve.rs:124,142`). The fail-closed policy lives in the caller,
not in `locate_tag`, and must stay there. Either way `select` agreeing with what
`docm` finally serves is *not* guaranteed, and no cheap mechanism can make it so. The
brief therefore claims what it can prove — "this is the version your lockfile
installs" — and leaves "this is the checkout you get" to `docm info`.

## Design

### 1. Batched selection

`importers::select` takes one package per call and re-reads and re-parses the
lockfile every time. The parse dominates: 597 KB of YAML through
`serde_yaml_ng` (`importers.rs:714`) is ~24 ms of the ~38 ms per call, and
everything after it is in-memory traversal. The brief resolves every registered
library at once, on every session start.

Introduce a per-`(ecosystem, start)` context; `select` becomes a wrapper over
it so every existing caller and test is unchanged.

```rust
pub struct Selector {
    context: Context,
}

impl Selector {
    /// Parse everything the ecosystem's lockfiles hold, once. Per-package
    /// traversal then runs against the parsed values.
    pub fn new(start: &Path, ecosystem: Ecosystem) -> Result<Self>;

    /// The full report (§3). `select` is its projection, here as at the free
    /// function level.
    pub fn inspect(&self, package: &str) -> Inspection;

    pub fn select(&self, package: &str) -> Result<Selection> {
        self.inspect(package).result
    }
}

pub fn select(start: &Path, ecosystem: Ecosystem, package: &str) -> Result<Selection> {
    Selector::new(start, ecosystem)?.select(package)
}
```

The split points. These are *not* pure code moves — two of them change
semantics or borrow structure, called out below the table:

| Function | Package-independent (→ context) | Per-package (stays) |
|---|---|---|
| `js` (`163`) | `164-182`: workspace + lock-dir discovery, `rel_key`, `present`, `nearest_package_manager` | `231`: `select_js_lock` |
| `pnpm` (`710`) | `711-722`: read, parse, `lockfileVersion` gate | `723-761`: importer lookup, `pnpm_candidates`, locator walk |
| `bun` (`463`) | `467`: read, `json5_ish`, `workspaces` lookup | `479-515` |
| `npm` (`864`) | `868`: read, parse, version gate | `883-931` |
| `from_package_array` (`1271`) | `1280-1302`: read, `toml::from_str`, Cargo version sweep, `choose_member` | `1303-1341`: `package_candidates`, edge matching, fork disambiguation, `assert_registry_source` |

`cargo` (`1354`) and `uv` (`1381`) hoist their manifest read the same way.

**Wrinkle one: eager parsing is a behavior change.** `js`'s no-`packageManager`
ambiguity arm (`importers.rs:206-227`) probes the package against *each* present
lockfile to build its error message, so a context that decides at construction
time loses the outcomes list. The obvious fix — parse every present lockfile up
front — is wrong: today `js` chooses the manager first and parses only the
chosen lockfile (`importers.rs:183`), so a malformed *unselected* lockfile is
silently ignored when `packageManager` names another. Eager parsing turns that
into a hard failure for a project that resolves fine today.

Parse lazily and memoize instead — each lockfile is parsed at most once per
context, and only if something asks for it:

```rust
enum ParsedLock {
    Bun(JsonValue),
    Pnpm(YamlValue),
    Npm(JsonValue),
}

struct JsContext {
    workspace: PathBuf,
    lock_dir: PathBuf,
    relative: String,
    package_manager: Option<String>,
    /// Lockfiles present in `lock_dir`, parsed on first use and cached.
    /// A parse failure is stored, not propagated: the ambiguity arm needs it
    /// as one lockfile's *outcome*, and the non-ambiguous path must keep
    /// ignoring lockfiles `packageManager` did not select.
    present: RefCell<Vec<(&'static str, Option<Result<ParsedLock, CachedErr>>)>>,
}
```

**A cached error must be replayable.** `anyhow::Error` is not `Clone`, so a
memoized `Result<ParsedLock>` can be handed out exactly once — the second
package to hit the same malformed lockfile would either move it or get a
reconstructed error that is not the same error. Nor is `Arc<anyhow::Error>` a
drop-in: `anyhow::Error` does not itself implement `std::error::Error`, so an
`Arc` of it cannot be turned back into one. `CachedErr` is
`Arc<dyn std::error::Error + Send + Sync + 'static>`, which is cloneable, keeps
the cause chain, and converts back into an `anyhow::Error` on each replay. The
goldens (task 1) assert every rendering the compatibility wrapper promises —
`{}`, `{:#}`, and `Debug` — on the first *and* second package to touch a failed
parse.

Error text is preserved verbatim, including the order lockfile outcomes appear
in.

**Wrinkle two: Cargo's split is a borrow problem, not a move.**
`choose_member` returns `&LockPackage` borrowed from the parsed lock
(`importers.rs:1088`). A context holding both the parsed lock and the chosen
member is self-referential and will not compile. Store the chosen member's
*index* into the owned package vector and re-index per call, or re-run
`choose_member` against the owned parse during selection — it is a cheap
in-memory scan, and the expensive part (the file read plus `toml::from_str`) is
what hoisting was for.

Expected cost for 20 libraries on one `pnpm-lock.yaml`: one parse plus twenty
in-memory walks, ~40-60 ms against ~496 ms today. The cost model is per
`(ecosystem, lock_dir)` parse, **not** per library — a mixed Rust+JS project
with 20 registrations pays two parses.

### 2. A typed marker for "not declared here"

The brief must distinguish "this workspace doesn't depend on that library" — a
routine, uninteresting state — from "your lockfile is malformed". Both arrive
today as an untyped `anyhow::Error`, and matching on message text is fragile.

Give `undeclared` a downcastable type without changing what it prints. It has
to be the **outer** error, not a cause:

```rust
/// `package` is present in the lockfile only transitively, or not at all —
/// this workspace does not declare it. Typed so a caller resolving many
/// libraries can tell an uninteresting miss from a broken lockfile.
#[derive(Debug)]
pub struct Undeclared {
    pub package: String,
    pub workspace: PathBuf,
    /// The full diagnostic `undeclared` builds today, rendered verbatim by
    /// `Display` so no output changes.
    message: String,
}

impl std::fmt::Display for Undeclared { /* writes `message` */ }
impl std::error::Error for Undeclared {}
```

Two constraints the obvious version violates. `anyhow::Error::new` requires
`std::error::Error + Send + Sync + 'static`, so `#[derive(Debug)]` alone will
not compile. And wrapping with `.context(<message>)` would preserve `{}` but
change `{:#}`, `Debug`, and `main`'s error rendering — `undeclared` returns a
single causeless `anyhow!` today, so attaching a cause is observable. Making
`Undeclared` the outer error with `Display` equal to the current message keeps
every rendering byte-identical, and `err.downcast_ref::<Undeclared>()` finds it.

`undeclared` (`importers.rs:91`) builds the same string it does now and returns
`anyhow::Error::new(Undeclared { .. })`.

### 3. `pins` returns outcomes, not versions

`pins` is the cheap readout `devkit brief` renders: for each registered
library, what this checkout pins. Filesystem reads only — no clone, no fetch,
no worktree, no lock.

```rust
pub enum Outcome {
    /// The importer graph named a version. `workspace` is the directory whose
    /// manifest selected it; `lockfile` is the file that carried it.
    Version { version: String, workspace: PathBuf, lockfile: String },
    /// A manual `ref` pin in the manifest. No lockfile is consulted.
    Ref(String),
    /// Nothing this readout can state. One line, already short enough to render.
    Unresolved(String),
    /// This workspace does not depend on the library.
    Undeclared,
}

pub struct Pin {
    pub name: String,
    pub outcome: Outcome,
    /// Declared by a project's own `devkit.toml`, not the machine-wide
    /// catalog — evidence this library belongs to the checkout in hand.
    pub project_scoped: bool,
    /// What the importer graph can say about this workspace depending on the
    /// package. Computed separately from `outcome` because a `ref` pin
    /// short-circuits resolution (`resolve.rs:115`) and would otherwise carry
    /// no evidence in either direction.
    pub declared: Evidence,
}

pub enum Evidence {
    /// A manifest in this workspace declares the package.
    Declared,
    /// The importer graph ran and the package is transitive-only or absent.
    Undeclared,
    /// Nothing could be established — no importer manifest, a malformed
    /// lockfile, a `Selector` that failed to construct, or a Git-ecosystem
    /// entry with no importer to ask.
    Unknown,
}

pub fn pins(start: &Path, global: Option<&Path>) -> Result<Vec<Pin>>;
```

`lockfile` is carried, not derived — and it has to be carried from further back
than `Outcome`. `Selection` (`importers.rs:12`) holds `workspace`, `version`,
and a *prose* `source` ("`apps/web` installs it (pnpm-lock.yaml; 1 other version
present)"), which `selection_source` composes for diagnostics. `workspace` alone
cannot say whether a JS version came from `bun.lock`, `pnpm-lock.yaml`, or
`package-lock.json`, and the alternatives — parsing the prose back apart, or
re-deriving the manager in `pins` — are respectively fragile and a second
selection path, which the governing principle forbids.

So `Selection` gains a structured field naming the lockfile, set at the point
each manager already knows it. The prose `source` stays exactly as it is;
nothing that consumes it changes. This is additive and lands before `pins`.

**And `Result<Selection>` cannot carry declaration evidence, so it is not the
API `pins` calls.** §4 needs `Declared` preserved *across* a later failure, but
an `Err` discards everything the importer had already established — the
non-registry check (`importers.rs:480`), the missing-row check, the alias and
fork checks all run after declaration is known and return a bare error. There is
no way to recover the tri-state from that without a second parser, which the
governing principle forbids.

The importers therefore expose an inspection that reports both:

```rust
pub struct Inspection {
    pub evidence: Evidence,
    pub result: Result<Selection>,
}

pub fn inspect(start: &Path, ecosystem: Ecosystem, package: &str) -> Inspection;

/// Compatibility projection — every existing caller keeps this shape.
pub fn select(start: &Path, ecosystem: Ecosystem, package: &str) -> Result<Selection> {
    inspect(start, ecosystem, package).result
}
```

`evidence` is set where each manager establishes declaration, before the checks
that can fail afterwards. `select` stays the public entry point for `resolve`
and everything else, so the goldens cover it unchanged. This lands with the
`Selection` field, ahead of `pins`.

**The `Result` is for `docm`, the outcomes are for the brief.** Two failure
classes, and collapsing them serves neither caller. Manifest discovery failing
means the caller cannot know what to resolve — `docm list --project` must exit
non-zero and say so, not print an empty listing that reads as "no libraries".
A single library failing to resolve is data, and belongs in that row's
`Outcome`. So `pins` returns `Err` only when it cannot enumerate registrations
at all, and never for a per-library failure. The brief discards the `Err` (§8);
`docm` propagates it.

A `Selector::new` failure is likewise per-ecosystem data, not fatal: every
library of that ecosystem gets `Unresolved` carrying the construction error's
top-level message.

A `Ref` outcome runs `names::validate_ref` (`names.rs:75`) before it is
produced. `manifest::discover` validates library names but not refs, while
resolution later rejects an invalid ref through `names::checkout_dir`
(`names.rs:98`) — so without the check the table could state a pin that `docm
info` refuses to serve, which is the one thing the governing principle forbids.
An invalid ref becomes `Unresolved` carrying the validation error.

The source branch's `other_versions` field is dropped. It counted extra
lockfile versions, an artifact of `highest()`-style matching; `select` returns
the one version this workspace declares, so there is nothing to count.

`Unresolved` strings are the error's **top-level** message only
(`format!("{err}")`, never `{err:#}`). `undeclared` builds a three-line
diagnostic; that belongs in `docm info`, not in injected context.

### 4. Relevance, and what the cwd decides

**A project-scoped registration always renders. A machine-wide one renders only
when `declared` is true** — the importer graph confirms this workspace depends
on the package.

The evidence has to be tracked separately from `Outcome`, and both of the
obvious shortcuts are wrong in opposite directions:

- *"Render unless `Undeclared`"* leaks. `Undeclared` is only ever produced by a
  lockfile check, and a ref pin never reaches one — nor does a library whose
  ecosystem has no lockfile here, which surfaces as `Unresolved`. Every
  machine-wide registration would render in every repository, which is the
  `/docs` accumulation the rule exists to stop.
- *"Render only on `Outcome::Version`"* hides real dependencies. `resolve`
  checks `entry.ref` **before** consulting the importer (`resolve.rs:115`), so
  a globally registered crate pinned with `docm add --ref` yields `Ref` even
  when this workspace genuinely depends on it. The rule would silently drop a
  library the project actually uses.

So `pins` computes `declared` independently: for a machine-wide entry with a
non-Git ecosystem, it probes the importer graph for the package regardless of
whether a `ref` short-circuited the version.

**The probe must record declaration before the checks that run after it.** A
boolean cannot express what the importers actually report, because several of
them *find* the declaration and then fail on something downstream — a
non-registry source (`importers.rs:479`, `1303`), a missing package row
(`importers.rs:883`), an npm alias, a resolution fork. Those failures are
exactly the ones whose error text recommends pinning with `--ref`, so a
boolean probe would answer "not declared" for the very dependency someone
ref-pinned *on that advice*, and the row would vanish. Construction failures
and malformed lockfiles are a third state again: not evidence of absence.

Hence `Evidence`, produced by a shared inspection that records declaration at
the point the importer establishes it, ahead of the later resolution checks.
The filter renders on `Declared`; `Undeclared` and `Unknown` are both dropped
for a machine-wide entry, but they are not the same claim, and the dropped-count
footer says "not evidenced here" rather than "not declared".

This makes the cwd load-bearing, deliberately. `select` finds the *nearest*
workspace (`find_up`, `importers.rs:164`), and the hook fires on directory
change, so a monorepo session at `apps/web` and one at the repo root see
different pins. That matches what `docm` itself answers from each directory,
which is the point. The consequence to render on purpose: a library registered
in the project's `devkit.toml` but declared only by `apps/web` shows as
`Undeclared` when the session sits at the root.

| `project_scoped` | `declared` | Outcome | Rendered as |
|---|---|---|---|
| true | any | `Version` | a row: version + the lockfile and workspace that named it |
| true | any | `Ref` | a row: the ref, source `ref` |
| true | any | `Undeclared` | a row saying this workspace does not declare it |
| true | any | `Unresolved` | a row carrying the reason |
| false | `Declared` | any | a row, as above |
| false | `Undeclared` / `Unknown` | any | **dropped**, counted in the footer |

The drop is what keeps `/docs` from poisoning unrelated repositories. The skill
registers an unregistered library with a bare `docm add`, which writes the
machine-wide manifest — it asks before `--project`. So the global manifest
accumulates every library ever asked about, across every project. Without this
rule each of those appears in every project's brief, three times a session,
forever.

**A filtered view must not read as an empty catalog.** `skills/docs/SKILL.md:22`
tells an agent that a library absent from the listing is unregistered and to run
`docm add`. Against `docm list --project` that inference is false — absence
means "not evidenced here", and acting on it would re-register a library the
machine-wide manifest already holds. Both ends close this: the filtered listing
ends with a count of what it dropped, split by evidence —

```
2 registered libraries not evidenced here (1 undeclared, 1 unknown) — see `docm list`
```

— and the skill's step 1 is reworded so "unregistered" is a claim only the
unfiltered listing can support. The split matters because the two mean different
things: `undeclared` is a checked answer, `unknown` means the check could not
run, and a project seeing several `unknown` has a configuration problem rather
than a short dependency list. The footer is one line regardless of how far the
global manifest has accumulated, so it does not reintroduce the noise the filter
removes.

The rule's cost is explicit and accepted: **a lockfile-less project sees
nothing until it declares its libraries in its own `devkit.toml`.** A Godot
project's pins are all refs, refs carry no importer evidence, and no filter can
invent evidence that does not exist. Adding a `[docs]` section is a few lines,
and the declaration is better than any inference — greppable, reviewable,
surviving `docm prune` and a cache wipe, and working on a fresh clone. See
*Out of scope* for the inference-based alternative and why it loses.

### 5. Rendering

A table, alphabetical, one row per pin. Every pin that survives §4 gets a row —
no ranking and no per-entry hedges. Rows are not capped by count; the section
is bounded by *bytes*, below, and truncation is always whole rows plus a visible
marker, never a silently clipped row.

```rust
devkit_common::ui::table(&["LIBRARY", "VERSION", "SOURCE"])
```

This matches the sections either side of it: tasks render through
`ui::table(&["NAME", "KIND", "APP", "DESCRIPTION"])` and live servers through
`registry::status_table`.

**There is a width budget, and it is not the one the one-line form assumed.**
`ui::table` sets `ContentArrangement::Dynamic` and `set_width(term_width_on(..))`
(`ui.rs:10-23`), and `term_width_on` reads `$COLUMNS`, then `TIOCGWINSZ`, then
falls back to 100 (`ui.rs:264-293`). So the table *does* wrap — to the real
terminal width when a hook runs under one, and to 100 columns when it does not.

Two consequences, both design-level:

- **Long `SOURCE` cells wrap inside the cell.** An unresolved reason is a full
  sentence. Wrapped context is ugly but readable, and it is strictly better
  than the one-liner's bare count; accepted, with a test asserting the
  100-column shape rather than an unbounded string.
- **The rendered text is environment-dependent, and `--if-changed` must not
  depend on it.** Resize the terminal, change directory, and a text-hashing
  watermark reports "changed" for content that did not move. The watermark
  therefore hashes a canonical structured snapshot of the whole brief, defined
  in §7.

**`ui::table` bounds line width, not total size.** Wrapping a 40 KB cell across
100 columns yields 400 lines, not a truncation. The `undeclared` diagnostic
enumerates every candidate version *and its locations*, so a large monorepo
produces a legitimately large cell with no attacker involved. The budgets:

- **200 bytes per cell.** Longer values are cut at a character boundary and
  suffixed `…`. One `SOURCE` sentence fits; an enumeration of candidate
  locations does not.
- **4 KB per section.** Rows are added whole until the next row would exceed
  it; the remainder collapses to a single `… N more (see \`docm list
  --project\`)` row. Truncation never cuts a row in half, and the marker row is
  reserved for out of the budget rather than competing with it.

`--project --json` keeps the full escaped values — truncation is a property of
the context-injection rendering, not of the data.

This is not the row cap returning. That cap rationed *rows* against a
non-existent line-width budget; these bound *bytes* against context, which is a
real and shared resource. At realistic registration counts neither fires, and a
table still costs on the order of thirty tokens more than a comma list.

```
LIBRARY   VERSION      SOURCE
gdUnit4   v5.0.0       ref
godot     4.3-stable   ref
kysely    0.28.17      pnpm-lock.yaml (apps/web)
react     19.1.0       pnpm-lock.yaml (apps/web)
zod       —            not declared by this workspace
```

Every row above is either a `Version` or project-scoped — the `zod` row is a
library this project's own `devkit.toml` registers and its lockfile does not
declare, which is worth saying. A machine-wide `zod` in the same state produces
no row at all.

A project with no lockfile renders entirely from refs, and needs those refs
declared in its own `devkit.toml` to render at all (§4). That is the expected
shape for a git-ecosystem project, not a degraded one.

The table is what makes a failed row honest. A one-liner had to collapse
unresolved entries to a bare count, because a per-entry `(unresolved)` tag
fires on everything in a cold state and stops discriminating — that is how the
source branch's version of this failed. A `SOURCE` cell is not a hedge appended
to a claim; it is the row's content, so it can carry the actual reason and the
reader can act on it.

`VERSION` is `—` exactly when there is no version to state. `SOURCE` is `ref`,
the lockfile and workspace that named the version, or the one-line reason it
has neither.

The section prose carries the caveat once, at O(1) rather than O(N):

> These are the versions this checkout's manifests and lockfiles name. `docm
> info <lib>` resolves the matching source and reports the version it actually
> serves. Answer questions about these libraries from those checkouts;
> training-set recall is a different version.

### 6. `docm list --project`, the second caller

`pins` is a library function in `devkit-docs`, and the brief calls it directly —
the same way it already calls `registry::status_table` rather than shelling out
to `portm status` (`brief.rs:91`). No process spawn inside a session-start hook,
the typed `Undeclared` marker survives instead of degrading to string-matching
on stderr, and the brief gains no dependency on `docm` being on `PATH` while the
hook only guards `command -v devkit`.

That makes a second caller nearly free. `docm list` today prints every entry of
the merged manifest — global `docs.toml` plus the project's `devkit.toml
[docs]` (`docm.rs:137`) — with each synced checkout's dirname, commit, and ref.
It is a catalog, not a project readout, and it cannot drop a global entry:
holding libraries that no lockfile declares is exactly what a machine-wide
manifest is for.

`docm list --project` renders the §5 table instead — same relevance filter, same
resolution, one flag. The `devkit:docs` skill's `allowed-tools` becomes
`Bash(docm list --project)` and its inline block becomes project-scoped at the
point of use.

`--project` composes with the existing `--json` (`docm.rs:61`) rather than
conflicting with it. It emits an **envelope**, not a bare array:

```json
{
  "pins": [
    { "name": "kysely", "project_scoped": false, "declared": "declared",
      "outcome": { "kind": "version", "version": "0.28.17",
                   "lockfile": "pnpm-lock.yaml", "workspace": "apps/web" } }
  ],
  "dropped": { "undeclared": 1, "unknown": 1 }
}
```

A bare array cannot distinguish an empty catalog from a catalog whose every
entry went unevidenced — `[]` for both — and those call for opposite responses:
register something, versus find out why the check could not run. The envelope
also carries `declared` per pin, so a consumer gets the discriminant rather than
parsing a `SOURCE` cell. Making the flags mutually exclusive would be cheaper
and wrong; the structured form is what a non-agent consumer wants.

The no-lock, no-cache-mutation guarantee is a property of `pins`, **not** of
this command. `docm`'s `main` runs `upgrade::run` before dispatching any
subcommand except `completions` (`docm.rs:92`), which can take the cache lock,
migrate the layout, and run git. `docm list --project` inherits that, as every
`docm` subcommand does. The brief does not, because it calls the library.

Scoping is an explicit flag, never inferred from a TTY. devkit already gates on
`isatty`, and it gates *authority*: `devrun reap` and cross-worktree `devrun
down` require a PTY so an agent cannot fire them. Deciding output *content* by
TTY would mean `docm list` shows a human one set of libraries and an agent a
different set — and the skill's own inline block runs with no TTY, so what the
agent reads would permanently disagree with what the user sees while debugging
it. `--json` is the existing precedent for a caller-selected shape.

### 7. Emission modes

The flag surface comes from `9605089`; `brief.rs` has one unrelated commit of
churn on main, so it applies cleanly. The watermark behind `--if-changed` does
not survive from that branch — it hashed rendered text, which is wrong for the
reasons below.

- `--pins-only` emits just the library-versions section, rendered as the same
  §5 table. Re-injecting the whole brief after compaction spends the context
  compaction just reclaimed, and a block repeated verbatim gains recency over
  the summary it displaced. Suppressing the post-compact emission outright is
  worse — compaction is exactly when the earlier injection was discarded. There
  is no denser post-compact variant: a second renderer would re-introduce the
  format §5 removed, to save tokens §5 established are not scarce.
- `--if-changed` keeps a per-session watermark under the state dir and prints
  nothing when this session already received the same pins. This is what a
  directory change wants.

**The watermark hashes a structured snapshot of the whole brief, not the
rendered text and not the pins alone.** Two failure modes bracket this:

- Hashing the *rendered string* makes the watermark width-sensitive (§5), so a
  terminal resize re-injects a brief whose content did not move.
- Hashing *only `Vec<Pin>`* — the previous revision's fix — silently suppresses
  a brief whose apps, tasks, or live servers changed while the pins held still.
  `--if-changed` governs the whole brief; a pins-shaped watermark is the wrong
  key for it.

So the snapshot is a declared type, not "whatever the brief happens to hold":

```rust
struct BriefSnapshot {
    root: String,
    apps: Vec<String>,                 // sorted
    tasks: Vec<(String, String, String, String)>,  // sorted
    servers: Vec<ServerKey>,           // sorted: port, app, role, pid, listening
    pins: Vec<PinKey>,                 // sorted: name, project_scoped, declared, outcome
}
```

**`AGE` is excluded, deliberately, and it is the reason this needs writing
down.** `status_table` renders `PORT, APP, ROLE, HOLDER, PID, LISTENING, AGE`
and computes `AGE` against `now()` (`registry.rs:671-674`). Hashing the rendered
server rows makes the digest change every second, so `--if-changed` degenerates
to always-changed. Hashing the raw registry rows instead misses `LISTENING`,
which is probed rather than stored, so a server going down would not re-emit.
`ServerKey` therefore carries identity plus the probed listening state and drops
age.

**One probe, two consumers.** `status_table` probes liveness itself, so
constructing `ServerKey` and then calling it re-probes — and a server that goes
down between the two makes the brief hash one state while injecting another,
which is a watermark that certifies text nobody was shown. The snapshot and the
rendered rows are built from a single pre-probed view of the registry.

Project identity is part of the key, and an empty result is an explicit snapshot
rather than an absent one — otherwise moving into a repository with no surviving
pins emits nothing, and the previous repository's pins stay in context as the
most recent thing the agent was told.

The watermark fails open: an unreadable or unwritable state directory reports
"changed", costing a duplicate brief rather than silently withholding one.

**Neither flag has a shipped carrier yet.** `hooks/hooks.json` invokes bare
`devkit brief` on `SessionStart` with matcher `startup|resume|clear`, and
nothing else; the `PostCompact` and cwd-change invocations exist only in
personal settings. The wiring is a contract, not a task name:

| Event | Matcher | Command |
|---|---|---|
| `SessionStart` | `startup\|resume\|clear` | `run-hook.cmd brief` |
| `PostCompact` | — | `run-hook.cmd brief --pins-only` |
| `CwdChanged` | — | `run-hook.cmd brief --if-changed` |

**Not a raw POSIX guard.** The obvious `command -v devkit >/dev/null 2>&1 && …
|| true` is bash-only, and this workspace tests on Windows. `hooks/run-hook.cmd`
already exists for exactly this — a polyglot shim that finds Git-for-Windows
bash, falls back to a `.ps1` twin, and exits 0 silently when neither is present.
`bootstrap-binaries` plus `bootstrap-binaries.ps1` is the precedent to copy: one
`hooks/brief` script carrying the presence check and flag pass-through, one
`hooks/brief.ps1` twin, both forwarding stdin so the payload survives.

**The session id, and what happens without one.** It arrives on the hook's
stdin JSON as `session_id`. Two rules:

- **No id means emit without persisting.** Falling back to a per-cwd key makes
  concurrent sessions share one watermark, so A → B → A suppresses A's
  re-injection even though B displaced it in A's context. A duplicate brief is
  the acceptable failure; a withheld one is not.
- **The filename is a hash of the complete raw id**, not an allowlisted
  transcription of it. Dropping disallowed characters is lossy, and two ids
  differing only in what was dropped would collide onto one watermark. Hashing
  is both injection-proof and total.

The integration test covers the five behaviors that only exist once a hook
supplies the input: startup emits, compaction emits pins only, a cwd change
inside one project emits nothing the second time, two session ids do not share
a watermark, and a cwd change *out* of a devkit project emits the empty
snapshot rather than silence.

### 8. Config gating

The hooks ship enabled; **config decides whether they produce anything.** The
alternative — leave the hooks out and document them for each user to paste in —
puts a multi-step editing task between someone and a feature they have not
seen, and leaves every installation's wiring slightly different. A shipped hook
plus a config switch inverts that: the wiring is uniform and correct
everywhere, and turning the output off is one line.

```toml
[brief]
enabled = true   # the whole brief
pins = true      # the library-versions section
```

This is a new `BriefConfig` on `Config` (`config.rs:7`), so it inherits the
existing deep-merge layering for free: set it in `~/.config/devkit/config.toml`
as a personal default, override it per project in that project's `devkit.toml`,
and `Provenance.origin` already records which layer won. No new resolution path.

Three properties this has to hold:

- **The gate is read before the work.** `enabled = false` returns before pins
  resolve. The relevance probe scales with the accumulating global catalog
  (§4), so a switch that suppressed output *after* paying for it would be a
  switch that does not switch anything off.
- **It reads `config::resolve`, not `load::load`.** `resolve` parses toml and
  merges layers; `load` additionally reads `doppler.yaml` and builds the app
  catalog, which is the part that fails on a docs-only project (§9). Gating on
  the expensive one would reintroduce the bug §9 fixes.
- **An unreadable config fails open to the defaults**, consistent with the rest
  of the brief: a malformed personal config costs an unwanted brief, never a
  silently withheld one.

The personal config is not a membership signal (`is_project_member` excludes
the home layer), but `[brief]` is a *preference*, not membership — so
`enabled = false` there does suppress everywhere, which is the point of setting
it there.

### 9. Failure containment and the membership gate

Two changes to `brief::render` (`brief.rs:23`), both about not letting the new
section break the old one.

**The pins section never gates the brief.** `render` returns `None` on any
failure today — "any failure means no output, exit 0" (`brief.rs:15-17`).
Manifest discovery is a new failure surface, so a broken `docs.toml` would kill
the apps, tasks, and servers sections too. Pins resolve inside a function
returning `Option<String>`; `None` omits the section and leaves the rest.

**Pins render outside the devrun path entirely — and the blocker is `load`, not
the membership gate.** `is_project_member` (`brief.rs:54`) passes for any
non-home `devkit.toml` layer, so a docs-only project already satisfies it. The
actual short-circuit is one line earlier: `render` does `load::load(None, cwd).ok()?`
(`brief.rs:28`) before it ever reaches the gate, so a `devkit.toml` carrying
`[docs]` and nothing devrun can use returns `None` for the whole brief. `render`
therefore computes the pins branch *before* `load`, and returns `Some` when
either half is non-empty. The test that matters is a `devkit.toml` with a
`[docs]` section and no usable devrun configuration.

Values reaching the brief come from checked-in manifests and land in agent
context, so names, versions, and refs render through an allowlist of the
characters they actually need — and so does the `Unresolved` reason text, which
is lockfile-derived (it interpolates package names and paths) and was otherwise
the one dynamic cell going in raw. Control and bidi characters are the hazard
being closed; the values themselves come from files the agent is already reading
in full, so no amount of quoting adds a boundary that is not already there.

### 10. SKILL.md

Two edits. `allowed-tools` moves to `Bash(docm list --project)` and the inline
block with it (§6). And one rule is added; main's rewrite is otherwise stronger
than the source branch's and stands. Reading a second version is a lookup, not a
recollection — and the bare clone already holds every tag, so it costs one `git
show`, not a new worktree.

## Delivery

Ordered so the user-visible section ships first and the refactor that touches
`docm`'s truth source lands last, once the feature it serves is proven.

The order is driven by three facts, not taste. A lockfile-less project never
calls `select` at all, so batching does nothing for it. The batching refactor is
the only task that can regress `docm info`. And the CLI flag is the shortest
path to the content, so it lands before the brief section whose necessity is
still open.

**Milestone A — the content, through the CLI.**

1. **Behavior goldens for `importers::select`.** Recorded against the *current*
   implementation across the fixture matrix, success and error alike. Written
   before any refactor, because after one there is nothing left to compare to.
2. The `importers.rs` API surface everything downstream needs: `Undeclared` as
   a typed outer error, a structured lockfile field on `Selection`, and
   `inspect` returning `Inspection { evidence, result }` with `select` as its
   projection (§3). All additive; the prose `source` and `select`'s signature
   are untouched.
3. `pins` returning `Result<Vec<Pin>>`, on the free `inspect`, taking
   `declared` from the same call that yields `outcome`. Manifest and lockfile
   reads only, no cache reads, no lock.
4. The shared renderer: the §5 table with per-cell and per-section truncation.
   One implementation, used by every caller from here on.
5. `docm list --project` and `--project --json`, plus the dropped-count footer;
   SKILL.md `allowed-tools`, inline block, the reworded step 1 that stops
   treating a filtered view as the catalog, and the bare-clone-holds-tags rule.

**Milestone C — cost.** Before any hook wiring, because the relevance probe
makes per-session cost scale with the accumulating global catalog rather than
with the rows rendered.

6. `Selector` extraction in `importers.rs`; `pins` changes to hold one
   `Selector` per ecosystem. Task 1's goldens are the gate.

**Milestone B — the ambient carrier.** A is complete and useful on its own, so
B still lands second; but §8 makes it reversible rather than conditional, which
is a better answer than withholding it. See below.

7. `[brief]` config gating (§8), read before any pins work.
8. Brief section, plus the `render` restructure — pins computed before `load`,
   failure containment.
9. `--pins-only` / `--if-changed`, the canonical `BriefSnapshot`, and the
   `hooks/hooks.json` matrix that gives all three invocations a caller.

**Batching is not free behind an unchanged call shape.** An earlier draft
claimed `pins` could be written against per-call `select` and inherit batching
with no caller change; that is false, because `select(start, eco, pkg)`
constructs a fresh `Selector` every call. Task 6 therefore has two halves — the
extraction *and* the `pins` change that reuses one selector per ecosystem — and
without the second half the first buys nothing. The call shape stays stable for
every *other* caller, which is what keeps the blast radius small.

**Why cost lands between A and B: the probe changes what N is.** An earlier
draft put the `Selector` work last, arguing that it is the only task that can silently
regress `docm info` and that today's cost is ~38 ms for the one registered
library. The second half of that was wrong, and §4 is what makes it wrong. The
relevance probe runs for *every* machine-wide non-Git registration, including
every one it is about to drop — and the whole reason the filter exists is that
the global manifest accumulates every library ever asked about, across every
project. So the hook's cost scales with the catalog, not with the rows
rendered, and the filter is itself what makes N grow. Deferring batching to the
end preserves exactly the pressure the filter creates.

The isolation argument still holds, so it keeps its place *relative to the
brief*: milestone A ships and proves the table through the CLI, C makes the
probe cheap, and only then does B wire a session-start hook. Task 1's goldens
guard C wherever it lands; what they cannot do is make a hook cheap after the
fact.

**The config switch dissolves the question rather than answering it.** B was
conditional because building an ambient injection nobody wants is expensive to
undo — it costs tokens in every session, and removing it later means another
release. A `[brief] pins = false` line makes that reversible per user and per
project, at which point withholding B to gather evidence is the more expensive
option: it delays the feature for everyone to answer a question a one-line
override already handles.

So B ships, defaulted **on**, and the evidence still gets gathered — from
whether anyone turns it off, which is a better signal than the counterfactual
the question originally asked for.

## Testing

- **Behavior goldens, captured before the refactor (task 1).** For a fixture
  matrix covering pnpm, bun, npm, Cargo, and uv — including a monorepo with two
  workspaces pinning different versions of one package — record what today's
  `select` returns for every package, success and error text alike, and assert
  against those recordings afterwards. Comparing `Selector::new(..).select(pkg)`
  to `select(start, eco, pkg)` *after* the refactor proves nothing, because by
  then the latter is defined as the former; it tests the wrapper. The goldens
  are the whole safety net for task 6, which is why they are task 1.
- **A malformed unselected lockfile is still ignored.** `packageManager` names
  pnpm, a corrupt `bun.lock` sits beside it: resolution succeeds, as it does
  today. This is the regression the eager-parse design would have introduced.
- **Ambiguity error survives batching.** Two lockfiles, no `packageManager`:
  the error still enumerates per-lockfile outcomes, in the same order.
- **`Undeclared` renders byte-identically.** Assert `{}`, `{:#}`, and `Debug`
  against the pre-change strings — a cause attached in the wrong place changes
  the last two while leaving the first intact.
- **Parse count.** Instrument or assert indirectly that 20 registered packages
  against one lockfile parse it once. A count assertion, not a timing
  assertion — timings are flaky on loaded CI runners.
- **Agreement.** On one fixture, `pins` and `resolve` select the same version
  for the same library from the same cwd. This is the test that replaces
  "correct by construction" for the part construction cannot guarantee.
- **Undeclared is not an error row.** A machine-wide registration in a repo
  that does not declare it produces no rendered entry, and does not suppress
  the libraries that did resolve.
- **Fail-soft per row.** A malformed lockfile for one ecosystem leaves another
  ecosystem's pins rendered.
- **Brief survives a broken `docs.toml`** — apps and tasks still render, and
  `docm list --project` on the same manifest exits non-zero. One condition, two
  policies; this is what the `Result` exists for.
- **A docs-only `devkit.toml` renders pins.** A project whose config has a
  `[docs]` section and nothing devrun can use gets a pins table — the case
  `load::load(..).ok()?` silently killed before the restructure.
- **A machine-wide ref pin produces no row.** Registered globally, no
  `[docs]` entry in the project: nothing renders, in a repository of every
  ecosystem including one with no lockfile. The same library declared in the
  project's `devkit.toml` does render. This is the §4 hole, tested directly.
- **100-column rendering is bounded.** With no TTY, a pin whose `SOURCE` is a
  full unresolved sentence renders inside the fallback width rather than as an
  unbounded line.
- **A machine-wide ref pin the workspace *does* declare still renders.** `docm
  add <crate> --ref v1.2.3` globally, in a Cargo project that depends on it:
  the row appears, sourced `ref`. This is the false negative the
  `Outcome::Version`-only rule would have introduced, and it is the inverse of
  the test above — both must pass at once.
- **The filtered listing states what it dropped.** `docm list --project` in a
  project declaring one of three registered libraries prints one row and a
  count of the other two.
- **The watermark ignores width but not content.** The same brief under two
  different `$COLUMNS` values emits once; a brief whose tasks changed while its
  pins held still emits again; moving to a project with no surviving pins emits
  an explicit empty section rather than nothing.
- **Evidence survives a post-declaration failure.** A workspace declaring a
  package from a non-registry source (`git`/`path`) yields `Err` from `select`
  *and* `Evidence::Declared` from `inspect`. This is the case whose error text
  recommends `--ref`; a globally ref-pinned library in that state must still
  render.
- **A cached parse error replays.** Two packages against one malformed
  lockfile: the second gets the same `{}`, `{:#}`, and `Debug` output as the
  first.
- **No session id emits every time.** Two invocations with no `session_id` on
  stdin both emit; two with the same id emit once; two ids differing only in
  characters an allowlist would strip do not share a watermark.
- **The JSON envelope distinguishes empty from unevidenced.** A project with no
  registrations and a project where every registration is `Unknown` produce
  different `dropped` counts, not two empty arrays.
- **`[brief] enabled = false` suppresses everything**, in a project that would
  otherwise render a full brief, whether set in the personal config or the
  project's `devkit.toml`; `pins = false` suppresses only that section.
- **The gate precedes the work.** With `enabled = false`, no manifest is
  discovered and no importer runs — asserted by pointing the config at a
  manifest whose resolution would fail loudly, and observing silence and exit 0.
- **A malformed config still emits.** An unparseable `[brief]` table falls back
  to the defaults rather than withholding the brief.
- **The project layer wins.** `pins = true` personally and `pins = false` in the
  project's `devkit.toml` yields no pins section.
- **The hook runs on Windows.** The `brief` hook resolves through
  `run-hook.cmd` and its `.ps1` twin, and forwards stdin far enough for
  `--if-changed` to read `session_id`.
- **A pathological cell is truncated, visibly.** An `undeclared` diagnostic
  enumerating hundreds of candidate locations renders truncated with an
  explicit marker, and `--project --json` on the same input carries the full
  value.
- **One row per surviving pin, alphabetical.** 20 registered libraries produce
  20 rows; no truncation, no counts, no ordering by tier.
- **A failed row states its reason.** An unresolved entry renders with the
  reason in `SOURCE`, not as a bare count and not as a `(unresolved)` suffix on
  a version.
- **Ref pins render without a lockfile.** A git-ecosystem project with no
  lockfile of any kind produces a full table, not an empty section — the
  lockfile-less case is the shape, not a degradation.
- **A machine-wide registration this workspace does not declare produces no
  row**, and does not suppress the libraries that did resolve. This is the
  `/docs`-accumulation guard; assert it with two registered libraries where
  only one is declared.
- **A project-scoped registration this workspace does not declare does get a
  row** — the monorepo case, where silence would hide a deliberate
  registration.
- **Both callers agree.** `docm list --project` and the brief's section render
  the same rows for the same cwd. One renderer, asserted rather than assumed.
- **`docm list` without the flag is unchanged.** The catalog form still lists
  machine-wide entries this workspace does not declare — the flag adds a view,
  it does not narrow the default.
- **Watermark.** Identical text twice under one session id prints once;
  an unwritable state dir prints both times.

## Risks

- **Task 6 restructures the module that is `docm`'s truth source.** Every
  per-manager path is dense with validation whose *ordering* matters —
  candidates are collected before the declaration check so error messages can
  cite them (`importers.rs:479-486`, `728-750`, `883-891`). A refactor that
  reorders or drops any of it regresses `docm info`, not just the brief.
  Adversarial review found two specific ways the "mechanical move" framing was
  wrong — eager lockfile parsing changes which malformed files are fatal, and
  `choose_member`'s borrow makes the Cargo context self-referential — so treat
  the remaining split points as suspect until the goldens say otherwise.
  Landing it after milestone A means the pins table is already shipped and
  green when this starts, so a regression is unambiguous.
- **The batching payoff is unmeasured on a real project.** The 496 ms figure
  came from a synthetic 20-library fixture; one library is registered today,
  and the most-used devkit project has no lockfile at all, so batching does
  nothing for it. Accepted deliberately: unbatched leaves permanent cost
  pressure in a session-start hook pointing at the cheap-and-wrong `lockfiles`
  path, which is what caused the original bug. If task 6 proves harder than
  §1 predicts, milestone A stands on its own and it can be cut.
- **Unresolved rows may be common enough to be noise.** A table makes them
  cheaper than the one-line form did — each carries an actionable reason rather
  than inflating a count — but if most rows are unresolved the section is
  reporting a broken configuration rather than orienting anyone. Measure on a
  real multi-registration project before defending it.

## Out of scope

- Anything that clones, fetches, materializes a worktree, or takes the library
  lock. `resolve_locked` does all four (`resolve.rs:90,109,210,303`); a
  first-ever session would block on N git clones. The brief stays on the
  pure-filesystem side of that line.
- Caching `select` results. The answer depends on `start`, so a cache keyed on
  lockfile identity alone is wrong across worktrees and across subdirectories
  of a monorepo. The correct key is `(lockfile identity, workspace, package)`,
  at which point batching has already taken the win without the invalidation
  risk.
- Reading the cache at all. `meta.worktrees` records `{ raw_ref, resolved_ref,
  commit }` per checkout and could say whether a pin is materialized, which
  would narrow the tag-miss gap for versions and give a ref its only
  independent evidence. It stays out because nothing consumes it: the table
  renders alphabetically, so there is no ordering for the signal to feed, and a
  per-row "not materialized" marker fires on every row on a fresh machine —
  the failure mode already established. Revisit only with a consumer in hand.
- **The reference registry as a relevance source.** `refs::RefRow` records
  `project`, `lib`, `version`, `git_ref`, and `commit` on every successful
  resolution (`resolve.rs:248`), keyed for a ref pin by `project_root(start)` —
  the nearest ancestor holding a `devkit.toml` (`resolve.rs:73`). That is a
  genuine per-project usage log, populated exactly where the manifest is silent,
  so it could supply membership for a project that declares nothing. It stays
  out for two reasons. It is a log of what *was* resolved, and its design bias
  is over-retention — it exists to stop prune deleting a checkout someone may
  still want — so a version read from it can be stale, and an ambient stale
  version is the precise failure this feature exists to prevent. (The safe form
  splits the roles: the registry answers *which* libraries, resolution answers
  *what version*. Staleness then costs an extra row, never a wrong version.)
  More decisively, its uniquely covered case is a project with neither a
  lockfile nor a declaration — a lockfile project already resolves through the
  importer graph. Three lines in a `devkit.toml` eliminate that case, and a
  declaration is greppable, reviewable, survives `docm prune` and a cache wipe,
  and works on a teammate's fresh clone. Revisit if annotating projects turns
  out not to happen in practice.
- `feat/docs-pin`. Left pushed and unmerged as the reference until a release
  makes it redundant.

## Settled by revision

The section had no width budget to spend. Once that premise went, four
mechanisms went with it — each had been introduced to ration a resource that
was not scarce:

- **The pin cap.** Triage for a problem the relevance filter already solves; it
  never fired at any realistic registration count.
- **Evidence ranking.** It existed to put the informative entries at the front
  of a line that had a front. A table is scanned, not read left to right, so
  alphabetical is better — it is how you look a library up.
- **The conditional workspace suffix** (append only when the workspace differs
  from the root). A `SOURCE` column carries it unconditionally for free.
- **The cache-materialization read** (`meta.worktrees` per ref-pinned library).
  Its only consumer was the ranking. With no ranking it buys nothing, so `pins`
  performs no cache reads at all.

Collapsing failures to counts went too. That was forced by the one-line form,
where a per-entry marker fires on everything in a cold state and stops
discriminating. A `SOURCE` cell is the row's content rather than a hedge
attached to a claim, so it states the reason and the reader can act on it.

The third revision separated the mechanism from its carrier. The relevance
filter and the resolution behind it are useful wherever a project's libraries
get listed, so they became a library function with two callers rather than a
brief-only feature. Two consequences: one renderer serves every path, including
`--pins-only`; and the brief section is no longer the only way to get the
content, which reduces the remaining open question to one about timing alone.

The fourth revision answered adversarial review, and three of its findings were
corrections to claims this spec had been making confidently:

- **The relevance filter cannot be read off `Outcome` at all.** "Not
  `Undeclared`" leaked, because a ref pin is never checked against a lockfile,
  so every machine-wide ref rendered everywhere. But the obvious correction —
  require `Outcome::Version` — hid real dependencies for the same reason in
  reverse. `Outcome` says how a version was obtained, not whether the workspace
  depends on the library; declaration evidence is now its own field (§4).
- **The batching refactor is not mechanical**, and its equivalence test was
  circular: once `select` is defined as the wrapper, comparing the two proves
  only that the wrapper forwards. Goldens against the current implementation
  moved to task 1, before anything can change.
- **The width premise was wrong in the other direction.** There *is* a budget,
  imposed by `ui::table`'s dynamic arrangement, not by a human at a terminal.
  It constrains cell width rather than row count, so nothing deleted in the
  third revision comes back — but it does mean rendered text is
  environment-dependent, which is why the watermark hashes data instead.

## Open question, now answerable in the field

**Does ambient delivery earn its tokens once `docm list --project` exists?**
With the flag, the `devkit:docs` skill gets the same filtered table when it
loads, so the content argument for the brief section is gone; what is left is
timing. The section pays for itself in exactly one scenario: an agent answers a
library question from training recall without ever loading the docs skill,
because the question read as "fix our code" rather than as a library question.
The skill description is written to catch that ("Trigger even when you already
know the answer"), which is an admission the failure is real — but whether it
still leaks is an observation about live sessions, not something the code can
settle.

This no longer blocks anything. `[brief] pins` (§8) makes the answer a
per-project setting rather than a build-or-don't decision, so the section ships
on by default and anyone it does not serve turns it off in one line. The
evidence to watch for is the same — a session where an agent answered from
memory in a project whose docm checkout held the answer — but a negative result
now costs a config edit rather than a release.
