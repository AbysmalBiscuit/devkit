//! Windows implementations of the primitives declared in `super`, via the
//! Win32 API (`windows-sys`).

use std::collections::{HashMap, HashSet};
use std::mem::size_of;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, PROCESS_VM_READ, TerminateProcess,
};

/// `GetExitCodeProcess` reports this code while a process is still running.
const STILL_ACTIVE: u32 = 259;

/// New process group: insulates the child from the parent console's Ctrl-C and
/// makes it independently addressable by `GenerateConsoleCtrlEvent`.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

pub(super) fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess returns null on failure; the handle is null-checked
    // before use and closed exactly once.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE
    }
}

pub(super) fn terminate(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: each opened handle is null-checked and closed; the console-control
    // call takes no handle.
    unsafe {
        // CTRL_BREAK reaches a child started in its own process group (see
        // `detach`) and gives it a chance to shut down cleanly.
        if GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 {
            return;
        }
        // No shared console or not a group leader: terminate directly.
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

pub(super) fn detach(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

pub(super) fn reap_owned(pid: u32) -> bool {
    // Windows has no zombies: an owned child is "reaped" once it is no longer
    // alive, so liveness is the whole story.
    !process_alive(pid)
}

pub(super) fn tree_rss_bytes(root: u32) -> u64 {
    let Some(parent) = snapshot_parents() else {
        return 0;
    };
    let mut total = 0u64;
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        total = total.saturating_add(working_set_bytes(pid));
        for (&child, &pp) in &parent {
            if pp == pid {
                stack.push(child);
            }
        }
    }
    total
}

pub(super) fn parent_pid() -> Option<u32> {
    // SAFETY: GetCurrentProcessId takes no arguments and cannot fail.
    let me = unsafe { GetCurrentProcessId() };
    snapshot_parents()?.get(&me).copied()
}

pub(super) fn controlling_tty() -> Option<String> {
    // Windows consoles have no controlling-tty device name to report.
    None
}

/// Map every running process id to its parent's id via a Toolhelp snapshot.
fn snapshot_parents() -> Option<HashMap<u32, u32>> {
    // SAFETY: the snapshot handle is checked against INVALID_HANDLE_VALUE and
    // closed; `entry` is zeroed with `dwSize` set before the first call.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut parent = HashMap::new();
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                parent.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        Some(parent)
    }
}

/// Resident working-set size (bytes) of a single process, or 0 if it cannot be
/// opened or queried.
fn working_set_bytes(pid: u32) -> u64 {
    if pid == 0 {
        return 0;
    }
    // SAFETY: the handle is null-checked and closed; the counters struct is
    // zeroed and its size is passed to GetProcessMemoryInfo.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return 0;
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        let cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = GetProcessMemoryInfo(handle, &mut counters, cb) != 0;
        CloseHandle(handle);
        if ok {
            counters.WorkingSetSize as u64
        } else {
            0
        }
    }
}

pub(super) fn cgroup_caps() -> super::CgroupCaps {
    super::CgroupCaps::Unsupported
}
pub(super) fn cgroup_create_leaf(
    _base: &std::path::Path,
    _name: &str,
    _max_bytes: u64,
) -> anyhow::Result<std::path::PathBuf> {
    anyhow::bail!("cgroups unsupported on this platform")
}
pub(super) fn cgroup_remove_leaf(_leaf: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}
pub(super) fn cgroup_list_leaves(_base: &std::path::Path) -> Vec<String> {
    Vec::new()
}
pub(super) fn join_cgroup(_cmd: &mut std::process::Command, _leaf: &std::path::Path) {}
