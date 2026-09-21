//! Whether a configured file injects for this tool call, and reading it.

use std::path::{Path, PathBuf};

use devkit_config::ContextFile;

use crate::query::{governs, normalize, relativize};

/// What a tool call is about to do, as the matcher sees it.
#[derive(Debug, Clone)]
pub struct Subject {
    /// The checkout root every target is relative to.
    pub root: PathBuf,
    /// Repo-relative, '/'-separated targets of this call.
    pub targets: Vec<String>,
    /// The calling harness, as its manifest declares it.
    pub harness: Option<String>,
}

/// A target relative to the checkout, including symlink aliases and files
/// that do not exist yet. The raw comparison avoids Windows verbatim-prefix
/// differences; the fallback resolves both paths through existing ancestors.
pub fn relativize_target(root: &Path, target: &Path) -> Option<String> {
    relativize(root, target).or_else(|| {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        relativize(&root, &canonicalize_prefix(&normalize(target)))
    })
}

fn canonicalize_prefix(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return path.to_path_buf();
        };
        tail.push(name);
        existing = parent;
    }
    let mut resolved = std::fs::canonicalize(existing).unwrap_or_else(|_| existing.to_path_buf());
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    resolved
}

/// Whether `entry`, declared by a `devkit.toml` in `layer_dir`, injects for
/// `subject`. Every condition present must hold.
pub fn fires(entry: &ContextFile, layer_dir: &Path, subject: &Subject) -> bool {
    let Some(rel) = relativize(&subject.root, &layer_dir.join(&entry.path)) else {
        return false;
    };
    let Some(when) = &entry.when else {
        let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
        return subject.targets.iter().any(|t| governs(dir, t));
    };
    if !when.harness.is_empty()
        && !subject
            .harness
            .as_ref()
            .is_some_and(|h| when.harness.contains(h))
    {
        return false;
    }
    for (key, want) in &when.env {
        match std::env::var(key) {
            Ok(got) if want.is_empty() || &got == want => {}
            _ => return false,
        }
    }
    if let Some(pattern) = &when.path {
        let Ok(glob) = glob::Pattern::new(pattern) else {
            return false;
        };
        if !subject.targets.iter().any(|t| glob.matches(t)) {
            return false;
        }
    }
    true
}

/// The file's text, or `None` when it is missing, unreadable, not UTF-8, or
/// over `cap` bytes. Every one of those is an absence rather than an error: an
/// injected file is an improvement to a tool call, never a precondition of it.
pub fn read_capped(path: &Path, cap: usize) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() as usize > cap {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use devkit_config::{ContextFile, FileCondition};

    use super::*;

    #[test]
    fn relativize_target_accepts_a_raw_path_without_existing_ancestors() {
        assert_eq!(
            relativize_target(Path::new("/repo"), Path::new("/repo/src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
    }

    #[test]
    fn relativize_target_resolves_parent_components_under_a_nonexistent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let target = dir.path().join("crates/bar/../foo/a.rs");
        assert_eq!(
            relativize_target(&root, &target).as_deref(),
            Some("crates/foo/a.rs")
        );
    }

    #[cfg(unix)]
    #[test]
    fn relativize_target_resolves_a_nonexistent_target_reached_through_a_symlink() {
        let real = tempfile::tempdir().unwrap();
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("via-symlink");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let root = std::fs::canonicalize(real.path()).unwrap();
        let abs = link.join("src/a.rs");
        assert_eq!(relativize_target(&root, &abs).as_deref(), Some("src/a.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn relativize_target_drops_a_path_outside_the_repository() {
        let real = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("via-symlink");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let outside = other.path().join("elsewhere/b.rs");
        assert_eq!(relativize_target(&link, &outside), None);
    }

    fn subject(targets: &[&str]) -> Subject {
        Subject {
            root: PathBuf::from("/repo"),
            targets: targets.iter().map(|t| (*t).to_string()).collect(),
            harness: Some("codex".to_string()),
        }
    }

    #[test]
    fn no_condition_fires_for_a_target_under_the_files_own_directory() {
        let entry = ContextFile {
            path: "crates/foo/AGENTS.md".to_string(),
            when: None,
        };
        let layer = Path::new("/repo");
        assert!(fires(&entry, layer, &subject(&["crates/foo/src/a.rs"])));
        assert!(fires(&entry, layer, &subject(&["crates/foo/AGENTS.md"])));
        assert!(!fires(&entry, layer, &subject(&["crates/bar/src/a.rs"])));
    }

    #[test]
    fn a_path_at_the_repo_root_fires_for_every_target() {
        let entry = ContextFile {
            path: "AGENTS.md".to_string(),
            when: None,
        };
        assert!(fires(
            &entry,
            Path::new("/repo"),
            &subject(&["anywhere/x.rs"])
        ));
    }

    #[test]
    fn the_layers_directory_anchors_a_relative_path() {
        let entry = ContextFile {
            path: "AGENTS.md".to_string(),
            when: None,
        };
        let layer = Path::new("/repo/crates/foo");
        assert!(fires(&entry, layer, &subject(&["crates/foo/src/a.rs"])));
        assert!(!fires(&entry, layer, &subject(&["crates/bar/src/a.rs"])));
    }

    #[test]
    fn a_harness_condition_gates_on_the_calling_harness() {
        let entry = ContextFile {
            path: "docs/x.md".to_string(),
            when: Some(FileCondition {
                harness: vec!["claude-code".to_string()],
                ..Default::default()
            }),
        };
        assert!(!fires(&entry, Path::new("/repo"), &subject(&["a.rs"])));
    }

    #[test]
    fn an_env_condition_accepts_presence_or_a_value() {
        // SAFETY: single-threaded test, and the variable is unique to it.
        unsafe { std::env::set_var("DEVKIT_RULES_TEST_ENV", "staging") };
        let present = FileCondition {
            env: [("DEVKIT_RULES_TEST_ENV".to_string(), String::new())].into(),
            ..Default::default()
        };
        let exact = FileCondition {
            env: [("DEVKIT_RULES_TEST_ENV".to_string(), "staging".to_string())].into(),
            ..Default::default()
        };
        let wrong = FileCondition {
            env: [("DEVKIT_RULES_TEST_ENV".to_string(), "prod".to_string())].into(),
            ..Default::default()
        };
        let entry = |w| ContextFile {
            path: "docs/x.md".to_string(),
            when: Some(w),
        };
        assert!(fires(
            &entry(present),
            Path::new("/repo"),
            &subject(&["a.rs"])
        ));
        assert!(fires(
            &entry(exact),
            Path::new("/repo"),
            &subject(&["a.rs"])
        ));
        assert!(!fires(
            &entry(wrong),
            Path::new("/repo"),
            &subject(&["a.rs"])
        ));
        unsafe { std::env::remove_var("DEVKIT_RULES_TEST_ENV") };
    }

    /// A directory, a missing file, and invalid UTF-8 each read as nothing;
    /// none of them is an error the caller has to handle.
    #[test]
    fn an_unreadable_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_capped(dir.path(), 4096), None, "a directory");
        assert_eq!(
            read_capped(&dir.path().join("absent.md"), 4096),
            None,
            "missing"
        );

        let binary = dir.path().join("binary.md");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(read_capped(&binary, 4096), None, "invalid utf-8");

        let big = dir.path().join("big.md");
        std::fs::write(&big, "x".repeat(100)).unwrap();
        assert_eq!(read_capped(&big, 10), None, "over the cap");
        assert_eq!(
            read_capped(&big, 200).as_deref(),
            Some("x".repeat(100).as_str())
        );
    }
}
