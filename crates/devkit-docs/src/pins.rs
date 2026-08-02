//! Cheap pin readout: which version of each registered library does this
//! project resolve to? Filesystem only — no clone, no fetch, no worktree —
//! so a session hook can call it on every start.

use crate::lockfiles;
use crate::manifest::{self, Ecosystem};
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
}

/// Every registered library with the version this project would resolve.
///
/// Precedence mirrors `resolve::resolve` — manual `ref`, then lockfile, then
/// the default branch. The two must agree: a brief that disagrees with what
/// `docm info` later prints is worse than no brief at all.
pub fn pins(start: &Path, global: Option<&Path>) -> Vec<Pin> {
    let Ok(d) = manifest::discover(start, global) else {
        return Vec::new();
    };
    let mut out: Vec<Pin> = d
        .manifest
        .libs
        .iter()
        .map(|entry| {
            if let Some(r) = entry.r#ref.as_deref() {
                return Pin {
                    name: entry.name.clone(),
                    version: Some(r.to_string()),
                    origin: Origin::Ref,
                };
            }
            let version = match entry.ecosystem {
                Some(eco) if eco != Ecosystem::Git => {
                    lockfiles::find_version(start, eco, &entry.package_name())
                        .and_then(|(_, vs)| lockfiles::highest(vs))
                }
                _ => None,
            };
            match version {
                Some(v) => Pin {
                    name: entry.name.clone(),
                    version: Some(v),
                    origin: Origin::Lockfile,
                },
                None => Pin {
                    name: entry.name.clone(),
                    version: None,
                    origin: Origin::Unpinned,
                },
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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

    #[test]
    fn pins_reports_ref_lockfile_and_unpinned() {
        let root = unique_tmp("all");
        let root = root.as_path();
        let global = root.join("docs.toml");
        write(
            &global,
            r#"
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
name = "serde"
ecosystem = "rust"
repo = "https://github.com/serde-rs/serde"
"#,
        );
        write(
            &root.join("Cargo.lock"),
            "version = 3\n\n[[package]]\nname = \"clap\"\nversion = \"3.2.25\"\n",
        );

        let got = pins(root, Some(&global));
        assert_eq!(
            got,
            vec![
                Pin {
                    name: "clap".into(),
                    version: Some("3.2.25".into()),
                    origin: Origin::Lockfile,
                },
                Pin {
                    name: "godot".into(),
                    version: Some("4.3-stable".into()),
                    origin: Origin::Ref,
                },
                Pin {
                    name: "serde".into(),
                    version: None,
                    origin: Origin::Unpinned,
                },
            ]
        );
    }

    #[test]
    fn pins_prefers_the_deepest_lockfile_walking_up() {
        let root = unique_tmp("deep");
        let root = root.as_path();
        let global = root.join("docs.toml");
        write(
            &global,
            "[[libs]]\nname = \"clap\"\necosystem = \"rust\"\nrepo = \"https://x/y\"\n",
        );
        write(
            &root.join("Cargo.lock"),
            "[[package]]\nname = \"clap\"\nversion = \"3.2.25\"\n",
        );
        let nested = root.join("crates/inner");
        write(
            &nested.join("Cargo.lock"),
            "[[package]]\nname = \"clap\"\nversion = \"4.6.2\"\n",
        );

        let got = pins(&nested, Some(&global));
        assert_eq!(got[0].version.as_deref(), Some("4.6.2"));
    }

    #[test]
    fn no_registered_libs_yields_no_pins() {
        let root = unique_tmp("empty");
        let global = root.join("docs.toml");
        write(&global, "");
        assert!(pins(&root, Some(&global)).is_empty());
    }
}
