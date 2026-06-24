pub mod hook;
pub mod ident;
pub mod model;
pub mod store;

#[cfg(feature = "daemon")]
pub mod daemon;

use anyhow::{Context, Result};
use model::{AcquireOutcome, Conflict, LockEntry};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Try a running daemon over `locks.sock`. `Ok(None)` = no daemon (caller uses
/// the flock path). `Ok(Some(resp))` = the daemon answered. `Err` = a live daemon
/// failed mid-request — surfaced rather than written behind its back. Inside the
/// daemon itself (`DEVKITD_SELF`) returns `Ok(None)` so its own ops stay local.
#[cfg(feature = "daemon")]
fn daemon_request(req: daemon::proto::Request) -> Result<Option<daemon::proto::Response>> {
    if std::env::var_os("DEVKITD_SELF").is_some() {
        return Ok(None);
    }
    let Some(mut c) = daemon::client::try_existing() else {
        return Ok(None);
    };
    Ok(Some(
        c.request::<daemon::proto::Request, daemon::proto::Response>(&req)?,
    ))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Nearest ancestor of `start` containing a `.git` entry; `start` itself if none.
pub fn find_root_from(start: &Path) -> PathBuf {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return start.to_path_buf(),
        }
    }
}

fn find_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    Ok(find_root_from(&cwd))
}

/// Lexically clean `abs` and express it relative to `root` ('/'-separated; the root
/// itself becomes "."). Errors if `abs` is not under `root`.
pub fn normalize_under_root(abs: &Path, root: &Path) -> Result<String> {
    let rel = abs
        .strip_prefix(root)
        .ok()
        .context("path is outside the project root")?;
    let mut parts: Vec<String> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str().context("non-utf8 path")?.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    Ok(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// Resolve a CLI path argument (absolute or cwd-relative) to a root-relative key.
fn normalize_arg(arg: &str, cwd: &Path, root: &Path) -> Result<String> {
    let p = Path::new(arg);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    normalize_under_root(&abs, root)
}

struct Ctx {
    root: String,
    holder: String,
    paths: Vec<String>,
}

fn ctx(paths_in: &[String], as_flag: Option<&str>) -> Result<Ctx> {
    let cwd = std::env::current_dir().context("getting current dir")?;
    let root = find_root_from(&cwd);
    let mut paths = Vec::with_capacity(paths_in.len());
    for a in paths_in {
        paths.push(normalize_arg(a, &cwd, &root)?);
    }
    Ok(Ctx {
        root: root.to_string_lossy().into_owned(),
        holder: ident::identity(as_flag),
        paths,
    })
}

pub fn acquire(
    paths_in: &[String],
    as_flag: Option<&str>,
    note: Option<&str>,
    ttl: u64,
) -> Result<AcquireOutcome> {
    let c = ctx(paths_in, as_flag)?;
    acquire_resolved(&c.root, &c.holder, &c.paths, ident::anchor_pid(), note, ttl)
}

/// Acquire `paths` for `holder` under `root` with a pre-resolved context (no CWD
/// or identity derivation). Routes through a live daemon when one is up, else the
/// flock store. The CWD-deriving `acquire` delegates here.
pub fn acquire_resolved(
    root: &str,
    holder: &str,
    paths: &[String],
    pid: Option<u32>,
    note: Option<&str>,
    ttl: u64,
) -> Result<AcquireOutcome> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Acquire {
        root: root.to_string(),
        holder: holder.to_string(),
        paths: paths.to_vec(),
        pid,
        note: note.map(str::to_string),
        ttl,
    })? {
        return match resp {
            daemon::proto::Response::Acquired(o) => Ok(o),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::acquire_with(
        &store::FlockStore::new(),
        root,
        holder,
        paths,
        pid,
        note,
        ttl,
        now(),
    )
}

pub fn check(paths_in: &[String], as_flag: Option<&str>) -> Result<Vec<Conflict>> {
    let c = ctx(paths_in, as_flag)?;
    check_resolved(&c.root, &c.holder, &c.paths)
}

/// Conflicts that would block `holder` from `paths` under `root` (pre-resolved).
pub fn check_resolved(root: &str, holder: &str, paths: &[String]) -> Result<Vec<Conflict>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Check {
        root: root.to_string(),
        holder: holder.to_string(),
        paths: paths.to_vec(),
    })? {
        return match resp {
            daemon::proto::Response::Conflicts(v) => Ok(v),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::check_with(&store::FlockStore::new(), root, holder, paths, now())
}

pub fn release(
    paths_in: &[String],
    as_flag: Option<&str>,
    force: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let c = ctx(paths_in, as_flag)?;
    release_resolved(&c.root, &c.holder, &c.paths, force)
}

/// Release named `paths` held by `holder` under `root` (pre-resolved). Returns
/// (released, refused).
pub fn release_resolved(
    root: &str,
    holder: &str,
    paths: &[String],
    force: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Release {
        root: root.to_string(),
        holder: holder.to_string(),
        paths: paths.to_vec(),
        force,
    })? {
        return match resp {
            daemon::proto::Response::Released { released, refused } => Ok((released, refused)),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::release_with(&store::FlockStore::new(), root, holder, paths, force)
}

pub fn release_all(as_flag: Option<&str>) -> Result<Vec<String>> {
    let c = ctx(&[], as_flag)?;
    release_all_resolved(&c.root, &c.holder)
}

/// Release every lock held by `holder` under `root` (pre-resolved).
pub fn release_all_resolved(root: &str, holder: &str) -> Result<Vec<String>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::ReleaseAll {
        root: root.to_string(),
        holder: holder.to_string(),
    })? {
        return match resp {
            daemon::proto::Response::Freed(v) => Ok(v),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::release_all_with(&store::FlockStore::new(), root, holder)
}

/// Live locks for the current project root, or every project when `all`.
pub fn status(all: bool) -> Result<Vec<LockEntry>> {
    let root = find_root()?.to_string_lossy().into_owned();
    status_resolved(&root, all)
}

/// Live locks for `root`, or every project when `all` (pre-resolved root).
pub fn status_resolved(root: &str, all: bool) -> Result<Vec<LockEntry>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Status {
        root: root.to_string(),
        all,
    })? {
        return match resp {
            daemon::proto::Response::Locks(v) => Ok(v),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::status_with(&store::FlockStore::new(), root, all, now())
}

pub fn prune() -> Result<usize> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Prune)? {
        return match resp {
            daemon::proto::Response::Pruned(n) => Ok(n),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::prune_with(&store::FlockStore::new(), now())
}

/// Resolve a write target (absolute, or cwd-relative) to (project_root, root-relative
/// path). The root is the nearest `.git` ancestor of the file's directory, so the
/// decision does not depend on where the hook process was spawned.
fn write_ctx(path_in: &str) -> Result<(String, String)> {
    let p = Path::new(path_in);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .context("getting current dir")?
            .join(p)
    };
    let start = abs.parent().unwrap_or(abs.as_path());
    let root = find_root_from(start);
    let rel = normalize_under_root(&abs, &root)?;
    Ok((root.to_string_lossy().into_owned(), rel))
}

/// Enforced-write decision for `path_in` by an explicit `holder` (the hook derives
/// the holder from the agent payload; identity is not resolved here). Free → acquire;
/// self/ancestor → allow; otherwise deny.
pub fn decide_write(
    path_in: &str,
    holder: &str,
    note: Option<&str>,
    ttl: u64,
) -> Result<model::WriteDecision> {
    let (root, path) = write_ctx(path_in)?;
    // The hook process is ephemeral; harness locks are reclaimed by lifecycle
    // release (SubagentStop/SessionEnd) or the TTL backstop, never by pid
    // liveness. Anchoring to a pid would cause locks to be treated as dead if
    // the hook ever ran attached to a tty.
    let pid: Option<u32> = None;
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::WriteDecide {
        root: root.clone(),
        holder: holder.to_string(),
        path: path.clone(),
        pid,
        note: note.map(str::to_string),
        ttl,
    })? {
        return match resp {
            daemon::proto::Response::WriteDecided(d) => Ok(d),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::write_decide_with(
        &store::FlockStore::new(),
        &root,
        holder,
        &path,
        pid,
        note,
        ttl,
        now(),
    )
}

/// Release every lock held by `holder_prefix` or its descendants, across all roots.
/// Holder ids are globally unique per session/sub-agent, so no root filter is needed.
pub fn release_prefix(holder_prefix: &str) -> Result<Vec<String>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::ReleasePrefix {
        prefix: holder_prefix.to_string(),
    })? {
        return match resp {
            daemon::proto::Response::Freed(v) => Ok(v),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::release_prefix_with(&store::FlockStore::new(), holder_prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("devkit-lib-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn facade_without_daemon_uses_flock_path() {
        // No daemon is running in unit tests, so daemon_request returns Ok(None) and
        // the call falls through to the FlockStore path — proving the split's fallback
        // stays wired.
        let n = prune().expect("prune via flock path");
        let _ = n; // count depends on ambient registry; success is the assertion
    }

    #[test]
    fn resolved_fns_roundtrip_via_flock_path() {
        // No daemon runs in unit tests, so the `_resolved` fns fall through to the
        // FlockStore path. A unique root namespaces these lock rows.
        let root = scratch("resolved-roundtrip");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let r = root.to_string_lossy().into_owned();
        let paths = vec!["a.rs".to_string()];

        let out = acquire_resolved(&r, "holder-a", &paths, None, None, 60).expect("acquire");
        assert_eq!(out.acquired.len(), 1);
        assert_eq!(out.acquired[0].path, "a.rs");
        assert!(out.conflicts.is_empty());

        let conflicts = check_resolved(&r, "holder-b", &paths).expect("check");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].held_by, "holder-a");

        let entries = status_resolved(&r, false).expect("status");
        assert!(entries.iter().any(|e| e.path == "a.rs" && e.holder == "holder-a"));

        let (released, refused) = release_resolved(&r, "holder-a", &paths, false).expect("release");
        assert_eq!(released, vec!["a.rs".to_string()]);
        assert!(refused.is_empty());

        // release_all on a now-empty root is a no-op but must succeed.
        assert!(release_all_resolved(&r, "holder-a").expect("release_all").is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_root_walks_up_to_git() {
        let root = scratch("git-root");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let deep = root.join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_root_from(&deep), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_root_falls_back_to_start_without_git() {
        let start = scratch("no-git");
        assert_eq!(find_root_from(&start), start);
        let _ = std::fs::remove_dir_all(&start);
    }

    #[test]
    fn normalize_makes_root_relative() {
        let root = Path::new("/repo");
        assert_eq!(
            normalize_under_root(Path::new("/repo/scenes/x.tscn"), root).unwrap(),
            "scenes/x.tscn"
        );
        assert_eq!(
            normalize_under_root(Path::new("/repo/./scenes/"), root).unwrap(),
            "scenes"
        );
        assert_eq!(normalize_under_root(Path::new("/repo"), root).unwrap(), ".");
    }

    #[test]
    fn normalize_rejects_outside_root() {
        assert!(normalize_under_root(Path::new("/elsewhere/x"), Path::new("/repo")).is_err());
    }

    #[test]
    fn write_ctx_derives_root_and_relpath() {
        let root = scratch("wctx");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/a.rs");
        let (r, rel) = write_ctx(file.to_str().unwrap()).unwrap();
        assert_eq!(PathBuf::from(&r), root);
        assert_eq!(rel, "src/a.rs");
        let _ = std::fs::remove_dir_all(&root);
    }
}
