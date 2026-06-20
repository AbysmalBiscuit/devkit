use anyhow::{Context, Result};
use devkit_common::paths;
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Role { Issue, Baseline }

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Issue => "issue",
            Role::Baseline => "baseline",
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub app: String,
    pub holder: String,
    pub role: Role,
    pub pid: Option<u32>,
    pub logfile: Option<PathBuf>,
    pub ts: u64,
}

/// Current on-disk schema version. Bump when the layout changes in a way that
/// older binaries can't read; `read` migrates known older versions rather than
/// discarding the file (which would orphan live reservations).
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub entries: BTreeMap<u16, Entry>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn read(path: &std::path::Path) -> Data {
    let s = match fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return Data::default(),
    };
    match serde_json::from_str::<Data>(&s) {
        Ok(d) => d,
        // A parse failure usually means a schema change, not garbage. Salvage every
        // entry we can still understand rather than discarding live reservations.
        Err(_) => match salvage(&s) {
            Some(d) => {
                eprintln!("warning: registry schema differs; salvaged {} entr{}",
                    d.entries.len(), if d.entries.len() == 1 { "y" } else { "ies" });
                d
            }
            None => {
                let _ = fs::rename(path, path.with_extension("json.bak"));
                eprintln!("warning: unreadable registry; backed up and reinitialised");
                Data::default()
            }
        },
    }
}

/// Best-effort recovery: pull whatever entries still deserialize from a registry
/// whose top-level schema has drifted. Returns None only if there's no `entries`
/// object to recover at all.
fn salvage(s: &str) -> Option<Data> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let obj = v.get("entries")?.as_object()?;
    let mut entries = BTreeMap::new();
    for (k, val) in obj {
        if let (Ok(port), Ok(entry)) =
            (k.parse::<u16>(), serde_json::from_value::<Entry>(val.clone()))
        {
            entries.insert(port, entry);
        }
    }
    Some(Data { version: 0, entries })
}

fn write(path: &std::path::Path, data: &Data) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(data)?)?;
    fs::rename(&tmp, path).context("atomically replacing registry")?;
    Ok(())
}

/// Run `f` while holding the exclusive registry lock; persists the mutated `Data`.
pub fn with_lock<T>(f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
    fs::create_dir_all(paths::state_dir())?;
    let lock_path = paths::lock_file();
    let _ = OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
    let mut lock = RwLock::new(File::open(&lock_path)?);
    let _guard = lock.write()?; // blocks until exclusive
    let reg = paths::registry_file();
    let mut data = read(&reg);
    let out = f(&mut data)?;
    data.version = SCHEMA_VERSION;
    write(&reg, &data)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_serde() {
        let mut d = Data::default();
        d.entries.insert(9100, Entry { app: "api".into(), holder: "/w".into(), role: Role::Issue, pid: None, logfile: None, ts: 1 });
        let s = serde_json::to_string(&d).unwrap();
        let back: Data = serde_json::from_str(&s).unwrap();
        assert_eq!(back.entries[&9100].app, "api");
    }
}

use std::net::TcpListener;

/// True if something is bound to localhost:port (we could NOT bind it).
pub fn listening(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}
pub fn pid_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}
pub fn holder_alive(holder: &str) -> bool {
    std::path::Path::new(holder).exists()
}

#[cfg(test)]
mod liveness_tests {
    use super::*;
    #[test]
    fn detects_bound_port() {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(listening(port));   // l holds it
        drop(l);
        assert!(!listening(port));  // freed
    }
    #[test]
    fn current_pid_alive() {
        assert!(pid_alive(std::process::id()));
    }
}

/// How long a pid-less reservation survives without something listening on its port.
/// Must exceed `devrun`'s readiness timeout (120s) so a reservation cannot expire
/// while its server is still being brought up in the same run.
pub const RESERVATION_GRACE_SECS: u64 = 300;

impl Data {
    /// Ports whose entry is no longer live (holder gone, pid dead, or a stale
    /// unbacked reservation). Runs the liveness syscalls (stat/kill/bind); callers
    /// run this on a snapshot *outside* the registry lock so the exclusive lock
    /// never wraps blocking probes.
    pub fn dead_ports(&self) -> Vec<u16> {
        let now = now();
        self.entries
            .iter()
            .filter(|(port, e)| {
                let alive = holder_alive(&e.holder)
                    && match e.pid {
                        Some(pid) => pid_alive(pid),
                        None => {
                            listening(**port) || now.saturating_sub(e.ts) < RESERVATION_GRACE_SECS
                        }
                    };
                !alive
            })
            .map(|(port, _)| *port)
            .collect()
    }

    /// Drop entries whose holder is gone, pid is dead, or are stale unbacked reservations.
    pub fn prune(&mut self) {
        for port in self.dead_ports() {
            self.entries.remove(&port);
        }
    }

    fn holds(&self, holder: &str, app: &str, role: Role) -> Option<u16> {
        self.entries.iter()
            .find(|(_, e)| e.holder == holder && e.app == app && e.role == role)
            .map(|(p, _)| *p)
    }

    /// Reserve a port for one app (idempotent per holder+app+role). pid stays None.
    pub fn alloc_one(&mut self, holder: &str, app: &str, base: u16, role: Role) -> u16 {
        if let Some(p) = self.holds(holder, app, role) { return p; }
        let mut port = base;
        while self.entries.contains_key(&port) || listening(port) {
            port = port.checked_add(1)
                .unwrap_or_else(|| panic!("no free port available at or above {base}"));
        }
        self.entries.insert(port, Entry {
            app: app.into(), holder: holder.into(), role, pid: None, logfile: None, ts: now(),
        });
        port
    }

    /// Attach a pid + logfile to a port's reservation, re-establishing the row if it
    /// was pruned in the gap between reserving and spawning — so a live process is
    /// never left untracked (which would make `devrun down` unable to stop it).
    pub fn record_pid(&mut self, port: u16, app: &str, holder: &str, role: Role, pid: u32, logfile: PathBuf) {
        let e = self.entries.entry(port).or_insert_with(|| Entry {
            app: app.into(), holder: holder.into(), role, pid: None, logfile: None, ts: now(),
        });
        e.app = app.into();
        e.holder = holder.into();
        e.role = role;
        e.pid = Some(pid);
        e.logfile = Some(logfile);
    }

    /// Release all entries for a holder (optionally one role). Returns freed ports.
    pub fn release(&mut self, holder: &str, role: Option<Role>) -> Vec<u16> {
        let freed: Vec<u16> = self.entries.iter()
            .filter(|(_, e)| e.holder == holder && role.is_none_or(|r| e.role == r))
            .map(|(p, _)| *p).collect();
        for p in &freed { self.entries.remove(p); }
        freed
    }
}

/// Read the registry, pruning dead entries. Probes liveness *outside* the lock:
/// take a short read-lock, probe the snapshot unlocked, then commit any removals.
pub fn snapshot() -> Result<Data> {
    let data = with_lock(|d| Ok(d.clone()))?;
    let dead = data.dead_ports();
    if dead.is_empty() {
        return Ok(data);
    }
    with_lock(|d| {
        for p in &dead {
            d.entries.remove(p);
        }
        Ok(d.clone())
    })
}

/// Prune dead entries; returns the ports removed. Probes outside the lock.
pub fn prune() -> Result<Vec<u16>> {
    let data = with_lock(|d| Ok(d.clone()))?;
    let dead = data.dead_ports();
    if dead.is_empty() {
        return Ok(Vec::new());
    }
    with_lock(|d| {
        for p in &dead {
            d.entries.remove(p);
        }
        Ok(())
    })?;
    Ok(dead)
}

/// Reserve a port for each `(app, base_port)` request under `holder`+`role`.
///
/// The free-port search probes `listening()` *outside* the lock; the lock is then
/// held only for the cheap in-memory commit. If a chosen port was claimed in the
/// gap, that one app falls back to an in-lock probe (`alloc_one`) — rare, so the
/// common path keeps the exclusive lock free of blocking syscalls.
pub fn alloc(holder: &str, reqs: &[(String, u16)], role: Role) -> Result<Vec<(String, u16)>> {
    let mut data = snapshot()?;
    let mut chosen: Vec<(String, u16)> = Vec::with_capacity(reqs.len());
    for (app, base) in reqs {
        if let Some(p) = data.holds(holder, app, role) {
            chosen.push((app.clone(), p));
            continue;
        }
        let mut port = *base;
        loop {
            let taken =
                data.entries.contains_key(&port) || chosen.iter().any(|(_, p)| *p == port);
            if !taken && !listening(port) {
                break;
            }
            port = port
                .checked_add(1)
                .unwrap_or_else(|| panic!("no free port available at or above {base}"));
        }
        // Tentatively reserve locally so later requests in this call don't collide.
        data.entries.insert(
            port,
            Entry { app: app.clone(), holder: holder.into(), role, pid: None, logfile: None, ts: now() },
        );
        chosen.push((app.clone(), port));
    }

    with_lock(|d| {
        let mut out = Vec::with_capacity(chosen.len());
        for (app, port) in &chosen {
            if let Some(p) = d.holds(holder, app, role) {
                out.push((app.clone(), p));
            } else if d.entries.contains_key(port) {
                let base = reqs.iter().find(|(a, _)| a == app).map(|(_, b)| *b).unwrap_or(*port);
                let p = d.alloc_one(holder, app, base, role);
                out.push((app.clone(), p));
            } else {
                d.entries.insert(
                    *port,
                    Entry { app: app.clone(), holder: holder.into(), role, pid: None, logfile: None, ts: now() },
                );
                out.push((app.clone(), *port));
            }
        }
        Ok(out)
    })
}

/// Attach a pid + logfile to a reservation (re-establishing it if pruned).
pub fn record_pid(
    port: u16, app: &str, holder: &str, role: Role, pid: u32, logfile: PathBuf,
) -> Result<()> {
    with_lock(|d| {
        d.record_pid(port, app, holder, role, pid, logfile);
        Ok(())
    })
}

/// Release all entries for `holder` (optionally one role); returns freed ports.
pub fn release(holder: &str, role: Option<Role>) -> Result<Vec<u16>> {
    with_lock(|d| Ok(d.release(holder, role)))
}

/// Render the port-status table shared by `portman status` and `devrun status`.
/// `only_holder = Some(h)` limits rows to that holder; `None` shows every port.
pub fn status_table(data: &Data, only_holder: Option<&str>) -> String {
    let mut t = devkit_common::ui::table(&["PORT", "APP", "ROLE", "HOLDER", "PID", "LISTENING", "AGE"]);
    let now = now();
    for (port, e) in &data.entries {
        if let Some(h) = only_holder
            && e.holder != h
        {
            continue;
        }
        let label = devkit_common::paths::leaf(&e.holder).unwrap_or(&e.holder);
        t.add_row(vec![
            port.to_string(),
            e.app.clone(),
            e.role.to_string(),
            label.to_string(),
            e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            if listening(*port) { "yes".into() } else { "no".into() },
            format!("{}s", now.saturating_sub(e.ts)),
        ]);
    }
    format!("{t}")
}

#[cfg(test)]
mod ops_tests {
    use super::*;
    #[test]
    fn alloc_is_idempotent_per_holder() {
        let mut d = Data::default();
        let a = d.alloc_one("/w", "api", 9100, Role::Issue);
        let b = d.alloc_one("/w", "api", 9100, Role::Issue);
        assert_eq!(a, b);
        assert_eq!(d.entries.len(), 1);
    }
    #[test]
    fn alloc_skips_claimed_ports() {
        let mut d = Data::default();
        let a = d.alloc_one("/w1", "api", 9100, Role::Issue);
        let b = d.alloc_one("/w2", "api", 9100, Role::Issue);
        assert_ne!(a, b);
    }
    #[test]
    fn prune_drops_dead_holder() {
        let mut d = Data::default();
        d.entries.insert(9100, Entry { app:"api".into(), holder:"/definitely/not/here".into(), role:Role::Issue, pid:None, logfile:None, ts: now() });
        d.prune();
        assert!(d.entries.is_empty());
    }
    #[test]
    fn release_frees_by_holder() {
        let mut d = Data::default();
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        d.alloc_one(&cwd, "api", 9100, Role::Issue);
        let freed = d.release(&cwd, None);
        assert_eq!(freed.len(), 1);
        assert!(d.entries.is_empty());
    }
    #[test]
    fn record_pid_reestablishes_pruned_reservation() {
        // A pruned reservation must not leave a spawned process untracked: record_pid
        // re-inserts the row so `down` can still find and stop it.
        let mut d = Data::default();
        d.record_pid(9100, "api", "/w", Role::Issue, 4321, PathBuf::from("/log"));
        assert_eq!(d.entries[&9100].pid, Some(4321));
        assert_eq!(d.entries[&9100].app, "api");
        assert_eq!(d.entries[&9100].holder, "/w");
    }
    #[test]
    fn record_pid_updates_existing_reservation() {
        let mut d = Data::default();
        let port = d.alloc_one("/w", "api", 9100, Role::Issue);
        d.record_pid(port, "api", "/w", Role::Issue, 99, PathBuf::from("/log"));
        assert_eq!(d.entries.len(), 1);
        assert_eq!(d.entries[&port].pid, Some(99));
    }

    #[test]
    fn salvage_recovers_entries_from_drifted_schema() {
        // `version` as the wrong type makes the whole Data parse fail; the live
        // reservation under `entries` must still be recovered, not discarded.
        let json = r#"{"version":"oops","entries":{"9100":{"app":"api","holder":"/w","role":"issue","pid":4321,"logfile":null,"ts":1}}}"#;
        assert!(serde_json::from_str::<Data>(json).is_err());
        let d = salvage(json).expect("entries object present");
        assert_eq!(d.entries[&9100].pid, Some(4321));
        assert_eq!(d.entries[&9100].app, "api");
    }

    #[test]
    fn salvage_gives_up_without_entries_object() {
        assert!(salvage(r#"{"something":"else"}"#).is_none());
    }

    #[test]
    fn dead_ports_flags_dead_holder() {
        let mut d = Data::default();
        d.entries.insert(9100, Entry { app: "api".into(), holder: "/definitely/not/here".into(), role: Role::Issue, pid: None, logfile: None, ts: now() });
        assert_eq!(d.dead_ports(), vec![9100]);
    }
}
