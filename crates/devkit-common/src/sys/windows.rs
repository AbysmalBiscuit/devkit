//! Windows implementations of the primitives declared in `super`, via the
//! Win32 API (`windows-sys`).

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::{OsStr, OsString},
    io,
    mem::size_of,
    net::{Ipv4Addr, SocketAddr},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, AsRawSocket, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    process::{Command, ExitStatus},
    time::Duration,
};

use socket2::{Domain, Socket, Type};
use windows_sys::Win32::{
    Foundation::{CloseHandle, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Networking::WinSock::{SIO_TCP_INITIAL_RTO, SOCKET, TCP_INITIAL_RTO_PARAMETERS, WSAIoctl},
    System::{
        Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent},
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::{
            CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetCurrentProcessId, GetExitCodeProcess,
            INFINITE, OpenProcess, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_TERMINATE, PROCESS_VM_READ, STARTUPINFOW, TerminateProcess,
            WaitForSingleObject,
        },
    },
};

/// `GetExitCodeProcess` reports this code while a process is still running.
const STILL_ACTIVE: u32 = 259;

/// New process group: insulates the child from the parent console's Ctrl-C and
/// makes it independently addressable by `GenerateConsoleCtrlEvent`.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// `TCP_INITIAL_RTO_PARAMETERS` values as `mstcpip.h` defines them:
/// `(USHORT)-1` keeps the default RTT and `(UCHAR)-2` stops SYN
/// retransmission. windows-sys types both constants as `u16`.
const UNSPECIFIED_RTT: u16 = u16::MAX;
const NO_SYN_RETRANSMISSIONS: u8 = 0xfe;

pub(super) fn localhost_accepts(port: u16, timeout: Duration) -> bool {
    let Ok(socket) = Socket::new(Domain::IPV4, Type::STREAM, None) else {
        return false;
    };
    let params = TCP_INITIAL_RTO_PARAMETERS {
        Rtt: UNSPECIFIED_RTT,
        MaxSynRetransmissions: NO_SYN_RETRANSMISSIONS,
    };
    let mut returned = 0u32;
    // SAFETY: a synchronous call on an open socket with a live input buffer of
    // the length passed. A failure keeps the defaults: slower, never wrong.
    unsafe {
        WSAIoctl(
            socket.as_raw_socket() as SOCKET,
            SIO_TCP_INITIAL_RTO,
            (&raw const params).cast(),
            size_of::<TCP_INITIAL_RTO_PARAMETERS>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            None,
        );
    }
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    socket.connect_timeout(&addr.into(), timeout).is_ok()
}

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
    // SAFETY: each opened handle is null-checked and closed; the
    // console-control call takes no handle.
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
    keep_stdio_from_children();
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// A console with no window. A child with no console at all would give each
/// console program it runs, git among them, a window of its own.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A child [`spawn_background`] started.
pub(super) struct BackgroundChild(OwnedHandle);

impl BackgroundChild {
    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.wait_for(0)
    }

    pub(super) fn wait(&mut self) -> io::Result<ExitStatus> {
        self.wait_for(INFINITE)
            .map(|status| status.expect("an infinite wait ends with the exit"))
    }

    fn wait_for(&self, millis: u32) -> io::Result<Option<ExitStatus>> {
        let handle = self.0.as_raw_handle();
        // SAFETY: `handle` is this child's process handle, open while `self`
        // lives; both calls only read it.
        unsafe {
            match WaitForSingleObject(handle, millis) {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    if GetExitCodeProcess(handle, &mut code) == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(Some(ExitStatus::from_raw(code)))
                }
                WAIT_TIMEOUT => Ok(None),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }
}

/// Starts `cmd` with no handle of this process. std's spawn passes the child
/// every inheritable handle, those this process's own parent passed down
/// included, and a process cannot list those to withhold them one by one.
/// The child's stdio is its own console, which has no window.
pub(super) fn spawn_background(cmd: &Command) -> io::Result<BackgroundChild> {
    let mut command_line = command_line(cmd);
    let environment = environment_block(cmd);
    let directory = cmd.get_current_dir().map(|dir| wide(dir.as_os_str()));
    // SAFETY: zeroed is the documented initial state of both structs.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = size_of::<STARTUPINFOW>() as u32;
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer is either null or into a buffer that outlives the
    // call, and the strings are NUL-terminated UTF-16.
    let created = unsafe {
        CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            environment
                .as_ref()
                .map_or(std::ptr::null(), |block| block.as_ptr().cast()),
            directory
                .as_ref()
                .map_or(std::ptr::null(), |dir| dir.as_ptr()),
            &startup,
            &mut info,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateProcessW returned both handles open and owned by the
    // caller.
    unsafe {
        CloseHandle(info.hThread);
        Ok(BackgroundChild(OwnedHandle::from_raw_handle(info.hProcess)))
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain([0]).collect()
}

/// The program and arguments as one command line, each quoted the way the
/// Microsoft C runtime splits them back apart.
fn command_line(cmd: &Command) -> Vec<u16> {
    let mut line = Vec::new();
    append_arg(&mut line, cmd.get_program(), true);
    for arg in cmd.get_args() {
        line.push(u16::from(b' '));
        append_arg(&mut line, arg, false);
    }
    line.push(0);
    line
}

fn append_arg(line: &mut Vec<u16>, arg: &OsStr, force_quotes: bool) {
    let [quote, backslash] = [u16::from(b'"'), u16::from(b'\\')];
    let quoted = force_quotes
        || arg.is_empty()
        || arg
            .encode_wide()
            .any(|c| c == u16::from(b' ') || c == u16::from(b'\t'));
    if quoted {
        line.push(quote);
    }
    let mut backslashes = 0;
    for c in arg.encode_wide() {
        if c == backslash {
            backslashes += 1;
        } else {
            if c == quote {
                line.extend(std::iter::repeat_n(backslash, backslashes + 1));
            }
            backslashes = 0;
        }
        line.push(c);
    }
    if quoted {
        line.extend(std::iter::repeat_n(backslash, backslashes));
        line.push(quote);
    }
}

/// This process's environment with `cmd`'s changes applied, as the block
/// `CreateProcessW` takes, or `None` when `cmd` changes nothing.
fn environment_block(cmd: &Command) -> Option<Vec<u16>> {
    cmd.get_envs().next()?;
    let mut vars: BTreeMap<OsString, (OsString, OsString)> = std::env::vars_os()
        .map(|(key, value)| (key.to_ascii_uppercase(), (key, value)))
        .collect();
    for (key, value) in cmd.get_envs() {
        match value {
            Some(value) => {
                vars.insert(key.to_ascii_uppercase(), (key.to_owned(), value.to_owned()))
            }
            None => vars.remove(&key.to_ascii_uppercase()),
        };
    }
    let mut block = Vec::new();
    for (key, value) in vars.values() {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    Some(block)
}

/// Windows spawns with `bInheritHandles`, so every inheritable handle in this
/// process reaches the child, this process's stdio among them: a parent's
/// reader would then wait for a detached child to exit before it sees end of
/// file. std duplicates a handle it passes as inherited stdio, so clearing
/// the flag on the originals changes nothing for other spawns.
fn keep_stdio_from_children() {
    use windows_sys::Win32::{
        Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation},
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle returns this process's handle or null/invalid,
        // and SetHandleInformation only changes a flag on a handle we own.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
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

pub(super) fn symlink(
    target: &std::path::Path,
    link: &std::path::Path,
    target_is_dir: bool,
) -> std::io::Result<()> {
    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}
