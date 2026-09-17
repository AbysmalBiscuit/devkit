//! Disk accounting for the trees devkit owns.

use std::path::Path;

use rayon::prelude::*;

/// Total bytes of the regular files under `path`. Hidden entries count,
/// because the bulk of these trees is dotted (`.venv`, `.next`, `.git`).
/// Symlinks are neither followed nor counted, so a link into a tree already
/// walked adds nothing twice.
///
/// An unreadable root or entry counts as zero: every caller is a report, which
/// a directory it cannot read should not end.
pub fn dir_size(path: &Path) -> u64 {
    crate::pool::install(|| walk(path))
}

/// Sizes come from the directory read itself. `DirEntry::file_type` and
/// `DirEntry::metadata` answer from what the read already returned, where a
/// fresh `fs::metadata` per path costs a file open apiece on Windows. That is
/// what rules out jwalk here, whose `DirEntry::metadata` always re-stats.
///
/// Recursion runs on the shared pool, entered once by [`dir_size`]; rayon
/// work-stealing joins the nested `par_iter`s to it.
fn walk(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
    entries
        .par_iter()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => walk(&entry.path()),
            Ok(t) if t.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The biggest directories in the trees devkit sizes are dotted: a
    /// baseline's `.venv`, `.next`, `.turbo` and `.devkit` marker, a docs
    /// checkout's `.git`. A walker that skipped them, as jwalk does by
    /// default, would report a fraction of the tree or nothing at all.
    #[test]
    fn a_size_counts_hidden_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("visible"), vec![b'x'; 100]).unwrap();
        assert_eq!(dir_size(dir.path()), 100);

        std::fs::create_dir_all(dir.path().join(".venv").join("lib")).unwrap();
        std::fs::write(dir.path().join(".venv").join("lib").join("f"), vec![
            b'x';
            500
        ])
        .unwrap();
        std::fs::write(dir.path().join(".dotfile"), vec![b'x'; 7]).unwrap();

        assert_eq!(dir_size(dir.path()), 607, "hidden entries were skipped");
    }

    #[test]
    fn an_absent_root_sizes_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dir_size(&dir.path().join("never-created")), 0);
    }
}
