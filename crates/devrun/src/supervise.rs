use anyhow::{Context, Result};
use std::collections::BTreeMap;
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
    use std::os::unix::process::CommandExt;
    fs::create_dir_all(logfile.parent().unwrap())?;
    let out = File::create(logfile)?;
    let err = out.try_clone()?;
    let (prog, rest) = argv.split_first().context("empty launch argv")?;
    let mut c = Command::new(prog);
    c.args(rest).current_dir(cwd).envs(env)
        .stdin(Stdio::null()).stdout(out).stderr(err);
    unsafe { c.pre_exec(|| { nix::unistd::setsid().map(|_| ()).map_err(|e| e.into()) }); }
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
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

pub fn tail(logfile: &PathBuf, lines: usize) -> String {
    let body = fs::read_to_string(logfile).unwrap_or_default();
    body.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
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
}
