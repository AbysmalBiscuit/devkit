use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Spawn `argv` detached (own session), env-augmented, stdout+stderr → logfile.
/// Returns the child pid.
pub fn spawn_detached(
    argv: &[String], cwd: &str, env: &BTreeMap<String, String>, logfile: &PathBuf,
) -> Result<u32> {
    fs::create_dir_all(logfile.parent().unwrap())?;
    let out = File::create(logfile)?;
    let err = out.try_clone()?;
    let (prog, rest) = argv.split_first().context("empty launch argv")?;
    let mut c = Command::new(prog);
    c.args(rest).current_dir(cwd).envs(env)
        .stdin(Stdio::null()).stdout(out).stderr(err);
    crate::sys::detach(&mut c);
    let child = c.spawn().with_context(|| format!("spawning {prog}"))?;
    Ok(child.id())
}

/// Poll localhost:port until it accepts a TCP connection or times out.
pub fn wait_ready(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect_timeout(&(std::net::Ipv4Addr::LOCALHOST, port).into(), Duration::from_millis(300)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}

/// SIGTERM a pid (ignore if already gone).
pub fn stop(pid: u32) {
    crate::sys::terminate(pid);
}

pub fn tail(logfile: &PathBuf, lines: usize) -> String {
    let body = fs::read_to_string(logfile).unwrap_or_default();
    body.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Resident set size, in bytes, summed over the process subtree rooted at `root`
/// (the process plus every descendant). Returns 0 if the root is gone. Linux-only;
/// reads `/proc` and needs no privilege.
pub fn tree_rss_bytes(root: u32) -> u64 {
    // pid -> ppid for every visible process.
    let mut parent: HashMap<u32, u32> = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else { return 0 };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if let Some(ppid) = read_ppid(pid) {
            parent.insert(pid, ppid);
        }
    }
    // Walk the subtree rooted at `root` (order is irrelevant; every descendant is visited).
    let mut total = 0u64;
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    // 4 KiB pages on x86-64 Linux; arm64 kernels may differ but not on this fleet.
    let page = 4096u64;
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) { continue; }
        total += resident_pages(pid).saturating_mul(page);
        for (&child, &pp) in &parent {
            if pp == pid { stack.push(child); }
        }
    }
    total
}

/// Parent pid from `/proc/<pid>/stat` (field 4, after the possibly-paren'd comm).
fn read_ppid(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is in parens and may contain spaces/parens; split after the last ')'.
    let rest = stat.rsplit_once(')')?.1;
    let mut it = rest.split_whitespace();
    let _state = it.next()?;          // field 3
    it.next()?.parse::<u32>().ok()    // field 4 = ppid
}

/// Resident pages from `/proc/<pid>/statm` (field 2). 0 if unreadable.
fn resident_pages(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|n| n.parse::<u64>().ok()))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spawn_and_ready_on_python_http() {
        let tmp = std::env::temp_dir().join(format!("devrun-{}.log", std::process::id()));
        // pick a free port
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let argv: Vec<String> = ["python3","-m","http.server",&port.to_string()].iter().map(|s| s.to_string()).collect();
        let env = BTreeMap::new();
        let pid = spawn_detached(&argv, ".", &env, &tmp).unwrap();
        assert!(wait_ready(port, Duration::from_secs(10)), "server never came up");
        stop(pid);
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn tree_rss_counts_self() {
        // Our own process has non-zero resident memory.
        let rss = tree_rss_bytes(std::process::id());
        assert!(rss > 0, "expected non-zero RSS for current process");
    }
}
