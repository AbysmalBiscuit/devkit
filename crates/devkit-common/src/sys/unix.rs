//! Unix implementations of the primitives declared in `super`.

#[cfg(target_os = "linux")]
use std::fs;
use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    time::Duration,
};

pub(super) fn localhost_accepts(port: u16, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), timeout).is_ok()
}

pub(super) fn process_alive(pid: u32) -> bool {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    // Pids that do not fit in a positive i32 are invalid on Linux/macOS.
    let Ok(signed) = i32::try_from(pid) else {
        return false;
    };
    if signed <= 0 {
        return false;
    }
    match kill(Pid::from_raw(signed), None) {
        Ok(()) => true,
        // The process exists; it belongs to another user. Callers ask this to
        // decide whether a port row is live and whether a tree is safe to
        // delete, so a process this one may not signal is still a process.
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

pub(super) fn terminate(pid: u32) {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

/// The first argument that makes a devkit binary run [`run_detach_trampoline`].
const DETACH_EXEC: &str = "__detach-exec";

/// The trampoline argument before the cgroup leaf the program joins.
const CGROUP_ARG: &str = "--cgroup";

/// std has no stable safe `setsid` for a child, so the child is this binary
/// run as [`run_detach_trampoline`]:
/// `__detach-exec [--cgroup <leaf>] -- <program> <args>`. It calls `setsid` on
/// itself, joins the leaf, and execs the program in place, keeping its pid.
/// Its stdin is the write end of a status pipe. The pipe closes empty when
/// the program starts, and carries the error when it cannot start.
pub(super) fn spawn_detached(
    cmd: &mut std::process::Command,
    cgroup_leaf: Option<&std::path::Path>,
    stdout: std::process::Stdio,
    stderr: std::process::Stdio,
) -> std::io::Result<std::process::Child> {
    use std::io::Read as _;
    let mut trampoline = std::process::Command::new(trampoline_exe()?);
    trampoline.arg(DETACH_EXEC);
    if let Some(leaf) = cgroup_leaf {
        trampoline.arg(CGROUP_ARG).arg(leaf);
    }
    trampoline
        .arg("--")
        .arg(cmd.get_program())
        .args(cmd.get_args());
    if let Some(dir) = cmd.get_current_dir() {
        trampoline.current_dir(dir);
    }
    for (key, value) in cmd.get_envs() {
        match value {
            Some(value) => trampoline.env(key, value),
            None => trampoline.env_remove(key),
        };
    }
    let (mut status, status_writer) = std::io::pipe()?;
    trampoline
        .stdin(status_writer)
        .stdout(stdout)
        .stderr(stderr);
    let mut child = trampoline.spawn()?;
    // The command holds this process's copy of the write end, and the read
    // below ends only when every copy is closed.
    drop(trampoline);
    let mut failure = String::new();
    status.read_to_string(&mut failure)?;
    if failure.is_empty() {
        return Ok(child);
    }
    let _ = child.wait();
    Err(std::io::Error::other(failure))
}

/// A long-lived daemon outlives an upgrade that replaces its binary. Linux
/// still execs the running image through `/proc/self/exe` then.
fn trampoline_exe() -> std::io::Result<std::path::PathBuf> {
    if cfg!(target_os = "linux") {
        Ok("/proc/self/exe".into())
    } else {
        std::env::current_exe()
    }
}

/// Runs the program a [`spawn_detached`] command names when this process was
/// started as its trampoline, and returns otherwise.
pub(super) fn run_detach_trampoline() {
    use std::{io::Write as _, os::fd::AsFd as _};
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(DETACH_EXEC)) {
        return;
    }
    // The clone is close-on-exec, so a successful exec closes the status pipe.
    // `exec` replaces stdin before it runs the program, so a failed exec has
    // only the clone left to report through.
    let status = std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from);
    let err = exec_detached(args);
    if let Ok(mut status) = status {
        let _ = status.write_all(err.to_string().as_bytes());
    }
    eprintln!("{DETACH_EXEC}: {err}");
    std::process::exit(127);
}

/// Reads `[--cgroup <leaf>] -- <program> <args>`, starts a session, joins the
/// leaf, and execs the program with stdin at the null device. Returns only
/// the error that stopped the program.
fn exec_detached(mut args: impl Iterator<Item = std::ffi::OsString>) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;
    let mut next = args.next();
    let mut leaf = None;
    if next.as_deref() == Some(std::ffi::OsStr::new(CGROUP_ARG)) {
        leaf = args.next();
        next = args.next();
    }
    let (Some(separator), Some(program)) = (next, args.next()) else {
        return usage();
    };
    if separator != "--" {
        return usage();
    }
    let err = match nix::unistd::setsid() {
        Ok(_) => {
            if let Some(leaf) = leaf {
                join_cgroup(std::path::Path::new(&leaf));
            }
            std::process::Command::new(&program)
                .args(args)
                .stdin(std::process::Stdio::null())
                .exec()
        }
        Err(errno) => errno.into(),
    };
    std::io::Error::new(
        err.kind(),
        format!("cannot run `{}`: {err}", program.to_string_lossy()),
    )
}

fn usage() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("usage: {DETACH_EXEC} [{CGROUP_ARG} <leaf>] -- <program> [args...]"),
    )
}

/// Moves this process into `leaf`. A leaf that cannot be joined leaves the
/// program uncapped. The pid goes in one write, because cgroupfs reads each
/// write as one request.
fn join_cgroup(leaf: &std::path::Path) {
    use std::io::Write as _;
    if let Ok(mut procs) = std::fs::OpenOptions::new()
        .write(true)
        .open(leaf.join("cgroup.procs"))
    {
        let _ = procs.write_all(format!("{}\n", std::process::id()).as_bytes());
    }
}

pub(super) fn exit_like(status: std::process::ExitStatus) -> ! {
    use std::os::unix::process::ExitStatusExt;
    let Some(signal) = status.signal() else {
        std::process::exit(status.code().unwrap_or(1));
    };
    // SAFETY: the number came from the child's status and installs no handler.
    // `nix::sys::signal::Signal` has no variant for a realtime signal.
    unsafe {
        nix::libc::signal(signal, nix::libc::SIG_DFL);
        nix::libc::raise(signal);
    }
    std::process::exit(128 + signal);
}

pub(super) type BackgroundChild = std::process::Child;

/// Unix closes every descriptor but the null stdio on exec, so a detached
/// child holds nothing of its parent's.
pub(super) fn spawn_background(
    cmd: &mut std::process::Command,
) -> std::io::Result<BackgroundChild> {
    use std::process::Stdio;
    spawn_detached(cmd, None, Stdio::null(), Stdio::null())
}

pub(super) fn reap_owned(pid: u32) -> bool {
    use nix::{
        sys::wait::{WaitPidFlag, WaitStatus, waitpid},
        unistd::Pid,
    };
    // A pid of 0 would make waitpid(0) reap any process-group member; never
    // probe it.
    if pid == 0 {
        return false;
    }
    match waitpid(Pid::from_raw(pid as i32), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => false,
        Ok(_) => true,  // exited/signaled -> reaped
        Err(_) => true, // ECHILD etc. -> treat as gone
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
    // Optional manual override (also used by integration tests): a
    // pre-delegated, writable cgroup-v2 base the daemon should use instead
    // of auto-detecting.
    if let Some(root) = std::env::var_os("DEVKIT_DAEMON_CGROUP_ROOT") {
        let base = std::path::PathBuf::from(root);
        return match prepare_base(&base) {
            Ok(()) => CgroupCaps::Enforce { base },
            Err(e) => CgroupCaps::Unavailable {
                reason: format!("{e:#}"),
            },
        };
    }
    // cgroup-v2 unified hierarchy is mounted at /sys/fs/cgroup with a
    // cgroup.controllers file at the root.
    let mount = std::path::Path::new("/sys/fs/cgroup");
    if !mount.join("cgroup.controllers").is_file() {
        return CgroupCaps::Unavailable {
            reason: "cgroup-v2 unified hierarchy not mounted".into(),
        };
    }
    // Resolve this process's own cgroup: /proc/self/cgroup line "0::<rel>".
    let rel = match fs::read_to_string("/proc/self/cgroup").ok().and_then(|s| {
        s.lines()
            .find_map(|l| l.strip_prefix("0::").map(str::to_string))
    }) {
        Some(r) => r,
        None => {
            return CgroupCaps::Unavailable {
                reason: "no cgroup-v2 entry in /proc/self/cgroup".into(),
            };
        }
    };
    let base = mount.join(rel.trim_start_matches('/'));
    match prepare_base(&base) {
        Ok(()) => CgroupCaps::Enforce { base },
        Err(e) => CgroupCaps::Unavailable {
            reason: format!("{e:#}"),
        },
    }
}

/// Make `base` able to host memory-capped leaves: enable `+memory` in
/// `cgroup.subtree_control`, and — to satisfy cgroup-v2's no-internal-processes
/// rule — move this process into `<base>/supervisor/` so server leaves can sit
/// beside it under `<base>/servers/`. Idempotent.
#[cfg(target_os = "linux")]
fn prepare_base(base: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    use anyhow::Context as _;
    // Writability probe: the base dir must be writable by this user
    // (delegation).
    let meta =
        fs::metadata(base).with_context(|| format!("cgroup base {} missing", base.display()))?;
    if meta.permissions().mode() & 0o200 == 0 {
        anyhow::bail!(
            "cgroup base {} not writable by this process",
            base.display()
        );
    }
    let sup = base.join("supervisor");
    fs::create_dir_all(&sup).with_context(|| format!("creating {}", sup.display()))?;
    // Move self out of `base` before enabling controllers on it.
    fs::write(
        sup.join("cgroup.procs"),
        format!("{}\n", std::process::id()),
    )
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
    // Delegate the memory controller one level further: leaves live at
    // `<base>/servers/<name>`, so `servers` must enable +memory in its own
    // subtree_control or the grandchild leaves get no memory.max to cap.
    let _ = fs::write(
        base.join("servers").join("cgroup.subtree_control"),
        "+memory\n",
    );
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
    // Kill the whole leaf together on breach so the daemon sees a clean tree
    // exit.
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

pub(super) fn symlink(
    target: &std::path::Path,
    link: &std::path::Path,
    _target_is_dir: bool,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
