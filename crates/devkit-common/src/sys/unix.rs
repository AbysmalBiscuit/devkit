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
