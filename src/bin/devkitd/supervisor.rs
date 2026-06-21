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
            },
        );
    }

    pub(crate) fn remove(&mut self, key: &Key) -> Option<u32> {
        self.children.remove(key).map(|c| c.pid)
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

    /// Update a key's pid after a successful respawn; marks the child as owned.
    pub(crate) fn set_pid(&mut self, key: &Key, pid: u32) {
        if let Some(c) = self.children.get_mut(key) {
            c.pid = pid;
            c.watch = Watch::Owned;
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
    /// the keys whose process is now gone (the caller decides restart vs. let-die by
    /// consulting `ports.json`).
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
