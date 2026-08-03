//! Manifest model: which libraries docm manages and where their repos live.
//!
//! Global entries live in `~/.config/devkit/docs.toml` (top-level `[[libs]]`);
//! per-project overrides live in a `[docs]` section (`[[docs.libs]]`) of the
//! project's `devkit.toml`. Layers merge field-by-field per lib name, the
//! deeper (more project-specific) layer winning.

use anyhow::{Context, Result, bail};
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
        .or_insert(toml_edit::Item::ArrayOfTables(
            toml_edit::ArrayOfTables::new(),
        ));
    let arr = libs
        .as_array_of_tables_mut()
        .context("docs.libs in devkit.toml is not an array of tables")?;
    // Collect tables, update or append, then rebuild array
    let mut tables: Vec<toml_edit::Table> = arr.iter().cloned().collect();
    let mut found = false;
    for t in &mut tables {
        if t.get("name").and_then(|v| v.as_str()) == Some(entry.name.as_str()) {
            *t = tbl.clone();
            found = true;
            break;
        }
    }
    if !found {
        tables.push(tbl.clone());
    }
    // Clear and rebuild
    arr.clear();
    for t in tables {
        arr.push(t);
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
        upsert_global(&path, &e).unwrap();
        let e2 = LibEntry {
            repo: Some("u2".into()),
            ..e.clone()
        };
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
        let e = LibEntry {
            name: "react".into(),
            ecosystem: Some(Ecosystem::Js),
            repo: Some("r1".into()),
            ..Default::default()
        };
        upsert_project(&path, &e).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("# keep me") && s.contains("# inline"));
        assert!(s.contains("[[docs.libs]]"));

        let e2 = LibEntry {
            repo: Some("r2".into()),
            ..e
        };
        upsert_project(&path, &e2).unwrap();
        let d = discover(path.parent().unwrap(), Some(&root.join("nope.toml"))).unwrap();
        assert_eq!(d.manifest.libs.len(), 1);
        assert_eq!(d.manifest.libs[0].repo.as_deref(), Some("r2"));

        assert!(remove_project(&path, "react").unwrap());
        assert!(!remove_project(&path, "react").unwrap());
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("# keep me")); // untouched content survives removal too
    }
}
