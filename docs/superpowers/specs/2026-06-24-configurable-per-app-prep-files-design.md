# Configurable per-app prep files

**Status:** Design approved 2026-06-24.

## Problem

`issue setup` bootstraps each requested app by writing a prep file before running
the app's configured `setup` commands. Today that write is hardcoded policy:

- the filename is always `.env.local`,
- the format is always dotenv (`key=value\n` lines),
- the strategy is always write-if-absent.

The content is config-driven (the `prep_env` map per app), but the three mechanics
above are baked into the binary (`src/bin/issue/setup.rs:100-110`). devkit is meant
to be project-agnostic — a project that needs a different filename/location, a
non-dotenv format, or a regenerate-on-setup file cannot express that. This removes
the hardcoded policy and moves it into `devkit.toml`.

## Scope

In scope: make the prep-file **path**, **content**, and **write strategy**
configurable per app, supporting **multiple** files per app.

Explicitly out of scope (deferred to the separate "Configurable templates for
messages" feature): format selectors (JSON/YAML/export-line generation), symlink
mode, and template interpolation inside `content`. `content` is a literal string
here; the templates feature later makes it dynamic. Keeping `content` a plain
string is the clean seam for that handoff.

`static_env` (which feeds the launch environment, not file writing) is unrelated
and untouched.

## Design

### Config schema

Remove `prep_env: HashMap<String, String>` from `AppConfig`
(`crates/devkit-ports/src/config.rs`). Add a typed `PrepFile` struct and a
`prep_files` list:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrepFile {
    /// Target path, relative to the app's directory. Parent dirs are created.
    pub path: String,
    /// Written verbatim — no formatting or trailing-newline injection.
    pub content: String,
    /// Overwrite an existing file; default false keeps write-if-absent.
    #[serde(default)]
    pub overwrite: bool,
}
```

`AppConfig` gains `#[serde(default)] pub prep_files: Vec<PrepFile>`, placed last in
the struct. As an array-of-tables it serializes after the scalar/array and
`static_env` fields, so `devrun config show` output stays cleanly grouped.

`App` (`crates/devkit-ports/src/apps.rs`) carries the same `Vec<PrepFile>`, copied
through `catalog()` alongside the other fields.

TOML form:

```toml
[[apps.api.prep_files]]
path    = ".env.local"
content = """
SOME_FEATURE_FLAG=dummy
WORKFLOW_ID=00000000
"""

[[apps.api.prep_files]]
path      = "config/local.json"
content   = "{ \"mode\": \"local\" }\n"
overwrite = true
```

### Layered-config interaction

The layered-config resolver merges tables key by key but replaces arrays wholesale.
`prep_files` is an array, so a deeper `devkit.toml` **replaces** the entire
`prep_files` list for an app rather than appending to a shallower one. This matches
the established array-merge rule and is the predictable behavior; it is documented
so a user does not expect per-file accumulation across layers. (`static_env`, a
table, still merges key by key — the two behave differently by design.)

### Write step

Replace the hardcoded block in `src/bin/issue/setup.rs` (currently lines 100-110)
with a loop over `app.prep_files`, kept **before** the `setup` command loop so an
install command can rely on the files already existing:

```rust
for pf in &app.prep_files {
    let target = app_dir.join(&pf.path);
    if pf.overwrite || !target.exists() {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&target, &pf.content)
            .with_context(|| format!("writing prep file `{}` for app `{a}`", pf.path))?;
    }
}
```

Deliberate behaviors:

- **`content` is written verbatim.** No `key=value` assembly, no newline injection.
  The TOML string is the file byte-for-byte; the author includes a trailing newline
  in the heredoc if they want one.
- **Parent directories are created**, so a nested `path` such as
  `config/local.json` works, not only a top-level dotfile.
- **Write-if-absent by default; `overwrite = true` regenerates.** A file the user
  hand-edited locally is preserved unless that file opts into overwrite.

## Testing

- Config round-trip: a `[[apps.x.prep_files]]` block parses into `Vec<PrepFile>`
  with `overwrite` defaulting to false.
- Write step: write-if-absent skips an existing file; `overwrite = true` replaces
  its contents; a nested `path` creates the parent directory; `content` lands
  verbatim.
- The existing `cargo test --workspace` gate and `clippy -D warnings` stay green.

## Touchpoints

- `crates/devkit-ports/src/config.rs` — remove `prep_env`, add `PrepFile` +
  `prep_files`.
- `crates/devkit-ports/src/apps.rs` — `App.prep_files`, copied in `catalog()`.
- `src/bin/issue/setup.rs` — replace the write block.
- Test helpers building an `App` literal — `src/bin/devrun/config.rs`,
  `crates/devkit-ports/src/run.rs`, and the `apps.rs` test module
  (`prep_env: HashMap::new()` → `prep_files: vec![]`).
- Docs: `docs/configuration.md` (table row + example), `README.md`, the `AGENTS.md`
  prep-files line, and `docs/next-features.md` (mark RESOLVED, point here).
- Personal config: migrate `prep_env` blocks in `~/.config/devkit/config.toml` to
  `prep_files`.

Historical plan/spec docs that mention `prep_env` are dated records and stay as-is.
