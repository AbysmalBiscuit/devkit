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

use std::path::{Path, PathBuf};

/// Whether this daemon can enforce a hard cgroup-v2 memory cap.
#[derive(Debug)]
pub enum CgroupCaps {
    /// Hard caps available; `base` is the daemon-owned delegated subtree.
    Enforce { base: PathBuf },
    /// The platform has cgroups but this process can't enforce (cgroup-v1,
    /// missing memory controller, or a non-writable / non-delegated subtree).
    Unavailable { reason: String },
    /// The platform has no cgroups at all (macOS / Windows).
    Unsupported,
}

/// Probe whether a hard memory cap can be enforced, preparing the daemon's
/// delegated cgroup base when so. Linux-only; other platforms return
/// `Unsupported`.
pub fn cgroup_caps() -> CgroupCaps {
    imp::cgroup_caps()
}

/// Create `<base>/servers/<name>/`, set `memory.max` and `memory.oom.group=1`.
/// Reuses the leaf if it already exists (rewriting `memory.max`). Off-Linux this
/// errors (`Unsupported` callers never reach it).
pub fn cgroup_create_leaf(base: &Path, name: &str, max_bytes: u64) -> anyhow::Result<PathBuf> {
    imp::cgroup_create_leaf(base, name, max_bytes)
}

/// Remove a leaf cgroup (`rmdir`; succeeds only when empty).
pub fn cgroup_remove_leaf(leaf: &Path) -> anyhow::Result<()> {
    imp::cgroup_remove_leaf(leaf)
}

/// Leaf directory names under `<base>/servers/`. Empty on any error or off-Linux.
pub fn cgroup_list_leaves(base: &Path) -> Vec<String> {
    imp::cgroup_list_leaves(base)
}

/// Register a `pre_exec` step that moves the child into `leaf` before `exec`,
/// best-effort (fail-open). No-op off-Linux. Call after `detach`.
pub fn join_cgroup(cmd: &mut std::process::Command, leaf: &Path) {
    imp::join_cgroup(cmd, leaf)
}

/// Format a non-negative pid into `buf` as decimal ASCII, returning the written
/// slice. Async-signal-safe: pure arithmetic into a caller buffer, no allocation,
/// so it is safe to call from a post-fork `pre_exec` closure.
#[allow(dead_code)]
fn fmt_pid(pid: i64, buf: &mut [u8; 20]) -> &[u8] {
    let mut n = pid.max(0) as u64;
    let mut i = buf.len();
    if n == 0 {
        i -= 1;
        buf[i] = b'0';
        return &buf[i..];
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[i..]
}

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

    #[test]
    fn fmt_pid_formats_decimal() {
        let mut buf = [0u8; 20];
        assert_eq!(fmt_pid(0, &mut buf), b"0");
        assert_eq!(fmt_pid(7, &mut buf), b"7");
        assert_eq!(fmt_pid(12345, &mut buf), b"12345");
        // pid_t max on Linux is 2^22, but format the full i32 range to be safe.
        assert_eq!(fmt_pid(2147483647, &mut buf), b"2147483647");
    }
}
