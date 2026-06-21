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
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path_for(key: &str) -> PathBuf {
    paths::cache_dir()
        .join("dashboard")
        .join(format!("{key}.json"))
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

/// Cached value for `key` if present and younger than `ttl` seconds; `None`
/// otherwise. Callers gate this on `--no-cache` themselves.
pub fn get<T: DeserializeOwned>(key: &str, ttl: u64) -> Option<T> {
    read_fresh(&path_for(key), ttl, now_secs())
}

/// Persist `value` under `key`, stamped with the current time. Best-effort.
/// Callers skip this for empty/failed fetches so a transient miss never
/// poisons the cache.
pub fn put<T: Serialize>(key: &str, value: &T) {
    write_at(&path_for(key), value, now_secs());
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
        let _ = std::fs::remove_file(path_for(key));
        assert!(get::<Vec<i64>>(key, 600).is_none());
        put(key, &vec![10i64, 20, 30]);
        assert_eq!(get::<Vec<i64>>(key, 600), Some(vec![10, 20, 30]));
        let _ = std::fs::remove_file(path_for(key));
    }
}
