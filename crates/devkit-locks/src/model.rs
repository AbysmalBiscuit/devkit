use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// On-disk schema version. Bump when the layout changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    /// Project-root-relative, normalized ('/'-separated; root itself is ".").
    pub path: String,
    /// Absolute project root the path is relative to.
    pub root: String,
    /// Resolved session identity holding the lock.
    pub holder: String,
    /// Durable anchor pid, when one could be trusted (None for agent sessions).
    #[serde(default)]
    pub pid: Option<u32>,
    /// Optional human-readable intent.
    #[serde(default)]
    pub note: Option<String>,
    /// Acquired/renewed at, unix seconds.
    pub ts: u64,
    /// Time-to-live in seconds; 0 means no expiry.
    pub ttl: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub locks: BTreeMap<String, LockEntry>,
}

/// Registry key for a claim: root and path joined by a NUL (never valid in a path).
pub fn key_for(root: &str, path: &str) -> String {
    format!("{root}\u{0}{path}")
}

/// True if two root-relative normalized paths overlap: equal, or one is a
/// path-component ancestor of the other. "." (the project root) overlaps everything.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    if a == b || a == "." || b == "." {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    long.strip_prefix(short)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// True if `existing` is the same holder as `writer`, or an ancestor of it in the
/// agent tree. Holder ids are '/'-separated segments (`session`, `session/agent`);
/// `existing` is an ancestor of `writer` when it is a leading segment-boundary prefix.
pub fn is_ancestor_or_self(existing: &str, writer: &str) -> bool {
    existing == writer
        || writer
            .strip_prefix(existing)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acquired {
    pub path: String,
    pub ttl_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub path: String,
    pub held_by: String,
    pub age_secs: u64,
    pub note: Option<String>,
}

/// Outcome of evaluating a single write against the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteDecision {
    /// File was free; a fresh lock was taken for the writer.
    Acquired,
    /// Writer already owns the file (self or an ancestor holds an overlapping lock).
    AllowedByOwnership,
    /// Blocked: live overlapping locks held by non-ancestors.
    Denied(Vec<Conflict>),
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AcquireOutcome {
    pub acquired: Vec<Acquired>,
    pub conflicts: Vec<Conflict>,
}

/// True if a process with this pid currently exists (signal 0 probe).
pub fn pid_alive(pid: u32) -> bool {
    devkit_common::sys::process_alive(pid)
}

/// A lock is dead when its TTL has lapsed, or its anchor pid is known and gone.
pub fn entry_dead(e: &LockEntry, now: u64) -> bool {
    if e.ttl != 0 && now.saturating_sub(e.ts) >= e.ttl {
        return true;
    }
    match e.pid {
        Some(pid) => !pid_alive(pid),
        None => false,
    }
}

impl Data {
    /// Remove every dead lock; returns how many were dropped.
    pub fn prune_dead(&mut self, now: u64) -> usize {
        let before = self.locks.len();
        self.locks.retain(|_, e| !entry_dead(e, now));
        before - self.locks.len()
    }

    /// Conflicts that would block acquiring `paths` for `holder` in `root`:
    /// any live lock by a *different* holder whose path overlaps a requested path.
    pub fn check(&self, root: &str, paths: &[String], holder: &str, now: u64) -> Vec<Conflict> {
        let mut out = Vec::new();
        for req in paths {
            for e in self.locks.values() {
                if e.root == root
                    && e.holder != holder
                    && !entry_dead(e, now)
                    && paths_overlap(&e.path, req)
                {
                    out.push(Conflict {
                        path: req.clone(),
                        held_by: e.holder.clone(),
                        age_secs: now.saturating_sub(e.ts),
                        note: e.note.clone(),
                    });
                }
            }
        }
        out
    }

    /// All-or-nothing acquire: if any requested path conflicts, acquire none and
    /// return the conflicts. Otherwise insert (or renew, for the same holder+path).
    #[allow(clippy::too_many_arguments)]
    pub fn try_acquire(
        &mut self,
        root: &str,
        paths: &[String],
        holder: &str,
        pid: Option<u32>,
        note: Option<&str>,
        ttl: u64,
        now: u64,
    ) -> AcquireOutcome {
        let conflicts = self.check(root, paths, holder, now);
        if !conflicts.is_empty() {
            return AcquireOutcome {
                acquired: Vec::new(),
                conflicts,
            };
        }
        let mut acquired = Vec::with_capacity(paths.len());
        for req in paths {
            self.locks.insert(
                key_for(root, req),
                LockEntry {
                    path: req.clone(),
                    root: root.into(),
                    holder: holder.into(),
                    pid,
                    note: note.map(str::to_string),
                    ts: now,
                    ttl,
                },
            );
            acquired.push(Acquired {
                path: req.clone(),
                ttl_secs: ttl,
            });
        }
        AcquireOutcome {
            acquired,
            conflicts: Vec::new(),
        }
    }

    /// Release named paths held by `holder` in `root`. Without `force`, a path held
    /// by another holder is refused (not freed). Returns (released, refused).
    pub fn do_release(
        &mut self,
        root: &str,
        paths: &[String],
        holder: &str,
        force: bool,
    ) -> (Vec<String>, Vec<String>) {
        let mut released = Vec::new();
        let mut refused = Vec::new();
        for req in paths {
            let key = key_for(root, req);
            match self.locks.get(&key) {
                Some(e) if e.holder == holder || force => {
                    self.locks.remove(&key);
                    released.push(req.clone());
                }
                Some(_) => refused.push(req.clone()),
                None => {}
            }
        }
        (released, refused)
    }

    /// Keys of every dead lock (TTL lapsed, or anchor pid known-gone), without
    /// mutating. Callers persist removals separately so liveness probes stay out
    /// of the write path's critical section.
    pub fn dead_keys(&self, now: u64) -> Vec<String> {
        self.locks
            .iter()
            .filter(|(_, e)| entry_dead(e, now))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Release every lock held by `holder` in `root`; returns the freed paths.
    pub fn release_all(&mut self, root: &str, holder: &str) -> Vec<String> {
        let freed: Vec<String> = self
            .locks
            .values()
            .filter(|e| e.root == root && e.holder == holder)
            .map(|e| e.path.clone())
            .collect();
        for p in &freed {
            self.locks.remove(&key_for(root, p));
        }
        freed
    }

    /// Live overlapping locks for `path` in `root` whose holder is NOT an
    /// ancestor-or-self of `writer` — the locks that block the write.
    pub fn write_blockers(&self, root: &str, path: &str, writer: &str, now: u64) -> Vec<Conflict> {
        let mut out = Vec::new();
        for e in self.locks.values() {
            if e.root == root
                && !entry_dead(e, now)
                && paths_overlap(&e.path, path)
                && !is_ancestor_or_self(&e.holder, writer)
            {
                out.push(Conflict {
                    path: path.to_string(),
                    held_by: e.holder.clone(),
                    age_secs: now.saturating_sub(e.ts),
                    note: e.note.clone(),
                });
            }
        }
        out
    }

    /// Decide a write and, only when the file is free, take a lock for `writer`.
    /// Mutates self solely in the `Acquired` case (insert) or to renew the writer's
    /// own exact-path lock; an ancestor's lock is never overwritten.
    #[allow(clippy::too_many_arguments)]
    pub fn decide_write(
        &mut self,
        root: &str,
        path: &str,
        writer: &str,
        pid: Option<u32>,
        note: Option<&str>,
        ttl: u64,
        now: u64,
    ) -> WriteDecision {
        let blockers = self.write_blockers(root, path, writer, now);
        if !blockers.is_empty() {
            return WriteDecision::Denied(blockers);
        }
        let overlaps = self.locks.values().any(|e| {
            e.root == root && !entry_dead(e, now) && paths_overlap(&e.path, path)
        });
        if overlaps {
            // Held only by self or an ancestor. Renew the writer's own exact lock if present.
            if let Some(e) = self.locks.get_mut(&key_for(root, path))
                && e.holder == writer
            {
                e.ts = now;
            }
            return WriteDecision::AllowedByOwnership;
        }
        self.locks.insert(
            key_for(root, path),
            LockEntry {
                path: path.to_string(),
                root: root.to_string(),
                holder: writer.to_string(),
                pid,
                note: note.map(str::to_string),
                ts: now,
                ttl,
            },
        );
        WriteDecision::Acquired
    }

    /// Release every lock in `root` whose holder is `prefix` or a descendant
    /// (`prefix/…`). Used by SubagentStop (`session/agent`) and SessionEnd (`session`).
    pub fn release_prefix(&mut self, root: &str, prefix: &str) -> Vec<String> {
        let freed: Vec<String> = self
            .locks
            .values()
            .filter(|e| e.root == root && is_ancestor_or_self(prefix, &e.holder))
            .map(|e| e.path.clone())
            .collect();
        for p in &freed {
            self.locks.remove(&key_for(root, p));
        }
        freed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_joins_root_and_path() {
        assert_eq!(key_for("/repo", "scenes/x"), "/repo\u{0}scenes/x");
    }
    #[test]
    fn overlap_equal_paths() {
        assert!(paths_overlap("scenes/x", "scenes/x"));
    }
    #[test]
    fn overlap_ancestor_either_direction() {
        assert!(paths_overlap("scenes", "scenes/player.tscn"));
        assert!(paths_overlap("scenes/player.tscn", "scenes"));
    }
    #[test]
    fn no_overlap_sibling_prefix() {
        assert!(!paths_overlap("scenes", "scenes-old"));
        assert!(!paths_overlap("scenes/a", "scenes/b"));
    }
    #[test]
    fn root_dot_overlaps_everything() {
        assert!(paths_overlap(".", "scenes/x"));
        assert!(paths_overlap("scenes/x", "."));
        assert!(paths_overlap(".", "."));
    }
    #[test]
    fn roundtrip_serde() {
        let mut d = Data::default();
        d.locks.insert(
            key_for("/repo", "scenes"),
            LockEntry {
                path: "scenes".into(),
                root: "/repo".into(),
                holder: "alice".into(),
                pid: None,
                note: Some("refactor".into()),
                ts: 5,
                ttl: 1800,
            },
        );
        let s = serde_json::to_string(&d).unwrap();
        let back: Data = serde_json::from_str(&s).unwrap();
        assert_eq!(back.locks[&key_for("/repo", "scenes")].holder, "alice");
    }

    fn entry(
        root: &str,
        path: &str,
        holder: &str,
        ts: u64,
        ttl: u64,
        pid: Option<u32>,
    ) -> (String, LockEntry) {
        (
            key_for(root, path),
            LockEntry {
                path: path.into(),
                root: root.into(),
                holder: holder.into(),
                pid,
                note: None,
                ts,
                ttl,
            },
        )
    }

    #[test]
    fn acquire_inserts_and_is_idempotent_renew() {
        let mut d = Data::default();
        let r = d.try_acquire("/repo", &["scenes".into()], "alice", None, None, 1800, 100);
        assert!(r.conflicts.is_empty());
        assert_eq!(r.acquired.len(), 1);
        let r2 = d.try_acquire("/repo", &["scenes".into()], "alice", None, None, 1800, 250);
        assert!(r2.conflicts.is_empty());
        assert_eq!(d.locks.len(), 1);
        assert_eq!(d.locks[&key_for("/repo", "scenes")].ts, 250);
    }

    #[test]
    fn acquire_conflicts_for_other_holder_overlap() {
        let mut d = Data::default();
        let (k, e) = entry("/repo", "scenes", "alice", 100, 1800, None);
        d.locks.insert(k, e);
        let r = d.try_acquire(
            "/repo",
            &["scenes/player.tscn".into()],
            "bob",
            None,
            None,
            1800,
            140,
        );
        assert_eq!(r.acquired.len(), 0);
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].held_by, "alice");
        assert_eq!(r.conflicts[0].age_secs, 40);
    }

    #[test]
    fn acquire_is_all_or_nothing() {
        let mut d = Data::default();
        let (k, e) = entry("/repo", "scenes", "alice", 100, 1800, None);
        d.locks.insert(k, e);
        let r = d.try_acquire(
            "/repo",
            &["art".into(), "scenes/x".into()],
            "bob",
            None,
            None,
            1800,
            120,
        );
        assert!(r.acquired.is_empty(), "no path acquired when any conflicts");
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(d.locks.len(), 1, "nothing new inserted");
    }

    #[test]
    fn other_root_never_conflicts() {
        let mut d = Data::default();
        let (k, e) = entry("/repoA", "scenes", "alice", 100, 1800, None);
        d.locks.insert(k, e);
        let r = d.try_acquire("/repoB", &["scenes".into()], "bob", None, None, 1800, 120);
        assert!(r.conflicts.is_empty());
        assert_eq!(r.acquired.len(), 1);
    }

    #[test]
    fn prune_drops_ttl_expired_keeps_live() {
        let mut d = Data::default();
        d.locks.extend([
            entry("/repo", "old", "alice", 100, 60, None),
            entry("/repo", "fresh", "alice", 990, 60, None),
            entry("/repo", "forever", "alice", 1, 0, None),
        ]);
        let removed = d.prune_dead(1000);
        assert_eq!(removed, 1);
        assert!(d.locks.contains_key(&key_for("/repo", "fresh")));
        assert!(d.locks.contains_key(&key_for("/repo", "forever")));
    }

    #[test]
    fn prune_drops_dead_pid() {
        let mut d = Data::default();
        d.locks
            .extend([entry("/repo", "p", "alice", 1, 0, Some(u32::MAX))]);
        assert_eq!(d.prune_dead(2), 1);
    }

    #[test]
    fn release_by_holder_and_force() {
        let mut d = Data::default();
        d.locks.extend([
            entry("/repo", "a", "alice", 1, 0, None),
            entry("/repo", "b", "bob", 1, 0, None),
        ]);
        let (rel, refused) = d.do_release("/repo", &["b".into()], "alice", false);
        assert!(rel.is_empty());
        assert_eq!(refused, vec!["b".to_string()]);
        let (rel, _) = d.do_release("/repo", &["a".into()], "alice", false);
        assert_eq!(rel, vec!["a".to_string()]);
        let (rel, _) = d.do_release("/repo", &["b".into()], "alice", true);
        assert_eq!(rel, vec!["b".to_string()]);
        assert!(d.locks.is_empty());
    }

    #[test]
    fn release_all_clears_only_callers_locks_in_root() {
        let mut d = Data::default();
        d.locks.extend([
            entry("/repo", "a", "alice", 1, 0, None),
            entry("/repo", "b", "bob", 1, 0, None),
            entry("/other", "c", "alice", 1, 0, None),
        ]);
        let rel = d.release_all("/repo", "alice");
        assert_eq!(rel, vec!["a".to_string()]);
        assert!(d.locks.contains_key(&key_for("/repo", "b")));
        assert!(d.locks.contains_key(&key_for("/other", "c")));
    }

    #[test]
    fn dead_keys_lists_ttl_and_pid_dead() {
        let mut d = Data::default();
        d.locks.extend([
            entry("/repo", "old", "alice", 100, 60, None), // ttl-expired by now=1000
            entry("/repo", "fresh", "alice", 990, 60, None), // live
            entry("/repo", "deadpid", "alice", 1, 0, Some(u32::MAX)), // dead pid
        ]);
        let mut got = d.dead_keys(1000);
        got.sort();
        assert_eq!(
            got,
            vec![key_for("/repo", "deadpid"), key_for("/repo", "old")]
        );
    }

    #[test]
    fn ancestry_self_and_prefix() {
        assert!(is_ancestor_or_self("S", "S"));            // self
        assert!(is_ancestor_or_self("S", "S/a1"));         // parent of sub-agent
        assert!(is_ancestor_or_self("S/a1", "S/a1/b2"));   // grandparent (if ever nested)
        assert!(!is_ancestor_or_self("S/a1", "S"));        // child does not own parent
        assert!(!is_ancestor_or_self("S/a1", "S/b2"));     // siblings contend
        assert!(!is_ancestor_or_self("S", "Sx"));          // not a segment boundary
        assert!(!is_ancestor_or_self("S", "T"));           // unrelated sessions
    }

    #[test]
    fn decide_write_free_acquires() {
        let mut d = Data::default();
        let r = d.decide_write("/repo", "src/a.rs", "S", None, Some("write-harness"), 1800, 100);
        assert_eq!(r, WriteDecision::Acquired);
        assert_eq!(d.locks[&key_for("/repo", "src/a.rs")].holder, "S");
    }

    #[test]
    fn decide_write_self_is_allowed_and_renews() {
        let mut d = Data::default();
        d.decide_write("/repo", "src/a.rs", "S", None, None, 1800, 100);
        let r = d.decide_write("/repo", "src/a.rs", "S", None, None, 1800, 250);
        assert_eq!(r, WriteDecision::AllowedByOwnership);
        assert_eq!(d.locks.len(), 1);
        assert_eq!(d.locks[&key_for("/repo", "src/a.rs")].ts, 250); // renewed
    }

    #[test]
    fn decide_write_ancestor_allowed_without_clobber() {
        let mut d = Data::default();
        // parent S holds the directory; child S/a1 writes a file under it
        d.decide_write("/repo", "src", "S", None, None, 1800, 100);
        let r = d.decide_write("/repo", "src/a.rs", "S/a1", None, None, 1800, 120);
        assert_eq!(r, WriteDecision::AllowedByOwnership);
        // the parent's lock is untouched; no new row inserted for the child
        assert_eq!(d.locks.len(), 1);
        assert_eq!(d.locks[&key_for("/repo", "src")].holder, "S");
    }

    #[test]
    fn decide_write_sibling_denied() {
        let mut d = Data::default();
        d.decide_write("/repo", "src/a.rs", "S/a1", None, None, 1800, 100);
        let r = d.decide_write("/repo", "src/a.rs", "S/b2", None, None, 1800, 140);
        match r {
            WriteDecision::Denied(c) => {
                assert_eq!(c.len(), 1);
                assert_eq!(c[0].held_by, "S/a1");
                assert_eq!(c[0].age_secs, 40);
            }
            other => panic!("expected Denied, got {other:?}"),
        }
        assert_eq!(d.locks.len(), 1); // nothing acquired for the loser
    }

    #[test]
    fn decide_write_other_session_denied() {
        let mut d = Data::default();
        d.decide_write("/repo", "src/a.rs", "S", None, None, 1800, 100);
        let r = d.decide_write("/repo", "src/a.rs", "T", None, None, 1800, 110);
        assert!(matches!(r, WriteDecision::Denied(_)));
    }

    #[test]
    fn release_prefix_frees_session_subtree_only() {
        let mut d = Data::default();
        d.decide_write("/repo", "a", "S", None, None, 1800, 1);
        d.decide_write("/repo", "b", "S/a1", None, None, 1800, 1);
        d.decide_write("/repo", "c", "T", None, None, 1800, 1);
        let freed = d.release_prefix("/repo", "S");
        assert_eq!(freed.len(), 2);                 // S and S/a1
        assert!(d.locks.contains_key(&key_for("/repo", "c"))); // T survives
    }

    #[test]
    fn release_prefix_single_subagent() {
        let mut d = Data::default();
        d.decide_write("/repo", "a", "S", None, None, 1800, 1);
        d.decide_write("/repo", "b", "S/a1", None, None, 1800, 1);
        let freed = d.release_prefix("/repo", "S/a1");
        assert_eq!(freed, vec!["b".to_string()]);   // only the sub-agent's lock
        assert!(d.locks.contains_key(&key_for("/repo", "a")));
    }
}
