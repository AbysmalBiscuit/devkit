//! Timestamp-gated `git fetch` to skip redundant network round-trips.
//!
//! Every devkit fetch feeds an immediate consumer — a `worktree add <ref>` or a
//! `reset --hard <ref>` that reads the just-fetched remote-tracking ref — so a
//! fetch can't simply be backgrounded without racing that follow-up. Instead
//! each fetch target (a repo path + remote) records when it last fetched under
//! `~/.cache/devkit/fetch/`; a fetch requested again within the TTL is skipped,
//! reusing the refs already on disk. The window is short, so the ref a branch is
//! cut from is at most `TTL` seconds stale.
//!
//! `DEVKIT_FETCH_TTL_SECS` overrides the window; `0` disables the gate (always
//! fetch). A fetch failure never stamps the marker, so the next call retries.

use crate::cmd::git;
use crate::paths;
use anyhow::Result;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_TTL_SECS: u64 = 60;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The freshness window in seconds. `DEVKIT_FETCH_TTL_SECS` overrides the
/// default; a malformed value falls back to the default; `0` disables the gate.
fn ttl_secs() -> u64 {
    match std::env::var("DEVKIT_FETCH_TTL_SECS") {
        Ok(v) => v.trim().parse().unwrap_or(DEFAULT_TTL_SECS),
        Err(_) => DEFAULT_TTL_SECS,
    }
}

/// Marker file for a `(cwd, remote)` fetch target, keyed by a hash of both so
/// two repos sharing a basename don't collide.
fn marker_path(cwd: &str, remote: &str) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut h);
    0u8.hash(&mut h); // separator so ("ab","c") != ("a","bc")
    remote.hash(&mut h);
    paths::cache_dir()
        .join("fetch")
        .join(format!("{:016x}", h.finish()))
}

/// Whether the marker at `path` still counts as fresh at `now`. `ttl == 0` is
/// never fresh (always fetch); a missing/malformed marker is never fresh.
fn is_fresh(path: &Path, ttl: u64, now: u64) -> bool {
    if ttl == 0 {
        return false;
    }
    let Ok(s) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(stamp) = s.trim().parse::<u64>() else {
        return false;
    };
    now.saturating_sub(stamp) <= ttl
}

/// Record `now` as the last-fetch time at `path`. Best-effort: a write failure
/// just means the next fetch isn't gated.
fn stamp(path: &Path, now: u64) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, now.to_string());
}

/// The gate, with the marker path, clock, and fetch action injected so it can be
/// unit-tested without a network or the real cache dir. Returns `Ok(true)` if it
/// fetched, `Ok(false)` if it skipped a still-fresh target. A failing `do_fetch`
/// propagates and leaves the marker untouched.
fn fetch_gated(
    marker: &Path,
    ttl: u64,
    now: u64,
    do_fetch: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    if is_fresh(marker, ttl, now) {
        return Ok(false);
    }
    do_fetch()?;
    stamp(marker, now);
    Ok(true)
}

/// `git fetch <remote>` in `cwd`, skipped when the same target was fetched
/// within the TTL. Returns `Ok(())` whether it fetched or reused a fresh fetch.
pub fn fetch(remote: &str, cwd: &str) -> Result<()> {
    let marker = marker_path(cwd, remote);
    fetch_gated(&marker, ttl_secs(), now_secs(), || {
        git(&["fetch", remote], cwd).map(|_| ())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn scratch(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("devkit-fetch-gate-{}-{tag}", std::process::id()))
    }

    #[test]
    fn skips_when_marker_is_fresh() {
        let p = scratch("fresh");
        let _ = std::fs::remove_file(&p);
        stamp(&p, 1000);
        let called = Cell::new(false);
        let fetched = fetch_gated(&p, 60, 1030, || {
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(!fetched, "within ttl → skipped");
        assert!(!called.get(), "fetch closure not invoked when fresh");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn fetches_when_marker_is_stale() {
        let p = scratch("stale");
        stamp(&p, 1000);
        let called = Cell::new(false);
        let fetched = fetch_gated(&p, 60, 1200, || {
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(fetched && called.get(), "past ttl → fetched");
        // marker advanced to `now`
        assert!(is_fresh(&p, 60, 1200));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn fetches_when_marker_missing() {
        let p = scratch("missing");
        let _ = std::fs::remove_file(&p);
        let fetched = fetch_gated(&p, 60, 5, || Ok(())).unwrap();
        assert!(fetched);
        assert!(p.exists(), "marker written after a fetch");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn ttl_zero_always_fetches() {
        let p = scratch("zero");
        stamp(&p, 1000);
        let called = Cell::new(false);
        let fetched = fetch_gated(&p, 0, 1001, || {
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(fetched && called.get(), "ttl 0 disables the gate");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn failed_fetch_leaves_no_marker() {
        let p = scratch("fail");
        let _ = std::fs::remove_file(&p);
        let r = fetch_gated(&p, 60, 5, || anyhow::bail!("boom"));
        assert!(r.is_err());
        assert!(
            !p.exists(),
            "no marker written on failure → next call retries"
        );
    }

    #[test]
    fn marker_path_distinguishes_targets() {
        let a = marker_path("/repo/one", "origin");
        let b = marker_path("/repo/two", "origin");
        let c = marker_path("/repo/one", "upstream");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, marker_path("/repo/one", "origin"));
    }
}
