//! Lockfile parsers: which version of a package does a project pin?
//!
//! All parsers are tolerant — an unreadable or unparsable lockfile yields no
//! versions rather than an error, so resolution can fall through to the
//! default branch.
//!
//! Every parser answers for a *set* of packages from a single parse. A brief
//! resolves every registered library at once, and parsing a large lockfile
//! per library dominated that cost.

use crate::manifest::Ecosystem;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Versions pinned in `dir` for each requested package that appears there.
pub fn versions_in_dir_many(
    dir: &Path,
    eco: Ecosystem,
    want: &BTreeSet<&str>,
) -> BTreeMap<String, Vec<String>> {
    match eco {
        Ecosystem::Rust => toml_packages(&dir.join("Cargo.lock"), want),
        Ecosystem::Python => toml_packages(&dir.join("uv.lock"), want),
        Ecosystem::Js => {
            // Each JS lockfile contributes independently: a malformed one must
            // not suppress the versions another would have supplied.
            let mut out = npm_versions(&dir.join("package-lock.json"), want);
            for (k, v) in pnpm_versions(&dir.join("pnpm-lock.yaml"), want) {
                out.entry(k).or_default().extend(v);
            }
            for (k, v) in bun_versions(&dir.join("bun.lock"), want) {
                out.entry(k).or_default().extend(v);
            }
            out
        }
        Ecosystem::Git => BTreeMap::new(),
    }
}

pub fn versions_in_dir(dir: &Path, eco: Ecosystem, package: &str) -> Vec<String> {
    let want = BTreeSet::from([package]);
    versions_in_dir_many(dir, eco, &want)
        .remove(package)
        .unwrap_or_default()
}

/// Resolve many packages in one walk. Each package independently takes the
/// nearest ancestor whose lockfile mentions it, so a monorepo where packages
/// resolve at different levels keeps that behaviour; the walk simply stops
/// early once nothing is left to find.
pub fn find_versions(
    start: &Path,
    eco: Ecosystem,
    packages: &[String],
) -> BTreeMap<String, (PathBuf, Vec<String>)> {
    let mut pending: BTreeSet<&str> = packages.iter().map(String::as_str).collect();
    let mut out = BTreeMap::new();
    let mut dir = Some(start);
    while let Some(d) = dir {
        if pending.is_empty() {
            break;
        }
        for (name, versions) in versions_in_dir_many(d, eco, &pending) {
            if versions.is_empty() {
                continue;
            }
            pending.remove(name.as_str());
            out.insert(name, (d.to_path_buf(), versions));
        }
        dir = d.parent();
    }
    out
}

/// Walk up from `start`; the first directory whose lockfile mentions
/// `package` wins. Returns that directory (the project root for registry
/// purposes) and every version it pins.
pub fn find_version(start: &Path, eco: Ecosystem, package: &str) -> Option<(PathBuf, Vec<String>)> {
    find_versions(start, eco, &[package.to_string()]).remove(package)
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

fn insert(out: &mut BTreeMap<String, Vec<String>>, want: &BTreeSet<&str>, name: &str, ver: &str) {
    if let Some(k) = want.get(name) {
        out.entry((*k).to_string())
            .or_default()
            .push(ver.to_string());
    }
}

/// `Cargo.lock` and `uv.lock` share the `[[package]] name/version` shape.
fn toml_packages(path: &Path, want: &BTreeSet<&str>) -> BTreeMap<String, Vec<String>> {
    #[derive(Deserialize)]
    struct Lock {
        #[serde(default)]
        package: Vec<Pkg>,
    }
    #[derive(Deserialize)]
    struct Pkg {
        name: String,
        version: Option<String>,
    }
    let mut out = BTreeMap::new();
    let Ok(s) = std::fs::read_to_string(path) else {
        return out;
    };
    let Ok(lock) = toml::from_str::<Lock>(&s) else {
        return out;
    };
    for p in lock.package {
        if let Some(v) = p.version {
            insert(&mut out, want, &p.name, &v);
        }
    }
    out
}

/// package-lock.json v2/v3 `packages` map, falling back to the ancient v1
/// top-level `dependencies` map.
fn npm_versions(path: &Path, want: &BTreeSet<&str>) -> BTreeMap<String, Vec<String>> {
    #[derive(Deserialize)]
    struct Lock {
        #[serde(default)]
        packages: BTreeMap<String, Entry>,
        #[serde(default)]
        dependencies: BTreeMap<String, Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        version: Option<String>,
    }
    let mut out = BTreeMap::new();
    let Ok(s) = std::fs::read_to_string(path) else {
        return out;
    };
    let Ok(lock) = serde_json::from_str::<Lock>(&s) else {
        return out;
    };
    for (key, e) in &lock.packages {
        // The name is the final `node_modules/` segment, which keeps a scoped
        // package whole and a nested copy attributed to the package itself.
        let Some((_, name)) = key.rsplit_once("node_modules/") else {
            continue;
        };
        if let Some(v) = &e.version {
            insert(&mut out, want, name, v);
        }
    }
    // The v1 fallback is per package, not per file: a v2 lockfile can carry a
    // `dependencies` map that names something `packages` never resolved.
    for (name, e) in &lock.dependencies {
        if out.contains_key(name.as_str()) {
            continue;
        }
        if let Some(v) = &e.version {
            insert(&mut out, want, name, v);
        }
    }
    out
}

/// pnpm-lock.yaml `packages` keys: v9 `name@1.2.3` / `@scope/name@1.2.3`,
/// v6 `/name@1.2.3(peer@x)`, v5 `/name/1.2.3`.
fn pnpm_versions(path: &Path, want: &BTreeSet<&str>) -> BTreeMap<String, Vec<String>> {
    #[derive(Deserialize)]
    struct Lock {
        // Only the keys carry name and version; discarding each value avoids
        // building a tree for the whole file.
        #[serde(default)]
        packages: BTreeMap<String, serde::de::IgnoredAny>,
    }
    let mut out = BTreeMap::new();
    let Ok(s) = std::fs::read_to_string(path) else {
        return out;
    };
    let Ok(lock) = serde_yaml_ng::from_str::<Lock>(&s) else {
        return out;
    };
    for key in lock.packages.keys() {
        let k = key.trim_start_matches('/');
        let k = k.split('(').next().unwrap_or(k);
        let parsed = k
            .rsplit_once('@')
            .filter(|(n, _)| !n.is_empty())
            .or_else(|| k.rsplit_once('/'));
        if let Some((name, ver)) = parsed {
            insert(&mut out, want, name, ver);
        }
    }
    out
}

/// bun.lock `packages` map: each value is an array whose first element is the
/// resolved `name@version` spec. The spec — not the map key — identifies the
/// package: a key like `parent/kysely` is a nested copy of `kysely`, while
/// `@scope/kysely` is a different package, and only the spec tells them apart.
fn bun_versions(path: &Path, want: &BTreeSet<&str>) -> BTreeMap<String, Vec<String>> {
    #[derive(Deserialize)]
    struct Lock {
        #[serde(default)]
        packages: BTreeMap<String, Vec<serde_json::Value>>,
    }
    let mut out = BTreeMap::new();
    let Ok(s) = std::fs::read_to_string(path) else {
        return out;
    };
    let Ok(lock) = serde_json::from_str::<Lock>(&strip_trailing_commas(&s)) else {
        return out;
    };
    for entry in lock.packages.values() {
        let Some(spec) = entry.first().and_then(|x| x.as_str()) else {
            continue;
        };
        if let Some((name, ver)) = spec.rsplit_once('@').filter(|(n, _)| !n.is_empty()) {
            insert(&mut out, want, name, ver);
        }
    }
    out
}

/// bun writes its lockfile as JSONC, but the only JSONC feature its writer
/// emits is trailing commas — drop them (outside strings) so serde_json can
/// parse the rest.
fn strip_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut in_str, mut escaped) = (false, false);
    for c in s.chars() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '}' | ']' => {
                let trimmed = out.trim_end().len();
                if out[..trimmed].ends_with(',') {
                    out.truncate(trimmed - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

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
    const BUN_LOCK: &str = r#"{
  "lockfileVersion": 1,
  "configVersion": 1,
  "workspaces": {
    "": {
      "name": "app",
      "dependencies": {
        "kysely": "^0.28.11",
      },
    },
  },
  "packages": {
    "kysely": ["kysely@0.28.17", "", {}, "sha512-x"],
    "@types/node": ["@types/node@20.12.0", "", {}, "sha512-y"],
    "@app/portal/kysely": ["kysely@0.28.14", "", {}, "sha512-z"],
    "@scope/kysely": ["@scope/kysely@9.9.9", "", {}, "sha512-w"],
  },
}"#;

    #[test]
    fn cargo_lock_versions() {
        let d = unique_tmp("cargo");
        std::fs::write(d.join("Cargo.lock"), CARGO_LOCK).unwrap();
        assert_eq!(
            versions_in_dir(&d, Ecosystem::Rust, "tokio"),
            vec!["1.38.0"]
        );
        assert!(versions_in_dir(&d, Ecosystem::Rust, "absent").is_empty());
    }

    #[test]
    fn uv_lock_versions() {
        let d = unique_tmp("uv");
        std::fs::write(d.join("uv.lock"), UV_LOCK).unwrap();
        assert_eq!(
            versions_in_dir(&d, Ecosystem::Python, "requests"),
            vec!["2.32.3"]
        );
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
        assert_eq!(
            versions_in_dir(&d, Ecosystem::Js, "@types/node"),
            vec!["20.12.0"]
        );
        let d6 = unique_tmp("pnpm6");
        std::fs::write(d6.join("pnpm-lock.yaml"), PNPM_V6).unwrap();
        assert_eq!(versions_in_dir(&d6, Ecosystem::Js, "react"), vec!["18.2.0"]);
    }

    #[test]
    fn bun_lock_collects_all_copies_despite_trailing_commas() {
        let d = unique_tmp("bun");
        std::fs::write(d.join("bun.lock"), BUN_LOCK).unwrap();
        let mut v = versions_in_dir(&d, Ecosystem::Js, "kysely");
        v.sort();
        assert_eq!(v, vec!["0.28.14", "0.28.17"]);
        assert_eq!(
            versions_in_dir(&d, Ecosystem::Js, "@types/node"),
            vec!["20.12.0"]
        );
        assert_eq!(
            versions_in_dir(&d, Ecosystem::Js, "@scope/kysely"),
            vec!["9.9.9"]
        );
        assert!(versions_in_dir(&d, Ecosystem::Js, "absent").is_empty());
    }

    #[test]
    fn batching_keeps_each_package_on_its_own_nearest_ancestor() {
        let root = unique_tmp("batch");
        let nested = root.join("apps/web");
        std::fs::create_dir_all(&nested).unwrap();
        // `serde` is pinned only at the root; `tokio` is pinned at both levels
        // and must take the nearer one.
        std::fs::write(
            root.join("Cargo.lock"),
            "[[package]]\nname = \"serde\"\nversion = \"1.0.203\"\n\n\
             [[package]]\nname = \"tokio\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            nested.join("Cargo.lock"),
            "[[package]]\nname = \"tokio\"\nversion = \"1.38.0\"\n",
        )
        .unwrap();

        let got = find_versions(
            &nested,
            Ecosystem::Rust,
            &["serde".to_string(), "tokio".to_string()],
        );
        assert_eq!(got["tokio"], (nested.clone(), vec!["1.38.0".to_string()]));
        assert_eq!(got["serde"], (root.clone(), vec!["1.0.203".to_string()]));
        // And it agrees with resolving them one at a time.
        for name in ["serde", "tokio"] {
            assert_eq!(
                find_version(&nested, Ecosystem::Rust, name).unwrap(),
                got[name],
                "{name}"
            );
        }
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
