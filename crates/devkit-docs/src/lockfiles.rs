//! Tolerant lockfile version discovery used by conservative prune liveness checks.
//!
//! Importer-aware checkout resolution lives in `crate::importers`. These parsers
//! only answer whether a live project still has a lockfile reference, so an
//! unreadable or unparsable lockfile yields no versions.

use crate::manifest::Ecosystem;
use std::path::{Path, PathBuf};

pub fn versions_in_dir(dir: &Path, eco: Ecosystem, package: &str) -> Vec<String> {
    match eco {
        Ecosystem::Rust => toml_packages(&dir.join("Cargo.lock"), package),
        Ecosystem::Python => toml_packages(&dir.join("uv.lock"), package),
        Ecosystem::Js => {
            let mut v = npm_versions(&dir.join("package-lock.json"), package);
            v.extend(pnpm_versions(&dir.join("pnpm-lock.yaml"), package));
            v.extend(bun_versions(&dir.join("bun.lock"), package));
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
    let Ok(s) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(t) = s.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(pkgs) = t.get("package").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    pkgs.iter()
        .filter(|p| p.get("name").and_then(|n| n.as_str()) == Some(package))
        .filter_map(|p| p.get("version").and_then(|v| v.as_str()).map(String::from))
        .collect()
}

/// package-lock.json v2/v3 `packages` map, falling back to the ancient v1
/// top-level `dependencies` map.
fn npm_versions(path: &Path, package: &str) -> Vec<String> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return Vec::new();
    };
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
    let Ok(s) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(y) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&s) else {
        return Vec::new();
    };
    let Some(pkgs) = y.get("packages").and_then(|p| p.as_mapping()) else {
        return Vec::new();
    };
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

/// bun.lock `packages` map: each value is an array whose first element is the
/// resolved `name@version` spec. The spec — not the map key — identifies the
/// package: a key like `parent/kysely` is a nested copy of `kysely`, while
/// `@scope/kysely` is a different package, and only the spec tells them apart.
fn bun_versions(path: &Path, package: &str) -> Vec<String> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(Some(v)) = jsonc_parser::parse_to_serde_value(&s, &Default::default()) else {
        return Vec::new();
    };
    let Some(pkgs) = v.get("packages").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    pkgs.values()
        .filter_map(|e| e.get(0).and_then(|x| x.as_str()))
        .filter_map(|spec| spec.rsplit_once('@').filter(|(n, _)| !n.is_empty()))
        .filter(|(name, _)| *name == package)
        .map(|(_, ver)| ver.to_string())
        .collect()
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
  // Prune must recognize the same JSONC that importer resolution accepts.
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
    "@scope/kysely": ["@scope/kysely@9.9.9", "", {}, "sha512-w"], /* note */
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
