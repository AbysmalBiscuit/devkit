pub mod client;
pub mod framing;
pub mod transport;

use std::sync::atomic::{AtomicBool, Ordering};

pub use client::{Client, connect, spawn};

static IN_DAEMON: AtomicBool = AtomicBool::new(false);

/// Mark this process as the daemon, so the registry facades resolve locally
/// instead of connecting back to this same daemon over its socket. The mark
/// lives in process memory, so processes the daemon starts never inherit it.
pub fn mark_in_daemon() {
    IN_DAEMON.store(true, Ordering::Relaxed);
}

/// Whether [`mark_in_daemon`] marked this process.
pub fn in_daemon() -> bool {
    IN_DAEMON.load(Ordering::Relaxed)
}
