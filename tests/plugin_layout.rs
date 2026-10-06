//! Each harness installs the plugin by copying the directory its marketplace
//! names, so that directory must hold everything the plugin reads and nothing
//! of the workspace around it.

use std::path::{Component, Path, PathBuf};

/// A harness's marketplace file and its plugin manifest, relative to the
/// plugin directory the marketplace names.
const HARNESSES: [(&str, &str); 3] = [
    (
        ".claude-plugin/marketplace.json",
        ".claude-plugin/plugin.json",
    ),
    (
        ".agents/plugins/marketplace.json",
        ".codex-plugin/plugin.json",
    ),
    (
        ".cursor-plugin/marketplace.json",
        ".cursor-plugin/plugin.json",
    ),
];

fn read_json(path: &Path) -> serde_json::Value {
    let body = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Claude Code and Cursor spell the source as a string, Codex as
/// `{ "source": "local", "path": ... }`.
fn plugin_root(marketplace: &str) -> PathBuf {
    let v = read_json(Path::new(marketplace));
    let source = &v["plugins"][0]["source"];
    let path = source
        .as_str()
        .or_else(|| source["path"].as_str())
        .unwrap_or_else(|| panic!("{marketplace}: no plugin source"));
    PathBuf::from(path)
}

#[test]
fn each_marketplace_installs_the_plugin_directory_alone() {
    for (marketplace, manifest) in HARNESSES {
        let root = plugin_root(marketplace);
        assert!(
            root.join(manifest).is_file(),
            "{marketplace}: no {manifest} under {}",
            root.display()
        );
        assert!(
            root.join("skills").is_dir(),
            "{marketplace}: no skills/ under {}",
            root.display()
        );
        assert!(
            !root.join("Cargo.toml").exists(),
            "{marketplace}: installing {} copies the workspace source",
            root.display()
        );
    }
}

/// Harnesses reject a manifest path that leaves the plugin directory, and one
/// that is missing breaks the install silently.
#[test]
fn every_path_a_manifest_names_stays_inside_the_plugin() {
    for (marketplace, manifest) in HARNESSES {
        let root = plugin_root(marketplace);
        let v = read_json(&root.join(manifest));
        for key in [
            "/skills",
            "/hooks",
            "/mcpServers",
            "/icon",
            "/logo",
            "/interface/logo",
            "/interface/composerIcon",
        ] {
            let Some(rel) = v.pointer(key).and_then(|p| p.as_str()) else {
                continue;
            };
            let rel = Path::new(rel);
            assert!(
                rel.components()
                    .all(|c| matches!(c, Component::CurDir | Component::Normal(_))),
                "{manifest}: {key} leaves the plugin: {}",
                rel.display()
            );
            assert!(
                root.join(rel).exists(),
                "{manifest}: {key} names a missing {}",
                rel.display()
            );
        }
    }
}
