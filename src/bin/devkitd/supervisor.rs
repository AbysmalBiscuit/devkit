use devkit_common::supervise::tree_rss_bytes;
use devkit_ports::registry::{self, Role};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Identity of a supervised server, matching its `ports.json` row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub(crate) holder: String,
    pub(crate) app: String,
    pub(crate) role: Role,
}

/// Everything needed to respawn a process after a crash. Owned children store
/// this; adopted survivors carry `None` and are dropped when they exit, since the
/// daemon never captured how to launch them.
#[derive(Clone)]
pub(crate) struct Launch {
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: String,
    pub(crate) env: std::collections::BTreeMap<String, String>,
}

/// How the daemon watches a process: `Owned` children are reaped with `waitpid`;
/// `Adopted` survivors (from a previous daemon) are polled with `pid_alive`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Watch {
    Owned,
    Adopted,
}

struct Child {
    pid: u32,
    port: u16,
    logfile: PathBuf,
    watch: Watch,
    restarts: Vec<Instant>,
    warned_mem: bool,
    launch: Option<Launch>,
    /// Has this process accepted a health probe at least once? Until it has, probe
    /// failures are ignored, so a slow-starting server is never judged hung.
    armed: bool,
    /// Consecutive failed probes since arming or the last success.
    probe_failures: u32,
}

pub(crate) struct Supervisor {
    children: HashMap<Key, Child>,
    max_restarts: u32,
    window: Duration,
    mem_warn: u64,
    mem_limit: u64,
}

impl Supervisor {
    pub(crate) fn new(max_restarts: u32, window: Duration, mem_warn: u64, mem_limit: u64) -> Self {
        Supervisor {
            children: HashMap::new(),
            max_restarts,
            window,
            mem_warn,
            mem_limit,
        }
    }

    pub(crate) fn any_live(&self) -> bool {
        !self.children.is_empty()
    }

    pub(crate) fn insert_owned(
        &mut self,
        key: Key,
        pid: u32,
        port: u16,
        logfile: PathBuf,
        launch: Launch,
    ) {
        self.children.insert(
            key,
            Child {
                pid,
                port,
                logfile,
                watch: Watch::Owned,
                restarts: Vec::new(),
                warned_mem: false,
                launch: Some(launch),
                armed: false,
                probe_failures: 0,
            },
        );
    }

    pub(crate) fn insert_adopted(&mut self, key: Key, pid: u32, port: u16, logfile: PathBuf) {
        self.children.insert(
            key,
            Child {
                pid,
                port,
                logfile,
                watch: Watch::Adopted,
                restarts: Vec::new(),
                warned_mem: false,
                launch: None,
                armed: false,
                probe_failures: 0,
            },
        );
    }

    pub(crate) fn remove(&mut self, key: &Key) -> Option<u32> {
        self.children.remove(key).map(|c| c.pid)
    }

    /// Whether a key is still tracked. Lets the restart path tell a concurrent
    /// `Down`/give-up (key gone) apart from an adopted survivor with no launch
    /// spec (key present) when deciding what to do with a reaped child.
    pub(crate) fn contains(&self, key: &Key) -> bool {
        self.children.contains_key(key)
    }

    pub(crate) fn logfile_of(&self, key: &Key) -> Option<PathBuf> {
        self.children.get(key).map(|c| c.logfile.clone())
    }

    /// Launch spec, logfile, and port for respawning a key. Only owned children
    /// (those with a stored launch spec) can be respawned.
    pub(crate) fn launch_of(&self, key: &Key) -> Option<(Launch, PathBuf, u16)> {
        let c = self.children.get(key)?;
        Some((c.launch.clone()?, c.logfile.clone(), c.port))
    }

    /// Update a key's pid after a successful respawn; marks the child as owned and
    /// disarms its health probe — a fresh process must re-prove readiness before it
    /// can be judged hung.
    pub(crate) fn set_pid(&mut self, key: &Key, pid: u32) {
        if let Some(c) = self.children.get_mut(key) {
            c.pid = pid;
            c.watch = Watch::Owned;
            c.armed = false;
            c.probe_failures = 0;
        }
    }

    /// Record a restart attempt against the crash-loop budget; returns whether one
    /// is still allowed in the current window. Shared by crash- and memory-triggered
    /// restarts so a server can't be restart-looped forever. Only a supervised child
    /// has a budget — an unknown key returns `false` rather than creating phantom state.
    pub(crate) fn may_restart(&mut self, holder: &str, app: &str, role: Role) -> bool {
        let key = Key {
            holder: holder.into(),
            app: app.into(),
            role,
        };
        let now = Instant::now();
        let window = self.window;
        let Some(entry) = self.children.get_mut(&key) else {
            return false;
        };
        entry.restarts.retain(|t| now.duration_since(*t) < window);
        if (entry.restarts.len() as u32) < self.max_restarts {
            entry.restarts.push(now);
            true
        } else {
            false
        }
    }

    /// Reap any exited `Owned` children and detect any dead `Adopted` ones. Returns
    /// the keys whose process is now gone. Every returned key is a crash: an intentional
    /// `Down` removes the key from the table before signalling its child, so a stopped
    /// server is never reaped here.
    pub(crate) fn reap_once(&mut self) -> Vec<Key> {
        let mut dead = Vec::new();
        for (key, child) in self.children.iter() {
            if child.pid == 0 {
                continue;
            }
            let gone = match child.watch {
                Watch::Owned => devkit_common::sys::reap_owned(child.pid),
                Watch::Adopted => !registry::pid_alive(child.pid),
            };
            if gone {
                dead.push(key.clone());
            }
        }
        dead
    }

    /// Owned children eligible for health probing: respawnable (a launch spec) and
    /// with a live pid. Adopted survivors and pid-less reservations are excluded — a
    /// probe restart needs a launch spec to respawn from.
    pub(crate) fn probe_targets(&self) -> Vec<(Key, u16)> {
        self.children
            .iter()
            .filter(|(_, c)| c.launch.is_some() && c.pid != 0)
            .map(|(k, c)| (k.clone(), c.port))
            .collect()
    }

    /// Fold one probe result into a child's health state. A successful connect arms
    /// the child and clears its failure run; a failure on an armed child grows the
    /// consecutive-failure count. Returns the pid to SIGTERM once that count reaches
    /// `threshold` — resetting the count in the same call, so a hung child is
    /// signalled once per K-failure run rather than every cycle. Returns `None` for a
    /// child below threshold, an unarmed child, or a key removed since the snapshot.
    pub(crate) fn record_probe(&mut self, key: &Key, ok: bool, threshold: u32) -> Option<u32> {
        let c = self.children.get_mut(key)?;
        if ok {
            c.armed = true;
            c.probe_failures = 0;
            return None;
        }
        if !c.armed {
            return None;
        }
        c.probe_failures += 1;
        if c.probe_failures >= threshold {
            c.probe_failures = 0;
            Some(c.pid)
        } else {
            None
        }
    }

    /// Memory breaches to act on this tick: returns `(Key, bytes)` for each child
    /// whose supervised process-tree RSS crosses `mem_warn`. Each child warns once
    /// per breach (re-armed when it drops back below the threshold).
    pub(crate) fn memory_breaches(&mut self) -> Vec<(Key, u64)> {
        if self.mem_warn == 0 {
            return Vec::new();
        }
        let mut breaches = Vec::new();
        for (key, child) in self.children.iter_mut() {
            if child.pid == 0 {
                continue;
            }
            let rss = tree_rss_bytes(child.pid);
            if rss >= self.mem_warn {
                if !child.warned_mem {
                    child.warned_mem = true;
                    breaches.push((key.clone(), rss));
                }
            } else {
                child.warned_mem = false;
            }
        }
        breaches
    }

    pub(crate) fn mem_limit(&self) -> u64 {
        self.mem_limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sup() -> Supervisor {
        Supervisor::new(2, Duration::from_secs(60), 0, 0)
    }

    fn live(s: &mut Supervisor, app: &str, pid: u32, port: u16) {
        let key = Key {
            holder: "/w".into(),
            app: app.into(),
            role: Role::Issue,
        };
        s.insert_owned(
            key,
            pid,
            port,
            PathBuf::new(),
            Launch {
                argv: vec!["true".into()],
                cwd: ".".into(),
                env: std::collections::BTreeMap::new(),
            },
        );
    }

    #[test]
    fn restart_budget_blocks_after_max() {
        let mut s = sup();
        live(&mut s, "api", 1, 9100);
        assert!(s.may_restart("/w", "api", Role::Issue)); // 1
        assert!(s.may_restart("/w", "api", Role::Issue)); // 2
        assert!(!s.may_restart("/w", "api", Role::Issue)); // exhausted (max=2)
    }

    #[test]
    fn unknown_key_has_no_restart_budget() {
        let mut s = sup();
        assert!(!s.may_restart("/w", "ghost", Role::Issue));
    }

    #[test]
    fn restart_budget_is_per_child() {
        let mut s = sup();
        live(&mut s, "api", 1, 9100);
        live(&mut s, "lab-os", 2, 9200);
        assert!(s.may_restart("/w", "api", Role::Issue));
        assert!(s.may_restart("/w", "api", Role::Issue));
        assert!(s.may_restart("/w", "lab-os", Role::Issue)); // different child, own budget
    }

    fn key_for(app: &str) -> Key {
        Key {
            holder: "/w".into(),
            app: app.into(),
            role: Role::Issue,
        }
    }

    #[test]
    fn probe_failures_before_arming_are_ignored() {
        let mut s = sup();
        live(&mut s, "api", 1, 9100);
        let k = key_for("api");
        assert_eq!(s.record_probe(&k, false, 2), None);
        assert_eq!(s.record_probe(&k, false, 2), None);
        assert_eq!(s.record_probe(&k, false, 2), None);
    }

    #[test]
    fn arms_on_success_then_signals_on_threshold() {
        let mut s = sup();
        live(&mut s, "api", 7, 9100);
        let k = key_for("api");
        assert_eq!(s.record_probe(&k, true, 2), None); // arm
        assert_eq!(s.record_probe(&k, false, 2), None); // failure 1
        assert_eq!(s.record_probe(&k, false, 2), Some(7)); // failure 2 → signal pid
        assert_eq!(s.record_probe(&k, false, 2), None); // counter reset: fresh run
    }

    #[test]
    fn success_resets_failure_run() {
        let mut s = sup();
        live(&mut s, "api", 7, 9100);
        let k = key_for("api");
        s.record_probe(&k, true, 2); // arm
        s.record_probe(&k, false, 2); // failure 1
        assert_eq!(s.record_probe(&k, true, 2), None); // success resets the run
        assert_eq!(s.record_probe(&k, false, 2), None); // back to failure 1, not threshold
    }

    #[test]
    fn set_pid_redisarms() {
        let mut s = sup();
        live(&mut s, "api", 7, 9100);
        let k = key_for("api");
        s.record_probe(&k, true, 2); // armed
        s.set_pid(&k, 99); // respawn → disarm
        assert_eq!(s.record_probe(&k, false, 2), None); // ignored until re-armed
        assert_eq!(s.record_probe(&k, false, 2), None);
        assert_eq!(s.record_probe(&k, true, 2), None); // re-arm
        assert_eq!(s.record_probe(&k, false, 2), None);
        assert_eq!(s.record_probe(&k, false, 2), Some(99)); // threshold on the new pid
    }

    #[test]
    fn record_probe_on_missing_key_is_none() {
        let mut s = sup();
        let k = key_for("ghost");
        assert_eq!(s.record_probe(&k, false, 2), None);
        assert_eq!(s.record_probe(&k, true, 2), None);
    }

    #[test]
    fn probe_targets_includes_owned_excludes_adopted() {
        let mut s = sup();
        live(&mut s, "api", 7, 9100); // owned
        s.insert_adopted(key_for("legacy"), 8, 9200, PathBuf::new()); // adopted, no launch spec
        let targets = s.probe_targets();
        assert!(targets.iter().any(|(k, p)| k.app == "api" && *p == 9100));
        assert!(
            !targets.iter().any(|(k, _)| k.app == "legacy"),
            "adopted survivors must not be probed"
        );
    }

    #[test]
    fn reaps_a_real_child_and_records_exit() {
        let mut s = sup();
        // A child that exits immediately.
        let argv: Vec<String> = ["true"].iter().map(|x| x.to_string()).collect();
        let key = Key {
            holder: "/w".into(),
            app: "api".into(),
            role: Role::Issue,
        };
        let pid = devkit_common::supervise::spawn_detached(
            &argv,
            ".",
            &std::collections::BTreeMap::new(),
            &std::env::temp_dir().join("portd-test.log"),
        )
        .unwrap();
        s.insert_owned(
            key.clone(),
            pid,
            9100,
            std::env::temp_dir().join("portd-test.log"),
            Launch {
                argv: argv.clone(),
                cwd: ".".into(),
                env: std::collections::BTreeMap::new(),
            },
        );
        // Poll for the exit: a real `true` can take longer than a fixed sleep to
        // start and exit on a loaded CI runner. `reap_once` is non-mutating, so
        // repeating it until the child is gone is safe.
        let start = std::time::Instant::now();
        let reaped = loop {
            if s.reap_once().iter().any(|k| k == &key) {
                break true;
            }
            if start.elapsed() > Duration::from_secs(5) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(reaped, "child should be reaped");
    }
}
