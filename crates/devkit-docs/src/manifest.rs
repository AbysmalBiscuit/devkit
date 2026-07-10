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
}
