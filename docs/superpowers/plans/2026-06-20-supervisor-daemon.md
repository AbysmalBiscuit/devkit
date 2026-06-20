# Supervisor Daemon Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an optional, opt-in `devkit-portd` daemon that owns running dev-server processes — restarting them on crash, tracking their memory, serving logs, and serving registry operations over a unix socket — while leaving default (no-daemon) behavior byte-for-byte unchanged.

**Architecture:** The daemon is a *flock participant, never a flock replacement*: every `ports.json` mutation still goes through `registry::with_lock`. Optionality is two gates — a `daemon` Cargo feature (build) and a runtime gate (`[daemon] enabled` / `DEVKIT_DAEMON=1` / `devrun up --supervise`). The client connects to an existing daemon opportunistically and falls back to the flock path on any failure, which is safe because every facade op is idempotent.

**Tech Stack:** Rust (edition 2024), `serde_json` over a unix domain socket (newline-delimited JSON), `fd-lock` single-instance lock, `nix` for `waitpid`/signals, no async runtime (thread-per-connection accept loop + one supervision thread).

**Spec:** `docs/superpowers/specs/2026-06-20-supervisor-daemon-design.md`. Deferred work: `docs/next-features.md`.

---

## Parallelization Map

Tasks are grouped into **waves**. Within a wave, tasks touch disjoint files and may be implemented by **parallel subagents in separate worktrees**. Across waves, order is mandatory (later waves import earlier waves' symbols).

> **Note for subagent-driven-development:** that skill dispatches implementers **sequentially** (one at a time, review between). The wave grouping below is for when you run implementers in parallel git worktrees. If executing strictly sequentially, just follow task order 1→11; the dependency notes still tell you nothing is referenced before it exists.

| Wave | Tasks | Parallel? | Touches (disjoint) |
|---|---|---|---|
| 0 | **1** | alone (prereq for all) | `devkit-common` (move `supervise.rs`), `devrun` import |
| 1 | **2, 3, 4** | ✅ 3-way parallel | T2 `paths.rs`; T3 `config.rs`; T4 `devkit-ports/src/daemon/` |
| 2 | **5, 6** | ✅ 2-way parallel | T5 `devkit-ports/src/daemon/client.rs`; T6 new crate `devkit-portd` |
| 3 | **7** | alone | `devkit-portd/src/supervisor.rs` |
| 4 | **8, 9** | ✅ 2-way parallel | T8 `devkit-portd/src/server.rs` + `main.rs`; T9 `devkit-ports/src/registry.rs` |
| 5 | **10** | alone | `devrun/src/main.rs` + `devrun/Cargo.toml` |
| 6 | **11** | alone | `devkit-portd/tests/` |

**Per-task dependency note** is repeated in each task header (`Depends on:` / `Parallel-safe with:`).

After **every** task: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` must be green (the merge gate). Daemon code is feature-gated, so also run the feature build where noted: `cargo build -p devkit-portd` and `cargo test -p devkit-portd`.

---

## Task 1: Move `supervise.rs` into `devkit-common` (prerequisite refactor)

**Depends on:** nothing. **Parallel-safe with:** nothing (everything else imports the moved module).

Both `devrun` and the daemon need `spawn_detached`/`wait_ready`/`stop`/`tail`. Move the file verbatim into `devkit-common`, add a memory helper, and re-point `devrun`.

**Files:**
- Create: `crates/devkit-common/src/supervise.rs` (moved + extended)
- Delete: `crates/devrun/src/supervise.rs`
- Modify: `crates/devkit-common/src/lib.rs` (add `pub mod supervise;`)
- Modify: `crates/devrun/src/main.rs:2` (`mod supervise;` → use common)

- [ ] **Step 1: Move the file**

```bash
git mv crates/devrun/src/supervise.rs crates/devkit-common/src/supervise.rs
```

- [ ] **Step 2: Register the module in common**

Edit `crates/devkit-common/src/lib.rs` — insert `pub mod supervise;` in alphabetical order:

```rust
pub mod cmd;
pub mod linear;
pub mod paths;
pub mod report;
pub mod supervise;
pub mod ui;
pub mod worktree;
```

- [ ] **Step 3: Write the failing test for the new tree-RSS helper**

Append to `crates/devkit-common/src/supervise.rs` inside `mod tests`:

```rust
    #[test]
    fn tree_rss_counts_self() {
        // Our own process has non-zero resident memory.
        let rss = tree_rss_bytes(std::process::id());
        assert!(rss > 0, "expected non-zero RSS for current process");
    }
```

- [ ] **Step 4: Run it to verify it fails**

Run: `cargo test -p devkit-common tree_rss_counts_self`
Expected: FAIL — `cannot find function tree_rss_bytes`.

- [ ] **Step 5: Implement `tree_rss_bytes`**

Add to `crates/devkit-common/src/supervise.rs` (before `#[cfg(test)]`). It scans `/proc` once, builds a ppid→children map, and sums resident pages over the process subtree rooted at `root` (so a dev server's forked workers are counted, not just the parent):

```rust
/// Resident set size, in bytes, summed over the process subtree rooted at `root`
/// (the process plus every descendant). Returns 0 if the root is gone. Linux-only;
/// reads `/proc` and needs no privilege.
pub fn tree_rss_bytes(root: u32) -> u64 {
    // pid -> ppid for every visible process.
    let mut parent: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else { return 0 };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if let Some(ppid) = read_ppid(pid) {
            parent.insert(pid, ppid);
        }
    }
    // BFS the subtree rooted at `root`.
    let mut total = 0u64;
    let mut stack = vec![root];
    let mut seen = std::collections::HashSet::new();
    let page = 4096u64;
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) { continue; }
        total += resident_pages(pid).saturating_mul(page);
        for (&child, &pp) in &parent {
            if pp == pid { stack.push(child); }
        }
    }
    total
}

/// Parent pid from `/proc/<pid>/stat` (field 4, after the possibly-paren'd comm).
fn read_ppid(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is in parens and may contain spaces/parens; split after the last ')'.
    let rest = stat.rsplit_once(')')?.1;
    let mut it = rest.split_whitespace();
    let _state = it.next()?;          // field 3
    it.next()?.parse::<u32>().ok()    // field 4 = ppid
}

/// Resident pages from `/proc/<pid>/statm` (field 2). 0 if unreadable.
fn resident_pages(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|n| n.parse::<u64>().ok()))
        .unwrap_or(0)
}
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p devkit-common tree_rss_counts_self`
Expected: PASS.

- [ ] **Step 7: Re-point `devrun` at the common module**

In `crates/devrun/src/main.rs`, delete the `mod supervise;` line (line 2) and add an import. Change the top of the file so `supervise::` resolves to common:

```rust
mod env;
mod baseline;

use devkit_common::supervise;
```

(Leave the existing `use devkit_common::{cmd::git, paths, ui};` as-is; add the `supervise` import on its own line as above. All existing `supervise::spawn_detached`/`wait_ready`/`stop`/`tail` call sites stay unchanged.)

- [ ] **Step 8: Verify the whole workspace builds and tests pass**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings. The moved `spawn_and_ready_on_python_http` test now runs under `devkit-common`.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor: move supervise into devkit-common, add tree_rss_bytes"
```

---

## Task 2: Daemon paths

**Depends on:** nothing (Task 1 doesn't touch `paths.rs`). **Parallel-safe with:** Tasks 3, 4.

**Files:**
- Modify: `crates/devkit-common/src/paths.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/devkit-common/src/paths.rs` `mod tests`:

```rust
    #[test]
    fn daemon_paths_under_state() {
        assert!(socket_file().ends_with(".claude/state/devkit/portd.sock"));
        assert!(daemon_lock_file().ends_with(".claude/state/devkit/portd.lock"));
        assert!(daemon_log().ends_with(".claude/state/devkit/logs/portd.log"));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-common daemon_paths_under_state`
Expected: FAIL — `cannot find function socket_file`.

- [ ] **Step 3: Implement the three paths**

Add after `pub fn logs_dir()` in `crates/devkit-common/src/paths.rs`:

```rust
/// Unix socket the daemon binds; clients connect here.
pub fn socket_file() -> PathBuf { state_dir().join("portd.sock") }
/// Single-instance lock for the daemon — separate from the registry's `ports.lock`.
pub fn daemon_lock_file() -> PathBuf { state_dir().join("portd.lock") }
/// Daemon log file.
pub fn daemon_log() -> PathBuf { logs_dir().join("portd.log") }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-common daemon_paths_under_state`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/paths.rs
git commit -m "feat(paths): add daemon socket/lock/log paths"
```

---

## Task 3: `[daemon]` config section

**Depends on:** nothing. **Parallel-safe with:** Tasks 2, 4.

**Files:**
- Modify: `crates/devkit-ports/src/config.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/devkit-ports/src/config.rs` `mod tests`:

```rust
    #[test]
    fn daemon_defaults_when_absent() {
        let c = Config::parse(SAMPLE).unwrap();
        assert!(!c.daemon.enabled);
        assert_eq!(c.daemon.idle_timeout_secs, 1800);
        assert_eq!(c.daemon.max_restarts, 5);
        assert_eq!(c.daemon.restart_window_secs, 60);
        assert_eq!(c.daemon.memory_warn_mb, 0);
        assert_eq!(c.daemon.memory_limit_mb, 0);
        assert_eq!(c.daemon.memory_action, "warn");
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-ports daemon_defaults_when_absent`
Expected: FAIL — `no field daemon on Config`.

- [ ] **Step 3: Add the struct and field**

In `crates/devkit-ports/src/config.rs`, add the field to `Config` (with a default so existing configs parse) and define `DaemonConfig`:

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    pub defaults: Defaults,
    pub apps: HashMap<String, AppConfig>,
    #[serde(default)]
    pub daemon: DaemonConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Run gate: autostart the daemon only when true (or via DEVKIT_DAEMON=1 / --supervise).
    pub enabled: bool,
    /// Exit after this many idle seconds with zero clients AND zero supervised children.
    pub idle_timeout_secs: u64,
    /// Crash-loop guard: restarts allowed within `restart_window_secs`.
    pub max_restarts: u32,
    pub restart_window_secs: u64,
    /// Log a loud line past this supervised tree-RSS in MB (0 = off).
    pub memory_warn_mb: u64,
    /// Take `memory_action` past this tree-RSS in MB (0 = off).
    pub memory_limit_mb: u64,
    /// "warn" (v1) — "restart" is deferred (see docs/next-features.md).
    pub memory_action: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        DaemonConfig {
            enabled: false,
            idle_timeout_secs: 1800,
            max_restarts: 5,
            restart_window_secs: 60,
            memory_warn_mb: 0,
            memory_limit_mb: 0,
            memory_action: "warn".to_string(),
        }
    }
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-ports daemon_defaults_when_absent`
Expected: PASS. (`rejects_prd` and `parses_sample` still pass — `daemon` defaults in.)

- [ ] **Step 5: Add a parse test for an explicit `[daemon]` block**

```rust
    #[test]
    fn parses_explicit_daemon_block() {
        let src = format!("{SAMPLE}\n[daemon]\nenabled = true\nidle_timeout_secs = 600\nmemory_warn_mb = 6000\n");
        let c = Config::parse(&src).unwrap();
        assert!(c.daemon.enabled);
        assert_eq!(c.daemon.idle_timeout_secs, 600);
        assert_eq!(c.daemon.memory_warn_mb, 6000);
        assert_eq!(c.daemon.max_restarts, 5); // untouched field keeps its default
    }
```

- [ ] **Step 6: Run it**

Run: `cargo test -p devkit-ports parses_explicit_daemon_block`
Expected: PASS.

- [ ] **Step 7: Document the block in the sample config**

Append to `configs/example.toml` (so the real config documents the knobs; all default, so behavior is unchanged):

```toml
[daemon]
enabled            = false
idle_timeout_secs  = 1800
max_restarts       = 5
restart_window_secs = 60
memory_warn_mb     = 6000
memory_limit_mb    = 12000
memory_action      = "warn"
```

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-ports/src/config.rs configs/example.toml
git commit -m "feat(config): add [daemon] section with serde defaults"
```

---

## Task 4: IPC protocol types (feature-gated)

**Depends on:** nothing (uses existing `registry::{Role, Data}`). **Parallel-safe with:** Tasks 2, 3.

**Files:**
- Create: `crates/devkit-ports/src/daemon/mod.rs`
- Create: `crates/devkit-ports/src/daemon/proto.rs`
- Modify: `crates/devkit-ports/src/lib.rs` (gated module)
- Modify: `crates/devkit-ports/Cargo.toml` (add `daemon` feature)

- [ ] **Step 1: Add the feature and gate the module**

In `crates/devkit-ports/Cargo.toml` add at the end:

```toml
[features]
daemon = []
```

In `crates/devkit-ports/src/lib.rs` add:

```rust
#[cfg(feature = "daemon")]
pub mod daemon;
```

Create `crates/devkit-ports/src/daemon/mod.rs`:

```rust
pub mod proto;
pub mod client;
```

(Task 5 creates `client.rs`. To keep this task compiling on its own, temporarily comment the `pub mod client;` line, or create an empty `client.rs` with `// filled in Task 5`. If running Wave 1 and Wave 2 in parallel worktrees, create a stub `client.rs` containing only `//! placeholder` so the module resolves.)

- [ ] **Step 2: Write the failing test (round-trip framing)**

Create `crates/devkit-ports/src/daemon/proto.rs` ending with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Role;

    #[test]
    fn frames_roundtrip_over_a_pipe() {
        let reqs = vec![("api".to_string(), 9100u16)];
        let msg = Request::Alloc { holder: "/w".into(), reqs, role: Role::Issue };
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &msg).unwrap();
        assert_eq!(*buf.last().unwrap(), b'\n', "frame must be newline-terminated");
        let mut rdr = std::io::BufReader::new(&buf[..]);
        let back: Request = recv(&mut rdr).unwrap().expect("one frame");
        match back {
            Request::Alloc { holder, role, .. } => {
                assert_eq!(holder, "/w");
                assert_eq!(role, Role::Issue);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn recv_returns_none_on_eof() {
        let mut rdr = std::io::BufReader::new(&b""[..]);
        let got: Option<Request> = recv(&mut rdr).unwrap();
        assert!(got.is_none());
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `cargo test -p devkit-ports --features daemon proto::`
Expected: FAIL — `Request`/`send`/`recv` undefined.

- [ ] **Step 4: Implement the protocol**

Put at the top of `crates/devkit-ports/src/daemon/proto.rs`:

```rust
use crate::registry::{Data, Role};
use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Wire-format version. Bump on any incompatible change to these types.
pub const PROTO: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Handshake — always the first frame on a connection.
    Ping { proto: u32 },
    // Registry ops (1:1 with the flock facade):
    Alloc { holder: String, reqs: Vec<(String, u16)>, role: Role },
    RecordPid { port: u16, app: String, holder: String, role: Role, pid: u32, logfile: PathBuf },
    Release { holder: String, role: Option<Role> },
    Snapshot,
    Prune,
    // Supervision (daemon-only):
    Supervise {
        holder: String, app: String, role: Role,
        argv: Vec<String>, cwd: String, env: BTreeMap<String, String>,
        logfile: PathBuf, base_port: u16,
    },
    Down { holder: String, role: Option<Role> },
    Tail { holder: String, app: String, role: Option<Role>, lines: usize },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Pong { proto: u32, pid: u32 },
    Ports(Vec<(String, u16)>),
    Snapshot(Data),
    Freed(Vec<u16>),
    /// (port, ready) for each supervised app started by a `Supervise` request.
    Supervised(Vec<(u16, bool)>),
    Lines(String),
    Ok,
    Err(String),
}

/// Write one newline-delimited JSON frame and flush.
pub fn send<W: Write>(w: &mut W, msg: &impl Serialize) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()?;
    Ok(())
}

/// Read one newline-delimited JSON frame. `Ok(None)` on clean EOF.
pub fn recv<R: BufRead, T: DeserializeOwned>(r: &mut R) -> Result<Option<T>> {
    let mut s = String::new();
    if r.read_line(&mut s)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(s.trim_end())?))
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports --features daemon proto::`
Expected: PASS.

- [ ] **Step 6: Verify the default build is unaffected**

Run: `cargo test -p devkit-ports` (no `--features daemon`) and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS — the `daemon` module isn't compiled without the feature.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-ports/Cargo.toml crates/devkit-ports/src/lib.rs crates/devkit-ports/src/daemon/
git commit -m "feat(daemon): IPC protocol types and JSON-line framing"
```

---

## Task 5: Daemon client — connect, handshake, route, autostart

**Depends on:** Tasks 2 (paths), 4 (proto). **Parallel-safe with:** Task 6 (disjoint crate).

**Files:**
- Create/replace: `crates/devkit-ports/src/daemon/client.rs`

- [ ] **Step 1: Write the failing test (proto-mismatch decision is pure & unit-testable)**

Create `crates/devkit-ports/src/daemon/client.rs` ending with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proto_match_decision() {
        assert!(handshake_ok(crate::daemon::proto::PROTO));
        assert!(!handshake_ok(crate::daemon::proto::PROTO + 1));
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-ports --features daemon client::`
Expected: FAIL — `handshake_ok` undefined.

- [ ] **Step 3: Implement the client**

Put at the top of `crates/devkit-ports/src/daemon/client.rs`:

```rust
use crate::daemon::proto::{self, Request, Response, PROTO};
use anyhow::{anyhow, Context, Result};
use devkit_common::paths;
use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// A live connection to the daemon. Reusable across requests.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: BufWriter<UnixStream>,
}

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

impl Client {
    fn from_stream(stream: UnixStream) -> Result<Self> {
        let reader = BufReader::new(stream.try_clone()?);
        let writer = BufWriter::new(stream);
        let mut c = Client { reader, writer };
        // Handshake: a proto mismatch means an old daemon survived a binary upgrade —
        // ask it to shut down so the caller can start a fresh one.
        match c.request(&Request::Ping { proto: PROTO })? {
            Response::Pong { proto, .. } if handshake_ok(proto) => Ok(c),
            Response::Pong { .. } => {
                let _ = c.request(&Request::Shutdown);
                Err(anyhow!("daemon proto mismatch"))
            }
            other => Err(anyhow!("unexpected handshake response: {other:?}")),
        }
    }

    /// Send one request, read one response.
    pub fn request(&mut self, req: &Request) -> Result<Response> {
        proto::send(&mut self.writer, req)?;
        proto::recv(&mut self.reader)?.ok_or_else(|| anyhow!("daemon closed connection"))
    }
}

/// Connect to an already-running daemon. Returns `None` if none is up or the
/// handshake fails — never autostarts. Used for opportunistic registry routing
/// (and by `status`, which must never spin a daemon up).
pub fn try_existing() -> Option<Client> {
    let stream = UnixStream::connect(paths::socket_file()).ok()?;
    Client::from_stream(stream).ok()
}

/// Locate the daemon binary: `$DEVKIT_PORTD_BIN`, else a sibling of the current
/// executable, else `devkit-portd` on `PATH`.
fn portd_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("DEVKIT_PORTD_BIN") {
        return p.into();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("devkit-portd");
        if sibling.is_file() {
            return sibling;
        }
    }
    std::path::PathBuf::from("devkit-portd")
}

/// Connect, autostarting a daemon if none is running. Used by supervision paths
/// (`devrun up --supervise`) — i.e. only when the run gate is on.
pub fn ensure_running() -> Result<Client> {
    if let Some(c) = try_existing() {
        return Ok(c);
    }
    std::process::Command::new(portd_bin())
        .spawn()
        .with_context(|| "spawning devkit-portd")?;
    // Poll the socket until the daemon accepts (it binds after taking its lock).
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(c) = try_existing() {
            return Ok(c);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!("daemon did not come up within 5s"))
}
```

- [ ] **Step 4: Run the unit test**

Run: `cargo test -p devkit-ports --features daemon client::`
Expected: PASS. (Connection paths are exercised end-to-end in Task 11.)

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/daemon/client.rs
git commit -m "feat(daemon): client connect/handshake/autostart with flock fallback seam"
```

---

## Task 6: `devkit-portd` crate skeleton — lock, socket, accept loop, idle-exit

**Depends on:** Tasks 2 (paths), 4 (proto). **Parallel-safe with:** Task 5 (disjoint crate).

This task stands up the daemon process with a stub dispatcher (Ping/Shutdown only). Task 7 adds supervision; Task 8 wires real registry handlers.

**Files:**
- Create: `crates/devkit-portd/Cargo.toml`
- Create: `crates/devkit-portd/src/main.rs`

- [ ] **Step 1: Create the crate manifest**

`crates/devkit-portd/Cargo.toml`:

```toml
[package]
name = "devkit-portd"
edition.workspace = true
version.workspace = true

[dependencies]
anyhow.workspace = true
serde = { workspace = true }
serde_json.workspace = true
fd-lock.workspace = true
nix = { workspace = true, features = ["signal", "process"] }
devkit-common.workspace = true
devkit-ports = { workspace = true, features = ["daemon"] }
```

(`crates/*` is already a workspace member glob, so no root edit is needed.)

- [ ] **Step 2: Implement the skeleton daemon**

`crates/devkit-portd/src/main.rs`:

```rust
//! devkit-portd — optional supervisor daemon. Single-instance (portd.lock),
//! binds a unix socket, serves one request per line. Supervision is added in a
//! later module; this entry point owns lifecycle: lock, bind, accept, idle-exit.

use anyhow::{Context, Result};
use devkit_common::paths;
use devkit_ports::daemon::proto::{self, Request, Response, PROTO};
use fd_lock::RwLock;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Shared daemon state. The supervision module (Task 7) extends this.
pub struct Daemon {
    pub started: Instant,
    pub last_activity: Mutex<Instant>,
    pub active_conns: AtomicUsize,
    pub shutdown: AtomicBool,
    pub idle_timeout: Duration,
}

impl Daemon {
    fn touch(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }
    /// Idle = no live connections and (Task 7) no supervised children, for longer
    /// than the timeout. Supervision suppresses this by keeping `supervising()` true.
    fn is_idle(&self) -> bool {
        self.active_conns.load(Ordering::SeqCst) == 0
            && !self.supervising()
            && self.last_activity.lock().unwrap().elapsed() >= self.idle_timeout
    }
    /// Overridden behavior arrives in Task 7; the skeleton supervises nothing.
    fn supervising(&self) -> bool {
        false
    }
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkit-portd");
    std::fs::create_dir_all(paths::state_dir())?;
    std::fs::create_dir_all(paths::logs_dir())?;

    // Single-instance: hold portd.lock for the daemon's whole life. If another
    // daemon holds it, exit 0 — autostart races resolve to exactly one winner.
    let lock_path = paths::daemon_lock_file();
    let _ = OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
    let mut lock = RwLock::new(File::open(&lock_path)?);
    let guard = match lock.try_write() {
        Ok(g) => g,
        Err(_) => return Ok(()), // another daemon already running
    };

    // Holding the lock, no live daemon owns the socket — clear any stale one and bind.
    let sock = paths::socket_file();
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;

    let idle_timeout = std::env::var("DEVKIT_DAEMON_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(1800));

    let daemon = Arc::new(Daemon {
        started: Instant::now(),
        last_activity: Mutex::new(Instant::now()),
        active_conns: AtomicUsize::new(0),
        shutdown: AtomicBool::new(false),
        idle_timeout,
    });

    // Idle-exit watcher: unblock the accept loop by connecting to ourselves.
    {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            if d.shutdown.load(Ordering::SeqCst) || d.is_idle() {
                d.shutdown.store(true, Ordering::SeqCst);
                let _ = UnixStream::connect(paths::socket_file()); // wake accept()
                break;
            }
        });
    }

    for stream in listener.incoming() {
        if daemon.shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let d = Arc::clone(&daemon);
        // A panicking handler would abort the whole daemon (panic=abort), so handlers
        // return Result and we only log failures here.
        std::thread::spawn(move || {
            d.active_conns.fetch_add(1, Ordering::SeqCst);
            d.touch();
            if let Err(e) = handle_conn(&d, stream) {
                log_line(&format!("connection error: {e:#}"));
            }
            d.active_conns.fetch_sub(1, Ordering::SeqCst);
            d.touch();
        });
    }

    // Clean shutdown: drop the socket and release the lock.
    let _ = std::fs::remove_file(paths::socket_file());
    drop(guard);
    Ok(())
}

/// Serve requests on one connection until EOF or Shutdown. The skeleton answers
/// Ping/Shutdown; every other request is dispatched in Task 8.
fn handle_conn(daemon: &Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);
    while let Some(req) = proto::recv::<_, Request>(&mut reader)? {
        daemon.touch();
        let resp = dispatch(daemon, req);
        let stop = matches!(resp, Response::Ok if false); // placeholder; see below
        proto::send(&mut writer, &resp)?;
        let _ = stop;
    }
    Ok(())
}

fn dispatch(daemon: &Arc<Daemon>, req: Request) -> Response {
    match req {
        Request::Ping { .. } => Response::Pong { proto: PROTO, pid: std::process::id() },
        Request::Shutdown => {
            daemon.shutdown.store(true, Ordering::SeqCst);
            let _ = UnixStream::connect(paths::socket_file());
            Response::Ok
        }
        // Registry + supervision handlers are added in Task 8.
        _ => Response::Err("not implemented in skeleton".into()),
    }
}

fn log_line(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(paths::daemon_log()) {
        let _ = writeln!(f, "{msg}");
    }
}
```

> **Implementer note:** remove the `stop`/placeholder lines once Task 8 lands real handlers; they exist only so the skeleton compiles and exercises the loop. Keep `handle_conn` returning `Result`.

- [ ] **Step 3: Build the daemon**

Run: `cargo build -p devkit-portd`
Expected: PASS.

- [ ] **Step 4: Smoke-test lifecycle by hand (idle-exit + ping)**

Run:

```bash
DEVKIT_DAEMON_IDLE_SECS=2 ./target/debug/devkit-portd &
sleep 0.3
# handshake: expect a Pong line
printf '{"Ping":{"proto":1}}\n' | nc -U ~/.claude/state/devkit/portd.sock
sleep 3   # exceeds idle timeout → daemon exits, socket removed
test ! -S ~/.claude/state/devkit/portd.sock && echo "idle-exit OK"
```

Expected: a `{"Pong":...}` line, then `idle-exit OK`. (If `nc -U` is unavailable, skip — Task 11 covers this with a Rust client.)

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-portd/
git commit -m "feat(portd): daemon skeleton — single-instance lock, socket, idle-exit"
```

---

## Task 7: Supervisor — child table, reap, restart/backoff, adoption, memory

**Depends on:** Tasks 1 (supervise+tree_rss), 4 (proto), 6 (Daemon skeleton). **Parallel-safe with:** nothing (extends the portd crate).

**Files:**
- Create: `crates/devkit-portd/src/supervisor.rs`
- Modify: `crates/devkit-portd/src/main.rs` (declare `mod supervisor;`, give `Daemon` a `Supervisor`, replace `supervising()`)

- [ ] **Step 1: Declare the module and embed it in `Daemon`**

In `crates/devkit-portd/src/main.rs`:
- add `mod supervisor;` near the top;
- add a field `pub sup: Mutex<supervisor::Supervisor>` to `Daemon` and initialize it in `main` (`sup: Mutex::new(supervisor::Supervisor::new(cfg_max_restarts, cfg_window, mem_warn, mem_limit))` — read those from `DEVKIT_DAEMON_*` env or defaults; the config-aware wiring is finalized in Task 8/10);
- replace `fn supervising(&self)` body with `self.sup.lock().unwrap().any_live()`.

For this task use env-driven knobs so the daemon is self-contained:

```rust
let max_restarts = env_u32("DEVKIT_DAEMON_MAX_RESTARTS", 5);
let restart_window = Duration::from_secs(env_u64("DEVKIT_DAEMON_RESTART_WINDOW", 60));
let mem_warn = env_u64("DEVKIT_DAEMON_MEM_WARN_MB", 0) * 1024 * 1024;
let mem_limit = env_u64("DEVKIT_DAEMON_MEM_LIMIT_MB", 0) * 1024 * 1024;
```

Add small helpers `env_u32`/`env_u64` next to `main`:

```rust
fn env_u64(k: &str, d: u64) -> u64 { std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d) }
fn env_u32(k: &str, d: u32) -> u32 { std::env::var(k).ok().and_then(|s| s.parse().ok()).unwrap_or(d) }
```

- [ ] **Step 2: Write the failing tests**

Create `crates/devkit-portd/src/supervisor.rs` ending with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sup() -> Supervisor { Supervisor::new(2, Duration::from_secs(60), 0, 0) }

    #[test]
    fn restart_budget_blocks_after_max() {
        let mut s = sup();
        assert!(s.may_restart("/w", "api", Role::Issue)); // 1
        assert!(s.may_restart("/w", "api", Role::Issue)); // 2
        assert!(!s.may_restart("/w", "api", Role::Issue)); // exhausted (max=2)
    }

    #[test]
    fn restart_budget_is_per_child() {
        let mut s = sup();
        assert!(s.may_restart("/w", "api", Role::Issue));
        assert!(s.may_restart("/w", "api", Role::Issue));
        assert!(s.may_restart("/w", "lab-os", Role::Issue)); // different child, own budget
    }

    #[test]
    fn reaps_a_real_child_and_records_exit() {
        let mut s = sup();
        // A child that exits immediately.
        let argv: Vec<String> = ["true"].iter().map(|x| x.to_string()).collect();
        let key = Key { holder: "/w".into(), app: "api".into(), role: Role::Issue };
        let pid = devkit_common::supervise::spawn_detached(
            &argv, ".", &std::collections::BTreeMap::new(),
            &std::env::temp_dir().join("portd-test.log"),
        ).unwrap();
        s.insert_owned(key.clone(), pid, 9100, std::env::temp_dir().join("portd-test.log"));
        // Give `true` a moment to exit, then reap.
        std::thread::sleep(Duration::from_millis(200));
        let exited = s.reap_once();
        assert!(exited.iter().any(|k| k == &key), "child should be reaped");
    }
}
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo test -p devkit-portd supervisor::`
Expected: FAIL — `Supervisor`/`Key`/methods undefined.

- [ ] **Step 4: Implement the supervisor**

Put at the top of `crates/devkit-portd/src/supervisor.rs`:

```rust
use devkit_common::supervise::tree_rss_bytes;
use devkit_ports::registry::{self, Role};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Identity of a supervised server, matching its `ports.json` row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub holder: String,
    pub app: String,
    pub role: Role,
}

/// How the daemon watches a process: `Owned` children are reaped with `waitpid`;
/// `Adopted` survivors (from a previous daemon) are polled with `pid_alive`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Watch { Owned, Adopted }

struct Child {
    pid: u32,
    port: u16,
    logfile: PathBuf,
    watch: Watch,
    restarts: Vec<Instant>,
    warned_mem: bool,
}

pub struct Supervisor {
    children: HashMap<Key, Child>,
    max_restarts: u32,
    window: Duration,
    mem_warn: u64,
    mem_limit: u64,
}

impl Supervisor {
    pub fn new(max_restarts: u32, window: Duration, mem_warn: u64, mem_limit: u64) -> Self {
        Supervisor { children: HashMap::new(), max_restarts, window, mem_warn, mem_limit }
    }

    pub fn any_live(&self) -> bool { !self.children.is_empty() }

    pub fn insert_owned(&mut self, key: Key, pid: u32, port: u16, logfile: PathBuf) {
        self.children.insert(key, Child {
            pid, port, logfile, watch: Watch::Owned, restarts: Vec::new(), warned_mem: false,
        });
    }

    pub fn insert_adopted(&mut self, key: Key, pid: u32, port: u16, logfile: PathBuf) {
        self.children.insert(key, Child {
            pid, port, logfile, watch: Watch::Adopted, restarts: Vec::new(), warned_mem: false,
        });
    }

    pub fn remove(&mut self, key: &Key) -> Option<u32> {
        self.children.remove(key).map(|c| c.pid)
    }

    pub fn logfile_of(&self, key: &Key) -> Option<PathBuf> {
        self.children.get(key).map(|c| c.logfile.clone())
    }

    /// Record a restart attempt against the crash-loop budget; returns whether one
    /// is still allowed in the current window. Shared by crash- and (future)
    /// memory-triggered restarts so a server can't be restart-looped forever.
    pub fn may_restart(&mut self, holder: &str, app: &str, role: Role) -> bool {
        let key = Key { holder: holder.into(), app: app.into(), role };
        let now = Instant::now();
        let window = self.window;
        let entry = self.children.entry(key).or_insert_with(|| Child {
            pid: 0, port: 0, logfile: PathBuf::new(), watch: Watch::Owned,
            restarts: Vec::new(), warned_mem: false,
        });
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
    pub fn reap_once(&mut self) -> Vec<Key> {
        use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
        use nix::unistd::Pid;
        let mut dead = Vec::new();
        for (key, child) in self.children.iter() {
            let gone = match child.watch {
                Watch::Owned => match waitpid(Pid::from_raw(child.pid as i32), Some(WaitPidFlag::WNOHANG)) {
                    Ok(WaitStatus::StillAlive) => false,
                    Ok(_) => true,                 // exited/signaled → reaped
                    Err(_) => true,                // ECHILD etc. → treat as gone
                },
                Watch::Adopted => !registry::pid_alive(child.pid),
            };
            if gone { dead.push(key.clone()); }
        }
        dead
    }

    /// Memory breaches to act on this tick: returns `(Key, bytes)` for each child
    /// whose supervised process-tree RSS crosses `mem_warn`. Each child warns once
    /// per breach (re-armed when it drops back below the threshold).
    pub fn memory_breaches(&mut self) -> Vec<(Key, u64)> {
        if self.mem_warn == 0 { return Vec::new(); }
        let mut breaches = Vec::new();
        for (key, child) in self.children.iter_mut() {
            if child.pid == 0 { continue; }
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

    pub fn mem_limit(&self) -> u64 { self.mem_limit }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-portd supervisor::`
Expected: PASS.

- [ ] **Step 6: Build the crate**

Run: `cargo build -p devkit-portd && cargo clippy -p devkit-portd --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-portd/
git commit -m "feat(portd): supervisor table — reap, crash-loop budget, memory tracking, adoption"
```

---

## Task 8: Server dispatch + supervision thread + adoption on startup

**Depends on:** Tasks 4 (proto), 6 (skeleton), 7 (supervisor); reads `registry` facade. **Parallel-safe with:** Task 9 (disjoint files).

Wires real handlers (registry ops + `Supervise`/`Down`/`Tail`), starts the supervision thread (reap → restart/let-die via `ports.json`, memory warnings, debounce for the legacy `down` race), and adopts survivors at startup.

**Files:**
- Create: `crates/devkit-portd/src/server.rs`
- Modify: `crates/devkit-portd/src/main.rs` (`mod server;`, call `server::dispatch`, spawn the supervision thread, adopt at startup)

- [ ] **Step 1: Implement the dispatcher**

Create `crates/devkit-portd/src/server.rs`:

```rust
//! Request handlers. Registry ops call the very same flock facade the no-daemon
//! path uses (the daemon is a flock participant). Supervision ops own processes.

use crate::supervisor::Key;
use crate::Daemon;
use devkit_common::supervise::{self};
use devkit_ports::daemon::proto::{Request, Response, PROTO};
use devkit_ports::registry::{self, Role};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// Map a request to a response. Returns `(response, should_close)`.
pub fn dispatch(daemon: &Arc<Daemon>, req: Request) -> (Response, bool) {
    match req {
        Request::Ping { .. } => (Response::Pong { proto: PROTO, pid: std::process::id() }, false),

        Request::Alloc { holder, reqs, role } => match registry::alloc(&holder, &reqs, role) {
            Ok(ports) => (Response::Ports(ports), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },
        Request::RecordPid { port, app, holder, role, pid, logfile } => {
            match registry::record_pid(port, &app, &holder, role, pid, logfile) {
                Ok(()) => (Response::Ok, false),
                Err(e) => (Response::Err(format!("{e:#}")), false),
            }
        }
        Request::Release { holder, role } => match registry::release(&holder, role) {
            Ok(freed) => (Response::Freed(freed), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },
        Request::Snapshot => match registry::snapshot() {
            Ok(data) => (Response::Snapshot(data), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },
        Request::Prune => match registry::prune() {
            Ok(freed) => (Response::Freed(freed), false),
            Err(e) => (Response::Err(format!("{e:#}")), false),
        },

        Request::Supervise { holder, app, role, argv, cwd, env, logfile, base_port } => {
            (supervise_app(daemon, holder, app, role, argv, cwd, env, logfile, base_port), false)
        }
        Request::Down { holder, role } => (down(daemon, holder, role), false),
        Request::Tail { holder, app, role, lines } => (tail(holder, app, role, lines), false),

        Request::Shutdown => {
            daemon.shutdown.store(true, Ordering::SeqCst);
            let _ = std::os::unix::net::UnixStream::connect(devkit_common::paths::socket_file());
            (Response::Ok, true)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn supervise_app(
    daemon: &Arc<Daemon>, holder: String, app: String, role: Role,
    argv: Vec<String>, cwd: String, env: std::collections::BTreeMap<String, String>,
    logfile: std::path::PathBuf, base_port: u16,
) -> Response {
    // Reserve before bind (same invariant as the flock path).
    let reqs = vec![(app.clone(), base_port)];
    let port = match registry::alloc(&holder, &reqs, role) {
        Ok(p) => p.into_iter().find(|(a, _)| *a == app).map(|(_, p)| p),
        Err(e) => return Response::Err(format!("{e:#}")),
    };
    let Some(port) = port else { return Response::Err("alloc returned no port".into()) };

    let pid = match supervise::spawn_detached(&argv, &cwd, &env, &logfile) {
        Ok(pid) => pid,
        Err(e) => return Response::Err(format!("{e:#}")),
    };
    if let Err(e) = registry::record_pid(port, &app, &holder, role, pid, logfile.clone()) {
        return Response::Err(format!("{e:#}"));
    }
    daemon.sup.lock().unwrap().insert_owned(
        Key { holder, app, role }, pid, port, logfile,
    );
    let ready = supervise::wait_ready(port, Duration::from_secs(120));
    Response::Supervised(vec![(port, ready)])
}

/// Atomic stop: mark stopping (remove from the child table so the supervision
/// thread won't restart it), SIGTERM, then release the row.
fn down(daemon: &Arc<Daemon>, holder: String, role: Option<Role>) -> Response {
    let mut sup = daemon.sup.lock().unwrap();
    // Remove every supervised child for this holder/role and SIGTERM it.
    let keys: Vec<Key> = registry::snapshot()
        .map(|d| {
            d.entries.values()
                .filter(|e| e.holder == holder && role.is_none_or(|r| e.role == r))
                .map(|e| Key { holder: e.holder.clone(), app: e.app.clone(), role: e.role })
                .collect()
        })
        .unwrap_or_default();
    for k in &keys {
        if let Some(pid) = sup.remove(k) {
            supervise::stop(pid);
        }
    }
    drop(sup);
    match registry::release(&holder, role) {
        Ok(freed) => Response::Freed(freed),
        Err(e) => Response::Err(format!("{e:#}")),
    }
}

fn tail(holder: String, app: String, role: Option<Role>, lines: usize) -> Response {
    match registry::snapshot() {
        Ok(d) => {
            let log = d.entries.values()
                .find(|e| e.holder == holder && e.app == app && role.is_none_or(|r| e.role == r))
                .and_then(|e| e.logfile.clone());
            match log {
                Some(p) => Response::Lines(supervise::tail(&p, lines)),
                None => Response::Err(format!("no tracked log for `{app}`")),
            }
        }
        Err(e) => Response::Err(format!("{e:#}")),
    }
}
```

- [ ] **Step 2: Wire dispatch, the supervision thread, and adoption into `main.rs`**

In `crates/devkit-portd/src/main.rs`:
- add `mod server;`
- replace the skeleton `dispatch` call in `handle_conn` with `server::dispatch`, honoring the close flag:

```rust
    while let Some(req) = proto::recv::<_, Request>(&mut reader)? {
        daemon.touch();
        let (resp, close) = server::dispatch(daemon, req);
        proto::send(&mut writer, &resp)?;
        if close { break; }
    }
```

- delete the skeleton's local `dispatch` fn (now in `server.rs`).
- **Adopt survivors at startup** (after binding the socket, before the accept loop):

```rust
    // Adopt servers a previous daemon left running: monitor by poll, not waitpid.
    if let Ok(data) = registry::snapshot() {
        let mut sup = daemon.sup.lock().unwrap();
        for (port, e) in &data.entries {
            if let (Some(pid), Some(log)) = (e.pid, e.logfile.clone())
                && registry::pid_alive(pid)
            {
                sup.insert_adopted(
                    supervisor::Key { holder: e.holder.clone(), app: e.app.clone(), role: e.role },
                    pid, *port, log,
                );
            }
        }
    }
```

(add `use devkit_ports::registry;` to `main.rs`.)

- **Supervision thread** (replace the idle-only watcher thread with one that also reaps, restarts, and warns on memory):

```rust
    {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(500));
            if d.shutdown.load(Ordering::SeqCst) || d.is_idle() {
                d.shutdown.store(true, Ordering::SeqCst);
                let _ = UnixStream::connect(paths::socket_file());
                break;
            }
            // Reap exited children; restart only those whose ports.json row survives
            // (the cross-tool stop signal). Debounce the read so a concurrent legacy
            // `down` that removes the row just before the exit isn't misread as a crash.
            let dead = d.sup.lock().unwrap().reap_once();
            if !dead.is_empty() {
                std::thread::sleep(Duration::from_millis(200)); // debounce
                let snap = registry::snapshot().unwrap_or_default();
                for key in dead {
                    let row = snap.entries.values().find(|e|
                        e.holder == key.holder && e.app == key.app && e.role == key.role);
                    match row {
                        Some(_) => restart(&d, &key),
                        None => { d.sup.lock().unwrap().remove(&key); } // intentional stop
                    }
                }
            }
            // Memory: warn once per breach (v1 action is warn-only).
            for (key, rss) in d.sup.lock().unwrap().memory_breaches() {
                log_line(&format!(
                    "memory: {}/{} ({:?}) tree-RSS {} MB exceeds warn threshold",
                    key.holder, key.app, key.role, rss / 1024 / 1024));
            }
        });
    }
```

- add the `restart` helper to `main.rs`:

```rust
/// Respawn a crashed child if its crash-loop budget allows; otherwise leave the
/// row with pid cleared and log. Re-uses the recorded argv/cwd/env? The daemon
/// does not persist those, so restart re-launches from the stored logfile context
/// by asking the supervisor for what it needs.
fn restart(daemon: &Arc<Daemon>, key: &supervisor::Key) {
    let mut sup = daemon.sup.lock().unwrap();
    if !sup.may_restart(&key.holder, &key.app, key.role) {
        sup.remove(key);
        drop(sup);
        log_line(&format!("giving up on {}/{} ({:?}) — crash-loop budget exhausted",
            key.holder, key.app, key.role));
        return;
    }
    drop(sup);
    log_line(&format!("restart: {}/{} ({:?})", key.holder, key.app, key.role));
    // Re-launch is performed by re-issuing the original Supervise parameters, which
    // the supervisor must retain. See Step 3.
    daemon.respawn(key);
}
```

- [ ] **Step 3: Make restart actually re-launch (retain launch spec)**

The supervisor needs the original `argv`/`cwd`/`env`/`base_port` to respawn. Extend `Child` and `insert_owned` in `crates/devkit-portd/src/supervisor.rs` to store a `Launch`:

```rust
#[derive(Clone)]
pub struct Launch {
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: std::collections::BTreeMap<String, String>,
    pub base_port: u16,
}
```

- add `launch: Launch` to `Child`;
- change `insert_owned(&mut self, key, pid, port, logfile, launch: Launch)` and `insert_adopted(... , launch: Launch)` to take and store it (adopted children won't be restarted from a known launch until they crash-and-respawn; store the best-known launch, or `None`. Use `Option<Launch>` to be honest: adopted = `None`, owned = `Some`). Update the call sites in `server.rs`/`main.rs` accordingly.
- add a method:

```rust
    /// Launch spec + logfile for respawning a key, if known (owned children only).
    pub fn launch_of(&self, key: &Key) -> Option<(Launch, std::path::PathBuf)> {
        let c = self.children.get(key)?;
        Some((c.launch.clone()?, c.logfile.clone()))
    }

    /// Update a key's pid after a successful respawn.
    pub fn set_pid(&mut self, key: &Key, pid: u32) {
        if let Some(c) = self.children.get_mut(key) { c.pid = pid; c.watch = Watch::Owned; }
    }
```

(make `Child.launch` an `Option<Launch>`; adjust the `may_restart` placeholder-insert to use `launch: None`.)

- add `Daemon::respawn` to `main.rs`:

```rust
impl Daemon {
    fn respawn(self: &Arc<Self>, key: &supervisor::Key) {
        let Some((launch, log)) = self.sup.lock().unwrap().launch_of(key) else {
            log_line(&format!("cannot respawn {}/{} — no launch spec", key.holder, key.app));
            return;
        };
        match devkit_common::supervise::spawn_detached(&launch.argv, &launch.cwd, &launch.env, &log) {
            Ok(pid) => {
                let _ = registry::record_pid(key.port_app(), &key.app, &key.holder, key.role, pid, log);
                self.sup.lock().unwrap().set_pid(key, pid);
            }
            Err(e) => log_line(&format!("respawn failed for {}/{}: {e:#}", key.holder, key.app)),
        }
    }
}
```

> **Implementer note:** `record_pid` needs the port. Store `port` on `Key`’s child and expose `port_of(key)` on the supervisor (the snapshot row also has it). Resolve the port from the supervisor’s `Child.port` rather than `key`; adjust the `respawn` body to fetch `(launch, log, port)` together. Keep `Key` as just identity (holder/app/role).

- [ ] **Step 4: Write the failing integration test (restart-on-crash)**

Create `crates/devkit-portd/tests/restart.rs`:

```rust
// Spawns a real daemon on a temp HOME, supervises a server that crashes once,
// and asserts the daemon respawns it (new pid, row still present).
mod common; // Task 11 provides the harness; see note.

#[test]
fn restarts_a_crashed_child() {
    let h = common::Harness::start();
    // A server that listens briefly then exits, so the first instance "crashes".
    let port = h.free_port();
    let pid1 = h.supervise_crashing_server(port);
    let pid2 = h.wait_for_respawn(port, pid1);
    assert_ne!(pid1, pid2, "daemon should have respawned the crashed server");
    h.shutdown();
}
```

> **Ordering note:** the `common` harness is built in Task 11. If implementing strictly task-by-task, write this test’s body in Task 11 alongside the harness and keep Task 8 focused on the unit-level dispatch (Step 5). The restart logic is still exercised by Task 11’s `restart` test.

- [ ] **Step 5: Build, clippy, and run unit tests**

Run: `cargo build -p devkit-portd && cargo clippy -p devkit-portd --all-targets -- -D warnings && cargo test -p devkit-portd`
Expected: PASS (integration tests needing the harness are added in Task 11).

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-portd/
git commit -m "feat(portd): request dispatch, supervision thread, restart, adoption, down coordination"
```

---

## Task 9: Registry facade routes daemon-first, else flock

**Depends on:** Task 5 (client). **Parallel-safe with:** Tasks 6, 7, 8 (edits `registry.rs` only).

Make `alloc`/`record_pid`/`release`/`snapshot`/`prune` use a running daemon when one is up (and the `daemon` feature is compiled), else fall through to the existing flock code unchanged. Fallback is safe because every op is idempotent.

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs`

- [ ] **Step 1: Add a routing helper (feature-gated)**

Add near the top of `crates/devkit-ports/src/registry.rs`:

```rust
/// Try a running daemon first; `None` means "no daemon — use flock". Any daemon
/// error is logged and also yields `None` so the caller falls back safely.
#[cfg(feature = "daemon")]
fn via_daemon(req: crate::daemon::proto::Request) -> Option<crate::daemon::proto::Response> {
    let mut c = crate::daemon::client::try_existing()?;
    match c.request(&req) {
        Ok(resp) => Some(resp),
        Err(e) => {
            eprintln!("warning: daemon request failed ({e:#}); using flock");
            None
        }
    }
}
```

- [ ] **Step 2: Route `snapshot` (representative; apply the same shape to the others)**

Wrap the existing `snapshot` body. Rename the current function to `snapshot_flock` and add the router:

```rust
pub fn snapshot() -> Result<Data> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = via_daemon(crate::daemon::proto::Request::Snapshot) {
        return match resp {
            crate::daemon::proto::Response::Snapshot(d) => Ok(d),
            crate::daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    snapshot_flock()
}

fn snapshot_flock() -> Result<Data> {
    // ... the existing snapshot body, verbatim ...
}
```

- [ ] **Step 3: Apply the identical pattern to `alloc`, `record_pid`, `release`, `prune`**

For each, rename the existing function to `<name>_flock` and add a router that maps to/from the protocol:
- `alloc` → `Request::Alloc { holder, reqs, role }` → `Response::Ports(v)`;
- `record_pid` → `Request::RecordPid { .. }` → `Response::Ok`;
- `release` → `Request::Release { holder, role }` → `Response::Freed(v)`;
- `prune` → `Request::Prune` → `Response::Freed(v)`.

Example for `alloc`:

```rust
pub fn alloc(holder: &str, reqs: &[(String, u16)], role: Role) -> Result<Vec<(String, u16)>> {
    #[cfg(feature = "daemon")]
    if let Some(resp) = via_daemon(crate::daemon::proto::Request::Alloc {
        holder: holder.to_string(), reqs: reqs.to_vec(), role,
    }) {
        return match resp {
            crate::daemon::proto::Response::Ports(v) => Ok(v),
            crate::daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    alloc_flock(holder, reqs, role)
}

fn alloc_flock(holder: &str, reqs: &[(String, u16)], role: Role) -> Result<Vec<(String, u16)>> {
    // ... the existing alloc body, verbatim ...
}
```

> **Important — avoid recursion in the daemon.** The daemon’s own handlers (Task 8) call `registry::alloc`/`snapshot`/etc. Those must hit the **flock** path, not re-enter the client. Since the daemon process never has a daemon to connect to as a client (it *is* the daemon, and `try_existing()` would connect to itself), guard against self-routing: in `via_daemon`, return `None` when `std::env::var_os("DEVKIT_PORTD_SELF").is_some()`. Set that env var in `devkit-portd/src/main.rs` at startup (`unsafe { std::env::set_var("DEVKIT_PORTD_SELF", "1") }` before any handler runs). Add a comment explaining the self-routing guard.

- [ ] **Step 4: Add a test that the flock path is unchanged without the feature**

The existing `ops_tests`/`liveness_tests` already cover the flock functions (now `*_flock` are exercised through the public fns when the feature is off). Run them both ways:

Run: `cargo test -p devkit-ports` (feature off — routes are compiled out)
Run: `cargo test -p devkit-ports --features daemon` (feature on — `via_daemon` returns `None` because no daemon is running in unit tests, so flock still runs)
Expected: PASS in both.

- [ ] **Step 5: Clippy and commit**

Run: `cargo clippy --workspace --all-targets --features devkit-ports/daemon -- -D warnings` and the default `cargo clippy --workspace --all-targets -- -D warnings`.

```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "feat(registry): route facade through daemon when up, flock fallback"
```

---

## Task 10: `devrun` wiring — `up --supervise`, daemon-aware `down`/`logs`/`status`

**Depends on:** Tasks 5 (client), 9 (facade). **Parallel-safe with:** nothing (edits `devrun`).

**Files:**
- Modify: `crates/devrun/Cargo.toml` (forward the `daemon` feature)
- Modify: `crates/devrun/src/main.rs`

- [ ] **Step 1: Forward the feature**

In `crates/devrun/Cargo.toml` add:

```toml
[features]
daemon = ["devkit-ports/daemon"]
```

(Building `devrun` with `--features daemon` compiles in daemon routing; the default build is unchanged.)

- [ ] **Step 2: Add the `--supervise` flag to `up`**

In `crates/devrun/src/main.rs`, add to the `Up` variant:

```rust
        /// Hand servers to the supervisor daemon (autostarting it) so they restart on crash.
        #[arg(long)]
        supervise: bool,
```

Thread it through `main`'s match arm into `cmd_up(.., supervise)` and add the parameter to `cmd_up`’s signature.

- [ ] **Step 3: Route supervised `up` through the daemon (feature-gated)**

In `cmd_up`, where servers are spawned (the loop that calls `supervise::spawn_detached` + `registry::record_pid`), branch when `supervise` is set:

```rust
        #[cfg(feature = "daemon")]
        if supervise {
            let mut client = devkit_ports::daemon::client::ensure_running()
                .context("starting supervisor daemon")?;
            for (a, port, argv, app_cwd, envmap, log) in &plans {
                let resp = client.request(&devkit_ports::daemon::proto::Request::Supervise {
                    holder: holder.clone(),
                    app: a.clone(),
                    role: *grp_role,
                    argv: argv.clone(),
                    cwd: app_cwd.to_str().context("app cwd not UTF-8")?.to_string(),
                    env: envmap.clone(),
                    logfile: log.clone(),
                    base_port: catalog[a].base_port,
                })?;
                let ready = matches!(resp,
                    devkit_ports::daemon::proto::Response::Supervised(ref v)
                        if v.first().map(|(_, r)| *r).unwrap_or(false));
                rows.push(Row { role: *grp_role, app: a.clone(), port: *port, pid: None, log: log.clone(), ready: Some(ready) });
            }
            continue; // skip the direct-spawn path for this group
        }
```

(Place this branch right after the `dry_run` block and before the direct-spawn loop. The `pid` shows `-` for supervised rows because the daemon owns the pid; `status`/`logs` resolve it from `ports.json`.)

- [ ] **Step 4: `down`/`logs`/`status` need no special-casing**

They already call `registry::{with_lock, snapshot}` — but daemon-aware `down` should route through the daemon’s atomic `Down` so no restart races. Update `cmd_down`:

```rust
fn cmd_down(cwd: &str, role: Option<Role>) -> Result<()> {
    let holder = toplevel(cwd)?;
    #[cfg(feature = "daemon")]
    if let Some(mut client) = devkit_ports::daemon::client::try_existing() {
        let resp = client.request(&devkit_ports::daemon::proto::Request::Down {
            holder: holder.clone(), role,
        })?;
        if let devkit_ports::daemon::proto::Response::Freed(freed) = resp {
            println!("stopped via daemon; released ports {freed:?}");
            return Ok(());
        }
    }
    // flock path (existing): stop then release under one lock, no prune first.
    let mut stopped = 0;
    let freed = registry::with_lock(|d| {
        for e in d.entries.values() {
            if e.holder == holder && role.is_none_or(|r| e.role == r)
                && let Some(pid) = e.pid { supervise::stop(pid); stopped += 1; }
        }
        Ok(d.release(&holder, role))
    })?;
    println!("stopped {stopped} process(es); released ports {freed:?}");
    Ok(())
}
```

`cmd_status` and `cmd_logs` keep calling `registry::snapshot()` — which now routes through the daemon if up (Task 9) and never autostarts (`try_existing`). No change needed beyond confirming behavior in Step 5.

- [ ] **Step 5: Manual end-to-end check (feature build)**

Run:

```bash
cargo build -p devrun --features daemon -p devkit-portd
# from a worktree with a devkit.toml + an app:
./target/debug/devrun up <app> --supervise
./target/debug/devrun status        # shows the supervised server (pid owned by daemon)
./target/debug/devrun down          # routes Down; daemon stops + releases, no respawn
```

Expected: server comes up under the daemon; `status` lists it; `down` stops it with no restart.

- [ ] **Step 6: Default build unaffected**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS — without `--features daemon`, `devrun` behaves exactly as before.

- [ ] **Step 7: Commit**

```bash
git add crates/devrun/
git commit -m "feat(devrun): up --supervise, daemon-aware down, daemon-routed status/logs"
```

---

## Task 11: Integration test harness + scenario suite

**Depends on:** all prior tasks. **Parallel-safe with:** nothing.

A test harness that boots a real `devkit-portd` against a temp `HOME` and socket, plus the spec’s scenario tests. Lives in the `devkit-portd` crate so `CARGO_BIN_EXE_devkit-portd` is available.

**Files:**
- Create: `crates/devkit-portd/tests/common/mod.rs` (harness)
- Create: `crates/devkit-portd/tests/parity.rs`
- Create: `crates/devkit-portd/tests/lifecycle.rs`

- [ ] **Step 1: Build the harness**

`crates/devkit-portd/tests/common/mod.rs`:

```rust
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// A daemon running against a throwaway HOME/state dir.
pub struct Harness {
    pub home: PathBuf,
    child: Child,
}

impl Harness {
    pub fn start() -> Harness {
        let home = std::env::temp_dir().join(format!("portd-it-{}-{}", std::process::id(), unique()));
        std::fs::create_dir_all(home.join(".claude/state/devkit/logs")).unwrap();
        let bin = env!("CARGO_BIN_EXE_devkit-portd");
        let child = Command::new(bin)
            .env("HOME", &home)
            .env("DEVKIT_DAEMON_IDLE_SECS", "3600") // don't idle-exit mid-test unless asked
            .spawn()
            .expect("spawn devkit-portd");
        let h = Harness { home, child };
        h.wait_socket();
        h
    }

    pub fn start_with_idle(secs: u64) -> Harness {
        let home = std::env::temp_dir().join(format!("portd-it-{}-{}", std::process::id(), unique()));
        std::fs::create_dir_all(home.join(".claude/state/devkit/logs")).unwrap();
        let bin = env!("CARGO_BIN_EXE_devkit-portd");
        let child = Command::new(bin)
            .env("HOME", &home)
            .env("DEVKIT_DAEMON_IDLE_SECS", secs.to_string())
            .spawn()
            .expect("spawn devkit-portd");
        let h = Harness { home, child };
        h.wait_socket();
        h
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join(".claude/state/devkit/portd.sock")
    }

    fn wait_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if UnixStream::connect(self.socket()).is_ok() { return; }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("daemon socket never appeared");
    }

    /// Send one JSON request line, return the raw JSON response line.
    pub fn rpc(&self, json: &str) -> String {
        let stream = UnixStream::connect(self.socket()).expect("connect");
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        writeln!(writer, "{json}").unwrap();
        writer.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        line.trim_end().to_string()
    }

    pub fn socket_gone(&self) -> bool { !self.socket().exists() }

    pub fn shutdown(&mut self) {
        let _ = self.rpc(r#"{"Shutdown":null}"#);
        let _ = self.child.wait();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Monotonic-ish unique suffix without Date/rand: an atomic counter.
fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::SeqCst)
}
```

- [ ] **Step 2: Handshake + idle-exit lifecycle test**

`crates/devkit-portd/tests/lifecycle.rs`:

```rust
mod common;
use common::Harness;

#[test]
fn ping_pong_handshake() {
    let mut h = Harness::start();
    let resp = h.rpc(r#"{"Ping":{"proto":1}}"#);
    assert!(resp.contains("\"Pong\""), "got: {resp}");
    assert!(resp.contains("\"proto\":1"), "got: {resp}");
    h.shutdown();
}

#[test]
fn idle_exit_with_no_clients_or_children() {
    let h = Harness::start_with_idle(1);
    // No requests, nothing supervised → exits after ~1s.
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert!(h.socket_gone(), "daemon should have idle-exited and removed its socket");
}

#[test]
fn second_instance_exits_immediately() {
    let h = Harness::start();
    // A second daemon on the same HOME can't take portd.lock → exits 0, socket stays h's.
    let bin = env!("CARGO_BIN_EXE_devkit-portd");
    let status = std::process::Command::new(bin)
        .env("HOME", &h.home)
        .env("DEVKIT_DAEMON_IDLE_SECS", "3600")
        .status()
        .unwrap();
    assert!(status.success(), "second instance should exit 0");
    // Original still serves.
    let mut h = h;
    assert!(h.rpc(r#"{"Ping":{"proto":1}}"#).contains("Pong"));
    h.shutdown();
}
```

- [ ] **Step 3: Backend-parity test (daemon registry ops == flock)**

`crates/devkit-portd/tests/parity.rs`:

```rust
mod common;
use common::Harness;

// Alloc through the daemon, then read ports.json directly: the daemon must have
// written the same rows the flock path would. (The daemon calls the same facade.)
#[test]
fn alloc_through_daemon_writes_registry() {
    let mut h = Harness::start();
    let resp = h.rpc(r#"{"Alloc":{"holder":"/tmp/wt","reqs":[["api",19100]],"role":"issue"}}"#);
    assert!(resp.contains("\"Ports\""), "got: {resp}");

    // ports.json now has a row for /tmp/wt api.
    let reg = h.home.join(".claude/state/devkit/ports.json");
    let body = std::fs::read_to_string(&reg).unwrap();
    assert!(body.contains("\"api\""));
    assert!(body.contains("/tmp/wt"));

    // Release frees it.
    let resp = h.rpc(r#"{"Release":{"holder":"/tmp/wt","role":null}}"#);
    assert!(resp.contains("\"Freed\""), "got: {resp}");
    h.shutdown();
}

#[test]
fn snapshot_roundtrips() {
    let mut h = Harness::start();
    h.rpc(r#"{"Alloc":{"holder":"/tmp/wt2","reqs":[["api",19200]],"role":"issue"}}"#);
    let snap = h.rpc(r#"{"Snapshot":null}"#);
    assert!(snap.contains("\"Snapshot\""));
    assert!(snap.contains("/tmp/wt2"));
    h.rpc(r#"{"Release":{"holder":"/tmp/wt2","role":null}}"#);
    h.shutdown();
}
```

- [ ] **Step 4: Restart-on-crash + stop-coordination + adoption + memory**

Add to `lifecycle.rs` (these drive a real short-lived server; use a tiny inline server via `python3 -m http.server` for "stays up" and `true`/`sh -c 'sleep 0.3'` for "crashes"). Implement using the `Supervise`/`Down` RPCs with a real free port. Concretely:

```rust
#[test]
fn supervised_server_restarts_after_crash() {
    let mut h = Harness::start();
    let port = free_port();
    // A server that exits after ~400ms → daemon should respawn it (budget allows).
    let argv = serde_json::json!(["sh","-c",format!("sleep 0.4; exit 1")]);
    let req = serde_json::json!({"Supervise":{
        "holder":"/tmp/sup","app":"api","role":"issue",
        "argv":argv,"cwd":".","env":{}, "logfile": h.home.join("sup.log"),
        "base_port": port
    }}).to_string();
    let resp = h.rpc(&req);
    assert!(resp.contains("\"Supervised\""), "got: {resp}");

    // After the first instance exits, the row should persist with a *new* pid.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let snap = h.rpc(r#"{"Snapshot":null}"#);
    assert!(snap.contains("/tmp/sup"), "row should survive a crash-restart: {snap}");
    h.shutdown();
}

#[test]
fn down_via_daemon_does_not_restart() {
    let mut h = Harness::start();
    let port = free_port();
    let argv = serde_json::json!(["python3","-m","http.server",port.to_string()]);
    let req = serde_json::json!({"Supervise":{
        "holder":"/tmp/down","app":"api","role":"issue",
        "argv":argv,"cwd":".","env":{}, "logfile": h.home.join("down.log"),
        "base_port": port
    }}).to_string();
    h.rpc(&req);
    let resp = h.rpc(r#"{"Down":{"holder":"/tmp/down","role":null}}"#);
    assert!(resp.contains("\"Freed\""), "got: {resp}");
    // Give the supervision loop a few ticks; the row must stay gone (no respawn).
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let snap = h.rpc(r#"{"Snapshot":null}"#);
    assert!(!snap.contains("/tmp/down"), "Down must not be followed by a restart: {snap}");
    h.shutdown();
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}
```

> Add `serde_json` to `devkit-portd`’s `[dev-dependencies]` for building request JSON in tests (it’s already a normal dep, so this is automatically available to tests — no manifest change needed).
>
> **Adoption test:** pre-seed `ports.json` under the temp HOME with a live non-child pid (spawn `sleep 30` yourself, write a row pointing at it), start the daemon, assert `Snapshot` includes it and that killing the pid triggers a logged restart attempt. Implement if time allows; the `insert_adopted`/poll path is unit-covered in Task 7.

- [ ] **Step 5: Run the full suite**

Run: `cargo test -p devkit-portd`
Expected: PASS. Then the whole gate:
Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-portd/tests/
git commit -m "test(portd): harness + parity/lifecycle/restart/down/idle-exit suite"
```

---

## Final verification

- [ ] `cargo test --workspace` — default build, green (no behavior change).
- [ ] `cargo build --release` — all binaries build.
- [ ] `cargo build -p devkit-portd && cargo test -p devkit-portd` — daemon crate green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` and with `--features devkit-ports/daemon` — zero warnings both ways.
- [ ] Manual: `devrun up <app> --supervise` → kill the server process → it respawns; `devrun down` → no respawn; idle for the timeout → daemon exits.

---

## Unresolved questions for the implementer

1. **Restart needs the launch spec.** The plan stores `argv`/`cwd`/`env`/`base_port` on the supervisor `Child` so a crashed owned child can be respawned (Task 8, Step 3). Confirm this is acceptable memory cost (env maps can be large); if not, the alternative is persisting launch specs to a sidecar file and reloading on adoption — heavier, but enables restarting *adopted* children too. Current plan: owned children restart from memory; adopted children are monitored but only become restartable after their first daemon-owned respawn.
2. **`status` MEM column.** The spec’s §6a calls for a `MEM` column in status output. This plan tracks tree-RSS and warns, but wiring RSS into `registry::status_table` requires the daemon to surface live RSS through `Snapshot` (the `Entry` has no RSS field). Options: (a) add an optional `rss` to the `Snapshot` response computed by the daemon, or (b) defer the column to a follow-up. Plan currently logs RSS warnings but does **not** add the column — confirm whether to add it now (extra protocol field) or defer to `next-features.md`.
3. **Config → daemon knobs.** Task 7 reads `max_restarts`/memory thresholds from `DEVKIT_DAEMON_*` env vars (so the daemon is self-contained for tests). Production should pass the `[daemon]` config values; the cleanest path is for `client::ensure_running()` to set those env vars from the loaded `DaemonConfig` when it spawns the daemon. Confirm that approach vs. having the daemon load `devkit.toml` itself (it currently has no config-location context — the worktree cwd isn't meaningful to a long-lived daemon).
