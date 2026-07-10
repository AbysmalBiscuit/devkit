# docm: Managed Library Docs Checkouts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A `devkit-docs` crate + `docm` binary that keeps version-correct local checkouts of library repos and backs a single plugin-shipped `devkit:docs` skill that resolves every lookup through `docm info`.

**Architecture:** Manifest (global `~/.config/devkit/docs.toml` merged with `[docs]` sections found in `devkit.toml` files walking up from CWD) → bare blobless clone per lib under `~/.cache/devkit/docs/<name>/` with on-demand detached worktrees per resolved version → flock-guarded `registry.json` recording which project roots use which versions → reference-based `prune`. Resolution order: manual `ref` pin → lockfile version → tag probe → `default` worktree fallback.

**Tech Stack:** Rust (edition 2024), anyhow, clap + clap_complete, serde/serde_json/toml/serde_yaml_ng, toml_edit (new workspace dep), fd-lock via `devkit_common::store`, ureq for registry lookups, git via `devkit_common::cmd`.

**Spec:** `docs/superpowers/specs/2026-07-10-docm-library-docs-design.md`

## Global Constraints

- Global manifest is `~/.config/devkit/docs.toml` with top-level `[[libs]]` entries; project overlay is `[[docs.libs]]` inside `devkit.toml` (same fields), merged field-by-field by lib `name`, project wins. `.devkit/` is never used.
- Cache layout is exactly `~/.cache/devkit/docs/` containing `registry.json` plus per-lib `<name>/{repo.git, <version>/, default/, meta.toml}`. Derive the root as `devkit_common::paths::cache_dir().join("docs")`.
- `docm path` prints exactly one path on stdout; ALL warnings/notes go to stderr.
- Registry file read-modify-writes go through `devkit_common::store::with_lock` (flock'd, atomic tmp+rename). No `devkitd.lock` gate — this store has no daemon.
- Every successful resolution records `{project, lib, version, resolved_at}` where `version` is the worktree dirname (`"1.38.0"` or `"default"`). Prune is reference-based: project path gone → rows drop; lockfile bumped → row retargets; zero-row version worktree → deleted; `default/` exempt; whole-lib deletion only with `--yes`/interactive confirm.
- v1 lockfiles: `Cargo.lock`, `pnpm-lock.yaml`, `package-lock.json`, `uv.lock`. Multiple versions → highest, with a stderr note.
- Tag probe shapes, in order: `v{ver}`, `{ver}`, `{leaf}-{ver}`, `{leaf}-v{ver}`, `{leaf}@{ver}` where `leaf` is the package name after the last `/`. First match cached in `meta.toml`.
- Registry-lookup HTTP lives behind a trait; tests stub it, never hit the network. crates.io requires a `User-Agent` header.
- Blobless clone (`--filter=blob:none`) is best-effort: on any clone failure retry without the filter.
- All git worktrees are created `--detach` (so fetch can force-update branch heads freely).
- Tests never read the real `$HOME` config or real cache — every function takes explicit paths; only the binary touches default paths.
- anyhow everywhere; `.context()` on fallible IO. No fixed sleeps in tests. Gate: `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all`.
- Conventional Commits; commit per task.

## File Structure

```
crates/devkit-docs/
  Cargo.toml
  src/lib.rs           # module declarations + doctor_summary
  src/manifest.rs      # Ecosystem, LibEntry, DocsManifest, discover/merge, global+project writes
  src/lockfiles.rs     # per-ecosystem lockfile parsers, find_version, highest
  src/tags.rs          # TagPattern, apply, find
  src/layout.rs        # Layout, detect, with_overrides
  src/cache.rs         # LibCache (git ops), Meta, read/write_meta, docs_cache_root, dir_size
  src/refs.rs          # Data/RefRow Document, RefStore, prune plan
  src/resolve.rs       # Resolved, resolve() facade, project_root
  src/lookup.rs        # Registry trait, Http impl, extract/normalize/name_from_url/detect
  tests/common/mod.rs  # unique_tmp + fixture_repo helpers
  tests/cache.rs       # fixture-repo integration tests for LibCache
  tests/resolve.rs     # end-to-end resolve integration test
  tests/refs_race.rs   # multiprocess flock test
src/bin/docm.rs        # CLI
src/bin/devkit/doctor.rs  # + docs_cache row
skills/docs/SKILL.md   # the devkit:docs skill
```

---

### Task 1: Crate scaffold, manifest model, merge, discovery

**Files:**
- Create: `crates/devkit-docs/Cargo.toml`, `crates/devkit-docs/src/lib.rs`, `crates/devkit-docs/src/manifest.rs`
- Modify: `Cargo.toml` (workspace members, workspace.dependencies, root dependencies)

**Interfaces:**
- Produces: `devkit_docs::manifest::{Ecosystem, LibEntry, DocsManifest, Discovered, merge, discover, global_docs_path}`; `LibEntry::package_name() -> String`; `Discovered { manifest: DocsManifest, project_devkit_toml: Option<PathBuf> }`; `discover(start: &Path, global: Option<&Path>) -> Result<Discovered>` (global `None` = default `~/.config/devkit/docs.toml`; tests pass `Some(path)`).

- [ ] **Step 1: Wire the workspace**

In root `Cargo.toml`:
- append `"crates/devkit-docs"` to the `members` array;
- under `[workspace.dependencies]` add (beside the other `devkit-*` path deps and version pins):

```toml
toml_edit = "0.22"
devkit-docs = { path = "crates/devkit-docs" }
```

- under root `[dependencies]` add:

```toml
devkit-docs.workspace = true
```

Create `crates/devkit-docs/Cargo.toml`:

```toml
[package]
name = "devkit-docs"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow.workspace = true
clap = { workspace = true }
serde = { workspace = true }
serde_json.workspace = true
toml.workspace = true
toml_edit.workspace = true
serde_yaml_ng.workspace = true
ureq.workspace = true
devkit-common.workspace = true
```

Create `crates/devkit-docs/src/lib.rs`:

```rust
pub mod manifest;
```

- [ ] **Step 2: Write the failing tests**

At the bottom of `crates/devkit-docs/src/manifest.rs` (create the file with only a `//! Manifest model` doc comment and the test module for the RED run):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-docs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn merge_overrides_fields_and_appends_new_libs() {
        let base: DocsManifest = toml::from_str(
            "[[libs]]\nname='tokio'\necosystem='rust'\nrepo='https://github.com/tokio-rs/tokio'\n",
        )
        .unwrap();
        let over: DocsManifest =
            toml::from_str("[[libs]]\nname='tokio'\nref='compat-v0.2'\n[[libs]]\nname='godot'\necosystem='git'\nrepo='https://github.com/godotengine/godot'\n")
                .unwrap();
        let m = merge(base, over);
        assert_eq!(m.libs.len(), 2);
        let tokio = m.libs.iter().find(|l| l.name == "tokio").unwrap();
        assert_eq!(tokio.r#ref.as_deref(), Some("compat-v0.2")); // overridden
        assert_eq!(
            tokio.repo.as_deref(),
            Some("https://github.com/tokio-rs/tokio") // inherited
        );
        assert_eq!(tokio.ecosystem, Some(Ecosystem::Rust));
    }

    #[test]
    fn package_name_defaults_to_name() {
        let e = LibEntry { name: "tokio".into(), ..Default::default() };
        assert_eq!(e.package_name(), "tokio");
        let e2 = LibEntry {
            name: "node-types".into(),
            package: Some("@types/node".into()),
            ..Default::default()
        };
        assert_eq!(e2.package_name(), "@types/node");
    }

    #[test]
    fn discover_merges_global_then_walked_up_devkit_toml_layers() {
        let root = unique_tmp("discover");
        let global = root.join("docs.toml");
        std::fs::write(&global, "[[libs]]\nname='tokio'\necosystem='rust'\nrepo='u'\n").unwrap();
        let proj = root.join("mono/app");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            root.join("mono/devkit.toml"),
            "[defaults]\n[[docs.libs]]\nname='tokio'\nref='pin'\n",
        )
        .unwrap();
        std::fs::write(
            proj.join("devkit.toml"),
            "[[docs.libs]]\nname='react'\necosystem='js'\nrepo='r'\n",
        )
        .unwrap();
        let d = discover(&proj, Some(&global)).unwrap();
        assert_eq!(d.manifest.libs.len(), 2);
        let tokio = d.manifest.libs.iter().find(|l| l.name == "tokio").unwrap();
        assert_eq!(tokio.r#ref.as_deref(), Some("pin")); // project layer wins
        assert_eq!(tokio.repo.as_deref(), Some("u")); // global inherited
        assert_eq!(d.project_devkit_toml, Some(proj.join("devkit.toml"))); // nearest
    }

    #[test]
    fn discover_without_any_manifest_is_empty_not_an_error() {
        let root = unique_tmp("empty");
        let d = discover(&root, Some(&root.join("missing.toml"))).unwrap();
        assert!(d.manifest.libs.is_empty());
        assert_eq!(d.project_devkit_toml, None);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p devkit-docs`
Expected: compile FAILURE — `Ecosystem`, `LibEntry`, `DocsManifest`, `merge`, `discover` not found. That is the right RED for a new module.

- [ ] **Step 4: Implement the model**

Fill `crates/devkit-docs/src/manifest.rs` above the test module:

```rust
//! Manifest model: which libraries docm manages and where their repos live.
//!
//! Global entries live in `~/.config/devkit/docs.toml` (top-level `[[libs]]`);
//! per-project overrides live in a `[docs]` section (`[[docs.libs]]`) of the
//! project's `devkit.toml`. Layers merge field-by-field per lib name, the
//! deeper (more project-specific) layer winning.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    Rust,
    Js,
    Python,
    /// No package registry or lockfile; pinned by `ref` or default branch.
    Git,
}

impl std::fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Ecosystem::Rust => "rust",
            Ecosystem::Js => "js",
            Ecosystem::Python => "python",
            Ecosystem::Git => "git",
        })
    }
}

/// One managed library. Every field except `name` is optional so a project
/// overlay entry can override a single field of a global entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LibEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ecosystem: Option<Ecosystem>,
    /// Registry package name when it differs from `name` (e.g. `@types/node`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Manual pin (tag/branch/sha); wins over lockfile resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl LibEntry {
    pub fn package_name(&self) -> String {
        self.package.clone().unwrap_or_else(|| self.name.clone())
    }
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DocsManifest {
    #[serde(default)]
    pub libs: Vec<LibEntry>,
}

/// Overlay `over` onto `base`: same-name entries merge field-by-field
/// (`over`'s `Some` fields win), unknown names append.
pub fn merge(mut base: DocsManifest, over: DocsManifest) -> DocsManifest {
    for e in over.libs {
        match base.libs.iter_mut().find(|b| b.name == e.name) {
            Some(b) => {
                if e.ecosystem.is_some() { b.ecosystem = e.ecosystem; }
                if e.package.is_some() { b.package = e.package; }
                if e.repo.is_some() { b.repo = e.repo; }
                if e.r#ref.is_some() { b.r#ref = e.r#ref; }
                if e.src_dir.is_some() { b.src_dir = e.src_dir; }
                if e.docs_dir.is_some() { b.docs_dir = e.docs_dir; }
                if e.notes.is_some() { b.notes = e.notes; }
            }
            None => base.libs.push(e),
        }
    }
    base
}

/// `$HOME/.config/devkit/docs.toml` — HOME-based (not XDG) so it sits beside
/// `config.toml` and `secrets.toml`, matching `devkit_common::secrets`.
pub fn global_docs_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_default();
    home.join(".config/devkit/docs.toml")
}

#[derive(Debug)]
pub struct Discovered {
    pub manifest: DocsManifest,
    /// Nearest `devkit.toml` walking up from the start dir (whether or not it
    /// has a `[docs]` section) — the target of `--project` writes.
    pub project_devkit_toml: Option<PathBuf>,
}

/// Build the merged manifest: global file lowest precedence, then each
/// `devkit.toml`'s `[docs]` section from the filesystem root down to `start`
/// (deepest wins). A missing global file or absent `[docs]` sections are not
/// errors — an empty manifest is valid.
pub fn discover(start: &Path, global: Option<&Path>) -> Result<Discovered> {
    let global_path = global.map(Path::to_path_buf).unwrap_or_else(global_docs_path);
    let mut manifest = match std::fs::read_to_string(&global_path) {
        Ok(s) => toml::from_str(&s)
            .with_context(|| format!("parsing {}", global_path.display()))?,
        Err(_) => DocsManifest::default(),
    };

    // Collect devkit.toml [docs] layers deepest-first, then apply shallowest-first.
    let mut layers: Vec<DocsManifest> = Vec::new();
    let mut nearest: Option<PathBuf> = None;
    let mut dir = Some(start);
    while let Some(d) = dir {
        let c = d.join("devkit.toml");
        if c.is_file() {
            if nearest.is_none() {
                nearest = Some(c.clone());
            }
            if let Some(layer) = docs_layer(&c)? {
                layers.push(layer);
            }
        }
        dir = d.parent();
    }
    for layer in layers.into_iter().rev() {
        manifest = merge(manifest, layer);
    }
    Ok(Discovered { manifest, project_devkit_toml: nearest })
}

/// Extract the `[docs]` section of one `devkit.toml`, if present.
fn docs_layer(path: &Path) -> Result<Option<DocsManifest>> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let t: toml::Table = s
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    match t.get("docs") {
        Some(v) => Ok(Some(v.clone().try_into().with_context(|| {
            format!("[docs] section in {}", path.display())
        })?)),
        None => Ok(None),
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p devkit-docs`
Expected: 4 passed. Then `cargo clippy -p devkit-docs --all-targets -- -D warnings` and `cargo fmt --all`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/devkit-docs
git commit -m "feat(docs): add devkit-docs crate with manifest model"
```

---

### Task 2: Lockfile parsers

**Files:**
- Create: `crates/devkit-docs/src/lockfiles.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod lockfiles;`)

**Interfaces:**
- Consumes: `manifest::Ecosystem`.
- Produces: `lockfiles::versions_in_dir(dir: &Path, eco: Ecosystem, package: &str) -> Vec<String>`; `lockfiles::find_version(start: &Path, eco: Ecosystem, package: &str) -> Option<(PathBuf, Vec<String>)>` (walks up; returns the dir holding the winning lockfile); `lockfiles::highest(versions: Vec<String>) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-docs/src/lockfiles.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Ecosystem;

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-docs-lf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const CARGO_LOCK: &str = "version = 4\n\n[[package]]\nname = \"tokio\"\nversion = \"1.38.0\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.203\"\n";
    const UV_LOCK: &str = "version = 1\n\n[[package]]\nname = \"requests\"\nversion = \"2.32.3\"\n";
    const NPM_LOCK: &str = r#"{ "lockfileVersion": 3, "packages": {
        "": { "name": "app" },
        "node_modules/react": { "version": "18.3.1" },
        "node_modules/foo/node_modules/react": { "version": "17.0.2" } } }"#;
    const PNPM_V9: &str = "lockfileVersion: '9.0'\npackages:\n  react@18.3.1:\n    resolution: {integrity: sha512-x}\n  '@types/node@20.12.0':\n    resolution: {integrity: sha512-y}\n";
    const PNPM_V6: &str = "lockfileVersion: '6.0'\npackages:\n  /react@18.2.0(scheduler@0.23.0):\n    resolution: {integrity: sha512-z}\n";

    #[test]
    fn cargo_lock_versions() {
        let d = unique_tmp("cargo");
        std::fs::write(d.join("Cargo.lock"), CARGO_LOCK).unwrap();
        assert_eq!(versions_in_dir(&d, Ecosystem::Rust, "tokio"), vec!["1.38.0"]);
        assert!(versions_in_dir(&d, Ecosystem::Rust, "absent").is_empty());
    }

    #[test]
    fn uv_lock_versions() {
        let d = unique_tmp("uv");
        std::fs::write(d.join("uv.lock"), UV_LOCK).unwrap();
        assert_eq!(versions_in_dir(&d, Ecosystem::Python, "requests"), vec!["2.32.3"]);
    }

    #[test]
    fn npm_lock_collects_all_copies() {
        let d = unique_tmp("npm");
        std::fs::write(d.join("package-lock.json"), NPM_LOCK).unwrap();
        let mut v = versions_in_dir(&d, Ecosystem::Js, "react");
        v.sort();
        assert_eq!(v, vec!["17.0.2", "18.3.1"]);
    }

    #[test]
    fn pnpm_v9_and_scoped_and_v6_keys() {
        let d = unique_tmp("pnpm9");
        std::fs::write(d.join("pnpm-lock.yaml"), PNPM_V9).unwrap();
        assert_eq!(versions_in_dir(&d, Ecosystem::Js, "react"), vec!["18.3.1"]);
        assert_eq!(versions_in_dir(&d, Ecosystem::Js, "@types/node"), vec!["20.12.0"]);
        let d6 = unique_tmp("pnpm6");
        std::fs::write(d6.join("pnpm-lock.yaml"), PNPM_V6).unwrap();
        assert_eq!(versions_in_dir(&d6, Ecosystem::Js, "react"), vec!["18.2.0"]);
    }

    #[test]
    fn find_version_walks_up_and_reports_lockfile_dir() {
        let root = unique_tmp("walk");
        std::fs::write(root.join("Cargo.lock"), CARGO_LOCK).unwrap();
        let deep = root.join("crates/app/src");
        std::fs::create_dir_all(&deep).unwrap();
        let (dir, vs) = find_version(&deep, Ecosystem::Rust, "tokio").unwrap();
        assert_eq!(dir, root);
        assert_eq!(vs, vec!["1.38.0"]);
        assert!(find_version(&deep, Ecosystem::Rust, "absent").is_none());
    }

    #[test]
    fn highest_orders_numerically_not_lexically() {
        assert_eq!(
            highest(vec!["9.0.1".into(), "10.0.0".into(), "9.10.2".into()]),
            Some("10.0.0".into())
        );
        assert_eq!(highest(Vec::new()), None);
    }
}
```

- [ ] **Step 2: Run to verify RED**

Run: `cargo test -p devkit-docs lockfiles`
Expected: compile FAILURE — functions not defined.

- [ ] **Step 3: Implement**

Above the test module in `lockfiles.rs`:

```rust
//! Lockfile parsers: which version of a package does a project pin?
//!
//! All parsers are tolerant — an unreadable or unparsable lockfile yields no
//! versions rather than an error, so resolution can fall through to the
//! default branch.

use crate::manifest::Ecosystem;
use std::path::{Path, PathBuf};

pub fn versions_in_dir(dir: &Path, eco: Ecosystem, package: &str) -> Vec<String> {
    match eco {
        Ecosystem::Rust => toml_packages(&dir.join("Cargo.lock"), package),
        Ecosystem::Python => toml_packages(&dir.join("uv.lock"), package),
        Ecosystem::Js => {
            let mut v = npm_versions(&dir.join("package-lock.json"), package);
            v.extend(pnpm_versions(&dir.join("pnpm-lock.yaml"), package));
            v
        }
        Ecosystem::Git => Vec::new(),
    }
}

/// Walk up from `start`; the first directory whose lockfile mentions
/// `package` wins. Returns that directory (the project root for registry
/// purposes) and every version it pins.
pub fn find_version(start: &Path, eco: Ecosystem, package: &str) -> Option<(PathBuf, Vec<String>)> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let vs = versions_in_dir(d, eco, package);
        if !vs.is_empty() {
            return Some((d.to_path_buf(), vs));
        }
        dir = d.parent();
    }
    None
}

/// Highest version by numeric dot-segment comparison (`10.0.0` > `9.0.1`).
pub fn highest(mut versions: Vec<String>) -> Option<String> {
    versions.sort_by_key(|v| ver_key(v));
    versions.pop()
}

fn ver_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

/// `Cargo.lock` and `uv.lock` share the `[[package]] name/version` shape.
fn toml_packages(path: &Path, package: &str) -> Vec<String> {
    let Ok(s) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(t) = s.parse::<toml::Table>() else { return Vec::new() };
    let Some(pkgs) = t.get("package").and_then(|p| p.as_array()) else { return Vec::new() };
    pkgs.iter()
        .filter(|p| p.get("name").and_then(|n| n.as_str()) == Some(package))
        .filter_map(|p| p.get("version").and_then(|v| v.as_str()).map(String::from))
        .collect()
}

/// package-lock.json v2/v3 `packages` map, falling back to the ancient v1
/// top-level `dependencies` map.
fn npm_versions(path: &Path, package: &str) -> Vec<String> {
    let Ok(s) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else { return Vec::new() };
    let mut out = Vec::new();
    if let Some(pkgs) = v.get("packages").and_then(|p| p.as_object()) {
        let suffix = format!("node_modules/{package}");
        for (k, e) in pkgs {
            if (k == &suffix || k.ends_with(&format!("/{suffix}")))
                && let Some(ver) = e.get("version").and_then(|x| x.as_str())
            {
                out.push(ver.to_string());
            }
        }
    }
    if out.is_empty()
        && let Some(ver) = v
            .get("dependencies")
            .and_then(|d| d.get(package))
            .and_then(|e| e.get("version"))
            .and_then(|x| x.as_str())
    {
        out.push(ver.to_string());
    }
    out
}

/// pnpm-lock.yaml `packages` keys: v9 `name@1.2.3` / `@scope/name@1.2.3`,
/// v6 `/name@1.2.3(peer@x)`, v5 `/name/1.2.3`.
fn pnpm_versions(path: &Path, package: &str) -> Vec<String> {
    let Ok(s) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(y) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&s) else { return Vec::new() };
    let Some(pkgs) = y.get("packages").and_then(|p| p.as_mapping()) else { return Vec::new() };
    let mut out = Vec::new();
    for key in pkgs.keys().filter_map(|k| k.as_str()) {
        let k = key.trim_start_matches('/');
        let k = k.split('(').next().unwrap_or(k);
        let parsed = k
            .rsplit_once('@')
            .filter(|(n, _)| !n.is_empty())
            .or_else(|| k.rsplit_once('/'));
        if let Some((name, ver)) = parsed
            && name == package
        {
            out.push(ver.to_string());
        }
    }
    out
}
```

Add `pub mod lockfiles;` to `lib.rs`.

- [ ] **Step 4: Run to verify GREEN**

Run: `cargo test -p devkit-docs lockfiles` — expected 6 passed. Then clippy + fmt.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs
git commit -m "feat(docs): parse Cargo, uv, npm, and pnpm lockfiles"
```

---

### Task 3: Tag patterns

**Files:**
- Create: `crates/devkit-docs/src/tags.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod tags;`)

**Interfaces:**
- Produces: `tags::TagPattern` (`Serialize`/`Deserialize`, kebab-case, `Clone, Copy, PartialEq, Eq, Debug`); `tags::apply(p: TagPattern, package: &str, version: &str) -> String`; `tags::find(tags: &[String], package: &str, version: &str) -> Option<(TagPattern, String)>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-docs/src/tags.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_covers_all_shapes_and_uses_package_leaf() {
        assert_eq!(apply(TagPattern::V, "tokio", "1.38.0"), "v1.38.0");
        assert_eq!(apply(TagPattern::Plain, "tokio", "1.38.0"), "1.38.0");
        assert_eq!(apply(TagPattern::NameDash, "tokio", "1.38.0"), "tokio-1.38.0");
        assert_eq!(apply(TagPattern::NameDashV, "tokio", "1.38.0"), "tokio-v1.38.0");
        assert_eq!(apply(TagPattern::NameAt, "@types/node", "20.1.0"), "node@20.1.0");
    }

    #[test]
    fn find_probes_in_order_and_returns_first_match() {
        let tags: Vec<String> = vec!["tokio-1.38.0".into(), "v9.9.9".into()];
        assert_eq!(
            find(&tags, "tokio", "1.38.0"),
            Some((TagPattern::NameDash, "tokio-1.38.0".into()))
        );
        assert_eq!(find(&tags, "tokio", "2.0.0"), None);
    }

    #[test]
    fn pattern_round_trips_through_toml() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct W { p: TagPattern }
        let s = toml::to_string(&W { p: TagPattern::NameDashV }).unwrap();
        assert_eq!(s.trim(), "p = \"name-dash-v\"");
        let w: W = toml::from_str(&s).unwrap();
        assert_eq!(w.p, TagPattern::NameDashV);
    }
}
```

- [ ] **Step 2: RED** — `cargo test -p devkit-docs tags` fails to compile.

- [ ] **Step 3: Implement**

```rust
//! Version → git tag probing. Repos tag `v1.2.3`, `1.2.3`, `pkg-1.2.3`,
//! `pkg-v1.2.3`, or `pkg@1.2.3`; the first shape that matches is cached in
//! the lib's meta.toml so later resolutions skip the probe.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagPattern {
    V,
    Plain,
    NameDash,
    NameDashV,
    NameAt,
}

pub const ALL: [TagPattern; 5] = [
    TagPattern::V,
    TagPattern::Plain,
    TagPattern::NameDash,
    TagPattern::NameDashV,
    TagPattern::NameAt,
];

pub fn apply(p: TagPattern, package: &str, version: &str) -> String {
    // Scoped npm packages tag by the leaf name: @scope/pkg → pkg@1.2.3.
    let leaf = package.rsplit('/').next().unwrap_or(package);
    match p {
        TagPattern::V => format!("v{version}"),
        TagPattern::Plain => version.to_string(),
        TagPattern::NameDash => format!("{leaf}-{version}"),
        TagPattern::NameDashV => format!("{leaf}-v{version}"),
        TagPattern::NameAt => format!("{leaf}@{version}"),
    }
}

pub fn find(tags: &[String], package: &str, version: &str) -> Option<(TagPattern, String)> {
    ALL.iter()
        .copied()
        .map(|p| (p, apply(p, package, version)))
        .find(|(_, t)| tags.iter().any(|x| x == t))
}
```

Add `pub mod tags;` to `lib.rs`.

- [ ] **Step 4: GREEN** — `cargo test -p devkit-docs tags`, then clippy + fmt.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs
git commit -m "feat(docs): probe version-to-tag patterns"
```

---

### Task 4: Layout detection

**Files:**
- Create: `crates/devkit-docs/src/layout.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod layout;`)

**Interfaces:**
- Consumes: `manifest::LibEntry`.
- Produces: `layout::Layout { docs_dir, src_dir, examples_dir, kind: Option<String> }` (all `Option<String>`, `Serialize`/`Deserialize`/`Clone`/`Default`/`PartialEq`/`Debug`); `layout::detect(root: &Path) -> Layout`; `layout::with_overrides(l: Layout, entry: &LibEntry) -> Layout`.

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-docs/src/layout.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::LibEntry;

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-docs-ly-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detects_dirs_and_mdbook() {
        let d = unique_tmp("mdbook");
        for p in ["docs", "src", "examples"] {
            std::fs::create_dir_all(d.join(p)).unwrap();
        }
        std::fs::write(d.join("book.toml"), "[book]").unwrap();
        let l = detect(&d);
        assert_eq!(l.docs_dir.as_deref(), Some("docs"));
        assert_eq!(l.src_dir.as_deref(), Some("src"));
        assert_eq!(l.examples_dir.as_deref(), Some("examples"));
        assert_eq!(l.kind.as_deref(), Some("mdbook"));
    }

    #[test]
    fn detects_sphinx_under_doc_and_empty_repo_is_all_none() {
        let d = unique_tmp("sphinx");
        std::fs::create_dir_all(d.join("doc")).unwrap();
        std::fs::write(d.join("doc/conf.py"), "").unwrap();
        let l = detect(&d);
        assert_eq!(l.docs_dir.as_deref(), Some("doc"));
        assert_eq!(l.kind.as_deref(), Some("sphinx"));
        let empty = unique_tmp("none");
        assert_eq!(detect(&empty), Layout::default());
    }

    #[test]
    fn manifest_overrides_beat_detection() {
        let l = Layout { docs_dir: Some("docs".into()), ..Default::default() };
        let e = LibEntry {
            name: "godot".into(),
            docs_dir: Some("doc/classes".into()),
            src_dir: Some("core".into()),
            ..Default::default()
        };
        let out = with_overrides(l, &e);
        assert_eq!(out.docs_dir.as_deref(), Some("doc/classes"));
        assert_eq!(out.src_dir.as_deref(), Some("core"));
    }
}
```

- [ ] **Step 2: RED** — `cargo test -p devkit-docs layout` fails to compile.

- [ ] **Step 3: Implement**

```rust
//! Where inside a checkout do docs, source, and examples live?
//! Detected once per worktree at materialization time, cached in meta.toml;
//! manifest `src_dir`/`docs_dir` overrides always win.

use crate::manifest::LibEntry;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples_dir: Option<String>,
    /// Doc system hint: mdbook | sphinx | docusaurus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

pub fn detect(root: &Path) -> Layout {
    let dir = |names: &[&str]| {
        names
            .iter()
            .find(|n| root.join(n).is_dir())
            .map(|s| s.to_string())
    };
    let file = |names: &[&str]| names.iter().any(|n| root.join(n).is_file());
    let kind = if file(&["book.toml"]) {
        Some("mdbook".to_string())
    } else if file(&["docs/conf.py", "doc/conf.py", "conf.py"]) {
        Some("sphinx".to_string())
    } else if file(&[
        "docusaurus.config.js",
        "docusaurus.config.ts",
        "website/docusaurus.config.js",
    ]) {
        Some("docusaurus".to_string())
    } else {
        None
    };
    Layout {
        docs_dir: dir(&["docs", "doc", "documentation"]),
        src_dir: dir(&["src", "lib", "crates"]),
        examples_dir: dir(&["examples", "example"]),
        kind,
    }
}

pub fn with_overrides(mut l: Layout, entry: &LibEntry) -> Layout {
    if entry.docs_dir.is_some() {
        l.docs_dir = entry.docs_dir.clone();
    }
    if entry.src_dir.is_some() {
        l.src_dir = entry.src_dir.clone();
    }
    l
}
```

Add `pub mod layout;` to `lib.rs`.

- [ ] **Step 4: GREEN** — `cargo test -p devkit-docs layout`, clippy + fmt.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs
git commit -m "feat(docs): detect checkout docs/src layout"
```

---

### Task 5: Cache git layer (clone, worktrees, sync, meta)

**Files:**
- Create: `crates/devkit-docs/src/cache.rs`, `crates/devkit-docs/tests/common/mod.rs`, `crates/devkit-docs/tests/cache.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod cache;`)

**Interfaces:**
- Consumes: `devkit_common::cmd::{capture, git}`; `tags::TagPattern`; `layout::Layout`.
- Produces: `cache::docs_cache_root() -> PathBuf`; `cache::dir_size(p: &Path) -> u64`; `cache::Meta { tag_pattern: Option<TagPattern>, layouts: BTreeMap<String, Layout> }`; `cache::read_meta(lib_dir: &Path) -> Meta`; `cache::write_meta(lib_dir: &Path, m: &Meta) -> Result<()>`; `cache::LibCache { dir: PathBuf }` with `new(cache_root, name)`, `cloned() -> bool`, `ensure_clone(repo) -> Result<()>`, `fetch()`, `tags() -> Result<Vec<String>>`, `default_branch() -> Result<String>`, `worktree_path(dirname) -> PathBuf`, `ensure_worktree(dirname, commitish) -> Result<PathBuf>`, `sync_default(pin: Option<&str>) -> Result<PathBuf>`, `version_worktrees() -> Vec<(String, PathBuf)>`, `remove_worktree(dirname) -> Result<()>`.

- [ ] **Step 1: Write the shared fixture helper**

Create `crates/devkit-docs/tests/common/mod.rs`:

```rust
//! Shared integration-test helpers: unique temp dirs and a local fixture
//! git repo with two tagged versions (v1.0.0 → "// v1", v1.1.0 tip → "// v2").

use std::path::{Path, PathBuf};

pub fn unique_tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("devkit-docs-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn sh(args: &[&str], cwd: &Path) {
    devkit_common::cmd::capture("git", args, Some(cwd.to_str().unwrap())).unwrap();
}

/// Returns the repo path as a clone URL (plain local path).
pub fn fixture_repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    sh(&["init", "-b", "main"], dir);
    sh(&["config", "user.email", "t@t"], dir);
    sh(&["config", "user.name", "t"], dir);
    sh(&["config", "commit.gpgsign", "false"], dir);
    sh(&["config", "tag.gpgsign", "false"], dir);
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("docs/guide.md"), "# guide").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "// v1").unwrap();
    sh(&["add", "."], dir);
    sh(&["commit", "-m", "v1"], dir);
    sh(&["tag", "v1.0.0"], dir);
    std::fs::write(dir.join("src/lib.rs"), "// v2").unwrap();
    sh(&["add", "."], dir);
    sh(&["commit", "-m", "v2"], dir);
    sh(&["tag", "v1.1.0"], dir);
    dir.to_str().unwrap().to_string()
}
```

- [ ] **Step 2: Write the failing integration tests**

Create `crates/devkit-docs/tests/cache.rs`:

```rust
mod common;

use common::{fixture_repo, unique_tmp};
use devkit_docs::cache::{self, LibCache, Meta};
use devkit_docs::tags::TagPattern;

#[test]
fn clone_tags_worktree_and_sync() {
    let tmp = unique_tmp("cache");
    let repo = fixture_repo(&tmp.join("upstream"));
    let lib = LibCache::new(&tmp.join("cacheroot"), "mylib");

    assert!(!lib.cloned());
    lib.ensure_clone(&repo).unwrap();
    assert!(lib.cloned());
    lib.ensure_clone(&repo).unwrap(); // idempotent

    let tags = lib.tags().unwrap();
    assert!(tags.contains(&"v1.0.0".to_string()) && tags.contains(&"v1.1.0".to_string()));
    assert_eq!(lib.default_branch().unwrap(), "main");

    // Version worktree pins the tag's content, not the tip.
    let wt = lib.ensure_worktree("1.0.0", "v1.0.0").unwrap();
    assert_eq!(std::fs::read_to_string(wt.join("src/lib.rs")).unwrap(), "// v1");
    lib.ensure_worktree("1.0.0", "v1.0.0").unwrap(); // idempotent

    // Default worktree tracks the branch tip.
    let def = lib.sync_default(None).unwrap();
    assert_eq!(std::fs::read_to_string(def.join("src/lib.rs")).unwrap(), "// v2");

    let mut names: Vec<String> = lib.version_worktrees().into_iter().map(|(n, _)| n).collect();
    names.sort();
    assert_eq!(names, vec!["1.0.0", "default"]);

    lib.remove_worktree("1.0.0").unwrap();
    assert!(!lib.worktree_path("1.0.0").exists());
}

#[test]
fn sync_default_follows_new_commits() {
    let tmp = unique_tmp("sync");
    let upstream = tmp.join("upstream");
    let repo = fixture_repo(&upstream);
    let lib = LibCache::new(&tmp.join("cacheroot"), "mylib");
    lib.ensure_clone(&repo).unwrap();
    let def = lib.sync_default(None).unwrap();
    assert_eq!(std::fs::read_to_string(def.join("src/lib.rs")).unwrap(), "// v2");

    // Upstream moves on; fetch + sync catches up.
    std::fs::write(upstream.join("src/lib.rs"), "// v3").unwrap();
    devkit_common::cmd::capture("git", &["add", "."], Some(upstream.to_str().unwrap())).unwrap();
    devkit_common::cmd::capture("git", &["commit", "-m", "v3"], Some(upstream.to_str().unwrap()))
        .unwrap();
    lib.fetch().unwrap();
    let def = lib.sync_default(None).unwrap();
    assert_eq!(std::fs::read_to_string(def.join("src/lib.rs")).unwrap(), "// v3");
}

#[test]
fn meta_round_trips() {
    let tmp = unique_tmp("meta");
    let mut m = Meta::default();
    m.tag_pattern = Some(TagPattern::NameDash);
    m.layouts.insert(
        "1.0.0".into(),
        devkit_docs::layout::Layout { docs_dir: Some("docs".into()), ..Default::default() },
    );
    cache::write_meta(&tmp, &m).unwrap();
    assert_eq!(cache::read_meta(&tmp), m);
    assert_eq!(cache::read_meta(&tmp.join("missing")), Meta::default());
}
```

- [ ] **Step 3: RED** — `cargo test -p devkit-docs --test cache` fails to compile.

- [ ] **Step 4: Implement `cache.rs`**

```rust
//! Per-library cache: one bare (ideally blobless) clone plus detached
//! worktrees per resolved version, all under `~/.cache/devkit/docs/<name>/`.

use crate::layout::Layout;
use crate::tags::TagPattern;
use anyhow::{Context, Result};
use devkit_common::cmd;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `~/.cache/devkit/docs` (or `$XDG_CACHE_HOME/devkit/docs`).
pub fn docs_cache_root() -> PathBuf {
    devkit_common::paths::cache_dir().join("docs")
}

/// Recursive byte count; used by the doctor row.
pub fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten()
        .map(|e| {
            let path = e.path();
            if path.is_dir() {
                dir_size(&path)
            } else {
                path.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

/// Per-lib sidecar: the cached tag pattern and detected layout per worktree.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_pattern: Option<TagPattern>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, Layout>,
}

pub fn read_meta(lib_dir: &Path) -> Meta {
    std::fs::read_to_string(lib_dir.join("meta.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn write_meta(lib_dir: &Path, m: &Meta) -> Result<()> {
    std::fs::create_dir_all(lib_dir)?;
    std::fs::write(lib_dir.join("meta.toml"), toml::to_string_pretty(m)?)
        .context("writing meta.toml")?;
    Ok(())
}

pub struct LibCache {
    pub dir: PathBuf,
}

impl LibCache {
    pub fn new(cache_root: &Path, name: &str) -> Self {
        Self { dir: cache_root.join(name) }
    }

    pub fn bare(&self) -> PathBuf {
        self.dir.join("repo.git")
    }

    fn bare_str(&self) -> String {
        self.bare().to_string_lossy().into_owned()
    }

    pub fn cloned(&self) -> bool {
        self.bare().is_dir()
    }

    /// Bare clone, blobless when the transport supports it. Filter support is
    /// best-effort: any failure retries as a plain bare clone.
    pub fn ensure_clone(&self, repo: &str) -> Result<()> {
        if self.cloned() {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        let dest = self.bare_str();
        if cmd::capture(
            "git",
            &["clone", "--bare", "--filter=blob:none", repo, dest.as_str()],
            None,
        )
        .is_err()
        {
            let _ = std::fs::remove_dir_all(self.bare());
            cmd::capture("git", &["clone", "--bare", repo, dest.as_str()], None)
                .with_context(|| format!("cloning {repo}"))?;
        }
        // Bare clones get no fetch refspec; heads must track the remote so
        // sync can fast-forward the default worktree.
        cmd::git(
            &["config", "remote.origin.fetch", "+refs/heads/*:refs/heads/*"],
            &dest,
        )?;
        Ok(())
    }

    pub fn fetch(&self) -> Result<()> {
        cmd::git(
            &["fetch", "--force", "--tags", "--prune", "origin"],
            &self.bare_str(),
        )
        .map(|_| ())
    }

    pub fn tags(&self) -> Result<Vec<String>> {
        Ok(cmd::git(&["tag", "--list"], &self.bare_str())?
            .lines()
            .map(str::to_string)
            .collect())
    }

    pub fn default_branch(&self) -> Result<String> {
        Ok(cmd::git(&["symbolic-ref", "--short", "HEAD"], &self.bare_str())?
            .trim()
            .to_string())
    }

    pub fn worktree_path(&self, dirname: &str) -> PathBuf {
        self.dir.join(dirname)
    }

    /// Materialize a detached worktree at `commitish` if missing.
    pub fn ensure_worktree(&self, dirname: &str, commitish: &str) -> Result<PathBuf> {
        let path = self.worktree_path(dirname);
        if path.is_dir() {
            return Ok(path);
        }
        let p = path.to_string_lossy().into_owned();
        cmd::git(
            &["worktree", "add", "--detach", p.as_str(), commitish],
            &self.bare_str(),
        )
        .with_context(|| format!("materializing {dirname} at {commitish}"))?;
        Ok(path)
    }

    /// Ensure the `default` worktree exists and sits at `pin` (or the default
    /// branch tip). Fetch first when the tip should move.
    pub fn sync_default(&self, pin: Option<&str>) -> Result<PathBuf> {
        let target = match pin {
            Some(r) => r.to_string(),
            None => self.default_branch()?,
        };
        let path = self.worktree_path("default");
        if !path.is_dir() {
            return self.ensure_worktree("default", &target);
        }
        cmd::git(
            &["checkout", "--detach", target.as_str()],
            &path.to_string_lossy(),
        )?;
        Ok(path)
    }

    /// Worktree dirs currently on disk (including `default`, excluding the
    /// bare repo itself).
    pub fn version_worktrees(&self) -> Vec<(String, PathBuf)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        rd.flatten()
            .filter(|e| e.path().is_dir())
            .filter(|e| e.file_name() != "repo.git")
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect()
    }

    pub fn remove_worktree(&self, dirname: &str) -> Result<()> {
        let p = self.worktree_path(dirname).to_string_lossy().into_owned();
        cmd::git(
            &["worktree", "remove", "--force", p.as_str()],
            &self.bare_str(),
        )
        .with_context(|| format!("removing worktree {dirname}"))?;
        Ok(())
    }
}
```

Add `pub mod cache;` to `lib.rs`.

- [ ] **Step 5: GREEN** — `cargo test -p devkit-docs --test cache` (3 passed) and `cargo test -p devkit-docs`, clippy + fmt. Note: local-path clones may ignore `--filter` with a warning — that is the designed best-effort behavior, not a failure.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-docs
git commit -m "feat(docs): manage bare clones and version worktrees"
```

---

### Task 6: Reference registry store + multiprocess flock test

**Files:**
- Create: `crates/devkit-docs/src/refs.rs`, `crates/devkit-docs/tests/refs_race.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod refs;`)

**Interfaces:**
- Consumes: `devkit_common::store::{Document, with_lock, load}`; `cache::docs_cache_root` (in the race test).
- Produces: `refs::SCHEMA: u32`; `refs::RefRow { project, lib, version, resolved_at }`; `refs::Data { version, rows }` implementing `Document`, with `Data::record(&mut self, project: &str, lib: &str, version: &str)`; `refs::RefStore::at(cache_root: &Path) -> RefStore` with `commit<T>(f) -> Result<T>` and `snapshot() -> Data`; `refs::plan(data, worktrees, manifest_libs, current) -> PrunePlan { keep: Vec<RefRow>, delete: Vec<(String, String)>, removable_libs: Vec<String> }`.

- [ ] **Step 1: Write the failing unit tests**

Create `crates/devkit-docs/src/refs.rs` with the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-docs-rf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn record_upserts_by_project_and_lib() {
        let mut d = Data::default();
        d.record("/p1", "tokio", "1.0.0");
        d.record("/p1", "tokio", "1.1.0"); // same key → update
        d.record("/p2", "tokio", "1.0.0"); // new project → append
        assert_eq!(d.rows.len(), 2);
        assert_eq!(d.rows[0].version, "1.1.0");
    }

    #[test]
    fn store_commit_and_snapshot_round_trip() {
        let root = unique_tmp("store");
        let store = RefStore::at(&root);
        store.commit(|d| { d.record("/p", "tokio", "1.0.0"); Ok(()) }).unwrap();
        let snap = store.snapshot();
        assert_eq!(snap.rows.len(), 1);
        assert_eq!(snap.version, SCHEMA);
        assert!(root.join("registry.json").is_file());
    }

    #[test]
    fn plan_drops_dead_projects_retargets_bumps_and_deletes_orphans() {
        let live = unique_tmp("live"); // an existing dir = live project
        let mut data = Data::default();
        data.record(live.to_str().unwrap(), "tokio", "1.0.0"); // will retarget to 1.1.0
        data.record("/gone/nowhere", "tokio", "0.9.0"); // dead project → drop
        data.record(live.to_str().unwrap(), "serde", "2.0.0"); // no longer in lockfile → drop

        let mut worktrees = BTreeMap::new();
        worktrees.insert("tokio".to_string(), vec!["1.0.0".into(), "0.9.0".into(), "1.1.0".into(), "default".into()]);
        worktrees.insert("legacy".to_string(), vec!["3.0.0".into(), "default".into()]);
        let manifest_libs: BTreeSet<String> = ["tokio", "serde"].iter().map(|s| s.to_string()).collect();

        let p = plan(&data, &worktrees, &manifest_libs, |_, lib| {
            (lib == "tokio").then(|| "1.1.0".to_string())
        });
        assert_eq!(p.keep.len(), 1);
        assert_eq!(p.keep[0].version, "1.1.0"); // retargeted
        let mut del = p.delete.clone();
        del.sort();
        assert_eq!(del, vec![
            ("legacy".to_string(), "3.0.0".to_string()),
            ("tokio".to_string(), "0.9.0".to_string()),
            ("tokio".to_string(), "1.0.0".to_string()),
        ]); // "default" never deleted; 1.1.0 referenced
        assert_eq!(p.removable_libs, vec!["legacy".to_string()]);
    }
}
```

- [ ] **Step 2: RED** — `cargo test -p devkit-docs refs` fails to compile.

- [ ] **Step 3: Implement**

```rust
//! Reference registry: which project roots resolved which lib versions.
//! One flock-guarded JSON file at the cache root; concurrent agent sessions
//! race docm, so every read-modify-write goes through
//! `devkit_common::store::with_lock`. No devkitd gate — no daemon serves this
//! file. A holder is live iff its project root path still exists (the same
//! model as the ports registry).

use anyhow::Result;
use devkit_common::store::{self, Document};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefRow {
    pub project: String,
    pub lib: String,
    /// Worktree dirname: a lockfile version like `1.38.0`, or `default`.
    pub version: String,
    pub resolved_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub rows: Vec<RefRow>,
}

impl Document for Data {
    fn stamp_version(&mut self) {
        self.version = SCHEMA;
    }
    fn salvage(_raw: &str) -> Option<Self> {
        None
    }
    fn label() -> &'static str {
        "docs registry"
    }
    fn len(&self) -> usize {
        self.rows.len()
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Data {
    /// Upsert keyed on (project, lib).
    pub fn record(&mut self, project: &str, lib: &str, version: &str) {
        match self
            .rows
            .iter_mut()
            .find(|r| r.project == project && r.lib == lib)
        {
            Some(r) => {
                r.version = version.to_string();
                r.resolved_at = now();
            }
            None => self.rows.push(RefRow {
                project: project.to_string(),
                lib: lib.to_string(),
                version: version.to_string(),
                resolved_at: now(),
            }),
        }
    }
}

pub struct RefStore {
    lock_path: PathBuf,
    data_path: PathBuf,
}

impl RefStore {
    pub fn at(cache_root: &Path) -> Self {
        Self {
            lock_path: cache_root.join("registry.lock"),
            data_path: cache_root.join("registry.json"),
        }
    }

    pub fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        store::with_lock(&self.lock_path, &self.data_path, f)
    }

    pub fn snapshot(&self) -> Data {
        store::load(&self.data_path)
    }
}

#[derive(Debug)]
pub struct PrunePlan {
    /// Rows that survive (already retargeted to the current version).
    pub keep: Vec<RefRow>,
    /// (lib, worktree dirname) pairs with zero remaining references.
    pub delete: Vec<(String, String)>,
    /// Libs absent from every manifest with zero references — deleted only
    /// after confirmation.
    pub removable_libs: Vec<String>,
}

/// Pure prune planner. `worktrees` maps lib → worktree dirnames on disk
/// (including `default`); `current(project, lib)` re-resolves what a live
/// project pins right now (`None` = no longer referenced). Liveness checks
/// run on a snapshot outside the registry lock.
pub fn plan(
    data: &Data,
    worktrees: &BTreeMap<String, Vec<String>>,
    manifest_libs: &BTreeSet<String>,
    current: impl Fn(&str, &str) -> Option<String>,
) -> PrunePlan {
    let mut keep = Vec::new();
    for r in &data.rows {
        if !Path::new(&r.project).exists() {
            continue; // project root gone → holder dead → row drops
        }
        if let Some(v) = current(&r.project, &r.lib) {
            keep.push(RefRow { version: v, ..r.clone() });
        }
    }
    let referenced: BTreeSet<(String, String)> = keep
        .iter()
        .map(|r| (r.lib.clone(), r.version.clone()))
        .collect();
    let mut delete = Vec::new();
    let mut removable_libs = Vec::new();
    for (lib, dirs) in worktrees {
        for d in dirs {
            if d != "default" && !referenced.contains(&(lib.clone(), d.clone())) {
                delete.push((lib.clone(), d.clone()));
            }
        }
        let lib_referenced = referenced.iter().any(|(l, _)| l == lib);
        if !manifest_libs.contains(lib) && !lib_referenced {
            removable_libs.push(lib.clone());
        }
    }
    PrunePlan { keep, delete, removable_libs }
}
```

Add `pub mod refs;` to `lib.rs`.

- [ ] **Step 4: GREEN for units** — `cargo test -p devkit-docs refs` (3 passed).

- [ ] **Step 5: Write the multiprocess race test**

Create `crates/devkit-docs/tests/refs_race.rs` (same self-re-exec shape as `devkit-ports --test registry`):

```rust
use devkit_docs::refs::RefStore;
use std::process::Command;

#[test]
fn concurrent_records_never_lose_rows() {
    // The test binary re-execs itself as the worker via an env switch.
    if let Ok(project) = std::env::var("DEVKIT_DOCS_TEST_RECORD") {
        let store = RefStore::at(&devkit_docs::cache::docs_cache_root());
        store
            .commit(|d| {
                d.record(&project, "tokio", "1.0.0");
                Ok(())
            })
            .unwrap();
        std::process::exit(0);
    }

    let tmp = std::env::temp_dir().join(format!("devkit-docs-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let exe = std::env::current_exe().unwrap();

    let mut kids = Vec::new();
    for i in 0..16 {
        let project = tmp.join(format!("p{i}"));
        std::fs::create_dir_all(&project).unwrap();
        kids.push(
            Command::new(&exe)
                // Pin both cache-home inputs so all workers flock the same
                // isolated registry: cache_dir() prefers $XDG_CACHE_HOME and
                // falls back to $HOME/.cache.
                .env("HOME", &tmp)
                .env("XDG_CACHE_HOME", &tmp)
                .env("DEVKIT_DOCS_TEST_RECORD", &project)
                .args(["--exact", "concurrent_records_never_lose_rows", "--nocapture"])
                .output()
                .unwrap(),
        );
    }
    for k in &kids {
        assert!(k.status.success(), "worker failed: {}", String::from_utf8_lossy(&k.stderr));
    }
    let data: devkit_docs::refs::Data = {
        let raw = std::fs::read_to_string(tmp.join("devkit/docs/registry.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    assert_eq!(data.rows.len(), 16, "rows lost to a race: {:?}", data.rows);
}
```

- [ ] **Step 6: Run the race test**

Run: `cargo test -p devkit-docs --test refs_race`
Expected: PASS (16 rows, none lost). Then full `cargo test -p devkit-docs`, clippy + fmt.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-docs
git commit -m "feat(docs): add flock'd reference registry with prune plan"
```

---

### Task 7: Resolution facade

**Files:**
- Create: `crates/devkit-docs/src/resolve.rs`, `crates/devkit-docs/tests/resolve.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod resolve;`)

**Interfaces:**
- Consumes: everything from Tasks 1–6.
- Produces: `resolve::Resolved { name, version, worktree, path: PathBuf, layout: Layout, notes: Option<String>, warnings: Vec<String> }` (derives `Serialize`); `resolve::resolve(entry: &LibEntry, start: &Path, cache_root: &Path) -> Result<Resolved>`; `resolve::project_root(start: &Path) -> PathBuf` (nearest `devkit.toml` dir, else `start`).

- [ ] **Step 1: Write the failing integration tests**

Create `crates/devkit-docs/tests/resolve.rs`:

```rust
mod common;

use common::{fixture_repo, unique_tmp};
use devkit_docs::manifest::{Ecosystem, LibEntry};
use devkit_docs::refs::RefStore;
use devkit_docs::resolve::resolve;

#[test]
fn lockfile_version_resolves_to_tag_worktree_and_records_ref() {
    let tmp = unique_tmp("resolve");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"mylib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();

    let entry = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let r = resolve(&entry, &project, &cache_root).unwrap();
    assert_eq!(r.worktree, "1.0.0");
    assert_eq!(r.version, "1.0.0");
    // Tag content, not tip: v1.0.0 has "// v1".
    assert_eq!(std::fs::read_to_string(r.path.join("src/lib.rs")).unwrap(), "// v1");
    assert_eq!(r.layout.docs_dir.as_deref(), Some("docs"));
    assert!(r.warnings.is_empty());

    let data = RefStore::at(&cache_root).snapshot();
    assert_eq!(data.rows.len(), 1);
    assert_eq!(data.rows[0].project, project.to_string_lossy());
    assert_eq!(data.rows[0].version, "1.0.0");

    // Cached tag pattern short-circuits the next probe.
    let meta = devkit_docs::cache::read_meta(&cache_root.join("mylib"));
    assert_eq!(meta.tag_pattern, Some(devkit_docs::tags::TagPattern::V));
}

#[test]
fn ref_pin_wins_and_no_lockfile_falls_back_to_default_with_warning() {
    let tmp = unique_tmp("resolve-pin");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let project = tmp.join("proj");
    std::fs::create_dir_all(&project).unwrap();

    // Manual pin → default worktree at the pin, version label = the pin.
    let pinned = LibEntry {
        name: "mylib".into(),
        repo: Some(repo.clone()),
        r#ref: Some("v1.0.0".into()),
        ..Default::default()
    };
    let r = resolve(&pinned, &project, &cache_root).unwrap();
    assert_eq!(r.worktree, "default");
    assert_eq!(r.version, "v1.0.0");
    assert_eq!(std::fs::read_to_string(r.path.join("src/lib.rs")).unwrap(), "// v1");

    // No pin, no lockfile → default branch + a warning.
    let cache2 = tmp.join("cache2");
    let unpinned = LibEntry {
        name: "mylib".into(),
        ecosystem: Some(Ecosystem::Rust),
        repo: Some(repo),
        ..Default::default()
    };
    let r2 = resolve(&unpinned, &project, &cache2).unwrap();
    assert_eq!(r2.worktree, "default");
    assert_eq!(r2.version, "main");
    assert_eq!(r2.warnings.len(), 1);
}

#[test]
fn layout_override_applies_and_meta_caches_detection() {
    let tmp = unique_tmp("resolve-layout");
    let repo = fixture_repo(&tmp.join("upstream"));
    let cache_root = tmp.join("cache");
    let entry = LibEntry {
        name: "mylib".into(),
        repo: Some(repo),
        r#ref: Some("v1.0.0".into()),
        docs_dir: Some("docs/special".into()),
        ..Default::default()
    };
    let r = resolve(&entry, &tmp, &cache_root).unwrap();
    assert_eq!(r.layout.docs_dir.as_deref(), Some("docs/special")); // override wins
    let meta = devkit_docs::cache::read_meta(&cache_root.join("mylib"));
    // meta stores the DETECTED layout (docs), not the override.
    assert_eq!(meta.layouts["default"].docs_dir.as_deref(), Some("docs"));
}
```

- [ ] **Step 2: RED** — `cargo test -p devkit-docs --test resolve` fails to compile.

- [ ] **Step 3: Implement `resolve.rs`**

```rust
//! The lookup facade: entry + CWD → version-correct checkout path.
//! Order: manual `ref` pin → lockfile version → tag probe → `default`
//! worktree fallback (with a warning). Every success records a reference row.

use crate::cache::{self, LibCache};
use crate::layout::{self, Layout};
use crate::lockfiles;
use crate::manifest::{Ecosystem, LibEntry};
use crate::refs::RefStore;
use crate::tags;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct Resolved {
    pub name: String,
    /// Human-facing version: lockfile version, the pinned ref, or the branch name.
    pub version: String,
    /// Worktree dirname — also the version recorded in the reference registry.
    pub worktree: String,
    pub path: PathBuf,
    pub layout: Layout,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Nearest dir containing `devkit.toml` walking up, else `start` itself —
/// the project identity recorded in the reference registry when no lockfile
/// pins the version.
pub fn project_root(start: &Path) -> PathBuf {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("devkit.toml").is_file() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    start.to_path_buf()
}

pub fn resolve(entry: &LibEntry, start: &Path, cache_root: &Path) -> Result<Resolved> {
    let repo = entry
        .repo
        .as_deref()
        .with_context(|| format!("lib `{}` has no repo url", entry.name))?;
    let lib = LibCache::new(cache_root, &entry.name);
    lib.ensure_clone(repo)?;
    let mut warnings = Vec::new();
    let mut meta = cache::read_meta(&lib.dir);

    let (worktree, version, path, project) = if let Some(pin) = entry.r#ref.as_deref() {
        // A changed pin is re-pointed by `docm sync`, not on every lookup.
        let path = lib.ensure_worktree("default", pin)?;
        ("default".to_string(), pin.to_string(), path, project_root(start))
    } else {
        let eco = entry.ecosystem.with_context(|| {
            format!("lib `{}` has neither ecosystem nor ref", entry.name)
        })?;
        let hit = if eco == Ecosystem::Git {
            None
        } else {
            lockfiles::find_version(start, eco, &entry.package_name())
        };
        match hit {
            Some((root, versions)) => {
                let v = lockfiles::highest(versions.clone()).expect("non-empty versions");
                if versions.len() > 1 {
                    warnings.push(format!(
                        "lockfile holds {} versions of {}; using {v}",
                        versions.len(),
                        entry.package_name()
                    ));
                }
                match locate_tag(&lib, &mut meta, &entry.package_name(), &v)? {
                    Some(tag) => {
                        let path = lib.ensure_worktree(&v, &tag)?;
                        (v.clone(), v, path, root)
                    }
                    None => {
                        warnings.push(format!(
                            "no git tag found for {} {v}; falling back to the default branch",
                            entry.name
                        ));
                        let (w, ver, p) = default_worktree(&lib)?;
                        (w, ver, p, root)
                    }
                }
            }
            None => {
                if eco != Ecosystem::Git {
                    warnings.push(format!(
                        "no lockfile pins {}; using the default branch",
                        entry.package_name()
                    ));
                }
                let (w, ver, p) = default_worktree(&lib)?;
                (w, ver, p, project_root(start))
            }
        }
    };

    RefStore::at(cache_root).commit(|d| {
        d.record(&project.to_string_lossy(), &entry.name, &worktree);
        Ok(())
    })?;

    let detected = match meta.layouts.get(&worktree) {
        Some(l) => l.clone(),
        None => {
            let l = layout::detect(&path);
            meta.layouts.insert(worktree.clone(), l.clone());
            l
        }
    };
    cache::write_meta(&lib.dir, &meta)?;

    Ok(Resolved {
        name: entry.name.clone(),
        version,
        worktree,
        path,
        layout: layout::with_overrides(detected, entry),
        notes: entry.notes.clone(),
        warnings,
    })
}

fn default_worktree(lib: &LibCache) -> Result<(String, String, PathBuf)> {
    let branch = lib.default_branch()?;
    let path = lib.ensure_worktree("default", &branch)?;
    Ok(("default".to_string(), branch, path))
}

/// Cached pattern first; then a probe; then one fetch (the version may be
/// newer than the last fetch) and a final probe.
fn locate_tag(
    lib: &LibCache,
    meta: &mut cache::Meta,
    package: &str,
    version: &str,
) -> Result<Option<String>> {
    let tags_now = lib.tags()?;
    if let Some(p) = meta.tag_pattern {
        let t = tags::apply(p, package, version);
        if tags_now.contains(&t) {
            return Ok(Some(t));
        }
    }
    if let Some((p, t)) = tags::find(&tags_now, package, version) {
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

Add `pub mod resolve;` to `lib.rs`.

- [ ] **Step 4: GREEN** — `cargo test -p devkit-docs --test resolve` (3 passed), full crate tests, clippy + fmt.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs
git commit -m "feat(docs): resolve entries to version-correct checkouts"
```

---

### Task 8: Package-registry lookup

**Files:**
- Create: `crates/devkit-docs/src/lookup.rs`
- Modify: `crates/devkit-docs/src/lib.rs` (add `pub mod lookup;`)

**Interfaces:**
- Consumes: `manifest::Ecosystem`.
- Produces: `lookup::Registry` trait with `fn repo_url(&self, eco: Ecosystem, package: &str) -> Result<String>`; `lookup::Http` (real impl, ureq); `lookup::detect(reg: &dyn Registry, package: &str) -> Result<(Ecosystem, String)>` (probes Rust → Js → Python); `lookup::extract(eco: Ecosystem, v: &serde_json::Value) -> Result<String>`; `lookup::normalize(raw: &str) -> String`; `lookup::name_from_url(url: &str) -> String`.

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-docs/src/lookup.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Ecosystem;
    use serde_json::json;

    #[test]
    fn extract_per_registry_shapes() {
        let crates = json!({ "crate": { "repository": "https://github.com/tokio-rs/tokio" } });
        assert_eq!(extract(Ecosystem::Rust, &crates).unwrap(), "https://github.com/tokio-rs/tokio");
        let npm = json!({ "repository": { "url": "git+https://github.com/facebook/react.git" } });
        assert_eq!(extract(Ecosystem::Js, &npm).unwrap(), "https://github.com/facebook/react.git");
        let npm_str = json!({ "repository": "github-user/repo-shorthand-is-rejected" });
        assert!(extract(Ecosystem::Js, &npm_str).is_err());
        let pypi = json!({ "info": { "project_urls": { "Homepage": "https://requests.io", "Source": "https://github.com/psf/requests" } } });
        assert_eq!(extract(Ecosystem::Python, &pypi).unwrap(), "https://github.com/psf/requests");
        assert!(extract(Ecosystem::Rust, &json!({})).is_err());
    }

    #[test]
    fn normalize_strips_git_plus_and_fragment() {
        assert_eq!(normalize("git+https://github.com/x/y.git"), "https://github.com/x/y.git");
        assert_eq!(normalize("https://github.com/x/y#readme"), "https://github.com/x/y");
        assert_eq!(normalize("https://github.com/x/y/"), "https://github.com/x/y");
    }

    #[test]
    fn name_from_url_takes_repo_leaf() {
        assert_eq!(name_from_url("https://github.com/godotengine/godot"), "godot");
        assert_eq!(name_from_url("git@github.com:tokio-rs/tokio.git"), "tokio");
        assert_eq!(name_from_url("https://github.com/x/y.git/"), "y");
    }

    #[test]
    fn detect_probes_ecosystems_in_order() {
        struct Stub;
        impl Registry for Stub {
            fn repo_url(&self, eco: Ecosystem, _p: &str) -> anyhow::Result<String> {
                match eco {
                    Ecosystem::Js => Ok("https://github.com/facebook/react".into()),
                    _ => anyhow::bail!("not found"),
                }
            }
        }
        let (eco, url) = detect(&Stub, "react").unwrap();
        assert_eq!(eco, Ecosystem::Js);
        assert_eq!(url, "https://github.com/facebook/react");

        struct Never;
        impl Registry for Never {
            fn repo_url(&self, _e: Ecosystem, _p: &str) -> anyhow::Result<String> {
                anyhow::bail!("nope")
            }
        }
        assert!(detect(&Never, "ghost").is_err());
    }
}
```

- [ ] **Step 2: RED** — `cargo test -p devkit-docs lookup` fails to compile.

- [ ] **Step 3: Implement**

```rust
//! Package-registry lookups: package name → repo URL, resolved once at
//! `docm add` time and stored in the manifest. HTTP sits behind a trait so
//! tests stub it and nothing else ever touches the network.

use crate::manifest::Ecosystem;
use anyhow::{Context, Result, bail};

pub trait Registry {
    fn repo_url(&self, eco: Ecosystem, package: &str) -> Result<String>;
}

/// Real lookups. crates.io rejects requests without a User-Agent.
pub struct Http;

impl Registry for Http {
    fn repo_url(&self, eco: Ecosystem, package: &str) -> Result<String> {
        let v: serde_json::Value = match eco {
            Ecosystem::Rust => ureq::get(&format!("https://crates.io/api/v1/crates/{package}"))
                .set("User-Agent", "devkit-docm (https://github.com/AbysmalBiscuit/devkit)")
                .call()?
                .into_json()?,
            Ecosystem::Js => ureq::get(&format!("https://registry.npmjs.org/{package}"))
                .call()?
                .into_json()?,
            Ecosystem::Python => ureq::get(&format!("https://pypi.org/pypi/{package}/json"))
                .call()?
                .into_json()?,
            Ecosystem::Git => bail!("git entries carry an explicit repo URL"),
        };
        extract(eco, &v)
    }
}

/// Probe registries in order (crates.io, npm, PyPI); first hit wins.
pub fn detect(reg: &dyn Registry, package: &str) -> Result<(Ecosystem, String)> {
    let mut errs = Vec::new();
    for eco in [Ecosystem::Rust, Ecosystem::Js, Ecosystem::Python] {
        match reg.repo_url(eco, package) {
            Ok(url) => return Ok((eco, url)),
            Err(e) => errs.push(format!("{eco}: {e}")),
        }
    }
    bail!(
        "`{package}` not found on crates.io, npm, or PyPI ({}); pass --eco or a git URL",
        errs.join("; ")
    )
}

/// Pull the repository URL out of one registry's JSON response.
pub fn extract(eco: Ecosystem, v: &serde_json::Value) -> Result<String> {
    let raw = match eco {
        Ecosystem::Rust => v
            .pointer("/crate/repository")
            .and_then(|x| x.as_str())
            .context("crates.io response has no crate.repository")?,
        Ecosystem::Js => v
            .pointer("/repository/url")
            .and_then(|x| x.as_str())
            .context("npm response has no repository.url")?,
        Ecosystem::Python => {
            let urls = v
                .pointer("/info/project_urls")
                .and_then(|x| x.as_object())
                .context("PyPI response has no info.project_urls")?;
            ["Repository", "Source", "Source Code", "Code", "Homepage"]
                .iter()
                .filter_map(|k| urls.get(*k).and_then(|x| x.as_str()))
                .find(|u| u.contains("github.com") || u.contains("gitlab.com"))
                .context("PyPI project_urls has no recognizable repo link")?
        }
        Ecosystem::Git => bail!("git entries carry an explicit repo URL"),
    };
    let url = normalize(raw);
    if !url.contains("://") {
        bail!("`{url}` is not a full repo URL");
    }
    Ok(url)
}

pub fn normalize(raw: &str) -> String {
    let s = raw.trim().trim_start_matches("git+");
    let s = s.split('#').next().unwrap_or(s);
    s.trim_end_matches('/').to_string()
}

pub fn name_from_url(url: &str) -> String {
    url.trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(url)
        .to_string()
}
```

Add `pub mod lookup;` to `lib.rs`.

- [ ] **Step 4: GREEN** — `cargo test -p devkit-docs lookup` (4 passed), clippy + fmt.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs
git commit -m "feat(docs): resolve repo urls from package registries"
```

---

### Task 9: Manifest mutation (add/rm targets)

**Files:**
- Modify: `crates/devkit-docs/src/manifest.rs`

**Interfaces:**
- Produces: `manifest::load_global(path: &Path) -> Result<DocsManifest>`; `manifest::upsert_global(path: &Path, entry: &LibEntry) -> Result<()>`; `manifest::remove_global(path: &Path, name: &str) -> Result<bool>`; `manifest::upsert_project(devkit_toml: &Path, entry: &LibEntry) -> Result<()>`; `manifest::remove_project(devkit_toml: &Path, name: &str) -> Result<bool>` (bool = an entry was removed). Project edits go through `toml_edit` and must not disturb unrelated content or comments.

- [ ] **Step 1: Write the failing tests**

Append to the test module in `manifest.rs`:

```rust
    #[test]
    fn upsert_global_creates_replaces_and_remove_deletes() {
        let root = unique_tmp("global");
        let path = root.join("docs.toml");
        let e = LibEntry { name: "tokio".into(), ecosystem: Some(Ecosystem::Rust), repo: Some("u1".into()), ..Default::default() };
        upsert_global(&path, &e).unwrap();
        let e2 = LibEntry { repo: Some("u2".into()), ..e.clone() };
        upsert_global(&path, &e2).unwrap();
        let m = load_global(&path).unwrap();
        assert_eq!(m.libs.len(), 1);
        assert_eq!(m.libs[0].repo.as_deref(), Some("u2"));
        assert!(remove_global(&path, "tokio").unwrap());
        assert!(!remove_global(&path, "tokio").unwrap());
        assert!(load_global(&path).unwrap().libs.is_empty());
    }

    #[test]
    fn upsert_project_preserves_comments_and_replaces_by_name() {
        let root = unique_tmp("project");
        let path = root.join("devkit.toml");
        std::fs::write(&path, "# keep me\n[defaults]\napps_dir = 'apps' # inline\n").unwrap();
        let e = LibEntry { name: "react".into(), ecosystem: Some(Ecosystem::Js), repo: Some("r1".into()), ..Default::default() };
        upsert_project(&path, &e).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("# keep me") && s.contains("# inline"));
        assert!(s.contains("[[docs.libs]]"));

        let e2 = LibEntry { repo: Some("r2".into()), ..e };
        upsert_project(&path, &e2).unwrap();
        let d = discover(path.parent().unwrap(), Some(&root.join("nope.toml"))).unwrap();
        assert_eq!(d.manifest.libs.len(), 1);
        assert_eq!(d.manifest.libs[0].repo.as_deref(), Some("r2"));

        assert!(remove_project(&path, "react").unwrap());
        assert!(!remove_project(&path, "react").unwrap());
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("# keep me")); // untouched content survives removal too
    }
```

- [ ] **Step 2: RED** — `cargo test -p devkit-docs manifest` fails to compile.

- [ ] **Step 3: Implement**

Add to `manifest.rs`:

```rust
pub fn load_global(path: &Path) -> Result<DocsManifest> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(_) => Ok(DocsManifest::default()),
    }
}

/// The global file is docm-owned and machine-written — a full serialize is fine.
pub fn upsert_global(path: &Path, entry: &LibEntry) -> Result<()> {
    let mut m = load_global(path)?;
    match m.libs.iter_mut().find(|l| l.name == entry.name) {
        Some(l) => *l = entry.clone(),
        None => m.libs.push(entry.clone()),
    }
    write_global(path, &m)
}

pub fn remove_global(path: &Path, name: &str) -> Result<bool> {
    let mut m = load_global(path)?;
    let before = m.libs.len();
    m.libs.retain(|l| l.name != name);
    let removed = m.libs.len() != before;
    if removed {
        write_global(path, &m)?;
    }
    Ok(removed)
}

fn write_global(path: &Path, m: &DocsManifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(m)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// devkit.toml is hand-maintained — edit via toml_edit so comments and
/// formatting survive.
pub fn upsert_project(devkit_toml: &Path, entry: &LibEntry) -> Result<()> {
    let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(devkit_toml)
        .with_context(|| format!("reading {}", devkit_toml.display()))?
        .parse()
        .with_context(|| format!("parsing {}", devkit_toml.display()))?;
    let tbl = toml_edit::ser::to_document(entry)
        .context("serializing lib entry")?
        .as_table()
        .clone();
    let root = doc.as_table_mut();
    let docs = root
        .entry("docs")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let docs_tbl = docs
        .as_table_mut()
        .context("[docs] in devkit.toml is not a table")?;
    docs_tbl.set_implicit(true); // no bare [docs] header, just [[docs.libs]]
    let libs = docs_tbl
        .entry("libs")
        .or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    let arr = libs
        .as_array_of_tables_mut()
        .context("docs.libs in devkit.toml is not an array of tables")?;
    match arr
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(entry.name.as_str()))
    {
        Some(existing) => *existing = tbl,
        None => arr.push(tbl),
    }
    std::fs::write(devkit_toml, doc.to_string())
        .with_context(|| format!("writing {}", devkit_toml.display()))?;
    Ok(())
}

pub fn remove_project(devkit_toml: &Path, name: &str) -> Result<bool> {
    let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(devkit_toml)
        .with_context(|| format!("reading {}", devkit_toml.display()))?
        .parse()
        .with_context(|| format!("parsing {}", devkit_toml.display()))?;
    let Some(arr) = doc
        .get_mut("docs")
        .and_then(|d| d.get_mut("libs"))
        .and_then(|l| l.as_array_of_tables_mut())
    else {
        return Ok(false);
    };
    let before = arr.len();
    arr.retain(|t| t.get("name").and_then(|v| v.as_str()) != Some(name));
    let removed = arr.len() != before;
    if removed {
        std::fs::write(devkit_toml, doc.to_string())
            .with_context(|| format!("writing {}", devkit_toml.display()))?;
    }
    Ok(removed)
}
```

- [ ] **Step 4: GREEN** — `cargo test -p devkit-docs manifest`, full crate tests, clippy + fmt.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs
git commit -m "feat(docs): write global and project manifest entries"
```

---

### Task 10: `docm` binary

**Files:**
- Create: `src/bin/docm.rs`

**Interfaces:**
- Consumes: everything the crate exports (Tasks 1–9).
- Produces: the CLI surface from the spec: `add`, `rm`, `list [--json]`, `sync [names…]`, `path <name>`, `info <name> [--json]`, `prune [--yes]`, `completions <shell>`. `docm path` prints exactly one path on stdout.

- [ ] **Step 1: Write the binary**

Create `src/bin/docm.rs`:

```rust
use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use devkit_docs::manifest::{self, Discovered, Ecosystem, LibEntry};
use devkit_docs::{cache, lockfiles, lookup, refs, resolve};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "docm",
    about = "Version-correct local library docs and source checkouts"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Register a library: a package name (looked up on crates.io/npm/PyPI) or a git URL.
    Add {
        target: String,
        /// Ecosystem; omitted → probe crates.io, npm, PyPI in order.
        #[arg(long)]
        eco: Option<Ecosystem>,
        /// Registry package name when it differs from the lib name (e.g. @types/node).
        #[arg(long)]
        package: Option<String>,
        /// Repo URL override (skips the registry lookup).
        #[arg(long)]
        repo: Option<String>,
        /// Pin a git ref (tag/branch/sha) instead of lockfile resolution.
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Layout override: source directory inside the checkout.
        #[arg(long)]
        src_dir: Option<String>,
        /// Layout override: docs directory inside the checkout.
        #[arg(long)]
        docs_dir: Option<String>,
        /// Freeform notes surfaced by `docm info`.
        #[arg(long)]
        notes: Option<String>,
        /// Write to the nearest devkit.toml [docs] section instead of the global manifest.
        #[arg(long)]
        project: bool,
    },
    /// Remove a library from the manifest (checkouts are reclaimed by prune).
    Rm {
        name: String,
        #[arg(long)]
        project: bool,
    },
    /// List registered libraries and their synced checkouts.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Fetch cloned repos and move default worktrees to their target.
    Sync { names: Vec<String> },
    /// Print the version-resolved checkout path (exactly one line on stdout).
    Path { name: String },
    /// Print checkout path, resolved version, layout map, and notes.
    Info {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete version checkouts no existing project references.
    Prune {
        /// Also delete unregistered libs without asking.
        #[arg(long)]
        yes: bool,
    },
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions { shell: Shell },
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("docm");
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Add { target, eco, package, repo, git_ref, src_dir, docs_dir, notes, project } => {
            cmd_add(target, eco, package, repo, git_ref, src_dir, docs_dir, notes, project)
        }
        Cmd::Rm { name, project } => cmd_rm(&name, project),
        Cmd::List { json } => cmd_list(json),
        Cmd::Sync { names } => cmd_sync(&names),
        Cmd::Path { name } => cmd_path(&name),
        Cmd::Info { name, json } => cmd_info(&name, json),
        Cmd::Prune { yes } => cmd_prune(yes),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "docm", &mut std::io::stdout());
            Ok(())
        }
    }
}

fn cwd() -> Result<PathBuf> {
    std::env::current_dir().context("resolving current directory")
}

fn discovered() -> Result<Discovered> {
    manifest::discover(&cwd()?, None)
}

fn find_entry(d: &Discovered, name: &str) -> Result<LibEntry> {
    d.manifest
        .libs
        .iter()
        .find(|l| l.name == name)
        .cloned()
        .with_context(|| {
            format!("`{name}` is not registered — see `docm list`, or `docm add {name}`")
        })
}

#[allow(clippy::too_many_arguments)]
fn cmd_add(
    target: String,
    eco: Option<Ecosystem>,
    package: Option<String>,
    repo: Option<String>,
    git_ref: Option<String>,
    src_dir: Option<String>,
    docs_dir: Option<String>,
    notes: Option<String>,
    project: bool,
) -> Result<()> {
    let is_url = target.contains("://") || target.starts_with("git@");
    let mut entry = if is_url {
        LibEntry {
            name: lookup::name_from_url(&target),
            ecosystem: Some(Ecosystem::Git),
            repo: Some(lookup::normalize(&target)),
            ..Default::default()
        }
    } else {
        let pkg = package.clone().unwrap_or_else(|| target.clone());
        let (eco, repo) = match (eco, repo) {
            (Some(e), Some(r)) => (e, r),
            (Some(Ecosystem::Git), None) => {
                anyhow::bail!("--eco git needs a git URL target or --repo")
            }
            (Some(e), None) => (e, lookup::Registry::repo_url(&lookup::Http, e, &pkg)?),
            (None, r) => {
                let (e, url) = lookup::detect(&lookup::Http, &pkg)?;
                (e, r.unwrap_or(url))
            }
        };
        LibEntry {
            name: target.clone(),
            ecosystem: Some(eco),
            package,
            repo: Some(lookup::normalize(&repo)),
            ..Default::default()
        }
    };
    entry.r#ref = git_ref;
    entry.src_dir = src_dir;
    entry.docs_dir = docs_dir;
    entry.notes = notes;

    let dest = if project {
        let d = discovered()?;
        let path = d
            .project_devkit_toml
            .context("no devkit.toml found walking up from CWD (required for --project)")?;
        manifest::upsert_project(&path, &entry)?;
        path
    } else {
        let path = manifest::global_docs_path();
        manifest::upsert_global(&path, &entry)?;
        path
    };
    println!(
        "registered {} ({}) -> {} in {}",
        entry.name,
        entry.ecosystem.map(|e| e.to_string()).unwrap_or_default(),
        entry.repo.as_deref().unwrap_or("-"),
        dest.display()
    );
    Ok(())
}

fn cmd_rm(name: &str, project: bool) -> Result<()> {
    let removed = if project {
        let d = discovered()?;
        let path = d
            .project_devkit_toml
            .context("no devkit.toml found walking up from CWD (required for --project)")?;
        manifest::remove_project(&path, name)?
    } else {
        manifest::remove_global(&manifest::global_docs_path(), name)?
    };
    if removed {
        println!("removed {name}; run `docm prune` to reclaim its checkouts");
        Ok(())
    } else {
        anyhow::bail!("`{name}` was not in the {} manifest", if project { "project" } else { "global" })
    }
}

fn cmd_list(json: bool) -> Result<()> {
    let d = discovered()?;
    let root = cache::docs_cache_root();
    if json {
        let items: Vec<serde_json::Value> = d
            .manifest
            .libs
            .iter()
            .map(|l| {
                let synced: Vec<String> = cache::LibCache::new(&root, &l.name)
                    .version_worktrees()
                    .into_iter()
                    .map(|(n, _)| n)
                    .collect();
                serde_json::json!({
                    "name": l.name,
                    "ecosystem": l.ecosystem,
                    "package": l.package_name(),
                    "repo": l.repo,
                    "ref": l.r#ref,
                    "synced": synced,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items)?);
        return Ok(());
    }
    if d.manifest.libs.is_empty() {
        println!("no libraries registered — `docm add <package>` or `docm add <git-url>`");
        return Ok(());
    }
    for l in &d.manifest.libs {
        let eco = l.ecosystem.map(|e| e.to_string()).unwrap_or_else(|| "?".into());
        let synced: Vec<String> = cache::LibCache::new(&root, &l.name)
            .version_worktrees()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        let synced = if synced.is_empty() { "(not synced)".to_string() } else { synced.join(", ") };
        println!(
            "{:<24} {:<7} {:<16} {synced}",
            l.name,
            eco,
            l.r#ref.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

fn cmd_sync(names: &[String]) -> Result<()> {
    let d = discovered()?;
    let root = cache::docs_cache_root();
    let selected: Vec<&LibEntry> = d
        .manifest
        .libs
        .iter()
        .filter(|l| names.is_empty() || names.contains(&l.name))
        .collect();
    if let Some(unknown) = names.iter().find(|n| !d.manifest.libs.iter().any(|l| &&l.name == n)) {
        anyhow::bail!("`{unknown}` is not registered — see `docm list`");
    }
    for l in selected {
        let lib = cache::LibCache::new(&root, &l.name);
        if !lib.cloned() {
            eprintln!("docm: {} not cloned yet (materialized on first lookup); skipping", l.name);
            continue;
        }
        lib.fetch().with_context(|| format!("fetching {}", l.name))?;
        lib.sync_default(l.r#ref.as_deref())?;
        println!("synced {}", l.name);
    }
    Ok(())
}

fn resolve_one(name: &str) -> Result<resolve::Resolved> {
    let d = discovered()?;
    let entry = find_entry(&d, name)?;
    let r = resolve::resolve(&entry, &cwd()?, &cache::docs_cache_root())?;
    for w in &r.warnings {
        eprintln!("docm: {w}");
    }
    Ok(r)
}

fn cmd_path(name: &str) -> Result<()> {
    let r = resolve_one(name)?;
    println!("{}", r.path.display());
    Ok(())
}

fn cmd_info(name: &str, json: bool) -> Result<()> {
    let r = resolve_one(name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("name     {}", r.name);
    println!("version  {}", r.version);
    println!("path     {}", r.path.display());
    if let Some(d) = &r.layout.docs_dir {
        println!("docs     {d}");
    }
    if let Some(s) = &r.layout.src_dir {
        println!("src      {s}");
    }
    if let Some(e) = &r.layout.examples_dir {
        println!("examples {e}");
    }
    if let Some(k) = &r.layout.kind {
        println!("kind     {k}");
    }
    if let Some(n) = &r.notes {
        println!("notes    {n}");
    }
    Ok(())
}

fn cmd_prune(yes: bool) -> Result<()> {
    let d = discovered()?;
    let root = cache::docs_cache_root();
    if !root.is_dir() {
        println!("cache is empty");
        return Ok(());
    }
    let store = refs::RefStore::at(&root);
    let data = store.snapshot();

    let mut worktrees: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in std::fs::read_dir(&root)?.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let dirs = cache::LibCache::new(&root, &name)
            .version_worktrees()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        worktrees.insert(name, dirs);
    }
    let manifest_libs: BTreeSet<String> =
        d.manifest.libs.iter().map(|l| l.name.clone()).collect();

    let plan = refs::plan(&data, &worktrees, &manifest_libs, |project, lib| {
        let entry = d.manifest.libs.iter().find(|l| l.name == lib)?;
        current_version(entry, Path::new(project))
    });

    for (lib, wt) in &plan.delete {
        cache::LibCache::new(&root, lib).remove_worktree(wt)?;
        println!("removed {lib}/{wt}");
    }
    if !plan.removable_libs.is_empty() {
        println!(
            "unregistered libs with no references: {}",
            plan.removable_libs.join(", ")
        );
        if yes || confirm("delete them entirely? [y/N] ")? {
            for lib in &plan.removable_libs {
                std::fs::remove_dir_all(root.join(lib))
                    .with_context(|| format!("deleting {lib}"))?;
                println!("deleted {lib}");
            }
        }
    }
    // A resolution racing this rewrite re-records itself on its next lookup,
    // so replacing rows with the plan's survivors is safe.
    store.commit(|data| {
        data.rows = plan.keep.clone();
        Ok(())
    })?;
    if plan.delete.is_empty() && plan.removable_libs.is_empty() {
        println!("nothing to prune");
    }
    Ok(())
}

/// What a live project pins right now; `None` = it no longer references the lib.
fn current_version(entry: &LibEntry, project: &Path) -> Option<String> {
    if entry.r#ref.is_some() {
        return Some("default".into());
    }
    let eco = entry.ecosystem?;
    if eco == Ecosystem::Git {
        return Some("default".into());
    }
    let (_, versions) = lockfiles::find_version(project, eco, &entry.package_name())?;
    lockfiles::highest(versions)
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(matches!(s.trim(), "y" | "Y" | "yes"))
}
```

- [ ] **Step 2: Build and exercise end-to-end against a fixture**

Run: `cargo build --bin docm` — expected: clean build (bins under `src/bin/*.rs` are auto-discovered; no Cargo.toml edit needed).

Smoke-check against a throwaway HOME so nothing real is touched (fish or bash):

```bash
T=$(mktemp -d)
git init -b main "$T/up" && git -C "$T/up" -c user.email=t@t -c user.name=t -c commit.gpgsign=false commit --allow-empty -m x && git -C "$T/up" -c tag.gpgsign=false tag v1.0.0
env HOME="$T" XDG_CACHE_HOME="$T/cache" target/debug/docm add "file://$T/up" --ref v1.0.0
env HOME="$T" XDG_CACHE_HOME="$T/cache" target/debug/docm list
env HOME="$T" XDG_CACHE_HOME="$T/cache" target/debug/docm path up
env HOME="$T" XDG_CACHE_HOME="$T/cache" target/debug/docm info up
env HOME="$T" XDG_CACHE_HOME="$T/cache" target/debug/docm prune --yes
```

Expected: `add` writes `$T/.config/devkit/docs.toml`; `path` prints exactly one line (`$T/cache/devkit/docs/up/default`); `info` shows `version v1.0.0`; `prune` reports nothing to prune (the `default` worktree is exempt). Record the actual output in the report.

- [ ] **Step 3: Full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`

- [ ] **Step 4: Commit**

```bash
git add src/bin/docm.rs
git commit -m "feat(docs): add docm CLI"
```

---

### Task 11: `devkit doctor` docs row

**Files:**
- Modify: `crates/devkit-docs/src/lib.rs` (add `doctor_summary`), `src/bin/devkit/doctor.rs`

**Interfaces:**
- Consumes: `cache::{docs_cache_root, dir_size, LibCache}`, `refs::RefStore`.
- Produces: `devkit_docs::DocsDoctor { libs: usize, bytes: u64, unreferenced: usize }`; `devkit_docs::doctor_summary(cache_root: &Path) -> DocsDoctor`; a `docs_cache` row in `devkit doctor`.

- [ ] **Step 1: Write the failing test**

Append a test module to `crates/devkit-docs/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_summary_counts_libs_and_unreferenced_worktrees() {
        let root = std::env::temp_dir().join(format!("devkit-docs-dr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // One lib with a referenced worktree, an unreferenced one, and default.
        for wt in ["1.0.0", "2.0.0", "default", "repo.git"] {
            std::fs::create_dir_all(root.join("tokio").join(wt)).unwrap();
        }
        std::fs::write(root.join("tokio/1.0.0/f"), "x").unwrap();
        refs::RefStore::at(&root)
            .commit(|d| {
                d.record("/some/project", "tokio", "1.0.0");
                Ok(())
            })
            .unwrap();
        let s = doctor_summary(&root);
        assert_eq!(s.libs, 1);
        assert_eq!(s.unreferenced, 1); // 2.0.0 (default + repo.git exempt)
        assert!(s.bytes > 0);
    }
}
```

- [ ] **Step 2: RED** — `cargo test -p devkit-docs doctor_summary` fails to compile.

- [ ] **Step 3: Implement in `lib.rs`**

```rust
use std::path::Path;

pub struct DocsDoctor {
    pub libs: usize,
    pub bytes: u64,
    pub unreferenced: usize,
}

/// Cheap health summary for `devkit doctor`: lib count, cache size, and
/// version worktrees no registry row references.
pub fn doctor_summary(cache_root: &Path) -> DocsDoctor {
    let mut out = DocsDoctor { libs: 0, bytes: cache::dir_size(cache_root), unreferenced: 0 };
    let data = refs::RefStore::at(cache_root).snapshot();
    let referenced: std::collections::BTreeSet<(String, String)> = data
        .rows
        .iter()
        .map(|r| (r.lib.clone(), r.version.clone()))
        .collect();
    let Ok(rd) = std::fs::read_dir(cache_root) else { return out };
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        out.libs += 1;
        for (wt, _) in cache::LibCache::new(cache_root, &name).version_worktrees() {
            if wt != "default" && !referenced.contains(&(name.clone(), wt)) {
                out.unreferenced += 1;
            }
        }
    }
    out
}
```

- [ ] **Step 4: Add the doctor row**

In `src/bin/devkit/doctor.rs`, append to the `vec![...]` in `gather`:

```rust
        Row {
            key: "docs_cache",
            source: Source::Unset,
            check: docs_cache_check(),
        },
```

and add the check function beside the other validators:

```rust
fn docs_cache_check() -> Check {
    let root = devkit_common::paths::cache_dir().join("docs");
    if !root.is_dir() {
        return Check::Ok("empty".into());
    }
    let s = devkit_docs::doctor_summary(&root);
    let msg = format!(
        "{} libs, {} MiB, {} unreferenced checkouts",
        s.libs,
        s.bytes / (1024 * 1024),
        s.unreferenced
    );
    if s.unreferenced > 0 {
        Check::Warn(format!("{msg} — run `docm prune`"))
    } else {
        Check::Ok(msg)
    }
}
```

(Adapt mechanically if `gather`'s signature or `Source` variants differ — follow the existing rows.)

- [ ] **Step 5: GREEN + gate**

Run: `cargo test -p devkit-docs doctor_summary`, then `cargo build --bin devkit && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`. Run `target/debug/devkit doctor` once and check the `docs_cache` row renders.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-docs src/bin/devkit/doctor.rs
git commit -m "feat(devkit): report docs cache health in doctor"
```

---

### Task 12: `devkit:docs` skill + documentation

**Files:**
- Create: `skills/docs/SKILL.md`
- Modify: `AGENTS.md`, `README.md`

**Interfaces:**
- Consumes: the `docm` CLI contract (`list`, `info`, `add`).
- Produces: the plugin-shipped skill (auto-discovered — `.claude-plugin/plugin.json` has no skills key and `.codex-plugin`/`.cursor-plugin` point at `./skills/`, so no manifest edits).

- [ ] **Step 1: Write the skill**

Create `skills/docs/SKILL.md`:

```markdown
---
name: docs
description: Use when the user asks how to use, configure, or debug an external library or framework (e.g. "how do I cancel a JoinHandle in tokio", "what does this godot node do"), or invokes /docs. Resolves a version-correct local checkout of the library's source and docs via the docm CLI, then searches it. First word of the argument is the library name; the rest is the question.
---

# Library docs lookup

Answer library-usage questions from a local, version-correct checkout of the
library's own source and docs — not from memory.

## Steps

1. Identify the library: the first token of the `/docs` argument, or infer it
   from the question. `docm list` prints the registered names; match against
   those.
2. Run `docm info <lib>`. It prints the checkout path (version-matched to the
   current project's lockfile), the resolved version, a layout map
   (docs/src/examples dirs, doc system), and any notes. The first resolution
   of a new version fetches git blobs and can take a few seconds. Warnings on
   stderr (e.g. "falling back to default branch") are context — relay them if
   the answer depends on the version.
3. Search ONLY under the printed path: the docs dir for guides and concepts,
   the source dir for API ground truth, examples for usage patterns. Use
   `rg` for text and `ast-grep` for structural queries.
4. Answer with `file:line` citations relative to the checkout.

## Rules

- Never reuse a checkout path from memory or an earlier session — versions
  differ per project. Always re-run `docm info`.
- If the library is not registered: `docm add <package>` (registry lookup) or
  `docm add <git-url>`, then retry. Ask before adding with `--project`
  (that edits the repo's devkit.toml).
- `docm path <lib>` prints just the path when that is all you need.
- If `docm` is not on PATH, tell the user to `cargo install --path .` in the
  devkit repo.
```

- [ ] **Step 2: Document in AGENTS.md**

In the Layout table of `AGENTS.md`, after the `devkit-mcp` row, add:

```markdown
| `crates/devkit-docs` | lib: version-correct library checkouts — manifest (global `docs.toml` + `devkit.toml` `[docs]`), lockfile→tag resolution, bare-clone cache with per-version worktrees, flock'd reference registry with reference-based prune |
```

After the `src/bin/devkit` row, add:

```markdown
| `src/bin/docm.rs` | CLI over the docs cache: `add`, `rm`, `list`, `sync`, `path`, `info`, `prune` |
```

Update the sentence "The five user-facing CLIs (`portm`, `devrun`, `issue`, `lockm`, `devkit`) each expose a `completions <shell>` subcommand" to "The six user-facing CLIs (`portm`, `devrun`, `issue`, `lockm`, `devkit`, `docm`) each expose a `completions <shell>` subcommand via `clap_complete`."

- [ ] **Step 3: Document in README.md**

Add a `## docm` section beside the other per-binary sections (match their heading style and depth), containing:

```markdown
## docm

Version-correct local library checkouts backing the `devkit:docs` skill.
Register a library once; every lookup resolves the version the current
project's lockfile pins and materializes a detached worktree for it under
`~/.cache/devkit/docs/`.

```sh
docm add tokio                    # registry lookup (crates.io/npm/PyPI)
docm add https://github.com/godotengine/godot --ref 4.3-stable
docm add react --project          # write to this repo's devkit.toml [docs]
docm info tokio                   # path + version + layout map + notes
docm path tokio                   # just the checkout path
docm sync                         # fetch clones, move default worktrees
docm prune                        # drop checkouts no live project references
```

Global manifest: `~/.config/devkit/docs.toml`. Per-project overlay:
`[[docs.libs]]` entries in `devkit.toml` (same fields; partial entries
override the global entry field-by-field). Resolution: manual `ref` pin →
lockfile version (`Cargo.lock`, `pnpm-lock.yaml`, `package-lock.json`,
`uv.lock`) → git tag → default branch fallback.
```

- [ ] **Step 4: Verify + gate**

`cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all` (docs-only task, but the gate stays the ritual). Confirm `skills/docs/SKILL.md` frontmatter matches the `using-devkit` shape (name + description only).

- [ ] **Step 5: Commit**

```bash
git add skills/docs AGENTS.md README.md
git commit -m "feat(docs): ship devkit:docs skill and document docm"
```

---

## Self-Review Notes

- Spec coverage: manifest+merge (T1), lockfiles (T2), tag probe (T3), layout (T4), cache/clone/worktrees/sync (T5), registry+prune (T6), resolution order+recording (T7), registry lookup+escape hatch (T8), add/rm targets incl. toml_edit (T9), CLI surface + stdout contract (T10), doctor row (T11), skill+docs (T12). Out-of-scope items (baked skills, multi-clone entries, more ecosystems, MCP, daemon) have no tasks — by design.
- The multiprocess flock test (T6) mirrors `devkit-ports --test registry` per convention; no fixed sleeps anywhere (all git/process operations are synchronous `capture` calls).
- Type consistency spot-checks: `LibEntry.r#ref` used in T1/T7/T9/T10; `version_worktrees()` returns `Vec<(String, PathBuf)>` consumed in T10/T11; `refs::plan` signature matches T10's call; `Resolved.worktree` is the registry version string in T6/T7.
```
