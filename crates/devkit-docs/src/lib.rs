pub mod barrier;
pub mod cache;
pub mod importers;
pub mod layout;
pub mod lockfiles;
pub mod locks;
pub mod lookup;
pub mod manifest;
pub mod names;
pub mod refs;
pub mod resolve;
pub mod tags;

use std::path::Path;

pub struct DocsDoctor {
    pub libs: usize,
    pub bytes: u64,
    pub unreferenced: usize,
}

/// Cheap health summary for `devkit doctor`: lib count, cache size, and
/// version worktrees no registry row references.
pub fn doctor_summary(cache_root: &Path) -> DocsDoctor {
    let mut out = DocsDoctor {
        libs: 0,
        bytes: cache::dir_size(cache_root),
        unreferenced: 0,
    };
    let data = refs::RefStore::at(cache_root).snapshot();
    let referenced: std::collections::BTreeSet<(String, String)> = data
        .rows
        .iter()
        .map(|r| (r.lib.clone(), r.version.clone()))
        .collect();
    let Ok(rd) = std::fs::read_dir(cache_root) else {
        return out;
    };
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let dirname = e.file_name().to_string_lossy().into_owned();
        if locks::is_control(&dirname) {
            continue;
        }
        let name = names::decode(&dirname);
        out.libs += 1;
        for (wt, _) in cache::LibCache::from_dir(cache_root, &dirname).version_worktrees() {
            if !referenced.contains(&(name.clone(), wt)) {
                out.unreferenced += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_summary_counts_libs_and_unreferenced_worktrees() {
        let root = std::env::temp_dir().join(format!("devkit-docs-dr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // One lib with a referenced worktree, an unreferenced one, and default.
        for wt in ["1.0.0", "2.0.0", "default", "repo.git"] {
            std::fs::create_dir_all(root.join("tokio").join(wt)).unwrap();
        }
        std::fs::write(root.join("tokio/1.0.0/f"), "x").unwrap();
        refs::RefStore::at(&root)
            .commit(|d| {
                d.record("/some/project", "tokio", "1.0.0", "v1.0.0", "aaa");
                Ok(())
            })
            .unwrap();
        let s = doctor_summary(&root);
        assert_eq!(s.libs, 1);
        assert_eq!(s.unreferenced, 2); // 2.0.0 and default; repo.git is not a checkout
        assert!(s.bytes > 0);
    }

    #[test]
    fn doctor_summary_skips_registry_lock_directory() {
        let root =
            std::env::temp_dir().join(format!("devkit-docs-dr-controls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("tokio/default")).unwrap();
        std::fs::create_dir_all(root.join("registry.locks")).unwrap();

        let summary = doctor_summary(&root);

        assert_eq!(summary.libs, 1);
    }
}
