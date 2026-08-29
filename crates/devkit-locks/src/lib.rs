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

/// The checkout containing `start`, or `start` itself when it is not in a
/// repository. The fallback is deliberate: `lockm` is usable outside a
/// repository, and its locks still need a scope. Asking git rather than
/// looking for a directory named `.git` is what keeps that answer honest — the
/// filename is not the repository. When git itself could not be run — as
/// opposed to running and reporting no repository — the same fallback
/// applies, but only after a warning: silently scoping to `start` would let
/// two callers under one repository, one hit while git is transiently
/// unavailable, land on two different roots with nothing to explain why.
pub fn find_root_from(start: &Path) -> PathBuf {
    match devkit_common::git::checkout_root_opt(start) {
        Ok(Some(root)) => root,
        Ok(None) => start.to_path_buf(),
        Err(e) => {
            eprintln!(
                "warning: git could not be run ({e:#}); scoping locks to {}",
                start.display()
            );
            start.to_path_buf()
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

/// Express `abs` relative to `root`, falling back to the filesystem's own
/// spelling of both when the caller's does not line up. git reports the
/// checkout root with links resolved, so a path reaching the repository
/// through a symlink or a junction shares no lexical prefix with it. Resolving
/// the directory rather than the path itself keeps this working for a file
/// that does not exist yet, which is every first write to a new file; when
/// either side refuses to resolve, the lexical answer stands so the error
/// stays the one the caller would have seen.
pub fn rel_under_root(abs: &Path, root: &Path) -> Result<String> {
    let lexical = normalize_under_root(abs, root);
    if lexical.is_ok() {
        return lexical;
    }
    let resolved = |p: &Path| std::fs::canonicalize(p).ok();
    match (abs.parent(), abs.file_name()) {
        (Some(dir), Some(name)) => match (resolved(dir), resolved(root)) {
            (Some(dir), Some(root)) => normalize_under_root(&dir.join(name), &root),
            _ => lexical,
        },
        _ => lexical,
    }
}

/// Resolve a CLI path argument (absolute or cwd-relative) to a root-relative key.
fn normalize_arg(arg: &str, cwd: &Path, root: &Path) -> Result<String> {
    let p = Path::new(arg);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    rel_under_root(&abs, root)
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
/// path). The root is git's checkout root for the file's own directory, not the
/// process's cwd, so the decision does not depend on where the hook process was
/// spawned; outside a repository the root falls back to that directory itself.
fn write_ctx(path_in: &str) -> Result<(String, String)> {
    WriteResolver::new().ctx(path_in)
}

// The hook process is ephemeral; harness locks are reclaimed by lifecycle
// release (SubagentStop/SessionEnd) or the TTL backstop, never by pid
// liveness. Anchoring to a pid would cause locks to be treated as dead if
// the hook ever ran attached to a tty.
fn decide_write_at(
    root: &str,
    path: &str,
    holder: &str,
    note: Option<&str>,
    ttl: u64,
) -> Result<model::WriteDecision> {
    let pid: Option<u32> = None;
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::WriteDecide {
        root: root.to_string(),
        holder: holder.to_string(),
        path: path.to_string(),
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
        root,
        holder,
        path,
        pid,
        note,
        ttl,
        now(),
    )
}

/// Enforced-write decision for `path_in` by an explicit `holder` (the hook derives
/// the holder from the agent payload; identity is not resolved here). Free → acquire;
/// self/ancestor → allow; otherwise deny. Resolves its own checkout root on every
/// call; a caller deciding several paths in one batch should use
/// [`WriteResolver`] instead so paths sharing a directory share one resolution.
pub fn decide_write(
    path_in: &str,
    holder: &str,
    note: Option<&str>,
    ttl: u64,
) -> Result<model::WriteDecision> {
    let (root, path) = write_ctx(path_in)?;
    decide_write_at(&root, &path, holder, note, ttl)
}

/// Decides write requests for a batch of paths handled in one hook invocation.
/// Each path's checkout root is resolved from that file's own directory, not
/// the process's cwd — a batch can touch files that belong to different
/// repositories than wherever the hook process happened to be spawned, and
/// scoping to cwd would put such a file's lock under the wrong root. The root
/// for a given directory is resolved once and reused for every later path in
/// the same directory, so a batch of files that share a directory pays for
/// one `git` call instead of one per file.
#[derive(Default)]
pub struct WriteResolver {
    roots: std::collections::HashMap<PathBuf, PathBuf>,
}

impl WriteResolver {
    pub fn new() -> Self {
        Self::default()
    }

    fn root_for(&mut self, start: &Path) -> PathBuf {
        if let Some(root) = self.roots.get(start) {
            return root.clone();
        }
        let root = find_root_from(start);
        self.roots.insert(start.to_path_buf(), root.clone());
        root
    }

    fn ctx(&mut self, path_in: &str) -> Result<(String, String)> {
        let p = Path::new(path_in);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir()
                .context("getting current dir")?
                .join(p)
        };
        let start = abs.parent().unwrap_or(abs.as_path());
        let root = self.root_for(start);
        let rel = rel_under_root(&abs, &root)?;
        Ok((root.to_string_lossy().into_owned(), rel))
    }

    /// Same decision as [`decide_write`], but sharing this resolver's cache.
    pub fn decide_write(
        &mut self,
        path_in: &str,
        holder: &str,
        note: Option<&str>,
        ttl: u64,
    ) -> Result<model::WriteDecision> {
        let (root, path) = self.ctx(path_in)?;
        decide_write_at(&root, &path, holder, note, ttl)
    }
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
        let root = tempfile::tempdir().unwrap();
        devkit_common::git::Git::fixture(root.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        let r = root.path().to_string_lossy().into_owned();
        let paths = vec!["a.rs".to_string()];

        let out = acquire_resolved(&r, "holder-a", &paths, None, None, 60).expect("acquire");
        assert_eq!(out.acquired.len(), 1);
        assert_eq!(out.acquired[0].path, "a.rs");
        assert!(out.conflicts.is_empty());

        let conflicts = check_resolved(&r, "holder-b", &paths).expect("check");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].held_by, "holder-a");

        let entries = status_resolved(&r, false).expect("status");
        assert!(
            entries
                .iter()
                .any(|e| e.path == "a.rs" && e.holder == "holder-a")
        );

        let (released, refused) = release_resolved(&r, "holder-a", &paths, false).expect("release");
        assert_eq!(released, vec!["a.rs".to_string()]);
        assert!(refused.is_empty());

        // release_all on a now-empty root is a no-op but must succeed.
        assert!(
            release_all_resolved(&r, "holder-a")
                .expect("release_all")
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A directory named `.git` is not a repository. The fixture is a real one
    /// so this asserts root resolution rather than the presence of a filename.
    fn init_repo(at: &Path) {
        devkit_common::git::Git::fixture(at)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
    }

    #[test]
    fn root_resolves_from_a_subdirectory() {
        let root = tempfile::tempdir().unwrap();
        init_repo(root.path());
        let deep = root.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            std::fs::canonicalize(find_root_from(&deep)).unwrap(),
            std::fs::canonicalize(root.path()).unwrap()
        );
    }

    /// Outside a repository, lock scoping falls back to the start directory.
    /// Declared, because `lockm` is usable outside a repository and its locks
    /// must still be scoped to somewhere.
    #[test]
    fn root_falls_back_to_start_outside_a_repository() {
        let start = tempfile::tempdir().unwrap();
        assert_eq!(find_root_from(start.path()), start.path());
    }

    /// A directory named `.git` without `HEAD`, `objects`, and `refs` is not a
    /// repository, so root resolution falls through to the start directory.
    #[test]
    fn a_bare_dot_git_directory_is_not_a_checkout_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();
        let deep = tmp.path().join("a/b");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_root_from(&deep), deep);
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

    /// Point `link` at the directory `target`. Returns false where the platform
    /// refuses to make one, which on Windows means the process lacks the
    /// privilege for a symlink and `mklink /J` was unavailable too.
    fn link_dir(target: &Path, link: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        }
    }

    /// git reports the checkout root with links resolved, so a path reaching the
    /// repository through one spells its root differently than a path that does
    /// not. Both must land on the same lock key: two sessions naming one file by
    /// different spellings have to collide, not sit on separate keys.
    #[test]
    fn write_ctx_agrees_on_a_path_reaching_the_repo_through_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        init_repo(&repo);
        let link = tmp.path().join("link");
        if !link_dir(&repo, &link) {
            return;
        }
        let direct = write_ctx(repo.join("src/a.rs").to_str().unwrap()).unwrap();
        let linked = write_ctx(link.join("src/a.rs").to_str().unwrap()).unwrap();
        assert_eq!(direct, linked);
    }

    #[test]
    fn write_ctx_derives_root_and_relpath() {
        let root = tempfile::tempdir().unwrap();
        devkit_common::git::Git::fixture(root.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let file = root.path().join("src/a.rs");
        let (r, rel) = write_ctx(file.to_str().unwrap()).unwrap();
        assert_eq!(PathBuf::from(&r), root.path());
        assert_eq!(rel, "src/a.rs");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A shared cache keyed on the wrong thing (e.g. "last root resolved"
    /// rather than "root resolved for this directory") would scope the
    /// second file to the first file's repository; `normalize_under_root`
    /// would then reject it as outside that root, since the two directories
    /// share no ancestor.
    #[test]
    fn resolver_scopes_each_batch_member_to_its_own_repository() {
        let repo_a = tempfile::tempdir().unwrap();
        init_repo(repo_a.path());
        std::fs::create_dir_all(repo_a.path().join("src")).unwrap();
        let file_a = repo_a.path().join("src/a.rs");

        let repo_b = tempfile::tempdir().unwrap();
        init_repo(repo_b.path());
        std::fs::create_dir_all(repo_b.path().join("src")).unwrap();
        let file_b = repo_b.path().join("src/a.rs");

        let holder = format!("resolver-test-{}", std::process::id());
        let mut resolver = WriteResolver::new();

        let a = resolver
            .decide_write(file_a.to_str().unwrap(), &holder, None, 60)
            .expect("file in repo_a resolves to repo_a");
        assert!(matches!(a, model::WriteDecision::Acquired));

        let b = resolver
            .decide_write(file_b.to_str().unwrap(), &holder, None, 60)
            .expect("file in repo_b resolves to repo_b, not repo_a's cached root");
        assert!(matches!(b, model::WriteDecision::Acquired));

        let _ = release_all_resolved(&repo_a.path().to_string_lossy(), &holder);
        let _ = release_all_resolved(&repo_b.path().to_string_lossy(), &holder);
    }
}
