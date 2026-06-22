//! Lock-registry request handlers. Ops run through the daemon's authoritative
//! `MemoryStore`; reads serve from memory, mutations write through to the file.
//! The daemon stamps `now`; clients supply resolved root/holder/paths/pid.

use crate::Daemon;
use devkit_locks::daemon::proto::{PROTO, Request, Response};
use devkit_locks::store;
use std::sync::Arc;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub(crate) fn dispatch(daemon: &Arc<Daemon>, req: Request) -> Response {
    let s = daemon.lock_store();
    match req {
        Request::Ping { .. } => Response::Pong {
            proto: PROTO,
            pid: std::process::id(),
        },
        Request::Acquire {
            root,
            holder,
            paths,
            pid,
            note,
            ttl,
        } => {
            match store::acquire_with(&s, &root, &holder, &paths, pid, note.as_deref(), ttl, now())
            {
                Ok(o) => Response::Acquired(o),
                Err(e) => Response::Err(format!("{e:#}")),
            }
        }
        Request::Check {
            root,
            holder,
            paths,
        } => match store::check_with(&s, &root, &holder, &paths, now()) {
            Ok(v) => Response::Conflicts(v),
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::Release {
            root,
            holder,
            paths,
            force,
        } => match store::release_with(&s, &root, &holder, &paths, force) {
            Ok((released, refused)) => Response::Released { released, refused },
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::ReleaseAll { root, holder } => match store::release_all_with(&s, &root, &holder) {
            Ok(v) => Response::Freed(v),
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::Status { root, all } => match store::status_with(&s, &root, all, now()) {
            Ok(v) => Response::Locks(v),
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::Prune => match store::prune_with(&s, now()) {
            Ok(n) => Response::Pruned(n),
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::WriteDecide {
            root,
            holder,
            path,
            pid,
            note,
            ttl,
        } => match store::write_decide_with(
            &s, &root, &holder, &path, pid, note.as_deref(), ttl, now(),
        ) {
            Ok(d) => Response::WriteDecided(d),
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::ReleasePrefix { root, prefix } => {
            match store::release_prefix_with(&s, &root, &prefix) {
                Ok(v) => Response::Freed(v),
                Err(e) => Response::Err(format!("{e:#}")),
            }
        }
    }
}
