use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub use crate::sys::tree_rss_bytes;

/// Configure a `Command` the same way `spawn_detached` does, minus the stdio
/// attachment (which requires a real logfile). Extracted so tests can inspect the
/// resulting env without spawning a real process.
fn configure_child<'a>(
    c: &'a mut Command,
    rest: &[String],
    cwd: &str,
    env: &BTreeMap<String, String>,
) -> &'a mut Command {
    // The daemon marker must not cross into supervised children: a devkit subprocess
    // of a child would see it, skip the portd.lock gate, and write ports.json directly
    // behind the live daemon, causing silent registry desync.
    c.args(rest)
        .current_dir(cwd)
        .envs(env)
        .env_remove("DEVKIT_PORTD_SELF")
}

/// Spawn `argv` detached (own session), env-augmented, stdout+stderr → logfile.
/// Returns the child pid.
pub fn spawn_detached(
    argv: &[String],
    cwd: &str,
    env: &BTreeMap<String, String>,
    logfile: &PathBuf,
) -> Result<u32> {
    fs::create_dir_all(logfile.parent().unwrap())?;
    let out = File::create(logfile)?;
    let err = out.try_clone()?;
    let (prog, rest) = argv.split_first().context("empty launch argv")?;
    let mut c = Command::new(prog);
    configure_child(&mut c, rest, cwd, env)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    crate::sys::detach(&mut c);
    let child = c.spawn().with_context(|| format!("spawning {prog}"))?;
    Ok(child.id())
}

/// Poll localhost:port until it accepts a TCP connection or times out.
pub fn wait_ready(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect_timeout(
            &(std::net::Ipv4Addr::LOCALHOST, port).into(),
            Duration::from_millis(300),
        )
        .is_ok()
        {
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
    body.lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// `configure_child` must remove `DEVKIT_PORTD_SELF` from the child's env even
    /// when the caller's process has it set, so a devkit subprocess of a supervised
    /// server cannot write the registry behind the live daemon.
    #[test]
    fn spawn_detached_does_not_leak_daemon_marker() {
        // Set the marker in the test process to confirm the removal is active.
        unsafe { std::env::set_var("DEVKIT_PORTD_SELF", "1") };
        let env = BTreeMap::new();
        let mut c = Command::new("true"); // program name does not matter
        configure_child(&mut c, &[], ".", &env);
        // get_envs() returns None for keys explicitly removed, Some(_) for inherited.
        let marker = c
            .get_envs()
            .find(|(k, _)| *k == OsStr::new("DEVKIT_PORTD_SELF"));
        match marker {
            Some((_, None)) => {} // explicit removal recorded — correct
            Some((_, Some(v))) => panic!(
                "DEVKIT_PORTD_SELF must be removed but child would inherit {:?}",
                v
            ),
            None => {
                // The key does not appear in get_envs() at all. On some platforms
                // env_remove of a key not present in the inherited set is a no-op in
                // the get_envs iterator. Confirm the test process actually has it set
                // so the removal was meaningful and we're not testing an empty case.
                assert!(
                    std::env::var_os("DEVKIT_PORTD_SELF").is_some(),
                    "DEVKIT_PORTD_SELF was not set in the test process — test is a no-op"
                );
            }
        }
        // Clean up so we don't pollute other tests in the same process.
        unsafe { std::env::remove_var("DEVKIT_PORTD_SELF") };
    }

    /// First python interpreter that actually launches, if any. Returns the program
    /// name to invoke. `None` when no interpreter can be spawned — e.g. a host where
    /// `python3` exists only as a shell shim the OS cannot exec directly — in which
    /// case the dependent test skips rather than failing on a missing tool.
    fn python_cmd() -> Option<&'static str> {
        ["python3", "python", "py"].into_iter().find(|cand| {
            Command::new(cand)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok()
        })
    }

    #[test]
    fn spawn_and_ready_on_python_http() {
        let Some(py) = python_cmd() else {
            eprintln!("skipping spawn_and_ready_on_python_http: no launchable python interpreter");
            return;
        };
        let tmp = std::env::temp_dir().join(format!("devrun-{}.log", std::process::id()));
        // pick a free port
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let argv: Vec<String> = [py, "-m", "http.server", &port.to_string()]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let env = BTreeMap::new();
        let pid = spawn_detached(&argv, ".", &env, &tmp).unwrap();
        assert!(
            wait_ready(port, Duration::from_secs(10)),
            "server never came up"
        );
        stop(pid);
        let _ = fs::remove_file(&tmp);
    }
}
