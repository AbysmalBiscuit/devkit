# Configurable Per-App Prep Files Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the hardcoded `prep_env` → `.env.local` dotenv write in `issue setup` with a configurable, multi-file `prep_files` step where each file declares its own path, verbatim content, and overwrite policy.

**Architecture:** Add a typed `PrepFile { path, content, overwrite }` to `devkit-ports::config`, carry a `Vec<PrepFile>` through the `App` catalog, and have `issue setup` write each file (parent dirs created, write-if-absent unless `overwrite`) before running the app's `setup` commands. The change lands additively (new field alongside the old), then the old `prep_env` field is removed once nothing reads it.

**Tech Stack:** Rust (edition 2024), `serde`/`toml` for config, `anyhow` for errors. No new dependencies.

---

## File Structure

- `crates/devkit-ports/src/config.rs` — `PrepFile` struct + `AppConfig.prep_files`; `prep_env` removed in Task 3.
- `crates/devkit-ports/src/apps.rs` — `App.prep_files`, copied in `catalog()`; `prep_env` removed in Task 3.
- `src/bin/issue/setup.rs` — `write_prep_files` helper + the per-app write loop.
- `crates/devkit-ports/src/run.rs`, `src/bin/devrun/config.rs` — `App`-literal test helpers updated to the new field set.
- `docs/configuration.md`, `README.md`, `AGENTS.md`, `docs/next-features.md` — user docs + feature status.

Each task below produces a workspace that compiles and passes `cargo test --workspace`.

---

## Task 1: Add `PrepFile` type and `prep_files` field (additive)

**Files:**
- Modify: `crates/devkit-ports/src/config.rs:101-127` (add struct + field, keep `prep_env`)
- Modify: `crates/devkit-ports/src/apps.rs:6-16` (App struct), `:43-54` (catalog copy)
- Modify: `crates/devkit-ports/src/run.rs:472` (test helper)
- Modify: `src/bin/devrun/config.rs:186`, `:218` (test helpers)
- Test: `crates/devkit-ports/src/config.rs` test module

- [ ] **Step 1: Write the failing config-parse test**

Add to the `#[cfg(test)] mod tests` block in `crates/devkit-ports/src/config.rs`:

```rust
#[test]
fn parses_prep_files_with_overwrite_default() {
    let toml = r#"
[defaults]
worktree_root = "~/wt"
branch_prefix = "x/"
baseline_ref = "origin/main"
apps_dir = "apps"

[[apps.api.prep_files]]
path = ".env.local"
content = "A=1\n"

[[apps.api.prep_files]]
path = "config/local.json"
content = "{}\n"
overwrite = true
"#;
    let c = Config::parse(toml).unwrap();
    let pf = &c.apps["api"].prep_files;
    assert_eq!(pf.len(), 2);
    assert_eq!(pf[0].path, ".env.local");
    assert_eq!(pf[0].content, "A=1\n");
    assert!(!pf[0].overwrite); // default false
    assert!(pf[1].overwrite);
}
```

If the surrounding `[defaults]` keys differ from the real `Defaults` struct, copy the field list from an existing passing test in the same file rather than guessing — the point of this test is the `prep_files` parsing, so reuse a known-good `[defaults]` block.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports parses_prep_files_with_overwrite_default`
Expected: FAIL — compile error, `no field prep_files on AppConfig`.

- [ ] **Step 3: Add the `PrepFile` struct**

In `crates/devkit-ports/src/config.rs`, immediately before `pub struct AppConfig` (line 101), add:

```rust
/// A file written into an app's directory during `issue setup`, before the app's
/// `setup` commands run. `content` is written verbatim — no format assembly or
/// newline injection. Parent directories are created. Existing files are left
/// untouched unless `overwrite` is set.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrepFile {
    /// Target path, relative to the app's directory.
    pub path: String,
    /// File contents, written byte-for-byte.
    pub content: String,
    /// Overwrite an existing file rather than skipping it.
    #[serde(default)]
    pub overwrite: bool,
}
```

- [ ] **Step 4: Add the `prep_files` field to `AppConfig`**

In `crates/devkit-ports/src/config.rs`, inside `struct AppConfig`, after the `prep_env` field (line 126), add:

```rust
    /// Files written into the app's directory during `issue setup` (before `setup`).
    #[serde(default)]
    pub prep_files: Vec<PrepFile>,
```

- [ ] **Step 5: Carry `prep_files` into the `App` catalog**

In `crates/devkit-ports/src/apps.rs`, add to `struct App` (after `prep_env`, line 14):

```rust
    pub prep_files: Vec<crate::config::PrepFile>,
```

And in `catalog()`'s `App { ... }` literal (after `prep_env: a.prep_env.clone(),`, line 51):

```rust
                prep_files: a.prep_files.clone(),
```

- [ ] **Step 6: Update the `App`-literal test helpers**

These build an `App` by hand and must list the new field or they won't compile.

In `crates/devkit-ports/src/run.rs:472`, after the `prep_env: HashMap::new(),` line, add:

```rust
            prep_files: vec![],
```

In `src/bin/devrun/config.rs`, after **each** `prep_env: HashMap::new(),` line (`:186` and `:218`), add:

```rust
                prep_files: vec![],
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p devkit-ports parses_prep_files_with_overwrite_default`
Expected: PASS.

- [ ] **Step 8: Verify the whole workspace still builds and tests pass**

Run: `cargo test --workspace`
Expected: PASS (all existing tests plus the new one).

- [ ] **Step 9: Commit**

```bash
git add crates/devkit-ports/src/config.rs crates/devkit-ports/src/apps.rs crates/devkit-ports/src/run.rs src/bin/devrun/config.rs
git commit -m "feat(config): add prep_files app field alongside prep_env"
```

---

## Task 2: Write prep files in `issue setup`

**Files:**
- Modify: `src/bin/issue/setup.rs:92-110` (replace the `prep_env` write block with a `write_prep_files` call)
- Test: `src/bin/issue/setup.rs` test module

- [ ] **Step 1: Write the failing helper tests**

In `src/bin/issue/setup.rs`, add these imports near the top of the `#[cfg(test)] mod tests` block (it currently only has `use super::*;`):

```rust
    use devkit_ports::config::PrepFile;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        // Unique per process + tag; no tempfile dependency.
        let dir = std::env::temp_dir().join(format!("devkit-prep-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
```

Then add the tests:

```rust
    #[test]
    fn writes_content_verbatim_and_creates_parents() {
        let dir = scratch("verbatim");
        let files = vec![PrepFile {
            path: "config/local.json".into(),
            content: "{\"mode\":\"local\"}\n".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files).unwrap();
        let got = std::fs::read_to_string(dir.join("config/local.json")).unwrap();
        assert_eq!(got, "{\"mode\":\"local\"}\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_if_absent_preserves_existing() {
        let dir = scratch("absent");
        std::fs::write(dir.join(".env.local"), "ORIGINAL\n").unwrap();
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "REPLACED\n".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".env.local")).unwrap(), "ORIGINAL\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overwrite_replaces_existing() {
        let dir = scratch("overwrite");
        std::fs::write(dir.join(".env.local"), "ORIGINAL\n").unwrap();
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "REPLACED\n".into(),
            overwrite: true,
        }];
        write_prep_files(&dir, &files).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(".env.local")).unwrap(), "REPLACED\n");
        std::fs::remove_dir_all(&dir).ok();
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit writes_content_verbatim_and_creates_parents write_if_absent_preserves_existing overwrite_replaces_existing`
Expected: FAIL — compile error, `cannot find function write_prep_files`.

- [ ] **Step 3: Add the `write_prep_files` helper**

In `src/bin/issue/setup.rs`, add this function (above `pub fn run`, after the `branch_name` helper). Add `use devkit_ports::config::PrepFile;` and `use std::path::Path;` is already imported:

```rust
/// Write each prep file into `app_dir`. Content is written verbatim; parent
/// directories are created; an existing file is left untouched unless the entry
/// opts into `overwrite`.
fn write_prep_files(app_dir: &Path, files: &[PrepFile]) -> Result<()> {
    for pf in files {
        let target = app_dir.join(&pf.path);
        if pf.overwrite || !target.exists() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            std::fs::write(&target, &pf.content)
                .with_context(|| format!("writing prep file `{}`", pf.path))?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Replace the per-app write block in `run`**

In `src/bin/issue/setup.rs`, replace the existing `prep_env` block (lines 100-110, the `if !app.prep_env.is_empty() { ... }` block) with:

```rust
        write_prep_files(&app_dir, &app.prep_files)
            .with_context(|| format!("preparing files for app `{a}`"))?;
```

Leave the `std::fs::create_dir_all(&app_dir).ok();` line above it and the `for cmd in &app.setup` loop below it unchanged — prep files are still written before `setup` commands run.

- [ ] **Step 5: Update the block comment**

Replace the comment above the loop (currently "Per-app bootstrap: write prep_env to `<app>/.env.local` ...", lines 92-94) with:

```rust
    // Per-app bootstrap: write the app's configured prep files, then run its
    // setup commands in its directory. Everything project-specific — filenames,
    // file contents, installs, doppler wiring — lives in config, not here.
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit writes_content_verbatim_and_creates_parents write_if_absent_preserves_existing overwrite_replaces_existing`
Expected: PASS.

- [ ] **Step 7: Verify the whole workspace builds and passes**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/bin/issue/setup.rs
git commit -m "feat(issue): write configurable prep_files during setup"
```

---

## Task 3: Remove the obsolete `prep_env` field

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (`AppConfig.prep_env` + its doc comment)
- Modify: `crates/devkit-ports/src/apps.rs` (`App.prep_env` + catalog copy)
- Modify: `crates/devkit-ports/src/run.rs:472`, `src/bin/devrun/config.rs:186`/`:218` (test helpers)

- [ ] **Step 1: Remove the field from `AppConfig`**

In `crates/devkit-ports/src/config.rs`, delete the `prep_env` doc comment and field (the `/// Env written to <app>/.env.local ...` comment plus `#[serde(default)] pub prep_env: HashMap<String, String>,`). Leave `static_env` and the new `prep_files` field in place.

- [ ] **Step 2: Remove the field from `App` and the catalog copy**

In `crates/devkit-ports/src/apps.rs`, delete `pub prep_env: HashMap<String, String>,` from `struct App` and `prep_env: a.prep_env.clone(),` from the `catalog()` literal.

- [ ] **Step 3: Remove `prep_env` from the test helpers**

Delete the `prep_env: HashMap::new(),` line in `crates/devkit-ports/src/run.rs:472` and both occurrences in `src/bin/devrun/config.rs` (`:186`, `:218`). If `HashMap` becomes unused in any of these files, remove its `use` import too — let the compiler/clippy tell you.

- [ ] **Step 4: Verify the workspace builds with zero warnings**

Run: `cargo test --workspace`
Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS — no `unused import` or dead-code warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs crates/devkit-ports/src/apps.rs crates/devkit-ports/src/run.rs src/bin/devrun/config.rs
git commit -m "refactor(config): drop prep_env, superseded by prep_files"
```

---

## Task 4: Update docs, feature status, and personal config

**Files:**
- Modify: `docs/configuration.md:49` (table row), `:205-214` (`[apps.web]` example)
- Modify: `README.md:134`
- Modify: `AGENTS.md:101`
- Modify: `docs/next-features.md` (mark the feature RESOLVED)
- Modify (outside repo, not committed): `~/.config/devkit/config.toml`

- [ ] **Step 1: Update the `docs/configuration.md` table row**

Replace the `prep_env` row (line 49) with:

```markdown
| `prep_files` | no | Files written into the app's directory during `issue setup`, before `setup` commands run. Each entry is `{ path, content, overwrite }` — `path` is relative to the app dir (parent dirs created), `content` is written verbatim, and `overwrite` (default `false`) keeps an existing file unless set to `true`. As an array, a deeper `devkit.toml` replaces the whole list rather than appending. |
```

- [ ] **Step 2: Update the `docs/configuration.md` example**

In the `[apps.web]` example (around line 210), replace the `prep_env   = { SOME_FEATURE_FLAG = "dummy" }` line with:

```toml

[[apps.web.prep_files]]
path    = ".env.local"
content = """
SOME_FEATURE_FLAG=dummy
"""
```

- [ ] **Step 3: Update `README.md`**

Replace line 134 (`- \`prep_env = { KEY = "value" }\` is written to \`<app>/.env.local\` during \`issue setup\`.`) with:

```markdown
- `prep_files` declares files written into an app's directory during `issue setup` (before its `setup` commands). Each is `{ path, content, overwrite }`; `content` is written verbatim, and existing files are kept unless `overwrite = true`.
```

- [ ] **Step 4: Update `AGENTS.md`**

On line 101, change `per-app prep files come from \`prep_env\`` to `per-app prep files come from \`prep_files\``.

- [ ] **Step 5: Mark the feature RESOLVED in `docs/next-features.md`**

Change the `## Configurable per-app prep step (generalize \`prep_env\` writing)` section's `**Status:** OPEN — wants its own brainstorm/spec.` line to:

```markdown
**Status:** RESOLVED 2026-06-24 — see
`docs/superpowers/specs/2026-06-24-configurable-per-app-prep-files-design.md`.
Per-app prep is now a configurable `prep_files` list (per file: `path`, verbatim
`content`, `overwrite`); the hardcoded `.env.local` filename, dotenv format, and
write-if-absent-only strategy are gone. Format/template generation and symlink mode
stay deferred to the messages-templates feature.
```

Leave the rest of that section's analysis text as historical context.

- [ ] **Step 6: Commit the docs**

```bash
git add docs/configuration.md README.md AGENTS.md docs/next-features.md
git commit -m "docs: document prep_files, retire prep_env"
```

- [ ] **Step 7: Migrate the personal config (outside the repo, no commit)**

Read `~/.config/devkit/config.toml`. For each app that has a `prep_env = { K = "V", ... }` inline table, replace it with a `prep_files` array-of-tables entry that writes the same dotenv content to `.env.local`. Example transform:

```toml
# before
prep_env = { SOME_FEATURE_FLAG = "dummy", WORKFLOW_ID = "00000000" }

# after — as a sibling [[apps.<name>.prep_files]] block (remove the prep_env line)
[[apps.<name>.prep_files]]
path    = ".env.local"
content = """
SOME_FEATURE_FLAG=dummy
WORKFLOW_ID=00000000
"""
```

Preserve each app's key/value pairs and their order. After editing, verify it parses:

Run: `cargo run -q --bin devrun -- config show >/dev/null`
Expected: exits 0 with no parse error (prints nothing to stderr).

- [ ] **Step 8: Final gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all PASS.

---

## Self-Review Notes

- **Spec coverage:** schema change (Task 1), verbatim write + parent dirs + overwrite-vs-write-if-absent + before-`setup` ordering (Task 2), clean `prep_env` removal (Task 3), layering note + docs + personal config migration (Task 4). All spec sections map to a task.
- **Type consistency:** `PrepFile { path, content, overwrite }` and `write_prep_files(&Path, &[PrepFile])` are used identically across Tasks 1–4.
- **No new dependencies:** filesystem tests use `std::env::temp_dir()`, not `tempfile`.
