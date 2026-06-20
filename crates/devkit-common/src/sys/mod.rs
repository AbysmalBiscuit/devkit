//! Platform abstraction boundary. Every OS-specific primitive lives behind this
//! module so the rest of the workspace stays platform-agnostic. The `unix` and
//! `windows` backends both implement the API; the active one is selected at
//! compile time.

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;

#[cfg(windows)]
#[path = "windows.rs"]
mod imp;

/// True if a process with `pid` currently exists.
pub fn process_alive(pid: u32) -> bool {
    imp::process_alive(pid)
}

/// Ask `pid` to terminate, gracefully where the platform supports it.
pub fn terminate(pid: u32) {
    imp::terminate(pid)
}

/// Configure `cmd` to start detached from the caller's session/process group.
/// Must be called before `spawn`.
pub fn detach(cmd: &mut std::process::Command) {
    imp::detach(cmd)
}

/// Non-blocking reap/poll of an owned child. Returns `true` once it has exited.
pub fn reap_owned(pid: u32) -> bool {
    imp::reap_owned(pid)
}

/// Resident set size (bytes) summed over the process subtree rooted at `root`
/// (the process plus every descendant). Returns 0 if the root is gone.
pub fn tree_rss_bytes(root: u32) -> u64 {
    imp::tree_rss_bytes(root)
}

/// Parent process id, on platforms that expose one.
pub fn parent_pid() -> Option<u32> {
    imp::parent_pid()
}

/// The controlling terminal's name, when stdin is attached to one.
pub fn controlling_tty() -> Option<String> {
    imp::controlling_tty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_not_alive() {
        assert!(!process_alive(0));
    }

    #[test]
    fn tree_rss_of_self_is_nonzero() {
        assert!(tree_rss_bytes(std::process::id()) > 0);
    }

    #[test]
    fn parent_pid_is_present() {
        assert!(parent_pid().is_some());
    }
}
