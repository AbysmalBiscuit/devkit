//! Where the baseline comparison starts. Every consumer of `baseline_ref`
//! resolves through here, so a project that declares nothing still gets the
//! remote's default branch.

mod locks;

use anyhow::{Context, Result};
use devkit_common::worktree::BASELINE_MARKER;
use devkit_config::Config;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The ref a worktree's baseline is measured against: the configured
/// `baseline_ref`, else the remote's default branch.
pub fn target(cfg: &Config, repo: &Path) -> Result<String> {
    if !cfg.defaults.baseline_ref.is_empty() {
        return Ok(cfg.defaults.baseline_ref.clone());
    }
    devkit_common::git::default_remote_branch(repo).context(
        "no baseline target: set `defaults.baseline_ref`, \
         or run `git remote set-head origin -a` so origin/HEAD names one",
    )
}

/// The commit this worktree forked from: the merge base of its HEAD and
/// `target`. Local refs only, so no fetch is needed — extending a branch does
/// not move its merge base with another branch, and the value changes only
/// when the worktree is rebased.
#[allow(dead_code)]
pub fn pin(worktree: &Path, target: &str) -> Result<String> {
    let out = devkit_common::git::Git::at(worktree)
        .args(["merge-base", "HEAD", target])
        .output()
        .with_context(|| format!("resolving the fork point between HEAD and `{target}`"))?;
    let sha = out.trim();
    anyhow::ensure!(
        !sha.is_empty(),
        "`git merge-base HEAD {target}` named no commit"
    );
    Ok(sha.to_string())
}

/// One app's prep fingerprint at the sha the baseline was built from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppMark {
    pub fingerprint: String,
}

/// The contents of a baseline worktree's marker file: the sha it was built at,
/// and each app's prep fingerprint at that build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub sha: String,
    #[serde(default)]
    pub apps: BTreeMap<String, AppMark>,
}

/// The result of reading a baseline's marker. `Absent` and `Unusable` are
/// both "no marker to trust", but only `Absent` means a fresh baseline may be
/// built here — `Unusable` means something occupies the path already and
/// must be dealt with before a rebuild can proceed.
#[allow(dead_code)]
pub enum MarkerState {
    Ok(Marker),
    Unusable,
    Absent,
}

/// Written last, after every bootstrap step, so its presence is what makes a
/// baseline complete: a directory without one is an interrupted bootstrap
/// whatever its HEAD says. It also carries identity, which lets a stray
/// directory be told from a real baseline, and each app's prep fingerprint.
#[allow(dead_code)]
pub fn write_marker(dir: &Path, m: &Marker) -> Result<()> {
    let p = dir.join(BASELINE_MARKER);
    let parent = p.parent().expect("marker path has a parent");
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let body = toml::to_string(m).context("serializing baseline marker")?;
    // Rename rather than write in place: a crash partway through a write would
    // otherwise leave a file that parses as neither a marker nor its absence.
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("renaming into {}", p.display()))
}

/// Reads a baseline's marker. A missing file is `Absent`: nothing was ever
/// built here, so the directory is free to build fresh. Any other read
/// failure (permission denied, the path occupied by something unreadable as
/// a file) is `Unusable`, not `Absent` — an I/O error does not prove there is
/// no baseline, only that this call could not confirm one, and treating that
/// as a clean slate risks building over a baseline other worktrees still
/// reference. A file that reads but does not parse as TOML is `Unusable` for
/// the same reason: something occupies the path and cannot be trusted.
#[allow(dead_code)]
pub fn read_marker(dir: &Path) -> MarkerState {
    match std::fs::read_to_string(dir.join(BASELINE_MARKER)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MarkerState::Absent,
        Err(_) => MarkerState::Unusable,
        Ok(body) => match toml::from_str(&body) {
            Ok(m) => MarkerState::Ok(m),
            Err(_) => MarkerState::Unusable,
        },
    }
}

/// Candidate directories tried for one sha before giving up. Two different
/// shas sharing a 12-character prefix is already rare; this many sharing one
/// is far beyond plausible, so hitting the bound signals a bug (or a
/// deliberately doctored marker) rather than a baseline directory to build.
const MAX_SLOT_CANDIDATES: u32 = 64;

/// Which directory serves `sha`, and what state it is in.
#[allow(dead_code)]
#[must_use]
pub enum Slot {
    /// A complete baseline for this exact sha is already here.
    Reuse(PathBuf, Marker),
    /// Something occupies the path but its marker cannot be trusted.
    Rebuild(PathBuf),
    /// Nothing occupies the path: build fresh here.
    Create(PathBuf),
    /// No candidate resolved within `MAX_SLOT_CANDIDATES` tries.
    Exhausted(String),
}

/// Directory-name form of a sha. Twelve hex characters is 48 bits against a
/// few dozen directories, and it leaves Windows path headroom a 40-character
/// name would spend.
///
/// Takes the first 12 *characters*, not bytes: slicing a `&str` by raw byte
/// index panics when that index falls inside a multi-byte character. Real
/// shas are hex ASCII where the two coincide, but this function accepts any
/// `&str`, so it walks char boundaries instead of assuming one.
#[allow(dead_code)]
pub fn short(sha: &str) -> &str {
    match sha.char_indices().nth(12) {
        Some((idx, _)) => &sha[..idx],
        None => sha,
    }
}

/// Which directory serves `sha`, and in what state. An interrupted bootstrap
/// leaves a registered worktree with no marker; classifying that as occupied
/// would strand it, since the baseline would move to `_2`, prune reports
/// rather than removes it, and the worktree filter would stop recognizing it.
///
/// Twelve-character names collide across unrelated shas, so a marker naming
/// a different sha does not mean rebuild — it means this sha belongs in the
/// next candidate, `<short>_2` and onward. The walk is bounded
/// (`MAX_SLOT_CANDIDATES`) so a run of collisions reports `Exhausted` instead
/// of looping forever.
#[allow(dead_code)]
pub fn slot(baseline_dir: &Path, sha: &str) -> Slot {
    let base = short(sha);
    for n in 1..=MAX_SLOT_CANDIDATES {
        let name = if n == 1 {
            base.to_string()
        } else {
            format!("{base}_{n}")
        };
        let path = baseline_dir.join(&name);
        match read_marker(&path) {
            MarkerState::Ok(m) if m.sha == sha => return Slot::Reuse(path, m),
            MarkerState::Ok(_) => continue,
            MarkerState::Unusable => return Slot::Rebuild(path),
            MarkerState::Absent => {
                return match std::fs::metadata(&path) {
                    Ok(_) => Slot::Rebuild(path),
                    Err(_) => Slot::Create(path),
                };
            }
        }
    }
    Slot::Exhausted(format!(
        "no free or reusable baseline slot for `{sha}` after {MAX_SLOT_CANDIDATES} \
         candidates under `{}`",
        baseline_dir.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut apps = std::collections::BTreeMap::new();
        apps.insert(
            "api".to_string(),
            AppMark {
                fingerprint: "9f2c".into(),
            },
        );
        let m = Marker {
            sha: "d13d90b724bf".into(),
            apps,
        };
        write_marker(dir.path(), &m).unwrap();
        assert!(matches!(read_marker(dir.path()), MarkerState::Ok(got) if got == m));
    }

    #[test]
    fn an_absent_marker_is_absent_and_a_corrupt_one_is_unusable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(read_marker(dir.path()), MarkerState::Absent));
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            dir.path().join(devkit_common::worktree::BASELINE_MARKER),
            "sha = ",
        )
        .unwrap();
        assert!(matches!(read_marker(dir.path()), MarkerState::Unusable));
    }

    /// A non-`NotFound` read error (here, the marker path occupied by a
    /// directory instead of a file) must not read as `Absent` — that would
    /// tell a caller a live baseline it cannot read is a clean slate to
    /// build fresh over.
    #[test]
    fn an_unreadable_marker_is_unusable_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join(devkit_common::worktree::BASELINE_MARKER);
        std::fs::create_dir_all(&marker_path).unwrap();

        let err = std::fs::read_to_string(&marker_path).unwrap_err();
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "test assumes reading a directory as a file yields a non-NotFound error on this platform"
        );

        assert!(matches!(read_marker(dir.path()), MarkerState::Unusable));
    }

    const SHA: &str = "d13d90b724bf8a3c0000000000000000000000ab";
    const OTHER: &str = "0123456789ab0000000000000000000000000000";

    fn place(root: &std::path::Path, name: &str, sha: &str) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        write_marker(
            &d,
            &Marker {
                sha: sha.into(),
                apps: Default::default(),
            },
        )
        .unwrap();
    }

    #[test]
    fn an_empty_dir_creates_at_the_short_sha() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            matches!(slot(root.path(), SHA), Slot::Create(p) if p == root.path().join("d13d90b724bf"))
        );
    }

    #[test]
    fn a_matching_marker_is_reused() {
        let root = tempfile::tempdir().unwrap();
        place(root.path(), "d13d90b724bf", SHA);
        assert!(matches!(slot(root.path(), SHA), Slot::Reuse(..)));
    }

    #[test]
    fn a_colliding_marker_moves_to_the_next_candidate() {
        let root = tempfile::tempdir().unwrap();
        place(root.path(), "d13d90b724bf", OTHER);
        assert!(matches!(slot(root.path(), SHA), Slot::Create(p) if p.ends_with("d13d90b724bf_2")));
    }

    #[test]
    fn a_markerless_directory_is_rebuilt_in_place() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("d13d90b724bf")).unwrap();
        assert!(matches!(slot(root.path(), SHA), Slot::Rebuild(p) if p.ends_with("d13d90b724bf")));
    }

    #[test]
    fn a_corrupt_marker_is_rebuilt_in_place() {
        let root = tempfile::tempdir().unwrap();
        let d = root.path().join("d13d90b724bf");
        std::fs::create_dir_all(d.join(".devkit")).unwrap();
        std::fs::write(d.join(devkit_common::worktree::BASELINE_MARKER), "sha = ").unwrap();
        assert!(matches!(slot(root.path(), SHA), Slot::Rebuild(_)));
    }

    /// Every candidate up to the bound collides with a genuinely different
    /// sha (not an empty or corrupt marker), so the only way out is the
    /// bound itself — proving the walk terminates instead of looping forever.
    #[test]
    fn exhausting_the_candidate_bound_reports_instead_of_looping() {
        let root = tempfile::tempdir().unwrap();
        for n in 1..=MAX_SLOT_CANDIDATES {
            let name = if n == 1 {
                "d13d90b724bf".to_string()
            } else {
                format!("d13d90b724bf_{n}")
            };
            let other = format!("d13d90b724bf{n:028x}");
            assert_ne!(other, SHA, "constructed collision must not equal SHA");
            place(root.path(), &name, &other);
        }
        assert!(matches!(slot(root.path(), SHA), Slot::Exhausted(_)));
    }

    #[test]
    fn short_truncates_to_twelve_chars() {
        assert_eq!(short(SHA), "d13d90b724bf");
    }

    #[test]
    fn short_of_a_short_input_returns_it_whole() {
        assert_eq!(short("abcd"), "abcd");
    }

    #[test]
    fn short_of_an_empty_input_returns_empty() {
        assert_eq!(short(""), "");
    }

    /// `short` must not panic when the 12th byte would split a multi-byte
    /// UTF-8 character — real shas are hex ASCII, but the function takes any
    /// `&str` and a caller could pass something else.
    #[test]
    fn short_does_not_panic_on_a_multibyte_boundary() {
        let s = "1234567890€23";
        let _ = short(s);
    }

    #[test]
    fn a_configured_ref_wins_over_detection() {
        let mut cfg = devkit_config::Config::default();
        cfg.defaults.baseline_ref = "origin/release".into();
        // Detection would fail in a non-repo path; the configured ref means it is
        // never consulted.
        let got = target(&cfg, std::path::Path::new("/nonexistent")).unwrap();
        assert_eq!(got, "origin/release");
    }

    #[test]
    fn an_undetectable_target_names_both_fixes() {
        let tmp = tempfile::tempdir().unwrap();
        devkit_common::git::Git::fixture(tmp.path())
            .args(["init", "-q"])
            .output()
            .unwrap();
        let err = target(&devkit_config::Config::default(), tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("baseline_ref"), "{msg}");
        assert!(msg.contains("git remote set-head"), "{msg}");
    }

    /// Two commits on main, a branch cut from the first, then main advances:
    /// the merge base stays at the fork point.
    #[test]
    fn the_pin_is_the_fork_point_not_the_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        let fork = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-qb", "feat"]);
        std::fs::write(p.join("b"), "2").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "two"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(p.join("c"), "3").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "three"]);
        git(&["checkout", "-q", "feat"]);

        let got = pin(p, "main").unwrap();
        assert_eq!(got, fork);
        assert_eq!(got.len(), 40);
    }

    /// Rebasing `feat` onto a `main` that has advanced moves the merge base to
    /// `main`'s new tip: this is what re-resolving after a rebase relies on.
    #[test]
    fn a_rebase_moves_the_pin_to_the_new_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        let first_fork = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-qb", "feat"]);
        std::fs::write(p.join("b"), "2").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "two"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(p.join("c"), "3").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "three"]);
        let new_tip = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-q", "feat"]);

        assert_eq!(pin(p, "main").unwrap(), first_fork);

        git(&["rebase", "main"]);

        let rebased = pin(p, "main").unwrap();
        assert_eq!(rebased, new_tip);
        assert_ne!(rebased, first_fork);
    }

    #[test]
    fn unrelated_histories_error_naming_both_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        git(&["checkout", "-q", "--orphan", "lonely"]);
        git(&["commit", "-qm", "orphan", "--allow-empty"]);

        let err = pin(p, "main").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("HEAD"), "{msg}");
        assert!(msg.contains("main"), "{msg}");
    }
}
