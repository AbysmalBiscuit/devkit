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
