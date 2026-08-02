//! Cheap pin readout: which version of each registered library does this
//! project resolve to? Filesystem only — no clone, no fetch, no worktree —
//! so a session hook can call it on every start.

use crate::cache::LibCache;
use crate::lockfiles;
use crate::manifest::{self, Ecosystem};
use std::collections::BTreeSet;
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
    /// A worktree for this exact version is already on disk, so `docm info`
    /// will resolve to it. When false the version is only what the lockfile
    /// *asks* for: resolution still has to find a matching git tag, and falls
    /// back to the default branch when there is none.
    pub materialized: bool,
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

/// Every registered library with the version this project would resolve.
///
/// Precedence mirrors `resolve::resolve` — manual `ref`, then lockfile, then
/// the default branch. Where this cannot match `resolve` without a network
/// call it reports less rather than guessing: `materialized` is the honest
/// signal for "`docm info` will agree with this line".
pub fn pins(start: &Path, global: Option<&Path>, cache_root: &Path) -> Vec<Pin> {
    let Ok(d) = manifest::discover(start, global) else {
        return Vec::new();
    };
    let scoped = project_scoped_names(&d);
    let mut out: Vec<Pin> = d
        .manifest
        .libs
        .iter()
        .map(|entry| {
            let project_scoped = scoped.contains(&entry.name);
            let lib = LibCache::new(cache_root, &entry.name);
            if let Some(r) = entry.r#ref.as_deref() {
                return Pin {
                    name: entry.name.clone(),
                    version: Some(r.to_string()),
                    origin: Origin::Ref,
                    // A changed pin is re-pointed by `docm sync`, never by a
                    // lookup, so the on-disk default worktree may still be at
                    // the previous commit. Never claim otherwise.
                    materialized: false,
                    other_versions: 0,
                    project_scoped,
                };
            }
            let found = match entry.ecosystem {
                Some(eco) if eco != Ecosystem::Git => {
                    lockfiles::find_version(start, eco, &entry.package_name())
                }
                _ => None,
            };
            match found {
                Some((_, versions)) => {
                    let extra = versions.len().saturating_sub(1);
                    let v = lockfiles::highest(versions).expect("non-empty versions");
                    let materialized = lib.worktree_path(&v).is_dir();
                    Pin {
                        name: entry.name.clone(),
                        version: Some(v),
                        origin: Origin::Lockfile,
                        materialized,
                        other_versions: extra,
                        project_scoped,
                    }
                }
                None => Pin {
                    name: entry.name.clone(),
                    version: None,
                    origin: Origin::Unpinned,
                    materialized: false,
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
    fn a_lockfile_version_is_not_claimed_resolved_until_its_worktree_exists() {
        let root = unique_tmp("mat");
        let cache = root.join("cache");
        let global = root.join("docs.toml");
        write(&global, GLOBAL);
        write(
            &root.join("Cargo.lock"),
            "[[package]]\nname = \"clap\"\nversion = \"3.2.25\"\n",
        );

        // No worktree on disk: resolution still has to find a tag and may fall
        // back to the default branch, so the version is a request, not a fact.
        let got = pins(&root, Some(&global), &cache);
        let clap = got.iter().find(|p| p.name == "clap").unwrap();
        assert_eq!(clap.version.as_deref(), Some("3.2.25"));
        assert!(!clap.materialized, "{clap:?}");

        std::fs::create_dir_all(cache.join("clap/3.2.25")).unwrap();
        let got = pins(&root, Some(&global), &cache);
        let clap = got.iter().find(|p| p.name == "clap").unwrap();
        assert!(clap.materialized, "{clap:?}");
    }

    #[test]
    fn a_ref_pin_is_never_reported_as_materialized() {
        let root = unique_tmp("ref");
        let cache = root.join("cache");
        let global = root.join("docs.toml");
        write(&global, GLOBAL);
        // Even with a default worktree present, `docm sync` — not a lookup —
        // is what re-points it at a changed pin.
        std::fs::create_dir_all(cache.join("godot/default")).unwrap();

        let got = pins(&root, Some(&global), &cache);
        let godot = got.iter().find(|p| p.name == "godot").unwrap();
        assert_eq!(godot.origin, Origin::Ref);
        assert!(!godot.materialized, "{godot:?}");
    }

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

        let got = pins(&root, Some(&global), &root.join("cache"));
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

        let got = pins(&root, Some(&global), &root.join("cache"));
        let by = |n: &str| got.iter().find(|p| p.name == n).unwrap().relevant();
        assert!(by("clap"), "lockfile hit is evidence");
        assert!(by("godot"), "project registration is evidence");
        assert!(!by("fish-shell"), "machine-wide registration is not");
    }
}
