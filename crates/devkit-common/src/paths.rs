use std::path::PathBuf;

/// `~/.claude/state/devkit/` — registry + lock + logs live here.
pub fn state_dir() -> PathBuf {
    home().join(".claude/state/devkit")
}
pub fn registry_file() -> PathBuf { state_dir().join("ports.json") }
pub fn lock_file() -> PathBuf { state_dir().join("ports.lock") }
pub fn logs_dir() -> PathBuf { state_dir().join("logs") }

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_under_state() {
        assert!(registry_file().ends_with(".claude/state/devkit/ports.json"));
        assert!(logs_dir().ends_with(".claude/state/devkit/logs"));
    }
}
