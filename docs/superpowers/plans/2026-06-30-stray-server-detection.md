# Stray dev-server detection & reaping — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect dev servers running outside the devrun port registry (config-driven, two signals), surface them read-only in `devrun status` / `devkit doctor` / MCP, and add a TTY-gated `devrun reap` that humans (never agents) use to kill them.

**Architecture:** A new read-only `devkit-ports::strays` facade scans for strays by two passes — a cross-platform port-band probe (`registry::listening`) and a `#[cfg(unix)]` process-table scan whose signatures derive from each app's `launch` argv. Both passes take injectable seams (`PortProbe`, `ProcTable`) so the core logic is unit-testable without real sockets or processes. Surfacing reuses the existing `Scope`, `status_table`, `Check`, and MCP `Action` machinery; `reap` mirrors the cross-worktree `down` TTY gate and adds no `--yes` bypass.

**Tech Stack:** Rust 2024 workspace, `anyhow`, `serde`/`serde_json`, std-only OS access (`/proc`, `std::net`), `clap`. Spec: `docs/superpowers/specs/2026-06-30-stray-server-detection-design.md`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/devkit-ports/src/strays/mod.rs` (new) | Facade: `Stray`/`Source`/`Proc` types, `PortProbe`/`ProcTable` traits, `scan`/`scan_with`, bands, process pass, climb, holder attribution, merge. |
| `crates/devkit-ports/src/strays/signature.rs` (new) | Pure: derive a server signature from `launch`; match a process argv against it. |
| `crates/devkit-ports/src/strays/os.rs` (new) | Real OS seams: `RealPortProbe` (over `registry::listening`), `RealProcTable` (`#[cfg(unix)]` `/proc`, else empty), `kill_tree`. |
| `crates/devkit-ports/src/lib.rs` | Add `pub mod strays;`. |
| `crates/devkit-ports/src/config.rs:71` | Add `stray_scan_width` default to `Defaults`. |
| `crates/devkit-ports/src/registry.rs` | Add `pub fn child_pids(...)`/`tracked_pid_tree` helper used by the process pass (kept here since it walks a `Proc` list, no registry state). *(Implemented inside strays instead — see Task 4; registry untouched.)* |
| `src/bin/devrun/main.rs` | `cmd_status` untracked section; new `Cmd::Reap` + `cmd_reap` (TTY gate). |
| `src/bin/devkit/doctor.rs` | Stray-count `Check` row. |
| `crates/devkit-mcp/src/ports.rs` | Read-only `ports.strays` action. |
| `README.md` | Document `devrun reap` + the untracked status section. |

**Conventions (apply to every task):** TDD — failing test first. Before each commit run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`. Commit messages follow Conventional Commits. Work happens in the `feat/stray-servers` worktree at `/home/lev/Git/lev/devkit-worktrees/stray-servers` — all `cargo`/`git` commands run there (`git -C` / `cargo --manifest-path` or `cd` into it in your shell).

---

## Task 1: strays module skeleton + data model

**Files:**
- Create: `crates/devkit-ports/src/strays/mod.rs`
- Modify: `crates/devkit-ports/src/lib.rs:5` (add module)

- [ ] **Step 1: Write the failing test**

In `crates/devkit-ports/src/strays/mod.rs`:

```rust
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
```

In `crates/devkit-ports/src/lib.rs`, add after `pub mod registry;`:

```rust
pub mod strays;
```

> If `Config` has no `Default` impl, derive one in `config.rs` (`#[derive(... Default)]` on `Config`, `Defaults`, and any non-defaulting nested types) — needed by these tests. Verify with `rg "struct Config" crates/devkit-ports/src/config.rs` and add `Default` where missing.

- [ ] **Step 2: Run test to verify it fails (then compiles+passes trivially)**

Run: `cargo test -p devkit-ports strays::tests::empty_world_has_no_strays`
Expected: first FAIL to compile (missing `Default`), then PASS once `Default` is in place.

- [ ] **Step 3: Make it compile** — add the `Default` derives noted above; no logic yet.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-ports strays::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p devkit-ports --all-targets -- -D warnings
git add crates/devkit-ports/src/strays/mod.rs crates/devkit-ports/src/lib.rs crates/devkit-ports/src/config.rs
git commit -m "feat(ports): add strays facade skeleton and data model"
```

---

## Task 2: signature derivation + argv matching

**Files:**
- Create: `crates/devkit-ports/src/strays/signature.rs`
- Modify: `crates/devkit-ports/src/strays/mod.rs` (add `mod signature;`)

- [ ] **Step 1: Write the failing test**

In `crates/devkit-ports/src/strays/signature.rs`:

```rust
//! Derive a dev-server signature from an app's `launch` argv, and test whether a
//! running process's command line matches it. Config-driven — no hardcoded
//! framework list beyond the runtime launchers we strip.

/// Runtime launchers stripped from the front of the server command.
const RUNTIMES: &[&str] = &[
    "bun", "bunx", "node", "uv", "uvx", "run", "python", "python3", "poetry", "pipenv",
];

/// The signature tokens for a launch command: the framework word, plus an
/// optional bare subcommand (`dev`/`run`). Empty when nothing meaningful remains.
///
/// Drops the doppler wrapper (everything up to and including the last `--`),
/// then flags, the `{port}` placeholder, and leading runtime launchers.
pub fn signature(launch: &[String]) -> Vec<String> {
    let cmd: &[String] = match launch.iter().rposition(|t| t == "--") {
        Some(i) => &launch[i + 1..],
        None => launch,
    };
    let mut words = cmd
        .iter()
        .filter(|t| !t.starts_with('-') && *t != "{port}");
    let framework = words.find(|t| !RUNTIMES.contains(&t.as_str()));
    let framework = match framework {
        Some(f) => f.clone(),
        None => return Vec::new(),
    };
    let mut sig = vec![framework];
    if let Some(next) = words.next() {
        // A bare subcommand like `dev`/`run`; exclude path/version-ish tokens.
        if !next.contains('/') && !next.contains(':') && !next.contains('.') {
            sig.push(next.clone());
        }
    }
    sig
}

/// True if every signature token appears as a whole argv word (or path leaf).
pub fn argv_matches(argv: &str, sig: &[String]) -> bool {
    !sig.is_empty() && sig.iter().all(|tok| word_present(argv, tok))
}

fn word_present(argv: &str, tok: &str) -> bool {
    let leaf = format!("/{tok}");
    argv.split_whitespace()
        .any(|w| w == tok || w.ends_with(&leaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nitro_signature_keeps_framework_and_dev() {
        let launch = v(&[
            "doppler", "run", "-p", "api-foundry", "-c", "dev_local", "--",
            "bun", "nitro", "dev", "--port", "{port}",
        ]);
        assert_eq!(signature(&launch), v(&["nitro", "dev"]));
    }

    #[test]
    fn uvicorn_signature_is_framework_only() {
        let launch = v(&[
            "doppler", "run", "-p", "plate-api", "-c", "dev_local", "--",
            "uv", "run", "uvicorn", "server.main:create_app", "--factory", "--reload",
            "--port", "{port}",
        ]);
        assert_eq!(signature(&launch), v(&["uvicorn"]));
    }

    #[test]
    fn vite_signature_drops_port_placeholder() {
        let launch = v(&[
            "doppler", "run", "-p", "monorepo", "-c", "dev_local", "--",
            "bun", "vite", "--port", "{port}",
        ]);
        assert_eq!(signature(&launch), v(&["vite"]));
    }

    #[test]
    fn flask_signature_is_framework_only() {
        let launch = v(&[
            "doppler", "run", "-p", "ada-printer", "-c", "dev_local", "--",
            "uv", "run", "flask", "--app", "src/server.py", "run", "--port", "{port}",
        ]);
        assert_eq!(signature(&launch), v(&["flask"]));
    }

    #[test]
    fn matches_bun_and_node_forms() {
        let sig = v(&["nitro", "dev"]);
        assert!(argv_matches("bun nitro dev --port 9200", &sig));
        assert!(argv_matches(
            "node /home/u/app/node_modules/.bin/nitro dev --port 9200",
            &sig
        ));
    }

    #[test]
    fn does_not_match_unrelated_or_partial() {
        let sig = v(&["vite"]);
        assert!(!argv_matches("bun vitest run", &sig)); // vitest != vite
        assert!(!argv_matches("doppler run -- bun test", &sig));
    }
}
```

In `crates/devkit-ports/src/strays/mod.rs`, add near the top:

```rust
mod signature;
pub use signature::{argv_matches, signature};
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-ports strays::signature`
Expected: FAIL to compile until the module is wired, then the assertions run.

- [ ] **Step 3: Implementation** — already shown above; ensure it compiles.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-ports strays::signature`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p devkit-ports --all-targets -- -D warnings
git add crates/devkit-ports/src/strays/
git commit -m "feat(ports): derive dev-server signatures from launch argv"
```

---

## Task 3: port-band pass

**Files:**
- Modify: `crates/devkit-ports/src/strays/mod.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/devkit-ports/src/strays/mod.rs` (above `#[cfg(test)]`):

```rust
/// Per-app scan window: ports `[base_port, base_port + width)`.
fn band(base: u16, width: u16) -> impl Iterator<Item = u16> {
    (base..base.saturating_add(width)).into_iter()
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
```

Change `scan_with` to call it:

```rust
pub fn scan_with(
    cfg: &Config,
    data: &Data,
    ports: &dyn PortProbe,
    _procs: &dyn ProcTable,
) -> Vec<Stray> {
    port_band_pass(cfg, data, ports)
}
```

Add tests:

```rust
    use crate::config::AppConfig;
    use crate::registry::{Entry, Role};

    fn app(base: u16) -> AppConfig {
        AppConfig {
            base_port: base,
            launch: vec!["doppler".into(), "run".into(), "--".into(), "bun".into(), "nitro".into(), "dev".into(), "--port".into(), "{port}".into()],
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
            Entry { app: "api".into(), holder: "/w".into(), role: Role::Issue, pid: Some(42), logfile: None, ts: 0 },
        );
        let strays = scan_with(&cfg, &data, &Listening(vec![9105]), &NoProcs);
        assert!(strays.is_empty());
    }
```

> `AppConfig` needs `Default`. Add `#[derive(Default)]` (alongside its existing derives) in `config.rs`; defaults for `Option`/`Vec`/`bool` fields are fine. If `base_port`/`launch` are required-by-deserialize, `Default` only affects test construction, not TOML parsing.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-ports strays::tests::untracked_listener_in_band_is_a_stray`
Expected: FAIL to compile until `stray_scan_width` exists (added in Task 7) — **temporarily** add the field now or land Task 7 first. To keep tasks ordered, add a minimal `pub stray_scan_width: u16` with `#[serde(default = "default_stray_scan_width")]` to `Defaults` here and flesh out config tests in Task 7.

- [ ] **Step 3: Implementation** — shown above.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-ports strays::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p devkit-ports --all-targets -- -D warnings
git add crates/devkit-ports/src/strays/mod.rs crates/devkit-ports/src/config.rs
git commit -m "feat(ports): port-band stray detection pass"
```

---

## Task 4: process pass — match, climb, attribute holder

**Files:**
- Modify: `crates/devkit-ports/src/strays/mod.rs`

- [ ] **Step 1: Write the failing test**

Add helpers + the pass to `mod.rs`:

```rust
/// Runtime/wrapper binaries to climb through to the launch root; never a shell.
const WRAPPERS: &[&str] = &[
    "doppler", "bun", "bunx", "node", "uv", "uvx", "python", "python3",
];

/// Index procs by pid, and compute the set of pids in any tracked server's tree.
fn tracked_tree(data: &Data, procs: &[Proc]) -> std::collections::BTreeSet<u32> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut out = BTreeSet::new();
    let mut stack: Vec<u32> = data.entries.values().filter_map(|e| e.pid).collect();
    while let Some(pid) = stack.pop() {
        if out.insert(pid) {
            if let Some(cs) = children.get(&pid) {
                stack.extend(cs.iter().copied());
            }
        }
    }
    out
}

/// Climb from a matched leaf to the highest consecutive wrapper ancestor,
/// stopping at a shell, Claude, the supervisor, or the process tree root.
fn launch_root(start: u32, by_pid: &std::collections::BTreeMap<u32, Proc>) -> u32 {
    let mut cur = start;
    loop {
        let Some(p) = by_pid.get(&cur) else { return cur };
        let Some(parent) = by_pid.get(&p.ppid) else { return cur };
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
fn managed_roots(cfg: &Config) -> Vec<String> {
    use crate::config::expand_tilde;
    let mut roots = Vec::new();
    if !cfg.defaults.worktree_root.is_empty() {
        roots.push(expand_tilde(&cfg.defaults.worktree_root));
    }
    if !cfg.defaults.baseline_path.is_empty() {
        roots.push(expand_tilde(&cfg.defaults.baseline_path));
    }
    roots
}

/// Attribute a cwd to a worktree holder: prefer the longest known registry
/// holder that prefixes it, else `worktree_root + first path segment`.
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
        let Some(cwd) = p.cwd.as_deref() else { continue };
        if !roots.iter().any(|r| cwd == r.as_str() || cwd.starts_with(&format!("{r}/"))) {
            continue;
        }
        let Some((app, _)) = sigs.iter().find(|(_, s)| argv_matches(&p.argv, s)) else {
            continue;
        };
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
        if (*t == "--port" || *t == "-p") {
            if let Some(v) = toks.get(i + 1).and_then(|v| v.parse::<u16>().ok()) {
                return Some(v);
            }
        }
        if let Some(v) = t.strip_prefix("--port=").and_then(|v| v.parse::<u16>().ok()) {
            return Some(v);
        }
    }
    None
}
```

Add tests (process-pass tests are unix-only since the real `process_pass` is):

```rust
    fn proc(pid: u32, ppid: u32, argv: &str, cwd: &str) -> Proc {
        Proc { pid, ppid, argv: argv.into(), cwd: Some(cwd.into()) }
    }

    struct Table(Vec<Proc>);
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
            proc(200, 100, "/bin/bash -c eval doppler run -- bun nitro dev --port 9200", wt),
            proc(300, 200, "doppler run -p api-foundry -c dev_local -- bun nitro dev --port 9200", wt),
            proc(400, 300, "bun nitro dev --port 9200", wt),
            proc(500, 400, "node /home/u/Git/x/swe-1/apps/api/node_modules/.bin/nitro dev --port 9200", wt),
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
            Entry { app: "api".into(), holder: "/home/u/Git/x/swe-1".into(), role: Role::Issue, pid: Some(300), logfile: None, ts: 0 },
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
            300, 1, "doppler run -- bun nitro dev --port 9200", "/home/u/other-project",
        )]);
        assert!(process_pass(&cfg, &data, &data_procs(&table)).is_empty());
    }
```

> In the last test, replace `data_procs(&table)` with `&table` (typo guard — pass the `&Table`). Keep `expand_tilde` import path correct: it is `crate::config::expand_tilde` (verify with `rg "fn expand_tilde" crates/devkit-ports/src/config.rs`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-ports strays::tests::climbs_to_doppler_root_and_attributes_holder`
Expected: FAIL to compile, then PASS once `process_pass` and helpers are in.

- [ ] **Step 3: Implementation** — shown above.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-ports strays::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p devkit-ports --all-targets -- -D warnings
git add crates/devkit-ports/src/strays/mod.rs
git commit -m "feat(ports): process-table stray pass with launch-root climb"
```

---

## Task 5: merge passes into final scan

**Files:**
- Modify: `crates/devkit-ports/src/strays/mod.rs`

- [ ] **Step 1: Write the failing test**

Replace `scan_with` body with the merge and add a `scan` entry point stub:

```rust
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
```

Add test:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-ports strays::tests::port_and_process_hits_on_same_port_merge_to_both`
Expected: FAIL, then PASS after `merge` is added.

- [ ] **Step 3: Implementation** — shown above.

- [ ] **Step 4: Run** `cargo test -p devkit-ports strays::` → PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p devkit-ports --all-targets -- -D warnings
git add crates/devkit-ports/src/strays/mod.rs
git commit -m "feat(ports): merge port and process stray passes"
```

---

## Task 6: real OS seams + `scan()` + `kill_tree`

**Files:**
- Create: `crates/devkit-ports/src/strays/os.rs`
- Modify: `crates/devkit-ports/src/strays/mod.rs`

- [ ] **Step 1: Write the failing test**

In `crates/devkit-ports/src/strays/os.rs`:

```rust
//! Real OS implementations of the strays seams. `/proc` is Linux-only; on other
//! targets the process table is empty and detection degrades to the port band.

use super::{PortProbe, Proc, ProcTable};

pub struct RealPortProbe;
impl PortProbe for RealPortProbe {
    fn listening(&self, port: u16) -> bool {
        crate::registry::listening(port)
    }
}

pub struct RealProcTable;
impl ProcTable for RealProcTable {
    #[cfg(target_os = "linux")]
    fn snapshot(&self) -> Vec<Proc> {
        let mut out = Vec::new();
        let Ok(dir) = std::fs::read_dir("/proc") else { return out };
        for ent in dir.flatten() {
            let name = ent.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let base = ent.path();
            let argv = std::fs::read(base.join("cmdline"))
                .ok()
                .map(|b| nul_to_space(&b))
                .unwrap_or_default();
            if argv.is_empty() {
                continue;
            }
            let ppid = read_ppid(&base.join("stat")).unwrap_or(0);
            let cwd = std::fs::read_link(base.join("cwd"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            out.push(Proc { pid, ppid, argv, cwd });
        }
        out
    }

    #[cfg(not(target_os = "linux"))]
    fn snapshot(&self) -> Vec<Proc> {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn nul_to_space(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.trim_end_matches('\0').replace('\0', " ")
}

#[cfg(target_os = "linux")]
fn read_ppid(stat: &std::path::Path) -> Option<u32> {
    // `/proc/<pid>/stat`: `pid (comm) state ppid ...`; comm may contain spaces,
    // so split after the last ')'.
    let s = std::fs::read_to_string(stat).ok()?;
    let rest = &s[s.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// SIGTERM every pid in each root's subtree, then SIGKILL survivors after a grace.
/// Unix-only; a no-op returning 0 elsewhere.
#[cfg(unix)]
pub fn kill_tree(roots: &[u32], procs: &[Proc]) -> usize {
    use std::collections::BTreeMap;
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut targets = Vec::new();
    let mut stack = roots.to_vec();
    while let Some(pid) = stack.pop() {
        if !targets.contains(&pid) {
            targets.push(pid);
            if let Some(cs) = children.get(&pid) {
                stack.extend(cs.iter().copied());
            }
        }
    }
    for pid in &targets {
        unsafe { libc::kill(*pid as i32, libc::SIGTERM) };
    }
    // Poll up to ~3s, then SIGKILL anything still alive.
    for _ in 0..30 {
        if targets.iter().all(|p| unsafe { libc::kill(*p as i32, 0) } != 0) {
            return targets.len();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for pid in &targets {
        unsafe { libc::kill(*pid as i32, libc::SIGKILL) };
    }
    targets.len()
}

#[cfg(not(unix))]
pub fn kill_tree(_roots: &[u32], _procs: &[Proc]) -> usize {
    0
}
```

In `mod.rs` add and provide the public `scan`:

```rust
pub mod os;

/// Production scan over the real OS seams.
pub fn scan(cfg: &Config, data: &Data) -> Vec<Stray> {
    scan_with(cfg, data, &os::RealPortProbe, &os::RealProcTable)
}

/// The live process table (used by `reap` to build kill trees).
pub fn proc_table() -> Vec<Proc> {
    use crate::strays::os::RealProcTable;
    ProcTable::snapshot(&RealProcTable)
}
```

Add a smoke test in `os.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_port_probe_reports_a_bound_listener() {
        let l = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(RealPortProbe.listening(port));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn proc_snapshot_includes_self() {
        let me = std::process::id();
        assert!(RealProcTable.snapshot().iter().any(|p| p.pid == me));
    }
}
```

> Add the `libc` dependency to `crates/devkit-ports/Cargo.toml` under `[target.'cfg(unix)'.dependencies] libc = "0.2"`. Pin it the way the repo pins others (see how `indicatif` is locked in `crates/devkit-common`). Run `cargo tree -p devkit-ports | rg libc` to confirm a single version.

- [ ] **Step 2: Run** `cargo test -p devkit-ports strays::os` → FAIL (missing libc/module) then PASS.

- [ ] **Step 3: Implementation** — shown above.

- [ ] **Step 4: Run** `cargo test -p devkit-ports` → PASS; `cargo build -p devkit-ports` clean.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p devkit-ports --all-targets -- -D warnings
git add crates/devkit-ports/src/strays/os.rs crates/devkit-ports/src/strays/mod.rs crates/devkit-ports/Cargo.toml Cargo.lock
git commit -m "feat(ports): real /proc + port-probe seams and kill_tree"
```

---

## Task 7: config `stray_scan_width`

**Files:**
- Modify: `crates/devkit-ports/src/config.rs:71-92`

- [ ] **Step 1: Write the failing test**

Add to the `Defaults` struct (after `ignored_checks`):

```rust
    /// Width of each app's port-band scan window for stray detection:
    /// ports `[base_port, base_port + stray_scan_width)`. Default 64.
    #[serde(default = "default_stray_scan_width")]
    pub stray_scan_width: u16,
```

Add the default fn near `default_pr_base`:

```rust
fn default_stray_scan_width() -> u16 {
    64
}
```

Add a test in `config.rs` `#[cfg(test)]`:

```rust
    #[test]
    fn stray_scan_width_defaults_to_64() {
        let toml = format!("[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=9100\nlaunch=['a']\n");
        let cfg = parse_str(&toml).unwrap(); // use the crate's existing parse helper
        assert_eq!(cfg.defaults.stray_scan_width, 64);
    }
```

> Match the existing test scaffolding: find how other config tests parse (`rg "fn parse|from_str|FULL_DEFAULTS" crates/devkit-ports/src/config.rs`) and reuse that exact helper and constant rather than inventing `parse_str`.

- [ ] **Step 2: Run** `cargo test -p devkit-ports config::` → FAIL then PASS.

- [ ] **Step 3: Implementation** — shown above (and remove the temporary field added in Task 3 if it differs).

- [ ] **Step 4: Run** `cargo test -p devkit-ports` → PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy -p devkit-ports --all-targets -- -D warnings
git add crates/devkit-ports/src/config.rs
git commit -m "feat(ports): add stray_scan_width default to config"
```

---

## Task 8: `devrun status` untracked section

**Files:**
- Modify: `src/bin/devrun/main.rs:684-699` (`cmd_status`) and add a render helper + Scope filter.

- [ ] **Step 1: Write the failing test**

Add a pure helper to `main.rs` that filters strays by `Scope` and renders, so it is testable without spawning servers:

```rust
/// Strays visible under a status scope: all of them with `--all`, else only
/// those attributed to the current worktree (or with an unknown holder).
fn strays_in_scope(strays: &[devkit_ports::strays::Stray], current: Option<&str>) -> Vec<devkit_ports::strays::Stray> {
    strays
        .iter()
        .filter(|s| match (current, s.holder.as_deref()) {
            (None, _) => true,                       // --all
            (Some(c), Some(h)) => h == c,
            (Some(_), None) => true,                 // port-only, unknown holder
        })
        .cloned()
        .collect()
}

fn render_strays(strays: &[devkit_ports::strays::Stray]) -> String {
    if strays.is_empty() {
        return String::new();
    }
    let mut t = ui::table(&["PORT", "APP", "PID", "HOLDER", "SOURCE", "COMMAND"]);
    for s in strays {
        let holder = s
            .holder
            .as_deref()
            .and_then(devkit_common::paths::leaf)
            .or(s.holder.as_deref())
            .unwrap_or("-");
        t.add_row(vec![
            s.port.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            s.app.clone().unwrap_or_else(|| "-".into()),
            s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            holder.to_string(),
            format!("{:?}", s.source).to_lowercase(),
            s.command.clone().unwrap_or_else(|| "-".into()),
        ]);
    }
    format!("untracked (outside devrun):\n{t}")
}
```

Update `cmd_status`:

```rust
fn cmd_status(cwd: &str, all: bool) -> Result<()> {
    let data = registry::snapshot()?;
    let current = if all { None } else { toplevel(cwd).ok() };
    match (&current, all) {
        (Some(h), _) => println!("{}", registry::status_table(&data, Some(h))),
        (None, true) => println!("{}", registry::status_table(&data, None)),
        (None, false) => println!("{}", registry::status_table(&registry::Data::default(), None)),
    }
    // Untracked strays (best-effort; never fails status).
    if let Ok(loaded) = load::load(None, Path::new(cwd)) {
        let strays = devkit_ports::strays::scan(&loaded.config, &data);
        let scoped = strays_in_scope(&strays, current.as_deref());
        let rendered = render_strays(&scoped);
        if !rendered.is_empty() {
            println!("\n{rendered}");
        }
    }
    Ok(())
}
```

Add tests in `main.rs` `#[cfg(test)]`:

```rust
    use devkit_ports::strays::{Source, Stray};

    fn stray(port: u16, holder: Option<&str>) -> Stray {
        Stray { port: Some(port), pid: Some(1), holder: holder.map(String::from), app: Some("api".into()), command: Some("doppler run -- bun nitro dev".into()), source: Source::ProcessPattern }
    }

    #[test]
    fn scope_all_shows_every_stray() {
        let strays = vec![stray(9200, Some("/w1")), stray(9201, Some("/w2"))];
        assert_eq!(strays_in_scope(&strays, None).len(), 2);
    }

    #[test]
    fn scope_current_filters_to_this_worktree_plus_unknown() {
        let strays = vec![stray(9200, Some("/w1")), stray(9201, Some("/w2")), stray(9202, None)];
        let scoped = strays_in_scope(&strays, Some("/w1"));
        assert_eq!(scoped.len(), 2); // /w1 + the unknown-holder one
        assert!(scoped.iter().all(|s| s.holder.as_deref() != Some("/w2")));
    }
```

- [ ] **Step 2: Run** `cargo test -p devkit --bin devrun` (or the workspace) → FAIL then PASS.

> Confirm the bin test invocation: `cargo test -p devkit` runs the root package tests including `src/bin/devrun`. Check with `rg "name = " Cargo.toml` for the root package name; use that with `-p`.

- [ ] **Step 3: Implementation** — shown above.

- [ ] **Step 4: Run** the tests → PASS; manually `cargo run --bin devrun -- status --all` shows the section when strays exist.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/devrun/main.rs
git commit -m "feat(devrun): show untracked strays in status"
```

---

## Task 9: `devrun reap` (TTY-gated, no bypass)

**Files:**
- Modify: `src/bin/devrun/main.rs` — add `Cmd::Reap`, dispatch, `cmd_reap`.

- [ ] **Step 1: Write the failing test (TTY-refusal + selection)**

Factor the agent-unsafe decision into a pure predicate and the selection into a pure fn:

```rust
/// Reap refuses to do anything destructive without an interactive terminal —
/// the same gate that protects cross-worktree `down`. There is deliberately no
/// flag that bypasses this, so an agent (no PTY) can never reap.
fn reap_allowed(is_tty: bool) -> bool {
    is_tty
}

/// Roots to kill from a scoped stray set: the resolved launch-root pids.
fn reap_roots(strays: &[devkit_ports::strays::Stray]) -> Vec<u32> {
    strays.iter().filter_map(|s| s.pid).collect()
}
```

`cmd_reap`:

```rust
fn cmd_reap(cwd: &str, all: bool) -> Result<()> {
    let data = registry::snapshot()?;
    let loaded = load::load(None, Path::new(cwd))?;
    let current = if all { None } else { Some(toplevel(cwd)?) };
    let strays = devkit_ports::strays::scan(&loaded.config, &data);
    let scoped = strays_in_scope(&strays, current.as_deref());
    if scoped.is_empty() {
        println!("no stray servers found");
        return Ok(());
    }
    println!("{}", render_strays(&scoped));

    if !reap_allowed(std::io::stdin().is_terminal()) {
        anyhow::bail!("reap requires an interactive terminal");
    }
    let roots = reap_roots(&scoped);
    if roots.is_empty() {
        println!("no killable strays (port-only, no resolved pid) — investigate manually");
        return Ok(());
    }
    if !confirm(&format!("Kill {} stray server(s)?", roots.len())) {
        println!("nothing killed");
        return Ok(());
    }
    let procs = devkit_ports::strays::proc_table();
    let n = devkit_ports::strays::os::kill_tree(&roots, &procs);
    println!("killed {n} process(es)");
    Ok(())
}
```

Add the clap variant in `enum Cmd` (after `Status`):

```rust
    /// Kill dev servers running outside devrun. This worktree by default; `--all`
    /// reaches every worktree. Requires an interactive terminal (no agent path).
    Reap {
        #[arg(long)]
        all: bool,
    },
```

Dispatch in `main()` (after the `Status` arm):

```rust
        Cmd::Reap { all } => cmd_reap(&cwd, *all),
```

Tests:

```rust
    #[test]
    fn reap_refused_without_tty() {
        assert!(!reap_allowed(false));
        assert!(reap_allowed(true));
    }

    #[test]
    fn reap_roots_are_resolved_pids_only() {
        let mut s1 = stray(9200, Some("/w1"));
        let mut s2 = stray(9201, Some("/w1"));
        s2.pid = None; // port-only
        let roots = reap_roots(&[s1.clone(), s2]);
        assert_eq!(roots, vec![1]); // only s1's pid
        s1.pid = Some(42);
        assert_eq!(reap_roots(&[s1]), vec![42]);
    }
```

- [ ] **Step 2: Run** `cargo test -p devkit --bin devrun reap` → FAIL then PASS.

- [ ] **Step 3: Implementation** — shown above. Reuse the existing `confirm` helper (already used by `cmd_down`).

- [ ] **Step 4: Run** tests → PASS. Manual: in a terminal, `cargo run --bin devrun -- reap` lists + prompts; piped (`echo | cargo run --bin devrun -- reap`) prints the list then errors "requires an interactive terminal".

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/devrun/main.rs
git commit -m "feat(devrun): add TTY-gated reap for untracked servers"
```

---

## Task 10: `devkit doctor` stray check

**Files:**
- Modify: `src/bin/devkit/doctor.rs`

- [ ] **Step 1: Write the failing test**

Add a `Check` row computed from a stray count, kept pure for testing:

```rust
fn stray_check(count: usize) -> Check {
    match count {
        0 => Check::Ok("no servers running outside devrun".into()),
        n => Check::Unset(Box::leak(
            format!("{n} server(s) running outside devrun — run `devrun reap`").into_boxed_str(),
        )),
    }
}
```

> `Check::Unset` holds `&'static str`; a `Box::leak` is acceptable for a one-shot CLI, but prefer adding a `Check::Warn(String)` variant if you want to avoid the leak — update `worst_exit` (Warn → exit 0), `print_human` (`⚠`), and `print_json` (`"warn"`) accordingly. Pick one; the test below targets the count→severity mapping.

Wire it into `gather` (append a row):

```rust
        Row {
            key: "devrun_strays",
            source: Source::Unset,
            check: {
                let n = devkit_ports::config::load_count_strays(); // see note
                stray_check(n)
            },
        },
```

> There is no `load_count_strays` yet. Implement a tiny helper in `doctor.rs` that mirrors `cmd_status`: `let data = registry::snapshot()?; let loaded = load::load(None, Path::new("."))?; strays::scan(&loaded.config, &data).len()`, wrapped to return 0 on error (doctor must not fail just because config is absent). Add the `devkit-ports` dependency to the root package if the `devkit` bin doesn't already depend on it (it does — `registry` is used elsewhere; confirm with `rg "devkit_ports" src/bin/devkit`).

Test:

```rust
    #[test]
    fn stray_check_severity_by_count() {
        assert!(matches!(stray_check(0), Check::Ok(_)));
        assert!(matches!(stray_check(3), Check::Unset(_))); // or Check::Warn
    }
```

- [ ] **Step 2: Run** `cargo test -p devkit --bin devkit doctor` → FAIL then PASS.

- [ ] **Step 3: Implementation** — shown above.

- [ ] **Step 4: Run** tests → PASS; `cargo run --bin devkit -- doctor` shows the row.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/devkit/doctor.rs
git commit -m "feat(devkit): doctor warns about servers outside devrun"
```

---

## Task 11: read-only `ports.strays` MCP action

**Files:**
- Modify: `crates/devkit-mcp/src/ports.rs`

- [ ] **Step 1: Write the failing test**

Add the action to the `actions()` vec in `ports.rs`:

```rust
        Action {
            name: "ports.strays",
            summary: "List dev servers running outside the devrun registry (read-only).",
            schema: strays_schema,
            handler: strays,
        },
```

Schema + handler:

```rust
fn strays_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Repo/worktree path to resolve config from." }
        },
        "required": ["root"],
        "additionalProperties": false
    })
}

#[derive(Deserialize)]
struct StraysArgs {
    root: String,
}

fn strays(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: StraysArgs = serde_json::from_value(args).context("invalid ports.strays arguments")?;
    let loaded = devkit_ports::load::load(None, std::path::Path::new(&a.root))
        .context("loading config for ports.strays")?;
    let data = registry::snapshot()?;
    let strays = devkit_ports::strays::scan(&loaded.config, &data);
    Ok(serde_json::to_value(strays)?)
}
```

Extend the existing describe test (`crates/devkit-mcp/src/actions.rs` `describe_lists_the_ports_actions`) to assert `ports.strays` is present, or add a focused test in `ports.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn strays_action_is_registered() {
        assert!(super::actions().iter().any(|a| a.name == "ports.strays"));
    }
}
```

- [ ] **Step 2: Run** `cargo test -p devkit-mcp` → FAIL then PASS.

- [ ] **Step 3: Implementation** — shown above. (No reap handler — mutation stays off MCP.)

- [ ] **Step 4: Run** `cargo test -p devkit-mcp` → PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-mcp/src/ports.rs crates/devkit-mcp/src/actions.rs
git commit -m "feat(mcp): expose read-only ports.strays detection"
```

---

## Task 12: docs + full gate

**Files:**
- Modify: `README.md`, `AGENTS.md` (layout/invariants note)

- [ ] **Step 1: Document the feature**

Add to `README.md` a `devrun reap` entry and an "untracked servers" note under `devrun status`, and to `AGENTS.md`:
- Under devrun's row: mention `reap`.
- An invariant bullet: *"`devrun reap` is TTY-gated with no bypass flag and is never exposed on MCP — only read-only `ports.strays` detection is. An agent (no PTY) can see strays but cannot kill them."*

- [ ] **Step 2: Run the full merge gate**

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all green (test count = prior 327 + the new tests).

- [ ] **Step 3: Commit**

```bash
git add README.md AGENTS.md
git commit -m "docs: document devrun reap and untracked-server detection"
```

- [ ] **Step 4: Manual end-to-end sanity (Linux)**

With a stray running (`cd <worktree>/apps/api && doppler run -c dev_local -- bun nitro dev --port 9300 &`):
- `devrun status --all` lists it under *untracked*.
- `devkit doctor` shows the warn row.
- `devrun reap` (in a terminal) lists, prompts, and kills it; piping stdin refuses.
- `devrun status` afterward shows it gone.

---

## Self-Review

**Spec coverage:**
- Two-pass detection (port-band + process) → Tasks 3, 4, 5. ✓
- Config-derived signatures → Task 2. ✓
- `Stray` serializable facade → Task 1. ✓
- Injectable test seams → Tasks 1, 6. ✓
- `devrun status` untracked, holder-scoped + `--all` → Task 8. ✓
- `devkit doctor` check → Task 10. ✓
- `devrun reap` TTY-gated, no bypass, CLI-only → Task 9. ✓
- Read-only MCP detection, no reap handler → Task 11. ✓
- Cross-platform (`#[cfg(unix)]`/`linux` fallbacks) → Tasks 4, 6. ✓
- `stray_scan_width` default 64 → Task 7. ✓

**Type consistency:** `Stray`/`Source`/`Proc`/`PortProbe`/`ProcTable` defined in Task 1, used unchanged through Tasks 3–11. `scan_with`/`scan`/`proc_table`/`os::kill_tree` signatures stable. `strays_in_scope`/`render_strays`/`reap_roots`/`reap_allowed` defined in Tasks 8–9 and reused within them.

**Known confirmations the implementer must make (call out, don't guess):**
- Root package name for `cargo test -p <pkg> --bin devrun|devkit` (Task 8/9/10).
- Exact config test helper + `FULL_DEFAULTS` constant (Task 7).
- `expand_tilde` path and `confirm`/`ui::table` signatures (Tasks 4, 8).
- `libc` pin location/style in `Cargo.toml` (Task 6).
- Whether to add a `Check::Warn(String)` variant vs. leaking a string (Task 10).
