//! A freshness-bounded JSON cache for the dashboard's network fetches.
//!
//! The at-a-glance panel (worktree triage + PR tables) is always fetched live.
//! The timeline charts below it are fed by slow-moving historical data from
//! Linear and GitHub — trends that barely move minute to minute — so each of
//! those fetches is memoized to `~/.cache/devkit/dashboard/<key>.json` with the
//! time it was taken. A cached value younger than its TTL is reused instead of
//! refetched; `issue dashboard --no-cache` bypasses the cache for a fully live
//! render. A cache miss or write failure is never fatal: the fetch just runs.

use devkit_common::paths;
use devkit_common::tracker::TrackerKind;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a dashboard cache entry belongs to. Two projects on different trackers
/// would otherwise serve each other's timelines, and two viewers of one
/// project would serve each other's assigned issues.
pub struct CacheScope {
    pub tracker: TrackerKind,
    pub repo: String,
    pub viewer: String,
}

/// Every component is hashed rather than interpolated, so no value — not even
/// a credential in `viewer` — reaches the filename. A configured repository
/// slug reaches this scope and `devkit.toml` travels with a checkout, so a
/// value carrying `..` would otherwise let a read-only dashboard command write
/// outside the cache directory.
///
/// `DefaultHasher` is explicitly not stable across Rust releases; a toolchain
/// upgrade invalidates every cached entry once, which is harmless for a cache
/// this disposable — not a reason to reach for an external hash crate.
fn path_for(scope: &CacheScope, key: &str) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    scope.tracker.as_str().hash(&mut h);
    scope.repo.hash(&mut h);
    scope.viewer.hash(&mut h);
    let digest = format!("{:016x}", h.finish());
    // `key` is a compile-time literal from this module; the scope is not.
    paths::cache_dir()
        .join("dashboard")
        .join(format!("{key}-{digest}.json"))
}

#[derive(serde::Deserialize)]
struct Cached<T> {
    fetched_at: u64,
    value: T,
}

#[derive(Serialize)]
struct CachedRef<'a, T> {
    fetched_at: u64,
    value: &'a T,
}

/// Read `path` if it holds a value no older than `ttl` seconds at `now`.
/// `None` when missing, unreadable, stale, or malformed. `ttl == 0` disables
/// the freshness check, treating any cached value as fresh.
fn read_fresh<T: DeserializeOwned>(path: &Path, ttl: u64, now: u64) -> Option<T> {
    let s = std::fs::read_to_string(path).ok()?;
    let c: Cached<T> = serde_json::from_str(&s).ok()?;
    if ttl != 0 && now.saturating_sub(c.fetched_at) > ttl {
        return None;
    }
    Some(c.value)
}

/// Persist `value` at `path`, stamped `now`. Best-effort: errors are swallowed
/// because a cache write failure must not break rendering.
fn write_at<T: Serialize>(path: &Path, value: &T, now: u64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&CachedRef {
        fetched_at: now,
        value,
    }) {
        let _ = std::fs::write(path, bytes);
    }
}

/// Cached value for `scope`/`key` if present and younger than `ttl` seconds;
/// `None` otherwise. Callers gate this on `--no-cache` themselves.
pub fn get<T: DeserializeOwned>(scope: &CacheScope, key: &str, ttl: u64) -> Option<T> {
    read_fresh(&path_for(scope, key), ttl, now_secs())
}

/// Persist `value` under `scope`/`key`, stamped with the current time.
/// Best-effort. Callers skip this for empty/failed fetches so a transient
/// miss never poisons the cache.
pub fn put<T: Serialize>(scope: &CacheScope, key: &str, value: &T) {
    write_at(&path_for(scope, key), value, now_secs());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "devkit-dash-cache-{}-{}.json",
            std::process::id(),
            tag
        ))
    }

    #[test]
    fn read_fresh_roundtrips_within_ttl() {
        let p = scratch("rt");
        write_at(&p, &vec![1u32, 2, 3], 1000);
        let back: Vec<u32> = read_fresh(&p, 600, 1300).expect("fresh within ttl");
        assert_eq!(back, vec![1, 2, 3]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_fresh_rejects_stale() {
        let p = scratch("stale");
        write_at(&p, &"hi".to_string(), 1000);
        // 1000s elapsed exceeds the 600s ttl.
        assert!(read_fresh::<String>(&p, 600, 2000).is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_fresh_missing_is_none() {
        let p = scratch("missing");
        let _ = std::fs::remove_file(&p);
        assert!(read_fresh::<u32>(&p, 600, 1000).is_none());
    }

    #[test]
    fn ttl_zero_never_expires() {
        let p = scratch("forever");
        write_at(&p, &7u32, 0);
        assert_eq!(read_fresh::<u32>(&p, 0, 1_000_000), Some(7));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn get_put_roundtrip_under_real_cache_dir() {
        let key = "devkit-dash-cache-getput-probe";
        let s = scope(TrackerKind::Linear, "acme", "me");
        let _ = std::fs::remove_file(path_for(&s, key));
        assert!(get::<Vec<i64>>(&s, key, 600).is_none());
        put(&s, key, &vec![10i64, 20, 30]);
        assert_eq!(get::<Vec<i64>>(&s, key, 600), Some(vec![10, 20, 30]));
        let _ = std::fs::remove_file(path_for(&s, key));
    }

    fn scope(tracker: TrackerKind, repo: &str, viewer: &str) -> CacheScope {
        CacheScope {
            tracker,
            repo: repo.to_string(),
            viewer: viewer.to_string(),
        }
    }

    #[test]
    fn two_projects_do_not_share_a_cache_entry() {
        // A cache entry belongs to one (tracker, repo, viewer). Every key this
        // module stores — `issues`, `pr-timeline-mine`, `pr-timeline-all` — is a
        // fixed literal, so the scope is the only thing keeping one project's
        // timelines out of another's dashboard.
        let a = path_for(&scope(TrackerKind::Linear, "acme", "me"), "issues");
        let b = path_for(&scope(TrackerKind::Github, "o/r", "me"), "issues");
        let c = path_for(&scope(TrackerKind::Github, "o/r", "someone"), "issues");
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    #[test]
    fn a_scope_component_cannot_escape_the_cache_directory() {
        // `issues_repo` reaches the scope from `devkit.toml`, which travels with a
        // checkout, so a cache path is built partly from a value the checkout
        // chose. Hashing every component is what keeps a `..` in one inside the
        // cache directory.
        let root = paths::cache_dir().join("dashboard");
        let p = path_for(&scope(TrackerKind::Github, "../../../etc", "me"), "issues");
        assert!(
            p.starts_with(&root),
            "{} escaped {}",
            p.display(),
            root.display()
        );
        assert!(!p.to_string_lossy().contains(".."), "{}", p.display());
    }
}
