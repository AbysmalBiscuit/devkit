//! Cheap pin readout: which version of each registered library does this
//! project resolve to? Filesystem only — no clone, no fetch, no worktree —
//! so a session hook can call it on every start.

use crate::lockfiles;
use crate::manifest::{self, Ecosystem};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum Origin {
    /// Manual `ref` pin in the manifest.
    Ref,
    /// Version read from the project's lockfile.
    Lockfile,
    /// Nothing pins it; resolution falls back to the default branch.
    Unpinned,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Pin {
    pub name: String,
    pub version: Option<String>,
    pub origin: Origin,
    /// Further versions the lockfile pins beyond the one shown.
    pub other_versions: usize,
    /// Declared by a project's own `devkit.toml`, not the machine-wide
    /// catalog — evidence this library belongs to the checkout in hand.
    pub project_scoped: bool,
}

impl Pin {
    /// Whether this library is evidence-backed context for the checkout, as
    /// opposed to a machine-wide registration that resolves the same in every
    /// unrelated repository.
    pub fn relevant(&self) -> bool {
        self.project_scoped || self.origin == Origin::Lockfile
    }
}

/// Every registered library with the version this project's own files name.
///
/// Precedence mirrors `resolve::resolve` — manual `ref`, then lockfile, then
/// the default branch. This is the version *requested*, which is all the
/// filesystem can answer; whether a matching tag exists, and what gets served
/// if none does, is `resolve`'s to report after it has fetched.
pub fn pins(start: &Path, global: Option<&Path>) -> Vec<Pin> {
    let Ok(d) = manifest::discover(start, global) else {
        return Vec::new();
    };
    let scoped = project_scoped_names(&d);

    // Resolve every library of an ecosystem in one walk: parsing a large
    // lockfile once per registered library dominated this call.
    let mut by_eco: BTreeMap<Ecosystem, Vec<String>> = BTreeMap::new();
    for e in &d.manifest.libs {
        if e.r#ref.is_none()
            && let Some(eco) = e.ecosystem
            && eco != Ecosystem::Git
        {
            by_eco.entry(eco).or_default().push(e.package_name());
        }
    }
    let resolved: BTreeMap<Ecosystem, BTreeMap<String, (std::path::PathBuf, Vec<String>)>> = by_eco
        .into_iter()
        .map(|(eco, pkgs)| (eco, lockfiles::find_versions(start, eco, &pkgs)))
        .collect();

    let mut out: Vec<Pin> = d
        .manifest
        .libs
        .iter()
        .map(|entry| {
            let project_scoped = scoped.contains(&entry.name);
            if let Some(r) = entry.r#ref.as_deref() {
                return Pin {
                    name: entry.name.clone(),
                    version: Some(r.to_string()),
                    origin: Origin::Ref,
                    other_versions: 0,
                    project_scoped,
                };
            }
            let found = entry
                .ecosystem
                .and_then(|eco| resolved.get(&eco))
                .and_then(|m| m.get(&entry.package_name()))
                .cloned();
            match found {
                Some((_, versions)) => {
                    let extra = versions.len().saturating_sub(1);
                    let v = lockfiles::highest(versions).expect("non-empty versions");
                    Pin {
                        name: entry.name.clone(),
                        version: Some(v),
                        origin: Origin::Lockfile,
                        other_versions: extra,
                        project_scoped,
                    }
                }
                None => Pin {
                    name: entry.name.clone(),
                    version: None,
                    origin: Origin::Unpinned,
                    other_versions: 0,
                    project_scoped,
                },
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Names declared by the nearest `devkit.toml`'s `[docs]` section. Registering
/// a library there is the project saying "this one is mine", which no global
/// entry can express.
fn project_scoped_names(d: &manifest::Discovered) -> BTreeSet<String> {
    d.project_devkit_toml
        .as_deref()
        .and_then(|p| manifest::docs_layer(p).ok().flatten())
        .map(|m| m.libs.into_iter().map(|l| l.name).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-docs-pn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    const GLOBAL: &str = r#"
[[libs]]
name = "clap"
ecosystem = "rust"
repo = "https://github.com/clap-rs/clap"

[[libs]]
name = "godot"
ecosystem = "git"
repo = "https://github.com/godotengine/godot"
ref = "4.3-stable"

[[libs]]
name = "fish-shell"
ecosystem = "git"
repo = "https://github.com/fish-shell/fish-shell"
"#;

    #[test]
    fn extra_lockfile_versions_are_counted_not_silently_dropped() {
        let root = unique_tmp("multi");
        let global = root.join("docs.toml");
        write(&global, GLOBAL);
        write(
            &root.join("Cargo.lock"),
            "[[package]]\nname = \"clap\"\nversion = \"3.2.25\"\n\n\
             [[package]]\nname = \"clap\"\nversion = \"4.6.2\"\n",
        );

        let got = pins(&root, Some(&global));
        let clap = got.iter().find(|p| p.name == "clap").unwrap();
        assert_eq!(clap.version.as_deref(), Some("4.6.2"));
        assert_eq!(clap.other_versions, 1, "{clap:?}");
    }

    #[test]
    fn relevance_needs_lockfile_evidence_or_a_project_registration() {
        let root = unique_tmp("rel");
        let global = root.join("docs.toml");
        write(&global, GLOBAL);
        write(
            &root.join("Cargo.lock"),
            "[[package]]\nname = \"clap\"\nversion = \"3.2.25\"\n",
        );
        // The project claims godot as its own; fish-shell stays machine-wide.
        write(
            &root.join("devkit.toml"),
            "[[docs.libs]]\nname = \"godot\"\n",
        );

        let got = pins(&root, Some(&global));
        let by = |n: &str| got.iter().find(|p| p.name == n).unwrap().relevant();
        assert!(by("clap"), "lockfile hit is evidence");
        assert!(by("godot"), "project registration is evidence");
        assert!(!by("fish-shell"), "machine-wide registration is not");
    }
}
