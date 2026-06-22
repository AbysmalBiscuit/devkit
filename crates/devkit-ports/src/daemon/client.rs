//! Port daemon client: connect to the supervisor over `ports.sock`, with the
//! port-proto handshake layered on the shared `Client`.

use crate::daemon::proto::{PROTO, Request, Response};
use anyhow::{Result, anyhow};
use devkit_common::daemon::{self, Client};
use devkit_common::paths;
use std::time::{Duration, Instant};

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

/// Validate a fresh connection with the port Ping/Pong handshake. A proto
/// mismatch (old daemon survived an upgrade) asks it to shut down and fails.
fn shake(mut c: Client) -> Option<Client> {
    match c.request::<Request, Response>(&Request::Ping { proto: PROTO }) {
        Ok(Response::Pong { proto, .. }) if handshake_ok(proto) => Some(c),
        Ok(Response::Pong { .. }) => {
            let _ = c.request::<Request, Response>(&Request::Shutdown);
            None
        }
        _ => None,
    }
}

/// Connect to an already-running daemon; `None` if none is up or the handshake
/// fails. Never autostarts.
pub fn try_existing() -> Option<Client> {
    shake(daemon::connect(&paths::port_socket_file())?)
}

/// Locate the daemon binary: `$DEVKITD_BIN`, else a sibling of the current exe,
/// else `devkitd` on `PATH`.
fn devkitd_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("DEVKITD_BIN") {
        return p.into();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("devkitd");
        if sibling.is_file() {
            return sibling;
        }
    }
    std::path::PathBuf::from("devkitd")
}

/// Whether autostart should launch the daemon via `systemctl --user start` rather
/// than exec'ing the binary directly: true when the systemd user unit is present.
fn use_systemd_unit() -> bool {
    devkit_common::paths::systemd_user_unit().is_file()
}

/// Connect, autostarting a daemon if none is running (supervision paths only).
pub fn ensure_running() -> Result<Client> {
    if let Some(c) = try_existing() {
        return Ok(c);
    }
    if use_systemd_unit() {
        match std::process::Command::new("systemctl")
            .args(["--user", "start", "devkitd.service"])
            .status()
        {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("devkitd: `systemctl --user start devkitd.service` exited with {s}"),
            Err(e) => {
                eprintln!("devkitd: failed to run `systemctl --user start devkitd.service`: {e}")
            }
        }
    } else {
        daemon::spawn(&devkitd_bin())?;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(c) = try_existing() {
            return Ok(c);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!("daemon did not come up within 5s"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proto_match_decision() {
        assert!(handshake_ok(PROTO));
        assert!(!handshake_ok(PROTO + 1));
    }

    #[test]
    fn routes_through_systemd_only_when_unit_present() {
        // Use a unique temp dir so the test does not observe the developer's real
        // ~/.config/systemd/user/devkitd.service — both branches are asserted.
        let tmp = std::env::temp_dir().join(format!("devkit-routing-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Point XDG_CONFIG_HOME at the empty temp dir: no unit present → false.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        assert!(!super::use_systemd_unit(), "no unit yet — should be false");

        // Write the unit file and assert the positive branch.
        let unit = tmp.join("systemd/user/devkitd.service");
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        std::fs::write(&unit, "").unwrap();
        assert!(super::use_systemd_unit(), "unit present — should be true");

        // Restore env and clean up.
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
