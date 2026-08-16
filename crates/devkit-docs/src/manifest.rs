//! Manifest model: which libraries docm manages and where their repos live.
//!
//! Global entries live in `~/.config/devkit/docs.toml` (top-level `[[libs]]`);
//! per-project overrides live in a `[docs]` section (`[[docs.libs]]`) of the
//! project's `devkit.toml`. Layers merge field-by-field per lib name, the
//! deeper (more project-specific) layer winning.

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Which importer graph resolves a library's version, and therefore which
/// lockfile is consulted.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema, Deserialize, clap::ValueEnum,
)]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, JsonSchema, Deserialize)]
pub struct LibEntry {
    /// Id this library is addressed by on the `docm` command line, and the
    /// key an overlay entry merges onto.
    pub name: String,
    /// Which importer graph resolves the version. Omit to detect it from the
    /// project's lockfiles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ecosystem: Option<Ecosystem>,
    /// Registry package name when it differs from `name` (e.g. `@types/node`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Git URL to clone, skipping the registry lookup that would find it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Manual pin (tag/branch/sha); wins over lockfile resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// Source directory inside the checkout, overriding layout detection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_dir: Option<String>,
    /// Docs directory inside the checkout, overriding layout detection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_dir: Option<String>,
    /// Freeform note surfaced by `docm info` and `docm list` — what this
    /// library is here for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Which file this entry was read from. Set during `discover`, never
    /// serialized — writing it back would put an absolute path in a manifest
    /// that gets committed to a repo.
    #[serde(skip)]
    pub origin_file: Option<PathBuf>,
}

impl LibEntry {
    pub fn package_name(&self) -> String {
        self.package.clone().unwrap_or_else(|| self.name.clone())
    }
}

/// Libraries whose source is checked out at the version this project resolves.
/// A project's `[docs]` section overlays the global `docs.toml` entry by entry.
#[derive(Debug, Default, PartialEq, Serialize, JsonSchema, Deserialize)]
pub struct DocsManifest {
    /// One entry per managed library. Every field except `name` is optional so
    /// an overlay can override a single field of a global entry.
    #[serde(default)]
    pub libs: Vec<LibEntry>,
}

/// Overlay `over` onto `base`: same-name entries merge field-by-field
/// (`over`'s `Some` fields win), unknown names append.
pub fn merge(mut base: DocsManifest, over: DocsManifest) -> DocsManifest {
    for e in over.libs {
        match base.libs.iter_mut().find(|b| b.name == e.name) {
            Some(b) => {
                if e.ecosystem.is_some() {
                    b.ecosystem = e.ecosystem;
                }
                if e.package.is_some() {
                    b.package = e.package;
                }
                if e.repo.is_some() {
                    b.repo = e.repo;
                }
                if e.r#ref.is_some() {
                    b.r#ref = e.r#ref;
                }
                if e.src_dir.is_some() {
                    b.src_dir = e.src_dir;
                }
                if e.docs_dir.is_some() {
                    b.docs_dir = e.docs_dir;
                }
                if e.notes.is_some() {
                    b.notes = e.notes;
                }
                if e.origin_file.is_some() {
                    b.origin_file = e.origin_file;
                }
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
    let global_path = global
        .map(Path::to_path_buf)
        .unwrap_or_else(global_docs_path);
    let mut manifest = match std::fs::read_to_string(&global_path) {
        Ok(s) => {
            toml::from_str(&s).with_context(|| format!("parsing {}", global_path.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DocsManifest::default(),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", global_path.display()));
        }
    };
    stamp(&mut manifest, &global_path);

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
            if let Some(mut layer) = docs_layer(&c)? {
                stamp(&mut layer, &c);
                layers.push(layer);
            }
        }
        dir = d.parent();
    }
    for layer in layers.into_iter().rev() {
        manifest = merge(manifest, layer);
    }
    let mut problems: Vec<String> = Vec::new();
    let mut seen: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for l in &manifest.libs {
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
        bail!(
            "the docs manifest is not usable:\n  {}",
            problems.join("\n  ")
        );
    }
    Ok(Discovered {
        manifest,
        project_devkit_toml: nearest,
    })
}

fn stamp(manifest: &mut DocsManifest, path: &Path) {
    for lib in &mut manifest.libs {
        lib.origin_file = Some(path.to_path_buf());
    }
}

/// Extract the `[docs]` section of one `devkit.toml`, if present.
fn docs_layer(path: &Path) -> Result<Option<DocsManifest>> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let t: toml::Table = s
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    match t.get("docs") {
        Some(v) => {
            Ok(Some(v.clone().try_into().with_context(|| {
                format!("[docs] section in {}", path.display())
            })?))
        }
        None => Ok(None),
    }
}

pub fn load_global(path: &Path) -> Result<DocsManifest> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocsManifest::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The global file is docm-owned and machine-written — a full serialize is fine.
pub fn upsert_global(path: &Path, entry: &LibEntry, cache_root: &Path) -> Result<()> {
    crate::locks::with_manifest(cache_root, || {
        let mut m = load_global(path)?;
        crate::barrier::signal("manifest-ready")?;
        crate::barrier::wait("manifest-go")?;
        match m.libs.iter_mut().find(|l| l.name == entry.name) {
            Some(l) => *l = entry.clone(),
            None => m.libs.push(entry.clone()),
        }
        write_global(path, &m)
    })
}

pub fn remove_global(path: &Path, name: &str, cache_root: &Path) -> Result<bool> {
    crate::locks::with_manifest(cache_root, || {
        let mut m = load_global(path)?;
        let before = m.libs.len();
        m.libs.retain(|l| l.name != name);
        let removed = m.libs.len() != before;
        if removed {
            write_global(path, &m)?;
        }
        Ok(removed)
    })
}

fn write_global(path: &Path, m: &DocsManifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    atomic_write(path, toml::to_string_pretty(m)?)
}

fn atomic_write(path: &Path, contents: String) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contents)
        .with_context(|| format!("writing temporary manifest {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing manifest {}", path.display()))
}

/// Every key `LibEntry` models. A `[[docs.libs]]` table may carry others; an
/// upsert owns only these, so anything else the file holds stays as written.
const ENTRY_KEYS: [&str; 8] = [
    "name",
    "ecosystem",
    "package",
    "repo",
    "ref",
    "src_dir",
    "docs_dir",
    "notes",
];

/// devkit.toml is hand-maintained — edit via toml_edit so comments and
/// formatting survive, and patch the existing entry rather than replacing it,
/// so a key docm does not model survives a re-registration of its library.
pub fn upsert_project(devkit_toml: &Path, entry: &LibEntry, cache_root: &Path) -> Result<()> {
    let fresh = toml_edit::ser::to_document(entry)
        .context("serializing lib entry")?
        .as_table()
        .clone();
    let table = match project_entry(devkit_toml, &entry.name)? {
        Some(mut existing) => {
            patch_entry(&mut existing, &fresh);
            existing
        }
        None => fresh,
    };
    put_project_entry(devkit_toml, &entry.name, &table, cache_root)
}

/// Apply `fresh`'s modeled keys to `entry` one at a time: update a key the
/// entry already has in place so its comments and position hold, append one it
/// lacks, and drop one the registration no longer sets.
fn patch_entry(entry: &mut toml_edit::Table, fresh: &toml_edit::Table) {
    for key in ENTRY_KEYS {
        let Some(item) = fresh.get(key) else {
            entry.remove(key);
            continue;
        };
        match (
            entry.get_mut(key).and_then(toml_edit::Item::as_value_mut),
            item.as_value(),
        ) {
            (Some(slot), Some(value)) => {
                let decor = slot.decor().clone();
                *slot = value.clone();
                *slot.decor_mut() = decor;
            }
            _ => {
                entry.insert(key, item.clone());
            }
        }
    }
}

/// The `[[docs.libs]]` table for `name` exactly as the file carries it — key
/// order and comments included — so a rollback can put back what was there
/// instead of a re-serialization of it.
pub(crate) fn project_entry(devkit_toml: &Path, name: &str) -> Result<Option<toml_edit::Table>> {
    let doc: toml_edit::DocumentMut = std::fs::read_to_string(devkit_toml)
        .with_context(|| format!("reading {}", devkit_toml.display()))?
        .parse()
        .with_context(|| format!("parsing {}", devkit_toml.display()))?;
    Ok(doc
        .get("docs")
        .and_then(|docs| docs.get("libs"))
        .and_then(|libs| libs.as_array_of_tables())
        .and_then(|arr| arr.iter().find(|t| entry_name(t) == Some(name)).cloned()))
}

fn entry_name(table: &toml_edit::Table) -> Option<&str> {
    table.get("name").and_then(|value| value.as_str())
}

/// Write `tbl` as `name`'s entry, replacing an existing one in place.
pub(crate) fn put_project_entry(
    devkit_toml: &Path,
    name: &str,
    tbl: &toml_edit::Table,
    cache_root: &Path,
) -> Result<()> {
    crate::locks::with_manifest(cache_root, || {
        let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(devkit_toml)
            .with_context(|| format!("reading {}", devkit_toml.display()))?
            .parse()
            .with_context(|| format!("parsing {}", devkit_toml.display()))?;
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
            .or_insert(toml_edit::Item::ArrayOfTables(
                toml_edit::ArrayOfTables::new(),
            ));
        let arr = libs
            .as_array_of_tables_mut()
            .context("docs.libs in devkit.toml is not an array of tables")?;
        let mut tables: Vec<toml_edit::Table> = arr.iter().cloned().collect();
        match tables.iter_mut().find(|t| entry_name(t) == Some(name)) {
            Some(existing) => *existing = tbl.clone(),
            None => tables.push(tbl.clone()),
        }
        arr.clear();
        for table in tables {
            arr.push(table);
        }
        atomic_write(devkit_toml, doc.to_string())
    })
}

pub fn remove_project(devkit_toml: &Path, name: &str, cache_root: &Path) -> Result<bool> {
    crate::locks::with_manifest(cache_root, || {
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
            atomic_write(devkit_toml, doc.to_string())?;
        }
        Ok(removed)
    })
}

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
        let e = LibEntry {
            name: "tokio".into(),
            ..Default::default()
        };
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
        std::fs::write(
            &global,
            "[[libs]]\nname='tokio'\necosystem='rust'\nrepo='u'\n",
        )
        .unwrap();
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

    #[test]
    fn upsert_global_creates_replaces_and_remove_deletes() {
        let root = unique_tmp("global");
        let path = root.join("docs.toml");
        let e = LibEntry {
            name: "tokio".into(),
            ecosystem: Some(Ecosystem::Rust),
            repo: Some("u1".into()),
            ..Default::default()
        };
        upsert_global(&path, &e, &root).unwrap();
        let e2 = LibEntry {
            repo: Some("u2".into()),
            ..e.clone()
        };
        upsert_global(&path, &e2, &root).unwrap();
        let m = load_global(&path).unwrap();
        assert_eq!(m.libs.len(), 1);
        assert_eq!(m.libs[0].repo.as_deref(), Some("u2"));
        assert!(remove_global(&path, "tokio", &root).unwrap());
        assert!(!remove_global(&path, "tokio", &root).unwrap());
        assert!(load_global(&path).unwrap().libs.is_empty());
    }

    /// `patch_entry` drops a modeled key the registration no longer sets, so
    /// a field added to `LibEntry` without a matching key here would be
    /// written and then deleted by the next upsert.
    #[test]
    fn entry_keys_covers_every_field_lib_entry_serializes() {
        let full = LibEntry {
            name: "n".into(),
            ecosystem: Some(Ecosystem::Rust),
            package: Some("p".into()),
            repo: Some("r".into()),
            r#ref: Some("v".into()),
            src_dir: Some("s".into()),
            docs_dir: Some("d".into()),
            notes: Some("note".into()),
            origin_file: Some("/ignored".into()),
        };
        let serialized = toml_edit::ser::to_document(&full).unwrap();
        let keys: Vec<&str> = serialized.as_table().iter().map(|(key, _)| key).collect();
        assert_eq!(keys, ENTRY_KEYS);
    }

    #[test]
    fn upsert_project_preserves_comments_and_replaces_by_name() {
        let root = unique_tmp("project");
        let path = root.join("devkit.toml");
        std::fs::write(&path, "# keep me\n[defaults]\napps_dir = 'apps' # inline\n").unwrap();
        let e = LibEntry {
            name: "react".into(),
            ecosystem: Some(Ecosystem::Js),
            repo: Some("r1".into()),
            ..Default::default()
        };
        upsert_project(&path, &e, &root).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("# keep me") && s.contains("# inline"));
        assert!(s.contains("[[docs.libs]]"));

        let e2 = LibEntry {
            repo: Some("r2".into()),
            ..e
        };
        upsert_project(&path, &e2, &root).unwrap();
        let d = discover(path.parent().unwrap(), Some(&root.join("nope.toml"))).unwrap();
        assert_eq!(d.manifest.libs.len(), 1);
        assert_eq!(d.manifest.libs[0].repo.as_deref(), Some("r2"));

        assert!(remove_project(&path, "react", &root).unwrap());
        assert!(!remove_project(&path, "react", &root).unwrap());
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("# keep me")); // untouched content survives removal too
    }
}
