use anyhow::Result;
use devkit_common::paths;
use devkit_common::store::{self, Document, salvage_map};
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Issue,
    Baseline,
}

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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl Document for Data {
    fn stamp_version(&mut self) {
        self.version = SCHEMA_VERSION;
    }
    /// Recover whatever port entries still deserialize from a registry whose
    /// top-level schema has drifted; `None` only if there's no `entries` object.
    fn salvage(raw: &str) -> Option<Self> {
        Some(Data {
            version: 0,
            entries: salvage_map(raw, "entries", |k| k.parse::<u16>().ok())?,
        })
    }
    fn label() -> &'static str {
        "registry"
    }
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// A driver for the registry read-modify-write cycle. `FlockStore` backs the
/// direct path; the daemon's `MemoryStore` (added later) backs in-memory state.
pub trait Store {
    /// Current registry state — a cheap read, no mutation.
    fn snapshot(&self) -> Result<Data>;
    /// Exclusive read-modify-write: run `f`, persist, return its value.
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T>;
}

/// Error marker: a live `devkitd` holds the registry write gate (`devkitd.lock`).
/// Carried via `anyhow` so callers can distinguish it (e.g. a best-effort prune).
#[derive(Debug)]
pub struct DaemonHoldsLock;

impl std::fmt::Display for DaemonHoldsLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "a devkitd daemon holds the registry lock; refusing to modify ports.json \
             behind it — stop the daemon or use a daemon-enabled binary",
        )
    }
}
impl std::error::Error for DaemonHoldsLock {}

/// Direct file driver. Reads load the file ungated (the daemon keeps it current via
/// write-through). Writes first take a shared, non-blocking lock on `devkitd.lock` — the
/// gate — and refuse if a daemon holds it exclusive, then run the data-flock RMW.
pub struct FlockStore {
    gate_path: PathBuf,
    lock_path: PathBuf,
    data_path: PathBuf,
}

impl FlockStore {
    /// Real-paths store used by every direct caller.
    pub fn new() -> Self {
        Self {
            gate_path: paths::devkitd_lock(),
            lock_path: paths::lock_file(),
            data_path: paths::registry_file(),
        }
    }
    /// Scratch-paths store for tests.
    #[cfg(test)]
    fn at(dir: &Path) -> Self {
        Self {
            gate_path: dir.join("devkitd.lock"),
            lock_path: dir.join("ports.lock"),
            data_path: dir.join("ports.json"),
        }
    }
}

impl Default for FlockStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Store for FlockStore {
    fn snapshot(&self) -> Result<Data> {
        Ok(store::load(&self.data_path))
    }
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        // Every direct writer holds the shared gate for its entire RMW. The daemon
        // holds devkitd.lock exclusive for its whole life (via MemoryStore, never
        // FlockStore), so a concurrent try_read failure here means a live daemon
        // owns the registry — surface that as the typed refusal rather than writing
        // ports.json behind it.
        if let Some(parent) = self.gate_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.gate_path)?;
        let gate = RwLock::new(file);
        // `anyhow::Error::new` (not `anyhow!`) so the type survives for `downcast_ref`.
        let _shared = gate
            .try_read()
            .map_err(|_| anyhow::Error::new(DaemonHoldsLock))?;
        store::with_lock(&self.lock_path, &self.data_path, f)
    }
}

/// The daemon's authoritative in-memory registry. Reads are served from memory
/// with no flock and no file read; a mutation writes the file through (atomic
/// rename) and updates memory only if that write succeeded — the file is the
/// commit point, so memory and file never diverge and a crash can't orphan a pid.
pub struct MemoryStore {
    state: std::sync::Arc<std::sync::Mutex<Data>>,
    data_path: PathBuf,
}

impl MemoryStore {
    pub fn new(state: std::sync::Arc<std::sync::Mutex<Data>>, data_path: PathBuf) -> Self {
        Self { state, data_path }
    }
}

impl Store for MemoryStore {
    fn snapshot(&self) -> Result<Data> {
        Ok(self.state.lock().expect("registry mutex poisoned").clone())
    }
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        let mut guard = self.state.lock().expect("registry mutex poisoned");
        let mut next = guard.clone();
        let out = f(&mut next)?;
        next.stamp_version();
        store::save(&self.data_path, &next)?; // commit point: persist before memory
        *guard = next;
        Ok(out)
    }
}

/// Load the registry file into a `Data` for an owner with its own exclusion
/// (the daemon, holding `devkitd.lock` exclusive, at startup).
pub fn load() -> Data {
    store::load(&paths::registry_file())
}

/// Run `f` against the registry under the direct flock path. Public because
/// `devrun down` and the multiprocess race test drive a custom RMW through it;
/// now gated, so it refuses to write behind a live daemon.
pub fn with_lock<T>(f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
    FlockStore::new().commit(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_serde() {
        let mut d = Data::default();
        d.entries.insert(
            9100,
            Entry {
                app: "api".into(),
                holder: "/w".into(),
                role: Role::Issue,
                pid: None,
                logfile: None,
                ts: 1,
            },
        );
        let s = serde_json::to_string(&d).unwrap();
        let back: Data = serde_json::from_str(&s).unwrap();
        assert_eq!(back.entries[&9100].app, "api");
    }
}

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// True if something accepts a connection on localhost:port.
///
/// A one-shot TCP connect, not a bind probe: a server binds the wildcard
/// address (`0.0.0.0`), and on macOS/Windows a fresh `bind(("127.0.0.1", port))`
/// still succeeds against that, so a bind probe wrongly reports the port free.
/// Connecting detects an accepting server identically on every platform.
pub fn listening(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}
pub fn pid_alive(pid: u32) -> bool {
    devkit_common::sys::process_alive(pid)
}
pub fn holder_alive(holder: &str) -> bool {
    std::path::Path::new(holder).exists()
}

#[cfg(test)]
mod liveness_tests {
    use super::*;
    #[test]
    fn detects_bound_port() {
        // Bind the wildcard address, as a real server does, and probe via
        // loopback: a bind probe wrongly reports this free on macOS/Windows,
        // so listening() must connect, not bind.
        let l = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(listening(port)); // l accepts on 127.0.0.1:port
        drop(l);
        assert!(!listening(port)); // freed
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
        self.entries
            .iter()
            .find(|(_, e)| e.holder == holder && e.app == app && e.role == role)
            .map(|(p, _)| *p)
    }

    /// Reserve a port for one app (idempotent per holder+app+role). pid stays None.
    pub fn alloc_one(&mut self, holder: &str, app: &str, base: u16, role: Role) -> u16 {
        if let Some(p) = self.holds(holder, app, role) {
            return p;
        }
        let mut port = base;
        while self.entries.contains_key(&port) || listening(port) {
            port = port
                .checked_add(1)
                .unwrap_or_else(|| panic!("no free port available at or above {base}"));
        }
        self.entries.insert(
            port,
            Entry {
                app: app.into(),
                holder: holder.into(),
                role,
                pid: None,
                logfile: None,
                ts: now(),
            },
        );
        port
    }

    /// Attach a pid + logfile to a port's reservation, re-establishing the row if it
    /// was pruned in the gap between reserving and spawning — so a live process is
    /// never left untracked (which would make `devrun down` unable to stop it).
    pub fn record_pid(
        &mut self,
        port: u16,
        app: &str,
        holder: &str,
        role: Role,
        pid: u32,
        logfile: PathBuf,
    ) {
        let e = self.entries.entry(port).or_insert_with(|| Entry {
            app: app.into(),
            holder: holder.into(),
            role,
            pid: None,
            logfile: None,
            ts: now(),
        });
        e.app = app.into();
        e.holder = holder.into();
        e.role = role;
        e.pid = Some(pid);
        e.logfile = Some(logfile);
    }

    /// Release exactly the listed ports that are still present. Returns freed ports.
    pub fn release_ports(&mut self, ports: &[u16]) -> Vec<u16> {
        let freed: Vec<u16> = ports
            .iter()
            .copied()
            .filter(|p| self.entries.contains_key(p))
            .collect();
        for p in &freed {
            self.entries.remove(p);
        }
        freed
    }

    /// Release all entries for a holder (optionally one role). Returns freed ports.
    pub fn release(&mut self, holder: &str, role: Option<Role>) -> Vec<u16> {
        let freed: Vec<u16> = self
            .entries
            .iter()
            .filter(|(_, e)| e.holder == holder && role.is_none_or(|r| e.role == r))
            .map(|(p, _)| *p)
            .collect();
        for p in &freed {
            self.entries.remove(p);
        }
        freed
    }
}

/// Try a running daemon. `Ok(None)` = no daemon (caller uses the flock path).
/// `Ok(Some(resp))` = the daemon answered (the response may be `Response::Err`,
/// which callers decode into an `Err`). `Err` = a *live* daemon failed mid-request
/// — surfaced to the caller rather than silently written behind its back.
/// Returns `Ok(None)` inside the daemon itself (`DEVKITD_SELF`).
#[cfg(feature = "daemon")]
fn daemon_request(
    req: crate::daemon::proto::Request,
) -> Result<Option<crate::daemon::proto::Response>> {
    if std::env::var_os("DEVKITD_SELF").is_some() {
        return Ok(None);
    }
    let Some(mut c) = crate::daemon::client::try_existing() else {
        return Ok(None);
    };
    Ok(Some(c.request(&req)?))
}

/// Read the registry, pruning dead entries. Probes liveness *outside* the lock.
pub fn snapshot_with(store: &impl Store) -> Result<Data> {
    let data = store.snapshot()?;
    let dead = data.dead_ports();
    if dead.is_empty() {
        return Ok(data);
    }
    // Best-effort prune: a read must never fail because cleanup was blocked (a
    // daemon now owns the write gate). Persist the removals if we can; otherwise
    // return the dead-pruned view without persisting.
    match store.commit(|d| {
        for p in &dead {
            d.entries.remove(p);
        }
        Ok(d.clone())
    }) {
        Ok(pruned) => Ok(pruned),
        Err(_) => {
            let mut d = data;
            for p in &dead {
                d.entries.remove(p);
            }
            Ok(d)
        }
    }
}

/// Prune dead entries; returns the ports removed. Probes outside the lock.
pub fn prune_with(store: &impl Store) -> Result<Vec<u16>> {
    let data = store.snapshot()?;
    let dead = data.dead_ports();
    if dead.is_empty() {
        return Ok(Vec::new());
    }
    store.commit(|d| {
        for p in &dead {
            d.entries.remove(p);
        }
        Ok(())
    })?;
    Ok(dead)
}

/// Reserve a port for each `(app, base_port)` under `holder`+`role`. Probes
/// `listening()` outside the lock; the commit re-checks under exclusion.
pub fn alloc_with(
    store: &impl Store,
    holder: &str,
    reqs: &[(String, u16)],
    role: Role,
) -> Result<Vec<(String, u16)>> {
    let mut data = store.snapshot()?;
    let mut chosen: Vec<(String, u16)> = Vec::with_capacity(reqs.len());
    for (app, base) in reqs {
        if let Some(p) = data.holds(holder, app, role) {
            chosen.push((app.clone(), p));
            continue;
        }
        let mut port = *base;
        loop {
            let taken = data.entries.contains_key(&port) || chosen.iter().any(|(_, p)| *p == port);
            if !taken && !listening(port) {
                break;
            }
            port = port
                .checked_add(1)
                .unwrap_or_else(|| panic!("no free port available at or above {base}"));
        }
        data.entries.insert(
            port,
            Entry {
                app: app.clone(),
                holder: holder.into(),
                role,
                pid: None,
                logfile: None,
                ts: now(),
            },
        );
        chosen.push((app.clone(), port));
    }

    store.commit(|d| {
        let mut out = Vec::with_capacity(chosen.len());
        for (app, port) in &chosen {
            if let Some(p) = d.holds(holder, app, role) {
                out.push((app.clone(), p));
            } else if d.entries.contains_key(port) {
                let base = reqs
                    .iter()
                    .find(|(a, _)| a == app)
                    .map(|(_, b)| *b)
                    .unwrap_or(*port);
                let p = d.alloc_one(holder, app, base, role);
                out.push((app.clone(), p));
            } else {
                d.entries.insert(
                    *port,
                    Entry {
                        app: app.clone(),
                        holder: holder.into(),
                        role,
                        pid: None,
                        logfile: None,
                        ts: now(),
                    },
                );
                out.push((app.clone(), *port));
            }
        }
        Ok(out)
    })
}

pub fn record_pid_with(
    store: &impl Store,
    port: u16,
    app: &str,
    holder: &str,
    role: Role,
    pid: u32,
    logfile: PathBuf,
) -> Result<()> {
    store.commit(|d| {
        d.record_pid(port, app, holder, role, pid, logfile);
        Ok(())
    })
}

pub fn release_with(store: &impl Store, holder: &str, role: Option<Role>) -> Result<Vec<u16>> {
    store.commit(|d| Ok(d.release(holder, role)))
}

pub fn release_ports_with(store: &impl Store, ports: &[u16]) -> Result<Vec<u16>> {
    store.commit(|d| Ok(d.release_ports(ports)))
}

/// Read the registry, pruning dead entries (daemon fast path, else flock).
pub fn snapshot() -> Result<Data> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::Snapshot)? {
        return match resp {
            crate::daemon::proto::Response::Snapshot(d) => Ok(d),
            crate::daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    snapshot_with(&FlockStore::new())
}

/// Prune dead entries; returns the ports removed.
pub fn prune() -> Result<Vec<u16>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::Prune)? {
        return match resp {
            crate::daemon::proto::Response::Freed(v) => Ok(v),
            crate::daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    prune_with(&FlockStore::new())
}

/// Reserve a port for each `(app, base_port)` request under `holder`+`role`.
pub fn alloc(holder: &str, reqs: &[(String, u16)], role: Role) -> Result<Vec<(String, u16)>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::Alloc {
        holder: holder.to_string(),
        reqs: reqs.to_vec(),
        role,
    })? {
        return match resp {
            crate::daemon::proto::Response::Ports(v) => Ok(v),
            crate::daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    alloc_with(&FlockStore::new(), holder, reqs, role)
}

/// Attach a pid + logfile to a reservation (re-establishing it if pruned).
pub fn record_pid(
    port: u16,
    app: &str,
    holder: &str,
    role: Role,
    pid: u32,
    logfile: PathBuf,
) -> Result<()> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::RecordPid {
        port,
        app: app.to_string(),
        holder: holder.to_string(),
        role,
        pid,
        logfile: logfile.clone(),
    })? {
        return match resp {
            crate::daemon::proto::Response::Ok => Ok(()),
            crate::daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    record_pid_with(&FlockStore::new(), port, app, holder, role, pid, logfile)
}

/// Release all entries for `holder` (optionally one role); returns freed ports.
pub fn release(holder: &str, role: Option<Role>) -> Result<Vec<u16>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(crate::daemon::proto::Request::Release {
        holder: holder.to_string(),
        role,
    })? {
        return match resp {
            crate::daemon::proto::Response::Freed(v) => Ok(v),
            crate::daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    release_with(&FlockStore::new(), holder, role)
}

/// Render the port-status table shared by `portm status` and `devrun status`.
/// `only_holder = Some(h)` limits rows to that holder; `None` shows every port.
pub fn status_table(data: &Data, only_holder: Option<&str>) -> String {
    let mut t =
        devkit_common::ui::table(&["PORT", "APP", "ROLE", "HOLDER", "PID", "LISTENING", "AGE"]);
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
            if listening(*port) {
                "yes".into()
            } else {
                "no".into()
            },
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
        d.entries.insert(
            9100,
            Entry {
                app: "api".into(),
                holder: "/definitely/not/here".into(),
                role: Role::Issue,
                pid: None,
                logfile: None,
                ts: now(),
            },
        );
        d.prune();
        assert!(d.entries.is_empty());
    }
    #[test]
    fn release_frees_by_holder() {
        let mut d = Data::default();
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
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
        let d = Data::salvage(json).expect("entries object present");
        assert_eq!(d.entries[&9100].pid, Some(4321));
        assert_eq!(d.entries[&9100].app, "api");
    }

    #[test]
    fn salvage_gives_up_without_entries_object() {
        assert!(Data::salvage(r#"{"something":"else"}"#).is_none());
    }

    #[test]
    fn release_ports_removes_only_listed_present_ports() {
        let mut d = Data::default();
        let p1 = d.alloc_one("/w", "api", 9100, Role::Issue);
        let p2 = d.alloc_one("/w", "web", 9200, Role::Issue);
        // Release the first allocated port and one absent port (65000 is unlikely to be allocated).
        let freed = d.release_ports(&[p1, 65000]);
        assert_eq!(freed, vec![p1], "only the present listed port is freed");
        assert!(d.entries.contains_key(&p2), "unlisted ports stay");
        assert!(!d.entries.contains_key(&p1));
    }

    #[test]
    fn dead_ports_flags_dead_holder() {
        let mut d = Data::default();
        d.entries.insert(
            9100,
            Entry {
                app: "api".into(),
                holder: "/definitely/not/here".into(),
                role: Role::Issue,
                pid: None,
                logfile: None,
                ts: now(),
            },
        );
        assert_eq!(d.dead_ports(), vec![9100]);
    }
}

#[cfg(test)]
mod store_seam_tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("devkit-seam-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn alloc_with_reserves_pidless_then_record_pid_attaches() {
        let dir = tmp("alloc");
        let store = FlockStore::at(&dir);
        let out = alloc_with(&store, "/w", &[("api".into(), 9100)], Role::Issue).unwrap();
        let (_, port) = out[0];
        let d = store.snapshot().unwrap();
        assert_eq!(
            d.entries[&port].pid, None,
            "reserve before bind: pid-less row"
        );
        record_pid_with(
            &store,
            port,
            "api",
            "/w",
            Role::Issue,
            4321,
            PathBuf::from("/log"),
        )
        .unwrap();
        assert_eq!(store.snapshot().unwrap().entries[&port].pid, Some(4321));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_with_frees_holder() {
        let dir = tmp("release");
        let store = FlockStore::at(&dir);
        alloc_with(&store, "/w", &[("api".into(), 9100)], Role::Issue).unwrap();
        let freed = release_with(&store, "/w", None).unwrap();
        assert_eq!(freed.len(), 1);
        assert!(store.snapshot().unwrap().entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_refused_while_gate_held_exclusive() {
        let dir = tmp("gate-held");
        let store = FlockStore::at(&dir);
        // Simulate a running daemon: hold devkitd.lock exclusive on a separate fd.
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("devkitd.lock"))
            .unwrap();
        let mut excl = fd_lock::RwLock::new(f);
        let _held = excl.try_write().expect("take exclusive gate");
        let err = store
            .commit(|d| {
                d.alloc_one("/w", "api", 9100, Role::Issue);
                Ok(())
            })
            .unwrap_err();
        assert!(
            err.downcast_ref::<DaemonHoldsLock>().is_some(),
            "got: {err:#}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_is_ungated_and_prune_is_best_effort_under_held_gate() {
        let dir = tmp("snap-gate");
        let store = FlockStore::at(&dir);
        // Seed a dead reservation (dead holder dir => dead_ports flags it).
        store
            .commit(|d| {
                d.entries.insert(
                    9100,
                    Entry {
                        app: "api".into(),
                        holder: "/definitely/not/here".into(),
                        role: Role::Issue,
                        pid: None,
                        logfile: None,
                        ts: 0,
                    },
                );
                Ok(())
            })
            .unwrap();
        // Now hold the gate exclusive: snapshot must still succeed (reads ungated)
        // and must not propagate the blocked prune.
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("devkitd.lock"))
            .unwrap();
        let mut excl = fd_lock::RwLock::new(f);
        let _held = excl.try_write().unwrap();
        let snap = snapshot_with(&store).expect("read must not fail under held gate");
        assert!(
            !snap.entries.contains_key(&9100),
            "dead entry pruned from the returned view"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memorystore_commit_writes_through_and_updates_memory() {
        let dir = tmp("mem-ok");
        let state = std::sync::Arc::new(std::sync::Mutex::new(Data::default()));
        let store = MemoryStore::new(state.clone(), dir.join("ports.json"));
        alloc_with(&store, "/w", &[("api".into(), 9100)], Role::Issue).unwrap();
        // memory updated
        assert_eq!(state.lock().unwrap().entries.len(), 1);
        // file written through (load sees it)
        let on_disk: Data = devkit_common::store::load(&dir.join("ports.json"));
        assert_eq!(on_disk.entries.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memorystore_commit_failure_leaves_memory_unchanged() {
        let dir = tmp("mem-fail");
        let state = std::sync::Arc::new(std::sync::Mutex::new(Data::default()));
        // Point the data path at a *directory* so the file write fails.
        let bad = dir.join("as-dir");
        std::fs::create_dir_all(&bad).unwrap();
        let store = MemoryStore::new(state.clone(), bad);
        let err = alloc_with(&store, "/w", &[("api".into(), 9100)], Role::Issue);
        assert!(err.is_err(), "write-through failure must error");
        assert!(
            state.lock().unwrap().entries.is_empty(),
            "memory must be unchanged when the write fails"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
