//! Read-only detection of dev servers running outside the port registry.
//! Serializable, no rendering, no mutation — mirrors the `devkit-issue` facade.

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

/// Core scan over injected OS seams. Pure given its inputs.
pub fn scan_with(
    _cfg: &Config,
    _data: &Data,
    _ports: &dyn PortProbe,
    _procs: &dyn ProcTable,
) -> Vec<Stray> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::registry::Data;

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

    #[test]
    fn empty_world_has_no_strays() {
        let cfg = Config::default();
        let data = Data::default();
        assert!(scan_with(&cfg, &data, &NoPorts, &NoProcs).is_empty());
    }
}
