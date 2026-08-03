# docm Correctness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every answer `docm` gives either provably correct or a hard error — no silent fallback, no state where the manifest and the disk disagree.

**Architecture:** Checkout directories are named for the ref that produced them (`/` → `~`), library directories for the encoded library name; `meta.toml` records origin, canonical ref and commit so every resolution can prove what it returned; versions resolve through each lockfile's importer graph rather than by matching semver ranges; and a per-library advisory lock serializes clone/fetch/materialize/registry-commit against prune.

**Tech Stack:** Rust 2024, `anyhow`, `serde`/`toml`/`serde_json`/`serde_yaml_ng`, `clap` + `clap_complete`, `fd-lock` advisory locks (the workspace lock crate — `fd-lock = "4"` at `Cargo.toml:69`, already used by `devkit-common`, `devkit-locks` and `devkit-ports`), `devkit_common::cmd` for git.

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
- **Adding a dependency is allowed** — this repo has no policy against it and
  already carries ~25. Two specific choices here are not about that:
  - **Locking uses `fd-lock`**, the crate `devkit-common`, `devkit-locks` and
    `devkit-ports` already use for advisory file locks (`Cargo.toml:69`). Do not
    add `fs2`. Not because it is a new dependency, but because two locking
    crates in one workspace means two sets of flock semantics to reason about
    on three platforms.
  - **`node-semver` stays dropped** because importer-graph resolution reads the
    version the lockfile already resolved; there is no range to match.
- **Reserved names (§1, §1.1):** checkout level `repo.git`, `meta.toml`; library level the stem `registry` — that exact name or any name beginning with `registry.`.
- **Never unlink an advisory lock file after release** (implementer note): persistent lock files avoid inode-replacement races.
- **Locks are not reentrant.** `fd-lock` takes an OS advisory lock; a second acquisition of the same path from the same process opens a second file description and blocks forever. Every function that takes the library lock is named `*_locked` at its inner, lock-free layer, and only the outermost caller wraps it. Never call a lock-taking function from inside `locks::with_lib`.
- **Lock ordering, always:** library lock → manifest lock → reference-registry lock. Never the reverse, and never two library locks at once.
- **A directory name is only correct if the host stored it verbatim.** After creating any cache directory, confirm the parent lists the exact bytes requested (`cache::create_dir_exact`). This is what catches case folding and NFC/NFD folding on macOS and Windows without a Unicode dependency (§1).

## File Structure

| File | Responsibility |
|---|---|
| `crates/devkit-docs/src/names.rs` | **new** — encode/decode library and ref directory names, reserved-name and host-representability validation. Pure, no IO. |
| `crates/devkit-docs/src/locks.rs` | **new** — per-library advisory lock; `with_lib(cache_root, lib, f)`, `with_manifest`, `with_lib_dir`. |
| `crates/devkit-docs/src/barrier.rs` | **new** — test-only rendezvous (`signal`/`wait`), no-op unless `DEVKIT_DOCS_MANIFEST_BARRIER` is set. |
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
  - `names::validate_lib(name: &str) -> anyhow::Result<()>` — takes the **logical** name (`@types/node`)
  - `names::validate_ref(git_ref: &str) -> anyhow::Result<()>` — takes the **raw ref** (`release/2.x`)
  - `names::lib_dir(name: &str) -> anyhow::Result<String>` — validate the logical name, then encode
  - `names::checkout_dir(git_ref: &str) -> anyhow::Result<String>` — validate the raw ref, then encode
  - `names::fold_key(component: &str) -> String` — ASCII-case-folded comparison key
  - `cache::LibCache::from_dir(cache_root: &Path, dirname: &str) -> LibCache` — build from an
    already-encoded directory name, skipping validation. Enumeration paths use this.
  - `cache::create_dir_exact(parent: &Path, name: &str) -> Result<PathBuf>`

Two levels, two validators, and they are **not** interchangeable. `validate_lib` sees
`@types/node` and rejects `~`; `validate_ref` sees `release/2.x` and rejects `~`. Both
encode afterwards. Validating the *encoded* form for `~` would reject every name
containing `/` — which is every scoped package and every branch ref.

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
fn lib_names_reject_the_registry_stem_however_it_is_cased() {
    for n in [
        "registry",
        "registry.json",
        "registry.lock",
        "registry.json.tmp",
        "registry.json.bak",
        "registry.anything-added-later",
        // A folding host stores these as the same entry as the lowercase form.
        "REGISTRY.JSON",
        "Registry.Locks",
    ] {
        assert!(names::validate_lib(n).is_err(), "{n} must be reserved");
    }
    assert!(names::validate_lib("registryfoo").is_ok());
}

#[test]
fn names_that_would_escape_the_cache_root_are_rejected() {
    for n in ["..", ".", "../../etc", "a/../../b"] {
        assert!(names::validate_lib(n).is_err(), "{n} must not traverse");
        assert!(names::validate_ref(n).is_err(), "{n} must not traverse");
    }
}

#[test]
fn a_branch_ref_containing_a_slash_encodes_rather_than_erroring() {
    // The bug this pins: validating the *encoded* form for `~` rejects every
    // ref with a `/` in it, which is most real branch and qualified refs.
    assert_eq!(names::checkout_dir("release/2.x").unwrap(), "release~2.x");
    assert_eq!(names::checkout_dir("refs/tags/v1.0.0").unwrap(), "refs~tags~v1.0.0");
    assert_eq!(names::lib_dir("@hey-api/client-fetch").unwrap(), "@hey-api~client-fetch");
}

#[test]
fn checkout_names_reject_control_files_but_not_the_registry_stem() {
    assert!(names::validate_ref("repo.git").is_err());
    assert!(names::validate_ref("meta.toml").is_err());
    // The registry lives at the cache root, one level up from checkouts.
    assert!(names::validate_ref("registry.json").is_ok());
}

#[test]
fn rejects_names_the_host_filesystem_cannot_represent() {
    assert!(names::validate_ref(&"v".repeat(256)).is_err());
    if cfg!(windows) {
        for n in ["a|b", "a<b", "a>b", "a\"b", "NUL", "con", "COM1", "LPT9.txt"] {
            assert!(names::validate_ref(n).is_err(), "{n} must be rejected");
        }
    }
}

#[test]
fn tilde_is_illegal_in_a_git_ref_so_checkout_encoding_is_injective() {
    // No valid ref contains `~`, so `/` -> `~` cannot collide with a literal.
    assert!(names::validate_ref("release~2.x").is_err());
}

#[test]
fn case_folding_keys_let_a_caller_spot_a_host_collision() {
    assert_eq!(names::fold_key("V1.0"), names::fold_key("v1.0"));
    assert_ne!(names::fold_key("v1.0"), names::fold_key("v1.1"));
}
```

The host may fold beyond ASCII case — macOS normalizes NFD, Windows folds
Unicode case — and a normalization crate would not help: those crates model
NFC/NFD, not a volume's case-folding table or its filename storage semantics,
which vary between APFS, HFS+, NTFS and ext4 and are configurable per volume.
So the collision is caught by observation rather than prediction:
`create_dir_exact` creates the directory and then requires the parent listing
to contain the exact bytes it asked for. Add to `crates/devkit-docs/tests/cache.rs`:

```rust
#[test]
fn a_directory_the_host_folds_into_an_existing_one_is_refused() {
    let root = common::unique_tmp("fold");
    let first = devkit_docs::cache::create_dir_exact(&root, "V1.0").unwrap();
    assert!(first.is_dir());
    // Case-sensitive hosts store both; folding hosts return the first, and the
    // exact-bytes check is what turns that into an error instead of a silent alias.
    match devkit_docs::cache::create_dir_exact(&root, "v1.0") {
        Ok(p) => assert!(p.is_dir(), "a case-sensitive host keeps them distinct"),
        Err(e) => assert!(
            e.to_string().contains("V1.0"),
            "a folding host must name the directory it collided with: {e}"
        ),
    }
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

/// ASCII case-folded key. Enough to catch the collision a caller can predict;
/// the rest is caught by `cache::create_dir_exact` observing what the host stored.
pub fn fold_key(component: &str) -> String {
    component.to_ascii_lowercase()
}

/// `.` and `..` are path traversal, not names: `cache_root.join("..")` leaves
/// the cache entirely, and every deletion path downstream trusts that it did not.
fn reject_traversal(name: &str) -> Result<()> {
    if name.split(['/', '\\']).any(|c| c == "." || c == "..") {
        bail!("`{name}` contains a path traversal component");
    }
    Ok(())
}

/// Takes the *logical* library name (`@types/node`), never the encoded form.
///
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
    reject_traversal(name)?;
    // Folded: on a case-folding host `REGISTRY.JSON` and `registry.json` are one
    // directory entry, so an exact comparison lets the reserved stem be bypassed.
    let folded = fold_key(name);
    if folded == "registry" || folded.starts_with("registry.") {
        bail!(
            "library name `{name}` is reserved: the docs cache keeps its reference registry \
             at <cache>/registry.* and a library directory there would shadow it"
        );
    }
    representable(&encode(name))
}

/// Takes the *raw* ref (`release/2.x`), never the encoded directory name.
pub fn validate_ref(git_ref: &str) -> Result<()> {
    if git_ref.is_empty() {
        bail!("ref is empty");
    }
    if git_ref.contains('~') {
        bail!("`{git_ref}` contains `~`, which is illegal in a git ref name");
    }
    reject_traversal(git_ref)?;
    let dir = encode(git_ref);
    if CHECKOUT_RESERVED.iter().any(|r| fold_key(r) == fold_key(&dir)) {
        bail!("ref `{git_ref}` collides with a control file inside the library directory");
    }
    representable(&dir)
}

pub fn lib_dir(name: &str) -> Result<String> {
    validate_lib(name)?;
    Ok(encode(name))
}

pub fn checkout_dir(git_ref: &str) -> Result<String> {
    validate_ref(git_ref)?;
    Ok(encode(git_ref))
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
Expected: PASS, 10 tests.

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
reports by propagating with `?`: `src/bin/docm.rs:228`, `:255`, `:291`, `:364`
and `crates/devkit-docs/src/resolve.rs`.

- [ ] **Step 5b: Add the enumeration constructor and the exact-bytes creator**

`LibCache::new` now *validates a logical name*. Two call sites do not have one —
they have an encoded directory name read from `read_dir`, and `@types~node`
would be rejected for containing `~`. They need the unvalidated constructor:

```rust
    /// From an already-encoded directory name, as read from the cache root.
    /// Skips validation: the name came from disk, not from a user.
    pub fn from_dir(cache_root: &Path, dirname: &str) -> Self {
        Self { dir: cache_root.join(dirname) }
    }
```

Convert `crates/devkit-docs/src/refs.rs:187-192` (the cache-root scan) and
`src/bin/docm.rs:364` (`remove_worktree`) to `from_dir`, and use
`names::decode(&dirname)` wherever the *logical* name is needed for display or
for a manifest lookup. Getting this backwards is what breaks prune.

Also in `cache.rs`:

```rust
/// Create `parent/name` and prove the host stored that exact name. A
/// case-folding or Unicode-normalizing filesystem silently aliases the new
/// directory onto an existing one; the listing check is what makes that visible.
pub fn create_dir_exact(parent: &Path, name: &str) -> Result<PathBuf> {
    let path = parent.join(name);
    std::fs::create_dir_all(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    let stored = std::fs::read_dir(parent)?
        .flatten()
        .any(|e| e.file_name().to_string_lossy() == name);
    if !stored {
        let existing: Vec<String> = std::fs::read_dir(parent)?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| crate::names::fold_key(n) == crate::names::fold_key(name))
            .collect();
        bail!(
            "this filesystem folds `{name}` onto {existing:?}; docm cannot keep them apart — \
             rename the library or pin a ref whose name does not collide"
        );
    }
    Ok(path)
}
```

- [ ] **Step 5c: Route every directory creation through `create_dir_exact`**

`create_dir_exact` is worthless unless it is the only way a cache directory
comes into being. Replace every `create_dir_all` that creates a *named* cache
directory:

```rust
    /// The library's own directory, proven to be stored under the exact name.
    pub fn ensure_dir(&self, cache_root: &Path, dirname: &str) -> Result<PathBuf> {
        create_dir_exact(cache_root, dirname)
    }
```

Call it from `ensure_clone` before cloning and from `write_meta` in place of its
`create_dir_all`. `git worktree add` creates the checkout directory itself, so
`ensure_at` cannot route through `create_dir_exact`; instead, immediately after a
successful `worktree add`, assert the parent lists the exact `dirname` and error
with the same message if it does not. A folded checkout name is how two refs
would come to share one directory — the precise failure §1 exists to prevent.

`registry.locks` and the lock files inside it are created by `locks::hold`'s
plain `create_dir_all`, which is correct: they are ASCII control names that
never derive from user input.

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

Returning empty rather than `Err` is deliberate: this is the prune path, and a
stray directory must make prune do *less*, never fail. But silence is what let
the 0.12.1 bug run unnoticed, so `plan_for_cache` (Task 7) reports each cache-root
entry it skipped for having no `repo.git`, and `devkit doctor` (Task 11) lists
them. The safe behaviour stays; the invisibility does not.

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

Collision detection must run over *every* entry, so it cannot stop at the first
invalid name: `a~b` is rejected by `validate_lib`, but the error the test demands
names both `a~b` and `a/b`, and that pairing is only visible once both have been
examined. Collect, then report.

```rust
    let mut problems: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for l in &manifest.libs {
        // Folded, because two entries differing only in case are one directory
        // on macOS and Windows.
        let dir = crate::names::encode(&l.name);
        if let Some(other) = seen.insert(crate::names::fold_key(&dir), l.name.clone())
            && other != l.name
        {
            problems.push(format!(
                "`{other}` and `{}` both map to the cache directory `{dir}`",
                l.name
            ));
        }
        if let Err(e) = crate::names::validate_lib(&l.name) {
            problems.push(format!("library `{}`: {e}", l.name));
        }
    }
    if !problems.is_empty() {
        bail!("the docs manifest is not usable:\n  {}", problems.join("\n  "));
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
- Modify: `crates/devkit-docs/tests/cache.rs:83` — **required in this commit**

**Interfaces:**
- Produces: `tags::ALL` ordered package-specific first; `tags::TagPattern::{PkgAt, LeafAt, PkgDashV, LeafDashV, LeafDash, V, Plain}`.

The old variants `NameDash`, `NameDashV` and `NameAt` are **removed**, and
`crates/devkit-docs/tests/cache.rs:83` still names `TagPattern::NameDash`. That
line becomes `TagPattern::LeafDash` in this same commit — otherwise the tree does
not compile at this task's boundary and the merge gate fails. `tests/resolve.rs:45`
uses `TagPattern::V`, which survives unchanged.

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
git add crates/devkit-docs/src/tags.rs crates/devkit-docs/src/resolve.rs \
        crates/devkit-docs/tests/cache.rs
git commit -m "fix(docs): probe package-specific tags before generic ones"
```

---

### Task 4: Per-library lock and atomic writes

§9. Land before anything adds new writers to `meta.toml`.

**Files:**
- Create: `crates/devkit-docs/src/locks.rs`
- Create: `crates/devkit-docs/src/barrier.rs` — the one test-rendezvous module
  Tasks 7 and 11 both use
- Create: `crates/devkit-docs/tests/concurrency.rs`
- Modify: `crates/devkit-docs/src/cache.rs` (`write_meta`)
- Modify: `crates/devkit-docs/src/manifest.rs` (`upsert_global`, `upsert_project`, `remove_global`, `remove_project` — all four gain `cache_root`)
- Modify: `src/bin/docm.rs` — the callers those signatures break
- Modify: `crates/devkit-docs/src/lib.rs`

**Interfaces:**
- Produces:
  - `locks::with_lib<T>(cache_root: &Path, lib: &str, f: impl FnOnce() -> Result<T>) -> Result<T>`
  - `locks::with_lib_dir<T>(cache_root: &Path, dirname: &str, f: impl FnOnce() -> Result<T>) -> Result<T>` — locks by *encoded directory name*, for the upgrade pass
  - `locks::with_manifest<T>(cache_root: &Path, f: impl FnOnce() -> Result<T>) -> Result<T>` — one lock over every manifest read-modify-write
  - `locks::lock_path(cache_root: &Path, lib: &str) -> Result<PathBuf>` — `<cache>/registry.locks/<encoded>.lock`
  - `locks::manifest_lock_path(cache_root: &Path) -> PathBuf` — `<cache>/registry.locks/manifest.lock`
  - `locks::is_control(component: &str) -> bool` — true for `registry` / `registry.*`, so prune, doctor and the upgrade pass can skip them

**Two locks, because they protect different things.** The library lock serializes
one library's clone/fetch/materialize/registry-commit. It does *not* protect the
manifest: `docm add a` and `docm add b` take different library locks and would
still perform overlapping read-modify-write cycles on the one `docs.toml`,
and the later writer's atomic rename would drop the earlier one's entry. Atomic
writes prevent a torn file, not a lost update. The manifest lock is global
because the manifest is a single file shared by every library.

**Neither lock is reentrant.** See Global Constraints. Every consumer below is
split so the lock is taken exactly once at the outermost layer.

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
use fd_lock::RwLock;
use std::fs::{self, OpenOptions};
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

/// Beside the other lock files rather than beside `docs.toml`: the reserved
/// `registry.` stem already keeps prune, doctor and the upgrade pass off it,
/// and it needs no path accessor that does not already exist.
pub fn manifest_lock_path(cache_root: &Path) -> PathBuf {
    cache_root.join(DIR).join("manifest.lock")
}

fn hold<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    fs::create_dir_all(path.parent().expect("lock path has a parent"))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut lock = RwLock::new(file);
    let _held = lock
        .write()
        .with_context(|| format!("locking {}", path.display()))?;
    f()
}

pub fn with_lib<T>(cache_root: &Path, lib: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    hold(&lock_path(cache_root, lib)?, f)
}

/// The manifest is one file shared by every library, so its read-modify-write
/// needs a lock of its own — a per-library lock lets two adds interleave and
/// lose one entry. One lock covers the global `docs.toml` and every project
/// `devkit.toml`: a merged view reads all of them, so serializing the whole set
/// is both correct and simpler than per-file locks.
pub fn with_manifest<T>(cache_root: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    hold(&manifest_lock_path(cache_root), f)
}
```

`_held` must stay bound for the body's whole scope — `let _ = lock.write()` drops
the guard immediately and releases the lock. Never `truncate(true)`: another
process may hold this same file.

- [ ] **Step 3b: Add the shared test barrier and the contention probe**

Three tasks place rendezvous points (Tasks 7 and 11's race tests, and
`resolve_locked`). They all use one module rather than three ad-hoc `env::var`
reads, because getting the variable name wrong is silent — a parent waits
forever on a file no code writes.

Create `crates/devkit-docs/src/barrier.rs`:

```rust
//! Test-only rendezvous. Every function is a no-op unless `VAR` is set, so a
//! production run pays one environment lookup and nothing else.
//!
//! Files are `<base>.<suffix>`, where `<base>` is the variable's value. A
//! bounded wait is mandatory: an unbounded one turns a logic error in a test
//! into a hung CI job with no output.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const VAR: &str = "DEVKIT_DOCS_MANIFEST_BARRIER";
const TIMEOUT: Duration = Duration::from_secs(60);

fn base() -> Option<PathBuf> {
    std::env::var_os(VAR).map(PathBuf::from)
}

pub fn signal(suffix: &str) -> Result<()> {
    let Some(b) = base() else { return Ok(()) };
    let p = b.with_extension(suffix);
    std::fs::create_dir_all(p.parent().expect("barrier path has a parent"))?;
    std::fs::write(&p, "").with_context(|| format!("signalling {}", p.display()))
}

pub fn wait(suffix: &str) -> Result<()> {
    let Some(b) = base() else { return Ok(()) };
    let p = b.with_extension(suffix);
    let deadline = Instant::now() + TIMEOUT;
    while !p.exists() {
        if Instant::now() > deadline {
            bail!("barrier timed out after {TIMEOUT:?} waiting for {}", p.display());
        }
        std::thread::yield_now();
    }
    Ok(())
}
```

Add `pub mod barrier;` to `lib.rs`. `resolve_locked`'s hook (Task 7) becomes
`barrier::signal("ready")?` then `barrier::wait("go")?`, replacing the inline
`DEVKIT_DOCS_BARRIER` sketch — one variable, one module.

Then give `hold` a contention probe, which is what makes a lock's *absence*
observable:

```rust
    let mut lock = RwLock::new(file);
    // When the barrier variable is set, a failed non-blocking attempt proves
    // another process holds this lock *right now*. That is the one fact a race
    // test cannot otherwise establish: every other signal proves only that a
    // contender started. Costs one `env::var_os` miss in production.
    if std::env::var_os(crate::barrier::VAR).is_some() {
        match lock.try_write() {
            // Held by someone else *right now* — the only outcome that means
            // contention. `fd-lock` maps Unix `flock` conflicts and Windows
            // ERROR_LOCK_VIOLATION to `WouldBlock`.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                crate::barrier::signal("contended")?;
            }
            // `Interrupted` and every other IO error say nothing about who
            // holds the lock; treating them as contention writes a false
            // `.contended` and makes the race tests pass on a fluke.
            Err(e) => {
                return Err(e).with_context(|| format!("probing {}", path.display()));
            }
            Ok(guard) => drop(guard),
        }
    }
    let _held = lock
        .write()
        .with_context(|| format!("locking {}", path.display()))?;
    f()
```

`lock.try_write().is_err()` drops its temporary guard at the end of the
statement, so the following `lock.write()` borrows freely. Binding the guard
instead — `match lock.try_write() { Ok(g) => …, Err(_) => lock.write()? }` —
does not compile: the `Ok` arm holds a mutable borrow of `lock` that the `Err`
arm's `write()` needs.

**The gap between the probe and the acquisition is real, and it is why the probe
is gated on the barrier variable.** When `try_write` *succeeds*, the lock is
released and retaken, and another process can take it in between. Two things
keep that from mattering:

- In production the variable is unset, so no probe runs and there is no gap.
- In the tests, the parent waits for the adder's `.ready` — written from inside
  `resolve_locked` while the adder already holds the lock — *before* spawning
  the contender. The contender therefore never exists during the adder's probe
  window. This ordering is load-bearing: spawning both children at once would
  make the probe able to invert the acquisition order, and the parent would then
  wait forever for a `.contended` that the wrong process was positioned to write.

Do not use this probe outside a test rendezvous, and do not reorder the tests'
spawns.

Add to `crates/devkit-docs/Cargo.toml`:

```toml
fd-lock.workspace = true
```

`fd-lock = "4"` is already declared at `Cargo.toml:69` and used by
`devkit-common`, `devkit-locks` and `devkit-ports`. Do not add `fs2`: a second
locking crate means two sets of flock semantics across three platforms, and
`fs2`'s `unlock` is now shadowed by `std::fs::File`'s inherent method of the
same name, so `file.unlock()` silently calls std's. `fd-lock`'s RAII guard also
ties the lock's lifetime to a binding, which manual lock/unlock pairs do not.
Add `pub mod locks;` to `lib.rs`.

Confirm `devkit_common::paths::config_dir` is the accessor this repo uses for
`~/.config/devkit`; if it is named differently, use whatever `manifest.rs`
already calls to locate `docs.toml` rather than inventing a second path source.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-docs --test concurrency`
Expected: PASS, 2 tests.

- [ ] **Step 5: Make `write_meta` atomic**

In `crates/devkit-docs/src/cache.rs`:

Task 1 Step 5c already replaced this function's `create_dir_all` with
`create_dir_exact`; keep that. Only the write becomes atomic here:

```rust
pub fn write_meta(lib_dir: &Path, m: &Meta) -> Result<()> {
    // Task 1: the directory is created by `create_dir_exact` at the point the
    // library is first materialized, not re-created here.
    let path = lib_dir.join("meta.toml");
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(m)?).context("writing meta.toml")?;
    std::fs::rename(&tmp, &path).context("replacing meta.toml")?;
    Ok(())
}
```

- [ ] **Step 6: Make manifest writes atomic *and* serialized**

Apply the same write-temp-then-rename to the file write inside
`manifest::upsert_global`, `upsert_project`, `remove_global` and
`remove_project`. Atomicity alone is not enough: each of those functions reads
the file, edits the parsed document and writes it back, so wrap the whole
read-modify-write of each in `locks::with_manifest`.

**All four gain a `cache_root: &Path` parameter**, because the manifest lock
lives at `<cache>/registry.locks/manifest.lock` and none of them can currently
name it:

```rust
pub fn upsert_global(path: &Path, entry: &LibEntry, cache_root: &Path) -> Result<()>
pub fn upsert_project(path: &Path, entry: &LibEntry, cache_root: &Path) -> Result<()>
pub fn remove_global(path: &Path, name: &str, cache_root: &Path) -> Result<bool>
pub fn remove_project(path: &Path, name: &str, cache_root: &Path) -> Result<bool>
```

Do **not** reach for a process-global `docs_root()` instead. Every test would
then share one manifest lock, so an unrelated test running in parallel could
produce the `.contended` file the race tests wait on — turning the lock-removed
build green and destroying the only evidence those tests provide. The lock root
must be the same per-test temporary directory the rest of the fixture uses.

Update and stage every caller in this commit — the compiler lists them; expect
`src/bin/docm.rs` (`cmd_add`, `cmd_rm`, `cmd_sync`) and `manifest.rs`'s own test
module. Task 11's `rm_library` and `docm_add` helper pass their `cache_root`
straight through.

Add the regression test to `crates/devkit-docs/tests/concurrency.rs`, using the
process-spawn harness in `crates/devkit-docs/tests/refs_race.rs`:

```rust
#[test]
fn concurrent_adds_of_different_libraries_both_survive() {
    // N child processes each call manifest::upsert_global for a distinct name
    // against one docs.toml. Afterwards every name must be present: an
    // unserialized read-modify-write keeps only the last writer's entry.
    // Spawn pattern: tests/refs_race.rs.
}
```

- [ ] **Step 7: Run the gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
# manifest.rs's four mutators gained a cache_root parameter, so every caller
# moves with them or this boundary does not compile.
git add crates/devkit-docs/src/locks.rs crates/devkit-docs/src/barrier.rs \
        crates/devkit-docs/src/lib.rs \
        crates/devkit-docs/src/cache.rs crates/devkit-docs/src/manifest.rs \
        src/bin/docm.rs \
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
  - `LibCache::resolve_ref(&self, r: &str) -> Result<(String, String)>` — returns `(canonical_ref, commit)`; fetches once and retries before failing
  - `LibCache::ensure_at(&self, dirname: &str, commit: &str) -> Result<(PathBuf, bool)>` — materialize, re-point on HEAD mismatch; the `bool` is "had to repair"
  - `LibCache::assert_clean(&self, path: &Path) -> Result<()>`
  - `resolve::Resolved` gains `git_ref: String`, `commit: String`, `status: Status`, `origin: String`
  - `resolve::Status { Ok, Repaired }`
  - `resolve::resolve(...)` takes the library lock; `resolve::resolve_locked(...)` assumes it is already held

**The split matters.** `docm add` and `docm sync` (Task 11) take the library lock
themselves so the manifest write and the materialization land under one hold.
They must call `resolve_locked`. `resolve` is the entry point for everything
else and is the *only* function that wraps `resolve_locked` in `locks::with_lib`.
Calling `resolve` from inside a held library lock deadlocks — `fd-lock` is not
reentrant, and the second acquisition blocks on the first forever.

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
    ///
    /// A ref pushed upstream after the last fetch is not in the local repo yet,
    /// so a miss triggers exactly one fetch and one retry before it is an error
    /// (§ spec: "a miss triggers exactly one fetch, then a retry, then a hard
    /// error"). Without it, every newly published tag fails on first use.
    pub fn resolve_ref(&self, r: &str) -> Result<(String, String)> {
        if let Some(found) = self.try_resolve_ref(r)? {
            return Ok(found);
        }
        self.fetch()?;
        self.try_resolve_ref(r)?
            .with_context(|| format!("`{r}` does not resolve to a commit, even after fetching"))
    }

    /// `Ok(None)` means "not present locally"; `Err` means ambiguous, which a
    /// fetch cannot fix and which must not be retried.
    fn try_resolve_ref(&self, r: &str) -> Result<Option<(String, String)>> {
        let peel = |name: &str| -> Option<String> {
            cmd::git(
                &["rev-parse", "--verify", "--end-of-options", &format!("{name}^{{commit}}")],
                &self.bare_str(),
            )
            .ok()
            .map(|s| s.trim().to_string())
        };
        if r.starts_with("refs/") {
            return Ok(peel(r).map(|c| (r.to_string(), c)));
        }
        if r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit())
            && let Some(c) = peel(r)
        {
            return Ok(Some((r.to_string(), c)));
        }
        let tag = peel(&format!("refs/tags/{r}"));
        let head = peel(&format!("refs/heads/{r}"));
        match (tag, head) {
            (Some(_), Some(_)) => bail!(
                "`{r}` is both a tag and a branch; pin it as refs/tags/{r} or refs/heads/{r}"
            ),
            (Some(c), None) => Ok(Some((format!("refs/tags/{r}"), c))),
            (None, Some(c)) => Ok(Some((format!("refs/heads/{r}"), c))),
            (None, None) => Ok(None),
        }
    }

    /// Materialize at `commit`, re-pointing an existing worktree that drifted.
    /// Returns whether it had to repair.
    pub fn ensure_at(&self, dirname: &str, commit: &str) -> Result<(PathBuf, bool)> {
        let path = self.worktree_path(dirname);
        // Checked on *both* branches. Checking only after `worktree add` leaves
        // a bypass: on a normalizing host the add succeeds, the check fails, and
        // the folded checkout stays registered — so the next call takes the
        // existing-directory branch, never re-checks, and returns the very
        // checkout the error was meant to prevent.
        let stored = |lib: &Self| -> Result<bool> {
            Ok(std::fs::read_dir(&lib.dir)?
                .flatten()
                .any(|e| e.file_name().to_string_lossy() == dirname))
        };
        if !path.is_dir() {
            let p = path.to_string_lossy().into_owned();
            cmd::git(
                &["worktree", "add", "--detach", p.as_str(), commit],
                &self.bare_str(),
            )
            .with_context(|| format!("materializing {dirname} at {commit}"))?;
            // `worktree add` creates the directory itself, so this cannot route
            // through `create_dir_exact`. The check it performs still must
            // happen: a host that folds `V1.0` onto `v1.0` would otherwise let
            // two refs share one checkout, which is the failure §1 exists to
            // prevent.
            if !stored(self)? {
                // Leave nothing registered behind, or the retry hits the
                // existing-directory branch and the error becomes unreachable.
                let _ = cmd::git(&["worktree", "remove", "--force", &p], &self.bare_str());
                let _ = cmd::git(&["worktree", "prune"], &self.bare_str());
                bail!(
                    "this filesystem did not store the checkout under `{dirname}`; \
                     it folds onto an existing directory and the two refs would share one checkout"
                );
            }
            return Ok((path, false));
        }
        if !stored(self)? {
            bail!(
                "`{dirname}` is not stored under that exact name; this filesystem folds it \
                 onto another checkout and the two refs would share one directory"
            );
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
    let previous = meta.worktrees.get(&dirname).cloned();
    if let Some(prev) = &previous
        && prev.resolved_ref != canonical
    {
        bail!(
            "`{git_ref}` previously resolved to {} and now resolves to {canonical}; \
             the pin changed kind upstream — re-pin it explicitly",
            prev.resolved_ref
        );
    }
    if let Some(prev) = &previous
        && prev.commit != commit
        && canonical.starts_with("refs/tags/")
    {
        eprintln!(
            "docm: tag {git_ref} moved {} → {commit} upstream; {dirname} re-pointed",
            prev.commit
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
    let status = if repaired { Status::Repaired } else { Status::Ok };
```

`previous` is cloned up front because the moved-tag report in Step 7 and the
kind-change guard both read it, and both must see the pre-update value —
`WorktreeMeta` derives `Clone` for exactly this. `status` flows into the
returned `Resolved`, which is what the `Status::Repaired` assertion reads.

`default_worktree` and every `ensure_worktree("default", …)` call site are deleted.

Wrap the whole body as `resolve_locked`, and add the thin outer entry point:

```rust
pub fn resolve(entry: &LibEntry, start: &Path, cache_root: &Path) -> Result<Resolved> {
    locks::with_lib(cache_root, &entry.name, || {
        resolve_locked(entry, start, cache_root)
    })
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p devkit-docs --test resolve`
Expected: PASS.

- [ ] **Step 7: Add `--prune-tags` and test the moved and deleted tag paths**

In `LibCache::fetch`, add `--prune-tags` to the argument list. The moved-tag
report itself is already in Step 5's body. Append the tests that prove both
directions:

```rust
#[test]
fn a_tag_moved_upstream_is_seen_on_sync_not_on_a_plain_resolve() {
    let base = common::unique_tmp("movedtag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let first = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();

    // Force-move the tag upstream onto the second commit.
    devkit_common::cmd::capture("git", &["tag", "-f", "v1.0.0", "v1.1.0"], Some(&repo)).unwrap();

    // `resolve` reads the local repo and finds v1.0.0 already present, so it
    // does not fetch and cannot see the move. That is the intended boundary:
    // fetching on every resolve would put a network call on the hot path.
    let second = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();
    assert_eq!(second.commit, first.commit, "resolve must not fetch for a ref it already has");

    // Sync is the operation that goes to the network, and it is where the move
    // is detected and the checkout re-pointed.
    let lib = devkit_docs::cache::LibCache::new(&cache, "up").unwrap();
    lib.fetch().unwrap();
    let third = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();
    assert_ne!(third.commit, first.commit, "after a fetch the checkout follows the moved tag");
    assert_eq!(third.path, first.path, "the ref names the directory, so it is reused");
}

#[test]
fn a_tag_deleted_upstream_is_a_hard_error_after_prune_tags() {
    let base = common::unique_tmp("deltag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    devkit_common::cmd::capture("git", &["tag", "v2.0.0", "v1.1.0"], Some(&repo)).unwrap();
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v2.0.0".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();

    // Withdrawn upstream. `--prune-tags` must remove it locally, and a pin to a
    // tag that no longer exists must fail rather than silently keep serving the
    // old checkout as if it were still released.
    devkit_common::cmd::capture("git", &["tag", "-d", "v2.0.0"], Some(&repo)).unwrap();
    let lib = devkit_docs::cache::LibCache::new(&cache, "up").unwrap();
    lib.fetch().unwrap();
    assert!(
        devkit_common::cmd::capture(
            "git", &["rev-parse", "--verify", "refs/tags/v2.0.0"],
            Some(lib.bare().to_str().unwrap()),
        ).is_err(),
        "--prune-tags must delete the local tag"
    );

    // The point of pruning the tag is that the pin then fails. Stopping at the
    // rev-parse check would leave the actual promise — a hard error rather than
    // a stale checkout served as if still released — untested.
    let err = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap_err().to_string();
    assert!(err.contains("v2.0.0"), "the error must name the withdrawn ref: {err}");
}

#[test]
fn a_pin_that_changes_from_a_tag_to_a_branch_is_refused() {
    let base = common::unique_tmp("kindchange");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    devkit_common::cmd::capture("git", &["tag", "release", "v1.0.0"], Some(&repo)).unwrap();
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("release".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();

    // Upstream retires the tag and publishes a branch of the same name. The
    // recorded `resolved_ref` was refs/tags/release; silently following the
    // branch would change what the pin means without saying so.
    devkit_common::cmd::capture("git", &["tag", "-d", "release"], Some(&repo)).unwrap();
    devkit_common::cmd::capture("git", &["branch", "release", "v1.1.0"], Some(&repo)).unwrap();
    let lib = devkit_docs::cache::LibCache::new(&cache, "up").unwrap();
    lib.fetch().unwrap();

    let err = devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap_err().to_string();
    assert!(err.contains("refs/tags/release"), "the error must name the previous kind: {err}");
}

#[test]
fn a_ref_published_after_the_last_fetch_resolves_without_a_manual_sync() {
    let base = common::unique_tmp("newtag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let mut entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    devkit_docs::resolve::resolve(&entry, &base, &cache).unwrap();

    // Published upstream only after the clone — the local repo has never seen it.
    devkit_common::cmd::capture("git", &["tag", "v3.0.0", "v1.1.0"], Some(&repo)).unwrap();

    entry.r#ref = Some("v3.0.0".into());
    let r = devkit_docs::resolve::resolve(&entry, &base, &cache)
        .expect("a miss must fetch once and retry before failing");
    assert!(r.path.ends_with("v3.0.0"));
}
```

`common::fixture_repo` must therefore build a repo with at least two tagged
commits (`v1.0.0` → `// v1`, `v1.1.0` → `// v2`) and return a path usable as
both a clone source and a `cwd` for further `git` calls. Extend it if it does
not already; every test in this task depends on that shape.

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
- Modify: `Cargo.toml`, `crates/devkit-docs/Cargo.toml`, `Cargo.lock` — the
  `jsonc-parser` dependency `bun.lock` parsing needs
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
    "apps/api": { "name": "@app/api", "dependencies": { "h3": "^1.15.5" } },
    "apps/web": { "name": "@app/web", "dependencies": {} }
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

    // `apps/web` must exist in bun.lock's workspaces, or this errors on the
    // missing workspace entry and never reaches the transitive check.
    let err = importers::select(&ws, Ecosystem::Js, "h3").unwrap_err().to_string();
    assert!(err.contains("does not declare"), "{err}");
    assert!(err.contains("--ref"), "the error must suggest a pin: {err}");
    assert!(err.contains("1.15.11"), "the error must list candidate versions: {err}");
    // The declarer must be whoever has an edge to h3 — `apps/api` — not h3's
    // own lockfile key, which would read "required by h3".
    assert!(err.contains("declared by: apps/api"), "the error must name the declarer: {err}");

    // BUN_LOCK holds three h3 versions and apps/api's edge resolves exactly
    // one. Attributing all three to apps/api states two falsehoods, so versions
    // and declarers are reported as separate lists and never cross-paired.
    assert!(err.contains("2.0.1"), "every candidate version is listed: {err}");
    for bogus in ["2.0.1 (required by apps/api)", "2.0.1-rc.20 (required by apps/api)"] {
        assert!(!err.contains(bogus), "versions must not be paired with declarers: {err}");
    }
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
    // The app depends on httpx, and a marker fork records two resolutions of
    // it. Without the dependency edge this fixture would prove nothing — the
    // error has to come from the package the workspace actually declares.
    std::fs::write(
        root.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
    { name = "httpx" },
]

[[package]]
name = "httpx"
version = "0.27.0"

[[package]]
name = "httpx"
version = "0.28.1"
"#,
    )
    .unwrap();
    std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"app\"\ndependencies = [\"httpx\"]\n").unwrap();

    let err = importers::select(&root, Ecosystem::Python, "httpx").unwrap_err().to_string();
    assert!(err.contains("0.27.0") && err.contains("0.28.1"), "{err}");
}

#[test]
fn a_uv_dev_dependency_resolves_like_a_runtime_one() {
    let root = common::unique_tmp("uv-dev");
    std::fs::write(
        root.join("uv.lock"),
        r#"version = 1

[[package]]
name = "app"
version = "0.1.0"

[package.dev-dependencies]
dev = [
    { name = "pytest" },
]

[[package]]
name = "pytest"
version = "8.3.2"
"#,
    )
    .unwrap();
    std::fs::write(root.join("pyproject.toml"), "[project]\nname = \"app\"\n").unwrap();

    assert_eq!(importers::select(&root, Ecosystem::Python, "pytest").unwrap().version, "8.3.2");
}

#[test]
fn a_cargo_member_gets_its_own_dependency_not_another_members() {
    let root = common::unique_tmp("cargo-ws");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"a\", \"b\"]\n").unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        r#"version = 4

[[package]]
name = "a"
version = "0.1.0"
dependencies = ["serde"]

[[package]]
name = "b"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.210"
"#,
    )
    .unwrap();
    for (m, dep) in [("a", "serde = \"1\"\n"), ("b", "")] {
        let d = root.join(m);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("Cargo.toml"), format!("[package]\nname = \"{m}\"\n\n[dependencies]\n{dep}")).unwrap();
    }

    assert_eq!(
        importers::select(&root.join("a"), Ecosystem::Rust, "serde").unwrap().version,
        "1.0.210"
    );
    // `b` does not depend on serde, even though the lockfile contains it.
    let err = importers::select(&root.join("b"), Ecosystem::Rust, "serde").unwrap_err().to_string();
    // `a`'s edge is a bare `"serde"` carrying no version, so there is no pair
    // to report. The real lockfile version must still be listed, and no
    // fabricated "unspecified" may stand in for it.
    assert!(err.contains("1.0.210"), "the lockfile version must be listed: {err}");
    assert!(err.contains("declared by: a"), "the declarer must be named: {err}");
    assert!(!err.contains("unspecified"), "no invented version: {err}");
}

#[test]
fn a_cargo_edge_naming_its_version_wins_over_a_duplicate_in_the_lockfile() {
    let root = common::unique_tmp("cargo-dup");
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"app\"\n\n[dependencies]\nserde = \"1\"\n").unwrap();
    // Two serde versions, and the edge from `app` names exactly one. Reducing
    // the edge to a bare name would report an unresolvable fork here.
    std::fs::write(
        root.join("Cargo.lock"),
        r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "serde 1.0.210 (registry+https://github.com/rust-lang/crates.io-index)",
]

[[package]]
name = "serde"
version = "1.0.210"

[[package]]
name = "serde"
version = "0.9.15"
"#,
    )
    .unwrap();

    assert_eq!(importers::select(&root, Ecosystem::Rust, "serde").unwrap().version, "1.0.210");
}

#[test]
fn npm_resolves_the_nearest_nested_copy_walking_up_from_the_workspace() {
    let root = common::unique_tmp("npm-nested");
    std::fs::write(
        root.join("package-lock.json"),
        r#"{
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "root" },
    "apps/api": { "name": "@app/api", "dependencies": { "h3": "^1.0.0" } },
    "apps/api/node_modules/h3": { "version": "1.15.11" },
    "node_modules/h3": { "version": "2.0.1" }
  }
}"#,
    )
    .unwrap();
    let ws = root.join("apps/api");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("package.json"), r#"{"name":"@app/api","dependencies":{"h3":"^1.0.0"}}"#).unwrap();

    // The hoisted root copy is 2.0.1; the workspace runs its own nested 1.15.11.
    assert_eq!(importers::select(&ws, Ecosystem::Js, "h3").unwrap().version, "1.15.11");
}

#[test]
fn a_pnpm_alias_locator_resolves_to_the_aliased_packages_version() {
    let root = common::unique_tmp("pnpm-alias");
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    dependencies:\n      h3-v2:\n        specifier: npm:h3@2.0.1\n        version: h3@2.0.1\n",
    )
    .unwrap();
    std::fs::write(root.join("package.json"), r#"{"name":"root","dependencies":{"h3-v2":"npm:h3@2.0.1"}}"#).unwrap();

    // Declared under the alias, so `h3` itself is not a declared dependency.
    assert!(importers::select(&root, Ecosystem::Js, "h3").is_err());
    assert_eq!(importers::select(&root, Ecosystem::Js, "h3-v2").unwrap().version, "2.0.1");
}

#[test]
fn a_dev_dependency_resolves_in_every_js_format() {
    let root = common::unique_tmp("js-dev");
    std::fs::write(
        root.join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\nimporters:\n  .:\n    devDependencies:\n      vitest:\n        specifier: ^3.2.0\n        version: 3.2.4\n",
    )
    .unwrap();
    std::fs::write(root.join("package.json"), r#"{"name":"root","devDependencies":{"vitest":"^3.2.0"}}"#).unwrap();

    assert_eq!(importers::select(&root, Ecosystem::Js, "vitest").unwrap().version, "3.2.4");
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
use anyhow::{Context, Result, bail};
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

Shared helpers — every format needs them:

```rust
/// Nearest ancestor of `start` (inclusive) containing `file`.
fn find_up(start: &Path, file: &str) -> Option<PathBuf> {
    start.ancestors().find(|d| d.join(file).is_file()).map(Path::to_path_buf)
}

/// Lockfile-relative workspace key: "apps/api", or "" for the lockfile root.
/// pnpm writes "." for the root importer; callers normalize.
fn rel_key(lock_dir: &Path, ws: &Path) -> Result<String> {
    let rel = ws.strip_prefix(lock_dir)
        .with_context(|| format!("{} is not under {}", ws.display(), lock_dir.display()))?;
    Ok(rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn undeclared(ws: &Path, package: &str, c: &Candidates) -> anyhow::Error {
    // `resolved` augments the version list, it does not replace it: an edge
    // without a version contributes a declarer and no pair, and suppressing the
    // package rows would then print nothing.
    let pairs = if c.resolved.is_empty() {
        String::new()
    } else {
        format!(
            "\nresolved edges: {}",
            c.resolved
                .iter()
                .map(|(v, by)| format!("{v} (required by {by})"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let versions = if c.versions.is_empty() {
        "none".to_string()
    } else {
        c.versions
            .iter()
            .map(|(v, at)| format!("{v} (at {at})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let declarers = if c.declarers.is_empty() {
        "nothing in the lockfile declares it".to_string()
    } else {
        c.declarers.join(", ")
    };
    anyhow::anyhow!(
        "{} does not declare `{package}` (it is transitive); pin the version with --ref.\n\
         versions present in the lockfile: {versions}\n\
         declared by: {declarers}{pairs}",
        ws.display()
    )
}
```

`declarers` is built by scanning for entries whose dependency map names
`package`. For cargo and uv the edge *does* carry a version, so those two report
`{version} (required by {member})` pairs — `dep_edge` already returns both, and
that pairing is read from the lockfile rather than inferred.

**`js`** — locate the workspace, then the lockfile, then choose among lockfiles:

```rust
fn js(start: &Path, package: &str) -> Result<Selection> {
    let ws = find_up(start, "package.json")
        .with_context(|| format!("no package.json at or above {}", start.display()))?;
    const LOCKS: [(&str, &str); 3] =
        [("bun", "bun.lock"), ("pnpm", "pnpm-lock.yaml"), ("npm", "package-lock.json")];
    let lock_dir = ws
        .ancestors()
        .find(|d| LOCKS.iter().any(|(_, f)| d.join(f).is_file()))
        .with_context(|| format!("no JS lockfile at or above {}", ws.display()))?
        .to_path_buf();

    let present: Vec<&(&str, &str)> =
        LOCKS.iter().filter(|(_, f)| lock_dir.join(f).is_file()).collect();
    let chosen = match present.as_slice() {
        [] => unreachable!("lock_dir was chosen because one exists"),
        [only] => **only,
        _ => {
            // `packageManager` may sit on the workspace or on any package.json
            // up to the lockfile root; the nearest declaration wins.
            let pm = ws
                .ancestors()
                .take_while(|d| d.starts_with(&lock_dir) || *d == lock_dir)
                .filter_map(|d| std::fs::read_to_string(d.join("package.json")).ok())
                .filter_map(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .find_map(|v| v.get("packageManager")?.as_str().map(str::to_string));
            let name = pm
                .as_deref()
                .map(|p| p.split('@').next().unwrap_or(p).to_string());
            match name.and_then(|n| present.iter().find(|(m, _)| *m == n).copied()) {
                Some(hit) => *hit,
                None => {
                    // Naming the versions each lockfile resolves to is the point:
                    // it shows the reader whether the ambiguity actually matters.
                    let each: Vec<String> = present
                        .iter()
                        .map(|(m, f)| {
                            let got = match m {
                                &"bun" => bun(&lock_dir, &ws, &rel_key(&lock_dir, &ws)?, package),
                                &"pnpm" => pnpm(&lock_dir, &ws, &rel_key(&lock_dir, &ws)?, package),
                                _ => npm(&lock_dir, &ws, &rel_key(&lock_dir, &ws)?, package),
                            };
                            Ok(match got {
                                Ok(s) => format!("{f} → {}", s.version),
                                Err(e) => format!("{f} → {e}"),
                            })
                        })
                        .collect::<Result<_>>()?;
                    bail!(
                        "{} holds {} and no `packageManager` field says which one governs: {}; \
                         add \"packageManager\" to package.json",
                        lock_dir.display(),
                        present.iter().map(|(_, f)| *f).collect::<Vec<_>>().join(" and "),
                        each.join("; ")
                    )
                }
            }
        }
    };

    let rel = rel_key(&lock_dir, &ws)?;
    match chosen.0 {
        "bun" => bun(&lock_dir, &ws, &rel, package),
        "pnpm" => pnpm(&lock_dir, &ws, &rel, package),
        _ => npm(&lock_dir, &ws, &rel, package),
    }
}

const DEP_CLASSES: [&str; 4] =
    ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"];

/// The key the workspace declares this package under. An alias is declared under
/// its alias, so looking up the declared key is what keeps `h3-v2` from
/// answering a query for `h3`.
fn declared_key(entry: &serde_json::Value, package: &str) -> Option<String> {
    DEP_CLASSES
        .iter()
        .filter_map(|c| entry.get(c)?.as_object())
        .find(|o| o.contains_key(package))
        .map(|_| package.to_string())
}
```

**bun** — `workspaces.<rel>` declares, `packages` resolves:

```rust
fn bun(lock_dir: &Path, ws: &Path, rel: &str, package: &str) -> Result<Selection> {
    let raw = std::fs::read_to_string(lock_dir.join("bun.lock"))?;
    // bun.lock is JSONC: trailing commas and // comments are legal.
    let v: serde_json::Value = json5_ish(&raw).context("parsing bun.lock")?;
    let entry = v.get("workspaces").and_then(|w| w.get(rel))
        .with_context(|| format!("bun.lock has no workspace entry for `{rel}`"))?;
    let key = declared_key(entry, package)
        .ok_or_else(|| undeclared(ws, package, &bun_candidates(&v, package)))?;

    let pkgs = v.get("packages").and_then(|p| p.as_object())
        .context("bun.lock has no `packages` table")?;
    let name = entry.get("name").and_then(|n| n.as_str()).unwrap_or_default();
    let scoped = format!("{name}/{key}");
    let row = pkgs.get(&scoped).or_else(|| pkgs.get(&key))
        .with_context(|| format!("bun.lock has no package row for `{key}`"))?;
    let spec = row.get(0).and_then(|s| s.as_str())
        .context("a bun package row starts with its `name@version` spec")?;
    let version = spec.rsplit_once('@').map(|(_, v)| v)
        .with_context(|| format!("`{spec}` is not a name@version spec"))?;
    Ok(Selection {
        workspace: ws.to_path_buf(),
        version: version.to_string(),
        source: format!("{rel} installs it (bun.lock)"),
    })
}

/// (version, dependent) pairs for the undeclared error.
///
/// The dependent is whoever has a dependency *edge* to `package` — not the
/// package's own lockfile key. Reading the key gives `1.15.11 required by h3`,
/// which names the package as its own dependent and tells the reader nothing.
///
/// What the lockfile knows about a package the workspace does not declare.
///
/// Versions and declarers are kept apart on purpose. Pairing them would mean
/// claiming which declarer resolves to which version, and that edge is not
/// available here: a bun workspace or an npm `dependencies` object records a
/// *range*, not a resolved version. Attributing all three `h3` versions to
/// `apps/api` because it declares `h3` is a fabricated relationship — it reads
/// as fact and is wrong for every version but one.
#[derive(Default)]
pub struct Candidates {
    /// (version, where the lockfile holds it)
    pub versions: Vec<(String, String)>,
    /// Owners with a dependency edge to the package.
    pub declarers: Vec<String>,
    /// (version, dependent) for formats whose dependency edge carries the
    /// resolved version — cargo, uv and pnpm. Read from the lockfile, never
    /// inferred, so it is safe to print as `required by`. Empty for bun and
    /// npm, whose edges record a range.
    pub resolved: Vec<(String, String)>,
}

/// Owners with an edge to `package`, across both a bun/npm workspace table
/// (deps under the dependency-class keys) and a package table (bun keeps the
/// dependency map at index 2 of the tuple, npm under `dependencies`).
fn declarers_in(
    table: Option<&serde_json::Map<String, serde_json::Value>>,
    package: &str,
    deps_of: fn(&serde_json::Value) -> Vec<&serde_json::Map<String, serde_json::Value>>,
) -> Vec<String> {
    table
        .map(|t| {
            t.iter()
                .filter(|(_, r)| deps_of(r).iter().any(|d| d.contains_key(package)))
                .map(|(k, _)| if k.is_empty() { "<root>".to_string() } else { k.clone() })
                .collect()
        })
        .unwrap_or_default()
}

fn class_deps(r: &serde_json::Value) -> Vec<&serde_json::Map<String, serde_json::Value>> {
    DEP_CLASSES.iter().filter_map(|c| r.get(c)?.as_object()).collect()
}

fn bun_candidates(v: &serde_json::Value, package: &str) -> Candidates {
    let pkgs = v.get("packages").and_then(|p| p.as_object());
    let versions = pkgs
        .map(|o| {
            o.iter()
                .filter_map(|(k, r)| {
                    let (n, ver) = r.get(0)?.as_str()?.rsplit_once('@')?;
                    (n == package).then(|| (ver.to_string(), k.clone()))
                })
                .collect()
        })
        .unwrap_or_default();

    // A workspace that declares it, or a package whose own dependency map
    // (tuple index 2) names it.
    let mut declarers =
        declarers_in(v.get("workspaces").and_then(|w| w.as_object()), package, class_deps);
    declarers.extend(declarers_in(pkgs, package, |r| {
        r.get(2).map(class_deps).unwrap_or_default()
    }));
    Candidates { versions, declarers, ..Default::default() }
}

fn json_candidates(
    pkgs: &serde_json::Map<String, serde_json::Value>,
    package: &str,
) -> Candidates {
    let suffix = format!("node_modules/{package}");
    Candidates {
        versions: pkgs
            .iter()
            .filter(|(k, _)| k.ends_with(&suffix))
            .filter_map(|(k, r)| Some((r.get("version")?.as_str()?.to_string(), k.clone())))
            .collect(),
        declarers: declarers_in(Some(pkgs), package, class_deps),
        ..Default::default()
    }
}
```

`bun.lock` is JSONC — `//` comments, `/* block comments */` and trailing commas
are all legal, and `serde_json` rejects every one of them.

**Use `jsonc-parser`, not a hand-rolled scanner.** Add to
`crates/devkit-docs/Cargo.toml` and the workspace `[workspace.dependencies]`:

```toml
jsonc-parser = { version = "0.26", features = ["serde"] }
```

```rust
fn json5_ish(src: &str) -> Result<serde_json::Value> {
    jsonc_parser::parse_to_serde_value(src, &Default::default())
        .context("parsing bun.lock as JSONC")?
        .context("bun.lock is empty")
}
```

The earlier draft of this plan hand-wrote the stripper because of a
no-new-dependency rule that this repo does not actually have. That scanner had
two bugs before it ever ran: a comma followed by a comment then a closer was
kept, and block comments were not handled at all — `/* … */` survived into the
output and also defeated the trailing-comma lookahead. Both are cases a JSONC
parser has already solved. Keep the tests below regardless; they now pin the
dependency's behaviour rather than ours:

```rust
#[test]
fn jsonc_handles_urls_trailing_commas_and_both_comment_forms() {
    let v = json5_ish(r#"{ "url": "https://example.com/x", "a": [1, 2,], }"#).unwrap();
    assert_eq!(v["url"], "https://example.com/x");
    assert_eq!(v["a"].as_array().unwrap().len(), 2);

    let v = json5_ish("{\n  \"a\": 1, // note\n}").unwrap();
    assert_eq!(v["a"], 1);

    // Block comment between a trailing comma and its closer — the shape that
    // defeated the hand-written lookahead.
    let v = json5_ish("{\n  \"a\": 1, /* note */ }").unwrap();
    assert_eq!(v["a"], 1);

    let v = json5_ish("{ /* lead */ \"a\": [1, 2 /* tail */ ] }").unwrap();
    assert_eq!(v["a"].as_array().unwrap().len(), 2);
}
```

Confirm the crate version and that `parse_to_serde_value` is behind its `serde`
feature before writing the manifest line; if the API differs, use whatever it
exposes that returns a `serde_json::Value`. 


**pnpm** — the importer graph, with peer qualifiers and aliases:

```rust
fn pnpm(lock_dir: &Path, ws: &Path, rel: &str, package: &str) -> Result<Selection> {
    let v: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(lock_dir.join("pnpm-lock.yaml"))?)
            .context("parsing pnpm-lock.yaml")?;
    let key = if rel.is_empty() { "." } else { rel };
    let imp = v.get("importers").and_then(|i| i.get(key))
        .with_context(|| format!("pnpm-lock.yaml has no importer `{key}`"))?;
    // Dependents come from real edges: other importers that declare it, plus
    // any snapshot/package whose own dependency map names it. Scanning only
    // importers misses the transitive case, which is exactly the case this
    // error is raised for.
    let mut elsewhere = Candidates::default();
    for table in ["importers", "snapshots", "packages"] {
        let Some(m) = v.get(table).and_then(|t| t.as_mapping()) else {
            continue;
        };
        for (who, row) in m {
            let Some(who) = who.as_str() else { continue };
            for c in ["dependencies", "devDependencies", "optionalDependencies"] {
                let Some(entry) = row.get(c).and_then(|d| d.get(package)) else {
                    continue;
                };
                elsewhere.declarers.push(who.to_string());
                // pnpm is the one JS format whose edge carries the resolved
                // locator, so here the (version, owner) pairing is read from the
                // lockfile rather than inferred. An importer nests it under
                // `version`; a snapshot maps the name straight to it.
                if let Some(loc) = entry
                    .get("version")
                    .and_then(|s| s.as_str())
                    .or_else(|| entry.as_str())
                {
                    let bare = loc.split_once('(').map_or(loc, |(h, _)| h);
                    elsewhere.resolved.push((
                        bare.rsplit_once('@').map_or(bare, |(_, x)| x).to_string(),
                        who.to_string(),
                    ));
                }
            }
        }
    }
    let found = ["dependencies", "devDependencies", "optionalDependencies"]
        .iter()
        .find_map(|c| imp.get(*c)?.get(package))
        .ok_or_else(|| undeclared(ws, package, &elsewhere))?;
    let locator = found.get("version").and_then(|s| s.as_str())
        .context("an importer entry carries a `version` locator")?;

    // `3.2.4(@types/node@25.5.0)` -> `3.2.4`; `h3@2.0.1` (alias) -> `2.0.1`.
    let bare = locator.split_once('(').map_or(locator, |(head, _)| head);
    let version = bare.rsplit_once('@').map_or(bare, |(_, v)| v);
    Ok(Selection {
        workspace: ws.to_path_buf(),
        version: version.to_string(),
        source: format!("{key} installs it (pnpm-lock.yaml)"),
    })
}
```

**npm** — declared by the workspace, resolved at the nearest `node_modules`:

```rust
fn npm(lock_dir: &Path, ws: &Path, rel: &str, package: &str) -> Result<Selection> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(lock_dir.join("package-lock.json"))?)
            .context("parsing package-lock.json")?;
    let pkgs = v.get("packages").and_then(|p| p.as_object())
        .context("package-lock.json has no `packages` table")?;
    let entry = pkgs.get(rel)
        .with_context(|| format!("package-lock.json has no entry for `{rel}`"))?;
    declared_key(entry, package)
        .ok_or_else(|| undeclared(ws, package, &json_candidates(pkgs, package)))?;

    // Nearest wins: apps/api/node_modules/h3 before the hoisted node_modules/h3.
    let mut dir = rel.to_string();
    loop {
        let probe = if dir.is_empty() {
            format!("node_modules/{package}")
        } else {
            format!("{dir}/node_modules/{package}")
        };
        if let Some(row) = pkgs.get(&probe)
            && let Some(ver) = row.get("version").and_then(|s| s.as_str())
        {
            return Ok(Selection {
                workspace: ws.to_path_buf(),
                version: ver.to_string(),
                source: format!("{rel} installs it via {probe} (package-lock.json)"),
            });
        }
        match dir.rsplit_once('/') {
            Some((head, _)) => dir = head.to_string(),
            None if dir.is_empty() => break,
            None => dir = String::new(),
        }
    }
    bail!("package-lock.json resolves no `{package}` reachable from `{rel}`")
}
```

**cargo** and **uv** share a shape — a `[[package]]` array where the member's own
entry lists its dependencies:

```rust
#[derive(serde::Deserialize)]
struct LockPkg {
    name: String,
    version: String,
    #[serde(default)]
    dependencies: Vec<toml::Value>,
}

/// A dependency edge, as name plus the version the edge pins if it carries one.
///
/// Cargo writes `"serde"`, `"serde 1.0.210"`, or
/// `"serde 1.0.210 (registry+https://github.com/rust-lang/crates.io-index)"`;
/// uv writes `{ name = "httpx" }` or `{ name = "httpx", version = "0.28.1" }`.
/// Keeping the version is what stops a duplicated package name from reading as
/// an unresolvable fork when the edge already names the exact one.
fn dep_edge(v: &toml::Value) -> Option<(&str, Option<&str>)> {
    if let Some(s) = v.as_str() {
        let mut parts = s.split_whitespace();
        let name = parts.next()?;
        return Some((name, parts.next()));
    }
    let name = v.get("name")?.as_str()?;
    Some((name, v.get("version").and_then(|x| x.as_str())))
}

fn from_package_array(
    lock: &Path, ws: &Path, member: &str, package: &str, extra_deps: &[String],
) -> Result<Selection> {
    #[derive(serde::Deserialize)]
    struct Lock { #[serde(default, rename = "package")] packages: Vec<LockPkg> }
    let l: Lock = toml::from_str(&std::fs::read_to_string(lock)?)
        .with_context(|| format!("parsing {}", lock.display()))?;

    let own = l.packages.iter().find(|p| p.name == member)
        .with_context(|| format!("{} has no `[[package]]` for `{member}`", lock.display()))?;

    let pinned: Option<&str> = own
        .dependencies
        .iter()
        .filter_map(dep_edge)
        .find(|(n, _)| *n == package)
        .and_then(|(_, v)| v);
    let declares = own.dependencies.iter().filter_map(dep_edge).any(|(n, _)| n == package)
        || extra_deps.iter().any(|n| n == package);

    let versions: Vec<String> =
        l.packages.iter().filter(|p| p.name == package).map(|p| p.version.clone()).collect();
    if !declares {
        let by: Vec<(String, String)> = l
            .packages
            .iter()
            .flat_map(|p| {
                p.dependencies
                    .iter()
                    .filter_map(dep_edge)
                    .filter(|(n, _)| *n == package)
                    // Only an edge that names a version becomes a pair. A
                    // bare `"serde"` edge has none, and inventing
                    // "unspecified" would both state a falsehood and suppress
                    // the real lockfile versions below.
                    .filter_map(move |(_, v)| Some((v?.to_string(), p.name.clone())))
            })
            .collect();
        // Three separate facts, none inferred from another:
        //   versions   — every `[[package]]` row for this name
        //   declarers  — every member with an edge to it, versioned or not
        //   resolved   — only the edges that carry a version
        // Populating `resolved` alone loses both others, and a lockfile whose
        // edges are all bare (`dependencies = ["serde"]`, which is what cargo
        // writes when the name is unambiguous) would then report nothing at all.
        return Err(undeclared(
            ws,
            package,
            &Candidates {
                versions: l
                    .packages
                    .iter()
                    .filter(|p| p.name == package)
                    .map(|p| (p.version.clone(), lock.display().to_string()))
                    .collect(),
                declarers: l
                    .packages
                    .iter()
                    .filter(|p| p.dependencies.iter().filter_map(dep_edge).any(|(n, _)| n == package))
                    .map(|p| p.name.clone())
                    .collect(),
                resolved: by,
            },
        ));
    }

    // The edge names the exact version, so duplicates in the lockfile are not
    // an ambiguity for *this* member.
    if let Some(want) = pinned
        && versions.iter().any(|v| v == want)
    {
        return Ok(Selection {
            workspace: ws.to_path_buf(),
            version: want.to_string(),
            source: format!("{member} depends on {package} {want} ({})", lock.display()),
        });
    }

    match versions.as_slice() {
        [] => bail!("{} declares `{package}` but the lockfile records no version", ws.display()),
        [one] => Ok(Selection {
            workspace: ws.to_path_buf(),
            version: one.clone(),
            source: format!("{member} depends on it ({})", lock.display()),
        }),
        many => bail!(
            "{} records {} versions of `{package}` ({}) and the dependency edge from \
             `{member}` does not name one; a resolution fork cannot be disambiguated \
             from the lockfile — pin one with --ref",
            lock.display(), many.len(), many.join(", ")
        ),
    }
}
```

The entry points, in full:

```rust
fn cargo(start: &Path, package: &str) -> Result<Selection> {
    let ws = find_up(start, "Cargo.toml")
        .with_context(|| format!("no Cargo.toml at or above {}", start.display()))?;
    let member: toml::Value = toml::from_str(&std::fs::read_to_string(ws.join("Cargo.toml"))?)
        .with_context(|| format!("parsing {}", ws.join("Cargo.toml").display()))?;
    let name = member.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str())
        .with_context(|| format!("{} has no [package] name", ws.join("Cargo.toml").display()))?;
    let lock_dir = find_up(&ws, "Cargo.lock")
        .with_context(|| format!("no Cargo.lock at or above {}", ws.display()))?;
    from_package_array(&lock_dir.join("Cargo.lock"), &ws, name, package, &[])
}

fn uv(start: &Path, package: &str) -> Result<Selection> {
    let ws = find_up(start, "pyproject.toml")
        .with_context(|| format!("no pyproject.toml at or above {}", start.display()))?;
    let proj: toml::Value = toml::from_str(&std::fs::read_to_string(ws.join("pyproject.toml"))?)
        .with_context(|| format!("parsing {}", ws.join("pyproject.toml").display()))?;
    let name = proj.get("project").and_then(|p| p.get("name")).and_then(|n| n.as_str())
        .with_context(|| format!("{} has no [project] name", ws.join("pyproject.toml").display()))?;
    let lock_dir = find_up(&ws, "uv.lock")
        .with_context(|| format!("no uv.lock at or above {}", ws.display()))?;
    let lock = lock_dir.join("uv.lock");

    // uv keeps dev and optional groups in their own tables on the member's
    // entry, outside `dependencies`, so they are gathered separately and passed
    // in as extra declared names.
    let raw: toml::Value = toml::from_str(&std::fs::read_to_string(&lock)?)?;
    let mut extra: Vec<String> = Vec::new();
    if let Some(pkgs) = raw.get("package").and_then(|p| p.as_array()) {
        for p in pkgs.iter().filter(|p| p.get("name").and_then(|n| n.as_str()) == Some(name)) {
            for table in ["dev-dependencies", "optional-dependencies"] {
                if let Some(groups) = p.get(table).and_then(|g| g.as_table()) {
                    for list in groups.values().filter_map(|g| g.as_array()) {
                        extra.extend(
                            list.iter().filter_map(dep_edge).map(|(n, _)| n.to_string()),
                        );
                    }
                }
            }
        }
    }
    from_package_array(&lock, &ws, name, package, &extra)
}
```

`cargo` reads `package.name` from the nearest `Cargo.toml` and calls
`from_package_array` with the nearest `Cargo.lock` and no extras. `uv` reads
`project.name` from the nearest `pyproject.toml`, collects names out of the
`[package.dev-dependencies]` and `[package.optional-dependencies]` tables of the
member's own entry into `extra_deps`, and calls the same function with `uv.lock`.

Each helper returns `Err` for every shape it does not recognize. Nothing in this
module may guess: an unparsed field is an error, never a fallback.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-docs --test importers`
Expected: PASS, 12 tests — one per format rule, plus the negative and ambiguity cases.

- [ ] **Step 5: Call it from `resolve`, and fix the fixtures that assumed the old selector**

Replace the `lockfiles::find_version` + `highest` block in `resolve::resolve`
with `importers::select`, print `selection.source` on stderr, and use
`selection.workspace` as the reference-registry key (Task 7).

Range-matching needed only the dependency's `[[package]]` row; importer-graph
resolution also needs *the member's own row and its edge*. Every existing
fixture in `crates/devkit-docs/tests/resolve.rs` that writes a bare `Cargo.lock`
therefore stops resolving, and fails in selection rather than where its
assertion lives. Update each to add a `[package] name` in its `Cargo.toml` and
the matching `[[package]]` entry with a `dependencies` edge in its `Cargo.lock`,
and stage `tests/resolve.rs` with this commit. Run
`cargo test -p devkit-docs --test resolve` before committing: any test failing
with "has no `[[package]]` for" is a fixture this step missed.

- [ ] **Step 6: Run the gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/importers.rs crates/devkit-docs/src/lib.rs \
        crates/devkit-docs/src/lockfiles.rs crates/devkit-docs/src/resolve.rs \
        crates/devkit-docs/tests/importers.rs crates/devkit-docs/tests/resolve.rs \
        Cargo.toml crates/devkit-docs/Cargo.toml Cargo.lock
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
- Produces:
  - `RefRow { project, lib, version, git_ref: String, commit: String, resolved_at }`, `#[serde(default)]` on `git_ref` and `commit` so a 0.12.x `registry.json` still deserializes
  - `Data::record(&mut self, workspace: &str, lib: &str, dirname: &str, git_ref: &str, commit: &str)` — `dirname` is stored in the `version` field, which now holds the checkout directory name rather than a bare version
  - `Data::record_legacy(&mut self, project: &str, lib: &str, version: &str)`
  - `Data::retire_legacy(&mut self, workspace: &str, lib: &str)`
  - `refs::row_dirname(row: &RefRow) -> String` — the checkout directory a row points at
  - `refs::plan_for_cache(cache_root: &Path, data: &Data) -> Result<Vec<Removal>>` — was `plan`; now takes the registry explicitly so a caller holding the library lock can pass the snapshot it read *inside* the hold
  - `refs::prune_with_lock(cache_root: &Path) -> Result<Vec<String>>` — per library: take the lock, re-read the registry, plan, remove
  - `Removal { pub lib_dir: String, pub checkout: String }`, with `Display` for the returned lines

**Callers this task must update — all of them, in this commit:**
`crates/devkit-docs/src/resolve.rs:113` is the only production call, but
`refs.rs` has seven in its own `#[cfg(test)]` module (lines 260-262, 273,
287-289, 326, 332-333, 357-358, 363-364). Every one takes three arguments today.
Leaving any behind fails the merge gate at this task's boundary.

The `version` field keeps its name (renaming it would break the on-disk schema)
but changes meaning: it now stores the checkout dirname. `row_dirname` exists so
no caller re-derives it — the old `current_version` computed `"default"` from an
entry, and that name no longer describes what it returns.

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
    assert_eq!(devkit_docs::refs::row_dirname(&d.rows[0]), "default");

    d.record("/repo/apps/api", "h3", "v1.15.11", "v1.15.11", "aaa");
    d.retire_legacy("/repo/apps/api", "h3");
    assert_eq!(d.rows.len(), 1);
    assert_eq!(d.rows[0].version, "v1.15.11");
}

#[test]
fn prune_never_enumerates_a_control_entry_as_a_library() {
    let root = common::unique_tmp("control");
    std::fs::create_dir_all(root.join("registry.locks")).unwrap();
    std::fs::write(root.join("registry.json"), "{}").unwrap();
    let plan = devkit_docs::refs::plan_for_cache(&root, &devkit_docs::refs::Data::default())
        .unwrap();
    assert!(
        plan.is_empty(),
        "the registry's own files are not unreferenced libraries: {plan:?}"
    );
}
```

Append to `crates/devkit-docs/tests/concurrency.rs`. This is the decisive test
for §9 — write it in full, not as a sketch. `tests/refs_race.rs` already spawns
the current test binary as a child with an env marker; copy that harness:

The window this test must hit is *inside* `resolve_locked`: after the checkout
exists on disk but before its reference row is committed. Neither a sleep nor
watching the lock file can hit it reliably — `locks::hold` creates the lock file
*before* acquiring the lock, so the file appearing proves nothing, and a fixed
200 ms hold can expire before a loaded CI runner gets prune started. Both are
also exactly the fixed-interval sleeping `AGENTS.md` forbids.

So add a deterministic, test-only barrier to `resolve_locked`, in the same
env-driven style the daemon already uses for `DEVKIT_DAEMON_HEALTH_PROBE_SECS`:

```rust
`resolve_locked` uses the shared module from Task 4 Step 3b — one variable, one
implementation, one bounded timeout:

```rust
    crate::barrier::signal("ready")?;
    crate::barrier::wait("go")?;
```

Call it in `resolve_locked` between `assert_clean` and the reference-registry
commit. `prune_with_lock` gets the mirror-image hook: with
`barrier::VAR` set, `prune_with_lock` takes its pre-lock registry snapshot and
then acquires the library lock — and `locks::hold`'s contention probe writes
`.contended` when that acquisition finds the lock already held. No separate
`.planned` signal: reaching the lock is not the same as being stopped by it, and
only the second is observable proof that prune took a lock at all.

**Run these against the unfixed code before implementing.** A concurrency test
that has never been observed failing is not evidence of anything. With the
per-library lock removed from `prune_with_lock`, this test must fail — prune
plans from the stale snapshot and deletes the child's checkout. Confirm that,
then restore the lock and confirm it passes.

Then:

```rust
/// The child re-enters this same test binary, so the two contenders are
/// genuinely separate processes holding separate file descriptions — two
/// threads would share one and never contend.
#[test]
fn prune_cannot_delete_a_directory_a_concurrent_resolve_just_materialized() {
    let root = common::unique_tmp("race");
    let repo = common::fixture_repo(&root.join("src"));
    let cache = root.join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let barrier = root.join("barrier");

    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "race_child_materializes_and_waits", "--ignored"])
        .env("DOCM_RACE_CACHE", &cache)
        .env("DOCM_RACE_REPO", &repo)
        .env(devkit_docs::barrier::VAR, &barrier)
        .spawn()
        .unwrap();

    // `.ready` is written only after the child holds the library lock AND has
    // materialized the checkout — the precise state prune must not act on.
    let ready = barrier.with_extension("ready");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !ready.exists() {
        assert!(std::time::Instant::now() < deadline, "child never materialized");
        std::thread::yield_now();
    }

    // Prune now runs against a cache holding an unreferenced-looking checkout.
    // It must block on the library lock rather than plan from a stale snapshot.
    // Prune runs as its own child, not a thread. `std::env::set_var` is unsafe
    // and process-global, and libtest runs tests in parallel threads — setting
    // the barrier variable in-process would leak into every concurrent test.
    // A child gets it at spawn, where it is scoped and safe.
    let mut pruner = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "race_child_prunes", "--ignored"])
        .env("DOCM_RACE_CACHE", &cache)
        .env(devkit_docs::barrier::VAR, &barrier)
        .env("DOCM_RACE_REMOVED", root.join("removed.json"))
        .spawn()
        .unwrap();

    // Wait for proof that prune is *blocked on the library lock*, not merely
    // that it started or that it finished planning. Releasing on a
    // "planning done" signal leaves the same hole the add/rm test had for four
    // rounds: prune can be preempted after signalling, the resolver commits its
    // row, and an unlocked prune then resumes, re-reads a registry that now
    // references the checkout, and passes without ever taking a lock.
    //
    // `.contended` comes from inside `locks::hold` and requires another process
    // to hold this lock at that instant, so a prune with no library lock cannot
    // produce it — the wait times out and the test fails. That is the RED.
    let contended = barrier.with_extension("contended");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !contended.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "prune never contended for the library lock — it is not taking one"
        );
        std::thread::yield_now();
    }

    std::fs::write(barrier.with_extension("go"), "").unwrap();
    assert!(child.wait().unwrap().success());
    assert!(pruner.wait().unwrap().success());
    let removed: Vec<String> =
        serde_json::from_str(&std::fs::read_to_string(root.join("removed.json")).unwrap()).unwrap();

    assert!(
        !removed.iter().any(|r| r.contains("v1.0.0")),
        "prune deleted a checkout a concurrent resolve had just materialized: {removed:?}"
    );
    assert!(cache.join("up").join("v1.0.0").is_dir());
}

/// The child half. `#[ignore]` keeps it out of the normal run; the parent
/// invokes it by name.
#[test]
#[ignore]
fn race_child_materializes_and_waits() {
    let cache = PathBuf::from(std::env::var("DOCM_RACE_CACHE").unwrap());
    let repo = std::env::var("DOCM_RACE_REPO").unwrap();
    let entry = devkit_docs::manifest::LibEntry {
        name: "up".into(),
        ecosystem: Some(devkit_docs::manifest::Ecosystem::Git),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    // Three arguments here. `Options` and the fourth parameter arrive in
    // Task 9, which stages this file in its own caller-table update — writing
    // the four-argument form now would not compile at Task 7's commit.
    devkit_docs::locks::with_lib(&cache, "up", || {
        devkit_docs::resolve::resolve_locked(&entry, &cache, &cache)
    })
    .unwrap();
}

/// The prune half, in its own process so the two library locks genuinely
/// contend. Results come back through a file because a child's return value
/// cannot.
#[test]
#[ignore]
fn race_child_prunes() {
    let cache = PathBuf::from(std::env::var("DOCM_RACE_CACHE").unwrap());
    let out = PathBuf::from(std::env::var("DOCM_RACE_REMOVED").unwrap());
    let removed = devkit_docs::refs::prune_with_lock(&cache).unwrap();
    std::fs::write(&out, serde_json::to_string(&removed).unwrap()).unwrap();
}
```

Both contenders are child processes. Nothing here uses `std::env::set_var`:
it is `unsafe` and process-global, and libtest runs tests on parallel threads,
so setting a barrier variable in-process would leak into whatever else is
running. Passing it at `Command::spawn` scopes it to the one child that needs it.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-docs --test prune && cargo test -p devkit-docs --test concurrency`
Expected: FAIL to compile — `record` takes three arguments, and
`prune_with_lock` does not exist. The `--test concurrency` run is not optional:
the race test added above lives there, and running only `--test prune` would
leave the task's decisive test unexecuted at its own RED step.

- [ ] **Step 3: Implement**

- `RefRow` gains `git_ref` and `commit`, both `#[serde(default)]`.
- `Data::record` takes the workspace path and the new fields; update the one
  production caller and all seven `refs.rs` test callers listed above.
- `Data::record_legacy` writes a row with an empty `commit` (test support and
  the read path for 0.12.x data).
- `refs::row_dirname` returns `row.version`, which is the dirname the resolver
  produced — and for a legacy row (empty `commit`) that value is `default`,
  so the same accessor covers both without a special case.
- `Data::retire_legacy(workspace, lib)` drops any row for `lib` whose project is
  an ancestor directory of `workspace` and whose `commit` is empty. Call it in
  the same registry commit that records the new row.
- Delete the `d != "default"` exemption in `plan`.
- `plan_for_cache` skips any cache-root entry where `locks::is_control(name)`,
  and builds its `LibCache` with `from_dir`, not `new` — the entry name is
  already encoded and `new` would reject the `~`.
- `cmd_prune` becomes `refs::prune_with_lock`, which for each library takes
  `locks::with_lib` and *then* re-reads the reference registry inside the hold
  before planning. Planning from a snapshot taken before the lock is what lets
  it delete a checkout a concurrent resolve committed in the gap. The recheck
  and `remove_worktree` both run under that same hold.

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

    // Capture what the worktree pointed at, so the assertion below is about
    // preservation rather than merely about the directory existing.
    let before_head = devkit_common::cmd::capture(
        "git", &["rev-parse", "HEAD"], Some(old.join("v1.0.0").to_str().unwrap()),
    ).unwrap().trim().to_string();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(!done.is_empty());

    let new = cache.join("@scope~pkg");
    assert!(new.is_dir());
    assert!(!cache.join("@scope").exists());

    // A clean status alone would also pass for a worktree pointing at the wrong
    // commit, so assert the exact HEAD.
    let after_head = devkit_common::cmd::capture(
        "git", &["rev-parse", "HEAD"], Some(new.join("v1.0.0").to_str().unwrap()),
    ).unwrap().trim().to_string();
    assert_eq!(after_head, before_head, "the migrated worktree must keep its commit");

    let status = devkit_common::cmd::capture(
        "git",
        &["status", "--porcelain"],
        Some(new.join("v1.0.0").to_str().unwrap()),
    )
    .unwrap();
    assert!(status.trim().is_empty());

    // The exact origin, not merely "some origin": a wrong URL here would make
    // Task 5's mismatch guard reject the library forever after.
    let meta = devkit_docs::cache::read_meta(&new);
    assert_eq!(meta.origin.as_deref(), Some(repo.as_str()));

    // Idempotent.
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}

#[test]
fn a_worktree_whose_link_cannot_be_repaired_is_rebuilt_at_its_recorded_commit() {
    let base = common::unique_tmp("upgrade-broken");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    let bare = old.join("repo.git");
    devkit_common::cmd::capture("git", &["clone", "--bare", &repo, bare.to_str().unwrap()], None).unwrap();
    devkit_common::cmd::capture(
        "git",
        &["worktree", "add", "--detach", old.join("v1.0.0").to_str().unwrap(), "v1.0.0"],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();
    let head = devkit_common::cmd::capture(
        "git", &["rev-parse", "HEAD"], Some(old.join("v1.0.0").to_str().unwrap()),
    ).unwrap().trim().to_string();

    // Break the worktree's link to its administrative directory beyond repair.
    std::fs::write(old.join("v1.0.0").join(".git"), "gitdir: /nonexistent/elsewhere").unwrap();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(done.iter().any(|l| l.contains("rebuilt")), "the rebuild must be reported: {done:?}");

    // Rebuilt, at the same commit, and usable — not left registered-but-absent
    // for prune to trip over.
    let new = cache.join("@scope~pkg").join("v1.0.0");
    assert_eq!(
        devkit_common::cmd::capture("git", &["rev-parse", "HEAD"], Some(new.to_str().unwrap()))
            .unwrap().trim(),
        head
    );
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty(), "still idempotent");
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-docs --test upgrade`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `upgrade::run`**

`locks::with_lib` validates a *logical* name, and a 0.12.x cache can hold
directories that fail that validation — including a literal `a~b`. Add the
sibling that locks by directory name instead, and use it here:

```rust
pub fn with_lib_dir<T>(cache_root: &Path, dirname: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    hold(&cache_root.join(DIR).join(format!("{dirname}{SUFFIX}")), f)
}
```

`upgrade::run` is a preflight-then-act state machine. It must never leave the
cache half-migrated, so **nothing moves until every rename is known to be safe**:

*Phase 1 — survey.* Enumerate cache-root entries, skipping `locks::is_control`.
Classify each: a directory containing `repo.git` is already a library; a
directory whose *children* contain `repo.git` is a nested 0.12.x scope;
anything else is left alone and reported.

*Phase 2 — preflight, no mutation.* Build the full list of
`<scope>/<pkg>` → `<scope>~<pkg>` renames. Refuse the whole run, changing
nothing, if any of:

- the target already exists — `@scope~pkg` may have been created by a newer
  docm before the old tree was migrated. Report both paths and stop; merging
  two caches is not something to guess at.
- two sources map to one target under `names::fold_key`, or a target folds onto
  an existing entry. On a case-folding host the second rename would silently
  land inside the first.
- the source name does not validate as a library once decoded.

Report every conflict in one error, not the first — an operator fixing these
wants the whole list.

*Phase 2b — record every HEAD, still without mutating.* Phase 3 needs each
worktree's commit to rebuild one whose administrative link cannot be repaired.
Read it from **the bare repository's worktree administration**, not from the
worktree:

```
<lib>/repo.git/worktrees/<name>/HEAD
```

`git -C <path> rev-parse HEAD` is the obvious way and the wrong one: it needs
the worktree's `.git` link to be intact, and the worktrees that most need their
commit captured are exactly the ones whose link is broken. The admin copy is
readable either way. Fall back to `git --git-dir <lib>/repo.git worktree list
--porcelain`, which reports each worktree's path and commit from the same
administration. A worktree with no readable admin entry is recorded as
unrecoverable and reported; it is not silently dropped.

**Persist the record before mutating anything.** Write it to
`<cache>/registry.locks/<encoded-lib>.migration.json` — under the reserved stem,
so `is_control` already keeps prune and doctor off it:

```json
{"worktrees": [{"dirname": "v1.0.0", "commit": "a1b2c3…"}]}
```

Holding these only in memory leaves a hole phase 5 cannot close. Phase 3's
rebuild removes the worktree and runs `git worktree prune`; a crash *between
that and `worktree add`* destroys both the directory and the admin entry, so the
next run sees no trace that the checkout ever existed — no directory to repair,
no entry to read a commit from. The name and commit were only ever in the
crashed process's memory. The journal is what survives that gap.

Delete the file once every worktree in it is present and resolving. Its presence
on startup means a previous run died mid-migration, and phase 5 reads it.

*Phase 3 — act, per library, under `with_lib_dir` on the target name.* For each
rename:

1. `fs::rename` the directory.
2. From the moved `repo.git`, run `git worktree repair <path>` for every
   worktree under it, then `git -C <path> rev-parse HEAD` and compare it to the
   commit captured in phase 2b. `worktree repair` exits 0 even where the
   worktree is unusable, so this comparison — not the exit code — is the check.
3. A worktree that fails that check is **fully unregistered and rebuilt before
   the lock is released**, so the pass never returns leaving a library in a
   state `resolve` would have to repair:
   - `git worktree remove --force <path>`; if that fails, `fs::remove_dir_all`
   - `git worktree prune` in the bare repo — mandatory, and the step most easily
     missed: `remove_dir_all` leaves `repo.git/worktrees/<name>/` behind, and a
     later `worktree add` for the same name then fails with "already registered"
   - `git worktree add --detach <path> <captured-commit>` to recreate it
   - record the rebuild in the returned lines

   Deferring this to `resolve` was wrong: prune runs before any resolve does, and
   a checkout that is registered-but-absent is exactly what it would misread.
4. Remove the emptied `<scope>` directory, ignoring `ENOTEMPTY` (a sibling
   scope member may not have been a library).

*Phase 5 — audit every already-encoded library, on every run.* For each library
directory that phase 1 classified as "already a library":

1. If a `<encoded-lib>.migration.json` journal exists, recreate every worktree
   it lists that is absent, at the commit it records, then delete the journal.
   This is the only way back from a crash between `worktree prune` and
   `worktree add`.
2. Check each present worktree resolves, and repair or rebuild exactly as
   phase 3 does.

This phase is what makes the forward-resume claim true, and it exists because
the claim was false without it. A crash between phase 3's `fs::rename` and its
`git worktree repair` leaves `@scope~pkg` present and containing `repo.git` —
so the next run's phase 1 classifies it as *already migrated*, phase 2 finds
nothing to rename, and the broken absolute worktree links are never touched
again. The cache is then permanently half-migrated with no operation that
notices. Phase 5 closes that by making repair unconditional rather than a
consequence of renaming.

With phase 5 there is deliberately **no rollback across renames.** Each
`fs::rename` is atomic, phase 2 has already proven every target free, `run` is
idempotent, and any state a crash can leave is one phase 5 repairs on the next
run. Unwinding completed renames would add a second failure path over one that
already recovers by re-running.

- [ ] **Step 3c: Test the resume that phase 5 exists for**

```rust
#[test]
fn a_crash_between_rename_and_repair_is_finished_by_the_next_run() {
    let base = common::unique_tmp("upgrade-resume");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    let bare = old.join("repo.git");
    devkit_common::cmd::capture("git", &["clone", "--bare", &repo, bare.to_str().unwrap()], None).unwrap();
    devkit_common::cmd::capture(
        "git",
        &["worktree", "add", "--detach", old.join("v1.0.0").to_str().unwrap(), "v1.0.0"],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();
    let head = devkit_common::cmd::capture(
        "git", &["rev-parse", "HEAD"], Some(old.join("v1.0.0").to_str().unwrap()),
    ).unwrap().trim().to_string();

    // Exactly the state a crash after `fs::rename` and before `worktree repair`
    // leaves: renamed, so it looks migrated, but every absolute link is stale.
    std::fs::rename(&old, cache.join("@scope~pkg")).unwrap();
    std::fs::remove_dir_all(cache.join("@scope")).ok();
    let moved = cache.join("@scope~pkg").join("v1.0.0");
    assert!(
        devkit_common::cmd::capture("git", &["status", "--porcelain"], Some(moved.to_str().unwrap())).is_err(),
        "the fixture must actually be broken, or this test proves nothing"
    );

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(!done.is_empty(), "phase 5 must repair a library that needs no rename");
    assert_eq!(
        devkit_common::cmd::capture("git", &["rev-parse", "HEAD"], Some(moved.to_str().unwrap()))
            .unwrap().trim(),
        head
    );
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty(), "and then settle");
}

#[test]
fn a_crash_between_worktree_prune_and_worktree_add_is_recovered_from_the_journal() {
    let base = common::unique_tmp("upgrade-journal");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    std::fs::create_dir_all(&lib).unwrap();
    let bare = lib.join("repo.git");
    devkit_common::cmd::capture("git", &["clone", "--bare", &repo, bare.to_str().unwrap()], None).unwrap();
    let head = devkit_common::cmd::capture(
        "git", &["rev-parse", "v1.0.0^{commit}"], Some(bare.to_str().unwrap()),
    ).unwrap().trim().to_string();

    // The state a crash after `worktree prune` and before `worktree add` leaves:
    // no directory, no admin entry, nothing on disk naming the checkout except
    // the journal the previous run wrote before it started mutating.
    let journal = cache.join("registry.locks").join("@scope~pkg.migration.json");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        format!(r#"{{"worktrees":[{{"dirname":"v1.0.0","commit":"{head}"}}]}}"#),
    )
    .unwrap();
    assert!(!lib.join("v1.0.0").exists());

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(done.iter().any(|l| l.contains("v1.0.0")), "the recovery must be reported: {done:?}");
    assert_eq!(
        devkit_common::cmd::capture(
            "git", &["rev-parse", "HEAD"], Some(lib.join("v1.0.0").to_str().unwrap()),
        ).unwrap().trim(),
        head
    );
    assert!(!journal.exists(), "the journal is cleared once its worktrees are back");
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}
```

*Phase 4, under the same per-library lock.* If `meta.toml` has no `origin`, read
`git config --get remote.origin.url` from its bare repo and write it via the
atomic `write_meta`. A bare repo with no origin is reported, not guessed at.
This runs inside the lock because it is a read-modify-write of `meta.toml`,
which `resolve` also writes.

Call `upgrade::run` once at the start of `docm`'s `main`, printing each returned
line to stderr. It returns `Ok(vec![])` when there was nothing to do, so the
common case costs one `read_dir`.

- [ ] **Step 3b: Test the states the survey has to distinguish**

Add to `crates/devkit-docs/tests/upgrade.rs`:

```rust
#[test]
fn a_rename_whose_target_already_exists_migrates_nothing() {
    let base = common::unique_tmp("upgrade-collide");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");

    // Old nested form and the new encoded form both present.
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    devkit_common::cmd::capture(
        "git", &["clone", "--bare", &repo, old.join("repo.git").to_str().unwrap()], None,
    ).unwrap();
    let new = cache.join("@scope~pkg");
    std::fs::create_dir_all(&new).unwrap();

    let err = devkit_docs::upgrade::run(&cache).unwrap_err().to_string();
    assert!(err.contains("@scope~pkg"), "the error must name the target: {err}");
    // Nothing moved: a refused migration leaves the cache exactly as it was.
    assert!(old.join("repo.git").is_dir());
}

#[test]
fn an_already_migrated_cache_is_left_alone() {
    let base = common::unique_tmp("upgrade-noop");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    std::fs::create_dir_all(&lib).unwrap();
    devkit_common::cmd::capture(
        "git", &["clone", "--bare", &repo, lib.join("repo.git").to_str().unwrap()], None,
    ).unwrap();

    // Only the origin backfill runs, and only once.
    assert_eq!(devkit_docs::upgrade::run(&cache).unwrap().len(), 1);
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}
```

- [ ] **Step 4: Run tests, gate, commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-docs/src/upgrade.rs crates/devkit-docs/src/lib.rs \
        crates/devkit-docs/src/locks.rs src/bin/docm.rs \
        crates/devkit-docs/tests/upgrade.rs
git commit -m "feat(docs): migrate 0.12 caches on first run"
```

---

### Task 9: Hard-error failure modes

§5. Land after Tasks 3 and 6, so every path that should succeed does.

**Files:**
- Modify: `crates/devkit-docs/src/resolve.rs`, `src/bin/docm.rs`
- Test: `crates/devkit-docs/tests/resolve.rs`, `crates/devkit-docs/tests/concurrency.rs`,
  `crates/devkit-docs/tests/upgrade.rs` — every caller in the table below

**Interfaces:**
- Produces: `resolve::Options { pub allow_default_branch: bool }`, deriving `Default`;
  `resolve::resolve(entry, start, cache_root, opts: &Options)` **and**
  `resolve::resolve_locked(entry, start, cache_root, opts: &Options)` — both gain
  the parameter, or the wrapper cannot pass it through.

**Callers to update and stage in this commit** — the signature change reaches
past this task's own files:

| File | What calls it |
|---|---|
| `crates/devkit-docs/tests/resolve.rs` | every test written in Task 5 |
| `crates/devkit-docs/tests/concurrency.rs` | the Task 7 race child |
| `crates/devkit-docs/tests/upgrade.rs` | any resolve after migration |
| `src/bin/docm.rs` | `cmd_add`, `cmd_sync`, `resolve_one` |

Passing `&Options::default()` is the correct update everywhere except `cmd_add`
and `cmd_sync`, which thread the parsed flag.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_version_with_no_tag_is_a_hard_error_listing_what_was_tried() {
    let base = common::unique_tmp("notag");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    // The lockfile needs `app`'s own entry with its edge to `up`, or importer
    // selection fails first and this test goes RED for the wrong reason —
    // proving nothing about the missing tag it exists to check.
    std::fs::write(
        base.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"up\",\n]\n\n[[package]]\nname = \"up\"\nversion = \"9.9.9\"\n",
    )
    .unwrap();
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
# Every file in the caller table above, or the committed snapshot keeps a
# three-argument `resolve_locked` call and does not compile.
git add crates/devkit-docs/src/resolve.rs src/bin/docm.rs \
        crates/devkit-docs/tests/resolve.rs \
        crates/devkit-docs/tests/concurrency.rs \
        crates/devkit-docs/tests/upgrade.rs
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

`detect` gains a `cwd` parameter, so update its caller in `src/bin/docm.rs`
(`cmd_add`) to pass the directory `add` was invoked from, and stage `docm.rs`
with this commit — otherwise the tree does not compile at this boundary.

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
- Modify: `crates/devkit-docs/src/lib.rs` — `rm_library`, the shared `rm`
  transaction `cmd_rm` and the race child both call
- Modify: `src/bin/devkit/` (doctor row)
- Test: `crates/devkit-docs/tests/concurrency.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `manifest::LibEntry` gains `#[serde(skip)] pub origin_file: Option<PathBuf>`.

- [ ] **Step 1: Give merged entries a provenance**

`manifest::discover` merges the global `docs.toml` and every `devkit.toml`
`[docs]` section into one `DocsManifest` (`manifest.rs:113-118`), and
`Discovered` records only `project_devkit_toml` — the *write target*, not where
each existing entry came from. `sync` cannot then tell whether a ref-less entry
is a global one it may backfill or a project one it must not touch.

Add a skipped field to `LibEntry` and stamp it as each layer loads:

```rust
    /// Which file this entry was read from. Set during `discover`, never
    /// serialized — writing it back would put an absolute path in a manifest
    /// that gets committed to a repo.
    #[serde(skip)]
    pub origin_file: Option<PathBuf>,
```

In `merge`, an overriding entry takes the overriding file's path. `#[serde(skip)]`
gives `None` on deserialize, so `Default` covers every existing construction
site — including all the test fixtures in the tasks above, which stay valid.

- [ ] **Step 2: Write the failing tests**

```rust
#[test]
fn a_failed_repin_leaves_the_previous_entry_intact() {
    let base = common::unique_tmp("repin-rollback");
    let repo = common::fixture_repo(&base.join("src"));
    let global = base.join("docs.toml");
    let cache = base.join("cache");

    docm_add(&global, &cache, &base, "up", &repo, Some("v1.0.0")).unwrap();
    let before = std::fs::read_to_string(&global).unwrap();

    docm_add(&global, &cache, &base, "up", &repo, Some("does-not-exist"))
        .expect_err("an unresolvable ref must fail the add");

    assert_eq!(
        std::fs::read_to_string(&global).unwrap(),
        before,
        "a failed re-pin must restore the previous entry byte for byte"
    );
}

#[test]
fn a_failed_add_of_a_new_library_leaves_the_manifest_byte_identical() {
    let base = common::unique_tmp("add-rollback");
    let repo = common::fixture_repo(&base.join("src"));
    let global = base.join("docs.toml");
    let cache = base.join("cache");

    docm_add(&global, &cache, &base, "keep", &repo, Some("v1.0.0")).unwrap();
    let before = std::fs::read_to_string(&global).unwrap();

    docm_add(&global, &cache, &base, "new", &repo, Some("does-not-exist"))
        .expect_err("an unresolvable ref must fail the add");

    assert_eq!(std::fs::read_to_string(&global).unwrap(), before);
    assert!(!before.contains("new"), "the failed entry must not survive");
}
```

`docm_add` is a test helper in `tests/common` performing exactly the Step 3
sequence — take the library lock once, then manifest write, then
`resolve::resolve_locked`, then rollback on error. Both tests fail today because
no rollback exists: the entry is written and left behind when resolution fails.

Add the cross-process test that the library lock over `rm` exists to satisfy:

Two things make this test real, and both were missing at first.

**The barrier has to sit between add's manifest *read* and its *write*.** Put it
where `add` already pauses — inside `resolve_locked`, after materialization —
and the manifest is already committed by then, so an unlocked `rm` still
observes the finished add and produces the same final state a locked one does.
The test then passes without the lock and proves nothing. `add`'s Step 3 order
is: snapshot (read) → write entry → resolve. So the child needs its own hook
*between the read and the write*, not the resolve-time one.

With the barrier there, the two outcomes separate cleanly:

| | rm reads | rm writes | final |
|---|---|---|---|
| `rm` takes the library lock | after add commits: `{keep, up}` | `{keep}` | `up` gone |
| `rm` unlocked | before add writes: `{keep}` | `{keep}` | add then writes its stale `{keep, up}` → **`up` survives** |

So "is `up` absent" is exactly the discriminator, and `keep` surviving catches
the lost update in the other direction.

```rust

```rust
#[test]
fn rm_blocks_until_a_concurrent_add_of_the_same_library_completes() {
    let base = common::unique_tmp("rm-add-race");
    let repo = common::fixture_repo(&base.join("src"));
    let global = base.join("docs.toml");
    let cache = base.join("cache");
    let barrier = base.join("barrier");
    docm_add(&global, &cache, &base, "keep", &repo, Some("v1.0.0")).unwrap();

    // The adder holds the library lock and pauses mid-transaction. `rm` must
    // block on that lock, so it can only observe the completed add — never the
    // half-written state, and never a manifest it rewrites from a stale read.
    let mut adder = spawn_child("child_add_up_and_wait", &global, &cache, &base, &repo, &barrier);
    let ready = barrier.with_extension("ready");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !ready.exists() {
        assert!(std::time::Instant::now() < deadline, "adder never reached the barrier");
        std::thread::yield_now();
    }

    let mut remover = spawn_child("child_rm_up", &global, &cache, &base, &repo, &barrier);

    // Wait for proof that the remover is *blocked on the library lock* — not
    // merely that it started. `.contended` is written from inside
    // `locks::hold` after a non-blocking acquisition fails, which can only
    // happen while the adder holds the same lock.
    //
    // Waiting on "the remover started" is not enough, and this is the exact
    // hole three earlier drafts of this test had: release `.go` then, and the
    // adder can finish its write before an unlocked remover does anything, so
    // the remover deletes `up` from a complete manifest and the locked and
    // unlocked builds agree. Contention is the only signal that distinguishes
    // them, and with `rm`'s lock removed it never arrives — the test fails on
    // this timeout, which is the RED this test exists to produce.
    let contended = barrier.with_extension("contended");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !contended.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the remover never contended for the library lock — it is not taking one"
        );
        std::thread::yield_now();
    }

    std::fs::write(barrier.with_extension("go"), "").unwrap();
    assert!(adder.wait().unwrap().success());
    assert!(remover.wait().unwrap().success());

    let d = devkit_docs::manifest::discover(&base, Some(&global)).unwrap();
    assert!(
        d.manifest.libs.iter().all(|l| l.name != "up"),
        "rm ran after the add completed, so `up` must be gone — finding it means \
         rm read the manifest before the add committed and wrote back a stale copy"
    );
    assert!(
        d.manifest.libs.iter().any(|l| l.name == "keep"),
        "the unrelated entry must survive: a lost update would drop it"
    );
}

/// Child: add `up`, pausing between the manifest read and the manifest write
/// while holding the library lock. `DEVKIT_DOCS_MANIFEST_BARRIER` is the hook
/// `add` consults at exactly that point; it is a no-op when unset.
#[test]
#[ignore]
fn child_add_up_and_wait() {
    let global = PathBuf::from(std::env::var("DOCM_RACE_GLOBAL").unwrap());
    let cache = PathBuf::from(std::env::var("DOCM_RACE_CACHE").unwrap());
    let base = PathBuf::from(std::env::var("DOCM_RACE_BASE").unwrap());
    let repo = std::env::var("DOCM_RACE_REPO").unwrap();
    docm_add(&global, &cache, &base, "up", &repo, Some("v1.0.0")).unwrap();
}

/// Child: remove `up` through the *production* path.
///
/// It calls `docm::rm_library`, the shared transaction `cmd_rm` also calls —
/// not a locking sequence rebuilt here. A child that took `with_lib` itself
/// would stay green with the production lock deleted, which is the opposite of
/// what this test is for.
#[test]
#[ignore]
fn child_rm_up() {
    let global = PathBuf::from(std::env::var("DOCM_RACE_GLOBAL").unwrap());
    let cache = PathBuf::from(std::env::var("DOCM_RACE_CACHE").unwrap());
    // No `.rm-started` write here — see below. The rendezvous is inside
    // `rm_library`, on the production path.
    devkit_docs::rm_library(
        devkit_docs::RmTarget::Global(&global),
        &cache,
        "up",
    )
    .unwrap();
}
```

`rm_library` lives in the library, not in `src/bin/docm.rs`, so both `cmd_rm`
and this child reach the same code:

```rust
pub enum RmTarget<'a> {
    Global(&'a Path),
    /// The `devkit.toml` a `--project` removal edits.
    Project(&'a Path),
}

/// Returns whether an entry was actually removed.
pub fn rm_library(target: RmTarget<'_>, cache_root: &Path, name: &str) -> Result<bool> {
    locks::with_lib(cache_root, name, || match target {
        RmTarget::Global(p) => manifest::remove_global(p, name, cache_root),
        RmTarget::Project(p) => manifest::remove_project(p, name, cache_root),
    })
}
```

No rendezvous of its own: `locks::hold`'s contention probe (Task 4 Step 3b)
signals `.contended` from inside the lock acquisition, which is strictly
stronger than anything this function could signal around it.

It must carry the target. `docm rm --project` edits a `devkit.toml`, so a helper
that only knows `remove_global` either removes from the wrong manifest, or is
used only for global removals and leaves the project branch outside the library
lock — and that branch is exactly where `rm` races `add`.

It takes `locks::with_lib` and **no manifest lock of its own**: Task 4 Step 6 put
`locks::with_manifest` *inside* `remove_global` and `remove_project`, and
wrapping it again would be a second acquisition of a non-reentrant lock from one
process. That deadlocks permanently — the exact hazard Global Constraints warns
about, reintroduced four tasks after the warning was written. Each lock is taken
once, at one layer, and the layer that owns it is the one closest to the file.

**The rendezvous must prove contention, not arrival.** Three earlier drafts of
this test signalled progressively closer to the lock — from the test child
before calling `rm_library`, then from `rm_library` just before `with_lib` — and
all three were satisfiable by a remover that takes no lock at all. The
interleaving they miss: add holds the lock and pauses, rm signals, the parent
releases `.go`, add completes its write, and *then* the unlocked remover runs
and deletes `up` from a finished manifest — producing exactly the state the test
asserts. Reaching the lock is not the same as being stopped by it.

`.contended` is written from inside `locks::hold` only when a non-blocking
acquisition fails, which is only possible while another process holds that same
lock. It cannot be produced by a build where `rm` takes no lock, so waiting on it
is what makes the test fail when the lock is removed.

`spawn_child` re-enters this binary at an `#[ignore]`d test and passes
`DOCM_RACE_GLOBAL`, `DOCM_RACE_CACHE`, `DOCM_RACE_BASE`, `DOCM_RACE_REPO` and —
critically — **`barrier::VAR`** — referenced by that constant, never
retyped as a string literal. An earlier draft passed a `DOCM_RACE_BARRIER` that
nothing consumed, so `.ready` was never written and the parent hung on a file no
code produces; naming the constant is what makes that class of mistake a compile
error. Pass it at spawn, never via `set_var` — for the reason given in Task 7.

Run this against a build with `rm`'s library lock removed and confirm it fails —
`up` survives — before restoring it. That RED run is the whole evidence the test
is load-bearing.

- [ ] **Step 3: Implement `add`**

Order, all inside **one** `locks::with_lib` for the target library:

1. snapshot any existing same-name entry
2. `barrier::signal("ready")?` then `barrier::wait("go")?` — the shared hook
   from Task 4 Step 3b, between the read and the write, no-op when the variable
   is unset. This is the only point where a locked and an unlocked `rm` diverge;
   putting it after the write makes the race test unfalsifiable.
3. write the new entry — `locks::with_manifest`, atomic rename
4. for a git URL with no `--ref`: resolve the default branch and store it as the
   entry's `ref`. Under `--project`, bail instead with the §6.1 text.
5. materialize by calling `resolve::resolve_locked` — **not** `resolve`, which
   would take the library lock a second time and deadlock against the hold this
   function is already inside
6. on error: restore the snapshot, or remove the entry when there was none, then
   return the error
7. on success print the multi-line block from §6.1

`rm` takes the same library lock around its manifest removal. Without it, `rm a`
racing `add a` can interleave into an entry that names a library whose
directory the other call is mid-way through building.

- [ ] **Step 4: Implement `sync`**

Per selected library, under its lock: fetch (`--prune --prune-tags`), infer and
record a missing `ref`, re-resolve, materialize, verify, record — again through
`resolve_locked`. Delete `sync_default` and `LibCache::sync_default`.

Backfilling a missing `ref` is only allowed when
`entry.origin_file` is the global `docs.toml`. A project entry read from a
`devkit.toml` is committed to someone's repo; writing an inferred default branch
into it would commit a machine-specific pin. For those, report the missing ref
and leave the file alone.

- [ ] **Step 5: Implement `info` and `list`**

`info` prints `name`, `repo`, `ref`, `version`, `commit`, `status`, `path`, then
layout and notes; exit non-zero on a mismatch that could not be repaired. `list`
gains ref, commit and origin columns. Both keep `--json` in sync with the struct.

- [ ] **Step 6: Add the doctor row**

`devkit doctor` sweeps every materialized checkout for HEAD correctness and
cleanliness, skipping cache-root entries where `locks::is_control(name)`.

- [ ] **Step 7: Gate and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/docm.rs crates/devkit-docs/src/manifest.rs \
        crates/devkit-docs/src/lib.rs src/bin/devkit \
        crates/devkit-docs/tests/concurrency.rs crates/devkit-docs/tests/common
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

**Known gaps handed to the implementer.** One remains: the `spawn_child` helper
in Tasks 4, 7 and 11 re-enters the test binary at an `#[ignore]`d test, and the
plan gives the call shape and the child bodies but not the helper itself —
`tests/refs_race.rs` already contains this repo's spawn pattern, and copying it
is more reliable than a second variant invented here.

**Type consistency.** `LibCache::new` returns `Result` and validates a *logical*
name from Task 1 onward; `LibCache::from_dir` is the unvalidated constructor for
names read from disk, and mixing them up breaks prune. `resolve::resolve` takes
`&Options` from Task 9 onward, so Tasks 5–8 write calls without it and Task 9
updates them — including the tests written in Task 5. `Data::record` takes five
arguments from Task 7 onward, which is one production caller and seven in
`refs.rs`'s own test module. `resolve_locked` is the lock-free inner layer;
`resolve` is the only wrapper, and `add`/`sync` call the inner one.

**Lock discipline.** Library lock → manifest lock → registry, never reversed,
never two library locks, never the same lock twice — `fd-lock` is not reentrant
and a second acquisition from one process blocks forever.
