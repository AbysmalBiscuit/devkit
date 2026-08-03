# docm Correctness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every answer `docm` gives either provably correct or a hard error — no silent fallback, no state where the manifest and the disk disagree.

**Architecture:** Checkout directories are named for the ref that produced them (`/` → `~`), library directories for the encoded library name; `meta.toml` records origin, canonical ref and commit so every resolution can prove what it returned; versions resolve through each lockfile's importer graph rather than by matching semver ranges; and a per-library advisory lock serializes clone/fetch/materialize/registry-commit against prune.

**Tech Stack:** Rust 2024, `anyhow`, `serde`/`toml`/`serde_json`/`serde_yaml_ng`, `clap` + `clap_complete`, `fs2` advisory locks (already used by `devkit-locks`/`devkit-ports`), `devkit_common::cmd` for git.

**Spec:** `docs/superpowers/specs/2026-08-03-docm-correctness-design.md` — read it before starting. Section references below (§1, §3…) point at it.

## Global Constraints

- **TDD is mandatory.** Every task writes a failing test first and runs it to watch it fail for the right reason before implementing.
- **`cargo test --workspace` is the merge gate.** It must be green at every commit.
- **`cargo clippy --workspace --all-targets -- -D warnings`** — zero warnings, run before every commit.
- **`cargo fmt --all`** before every commit.
- **CI runs ubuntu, macos and windows.** Tests that spawn or reap processes poll for state; they never sleep a fixed interval. Path assertions must not assume `/`.
- **`anyhow` everywhere**, with `.context()` on every fallible IO or git call.
- **Comments follow `AGENTS.md`:** no narration, no PR/issue references, no change-relative phrasing. A comment explains a non-obvious *why* or it does not exist.
- **Conventional Commits**, one logical change per commit, imperative subject ≤50 chars.
- **No new third-party dependency.** The spec explicitly dropped `node-semver`; importer-graph resolution needs no range matching.
- **Reserved names (§1, §1.1):** checkout level `repo.git`, `meta.toml`; library level the stem `registry` — that exact name or any name beginning with `registry.`.
- **Never unlink an advisory lock file after release** (implementer note): persistent lock files avoid inode-replacement races.

## File Structure

| File | Responsibility |
|---|---|
| `crates/devkit-docs/src/names.rs` | **new** — encode/decode library and ref directory names, reserved-name and host-representability validation. Pure, no IO. |
| `crates/devkit-docs/src/locks.rs` | **new** — per-library advisory lock; `with_lib_lock(cache_root, lib, f)`. |
| `crates/devkit-docs/src/upgrade.rs` | **new** — one-shot 0.12.x cache migration: nested scoped dirs, `git worktree repair`, `origin` bootstrap. |
| `crates/devkit-docs/src/importers.rs` | **new** — per-lockfile importer-graph resolution: workspace path → installed version. |
| `crates/devkit-docs/src/cache.rs` | `LibCache` gains origin recording, worktree cleanliness/HEAD verification, ref-named worktrees. |
| `crates/devkit-docs/src/resolve.rs` | Orchestration only: select ref → resolve commit → materialize → verify → record. |
| `crates/devkit-docs/src/tags.rs` | Tag pattern set and probe order. |
| `crates/devkit-docs/src/lockfiles.rs` | Lockfile *parsing* primitives; selection moves to `importers.rs`. |
| `crates/devkit-docs/src/refs.rs` | Reference rows keyed by workspace; legacy-row retirement; prune under the library lock. |
| `crates/devkit-docs/src/manifest.rs` | Atomic writes, load-time name validation. |
| `crates/devkit-docs/src/lookup.rs` | Ecosystem probing: CWD-biased order, collision refusal. |
| `src/bin/docm.rs` | CLI surface: `add` materialization + rollback, `sync` rewrite, `info`/`list` output, `rm` aliases, `--allow-default-branch`. |
| `crates/devkit-docs/tests/names.rs` | **new** — encoding, reserved names, representability. |
| `crates/devkit-docs/tests/importers.rs` | **new** — one fixture per lockfile format. |
| `crates/devkit-docs/tests/upgrade.rs` | **new** — migration, including worktree repair. |
| `crates/devkit-docs/tests/concurrency.rs` | **new** — add/rm/prune races. |

Existing `tests/{cache,prune,refs_race,resolve}.rs` are extended in place.

---

### Task 1: Name encoding and validation

Fixes the live 0.12.1 data-loss bug (§1.1) and is independent of everything else — land it first.

**Files:**
- Create: `crates/devkit-docs/src/names.rs`
- Create: `crates/devkit-docs/tests/names.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod names;`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `names::encode(name: &str) -> String` — `/` → `~`, no validation
  - `names::decode(dir: &str) -> String` — `~` → `/`
  - `names::validate_lib(name: &str) -> anyhow::Result<()>`
  - `names::validate_checkout(dirname: &str) -> anyhow::Result<()>`
  - `names::lib_dir(name: &str) -> anyhow::Result<String>` — validate then encode
  - `names::checkout_dir(git_ref: &str) -> anyhow::Result<String>` — validate then encode

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-docs/tests/names.rs`:

```rust
use devkit_docs::names;

#[test]
fn encodes_slashes_and_round_trips() {
    assert_eq!(names::encode("@hey-api/client-fetch"), "@hey-api~client-fetch");
    assert_eq!(names::decode("@hey-api~client-fetch"), "@hey-api/client-fetch");
    assert_eq!(names::encode("release/2.x"), "release~2.x");
}

#[test]
fn lib_names_reject_tilde_so_encoding_is_injective() {
    // `a/b` and `a~b` would otherwise share one directory.
    assert!(names::validate_lib("a~b").is_err());
    assert!(names::validate_lib("a/b").is_ok());
    assert_eq!(names::lib_dir("a/b").unwrap(), "a~b");
}

#[test]
fn lib_names_reject_the_registry_stem() {
    for n in [
        "registry",
        "registry.json",
        "registry.lock",
        "registry.json.tmp",
        "registry.json.bak",
        "registry.anything-added-later",
    ] {
        assert!(names::validate_lib(n).is_err(), "{n} must be reserved");
    }
    assert!(names::validate_lib("registryfoo").is_ok());
}

#[test]
fn checkout_names_reject_control_files_but_not_the_registry_stem() {
    assert!(names::validate_checkout("repo.git").is_err());
    assert!(names::validate_checkout("meta.toml").is_err());
    // The registry lives at the cache root, one level up from checkouts.
    assert!(names::validate_checkout("registry.json").is_ok());
}

#[test]
fn rejects_names_the_host_filesystem_cannot_represent() {
    assert!(names::validate_checkout(&"v".repeat(256)).is_err());
    if cfg!(windows) {
        for n in ["a|b", "a<b", "a>b", "a\"b", "NUL", "con", "COM1", "LPT9.txt"] {
            assert!(names::validate_checkout(n).is_err(), "{n} must be rejected");
        }
    }
}

#[test]
fn tilde_is_illegal_in_a_git_ref_so_checkout_encoding_is_injective() {
    // `release/2.x` -> `release~2.x`; no valid ref can produce a `~` itself.
    assert!(names::validate_checkout("release~2.x").is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-docs --test names`
Expected: FAIL to compile — `unresolved import devkit_docs::names`.

- [ ] **Step 3: Implement `names.rs`**

```rust
//! Cache path components. Two levels, two reserved sets: library directories
//! sit at the cache root beside the reference registry's own files, checkout
//! directories sit inside a library beside its bare clone and metadata.

use anyhow::{Result, bail};

const CHECKOUT_RESERVED: [&str; 2] = ["repo.git", "meta.toml"];
const MAX_COMPONENT_BYTES: usize = 255;

pub fn encode(name: &str) -> String {
    name.replace('/', "~")
}

pub fn decode(dir: &str) -> String {
    dir.replace('~', "/")
}

/// `/` → `~` is injective for refs because git forbids `~` in a ref name.
/// Library names carry no such constraint, so the constraint is imposed here.
pub fn validate_lib(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("library name is empty");
    }
    if name.contains('~') {
        bail!(
            "library name `{name}` contains `~`, which docm uses to encode `/` in cache paths; \
             pass --package to register it under a different name"
        );
    }
    if name == "registry" || name.starts_with("registry.") {
        bail!(
            "library name `{name}` is reserved: the docs cache keeps its reference registry \
             at <cache>/registry.* and a library directory there would shadow it"
        );
    }
    representable(&encode(name))
}

pub fn validate_checkout(dirname: &str) -> Result<()> {
    if dirname.is_empty() {
        bail!("ref is empty");
    }
    if dirname.contains('~') {
        bail!("`{dirname}` contains `~`, which is illegal in a git ref name");
    }
    if CHECKOUT_RESERVED.contains(&dirname) {
        bail!("ref `{dirname}` collides with a control file inside the library directory");
    }
    representable(dirname)
}

pub fn lib_dir(name: &str) -> Result<String> {
    validate_lib(name)?;
    Ok(encode(name))
}

pub fn checkout_dir(git_ref: &str) -> Result<String> {
    let dir = encode(git_ref);
    validate_checkout(&dir)?;
    Ok(dir)
}

/// Git permits characters and names Windows forbids in a path component.
fn representable(component: &str) -> Result<()> {
    if component.len() > MAX_COMPONENT_BYTES {
        bail!(
            "`{component}` is {} bytes; a path component cannot exceed {MAX_COMPONENT_BYTES}",
            component.len()
        );
    }
    if cfg!(windows) {
        if let Some(c) = component.chars().find(|c| "<>:\"|?*\\".contains(*c)) {
            bail!("`{component}` contains `{c}`, which Windows does not allow in a path");
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem[3..].parse::<u8>().is_ok_and(|n| (1..=9).contains(&n));
        if reserved {
            bail!("`{component}` is a reserved device name on Windows");
        }
    }
    Ok(())
}
```

Add `pub mod names;` to `crates/devkit-docs/src/lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-docs --test names`
Expected: PASS, 6 tests.

- [ ] **Step 5: Route `LibCache` through the encoder**

Modify `crates/devkit-docs/src/cache.rs`. `LibCache::new` currently joins the raw name:

```rust
    pub fn new(cache_root: &Path, name: &str) -> Result<Self> {
        Ok(Self {
            dir: cache_root.join(crate::names::lib_dir(name)?),
        })
    }
```

This changes the signature to return `Result`. Fix every caller the compiler
reports (`resolve.rs`, `refs.rs`, `src/bin/docm.rs`) by propagating with `?`.

- [ ] **Step 6: Guard the prune scan**

In `crates/devkit-docs/src/cache.rs`, `version_worktrees` must refuse to
enumerate a directory that is not a library:

```rust
    pub fn version_worktrees(&self) -> Vec<(String, PathBuf)> {
        if !self.bare().is_dir() {
            return Vec::new();
        }
        // ... existing body unchanged
    }
```

- [ ] **Step 7: Write the prune regression test**

Append to `crates/devkit-docs/tests/prune.rs`:

```rust
#[test]
fn a_scoped_library_is_one_directory_and_prune_leaves_it_alone() {
    let root = common::unique_tmp("scoped");
    let lib = devkit_docs::cache::LibCache::new(&root, "@types/node").unwrap();
    assert!(lib.dir.ends_with("@types~node"));

    // A stray nested directory is not a library and must never be enumerated
    // as one — that is what deleted `client-fetch` in 0.12.1.
    std::fs::create_dir_all(root.join("@scope/pkg")).unwrap();
    let stray = devkit_docs::cache::LibCache::new(&root, "@scope").unwrap();
    assert!(stray.version_worktrees().is_empty());
}
```

- [ ] **Step 8: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/devkit-docs/src/names.rs crates/devkit-docs/src/lib.rs \
        crates/devkit-docs/src/cache.rs crates/devkit-docs/tests/names.rs \
        crates/devkit-docs/tests/prune.rs crates/devkit-docs/src/resolve.rs \
        crates/devkit-docs/src/refs.rs src/bin/docm.rs
git commit -m "fix(docs): encode library names into one cache directory"
```

---

### Task 2: Manifest name validation on load

Closes the legacy-collision hole (§1.1): a 0.12.x manifest may already hold `a~b`.

**Files:**
- Modify: `crates/devkit-docs/src/manifest.rs`
- Test: `crates/devkit-docs/tests/names.rs`

**Interfaces:**
- Consumes: `names::validate_lib`.
- Produces: `manifest::discover` returns `Err` when any loaded entry has an invalid name or two entries encode to one directory.

- [ ] **Step 1: Write the failing test**

Append to `crates/devkit-docs/tests/names.rs`:

```rust
#[test]
fn a_manifest_holding_both_a_slash_b_and_a_tilde_b_is_rejected_on_load() {
    let dir = std::env::temp_dir().join(format!("docm-names-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let global = dir.join("docs.toml");
    std::fs::write(
        &global,
        "[[libs]]\nname = \"a~b\"\n\n[[libs]]\nname = \"a/b\"\n",
    )
    .unwrap();

    let err = devkit_docs::manifest::discover(&dir, Some(&global)).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("a~b"), "error must name both entries: {msg}");
    assert!(msg.contains("a/b"), "error must name both entries: {msg}");
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-docs --test names -- a_manifest_holding`
Expected: FAIL — `unwrap_err` panics because discover currently succeeds.

- [ ] **Step 3: Implement validation in `discover`**

At the end of `manifest::discover`, before returning `Discovered`:

```rust
    let mut seen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for l in &manifest.libs {
        crate::names::validate_lib(&l.name)
            .with_context(|| format!("library `{}` in the docs manifest", l.name))?;
        let dir = crate::names::encode(&l.name);
        if let Some(other) = seen.insert(dir.clone(), l.name.clone())
            && other != l.name
        {
            bail!("`{other}` and `{}` both map to the cache directory `{dir}`", l.name);
        }
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-docs --test names -- a_manifest_holding`
Expected: PASS.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/manifest.rs crates/devkit-docs/tests/names.rs
git commit -m "fix(docs): validate library names when a manifest loads"
```

---

### Task 3: Tag patterns and probe order

§4. Must land before Task 9 makes a missing tag fatal.

**Files:**
- Modify: `crates/devkit-docs/src/tags.rs`
- Modify: `crates/devkit-docs/src/resolve.rs:146-169` (`locate_tag`)
- Modify: `crates/devkit-docs/src/cache.rs` (`Meta`)

**Interfaces:**
- Produces: `tags::ALL` ordered package-specific first; `tags::TagPattern::{PkgAt, LeafAt, PkgDashV, LeafDashV, LeafDash, V, Plain}`.

- [ ] **Step 1: Write the failing tests**

Replace the tests module in `crates/devkit-docs/src/tags.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scoped_name_is_tried_before_the_leaf_and_before_generic() {
        let tags: Vec<String> = vec![
            "v0.13.1".into(),
            "client-fetch@0.13.1".into(),
            "@hey-api/client-fetch@0.13.1".into(),
        ];
        let (p, t) = find(&tags, "@hey-api/client-fetch", "0.13.1").unwrap();
        assert_eq!(p, TagPattern::PkgAt);
        assert_eq!(t, "@hey-api/client-fetch@0.13.1");
    }

    #[test]
    fn generic_patterns_still_match_when_nothing_specific_exists() {
        let tags: Vec<String> = vec!["v1.15.11".into()];
        let (p, t) = find(&tags, "h3", "1.15.11").unwrap();
        assert_eq!(p, TagPattern::V);
        assert_eq!(t, "v1.15.11");
        assert!(find(&tags, "h3", "9.9.9").is_none());
    }

    #[test]
    fn apply_renders_every_shape() {
        let pkg = "@hey-api/client-fetch";
        assert_eq!(apply(TagPattern::PkgAt, pkg, "0.13.1"), "@hey-api/client-fetch@0.13.1");
        assert_eq!(apply(TagPattern::LeafAt, pkg, "0.13.1"), "client-fetch@0.13.1");
        assert_eq!(apply(TagPattern::PkgDashV, pkg, "0.13.1"), "@hey-api/client-fetch-v0.13.1");
        assert_eq!(apply(TagPattern::LeafDashV, pkg, "0.13.1"), "client-fetch-v0.13.1");
        assert_eq!(apply(TagPattern::LeafDash, pkg, "0.13.1"), "client-fetch-0.13.1");
        assert_eq!(apply(TagPattern::V, pkg, "0.13.1"), "v0.13.1");
        assert_eq!(apply(TagPattern::Plain, pkg, "0.13.1"), "0.13.1");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-docs tags::`
Expected: FAIL — `TagPattern::PkgAt` does not exist.

- [ ] **Step 3: Implement the new pattern set**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagPattern {
    PkgAt,
    LeafAt,
    PkgDashV,
    LeafDashV,
    LeafDash,
    V,
    Plain,
}

pub const ALL: [TagPattern; 7] = [
    TagPattern::PkgAt,
    TagPattern::LeafAt,
    TagPattern::PkgDashV,
    TagPattern::LeafDashV,
    TagPattern::LeafDash,
    TagPattern::V,
    TagPattern::Plain,
];

pub fn apply(p: TagPattern, package: &str, version: &str) -> String {
    let leaf = package.rsplit('/').next().unwrap_or(package);
    match p {
        TagPattern::PkgAt => format!("{package}@{version}"),
        TagPattern::LeafAt => format!("{leaf}@{version}"),
        TagPattern::PkgDashV => format!("{package}-v{version}"),
        TagPattern::LeafDashV => format!("{leaf}-v{version}"),
        TagPattern::LeafDash => format!("{leaf}-{version}"),
        TagPattern::V => format!("v{version}"),
        TagPattern::Plain => version.to_string(),
    }
}
```

`find` is unchanged — it already probes `ALL` in order and returns the first hit.

- [ ] **Step 4: Make the cached pattern a hint, not a short circuit**

In `crates/devkit-docs/src/resolve.rs`, `locate_tag` currently returns as soon as
the cached pattern matches. Delete that early return so `tags::find` always runs
in priority order; keep writing `meta.tag_pattern` on success. A `meta.toml`
written by 0.12.x carries a pattern name that no longer deserializes, which
`read_meta`'s `unwrap_or_default()` already discards.

```rust
fn locate_tag(
    lib: &LibCache,
    meta: &mut cache::Meta,
    package: &str,
    version: &str,
) -> Result<Option<String>> {
    if let Some((p, t)) = tags::find(&lib.tags()?, package, version) {
        meta.tag_pattern = Some(p);
        return Ok(Some(t));
    }
    lib.fetch()?;
    if let Some((p, t)) = tags::find(&lib.tags()?, package, version) {
        meta.tag_pattern = Some(p);
        return Ok(Some(t));
    }
    Ok(None)
}
```

- [ ] **Step 5: Run the gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/tags.rs crates/devkit-docs/src/resolve.rs
git commit -m "fix(docs): probe package-specific tags before generic ones"
```

---

### Task 4: Per-library lock and atomic writes

§9. Land before anything adds new writers to `meta.toml`.

**Files:**
- Create: `crates/devkit-docs/src/locks.rs`
- Create: `crates/devkit-docs/tests/concurrency.rs`
- Modify: `crates/devkit-docs/src/cache.rs` (`write_meta`)
- Modify: `crates/devkit-docs/src/manifest.rs` (`upsert_global`, `upsert_project`, `remove_global`, `remove_project`)
- Modify: `crates/devkit-docs/src/lib.rs`

**Interfaces:**
- Produces:
  - `locks::with_lib<T>(cache_root: &Path, lib: &str, f: impl FnOnce() -> Result<T>) -> Result<T>`
  - `locks::lock_path(cache_root: &Path, lib: &str) -> Result<PathBuf>` — `<cache>/registry.locks/<encoded>.lock`
  - `locks::is_control(component: &str) -> bool` — true for `registry` / `registry.*`, so prune, doctor and the upgrade pass can skip them

- [ ] **Step 1: Write the failing test**

Create `crates/devkit-docs/tests/concurrency.rs`:

```rust
use std::path::Path;

#[test]
fn the_lock_path_sits_under_the_reserved_stem_and_outside_the_library_dir() {
    let root = Path::new("/tmp/docm-lockpath");
    let p = devkit_docs::locks::lock_path(root, "@types/node").unwrap();
    assert_eq!(p, root.join("registry.locks").join("@types~node.lock"));
    assert!(devkit_docs::locks::is_control("registry.locks"));
    assert!(devkit_docs::locks::is_control("registry.json.tmp"));
    assert!(!devkit_docs::locks::is_control("registryfoo"));
}

#[test]
fn a_long_library_name_is_rejected_before_the_lock_suffix_overflows() {
    let root = Path::new("/tmp/docm-lockpath");
    let long = "n".repeat(252);
    assert!(devkit_docs::locks::lock_path(root, &long).is_err());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-docs --test concurrency`
Expected: FAIL to compile — no `locks` module.

- [ ] **Step 3: Implement `locks.rs`**

```rust
//! Per-library advisory lock. Serializes clone, fetch, materialization,
//! metadata writes and the reference-registry commit for one library against
//! each other and against prune.
//!
//! Lock files live outside the library directory so a held lock survives the
//! upgrade pass renaming that directory, and are never unlinked: an unlink
//! would let two processes hold locks on different inodes for one path.

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

const DIR: &str = "registry.locks";
const SUFFIX: &str = ".lock";
const MAX_COMPONENT_BYTES: usize = 255;

pub fn is_control(component: &str) -> bool {
    component == "registry" || component.starts_with("registry.")
}

pub fn lock_path(cache_root: &Path, lib: &str) -> Result<PathBuf> {
    let component = format!("{}{SUFFIX}", crate::names::lib_dir(lib)?);
    if component.len() > MAX_COMPONENT_BYTES {
        bail!("library name `{lib}` is too long to form a lock file name");
    }
    Ok(cache_root.join(DIR).join(component))
}

pub fn with_lib<T>(cache_root: &Path, lib: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let path = lock_path(cache_root, lib)?;
    fs::create_dir_all(path.parent().expect("lock path has a parent"))?;
    let file = File::create(&path).with_context(|| format!("opening {}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("locking {}", path.display()))?;
    let out = f();
    let _ = fs2::FileExt::unlock(&file);
    out
}
```

Add `fs2` to `crates/devkit-docs/Cargo.toml` (already a workspace dependency —
copy the line from `crates/devkit-locks/Cargo.toml`), and `pub mod locks;` to
`lib.rs`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-docs --test concurrency`
Expected: PASS, 2 tests.

- [ ] **Step 5: Make `write_meta` atomic**

In `crates/devkit-docs/src/cache.rs`:

```rust
pub fn write_meta(lib_dir: &Path, m: &Meta) -> Result<()> {
    std::fs::create_dir_all(lib_dir)?;
    let path = lib_dir.join("meta.toml");
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(m)?).context("writing meta.toml")?;
    std::fs::rename(&tmp, &path).context("replacing meta.toml")?;
    Ok(())
}
```

- [ ] **Step 6: Make manifest writes atomic**

Apply the same write-temp-then-rename to the file write inside
`manifest::upsert_global`, `upsert_project`, `remove_global` and
`remove_project`.

- [ ] **Step 7: Run the gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/locks.rs crates/devkit-docs/src/lib.rs \
        crates/devkit-docs/src/cache.rs crates/devkit-docs/src/manifest.rs \
        crates/devkit-docs/Cargo.toml crates/devkit-docs/tests/concurrency.rs
git commit -m "feat(docs): add a per-library lock and atomic writes"
```

---

### Task 5: Ref-named checkouts, identity and verification

§1, §2 — the core change. Retires the shared `default` directory.

**Files:**
- Modify: `crates/devkit-docs/src/cache.rs`
- Modify: `crates/devkit-docs/src/resolve.rs`
- Test: `crates/devkit-docs/tests/resolve.rs`, `crates/devkit-docs/tests/cache.rs`

**Interfaces:**
- Consumes: `names::checkout_dir`, `locks::with_lib`.
- Produces:
  - `cache::WorktreeMeta { raw_ref: String, resolved_ref: String, commit: String }`
  - `cache::Meta { origin: Option<String>, tag_pattern: Option<TagPattern>, layouts: BTreeMap<String, Layout>, worktrees: BTreeMap<String, WorktreeMeta> }`
  - `LibCache::resolve_ref(&self, r: &str) -> Result<(String, String)>` — returns `(canonical_ref, commit)`
  - `LibCache::ensure_at(&self, dirname: &str, commit: &str) -> Result<PathBuf>` — materialize, re-point on HEAD mismatch
  - `LibCache::assert_clean(&self, path: &Path) -> Result<()>`
  - `resolve::Resolved` gains `git_ref: String`, `commit: String`, `status: Status`, `origin: String`
  - `resolve::Status { Ok, Repaired }`

- [ ] **Step 1: Write the failing tests**

Append to `crates/devkit-docs/tests/resolve.rs`:

```rust
#[test]
fn a_changed_pin_gets_its_own_directory_and_never_returns_the_old_checkout() {
    let base = common::unique_tmp("repin");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");

    let mut entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let first = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();
    assert!(first.path.ends_with("v1.0.0"));
    assert_eq!(std::fs::read_to_string(first.path.join("src/lib.rs")).unwrap(), "// v1");

    entry.r#ref = Some("v1.1.0".into());
    let second = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();
    assert!(second.path.ends_with("v1.1.0"), "a re-pin must not reuse the old directory");
    assert_eq!(std::fs::read_to_string(second.path.join("src/lib.rs")).unwrap(), "// v2");
    // The old checkout is untouched until prune reclaims it.
    assert_eq!(std::fs::read_to_string(first.path.join("src/lib.rs")).unwrap(), "// v1");
    assert_ne!(first.commit, second.commit);
}

#[test]
fn a_corrupted_head_is_repaired_and_reported() {
    let base = common::unique_tmp("repair");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let r = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();
    devkit_common::cmd::capture(
        "git",
        &["checkout", "--detach", "v1.1.0"],
        Some(r.path.to_str().unwrap()),
    )
    .unwrap();

    let again = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();
    assert_eq!(again.status, devkit_docs::resolve::Status::Repaired);
    assert_eq!(again.commit, r.commit);
}

#[test]
fn a_dirty_checkout_is_a_hard_error_tracked_or_untracked() {
    let base = common::unique_tmp("dirty");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let r = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();

    std::fs::write(r.path.join("src/lib.rs"), "// tampered").unwrap();
    assert!(devkit_docs::resolve::resolve(&entry, &base, &cache).is_err());
    std::fs::write(r.path.join("src/lib.rs"), "// v1").unwrap();

    // An untracked file is found by rg and cited exactly like tracked source.
    std::fs::write(r.path.join("src/planted.rs"), "// planted").unwrap();
    assert!(devkit_docs::resolve::resolve(&entry, &base, &cache).is_err());
}

#[test]
fn a_repo_url_change_is_refused_rather_than_reusing_the_clone() {
    let base = common::unique_tmp("origin");
    let a = common::fixture_repo(&base.join("a"));
    let b = common::fixture_repo(&base.join("b"));
    let cache = base.join("cache");
    let mut entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(a),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();
    entry.repo = Some(b);
    let err = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap_err();
    assert!(err.to_string().contains("origin"), "error must name the mismatch: {err}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-docs --test resolve`
Expected: FAIL — `Resolved` has no `commit`/`status`; the re-pin test returns the `default` path.

- [ ] **Step 3: Extend `Meta` and add origin recording**

In `crates/devkit-docs/src/cache.rs`:

```rust
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeMeta {
    pub raw_ref: String,
    pub resolved_ref: String,
    pub commit: String,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_pattern: Option<TagPattern>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, Layout>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub worktrees: BTreeMap<String, WorktreeMeta>,
}
```

`ensure_clone` gains an origin check. A cache written by 0.12.x has no `origin`,
so bootstrap it from the bare repo rather than assuming the manifest is right:

```rust
    pub fn ensure_clone(&self, repo: &str, meta: &mut Meta) -> Result<()> {
        if self.cloned() {
            let actual = match meta.origin.clone() {
                Some(o) => o,
                None => cmd::git(&["config", "--get", "remote.origin.url"], &self.bare_str())?
                    .trim()
                    .to_string(),
            };
            if actual != repo {
                bail!(
                    "{} was cloned from origin {actual}, but the manifest now asks for {repo}; \
                     remove the entry and re-add it to use a different repository",
                    self.dir.display()
                );
            }
            meta.origin = Some(actual);
            return Ok(());
        }
        // ... existing clone body ...
        meta.origin = Some(repo.to_string());
        Ok(())
    }
```

- [ ] **Step 4: Implement ref resolution, materialization and verification**

```rust
    /// Canonical ref name plus the commit it peels to, from the local repo.
    /// A `refs/`-qualified ref is used verbatim; a 40-hex string is an object
    /// name; anything else is a tag first, then a branch, and matching both is
    /// ambiguous.
    pub fn resolve_ref(&self, r: &str) -> Result<(String, String)> {
        let peel = |name: &str| -> Option<String> {
            cmd::git(
                &["rev-parse", "--verify", "--end-of-options", &format!("{name}^{{commit}}")],
                &self.bare_str(),
            )
            .ok()
            .map(|s| s.trim().to_string())
        };
        if r.starts_with("refs/") {
            let c = peel(r).with_context(|| format!("`{r}` does not resolve"))?;
            return Ok((r.to_string(), c));
        }
        if r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit())
            && let Some(c) = peel(r)
        {
            return Ok((r.to_string(), c));
        }
        let tag = peel(&format!("refs/tags/{r}"));
        let head = peel(&format!("refs/heads/{r}"));
        match (tag, head) {
            (Some(_), Some(_)) => bail!(
                "`{r}` is both a tag and a branch; pin it as refs/tags/{r} or refs/heads/{r}"
            ),
            (Some(c), None) => Ok((format!("refs/tags/{r}"), c)),
            (None, Some(c)) => Ok((format!("refs/heads/{r}"), c)),
            (None, None) => bail!("`{r}` does not resolve to a commit"),
        }
    }

    /// Materialize at `commit`, re-pointing an existing worktree that drifted.
    /// Returns whether it had to repair.
    pub fn ensure_at(&self, dirname: &str, commit: &str) -> Result<(PathBuf, bool)> {
        let path = self.worktree_path(dirname);
        if !path.is_dir() {
            let p = path.to_string_lossy().into_owned();
            cmd::git(
                &["worktree", "add", "--detach", p.as_str(), commit],
                &self.bare_str(),
            )
            .with_context(|| format!("materializing {dirname} at {commit}"))?;
            return Ok((path, false));
        }
        let head = cmd::git(&["rev-parse", "HEAD"], &path.to_string_lossy())?
            .trim()
            .to_string();
        if head == commit {
            return Ok((path, false));
        }
        cmd::git(&["checkout", "--detach", commit], &path.to_string_lossy())?;
        Ok((path, true))
    }

    /// A checkout an agent reads must be exactly the released source: any
    /// modification, tracked or not, makes a citation from it untrue.
    pub fn assert_clean(&self, path: &Path) -> Result<()> {
        let out = cmd::git(&["status", "--porcelain"], &path.to_string_lossy())?;
        if !out.trim().is_empty() {
            bail!(
                "{} has local modifications:\n{}\nremove them, or `docm prune` and re-resolve",
                path.display(),
                out.trim()
            );
        }
        Ok(())
    }
```

- [ ] **Step 5: Rewrite `resolve::resolve` around them**

`resolve` runs its whole body inside `locks::with_lib`, including the reference
registry commit (§9): releasing between materialization and the commit reopens
the window prune must not have. The pinned branch of the selection becomes:

```rust
    let dirname = names::checkout_dir(&git_ref)?;
    let (canonical, commit) = lib.resolve_ref(&git_ref)?;
    if let Some(prev) = meta.worktrees.get(&dirname)
        && prev.resolved_ref != canonical
    {
        bail!(
            "`{git_ref}` previously resolved to {} and now resolves to {canonical}; \
             the pin changed kind upstream — re-pin it explicitly",
            prev.resolved_ref
        );
    }
    let (path, repaired) = lib.ensure_at(&dirname, &commit)?;
    lib.assert_clean(&path)?;
    meta.worktrees.insert(
        dirname.clone(),
        cache::WorktreeMeta {
            raw_ref: git_ref.clone(),
            resolved_ref: canonical,
            commit: commit.clone(),
        },
    );
```

`default_worktree` and every `ensure_worktree("default", …)` call site are deleted.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p devkit-docs --test resolve`
Expected: PASS.

- [ ] **Step 7: Add `--prune-tags` and the moved-tag report**

In `LibCache::fetch`, add `--prune-tags` to the argument list. In `resolve`,
when `meta.worktrees[dirname].commit` differs from the freshly resolved commit
for a `refs/tags/` ref, print to stderr before re-pointing:

```rust
eprintln!(
    "docm: tag {git_ref} moved {} → {commit} upstream; {dirname} re-pointed",
    prev.commit
);
```

- [ ] **Step 8: Run the gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/cache.rs crates/devkit-docs/src/resolve.rs \
        crates/devkit-docs/tests/resolve.rs crates/devkit-docs/tests/cache.rs
git commit -m "fix(docs): name checkouts for their ref and verify them"
```

---

### Task 6: Importer-graph version selection

§3. The largest task. Depends on Task 1 only.

**Files:**
- Create: `crates/devkit-docs/src/importers.rs`
- Create: `crates/devkit-docs/tests/importers.rs`
- Modify: `crates/devkit-docs/src/lockfiles.rs` (keep parsing helpers, drop `highest`-based selection)
- Modify: `crates/devkit-docs/src/resolve.rs`

**Interfaces:**
- Produces:
  - `importers::Selection { pub workspace: PathBuf, pub version: String, pub source: String }`
  - `importers::select(start: &Path, eco: Ecosystem, package: &str) -> Result<Selection>`

`source` is the human explanation printed on stderr, e.g.
`"apps/lab-os installs it (bun.lock; 4 other versions present)"`.

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-docs/tests/importers.rs`. The bun case is the field
report verbatim — an alias entry that must not win:

```rust
use devkit_docs::importers;
use devkit_docs::manifest::Ecosystem;

mod common;

const BUN_LOCK: &str = r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": { "name": "root" },
    "apps/api": { "name": "@app/api", "dependencies": { "h3": "^1.15.5" } }
  },
  "packages": {
    "h3": ["h3@1.15.11", "", {}, "sha512-a"],
    "h3-v2": ["h3@2.0.1-rc.20", "", {}, "sha512-b"],
    "@compat/h3": ["h3@2.0.1", "", {}, "sha512-c"]
  }
}"#;

#[test]
fn a_bun_alias_never_wins_over_the_declared_dependency() {
    let root = common::unique_tmp("bun-alias");
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/api");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("package.json"), r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#).unwrap();

    let s = importers::select(&ws, Ecosystem::Js, "h3").unwrap();
    assert_eq!(s.version, "1.15.11");
    assert!(s.source.contains("apps/api"), "must name the deciding workspace: {}", s.source);
}

#[test]
fn a_transitive_package_is_a_hard_error_that_lists_candidates() {
    let root = common::unique_tmp("bun-transitive");
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    let ws = root.join("apps/web");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("package.json"), r#"{"name":"@app/web","dependencies":{}}"#).unwrap();

    let err = importers::select(&ws, Ecosystem::Js, "h3").unwrap_err().to_string();
    assert!(err.contains("does not declare"), "{err}");
    assert!(err.contains("--ref"), "the error must suggest a pin: {err}");
}

#[test]
fn a_pnpm_peer_qualified_locator_yields_a_bare_version() {
    let root = common::unique_tmp("pnpm-peer");
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  apps/api:\n    dependencies:\n      vitest:\n        specifier: ^3.2.0\n        version: 3.2.4(@types/node@25.5.0)\n",
    )
    .unwrap();
    let ws = root.join("apps/api");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("package.json"), r#"{"name":"api","dependencies":{"vitest":"^3.2.0"}}"#).unwrap();

    assert_eq!(importers::select(&ws, Ecosystem::Js, "vitest").unwrap().version, "3.2.4");
}

#[test]
fn competing_js_lockfiles_are_selected_by_packagemanager_and_otherwise_error() {
    let root = common::unique_tmp("two-locks");
    std::fs::write(root.join("bun.lock"), BUN_LOCK).unwrap();
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  apps/api:\n    dependencies:\n      h3:\n        specifier: ^1.15.5\n        version: 1.15.7\n",
    )
    .unwrap();
    let ws = root.join("apps/api");
    std::fs::create_dir_all(&ws).unwrap();

    std::fs::write(ws.join("package.json"), r#"{"name":"@app/api","dependencies":{"h3":"^1.15.5"}}"#).unwrap();
    let err = importers::select(&ws, Ecosystem::Js, "h3").unwrap_err().to_string();
    assert!(err.contains("packageManager"), "{err}");

    std::fs::write(
        ws.join("package.json"),
        r#"{"name":"@app/api","packageManager":"pnpm@9.0.0","dependencies":{"h3":"^1.15.5"}}"#,
    )
    .unwrap();
    assert_eq!(importers::select(&ws, Ecosystem::Js, "h3").unwrap().version, "1.15.7");
}

#[test]
fn a_uv_fork_recording_two_versions_is_a_hard_error() {
    let root = common::unique_tmp("uv-fork");
    std::fs::write(
        root.join("uv.lock"),
        "version = 1\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package.dev-dependencies]]\n\n[[package]]\nname = \"httpx\"\nversion = \"0.27.0\"\n\n[[package]]\nname = \"httpx\"\nversion = \"0.28.1\"\n",
    )
    .unwrap();
    std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"app\"\ndependencies = [\"httpx\"]\n").unwrap();

    let err = importers::select(&root, Ecosystem::Python, "httpx").unwrap_err().to_string();
    assert!(err.contains("0.27.0") && err.contains("0.28.1"), "{err}");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-docs --test importers`
Expected: FAIL to compile — no `importers` module.

- [ ] **Step 3: Implement `importers.rs`**

Structure it as one function per format behind a dispatcher. Each returns
`Result<Selection>`; every unhandled shape is an error, never a guess.

```rust
//! Which version does *this workspace* install? Resolved from the lockfile's
//! importer graph, not by matching a declared range: a nested copy elsewhere in
//! the lockfile can satisfy the same range and is not what the workspace runs.

use crate::manifest::Ecosystem;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

pub struct Selection {
    pub workspace: PathBuf,
    pub version: String,
    pub source: String,
}

pub fn select(start: &Path, eco: Ecosystem, package: &str) -> Result<Selection> {
    match eco {
        Ecosystem::Js => js(start, package),
        Ecosystem::Rust => cargo(start, package),
        Ecosystem::Python => uv(start, package),
        Ecosystem::Git => bail!("git entries resolve by ref, not by lockfile"),
    }
}
```

Implementation requirements, each covered by a test above:

- **`js`**: walk up from `start` for the nearest `package.json`; walk up from
  there for the lockfile directory. Choose the lockfile by `packageManager`
  (`bun` → `bun.lock`, `pnpm` → `pnpm-lock.yaml`, `npm`/`yarn` →
  `package-lock.json`); with no `packageManager`, exactly one present is used and
  two or more is an error naming each file and the version it resolves to.
- **pnpm**: read `importers.<rel-workspace>.{dependencies,devDependencies,optionalDependencies}.<pkg>.version`; strip a trailing `(…)` peer suffix; follow an alias locator (`npm:other@1.2.3`) to its package identity.
- **bun**: the workspace's own declaration in `workspaces.<rel>.{dependencies,devDependencies,optionalDependencies,peerDependencies}` names the key to look up; then `packages["<workspace-name>/<pkg>"]` if present, else `packages["<pkg>"]`. Looking up the declared *key* is what makes an alias unreachable unless declared.
- **npm**: require `packages.<rel-workspace>` to declare the package, then resolve the nearest `packages["<dir>/node_modules/<pkg>"]` walking `<dir>` upward from the workspace to the root.
- **cargo**: find the member's `[[package]]` entry by the workspace `Cargo.toml`'s `package.name`, then resolve its `dependencies` entry against the lock's package set.
- **uv**: same shape over `uv.lock`, reading `dependencies`, `[package.dev-dependencies]` and optional-dependency tables. More than one version for the package is an error listing them.
- Not declared by the workspace → error: `"<ws> does not declare <pkg> (it is transitive); pin the version with --ref"`, listing each candidate version and the dependent requiring it.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-docs --test importers`
Expected: PASS, 5 tests.

- [ ] **Step 5: Call it from `resolve`**

Replace the `lockfiles::find_version` + `highest` block in `resolve::resolve`
with `importers::select`, print `selection.source` on stderr, and use
`selection.workspace` as the reference-registry key (Task 7).

- [ ] **Step 6: Run the gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/importers.rs crates/devkit-docs/src/lib.rs \
        crates/devkit-docs/src/lockfiles.rs crates/devkit-docs/src/resolve.rs \
        crates/devkit-docs/tests/importers.rs
git commit -m "fix(docs): resolve versions from the workspace importer graph"
```

---

### Task 7: Workspace-keyed references, legacy retirement, prune under the lock

§7.

**Files:**
- Modify: `crates/devkit-docs/src/refs.rs`
- Modify: `src/bin/docm.rs` (`cmd_prune`)
- Test: `crates/devkit-docs/tests/prune.rs`, `crates/devkit-docs/tests/concurrency.rs`

**Interfaces:**
- Consumes: `importers::Selection::workspace`, `locks::with_lib`, `locks::is_control`.
- Produces: `RefRow { project, lib, version, git_ref: String, commit: String, resolved_at }` with `#[serde(default)]` on `git_ref`/`commit`; `Data::record(workspace, lib, dirname, git_ref, commit)`; `Data::retire_legacy(workspace, lib)`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/devkit-docs/tests/prune.rs`:

```rust
#[test]
fn two_workspaces_sharing_a_lockfile_keep_separate_rows() {
    let mut d = devkit_docs::refs::Data::default();
    d.record("/repo/apps/api", "h3", "v1.15.11", "v1.15.11", "aaa");
    d.record("/repo/apps/web", "h3", "v2.0.1", "v2.0.1", "bbb");
    assert_eq!(d.rows.len(), 2, "a workspace key must not overwrite its sibling");
}

#[test]
fn a_legacy_row_protects_default_until_a_real_materialization_retires_it() {
    let mut d = devkit_docs::refs::Data::default();
    // A 0.12.x row: empty commit, keyed by the lockfile directory.
    d.record_legacy("/repo", "h3", "default");
    assert_eq!(devkit_docs::refs::current_for_row(&d.rows[0]), "default");

    d.record("/repo/apps/api", "h3", "v1.15.11", "v1.15.11", "aaa");
    d.retire_legacy("/repo/apps/api", "h3");
    assert_eq!(d.rows.len(), 1);
    assert_eq!(d.rows[0].version, "v1.15.11");
}
```

Append to `crates/devkit-docs/tests/concurrency.rs` — the race the lock must
close, using the same multiprocess pattern as `tests/refs_race.rs`:

```rust
#[test]
fn prune_cannot_delete_a_directory_a_concurrent_resolve_just_materialized() {
    // Child holds the library lock, materializes, sleeps before recording;
    // parent runs prune. Prune must block on the lock, then observe the row.
    // See tests/refs_race.rs for the process-spawn harness this mirrors.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-docs --test prune`
Expected: FAIL to compile — `record` takes three arguments.

- [ ] **Step 3: Implement**

- `RefRow` gains `git_ref` and `commit`, both `#[serde(default)]`.
- `Data::record` takes the workspace path and the new fields.
- `Data::record_legacy` writes a row with an empty `commit` (test support and
  the read path for 0.12.x data).
- `refs::current_version` computes the dirname the resolver would produce —
  except for a legacy row (empty `commit`), which keeps returning `default`.
- `Data::retire_legacy(workspace, lib)` drops any row for `lib` whose project is
  an ancestor directory of `workspace` and whose `commit` is empty. Call it in
  the same registry commit that records the new row.
- Delete the `d != "default"` exemption in `plan`.
- `plan_for_cache` skips any cache-root entry where `locks::is_control(name)`.
- `cmd_prune` wraps each library's recheck and `remove_worktree` in
  `locks::with_lib`.

- [ ] **Step 4: Run tests, gate, commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/refs.rs src/bin/docm.rs \
        crates/devkit-docs/tests/prune.rs crates/devkit-docs/tests/concurrency.rs
git commit -m "fix(docs): key references by workspace and lock prune"
```

---

### Task 8: The 0.12.x upgrade pass

§7.

**Files:**
- Create: `crates/devkit-docs/src/upgrade.rs`
- Create: `crates/devkit-docs/tests/upgrade.rs`
- Modify: `crates/devkit-docs/src/lib.rs`, `src/bin/docm.rs`

**Interfaces:**
- Produces: `upgrade::run(cache_root: &Path) -> Result<Vec<String>>` — returns a line per migration performed, empty when nothing needed doing. Idempotent.

- [ ] **Step 1: Write the failing test**

```rust
mod common;

#[test]
fn a_nested_scoped_cache_migrates_and_its_worktree_still_works() {
    let base = common::unique_tmp("upgrade");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");

    // Build a 0.12.x-shaped cache by hand: nested scope, worktree, no origin.
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    let bare = old.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", &repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    devkit_common::cmd::capture(
        "git",
        &["worktree", "add", "--detach", old.join("v1.0.0").to_str().unwrap(), "v1.0.0"],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(!done.is_empty());

    let new = cache.join("@scope~pkg");
    assert!(new.is_dir());
    assert!(!cache.join("@scope").exists());

    // git worktree repair must have fixed both reciprocal paths.
    let status = devkit_common::cmd::capture(
        "git",
        &["status", "--porcelain"],
        Some(new.join("v1.0.0").to_str().unwrap()),
    )
    .unwrap();
    assert!(status.trim().is_empty());

    let meta = devkit_docs::cache::read_meta(&new);
    assert!(meta.origin.is_some(), "origin must be recovered from remote.origin.url");

    // Idempotent.
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-docs --test upgrade`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `upgrade::run`**

Under `locks::with_lib` per migrated library:

1. Enumerate cache-root entries, skipping any where `locks::is_control(name)`.
2. An entry that is a directory *without* `repo.git` but whose children contain
   `repo.git` is a nested scope: for each child, rename
   `<root>/<scope>/<pkg>` to `<root>/<scope>~<pkg>`, then run
   `git worktree repair <path>` from the moved bare repo for every worktree
   under it, then verify each worktree's HEAD resolves; remove and
   re-materialize any that will not repair. Remove the emptied `<scope>` dir.
3. For every library directory, if `meta.toml` has no `origin`, read
   `git config --get remote.origin.url` from its bare repo and write it.

Call `upgrade::run` once at the start of `docm`'s `main`, printing each returned
line to stderr.

- [ ] **Step 4: Run tests, gate, commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/upgrade.rs crates/devkit-docs/src/lib.rs \
        src/bin/docm.rs crates/devkit-docs/tests/upgrade.rs
git commit -m "feat(docs): migrate 0.12 caches on first run"
```

---

### Task 9: Hard-error failure modes

§5. Land after Tasks 3 and 6, so every path that should succeed does.

**Files:**
- Modify: `crates/devkit-docs/src/resolve.rs`, `src/bin/docm.rs`
- Test: `crates/devkit-docs/tests/resolve.rs`

**Interfaces:**
- Produces: `resolve::Options { pub allow_default_branch: bool }`; `resolve::resolve(entry, start, cache_root, opts)`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_version_with_no_tag_is_a_hard_error_listing_what_was_tried() {
    let base = common::unique_tmp("notag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    std::fs::write(base.join("Cargo.lock"), "version = 4\n\n[[package]]\nname = \"up\"\nversion = \"9.9.9\"\n").unwrap();
    std::fs::write(base.join("Cargo.toml"), "[package]\nname = \"app\"\n\n[dependencies]\nup = \"9.9.9\"\n").unwrap();

    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let opts = devkit_docs::resolve::Options::default();
    let err = devkit_docs::resolve::resolve(&entry, &base, &cache, &opts).unwrap_err().to_string();
    assert!(err.contains("9.9.9"), "{err}");
    assert!(err.contains("v9.9.9"), "the error must list the patterns tried: {err}");
    assert!(err.contains("--allow-default-branch"), "{err}");

    let opts = devkit_docs::resolve::Options { allow_default_branch: true };
    assert!(devkit_docs::resolve::resolve(&entry, &base, &cache, &opts).is_ok());
}

#[test]
fn a_git_entry_with_no_ref_is_a_hard_error_naming_sync() {
    let base = common::unique_tmp("noref");
    let repo = common::fixture_repo(&base.join("src"));
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo),
        ..Default::default()
    };
    let err = devkit_docs::resolve::resolve(
        &entry,
        &base,
        &base.join("cache"),
        &devkit_docs::resolve::Options::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("docm sync"), "{err}");
}
```

- [ ] **Step 2: Run to verify failure, implement, re-run**

Run: `cargo test -p devkit-docs --test resolve`

Implement: replace each `warnings.push(...)` fallback in `resolve` with a
`bail!` unless `opts.allow_default_branch`. Add `--allow-default-branch` as a
clap-global flag in `src/bin/docm.rs` and thread it through `resolve_one`.

- [ ] **Step 3: Gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/resolve.rs src/bin/docm.rs crates/devkit-docs/tests/resolve.rs
git commit -m "feat(docs)!: fail instead of falling back to the default branch"
```

---

### Task 10: Ecosystem probing and `rm` aliases

§5, §6. Independent of every other task.

**Files:**
- Modify: `crates/devkit-docs/src/lookup.rs`, `src/bin/docm.rs`

**Interfaces:**
- Produces: `lookup::detect(reg: &dyn Registry, package: &str, cwd: &Path) -> Result<(Ecosystem, String)>`.

- [ ] **Step 1: Write the failing tests**

Extend the tests module in `lookup.rs`:

```rust
    #[test]
    fn a_name_in_two_registries_refuses_and_names_both() {
        struct Both;
        impl Registry for Both {
            fn repo_url(&self, eco: Ecosystem, _p: &str) -> anyhow::Result<String> {
                match eco {
                    Ecosystem::Rust => Ok("https://github.com/hyperium/h3".into()),
                    Ecosystem::Js => Ok("https://github.com/unjs/h3".into()),
                    _ => anyhow::bail!("not found"),
                }
            }
        }
        let dir = std::env::temp_dir();
        let err = detect(&Both, "h3", &dir).unwrap_err().to_string();
        assert!(err.contains("hyperium"), "{err}");
        assert!(err.contains("unjs"), "{err}");
        assert!(err.contains("--eco"), "{err}");
    }

    #[test]
    fn a_js_project_reports_js_when_only_npm_has_the_name() {
        struct Npm;
        impl Registry for Npm {
            fn repo_url(&self, eco: Ecosystem, _p: &str) -> anyhow::Result<String> {
                match eco {
                    Ecosystem::Js => Ok("https://github.com/unjs/h3".into()),
                    _ => anyhow::bail!("not found"),
                }
            }
        }
        let dir = std::env::temp_dir().join(format!("docm-eco-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bun.lock"), "{}").unwrap();
        let (eco, _) = detect(&Npm, "h3", &dir).unwrap();
        assert_eq!(eco, Ecosystem::Js);
    }
```

- [ ] **Step 2: Run to verify failure, implement, re-run**

`detect` probes **all three** registries, orders them by markers found walking up
from `cwd` (`bun.lock`/`package-lock.json`/`pnpm-lock.yaml`/`package.json` → js;
`Cargo.toml` → rust; `pyproject.toml`/`uv.lock` → python), and bails when more
than one hits, naming each ecosystem and URL.

Add the aliases in `src/bin/docm.rs`:

```rust
    /// Remove a library from the manifest (checkouts are reclaimed by prune).
    #[command(visible_alias = "remove", visible_alias = "delete")]
    Rm {
```

- [ ] **Step 3: Gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/lookup.rs src/bin/docm.rs
git commit -m "fix(docs): refuse ambiguous ecosystems and alias rm"
```

---

### Task 11: CLI surface — `add`, `sync`, `info`, `list`, doctor

§6, §6.1.

**Files:**
- Modify: `src/bin/docm.rs`, `crates/devkit-docs/src/manifest.rs`
- Modify: `src/bin/devkit/` (doctor row)
- Test: `crates/devkit-docs/tests/concurrency.rs`

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_failed_repin_leaves_the_previous_entry_intact() {
    // add lib at v1.0.0, then re-add with --ref does-not-exist; the manifest
    // must still hold v1.0.0, not lose the library.
}

#[test]
fn a_failed_add_of_a_new_library_leaves_the_manifest_byte_identical() {
}
```

Fill both in with the harness used by `tests/upgrade.rs`, driving
`manifest::upsert_global` plus `resolve::resolve` the way `cmd_add` will.

- [ ] **Step 2: Implement `add`**

Order, all inside `locks::with_lib` for the target library:

1. snapshot any existing same-name entry
2. write the new entry (manifest lock, atomic)
3. for a git URL with no `--ref`: resolve the default branch and store it as the
   entry's `ref`. Under `--project`, bail instead with the §6.1 text.
4. materialize by calling `resolve::resolve`
5. on error: restore the snapshot, or remove the entry when there was none, then
   return the error
6. on success print the multi-line block from §6.1

- [ ] **Step 3: Implement `sync`**

Per selected library, under its lock: fetch (`--prune --prune-tags`), infer and
record a missing `ref` for a *global* git entry, re-resolve, materialize, verify,
record. Delete `sync_default` and `LibCache::sync_default`.

- [ ] **Step 4: Implement `info` and `list`**

`info` prints `name`, `repo`, `ref`, `version`, `commit`, `status`, `path`, then
layout and notes; exit non-zero on a mismatch that could not be repaired. `list`
gains ref, commit and origin columns. Both keep `--json` in sync with the struct.

- [ ] **Step 5: Add the doctor row**

`devkit doctor` sweeps every materialized checkout for HEAD correctness and
cleanliness, skipping cache-root entries where `locks::is_control(name)`.

- [ ] **Step 6: Gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/docm.rs crates/devkit-docs/src/manifest.rs src/bin/devkit \
        crates/devkit-docs/tests/concurrency.rs
git commit -m "feat(docs): materialize on add and prove checkouts in info"
```

---

### Task 12: Documentation

§8.

**Files:**
- Modify: `skills/docs/SKILL.md`, `README.md`, `AGENTS.md`

- [ ] **Step 1: Correct `skills/docs/SKILL.md`**

- Line 25's claim that the path is "version-matched to the current project's
  lockfile" becomes workspace-manifest-and-lockfile based, and points at
  `commit` as the proof of what is checked out.
- Any stderr line from docm is a hard stop until explained, not context to relay
  conditionally.
- Always pass `--notes` recording which workspace the version came from.
- When a checkout looks wrong for reasons docm cannot see, compare against the
  installed package under `node_modules`, which is ground truth for what runs.
- Drop any instruction to run `docm sync` after `add`.

- [ ] **Step 2: Update `README.md` and the `AGENTS.md` crate table**

Describe ref-named checkouts, the reserved stems, `--allow-default-branch`, and
the `rm` aliases.

- [ ] **Step 3: Commit**

```bash
git add skills/docs/SKILL.md README.md AGENTS.md
git commit -m "docs: describe version-truthful docm checkouts"
```

---

## Self-review

**Spec coverage.** §1 → Task 1; §1.1 → Tasks 1–2; §2 → Task 5; §3 → Task 6;
§4 → Task 3; §5 → Tasks 9–10; §6/§6.1 → Task 11; §7 → Tasks 7–8; §9 → Task 4;
§8 → Task 12. Spec tests 1–28 map onto Tasks 1, 3, 5, 6, 7, 8, 9, 10, 11.

**Known gaps handed to the implementer.** Task 6 step 3 specifies five lockfile
parsers as requirements rather than as finished code — each has a test above
that pins its behaviour, and the shapes are documented in the spec's §3 table.
Task 7 and Task 11 leave two multiprocess race harnesses as sketches pointing at
`tests/refs_race.rs`, which already contains the spawn pattern to copy.

**Type consistency.** `LibCache::new` returns `Result` from Task 1 onward;
`resolve::resolve` takes `&Options` from Task 9 onward, so Tasks 5–8 write calls
without it and Task 9 updates them. `Data::record` takes five arguments from
Task 7 onward.
