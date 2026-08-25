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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::LibEntry;

    #[test]
    fn detects_dirs_and_mdbook() {
        let d_dir = tempfile::tempdir().unwrap();
        let d = d_dir.path();
        for p in ["docs", "src", "examples"] {
            std::fs::create_dir_all(d.join(p)).unwrap();
        }
        std::fs::write(d.join("book.toml"), "[book]").unwrap();
        let l = detect(d);
        assert_eq!(l.docs_dir.as_deref(), Some("docs"));
        assert_eq!(l.src_dir.as_deref(), Some("src"));
        assert_eq!(l.examples_dir.as_deref(), Some("examples"));
        assert_eq!(l.kind.as_deref(), Some("mdbook"));
    }

    #[test]
    fn detects_sphinx_under_doc_and_empty_repo_is_all_none() {
        let d_dir = tempfile::tempdir().unwrap();
        let d = d_dir.path();
        std::fs::create_dir_all(d.join("doc")).unwrap();
        std::fs::write(d.join("doc/conf.py"), "").unwrap();
        let l = detect(d);
        assert_eq!(l.docs_dir.as_deref(), Some("doc"));
        assert_eq!(l.kind.as_deref(), Some("sphinx"));
        let empty_dir = tempfile::tempdir().unwrap();
        let empty = empty_dir.path();
        assert_eq!(detect(empty), Layout::default());
    }

    #[test]
    fn manifest_overrides_beat_detection() {
        let l = Layout {
            docs_dir: Some("docs".into()),
            ..Default::default()
        };
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
