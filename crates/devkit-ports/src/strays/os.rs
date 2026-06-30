//! Real OS implementations of the strays seams. `/proc` is Linux-only; on other
//! targets the process table is empty and detection degrades to the port band.

use super::{PortProbe, Proc, ProcTable};

pub struct RealPortProbe;
impl PortProbe for RealPortProbe {
    fn listening(&self, port: u16) -> bool {
        crate::registry::listening(port)
    }
}

pub struct RealProcTable;
impl ProcTable for RealProcTable {
    #[cfg(target_os = "linux")]
    fn snapshot(&self) -> Vec<Proc> {
        let mut out = Vec::new();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return out;
        };
        for ent in dir.flatten() {
            let name = ent.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let base = ent.path();
            let argv = std::fs::read(base.join("cmdline"))
                .ok()
                .map(|b| nul_to_space(&b))
                .unwrap_or_default();
            if argv.is_empty() {
                continue;
            }
            let ppid = read_ppid(&base.join("stat")).unwrap_or(0);
            let cwd = std::fs::read_link(base.join("cwd"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            out.push(Proc {
                pid,
                ppid,
                argv,
                cwd,
            });
        }
        out
    }

    #[cfg(not(target_os = "linux"))]
    fn snapshot(&self) -> Vec<Proc> {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn nul_to_space(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.trim_end_matches('\0').replace('\0', " ")
}

#[cfg(target_os = "linux")]
fn read_ppid(stat: &std::path::Path) -> Option<u32> {
    // `/proc/<pid>/stat`: `pid (comm) state ppid ...`; comm may contain spaces
    // and parens, so split after the LAST ')'.
    let s = std::fs::read_to_string(stat).ok()?;
    let rest = &s[s.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// SIGTERM every pid in each root's subtree, then SIGKILL survivors after a grace.
/// Unix-only; a no-op returning 0 elsewhere.
#[cfg(unix)]
pub fn kill_tree(roots: &[u32], procs: &[Proc]) -> usize {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use std::collections::BTreeMap;

    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut targets: Vec<u32> = Vec::new();
    let mut stack = roots.to_vec();
    while let Some(pid) = stack.pop() {
        if !targets.contains(&pid) {
            targets.push(pid);
            if let Some(cs) = children.get(&pid) {
                stack.extend(cs.iter().copied());
            }
        }
    }
    let alive = |pid: u32| kill(Pid::from_raw(pid as i32), None).is_ok();
    for pid in &targets {
        let _ = kill(Pid::from_raw(*pid as i32), Signal::SIGTERM);
    }
    // Poll up to ~3s, then SIGKILL anything still alive.
    for _ in 0..30 {
        if targets.iter().all(|p| !alive(*p)) {
            return targets.len();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for pid in &targets {
        let _ = kill(Pid::from_raw(*pid as i32), Signal::SIGKILL);
    }
    targets.len()
}

#[cfg(not(unix))]
pub fn kill_tree(_roots: &[u32], _procs: &[Proc]) -> usize {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_port_probe_reports_a_bound_listener() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(RealPortProbe.listening(port));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_snapshot_includes_self() {
        let me = std::process::id();
        assert!(RealProcTable.snapshot().iter().any(|p| p.pid == me));
    }
}
