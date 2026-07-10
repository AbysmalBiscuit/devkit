# `docm`: managed library checkouts + one `/docs` skill

## Problem

Looking up how to use a library means having its docs and source on disk, at
the version the project actually depends on. Today that is manual: clone the
repo somewhere, hand-write a `LIBRARY-docs` skill pointing at it, repeat per
library, and let both rot as versions bump. Hosted doc services (Context7 and
friends) serve latest-ish docs, can't be searched with `rg`/`ast-grep`, and
don't cover private repos. Nothing existing manages the whole loop:
register a library, keep version-correct checkouts, and give the agent one
reliable way to find them.

## Design

### Shape

A new lib crate `crates/devkit-docs` plus a thin binary `src/bin/docm.rs`
(clap + `completions <shell>`), following the `devkit-ports`/`portm` split:
logic and stores in the lib, CLI parsing and rendering in the binary. Git
operations go through `devkit-common::cmd`; package-registry HTTP lookups
live behind a trait so tests stub them.

### Manifest

Two sources, merged by lib `name` (project overrides/extends global):

- global: `~/.config/devkit/docs.toml` — docm-owned, machine-written, uses
  top-level `[[libs]]` entries.
- per-project: a `[docs]` section (`[[docs.libs]]` entries, same fields) in
  the project's `devkit.toml`, found by the same walk-up-from-CWD convention
  as existing config discovery. Committed by definition, so teammates share
  the lib set. (`.devkit/` is unsuitable: everything written there is
  self-gitignored and structurally deleted with the worktree.)

`docm add --project` edits the nearest `devkit.toml` via `toml_edit` so
hand-written comments and formatting survive. A project entry may be
partial — every field except `name` is optional and overrides the global
entry field-by-field (e.g. pin only `ref` while inheriting `repo`).

```toml
[[libs]]
name = "tokio"                # skill-facing name, unique
ecosystem = "rust"            # rust | js | python | git
package = "tokio"             # registry package name (differs for @scope/pkg)
repo = "https://github.com/tokio-rs/tokio"
ref = "compat-v0.2"           # optional manual pin; the only mechanism for git
src_dir = "src"               # optional layout overrides
docs_dir = "docs"
notes = "class reference is XML under doc/classes"  # surfaced by `docm info`
```

`repo` is resolved from the package registry (crates.io API, npm registry,
PyPI JSON) once at `docm add` time and stored; resolution never happens at
lookup time, so `path`/`info` work offline. `docm add <git-url>` is the
escape hatch for private repos and non-package projects (e.g. Godot). A
docs-in-a-separate-repo project (godot + godot-docs) registers two entries;
a lib entry holds exactly one clone.

### Cache

```
~/.cache/devkit/docs/
  registry.json        # flock-guarded project→version reference registry
  <name>/
    repo.git/          # one bare, blobless clone (--filter=blob:none), with tags
    1.38.0/            # git worktree per referenced version, created on demand
    default/           # worktree tracking the default branch (or the manual ref)
    meta.toml          # detected layout per worktree, cached tag pattern
```

Version worktrees are materialized on demand: `docm path`/`info` creates the
worktree it resolved if missing (first hit on a version fetches that tag's
blobs; later hits are pure disk). `docm sync` fetches the bare clones and
fast-forwards `default` worktrees. Layout detection runs once per new
worktree — `docs/`, `doc/`, `book.toml` (mdBook), `conf.py` (Sphinx),
Docusaurus config, `examples/`, `src/` — and manifest `src_dir`/`docs_dir`
overrides beat detection.

### Reference registry and prune

`registry.json` at the cache root records, on every successful resolution,
`{ project, lib, version, resolved_at }` where `project` is the resolving
project's root path. All read-modify-writes hold an exclusive flock, same
pattern as the ports/locks stores — concurrent agent sessions race docm.

`docm prune` reclaims from references, not age (the ports-registry holder
model: a holder is live iff its root path exists):

1. Project root gone → drop all its rows.
2. Root exists → re-read its lockfile; version bumped → retarget the row
   (without materializing the new worktree) and drop the old reference.
3. Version worktree with zero rows → deleted. `default` worktrees are exempt
   (they exist because the manifest entry exists).
4. A lib absent from every manifest with zero rows → the whole `<name>/`
   directory is listed and removed only with `--yes` or interactive confirm.

### Version resolution (`docm path` / `docm info`)

1. Walk up from CWD to the project root (same discovery as config).
2. A manual `ref` pin wins outright.
3. Otherwise find the lib's `package` in the project's lockfiles — v1
   parsers: `Cargo.lock`, `pnpm-lock.yaml`, `package-lock.json`, `uv.lock`.
   Multiple versions in one lockfile (transitive dupes) → highest, with a
   stderr note.
4. Version → tag: probe the bare clone's tags against the common shapes
   (`v1.38.0`, `1.38.0`, `tokio-1.38.0`, `pkg@1.38.0`); the first matching
   pattern is cached in `meta.toml` so later resolutions skip probing.
5. No lockfile / package absent / tag not found → fall back to the `default`
   worktree with a one-line stderr warning.

stdout is machine-clean: `docm path` prints exactly one path; all warnings
go to stderr.

### CLI surface

```
docm add <name|git-url> [--eco …] [--repo URL] [--ref REF]
         [--src-dir P] [--docs-dir P] [--notes …] [--project]
docm rm <name> [--project]
docm list [--json]          # name, ecosystem, pin, synced versions
docm sync [name…]
docm path <name>
docm info <name> [--json]   # path + resolved version + layout map + notes
docm prune [--yes]
docm completions <shell>
```

`--project` targets the nearest `devkit.toml`'s `[docs]` section instead of
the global manifest.
`devkit doctor` gains a `docs` row: cache size and unreferenced worktrees.

### The `devkit:docs` skill

One static skill at `skills/docs/` in this repo, distributed by the existing
plugin exactly like `using-devkit` (the codex/cursor plugin variants pick it
up too). The binary never writes into `~/.claude/skills/` — skill text and
CLI contract version together in this repo. No per-library skills exist, and
no skill ever contains a concrete checkout path.

Skill contract: the first token of the argument is the library name (or the
agent infers it; `docm list` supplies the vocabulary). Run `docm info <lib>`
— it prints the checkout path, resolved version, layout map, and notes.
Search those directories with `rg`/`ast-grep`: the docs dir for guides, the
source for API ground truth. Cite `file:line`. Always re-run `docm info`
rather than reusing a remembered path; the first resolution of a new version
fetches blobs and may take seconds. The skill description triggers on
"how do I use library X" questions, not only explicit `/docs` invocations.

## Out of scope

- Baked per-project skills (`sync --project`-style export). A skill with
  hardcoded paths is useless without the cache docm creates, so the target
  audience for it does not exist; indirection through `docm info` is the
  design, not one mode of it.
- Multiple clones per lib entry (docs-repo + source-repo pairs) — register
  two entries instead.
- Lockfile ecosystems beyond the four v1 parsers (`yarn.lock`, `poetry.lock`,
  Go, …) — each is an isolated follow-up behind the same parser seam.
- MCP exposure of docm — the skill drives the CLI directly; revisit only if
  a host without shell access needs it.
- Daemon (`devkitd`) integration — the flock'd registry is sufficient; no
  in-memory authority is needed for a cache.

## Tests (TDD)

- Unit, in `devkit-docs`: manifest merge (project-over-global by name); each
  lockfile parser against fixture files; tag-pattern probing against fixture
  tag lists; layout detection against temp dir trees; prune against a
  synthetic registry with fake project roots (existing, deleted, and bumped).
- Integration: fixture git repos created in temp dirs (init, commit, tag)
  driving add → path → prune end-to-end with no network; `add`'s registry
  lookups stubbed via the HTTP trait.
- Concurrency: one multiprocess flock test on `registry.json`, same shape as
  `devkit-ports --test registry`.
- CI portability: no fixed sleeps; any process-spawning test polls (existing
  convention).

## Resolved decisions

1. One global `/docs` (`devkit:docs`) skill, not per-library skills; lib name
   is the first token or inferred.
2. Skills never contain paths; every lookup goes through `docm info`.
3. Skill ships in the plugin, not installed by the binary.
4. Registration is registry-resolved (`docm add tokio`); git URL is the
   escape hatch.
5. v1 ecosystems: Rust, JS/TS, Python lockfiles + manual-ref git entries.
6. Reclamation is reference-based via `registry.json` (holder = project root
   path), not mtime-based.
7. The project overlay lives in `devkit.toml`'s `[docs]` section, not
   `.devkit/docs.toml` — `.devkit/` is structurally gitignored (self-ignoring
   `.gitignore`, global git excludes), so nothing there can be committed.
