//! Read-only detection of dev servers running outside the port registry.
//! Serializable, no rendering, no mutation — mirrors the `devkit-issue` facade.

mod signature;
pub use signature::{argv_matches, signature};

use crate::config::Config;
use crate::registry::Data;

/// Which signal(s) flagged a stray.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    PortBand,
    ProcessPattern,
    Both,
}

/// A dev server that is listening but not owned by a live registry row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Stray {
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub holder: Option<String>,
    pub app: Option<String>,
    pub command: Option<String>,
    pub source: Source,
}

/// A process-table row — the OS-agnostic `ps aux` equivalent, so the scan is
/// testable without real processes.
#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub argv: String,
    pub cwd: Option<String>,
}

/// Probe whether a TCP port is accepting connections on localhost.
pub trait PortProbe {
    fn listening(&self, port: u16) -> bool;
}

/// Snapshot the process table.
pub trait ProcTable {
    fn snapshot(&self) -> Vec<Proc>;
}

/// Per-app scan window: ports `[base_port, base_port + width)`.
fn band(base: u16, width: u16) -> impl Iterator<Item = u16> {
    base..base.saturating_add(width)
}

/// Port-band pass: a listening port in any app's band with no registry row is a
/// stray. Cross-platform. Holder/pid are unknown here (filled by the merge).
fn port_band_pass(cfg: &Config, data: &Data, ports: &dyn PortProbe) -> Vec<Stray> {
    let width = cfg.defaults.stray_scan_width;
    let mut out = Vec::new();
    let mut seen: std::collections::BTreeSet<u16> = std::collections::BTreeSet::new();
    for (name, app) in &cfg.apps {
        for p in band(app.base_port, width) {
            if data.entries.contains_key(&p) || seen.contains(&p) {
                continue;
            }
            if ports.listening(p) {
                seen.insert(p);
                out.push(Stray {
                    port: Some(p),
                    pid: None,
                    holder: None,
                    app: Some(name.clone()),
                    command: None,
                    source: Source::PortBand,
                });
            }
        }
    }
    out
}

/// Core scan over injected OS seams. Pure given its inputs.
pub fn scan_with(
    cfg: &Config,
    data: &Data,
    ports: &dyn PortProbe,
    _procs: &dyn ProcTable,
) -> Vec<Stray> {
    port_band_pass(cfg, data, ports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, Config};
    use crate::registry::{Data, Entry, Role};

    struct NoPorts;
    impl PortProbe for NoPorts {
        fn listening(&self, _port: u16) -> bool {
            false
        }
    }
    struct NoProcs;
    impl ProcTable for NoProcs {
        fn snapshot(&self) -> Vec<Proc> {
            Vec::new()
        }
    }

    fn app(base: u16) -> AppConfig {
        AppConfig {
            base_port: base,
            launch: vec![
                "doppler".into(),
                "run".into(),
                "--".into(),
                "bun".into(),
                "nitro".into(),
                "dev".into(),
                "--port".into(),
                "{port}".into(),
            ],
            ..AppConfig::default()
        }
    }

    struct Listening(Vec<u16>);
    impl PortProbe for Listening {
        fn listening(&self, port: u16) -> bool {
            self.0.contains(&port)
        }
    }

    #[test]
    fn empty_world_has_no_strays() {
        let cfg = Config::default();
        let data = Data::default();
        assert!(scan_with(&cfg, &data, &NoPorts, &NoProcs).is_empty());
    }

    #[test]
    fn untracked_listener_in_band_is_a_stray() {
        let mut cfg = Config::default();
        cfg.defaults.stray_scan_width = 64;
        cfg.apps.insert("api".into(), app(9100));
        let data = Data::default();
        let strays = scan_with(&cfg, &data, &Listening(vec![9105]), &NoProcs);
        assert_eq!(strays.len(), 1);
        assert_eq!(strays[0].port, Some(9105));
        assert_eq!(strays[0].app.as_deref(), Some("api"));
        assert_eq!(strays[0].source, Source::PortBand);
    }

    #[test]
    fn tracked_port_is_not_a_stray() {
        let mut cfg = Config::default();
        cfg.defaults.stray_scan_width = 64;
        cfg.apps.insert("api".into(), app(9100));
        let mut data = Data::default();
        data.entries.insert(
            9105,
            Entry {
                app: "api".into(),
                holder: "/w".into(),
                role: Role::Issue,
                pid: Some(42),
                logfile: None,
                ts: 0,
            },
        );
        let strays = scan_with(&cfg, &data, &Listening(vec![9105]), &NoProcs);
        assert!(strays.is_empty());
    }
}
