//! Request handlers. Registry ops call the same flock facade the no-daemon path
//! uses (the daemon is a flock participant). Supervision ops own processes.

use crate::supervisor::{Key, Launch};
use crate::Daemon;
use devkit_common::supervise;
use devkit_ports::daemon::proto::{Request, Response, PROTO};
use devkit_ports::registry::{self, Role};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// Map a request to `(response, should_close)`.
pub(crate) fn dispatch(daemon: &Arc<Daemon>, req: Request) -> (Response, bool) {
    match req {
        Request::Ping { .. } => (Response::Pong { proto: PROTO, pid: std::process::id() }, false),

        Request::Alloc { holder, reqs, role } => match registry::alloc(&holder, &reqs, role) {
            Ok(ports) => (Response::Ports(ports), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },
        Request::RecordPid { port, app, holder, role, pid, logfile } => {
            match registry::record_pid(port, &app, &holder, role, pid, logfile) {
                Ok(()) => (Response::Ok, false),
                Err(e) => (Response::Err(format!("{e:#}")), false),
            }
        }
        Request::Release { holder, role } => match registry::release(&holder, role) {
            Ok(freed) => (Response::Freed(freed), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },
        Request::Snapshot => match registry::snapshot() {
            Ok(data) => (Response::Snapshot(data), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },
        Request::Prune => match registry::prune() {
            Ok(freed) => (Response::Freed(freed), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },

        Request::Supervise { holder, app, role, argv, cwd, env, logfile, base_port } => {
            (supervise_app(daemon, holder, app, role, argv, cwd, env, logfile, base_port), false)
        }
        Request::Down { holder, role } => (down(daemon, holder, role), false),
        Request::Tail { holder, app, role, lines } => (tail(holder, app, role, lines), false),

        Request::Shutdown => {
            daemon.shutdown.store(true, Ordering::SeqCst);
            let _ = std::os::unix::net::UnixStream::connect(devkit_common::paths::socket_file());
            (Response::Ok, true)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn supervise_app(
    daemon: &Arc<Daemon>, holder: String, app: String, role: Role,
    argv: Vec<String>, cwd: String, env: std::collections::BTreeMap<String, String>,
    logfile: std::path::PathBuf, base_port: u16,
) -> Response {
    // Reserve before bind (same invariant as the flock path).
    let reqs = vec![(app.clone(), base_port)];
    let port = match registry::alloc(&holder, &reqs, role) {
        Ok(p) => p.into_iter().find(|(a, _)| *a == app).map(|(_, p)| p),
        Err(e) => return Response::Err(format!("{e:#}")),
    };
    let Some(port) = port else { return Response::Err("alloc returned no port".into()) };

    let pid = match supervise::spawn_detached(&argv, &cwd, &env, &logfile) {
        Ok(pid) => pid,
        Err(e) => return Response::Err(format!("{e:#}")),
    };
    if let Err(e) = registry::record_pid(port, &app, &holder, role, pid, logfile.clone()) {
        return Response::Err(format!("{e:#}"));
    }
    let launch = Launch { argv, cwd, env };
    daemon.sup.lock().unwrap().insert_owned(
        Key { holder, app, role }, pid, port, logfile, launch,
    );
    let ready = supervise::wait_ready(port, Duration::from_secs(120));
    Response::Supervised(vec![(port, ready)])
}

/// Atomic stop: remove each supervised child for this holder/role from the table
/// (so the supervision thread won't restart it), SIGTERM it, then release the rows.
fn down(daemon: &Arc<Daemon>, holder: String, role: Option<Role>) -> Response {
    // Read the registry first, without holding `sup`, so this never blocks the
    // supervision thread while that thread holds the registry flock (every path
    // takes the flock before `sup`, never the reverse).
    let keys: Vec<Key> = registry::snapshot()
        .map(|d| {
            d.entries.values()
                .filter(|e| e.holder == holder && role.is_none_or(|r| e.role == r))
                .map(|e| Key { holder: e.holder.clone(), app: e.app.clone(), role: e.role })
                .collect()
        })
        .unwrap_or_default();
    let mut sup = daemon.sup.lock().unwrap();
    for k in &keys {
        if let Some(pid) = sup.remove(k) {
            supervise::stop(pid);
        }
    }
    drop(sup);
    match registry::release(&holder, role) {
        Ok(freed) => Response::Freed(freed),
        Err(e) => Response::Err(format!("{e:#}")),
    }
}

fn tail(holder: String, app: String, role: Option<Role>, lines: usize) -> Response {
    match registry::snapshot() {
        Ok(d) => {
            let log = d.entries.values()
                .find(|e| e.holder == holder && e.app == app && role.is_none_or(|r| e.role == r))
                .and_then(|e| e.logfile.clone());
            match log {
                Some(p) => Response::Lines(supervise::tail(&p, lines)),
                None => Response::Err(format!("no tracked log for `{app}`")),
            }
        }
        Err(e) => Response::Err(format!("{e:#}")),
    }
}
