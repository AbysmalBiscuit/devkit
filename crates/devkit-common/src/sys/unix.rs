//! Unix implementations of the primitives declared in `super`.

use std::collections::{HashMap, HashSet};

#[cfg(target_os = "linux")]
use std::fs;

pub(super) fn process_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    // Pids that do not fit in a positive i32 are invalid on Linux/macOS.
    let Ok(signed) = i32::try_from(pid) else {
        return false;
    };
    if signed <= 0 {
        return false;
    }
    kill(Pid::from_raw(signed), None).is_ok()
}

pub(super) fn terminate(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

pub(super) fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // Start a new session so the child outlives the launching shell and is
    // insulated from its controlling terminal's signals.
    // SAFETY: setsid only mutates the child after fork; it is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(|e| e.into()));
    }
}

pub(super) fn reap_owned(pid: u32) -> bool {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;
    // A pid of 0 would make waitpid(0) reap any process-group member; never probe it.
    if pid == 0 {
        return false;
    }
    match waitpid(Pid::from_raw(pid as i32), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => false,
        Ok(_) => true,  // exited/signaled → reaped
        Err(_) => true, // ECHILD etc. → treat as gone
    }
}

pub(super) fn tree_rss_bytes(root: u32) -> u64 {
    let table = process_table();
    let mut total = 0u64;
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(&(_, rss)) = table.get(&pid) {
            total = total.saturating_add(rss);
        }
        for (&child, &(ppid, _)) in &table {
            if ppid == pid {
                stack.push(child);
            }
        }
    }
    total
}

/// Every process mapped to its `(parent pid, resident set size in bytes)`.
/// Linux reads `/proc`; the resident set comes from `statm` (pages × 4 KiB).
#[cfg(target_os = "linux")]
fn process_table() -> HashMap<u32, (u32, u64)> {
    let page = 4096u64;
    let mut table = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return table;
    };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if let Some(ppid) = read_ppid(pid) {
            table.insert(pid, (ppid, resident_pages(pid).saturating_mul(page)));
        }
    }
    table
}

/// BSD-derived systems (macOS) have no `/proc`; `ps` is the portable way to
/// enumerate every process with its parent and resident set. `rss` is reported
/// in kibibytes.
#[cfg(not(target_os = "linux"))]
fn process_table() -> HashMap<u32, (u32, u64)> {
    let mut table = HashMap::new();
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,rss="])
        .output()
    else {
        return table;
    };
    if !out.status.success() {
        return table;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut cols = line.split_whitespace();
        let (Some(pid), Some(ppid), Some(rss)) = (cols.next(), cols.next(), cols.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid), Ok(rss_kib)) =
            (pid.parse::<u32>(), ppid.parse::<u32>(), rss.parse::<u64>())
        else {
            continue;
        };
        table.insert(pid, (ppid, rss_kib.saturating_mul(1024)));
    }
    table
}

pub(super) fn parent_pid() -> Option<u32> {
    Some(nix::unistd::getppid().as_raw() as u32)
}

pub(super) fn controlling_tty() -> Option<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return None;
    }
    nix::unistd::ttyname(std::io::stdin())
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(target_os = "linux")]
pub(super) fn cgroup_caps() -> super::CgroupCaps {
    use super::CgroupCaps;
    // Optional manual override (also used by integration tests): a pre-delegated,
    // writable cgroup-v2 base the daemon should use instead of auto-detecting.
    if let Some(root) = std::env::var_os("DEVKIT_DAEMON_CGROUP_ROOT") {
        let base = std::path::PathBuf::from(root);
        return match prepare_base(&base) {
            Ok(()) => CgroupCaps::Enforce { base },
            Err(e) => CgroupCaps::Unavailable { reason: format!("{e:#}") },
        };
    }
    // cgroup-v2 unified hierarchy is mounted at /sys/fs/cgroup with a
    // cgroup.controllers file at the root.
    let mount = std::path::Path::new("/sys/fs/cgroup");
    if !mount.join("cgroup.controllers").is_file() {
        return CgroupCaps::Unavailable { reason: "cgroup-v2 unified hierarchy not mounted".into() };
    }
    // Resolve this process's own cgroup: /proc/self/cgroup line "0::<rel>".
    let rel = match fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("0::").map(str::to_string)))
    {
        Some(r) => r,
        None => return CgroupCaps::Unavailable { reason: "no cgroup-v2 entry in /proc/self/cgroup".into() },
    };
    let base = mount.join(rel.trim_start_matches('/'));
    match prepare_base(&base) {
        Ok(()) => CgroupCaps::Enforce { base },
        Err(e) => CgroupCaps::Unavailable { reason: format!("{e:#}") },
    }
}

/// Make `base` able to host memory-capped leaves: enable `+memory` in
/// `cgroup.subtree_control`, and — to satisfy cgroup-v2's no-internal-processes
/// rule — move this process into `<base>/supervisor/` so server leaves can sit
/// beside it under `<base>/servers/`. Idempotent.
#[cfg(target_os = "linux")]
fn prepare_base(base: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::os::unix::fs::PermissionsExt as _;
    // Writability probe: the base dir must be writable by this user (delegation).
    let meta = fs::metadata(base)
        .with_context(|| format!("cgroup base {} missing", base.display()))?;
    if meta.permissions().mode() & 0o200 == 0 {
        anyhow::bail!("cgroup base {} not writable by this process", base.display());
    }
    let sup = base.join("supervisor");
    fs::create_dir_all(&sup).with_context(|| format!("creating {}", sup.display()))?;
    // Move self out of `base` before enabling controllers on it.
    fs::write(sup.join("cgroup.procs"), format!("{}\n", std::process::id()))
        .with_context(|| "moving daemon into supervisor leaf")?;
    fs::create_dir_all(base.join("servers")).with_context(|| "creating servers subtree")?;
    // Enable the memory controller for children. Ignore "already enabled".
    let _ = fs::write(base.join("cgroup.subtree_control"), "+memory\n");
    if !fs::read_to_string(base.join("cgroup.controllers"))
        .unwrap_or_default()
        .split_whitespace()
        .any(|c| c == "memory")
    {
        anyhow::bail!("memory controller unavailable in {}", base.display());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn cgroup_create_leaf(
    base: &std::path::Path,
    name: &str,
    max_bytes: u64,
) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context as _;
    let leaf = base.join("servers").join(name);
    fs::create_dir_all(&leaf).with_context(|| format!("mkdir {}", leaf.display()))?;
    fs::write(leaf.join("memory.max"), format!("{max_bytes}\n"))
        .with_context(|| format!("set memory.max on {}", leaf.display()))?;
    // Kill the whole leaf together on breach so the daemon sees a clean tree exit.
    let _ = fs::write(leaf.join("memory.oom.group"), "1\n");
    Ok(leaf)
}

#[cfg(target_os = "linux")]
pub(super) fn cgroup_remove_leaf(leaf: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    fs::remove_dir(leaf).with_context(|| format!("rmdir {}", leaf.display()))
}

#[cfg(target_os = "linux")]
pub(super) fn cgroup_list_leaves(base: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(rd) = fs::read_dir(base.join("servers")) {
        for ent in rd.flatten() {
            if ent.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && let Some(n) = ent.file_name().to_str()
            {
                names.push(n.to_string());
            }
        }
    }
    names
}

#[cfg(target_os = "linux")]
pub(super) fn join_cgroup(cmd: &mut std::process::Command, leaf: &std::path::Path) {
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    // Open the leaf's cgroup.procs in the parent (write, close-on-exec). A failure
    // here leaves the child uncapped — fail-open, never block the spawn.
    let path = leaf.join("cgroup.procs");
    let Ok(file) = fs::OpenOptions::new().write(true).open(&path) else {
        return;
    };
    let fd: OwnedFd = file.into();
    // SAFETY: the closure runs in the forked child before `exec` and calls only
    // async-signal-safe primitives — getpid(), arithmetic via fmt_pid, and a
    // single write() to a pre-opened fd. Writing the pid to cgroup.procs moves the
    // child (and every descendant it later forks) into the leaf. The write error
    // is ignored: an unplaced child runs uncapped rather than failing the spawn.
    unsafe {
        cmd.pre_exec(move || {
            let mut buf = [0u8; 20];
            let s = super::fmt_pid(nix::libc::getpid() as i64, &mut buf);
            let _ = nix::libc::write(
                fd.as_raw_fd(),
                s.as_ptr() as *const nix::libc::c_void,
                s.len(),
            );
            Ok(())
        });
    }
}

// macOS: no cgroups.
#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_caps() -> super::CgroupCaps {
    super::CgroupCaps::Unsupported
}
#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_create_leaf(
    _base: &std::path::Path,
    _name: &str,
    _max_bytes: u64,
) -> anyhow::Result<std::path::PathBuf> {
    anyhow::bail!("cgroups unsupported on this platform")
}
#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_remove_leaf(_leaf: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}
#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_list_leaves(_base: &std::path::Path) -> Vec<String> {
    Vec::new()
}
#[cfg(not(target_os = "linux"))]
pub(super) fn join_cgroup(_cmd: &mut std::process::Command, _leaf: &std::path::Path) {}

#[cfg(target_os = "linux")]
fn read_ppid(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let mut it = rest.split_whitespace();
    let _state = it.next()?;
    it.next()?.parse::<u32>().ok()
}

#[cfg(target_os = "linux")]
fn resident_pages(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|n| n.parse::<u64>().ok())
        })
        .unwrap_or(0)
}
