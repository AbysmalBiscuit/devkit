mod common;

use common::Harness;
use devkit_ports::daemon::proto::{PROTO, Request, Response};
use std::process::Command;
use std::time::Duration;

/// Handshake: a `Ping` with the current proto version must yield a `Pong` with
/// the same version and the daemon's pid.
#[test]
fn ping_pong_handshake() {
    let mut h = Harness::start();
    let resp = h.request(&Request::Ping { proto: PROTO });
    assert!(
        matches!(resp, Response::Pong { proto, .. } if proto == PROTO),
        "expected Pong with proto={PROTO}, got {resp:?}"
    );
    h.shutdown();
}

/// A daemon with a very short idle timeout exits on its own when it has no
/// active connections and no supervised children.
#[test]
fn idle_exit_with_no_clients_or_children() {
    let mut h = Harness::start_with_idle(1);
    // The idle watcher ticks every 500 ms. With a 1 s timeout, the daemon
    // should exit within ~2.5 s in normal conditions.
    let exited = h.wait_exit(Duration::from_secs(5));
    assert!(
        exited || h.socket_gone(),
        "daemon did not idle-exit within 5 s"
    );
}

/// A second `devkit-portd` started against the same HOME cannot take the
/// `portd.lock` and exits 0 immediately.  The original daemon must still
/// answer a Ping after the second one exits.
#[test]
fn second_instance_exits_immediately() {
    let mut h = Harness::start();

    let status = Command::new(common::daemon_bin())
        .env("HOME", &h.home)
        .env("XDG_STATE_HOME", &h.xdg_state)
        .env("DEVKIT_DAEMON_IDLE_SECS", "3600")
        .env("DEVKIT_PORTD_SELF", "1")
        .status()
        .expect("spawn second daemon");

    assert!(
        status.success(),
        "second daemon should exit 0 (lock contention), got {status:?}"
    );

    // The original daemon must still be responsive.
    let resp = h.request(&Request::Ping { proto: PROTO });
    assert!(
        matches!(resp, Response::Pong { .. }),
        "original daemon stopped responding after second instance exit: {resp:?}"
    );

    h.shutdown();
}
