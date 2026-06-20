use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// On-disk schema version. Bump when the layout changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// Project-root-relative, normalized ('/'-separated; root itself is ".").
    pub path: String,
    /// Absolute project root the path is relative to.
    pub root: String,
    /// Resolved session identity holding the lock.
    pub holder: String,
    /// Durable anchor pid, when one could be trusted (None for agent sessions).
    #[serde(default)]
    pub pid: Option<u32>,
    /// Optional human-readable intent.
    #[serde(default)]
    pub note: Option<String>,
    /// Acquired/renewed at, unix seconds.
    pub ts: u64,
    /// Time-to-live in seconds; 0 means no expiry.
    pub ttl: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub locks: BTreeMap<String, LockEntry>,
}

/// Registry key for a claim: root and path joined by a NUL (never valid in a path).
pub fn key_for(root: &str, path: &str) -> String {
    format!("{root}\u{0}{path}")
}

/// True if two root-relative normalized paths overlap: equal, or one is a
/// path-component ancestor of the other. "." (the project root) overlaps everything.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b || a == "." || b == "." {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.strip_prefix(short).is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_joins_root_and_path() {
        assert_eq!(key_for("/repo", "scenes/x"), "/repo\u{0}scenes/x");
    }
    #[test]
    fn overlap_equal_paths() {
        assert!(paths_overlap("scenes/x", "scenes/x"));
    }
    #[test]
    fn overlap_ancestor_either_direction() {
        assert!(paths_overlap("scenes", "scenes/player.tscn"));
        assert!(paths_overlap("scenes/player.tscn", "scenes"));
    }
    #[test]
    fn no_overlap_sibling_prefix() {
        assert!(!paths_overlap("scenes", "scenes-old"));
        assert!(!paths_overlap("scenes/a", "scenes/b"));
    }
    #[test]
    fn root_dot_overlaps_everything() {
        assert!(paths_overlap(".", "scenes/x"));
        assert!(paths_overlap("scenes/x", "."));
        assert!(paths_overlap(".", "."));
    }
    #[test]
    fn roundtrip_serde() {
        let mut d = Data::default();
        d.locks.insert(
            key_for("/repo", "scenes"),
            LockEntry { path: "scenes".into(), root: "/repo".into(), holder: "alice".into(),
                        pid: None, note: Some("refactor".into()), ts: 5, ttl: 1800 },
        );
        let s = serde_json::to_string(&d).unwrap();
        let back: Data = serde_json::from_str(&s).unwrap();
        assert_eq!(back.locks[&key_for("/repo", "scenes")].holder, "alice");
    }
}
