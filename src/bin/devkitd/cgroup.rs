//! Daemon-side cgroup leaf orchestration: maps a supervised `Key` to a cgroup
//! leaf under the daemon's delegated base, and creates / removes / reconciles
//! those leaves. All operations are best-effort and fail-open — a cgroup error
//! degrades to an uncapped server, never a failed spawn.

use crate::supervisor::Key;
use crate::{CgroupCap, Daemon};
use devkit_ports::registry::Role;
use std::path::PathBuf;

/// FNV-1a 64-bit hash — stable across Rust versions and runs, unlike
/// `DefaultHasher`. Used to make leaf names collision-free without a dependency.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// A filesystem-safe leaf directory name for a supervised key. cgroup leaf names
/// may not contain `/`; holders are worktree paths, so every `/`, `\`, and `.` is
/// escaped to `_` and the role appended. The sanitized prefix is for readability;
/// the trailing 16-hex-digit FNV-1a hash of the raw key fields (separated by NUL)
/// guarantees that distinct keys always produce distinct names even when their
/// sanitized prefixes collide (e.g. holders `/a/b` and `/a_b` both sanitize to
/// `_a_b` but carry different hashes).
pub(crate) fn leaf_name(key: &Key) -> String {
    let san = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let role = match key.role {
        Role::Issue => "issue",
        Role::Baseline => "baseline",
    };
    // Hash the raw (un-sanitized) fields with NUL separators so that field
    // contents cannot forge a separator (e.g. a holder ending in `\0` appended
    // to an app string would otherwise shift the boundary).
    let mut raw = Vec::with_capacity(key.holder.len() + key.app.len() + role.len() + 2);
    raw.extend_from_slice(key.holder.as_bytes());
    raw.push(0);
    raw.extend_from_slice(key.app.as_bytes());
    raw.push(0);
    raw.extend_from_slice(role.as_bytes());
    format!(
        "{}__{}__{}__{:016x}",
        san(&key.holder),
        san(&key.app),
        role,
        fnv1a(&raw)
    )
}

impl Daemon {
    fn cap(&self) -> Option<&CgroupCap> {
        self.cgroup_cap.as_ref()
    }
}

/// Create (or reuse) the cgroup leaf for `key` and return its path. `None` when
/// hard caps are inactive, or when leaf creation fails (logged once) — the caller
/// then spawns uncapped.
pub(crate) fn leaf_for(daemon: &Daemon, key: &Key) -> Option<PathBuf> {
    let cap = daemon.cap()?;
    let name = leaf_name(key);
    match devkit_common::sys::cgroup_create_leaf(&cap.base, &name, cap.max_bytes) {
        Ok(leaf) => Some(leaf),
        Err(e) => {
            crate::log_line(&format!(
                "memory: could not create cgroup leaf for {}/{} ({:?}): {e:#} — running uncapped",
                key.holder, key.app, key.role
            ));
            None
        }
    }
}

/// Remove the cgroup leaf for `key` (best-effort; a non-empty or missing leaf is
/// ignored). Called when a server leaves supervision for good.
pub(crate) fn remove_leaf(daemon: &Daemon, key: &Key) {
    let Some(cap) = daemon.cap() else { return };
    let leaf = cap.base.join("servers").join(leaf_name(key));
    let _ = devkit_common::sys::cgroup_remove_leaf(&leaf);
}

/// Remove leaves under the base that don't correspond to a currently-live key —
/// clears leaves orphaned by a previous daemon's unclean exit.
pub(crate) fn reconcile(daemon: &Daemon, live: &[Key]) {
    let Some(cap) = daemon.cap() else { return };
    let keep: std::collections::HashSet<String> = live.iter().map(leaf_name).collect();
    for name in devkit_common::sys::cgroup_list_leaves(&cap.base) {
        if !keep.contains(&name) {
            let _ = devkit_common::sys::cgroup_remove_leaf(&cap.base.join("servers").join(&name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(holder: &str, app: &str, role: Role) -> Key {
        Key {
            holder: holder.into(),
            app: app.into(),
            role,
        }
    }

    #[test]
    fn leaf_name_is_filesystem_safe_and_distinct() {
        let a = leaf_name(&key("/home/ex/wt", "web", Role::Issue));
        assert!(!a.contains('/'), "no slashes in a leaf name: {a}");
        // The readable prefix is intact; the trailing hash makes names unique.
        assert!(
            a.starts_with("_home_ex_wt__web__issue__"),
            "expected readable prefix in: {a}"
        );
        // Role distinguishes otherwise-identical keys.
        let b = leaf_name(&key("/home/ex/wt", "web", Role::Baseline));
        assert_ne!(a, b);
        // App distinguishes.
        let c = leaf_name(&key("/home/ex/wt", "api", Role::Issue));
        assert_ne!(a, c);
        // Distinct holders that sanitize to the same prefix must still differ
        // (the trailing hash disambiguates).
        let p = leaf_name(&key("/a/b", "web", Role::Issue));
        let q = leaf_name(&key("/a_b", "web", Role::Issue));
        assert_ne!(p, q, "punctuation-only difference must not collide");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reconcile_removes_orphan_leaves() {
        // A temp dir standing in for a delegated cgroup base. Leaves are created
        // as empty dirs, modeling a process-free orphan cgroup — which is exactly
        // the removable state reconcile expects (rmdir succeeds; a real cgroup's
        // memory.* pseudo-files do not block rmdir, and a tmpfs dir has none).
        let base = std::env::temp_dir().join(format!("devkitd-cg-{}", crate::tests_unique()));
        let servers = base.join("servers");
        let live = key("/w", "api", Role::Issue);
        let orphan = key("/w", "ghost", Role::Issue);
        std::fs::create_dir_all(servers.join(leaf_name(&live))).unwrap();
        std::fs::create_dir_all(servers.join(leaf_name(&orphan))).unwrap();
        let d = crate::test_daemon_with_base(base.clone(), 1 << 30);
        reconcile(&d, std::slice::from_ref(&live));
        let left = devkit_common::sys::cgroup_list_leaves(&base);
        assert!(left.contains(&leaf_name(&live)), "live leaf kept");
        assert!(!left.contains(&leaf_name(&orphan)), "orphan leaf removed");
        let _ = std::fs::remove_dir_all(&base);
    }
}
