//! Read-only detection of dev servers running outside the port registry.
//! Serializable, no rendering, no mutation — mirrors the `devkit-issue` facade.

mod signature;
pub use signature::{argv_matches, signature};

pub mod os;

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

/// Production scan over the real OS seams.
pub fn scan(cfg: &Config, data: &Data) -> Vec<Stray> {
    scan_with(cfg, data, &os::RealPortProbe, &os::RealProcTable)
}

/// The live process table (used by `reap` to build kill trees).
pub fn proc_table() -> Vec<Proc> {
    os::RealProcTable.snapshot()
}

/// Core scan over injected OS seams. Pure given its inputs.
pub fn scan_with(
    cfg: &Config,
    data: &Data,
    ports: &dyn PortProbe,
    procs: &dyn ProcTable,
) -> Vec<Stray> {
    let band = port_band_pass(cfg, data, ports);
    let proc = process_pass(cfg, data, procs);
    merge(band, proc)
}

/// Fold the two passes together: a port hit and a process hit on the same port
/// collapse into one `Source::Both` row carrying the process's pid/holder/command.
fn merge(band: Vec<Stray>, proc: Vec<Stray>) -> Vec<Stray> {
    use std::collections::BTreeMap;
    let mut by_port: BTreeMap<u16, Stray> = BTreeMap::new();
    let mut portless: Vec<Stray> = Vec::new();
    for s in proc {
        match s.port {
            Some(p) => {
                by_port.insert(p, s);
            }
            None => portless.push(s),
        }
    }
    for b in band {
        let Some(p) = b.port else { continue };
        by_port
            .entry(p)
            .and_modify(|existing| existing.source = Source::Both)
            .or_insert(b);
    }
    by_port.into_values().chain(portless).collect()
}

/// Runtime/wrapper binaries to climb through to the launch root; never a shell.
#[cfg(unix)]
const WRAPPERS: &[&str] = &[
    "doppler", "bun", "bunx", "node", "uv", "uvx", "python", "python3",
];

/// Index procs by pid, and compute the set of pids in any tracked server's tree.
#[cfg(unix)]
fn tracked_tree(data: &Data, procs: &[Proc]) -> std::collections::BTreeSet<u32> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut out = BTreeSet::new();
    let mut stack: Vec<u32> = data.entries.values().filter_map(|e| e.pid).collect();
    while let Some(pid) = stack.pop() {
        if out.insert(pid)
            && let Some(cs) = children.get(&pid)
        {
            stack.extend(cs.iter().copied());
        }
    }
    out
}

/// Climb from a matched leaf to the highest consecutive wrapper ancestor,
/// stopping at a shell, Claude, the supervisor, or the process tree root.
#[cfg(unix)]
fn launch_root(start: u32, by_pid: &std::collections::BTreeMap<u32, Proc>) -> u32 {
    let mut cur = start;
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(cur) {
            return cur;
        }
        let Some(p) = by_pid.get(&cur) else {
            return cur;
        };
        let Some(parent) = by_pid.get(&p.ppid) else {
            return cur;
        };
        let first = parent.argv.split_whitespace().next().unwrap_or("");
        let base = first.rsplit('/').next().unwrap_or(first);
        let is_wrapper = WRAPPERS.contains(&base);
        let tainted = parent.argv.contains("claude")
            || parent.argv.contains("shell-snapshots")
            || parent.argv.contains("devkitd")
            || parent.argv.contains("devrun");
        if !is_wrapper || tainted {
            return cur;
        }
        cur = p.ppid;
    }
}

/// Managed roots a stray's cwd must fall under (config-driven).
#[cfg(unix)]
fn managed_roots(cfg: &Config) -> Vec<String> {
    use crate::config::expand_tilde;
    let mut roots = Vec::new();
    if !cfg.defaults.worktree_root.is_empty() {
        roots.push(
            expand_tilde(&cfg.defaults.worktree_root)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if !cfg.defaults.baseline_path.is_empty() {
        roots.push(
            expand_tilde(&cfg.defaults.baseline_path)
                .to_string_lossy()
                .into_owned(),
        );
    }
    roots
}

/// Attribute a cwd to a worktree holder: prefer the longest known registry
/// holder that prefixes it, else `worktree_root + first path segment`.
#[cfg(unix)]
fn attribute_holder(cwd: &str, known: &[String], roots: &[String]) -> Option<String> {
    if let Some(h) = known
        .iter()
        .filter(|h| cwd == h.as_str() || cwd.starts_with(&format!("{h}/")))
        .max_by_key(|h| h.len())
    {
        return Some(h.clone());
    }
    for r in roots {
        if let Some(rest) = cwd.strip_prefix(&format!("{r}/")) {
            let seg = rest.split('/').next().unwrap_or("");
            if !seg.is_empty() {
                return Some(format!("{r}/{seg}"));
            }
        } else if cwd == r {
            return Some(r.clone());
        }
    }
    None
}

#[cfg(unix)]
fn process_pass(cfg: &Config, data: &Data, procs: &dyn ProcTable) -> Vec<Stray> {
    use std::collections::{BTreeMap, BTreeSet};
    let table = procs.snapshot();
    let by_pid: BTreeMap<u32, Proc> = table.iter().map(|p| (p.pid, p.clone())).collect();
    let tracked = tracked_tree(data, &table);
    let roots = managed_roots(cfg);
    let known: Vec<String> = data.entries.values().map(|e| e.holder.clone()).collect();
    // (app name, signature) pairs.
    let sigs: Vec<(String, Vec<String>)> = cfg
        .apps
        .iter()
        .map(|(n, a)| (n.clone(), signature(&a.launch)))
        .filter(|(_, s)| !s.is_empty())
        .collect();

    let mut out = Vec::new();
    let mut seen_roots: BTreeSet<u32> = BTreeSet::new();
    for p in &table {
        if tracked.contains(&p.pid) {
            continue;
        }
        let Some(cwd) = p.cwd.as_deref() else {
            continue;
        };
        if !roots
            .iter()
            .any(|r| cwd == r.as_str() || cwd.starts_with(&format!("{r}/")))
        {
            continue;
        }
        let Some((app, _)) = sigs.iter().find(|(_, s)| argv_matches(&p.argv, s)) else {
            continue;
        };
        // Only treat wrapper-launched processes (doppler/bun/node/uv/…) as climb
        // candidates. This drops two kinds of spurious matches: a shell whose
        // `-c "…"` argv merely contains the server command, and any non-server
        // process that happens to mention the signature words. The real chain
        // still resolves — every devkit app launches via `doppler run --`, whose
        // argv embeds the downstream command, so the doppler ancestor is always a
        // valid candidate and the climb/dedup land on it. The deliberate
        // narrowing: a bare binary launched with no wrapper is not attributed
        // here (the port-band pass still surfaces it by port).
        let prog = p.argv.split_whitespace().next().unwrap_or("");
        let prog_base = prog.rsplit('/').next().unwrap_or(prog);
        if !WRAPPERS.contains(&prog_base) {
            continue;
        }
        let root = launch_root(p.pid, &by_pid);
        if tracked.contains(&root) || !seen_roots.insert(root) {
            continue;
        }
        let root_proc = by_pid.get(&root).unwrap_or(p);
        let port = port_from_argv(&root_proc.argv).or_else(|| port_from_argv(&p.argv));
        out.push(Stray {
            port,
            pid: Some(root),
            holder: attribute_holder(cwd, &known, &roots),
            app: Some(app.clone()),
            command: Some(root_proc.argv.clone()),
            source: Source::ProcessPattern,
        });
    }
    out
}

#[cfg(not(unix))]
fn process_pass(_cfg: &Config, _data: &Data, _procs: &dyn ProcTable) -> Vec<Stray> {
    Vec::new()
}

/// Best-effort `--port N` / `-p N` extraction from a command line.
fn port_from_argv(argv: &str) -> Option<u16> {
    let toks: Vec<&str> = argv.split_whitespace().collect();
    for (i, t) in toks.iter().enumerate() {
        if (*t == "--port" || *t == "-p")
            && let Some(v) = toks.get(i + 1).and_then(|v| v.parse::<u16>().ok())
        {
            return Some(v);
        }
        if let Some(v) = t
            .strip_prefix("--port=")
            .and_then(|v| v.parse::<u16>().ok())
        {
            return Some(v);
        }
    }
    None
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

    #[cfg(unix)]
    fn proc(pid: u32, ppid: u32, argv: &str, cwd: &str) -> Proc {
        Proc {
            pid,
            ppid,
            argv: argv.into(),
            cwd: Some(cwd.into()),
        }
    }

    #[cfg(unix)]
    struct Table(Vec<Proc>);
    #[cfg(unix)]
    impl ProcTable for Table {
        fn snapshot(&self) -> Vec<Proc> {
            self.0.clone()
        }
    }

    #[cfg(unix)]
    #[test]
    fn climbs_to_doppler_root_and_attributes_holder() {
        let mut cfg = Config::default();
        cfg.defaults.stray_scan_width = 64;
        cfg.defaults.worktree_root = "/home/u/Git/x".into();
        cfg.apps.insert("api".into(), app(9100));
        let data = Data::default();
        let wt = "/home/u/Git/x/swe-1/apps/api";
        let table = Table(vec![
            proc(100, 1, "claude", "/home/u"),
            proc(
                200,
                100,
                "/bin/bash -c eval doppler run -- bun nitro dev --port 9200",
                wt,
            ),
            proc(
                300,
                200,
                "doppler run -p api-foundry -c dev_local -- bun nitro dev --port 9200",
                wt,
            ),
            proc(400, 300, "bun nitro dev --port 9200", wt),
            proc(
                500,
                400,
                "node /home/u/Git/x/swe-1/apps/api/node_modules/.bin/nitro dev --port 9200",
                wt,
            ),
        ]);
        let strays = process_pass(&cfg, &data, &table);
        assert_eq!(strays.len(), 1);
        let s = &strays[0];
        assert_eq!(s.pid, Some(300)); // doppler root, not bash, not claude
        assert_eq!(s.port, Some(9200));
        assert_eq!(s.holder.as_deref(), Some("/home/u/Git/x/swe-1"));
        assert_eq!(s.app.as_deref(), Some("api"));
    }

    #[cfg(unix)]
    #[test]
    fn tracked_server_tree_is_skipped() {
        let mut cfg = Config::default();
        cfg.defaults.worktree_root = "/home/u/Git/x".into();
        cfg.apps.insert("api".into(), app(9100));
        let mut data = Data::default();
        data.entries.insert(
            9100,
            Entry {
                app: "api".into(),
                holder: "/home/u/Git/x/swe-1".into(),
                role: Role::Issue,
                pid: Some(300),
                logfile: None,
                ts: 0,
            },
        );
        let wt = "/home/u/Git/x/swe-1/apps/api";
        let table = Table(vec![
            proc(300, 1, "doppler run -- bun nitro dev --port 9100", wt),
            proc(400, 300, "bun nitro dev --port 9100", wt),
        ]);
        assert!(process_pass(&cfg, &data, &table).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn server_outside_managed_root_is_ignored() {
        let mut cfg = Config::default();
        cfg.defaults.worktree_root = "/home/u/Git/x".into();
        cfg.apps.insert("api".into(), app(9100));
        let data = Data::default();
        let table = Table(vec![proc(
            300,
            1,
            "doppler run -- bun nitro dev --port 9200",
            "/home/u/other-project",
        )]);
        assert!(process_pass(&cfg, &data, &table).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn port_and_process_hits_on_same_port_merge_to_both() {
        let mut cfg = Config::default();
        cfg.defaults.stray_scan_width = 64;
        cfg.defaults.worktree_root = "/home/u/Git/x".into();
        cfg.apps.insert("api".into(), app(9100));
        let data = Data::default();
        let wt = "/home/u/Git/x/swe-1/apps/api";
        let table = Table(vec![
            proc(300, 1, "doppler run -- bun nitro dev --port 9105", wt),
            proc(400, 300, "bun nitro dev --port 9105", wt),
        ]);
        let strays = scan_with(&cfg, &data, &Listening(vec![9105]), &table);
        assert_eq!(strays.len(), 1);
        assert_eq!(strays[0].port, Some(9105));
        assert_eq!(strays[0].source, Source::Both);
        assert_eq!(strays[0].pid, Some(300));
    }
}
