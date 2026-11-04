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

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

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
/// Reuses the leaf if it already exists (rewriting `memory.max`). Off-Linux
/// this errors (`Unsupported` callers never reach it).
pub fn cgroup_create_leaf(base: &Path, name: &str, max_bytes: u64) -> anyhow::Result<PathBuf> {
    imp::cgroup_create_leaf(base, name, max_bytes)
}

/// Remove a leaf cgroup (`rmdir`; succeeds only when empty).
pub fn cgroup_remove_leaf(leaf: &Path) -> anyhow::Result<()> {
    imp::cgroup_remove_leaf(leaf)
}

/// Leaf directory names under `<base>/servers/`. Empty on any error or
/// off-Linux.
pub fn cgroup_list_leaves(base: &Path) -> Vec<String> {
    imp::cgroup_list_leaves(base)
}

/// The hidden `devkitd` subcommand that joins a cgroup leaf and then replaces
/// itself with a program: `devkitd __cgroup-exec <leaf> -- <program> <args>`.
pub const CGROUP_EXEC: &str = "__cgroup-exec";

/// A command that runs `program` inside the cgroup `leaf`, or `None` off Linux,
/// where a leaf has no meaning. It runs this binary's [`CGROUP_EXEC`]
/// subcommand, so only `devkitd` may call it. The caller appends the program's
/// arguments and gives the command the write end of a pipe as its stdin. That
/// pipe closes empty when the program starts, and carries the error when the
/// program cannot start.
pub fn cgroup_command(leaf: &Path, program: &str) -> Option<std::process::Command> {
    imp::cgroup_command(leaf, program)
}

/// Move this process into `leaf`, then replace it with `program`. The pid
/// stays the same, so the program and every process it starts are in the
/// leaf, with stdin at the null device. A leaf that cannot be joined leaves the
/// program uncapped. Returns only when the exec fails, after it writes the
/// error to the status pipe that stdin holds on entry.
#[cfg(unix)]
pub fn exec_in_cgroup(
    leaf: &Path,
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
) -> std::io::Error {
    imp::exec_in_cgroup(leaf, program, args)
}

/// True if a process with `pid` currently exists.
pub fn process_alive(pid: u32) -> bool {
    imp::process_alive(pid)
}

/// Ask `pid` to terminate, gracefully where the platform supports it.
pub fn terminate(pid: u32) {
    imp::terminate(pid)
}

/// Configure `cmd` to start detached from the caller's terminal, holding none
/// of this process's stdio, so a caller reading this process's output to its
/// end is not held open by the child.
///
/// On Unix the child leads a session of its own, through this binary's
/// [`run_detach_trampoline`], so each binary that calls `detach` calls that
/// first. `cmd` keeps only its program, arguments, environment changes and
/// directory, so call `detach` before setting stdio. A program that `cmd`'s
/// `PATH` does not find is an error that names it.
pub fn detach(cmd: &mut std::process::Command) -> std::io::Result<()> {
    imp::detach(cmd)
}

/// Ends this process the way a child ended, so a parent reads the child's
/// status. On Unix, `process::exit` can only report a code, so a child killed
/// by a signal would look like `exit 143`. There, this process dies by the
/// same signal. On Windows, every status is an exit code, so this process
/// exits with that code.
pub fn exit_like(status: std::process::ExitStatus) -> ! {
    imp::exit_like(status)
}

/// The first `search_path` entry that holds `file` and that `eligible`
/// accepts. A relative entry, the empty one included, is a directory under
/// `cwd`, and with `cwd` unknown it holds nothing.
pub fn find_on_path(
    file: &Path,
    search_path: &std::ffi::OsStr,
    cwd: Option<&Path>,
    eligible: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .filter_map(|dir| match dir.is_absolute() {
            true => Some(dir),
            false => cwd.map(|cwd| cwd.join(dir)),
        })
        .map(|dir| dir.join(file))
        .find(|candidate| eligible(candidate))
}

/// Runs the program of a [`detach`]ed command, and never returns, when this
/// process is its trampoline. Returns at once otherwise. Call it first in
/// `main`.
#[cfg(unix)]
pub fn run_detach_trampoline() {
    imp::run_detach_trampoline()
}

/// Spawns `cmd` detached as [`detach`] does, holding no handle of this
/// process, its stdio included: a caller that reads this process's output to
/// its end is not held open by the child. On Windows the child's console has
/// no window.
pub fn spawn_background(cmd: &mut std::process::Command) -> std::io::Result<BackgroundChild> {
    imp::spawn_background(cmd).map(BackgroundChild)
}

/// A child [`spawn_background`] started.
pub struct BackgroundChild(imp::BackgroundChild);

impl BackgroundChild {
    /// The child's exit status, if it has exited.
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }

    /// Waits for the child to exit.
    pub fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait()
    }
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

/// True if something accepts a TCP connection on `127.0.0.1:port` within
/// `timeout`. A port nothing listens on answers at once on every platform,
/// where Windows would otherwise retransmit the refused SYN until `timeout`
/// runs out.
pub fn localhost_accepts(port: u16, timeout: Duration) -> bool {
    imp::localhost_accepts(port, timeout)
}

/// The controlling terminal's name, when stdin is attached to one.
pub fn controlling_tty() -> Option<String> {
    imp::controlling_tty()
}

/// Create a symlink at `link` pointing at `target`, where `target` is written
/// verbatim rather than resolved. Windows needs to know at creation time
/// whether the target is a directory and has no way to infer it from the path,
/// so `target_is_dir` is supplied by the caller; Unix ignores it.
///
/// Windows refuses this without Developer Mode or administrator rights. The
/// error is returned rather than handled, so a caller can degrade.
pub fn symlink(target: &Path, link: &Path, target_is_dir: bool) -> std::io::Result<()> {
    imp::symlink(target, link, target_is_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_on_path_takes_the_first_eligible_entry() {
        let root = tempfile::tempdir().unwrap();
        for dir in ["skipped", "found"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
            std::fs::write(root.path().join(dir).join("tool"), "").unwrap();
        }
        let search = std::env::join_paths([root.path().join("skipped"), "found".into()]).unwrap();
        let eligible = |path: &Path| !path.starts_with(root.path().join("skipped"));
        assert_eq!(
            find_on_path(Path::new("tool"), &search, Some(root.path()), eligible),
            Some(root.path().join("found/tool"))
        );
        assert_eq!(
            find_on_path(Path::new("tool"), &search, None, eligible),
            None
        );
    }

    #[test]
    fn self_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_not_alive() {
        assert!(!process_alive(0));
    }

    /// Callers ask this to decide whether a port row is live and whether a
    /// tree is safe to delete, so a process the caller may not signal has to
    /// read as alive. Pid 1 is the reachable case: `kill(1, None)` from an
    /// unprivileged process is `EPERM`, not `ESRCH`. Running as root takes the
    /// `Ok` arm instead and the answer is the same.
    #[cfg(unix)]
    #[test]
    fn a_process_this_user_cannot_signal_is_alive() {
        assert!(process_alive(1));
    }

    #[test]
    fn tree_rss_of_self_is_nonzero() {
        assert!(tree_rss_bytes(std::process::id()) > 0);
    }

    #[test]
    fn parent_pid_is_present() {
        assert!(parent_pid().is_some());
    }

    /// Windows refuses symlink creation without Developer Mode or admin, and
    /// CI runners may not have either. A refusal is not a failure of the code
    /// under test, so the test reports the skip and stops.
    fn can_symlink(dir: &Path) -> bool {
        let probe = dir.join("probe_link");
        match symlink(Path::new("probe_target"), &probe, false) {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(e) => {
                eprintln!("skipping: this platform refuses symlink creation ({e})");
                false
            }
        }
    }

    #[test]
    fn symlink_to_a_file_is_a_link_holding_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        if !can_symlink(tmp.path()) {
            return;
        }
        std::fs::write(tmp.path().join("real.txt"), "content").unwrap();
        let link = tmp.path().join("link.txt");

        symlink(Path::new("real.txt"), &link, false).unwrap();

        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "destination is a link");
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("real.txt"));
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "content");
    }

    #[test]
    fn symlink_to_a_directory_resolves_through_to_its_contents() {
        let tmp = tempfile::tempdir().unwrap();
        if !can_symlink(tmp.path()) {
            return;
        }
        std::fs::create_dir(tmp.path().join("real_dir")).unwrap();
        std::fs::write(tmp.path().join("real_dir/inner.txt"), "inner").unwrap();
        let link = tmp.path().join("link_dir");

        symlink(Path::new("real_dir"), &link, true).unwrap();

        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "destination is a link");
        assert_eq!(
            std::fs::read_to_string(link.join("inner.txt")).unwrap(),
            "inner"
        );
    }

    /// A source link whose target is gone is reproduced as a link whose target
    /// is gone, not reported as an error.
    #[test]
    fn symlink_to_a_missing_target_still_creates_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        if !can_symlink(tmp.path()) {
            return;
        }
        let link = tmp.path().join("dangling");

        symlink(Path::new("nothing_here"), &link, false).unwrap();

        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink());
        assert!(!link.exists(), "target does not resolve");
    }
}
