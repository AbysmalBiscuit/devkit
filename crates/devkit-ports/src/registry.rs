use anyhow::{Context, Result};
use devkit_common::paths;
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Issue, Baseline }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub app: String,
    pub holder: String,
    pub role: Role,
    pub pid: Option<u32>,
    pub logfile: Option<PathBuf>,
    pub ts: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub entries: BTreeMap<u16, Entry>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn read(path: &std::path::Path) -> Data {
    match fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).unwrap_or_else(|_| {
            let _ = fs::rename(path, path.with_extension("json.bak"));
            eprintln!("warning: corrupt registry; backed up and reinitialised");
            Data::default()
        }),
        _ => Data::default(),
    }
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
    let _ = OpenOptions::new().create(true).write(true).open(&lock_path)?;
    let mut lock = RwLock::new(File::open(&lock_path)?);
    let _guard = lock.write()?; // blocks until exclusive
    let reg = paths::registry_file();
    let mut data = read(&reg);
    let out = f(&mut data)?;
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

pub const RESERVATION_GRACE_SECS: u64 = 120;

impl Data {
    /// Drop entries whose holder is gone, pid is dead, or are stale unbacked reservations.
    pub fn prune(&mut self) {
        let now = now();
        self.entries.retain(|port, e| {
            if !holder_alive(&e.holder) { return false; }
            match e.pid {
                Some(pid) => pid_alive(pid),
                None => listening(*port) || now.saturating_sub(e.ts) < RESERVATION_GRACE_SECS,
            }
        });
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
            port += 1;
        }
        self.entries.insert(port, Entry {
            app: app.into(), holder: holder.into(), role, pid: None, logfile: None, ts: now(),
        });
        port
    }

    pub fn record_pid(&mut self, port: u16, pid: u32, logfile: PathBuf) {
        if let Some(e) = self.entries.get_mut(&port) {
            e.pid = Some(pid);
            e.logfile = Some(logfile);
        }
    }

    /// Release all entries for a holder (optionally one role). Returns freed ports.
    pub fn release(&mut self, holder: &str, role: Option<Role>) -> Vec<u16> {
        let freed: Vec<u16> = self.entries.iter()
            .filter(|(_, e)| e.holder == holder && role.map_or(true, |r| e.role == r))
            .map(|(p, _)| *p).collect();
        for p in &freed { self.entries.remove(p); }
        freed
    }
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
}
