use std::path::{Path, PathBuf};

/// Agent-neutral state home: `$XDG_STATE_HOME/devkit` (default
/// `~/.local/state/devkit`).
///
/// Pure resolution (stat only, never writes): prefer the XDG path when it
/// exists; otherwise fall back to the legacy `~/.claude/state/devkit` in place
/// when it exists (so live state is never orphaned before
/// `migrate_legacy_state` runs); otherwise the XDG path. Run
/// `migrate_legacy_state()` once at process startup to move the data.
pub fn state_dir() -> PathBuf {
    let new = xdg_state_home().join("devkit");
    let legacy = home().join(".claude/state/devkit");
    let (ne, le) = (new.exists(), legacy.exists());
    pick_state_dir(new, legacy, ne, le)
}

fn pick_state_dir(new: PathBuf, legacy: PathBuf, new_exists: bool, legacy_exists: bool) -> PathBuf {
    if new_exists {
        new
    } else if legacy_exists {
        legacy
    } else {
        new
    }
}

fn xdg_state_home() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => default_state_home(),
    }
}

/// Platform default for the state home when `XDG_STATE_HOME` is unset.
#[cfg(windows)]
fn default_state_home() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => home().join("AppData/Local"),
    }
}

#[cfg(not(windows))]
fn default_state_home() -> PathBuf {
    home().join(".local/state")
}

/// One-time best-effort migration of the legacy `~/.claude/state/devkit` home
/// to the XDG state dir. No-op if the new home already exists or the legacy one
/// is absent. On rename failure (cross-device, permissions) the legacy dir is
/// left in place and `state_dir()` keeps resolving to it.
pub fn migrate_legacy_state() {
    migrate_state_between(
        &xdg_state_home().join("devkit"),
        &home().join(".claude/state/devkit"),
    );
}

fn migrate_state_between(new: &Path, legacy: &Path) {
    if new.exists() || !legacy.exists() {
        return;
    }
    if let Some(parent) = new.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::rename(legacy, new);
}

pub fn registry_file() -> PathBuf {
    state_dir().join("ports.json")
}
pub fn lock_file() -> PathBuf {
    state_dir().join("ports.lock")
}
pub fn locks_file() -> PathBuf {
    state_dir().join("locks.json")
}
pub fn locks_lock() -> PathBuf {
    state_dir().join("locks.lock")
}
pub fn logs_dir() -> PathBuf {
    state_dir().join("logs")
}
/// Unix socket the daemon binds; clients connect here.
pub fn port_socket_file() -> PathBuf {
    state_dir().join("ports.sock")
}
/// Unix socket the daemon binds for the lock registry; clients connect here.
pub fn lock_socket_file() -> PathBuf {
    state_dir().join("locks.sock")
}
/// Single-instance lock for the daemon — separate from the registry's
/// `ports.lock`.
pub fn devkitd_lock() -> PathBuf {
    state_dir().join("devkitd.lock")
}
/// Daemon log file.
pub fn daemon_log() -> PathBuf {
    logs_dir().join("devkitd.log")
}

/// `$XDG_DATA_HOME/devkit` or `~/.local/share/devkit`.
pub fn data_dir() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("devkit"),
        _ => home().join(".local/share/devkit"),
    }
}

/// `$XDG_CACHE_HOME/devkit` or `~/.cache/devkit`.
pub fn cache_dir() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("devkit"),
        _ => home().join(".cache/devkit"),
    }
}

/// The user's home directory.
///
/// The rules cache path needs this to reach a non-XDG location on all
/// platforms.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
        return PathBuf::from(h);
    }
    // Windows has no HOME; the user profile is the home equivalent.
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("USERPROFILE").filter(|s| !s.is_empty()) {
        return PathBuf::from(p);
    }
    panic!("HOME must be set");
}

/// The `systemd --user` unit path for the daemon:
/// `~/.config/systemd/user/devkitd.service`. Honors `$XDG_CONFIG_HOME`, else
/// `$HOME/.config`.
pub fn systemd_user_unit() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    config.join("systemd/user/devkitd.service")
}

/// The final path component (basename) of `path`, if any.
pub fn leaf(path: &str) -> Option<&str> {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
}

/// Whether two paths name one directory, and whether that could be decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathIdentity {
    Same,
    Different,
    /// Neither answer is established: a resolution failed for a reason other
    /// than the path being absent, so the two may or may not be one directory.
    Unknown,
}

/// Compare two paths by identity, keeping "cannot tell" apart from "not the
/// same".
///
/// A path that is absent is decidably not the path that resolved, so only a
/// resolution that fails for another reason — a permission on some parent, an
/// I/O error — is `Unknown`. When neither path exists there is nothing to
/// resolve and a lexical comparison is the whole of the available answer.
///
/// [`same_path`] folds `Unknown` into `false`, which is the safe reading
/// wherever a mismatch costs a permission. It is the wrong reading wherever a
/// mismatch *grants* one — deciding that no live server holds a directory
/// about to be deleted, most of all — and those callers match on this instead.
pub fn path_identity(a: &Path, b: &Path) -> PathIdentity {
    let decide = |x: &Path, y: &Path| {
        if x == y {
            PathIdentity::Same
        } else {
            PathIdentity::Different
        }
    };
    let missing = |e: &std::io::Error| e.kind() == std::io::ErrorKind::NotFound;
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ra), Ok(rb)) => decide(&ra, &rb),
        (Err(e), Ok(_)) | (Ok(_), Err(e)) if missing(&e) => PathIdentity::Different,
        (Err(ea), Err(eb)) if missing(&ea) && missing(&eb) => decide(a, b),
        _ => PathIdentity::Unknown,
    }
}

/// Whether two paths name one directory, reading "cannot tell" as "no".
///
/// Correct wherever a mismatch costs a permission rather than granting one. A
/// caller for which an undecided answer is dangerous uses [`path_identity`].
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_identity(a, b) == PathIdentity::Same
}

/// `dir` resolved, or an error when the current directory is `dir` or inside
/// it: the guard every removal runs so it never deletes where its caller
/// stands.
///
/// Both sides resolve or the guard refuses. A side that fell back to its
/// unresolved spelling would be compared against a resolved one, and on
/// Windows the two can never match at all, since `canonicalize` returns a
/// `\\?\`-prefixed path and `current_dir` does not. A fallback here silently
/// disarms the check.
pub fn refuse_if_inside(dir: &Path) -> anyhow::Result<PathBuf> {
    use anyhow::Context;
    let here =
        std::fs::canonicalize(dir).with_context(|| format!("resolving {}", dir.display()))?;
    let cwd = std::env::current_dir().context("resolving the current directory")?;
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("resolving the current directory {}", cwd.display()))?;
    anyhow::ensure!(
        !cwd.starts_with(&here),
        "cd out of {} before removing it",
        dir.display()
    );
    Ok(here)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_spellings_of_one_directory_are_the_same_path() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let indirect = tmp.path().join("real/./../real");
        assert_eq!(path_identity(&real, &indirect), PathIdentity::Same);
    }

    #[test]
    fn a_path_that_is_absent_is_decidably_not_one_that_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let gone = tmp.path().join("gone");
        assert_eq!(path_identity(&real, &gone), PathIdentity::Different);
        assert_eq!(path_identity(&gone, &real), PathIdentity::Different);
    }

    #[test]
    fn two_absent_paths_fall_back_to_a_lexical_answer() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("gone-a");
        let b = tmp.path().join("gone-b");
        assert_eq!(path_identity(&a, &a), PathIdentity::Same);
        assert_eq!(path_identity(&a, &b), PathIdentity::Different);
    }

    /// A resolution that fails for a reason other than absence establishes
    /// nothing. Folding it into "different" is what lets a deletion past the
    /// live-server refusal that reads this.
    #[cfg(unix)]
    #[test]
    fn a_path_that_cannot_be_resolved_is_unknown_rather_than_different() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let inside = locked.join("tree");
        std::fs::create_dir(&inside).unwrap();
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).unwrap();

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let verdict = path_identity(&inside, &other);
        // Restored before the assert so a failure cannot leave the tempdir
        // undeletable.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(verdict, PathIdentity::Unknown);
        assert!(
            !same_path(&inside, &other),
            "same_path still reads Unknown as no"
        );
    }

    /// When both resolutions fail, only *absence* on both sides decides
    /// anything; a failure for any other reason leaves the pair undecided
    /// however the other side failed. Reading either of these as the lexical
    /// answer is what lets a deletion past the live-server refusal.
    #[cfg(unix)]
    #[test]
    fn a_double_failure_is_unknown_unless_both_paths_are_merely_absent() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        let a = locked.join("a");
        let b = locked.join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let absent = tmp.path().join("gone");

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let verdicts = [
            path_identity(&a, &b),
            path_identity(&a, &absent),
            path_identity(&absent, &a),
        ];
        // Restored before the asserts so a failure cannot leave the tempdir
        // undeletable.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            verdicts,
            [PathIdentity::Unknown; 3],
            "unreadable/unreadable and unreadable/absent are both undecided"
        );
    }

    #[test]
    fn registry_under_state() {
        assert!(registry_file().ends_with("devkit/ports.json"));
        assert!(logs_dir().ends_with("devkit/logs"));
    }
    #[test]
    fn lock_paths_under_state() {
        assert!(locks_file().ends_with("devkit/locks.json"));
        assert!(locks_lock().ends_with("devkit/locks.lock"));
    }
    #[test]
    fn leaf_is_basename() {
        assert_eq!(leaf("/a/b/eng-1234"), Some("eng-1234"));
        assert_eq!(leaf("solo"), Some("solo"));
    }
    #[test]
    fn daemon_paths_under_state() {
        assert!(port_socket_file().ends_with("devkit/ports.sock"));
        assert!(devkitd_lock().ends_with("devkit/devkitd.lock"));
        assert!(daemon_log().ends_with("logs/devkitd.log"));
        assert!(lock_socket_file().ends_with("devkit/locks.sock"));
    }

    #[test]
    fn pick_prefers_new_when_present() {
        let n = PathBuf::from("/new/devkit");
        let l = PathBuf::from("/legacy/devkit");
        assert_eq!(pick_state_dir(n.clone(), l.clone(), true, true), n);
        assert_eq!(pick_state_dir(n.clone(), l.clone(), true, false), n);
    }
    #[test]
    fn pick_falls_back_to_legacy_in_place() {
        let n = PathBuf::from("/new/devkit");
        let l = PathBuf::from("/legacy/devkit");
        assert_eq!(pick_state_dir(n.clone(), l.clone(), false, true), l);
    }
    #[test]
    fn pick_defaults_to_new_when_neither_exists() {
        let n = PathBuf::from("/new/devkit");
        let l = PathBuf::from("/legacy/devkit");
        assert_eq!(pick_state_dir(n.clone(), l.clone(), false, false), n);
    }
    #[test]
    fn systemd_user_unit_under_config() {
        assert!(systemd_user_unit().ends_with("systemd/user/devkitd.service"));
    }
    #[test]
    fn migrate_moves_legacy_to_new() {
        let base = tempfile::tempdir().unwrap();
        let new = base.path().join("new/devkit");
        let legacy = base.path().join("legacy/devkit");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("ports.json"), b"{}").unwrap();

        migrate_state_between(&new, &legacy);

        assert!(new.join("ports.json").exists(), "data moved to new home");
        assert!(!legacy.exists(), "legacy home removed after move");
    }
}
