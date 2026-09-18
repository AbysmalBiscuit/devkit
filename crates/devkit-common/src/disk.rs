//! Disk accounting for the trees devkit owns.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use rayon::prelude::*;

/// Total bytes of the regular files under `path`. Hidden entries count,
/// because the bulk of these trees is dotted (`.venv`, `.next`, `.git`).
/// Symlinks are neither followed nor counted, so a link into a tree already
/// walked adds nothing twice.
///
/// An unreadable root or entry counts as zero: every caller is a report, which
/// a directory it cannot read should not end.
pub fn dir_size(path: &Path) -> u64 {
    crate::pool::install(|| walk(path, &HashMap::new()))
}

/// [`dir_size`], except that a directory whose path `known` records contributes
/// the recorded number and is not descended into. What a caller already
/// measured stays measured; everything else is still walked, so a tree that
/// grew a directory nobody recorded is still counted rather than silently
/// dropped.
///
/// A recorded directory that is no longer on disk contributes nothing, because
/// the substitution happens where the walk meets the entry. That is what keeps
/// a stale record from inventing bytes a deleted checkout no longer holds.
pub fn dir_size_with_known(path: &Path, known: &HashMap<PathBuf, u64>) -> u64 {
    if let Some(bytes) = known.get(path) {
        return *bytes;
    }
    crate::pool::install(|| walk(path, known))
}

/// Sizes come from the directory read itself. `DirEntry::file_type` and
/// `DirEntry::metadata` answer from what the read already returned, where a
/// fresh `fs::metadata` per path costs a file open apiece on Windows. That is
/// what rules out jwalk here, whose `DirEntry::metadata` always re-stats.
///
/// Recursion runs on the shared pool, entered once by [`dir_size`]; rayon
/// work-stealing joins the nested `par_iter`s to it.
/// The trees devkit sizes are dependency trees and source checkouts, so MiB is
/// the useful unit until one is small enough for it to read as nothing at all.
pub fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * KIB;
    match bytes {
        b if b >= MIB => format!("{} MiB", b / MIB),
        b if b >= KIB => format!("{} KiB", b / KIB),
        b => format!("{b} B"),
    }
}

fn walk(dir: &Path, known: &HashMap<PathBuf, u64>) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
    entries
        .par_iter()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => {
                let path = entry.path();
                match known.get(&path) {
                    Some(bytes) => *bytes,
                    None => walk(&path, known),
                }
            }
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

    /// A recorded directory is taken at its word and skipped; everything beside
    /// it is still walked. The second half is the point: the docs cache holds a
    /// shared object store, sidecars and lock files that no checkout record
    /// covers, and a total composed only of the recorded parts would lose them.
    #[test]
    fn a_known_directory_is_substituted_and_the_rest_still_walks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let recorded = root.join("recorded");
        std::fs::create_dir_all(&recorded).unwrap();
        std::fs::write(recorded.join("f"), vec![b'x'; 1000]).unwrap();
        std::fs::create_dir_all(root.join("walked")).unwrap();
        std::fs::write(root.join("walked").join("f"), vec![b'x'; 30]).unwrap();
        std::fs::write(root.join("loose"), vec![b'x'; 7]).unwrap();

        let known = HashMap::from([(recorded.clone(), 5)]);

        assert_eq!(dir_size(root), 1037);
        assert_eq!(dir_size_with_known(root, &known), 42);
    }

    /// The unit changes at each boundary and the division truncates, so a tree
    /// just short of the next unit must not round up into it.
    #[test]
    fn human_size_changes_unit_at_each_boundary() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1 KiB");
        assert_eq!(human_size(1024 * 1024 - 1), "1023 KiB");
        assert_eq!(human_size(1024 * 1024), "1 MiB");
        assert_eq!(human_size(2 * 1024 * 1024 - 1), "1 MiB");
    }

    /// A record outliving the directory it describes. Counting it anyway would
    /// report bytes a deleted checkout has already given back, which is the
    /// opposite of what a disk report is for.
    #[test]
    fn a_known_directory_that_is_gone_contributes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("loose"), vec![b'x'; 7]).unwrap();

        let known = HashMap::from([(root.join("deleted"), 9000)]);

        assert_eq!(dir_size_with_known(root, &known), 7);
    }
}
