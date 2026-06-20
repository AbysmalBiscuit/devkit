use std::path::{Path, PathBuf};

/// Agent-neutral state home: `$XDG_STATE_HOME/devkit` (default `~/.local/state/devkit`).
///
/// Pure resolution (stat only, never writes): prefer the XDG path when it exists;
/// otherwise fall back to the legacy `~/.claude/state/devkit` in place when it exists
/// (so live state is never orphaned before `migrate_legacy_state` runs); otherwise the
/// XDG path. Run `migrate_legacy_state()` once at process startup to move the data.
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
        _ => home().join(".local/state"),
    }
}

/// One-time best-effort migration of the legacy `~/.claude/state/devkit` home to the
/// XDG state dir. No-op if the new home already exists or the legacy one is absent.
/// On rename failure (cross-device, permissions) the legacy dir is left in place and
/// `state_dir()` keeps resolving to it.
pub fn migrate_legacy_state() {
    migrate_state_between(&xdg_state_home().join("devkit"), &home().join(".claude/state/devkit"));
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

pub fn registry_file() -> PathBuf { state_dir().join("ports.json") }
pub fn lock_file() -> PathBuf { state_dir().join("ports.lock") }
pub fn locks_file() -> PathBuf { state_dir().join("locks.json") }
pub fn locks_lock() -> PathBuf { state_dir().join("locks.lock") }
pub fn logs_dir() -> PathBuf { state_dir().join("logs") }
/// Unix socket the daemon binds; clients connect here.
pub fn socket_file() -> PathBuf { state_dir().join("portd.sock") }
/// Single-instance lock for the daemon — separate from the registry's `ports.lock`.
pub fn daemon_lock_file() -> PathBuf { state_dir().join("portd.lock") }
/// Daemon log file.
pub fn daemon_log() -> PathBuf { logs_dir().join("portd.log") }

/// `$XDG_CACHE_HOME/devkit` or `~/.cache/devkit`.
pub fn cache_dir() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("devkit"),
        _ => home().join(".cache/devkit"),
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME must be set"))
}

/// The final path component (basename) of `path`, if any.
pub fn leaf(path: &str) -> Option<&str> {
    std::path::Path::new(path).file_name().and_then(|s| s.to_str())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(socket_file().ends_with("devkit/portd.sock"));
        assert!(daemon_lock_file().ends_with("devkit/portd.lock"));
        assert!(daemon_log().ends_with("devkit/logs/portd.log"));
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
    fn migrate_moves_legacy_to_new() {
        let base = std::env::temp_dir().join(format!("devkit-paths-{}", std::process::id()));
        let new = base.join("new/devkit");
        let legacy = base.join("legacy/devkit");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("ports.json"), b"{}").unwrap();

        migrate_state_between(&new, &legacy);

        assert!(new.join("ports.json").exists(), "data moved to new home");
        assert!(!legacy.exists(), "legacy home removed after move");
        let _ = std::fs::remove_dir_all(&base);
    }
}
