# Authoritative In-Memory Lock Registry (unified `devkitd`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fold the lock registry into the existing daemon (renamed `devkitd`) so one process holds both registries in memory, serving lock reads from memory and writing through to `locks.json`, with `devkitd.lock` enforcing the daemon-vs-direct boundary.

**Architecture:** Rename the ports-specific daemon to a neutral `devkitd` and mirror the CLI names (`portm`/`lockm`). Extract the daemon framing/transport/client into `devkit-common::daemon` (feature-gated). Give `devkit-locks` the same `Store` seam (`FlockStore` + `MemoryStore`) the port registry already has, plus a resolved-context daemon split: the facade resolves project root + holder + pid client-side, then either sends them to the daemon over a second socket or runs the op against `FlockStore`. The daemon binds a second socket (`locks.sock`) and dispatches lock requests against its in-memory `Data`.

**Tech Stack:** Rust 2024, `anyhow`, `serde`/`serde_json`, `fd-lock` (advisory file locks), `interprocess` (local sockets), `clap`/`clap_complete`.

## Global Constraints

- **Spec:** `docs/superpowers/specs/2026-06-21-authoritative-in-memory-locks-design.md`. Every task implicitly includes this section.
- **Merge gate:** `cargo test --workspace` (all tests green) and `cargo clippy --workspace --all-targets -- -D warnings` (zero warnings) must pass at the end of every task.
- **Naming scheme (mirror ports and locks), applied in Task 1 and assumed thereafter:**
  - Daemon binary `devkit-portd` → `devkitd`; directory `src/bin/devkit-portd/` → `src/bin/devkitd/`.
  - CLI `portman` → `portm` (dir `src/bin/portman/` → `src/bin/portm/`); CLI `lock` → `lockm` (file `src/bin/lock.rs` → `src/bin/lockm.rs`).
  - State files: port socket `portd.sock` → `ports.sock`; daemon lock `portd.lock` → `devkitd.lock`; daemon log `portd.log` → `devkitd.log`. Lock socket is new: `locks.sock`.
  - `paths` accessors: `socket_file()` → `port_socket_file()`; `daemon_lock_file()` → `devkitd_lock()`; add `lock_socket_file()`. `daemon_log()` keeps its name, returns `devkitd.log`.
  - Env markers: `DEVKIT_PORTD_SELF` → `DEVKITD_SELF`; `DEVKIT_PORTD_BIN` → `DEVKITD_BIN`.
  - `devrun` and `issue` are unchanged.
- **Gate guarantee:** every direct registry *writer* (port or lock) takes `devkitd.lock` **shared** (non-blocking `try_read`) for its whole RMW and hard-errors (`DaemonHoldsLock`) if a daemon holds it exclusive. Reads are ungated.
- **Commit point:** `MemoryStore::commit` persists the file *before* swapping memory; on write failure memory is unchanged.
- **Daemon stamps `now`:** lock op cores take `now: u64`; the direct facade passes `now()`, the daemon passes its own clock.
- **`lockm` never autostarts** the daemon — `try_existing` only.
- **Conventional Commits**, footer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` (no other trailers). Commit after every task.
- **Timeless comments** — describe behavior, never the change/PR/task.
- **Project-agnostic** — no real project names; `example`/`exampleuser` placeholders only.

---

### Task 1: Rename to the mirrored scheme (`devkitd`, `portm`, `lockm`)

Pure mechanical rename, no behavior change. Use rust-analyzer (LSP) `rename`/find-references for Rust **symbols** (the `paths` accessors, any renamed items). LSP does **not** see string literals, file/dir names, `Cargo.toml`, or docs — sweep those with `rg` and edit by hand.

**Files:**
- Rename dir: `src/bin/devkit-portd/` → `src/bin/devkitd/`
- Rename dir: `src/bin/portman/` → `src/bin/portm/`
- Rename file: `src/bin/lock.rs` → `src/bin/lockm.rs`
- Modify: `Cargo.toml` (`[[bin]]` for `devkit-portd` → `devkitd`, path, `description`)
- Modify: `crates/devkit-common/src/paths.rs` (accessor names + literals + their tests)
- Modify: `crates/devkit-ports/src/registry.rs` (gate path accessor, `DaemonHoldsLock` message, `DEVKIT_PORTD_SELF`)
- Modify: `crates/devkit-ports/src/daemon/client.rs` (`DEVKIT_PORTD_BIN`, sibling bin name `devkit-portd`→`devkitd`)
- Modify: `crates/devkit-ports/src/daemon/transport.rs` (windows pipe prefix literal)
- Modify: `src/bin/devkitd/main.rs`, `src/bin/devkitd/server.rs` (`DEVKITD_SELF`, accessor calls)
- Modify: `crates/devkit-common/src/supervise.rs` (`env_remove("DEVKIT_PORTD_SELF")` + its test)
- Modify: each CLI's `#[command(name = "...")]` and `clap_complete::generate(.., "lock"/"portman", ..)` strings
- Modify: `tests/common/mod.rs`, `tests/lifecycle.rs` (and any other `tests/*.rs`) — `env!("CARGO_BIN_EXE_devkit-portd")` → `CARGO_BIN_EXE_devkitd`, the `portd.sock` socket path → `ports.sock`, and `DEVKIT_PORTD_SELF` → `DEVKITD_SELF`
- Modify docs: `README.md`, `CLAUDE.md`, `docs/next-steps.md`, anything under `docs/` referencing old names

- [ ] **Step 1: Inventory every occurrence (expect non-empty, this is the worklist)**

Run each and keep the output as your checklist:
```sh
rg -n "devkit-portd|portd\.(sock|lock|log)" --glob '!docs/superpowers/**'
rg -n "DEVKIT_PORTD_SELF|DEVKIT_PORTD_BIN"
rg -n "\bportman\b|\bsocket_file\b|\bdaemon_lock_file\b"
rg -n '"lock"|name = "lock"|bin/lock\.rs'
```
Expected: matches across `src/bin/`, `crates/`, `Cargo.toml`, `README.md`, `CLAUDE.md`. The migration docs under `docs/superpowers/**` are historical — leave the already-committed spec/plan text alone; only update `docs/next-steps.md` and `README.md`/`CLAUDE.md`.

- [ ] **Step 2: Rename the binary directories and file with git**

```sh
git mv src/bin/devkit-portd src/bin/devkitd
git mv src/bin/portman src/bin/portm
git mv src/bin/lock.rs src/bin/lockm.rs
```

- [ ] **Step 3: Update `paths.rs` accessors and literals**

In `crates/devkit-common/src/paths.rs`, rename and re-target:
```rust
/// Local socket the daemon binds for the port registry; clients connect here.
pub fn port_socket_file() -> PathBuf {
    state_dir().join("ports.sock")
}
/// Local socket the daemon binds for the lock registry.
pub fn lock_socket_file() -> PathBuf {
    state_dir().join("locks.sock")
}
/// Single-instance lock for the daemon — distinct from each registry's data lock.
pub fn devkitd_lock() -> PathBuf {
    state_dir().join("devkitd.lock")
}
/// Daemon log file.
pub fn daemon_log() -> PathBuf {
    logs_dir().join("devkitd.log")
}
```
Update the `daemon_paths_under_state` test to assert `ports.sock`, `locks.sock`, `devkitd.lock`, `logs/devkitd.log`, and add a `lock_socket_file()` assertion.

- [ ] **Step 4: Propagate the symbol renames with LSP**

For `socket_file` → `port_socket_file` and `daemon_lock_file` → `devkitd_lock`, use rust-analyzer rename (or find-all-references) so the call sites in `registry.rs`, `src/bin/devkitd/main.rs`, and `src/bin/devkitd/server.rs` update consistently. Verify none remain:
```sh
rg -n "\bsocket_file\b|\bdaemon_lock_file\b"
```
Expected: no matches.

- [ ] **Step 5: Update env-var literals and the daemon binary name**

Replace `DEVKIT_PORTD_SELF` → `DEVKITD_SELF` and `DEVKIT_PORTD_BIN` → `DEVKITD_BIN` in `registry.rs`, `src/bin/devkitd/main.rs`, `crates/devkit-ports/src/daemon/client.rs`, `crates/devkit-common/src/supervise.rs` and its test, and `tests/common/mod.rs` + `tests/lifecycle.rs` (`env!("CARGO_BIN_EXE_devkit-portd")` → `CARGO_BIN_EXE_devkitd`, `daemon_bin()`'s `env!`, the `socket()` path `portd.sock` → `ports.sock`). In `client.rs`, change the sibling-binary lookup `dir.join("devkit-portd")` → `dir.join("devkitd")` and the `PathBuf::from("devkit-portd")` fallback → `"devkitd"`. In `transport.rs` (windows arm) change the pipe prefix literal `format!("devkit-portd-{sanitized}.sock")` → `format!("devkit-{sanitized}.sock")`. In `registry.rs` update the `DaemonHoldsLock` message text `devkit-portd` → `devkitd`.

- [ ] **Step 6: Update `Cargo.toml` and CLI name strings**

In root `Cargo.toml`: the `[[bin]]` `name = "devkit-portd"` → `"devkitd"`, `path = "src/bin/devkit-portd/main.rs"` → `"src/bin/devkitd/main.rs"`, and the package `description` `portman, devrun, issue, devkit-portd` → `portm, lockm, devrun, issue, devkitd`. In `src/bin/portm/` set `#[command(name = "portm")]` and `clap_complete::generate(.., "portm", ..)`; in `src/bin/lockm.rs` set `#[command(name = "lock", ...)]` → `name = "lockm"` and `generate(.., "lock", ..)` → `"lockm"`.

- [ ] **Step 7: Update the docs**

`README.md`, `CLAUDE.md` (the Layout table rows for `portman`/`lock`/`devkit-portd`, the "File locks" section's `lock acquire`/`lock release` → `lockm acquire`/`lockm release`, and the `cargo test -p devkit-ports --test registry` line is unaffected), and `docs/next-steps.md` external-caller note. Final sweep:
```sh
rg -n "devkit-portd|\bportman\b|portd\.(sock|lock|log)|DEVKIT_PORTD" --glob '!docs/superpowers/**'
```
Expected: no matches outside `docs/superpowers/**`.

- [ ] **Step 8: Build, test, clippy**

```sh
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all binaries build as `portm`, `lockm`, `devrun`, `issue`, `devkitd`; all tests pass; zero warnings.

- [ ] **Step 9: Commit**

```sh
git add -A
git commit -m "refactor: rename daemon and CLIs to the mirrored devkitd/portm/lockm scheme"
```

---

### Task 2: Extract the daemon framework into `devkit-common::daemon`

Move the registry-agnostic plumbing (framing, transport, a generic `Client`) into `devkit-common` behind a `daemon` feature, then rebind `devkit-ports::daemon` onto it with no wire/behavior change.

**Files:**
- Create: `crates/devkit-common/src/daemon/mod.rs`
- Create: `crates/devkit-common/src/daemon/framing.rs`
- Create: `crates/devkit-common/src/daemon/transport.rs`
- Create: `crates/devkit-common/src/daemon/client.rs`
- Modify: `crates/devkit-common/Cargo.toml` (optional `interprocess`, `daemon` feature)
- Modify: `crates/devkit-common/src/lib.rs` (gated `pub mod daemon;`)
- Modify: `crates/devkit-ports/Cargo.toml` (`daemon` feature pulls `devkit-common/daemon`)
- Modify: `crates/devkit-ports/src/daemon/proto.rs` (re-export framing from common)
- Modify: `crates/devkit-ports/src/daemon/transport.rs` (re-export from common)
- Modify: `crates/devkit-ports/src/daemon/client.rs` (thin wrapper over common `Client`)

**Interfaces:**
- Produces (consumed by Tasks 5–7):
  - `devkit_common::daemon::framing::{send, recv}` — `send<W: Write>(&mut W, &impl Serialize) -> Result<()>`; `recv<R: BufRead, T: DeserializeOwned>(&mut R) -> Result<Option<T>>`.
  - `devkit_common::daemon::transport::socket_name(&Path) -> io::Result<Name<'static>>`.
  - `devkit_common::daemon::Client` with `request<Req: Serialize, Resp: DeserializeOwned>(&mut self, &Req) -> Result<Resp>`.
  - `devkit_common::daemon::connect(&Path) -> Option<Client>` (no handshake).
  - `devkit_common::daemon::spawn(&Path) -> Result<()>` (spawn a daemon binary detached-enough to poll).

- [ ] **Step 1: Add the feature and optional dep to `devkit-common`**

`crates/devkit-common/Cargo.toml`:
```toml
[dependencies]
interprocess = { workspace = true, optional = true }
# ...existing deps...

[features]
daemon = ["dep:interprocess"]
```
`crates/devkit-common/src/lib.rs` — add:
```rust
#[cfg(feature = "daemon")]
pub mod daemon;
```

- [ ] **Step 2: Write a failing framing test in common**

`crates/devkit-common/src/daemon/framing.rs`:
```rust
//! Newline-delimited JSON framing for the daemon control channel. Generic over
//! any serde message type so each registry's proto reuses it.

use anyhow::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{BufRead, Write};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrips_over_a_pipe() {
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &("hello", 7u32)).unwrap();
        assert_eq!(*buf.last().unwrap(), b'\n');
        let mut rdr = std::io::BufReader::new(&buf[..]);
        let back: (String, u32) = recv(&mut rdr).unwrap().expect("one frame");
        assert_eq!(back, ("hello".to_string(), 7));
    }

    #[test]
    fn recv_returns_none_on_eof() {
        let mut rdr = std::io::BufReader::new(&b""[..]);
        let got: Option<(String, u32)> = recv(&mut rdr).unwrap();
        assert!(got.is_none());
    }
}
```

- [ ] **Step 3: Run the framing test — verify it fails to compile/find the module**

Run: `cargo test -p devkit-common --features daemon framing`
Expected: FAIL — `daemon` module not declared yet / `mod.rs` missing.

- [ ] **Step 4: Add `mod.rs`, `transport.rs`, `client.rs`**

`crates/devkit-common/src/daemon/mod.rs`:
```rust
pub mod client;
pub mod framing;
pub mod transport;

pub use client::{Client, connect, spawn};
```
`crates/devkit-common/src/daemon/transport.rs` — move the body of `devkit-ports/src/daemon/transport.rs` here verbatim, changing only the windows pipe prefix to be registry-neutral (already done in Task 1: `format!("devkit-{sanitized}.sock")`). Keep both `#[cfg(unix)]` and `#[cfg(windows)]` arms.
`crates/devkit-common/src/daemon/client.rs`:
```rust
//! Generic daemon client: a reusable connection over a local socket, plus
//! connect/spawn helpers. Handshake (Ping/Pong/proto) is registry-specific and
//! lives in each consumer's thin wrapper.

use crate::daemon::framing;
use crate::daemon::transport;
use anyhow::{Context, Result, anyhow};
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{RecvHalf, SendHalf, Stream};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{BufReader, BufWriter};
use std::path::Path;

/// A live connection to a daemon. Reusable across requests.
pub struct Client {
    reader: BufReader<RecvHalf>,
    writer: BufWriter<SendHalf>,
}

impl Client {
    /// Send one request frame, read one response frame.
    pub fn request<Req: Serialize, Resp: DeserializeOwned>(&mut self, req: &Req) -> Result<Resp> {
        framing::send(&mut self.writer, req)?;
        framing::recv(&mut self.reader)?.ok_or_else(|| anyhow!("daemon closed connection"))
    }
}

/// Connect to the socket at `path` without any handshake. `None` if nothing is
/// listening (no daemon, or a stale socket file).
pub fn connect(path: &Path) -> Option<Client> {
    let name = transport::socket_name(path).ok()?;
    let stream = Stream::connect(name).ok()?;
    let (recv, send) = stream.split();
    Some(Client {
        reader: BufReader::new(recv),
        writer: BufWriter::new(send),
    })
}

/// Spawn the daemon binary at `bin` (it backgrounds itself by taking its lock and
/// binding sockets). Callers poll `connect` until it answers.
pub fn spawn(bin: &Path) -> Result<()> {
    std::process::Command::new(bin)
        .spawn()
        .with_context(|| format!("spawning {}", bin.display()))?;
    Ok(())
}
```

- [ ] **Step 5: Run the framing test — verify it passes**

Run: `cargo test -p devkit-common --features daemon`
Expected: PASS (framing + the rest of common's tests).

- [ ] **Step 6: Rebind `devkit-ports::daemon` onto common**

`crates/devkit-ports/Cargo.toml`:
```toml
[features]
daemon = ["dep:interprocess", "devkit-common/daemon"]
```
`crates/devkit-ports/src/daemon/transport.rs` — replace the whole body with a re-export:
```rust
//! Port daemon socket naming, delegated to the shared framework.
pub use devkit_common::daemon::transport::socket_name;
```
`crates/devkit-ports/src/daemon/proto.rs` — keep `Request`/`Response`/`PROTO`; replace the local `send`/`recv` definitions and their now-unneeded imports with a re-export so existing `proto::send`/`proto::recv` call sites keep working:
```rust
pub use devkit_common::daemon::framing::{recv, send};
```
Keep the `frames_roundtrip_over_a_pipe` / `recv_returns_none_on_eof` tests in `proto.rs` (they now exercise the re-exported fns through the port `Request` type — still valuable as a port-proto integration check).
`crates/devkit-ports/src/daemon/client.rs` — rewrite as a thin wrapper:
```rust
//! Port daemon client: connect to the supervisor over `ports.sock`, with the
//! port-proto handshake layered on the shared `Client`.

use crate::daemon::proto::{PROTO, Request, Response};
use anyhow::{Result, anyhow};
use devkit_common::daemon::{self, Client};
use devkit_common::paths;
use std::time::{Duration, Instant};

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

/// Validate a fresh connection with the port Ping/Pong handshake. A proto
/// mismatch (old daemon survived an upgrade) asks it to shut down and fails.
fn shake(mut c: Client) -> Option<Client> {
    match c.request::<Request, Response>(&Request::Ping { proto: PROTO }) {
        Ok(Response::Pong { proto, .. }) if handshake_ok(proto) => Some(c),
        Ok(Response::Pong { .. }) => {
            let _ = c.request::<Request, Response>(&Request::Shutdown);
            None
        }
        _ => None,
    }
}

/// Connect to an already-running daemon; `None` if none is up or the handshake
/// fails. Never autostarts.
pub fn try_existing() -> Option<Client> {
    shake(daemon::connect(&paths::port_socket_file())?)
}

/// Locate the daemon binary: `$DEVKITD_BIN`, else a sibling of the current exe,
/// else `devkitd` on `PATH`.
fn devkitd_bin() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("DEVKITD_BIN") {
        return p.into();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("devkitd");
        if sibling.is_file() {
            return sibling;
        }
    }
    std::path::PathBuf::from("devkitd")
}

/// Connect, autostarting a daemon if none is running (supervision paths only).
pub fn ensure_running() -> Result<Client> {
    if let Some(c) = try_existing() {
        return Ok(c);
    }
    daemon::spawn(&devkitd_bin())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(c) = try_existing() {
            return Ok(c);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!("daemon did not come up within 5s"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proto_match_decision() {
        assert!(handshake_ok(PROTO));
        assert!(!handshake_ok(PROTO + 1));
    }
}
```
Update `registry.rs::daemon_request` if needed: `crate::daemon::client::try_existing()` still returns a `Client`; `c.request(&req)?` now needs the response type inferable — it already is, because `daemon_request` returns `Result<Option<Response>>`, so `Ok(Some(c.request(&req)?))` infers `Resp = Response`.

- [ ] **Step 7: Build the daemon binary, run port + common suites**

```sh
cargo build --release
cargo test -p devkit-common --features daemon
cargo test -p devkit-ports --features daemon
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all green; the four `devkitd` lifecycle tests and the `tests/registry.rs` race test still pass on the relocated framework.

- [ ] **Step 8: Commit**

```sh
git add -A
git commit -m "refactor(daemon): extract framing/transport/client into devkit-common"
```

---

### Task 3: `Store` seam, `FlockStore`, gate, and generic `*_with` ops in `devkit-locks`

Mirror `crates/devkit-ports/src/registry.rs` (the proven template): a `Store` trait with a `FlockStore` driver that takes the `devkitd.lock` shared gate, plus the six op cores as generics. The public facade routes through `FlockStore` — no daemon path yet.

**Files:**
- Modify: `crates/devkit-locks/src/model.rs` (add `Data::dead_keys`)
- Modify: `crates/devkit-locks/src/store.rs` (add `Store`, `DaemonHoldsLock`, `FlockStore`, the six `*_with`)
- Modify: `crates/devkit-locks/src/lib.rs` (facade calls the `*_with` ops via `FlockStore`)

**Interfaces:**
- Consumes: `devkit_common::store::{load, save, with_lock}`, `devkit_common::paths::{devkitd_lock, locks_lock, locks_file}`.
- Produces (consumed by Tasks 4–7):
  - `devkit_locks::store::Store { snapshot(&self) -> Result<Data>; commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> }`.
  - `devkit_locks::store::{FlockStore, DaemonHoldsLock}`.
  - `acquire_with(&impl Store, root: &str, holder: &str, paths: &[String], pid: Option<u32>, note: Option<&str>, ttl: u64, now: u64) -> Result<AcquireOutcome>`
  - `check_with(&impl Store, root: &str, holder: &str, paths: &[String], now: u64) -> Result<Vec<Conflict>>`
  - `release_with(&impl Store, root: &str, holder: &str, paths: &[String], force: bool) -> Result<(Vec<String>, Vec<String>)>`
  - `release_all_with(&impl Store, root: &str, holder: &str) -> Result<Vec<String>>`
  - `status_with(&impl Store, root: &str, all: bool, now: u64) -> Result<Vec<LockEntry>>`
  - `prune_with(&impl Store, now: u64) -> Result<usize>`
  - `Data::dead_keys(&self, now: u64) -> Vec<String>`

- [ ] **Step 1: Write a failing test for `Data::dead_keys`**

In `crates/devkit-locks/src/model.rs` `tests` module:
```rust
#[test]
fn dead_keys_lists_ttl_and_pid_dead() {
    let mut d = Data::default();
    d.locks.extend([
        entry("/repo", "old", "alice", 100, 60, None),     // ttl-expired by now=1000
        entry("/repo", "fresh", "alice", 990, 60, None),   // live
        entry("/repo", "deadpid", "alice", 1, 0, Some(u32::MAX)), // dead pid
    ]);
    let mut got = d.dead_keys(1000);
    got.sort();
    assert_eq!(got, vec![key_for("/repo", "deadpid"), key_for("/repo", "old")]);
}
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test -p devkit-locks dead_keys`
Expected: FAIL — `no method named dead_keys`.

- [ ] **Step 3: Implement `dead_keys`**

In `crates/devkit-locks/src/model.rs`, add to `impl Data`:
```rust
/// Keys of every dead lock (TTL lapsed, or anchor pid known-gone), without
/// mutating. Callers persist removals separately so liveness probes stay out
/// of the write path's critical section.
pub fn dead_keys(&self, now: u64) -> Vec<String> {
    self.locks
        .iter()
        .filter(|(_, e)| entry_dead(e, now))
        .map(|(k, _)| k.clone())
        .collect()
}
```

- [ ] **Step 4: Run it — verify it passes**

Run: `cargo test -p devkit-locks dead_keys`
Expected: PASS.

- [ ] **Step 5: Write failing seam + gate tests**

Append to `crates/devkit-locks/src/store.rs` a `seam_tests` module:
```rust
#[cfg(test)]
mod seam_tests {
    use super::*;
    use crate::model::key_for;
    use std::path::PathBuf;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("devkit-lockseam-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn acquire_with_then_check_sees_conflict_for_other_holder() {
        let dir = tmp("acq");
        let s = FlockStore::at(&dir);
        let out = acquire_with(&s, "/repo", "alice", &["scenes".into()], None, None, 1800, 100).unwrap();
        assert_eq!(out.acquired.len(), 1);
        let conflicts = check_with(&s, "/repo", "bob", &["scenes/x".into()], 120).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].held_by, "alice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn release_with_frees_holders_path() {
        let dir = tmp("rel");
        let s = FlockStore::at(&dir);
        acquire_with(&s, "/repo", "alice", &["scenes".into()], None, None, 1800, 100).unwrap();
        let (released, refused) = release_with(&s, "/repo", "alice", &["scenes".into()], false).unwrap();
        assert_eq!(released, vec!["scenes".to_string()]);
        assert!(refused.is_empty());
        assert!(s.snapshot().unwrap().locks.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_with_drops_dead_and_is_a_hard_mutation() {
        let dir = tmp("prune");
        let s = FlockStore::at(&dir);
        // ttl=60, ts=0 → dead at now=1000
        acquire_with(&s, "/repo", "alice", &["scenes".into()], None, None, 60, 0).unwrap();
        let dropped = prune_with(&s, 1000).unwrap();
        assert_eq!(dropped, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_refused_while_gate_held_exclusive() {
        let dir = tmp("gate");
        let s = FlockStore::at(&dir);
        let f = std::fs::OpenOptions::new()
            .create(true).write(true).truncate(false)
            .open(dir.join("devkitd.lock")).unwrap();
        let mut excl = fd_lock::RwLock::new(f);
        let _held = excl.try_write().expect("take exclusive gate");
        let err = acquire_with(&s, "/repo", "alice", &["scenes".into()], None, None, 1800, 100).unwrap_err();
        assert!(err.downcast_ref::<DaemonHoldsLock>().is_some(), "got: {err:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn check_is_ungated_under_held_gate() {
        let dir = tmp("checkgate");
        let s = FlockStore::at(&dir);
        acquire_with(&s, "/repo", "alice", &["scenes".into()], None, None, 1800, 100).unwrap();
        let f = std::fs::OpenOptions::new()
            .create(true).write(true).truncate(false)
            .open(dir.join("devkitd.lock")).unwrap();
        let mut excl = fd_lock::RwLock::new(f);
        let _held = excl.try_write().unwrap();
        // ungated read must still succeed (and best-effort prune must not error out)
        let conflicts = check_with(&s, "/repo", "bob", &["scenes".into()], 120)
            .expect("read must not fail under held gate");
        assert_eq!(conflicts.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 6: Run the seam tests — verify they fail**

Run: `cargo test -p devkit-locks seam_tests`
Expected: FAIL — `Store`, `FlockStore::at`, `acquire_with`, etc. not defined.

- [ ] **Step 7: Implement the seam, gate, and ops in `store.rs`**

Add to `crates/devkit-locks/src/store.rs` (keep the existing `Document for Data` impl and the existing `with_lock`):
```rust
use crate::model::{AcquireOutcome, Conflict};
use fd_lock::RwLock;
use std::fs::OpenOptions;
use std::path::PathBuf;
#[cfg(test)]
use std::path::Path;

/// A driver for the lock-registry read-modify-write cycle. `FlockStore` backs the
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
            "a devkitd daemon holds the registry lock; refusing to modify locks.json \
             behind it — stop the daemon or use a daemon-enabled binary",
        )
    }
}
impl std::error::Error for DaemonHoldsLock {}

/// Direct file driver. Reads load the file ungated. Writes take a shared,
/// non-blocking lock on `devkitd.lock` — the gate — and refuse if a daemon holds
/// it exclusive, then run the data-flock RMW.
pub struct FlockStore {
    gate_path: PathBuf,
    lock_path: PathBuf,
    data_path: PathBuf,
}

impl FlockStore {
    pub fn new() -> Self {
        Self {
            gate_path: paths::devkitd_lock(),
            lock_path: paths::locks_lock(),
            data_path: paths::locks_file(),
        }
    }
    #[cfg(test)]
    fn at(dir: &Path) -> Self {
        Self {
            gate_path: dir.join("devkitd.lock"),
            lock_path: dir.join("locks.lock"),
            data_path: dir.join("locks.json"),
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
        // owns the registry — surface the typed refusal rather than writing behind it.
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

/// Acquire (explicit mutation): prune dead, then all-or-nothing acquire.
#[allow(clippy::too_many_arguments)]
pub fn acquire_with(
    s: &impl Store,
    root: &str,
    holder: &str,
    paths: &[String],
    pid: Option<u32>,
    note: Option<&str>,
    ttl: u64,
    now: u64,
) -> Result<AcquireOutcome> {
    s.commit(|d| {
        d.prune_dead(now);
        Ok(d.try_acquire(root, paths, holder, pid, note, ttl, now))
    })
}

/// Check (ungated read): conflicts that would block `holder`, with a best-effort
/// prune of dead rows. A blocked prune (a daemon owns the gate) is swallowed —
/// a read must never hard-fail because cleanup couldn't persist.
pub fn check_with(
    s: &impl Store,
    root: &str,
    holder: &str,
    paths: &[String],
    now: u64,
) -> Result<Vec<Conflict>> {
    let data = s.snapshot()?;
    let conflicts = data.check(root, paths, holder, now);
    if !data.dead_keys(now).is_empty() {
        let _ = s.commit(|d| {
            d.prune_dead(now);
            Ok(())
        });
    }
    Ok(conflicts)
}

/// Release named paths (explicit mutation). Returns (released, refused).
pub fn release_with(
    s: &impl Store,
    root: &str,
    holder: &str,
    paths: &[String],
    force: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    s.commit(|d| Ok(d.do_release(root, paths, holder, force)))
}

/// Release every lock held by `holder` in `root` (explicit mutation).
pub fn release_all_with(s: &impl Store, root: &str, holder: &str) -> Result<Vec<String>> {
    s.commit(|d| Ok(d.release_all(root, holder)))
}

/// Live locks (ungated read), best-effort prune. `all` ignores the root filter.
pub fn status_with(s: &impl Store, root: &str, all: bool, now: u64) -> Result<Vec<crate::model::LockEntry>> {
    let data = s.snapshot()?;
    if !data.dead_keys(now).is_empty() {
        let _ = s.commit(|d| {
            d.prune_dead(now);
            Ok(())
        });
    }
    let mut out: Vec<crate::model::LockEntry> = data
        .locks
        .values()
        .filter(|e| !crate::model::entry_dead(e, now) && (all || e.root == root))
        .cloned()
        .collect();
    out.sort_by(|a, b| (a.root.as_str(), a.path.as_str()).cmp(&(b.root.as_str(), b.path.as_str())));
    Ok(out)
}

/// Drop dead locks (explicit mutation); returns how many were removed.
pub fn prune_with(s: &impl Store, now: u64) -> Result<usize> {
    let data = s.snapshot()?;
    if data.dead_keys(now).is_empty() {
        return Ok(0);
    }
    s.commit(|d| Ok(d.prune_dead(now)))
}
```
Note: import `Data` is already in scope at the top of `store.rs` (it imports `crate::model::{Data, LockEntry, SCHEMA_VERSION}`). Add `AcquireOutcome, Conflict` to that import or use the `crate::model::` paths as written.

- [ ] **Step 8: Run the seam tests — verify they pass**

Run: `cargo test -p devkit-locks seam_tests`
Expected: PASS.

- [ ] **Step 9: Route the facade through `FlockStore`**

In `crates/devkit-locks/src/lib.rs`, replace each `store::with_lock(|d| …)` body with the matching `*_with` call against a `FlockStore`. The `ctx`/`ident`/`now` resolution is unchanged:
```rust
pub fn acquire(paths_in: &[String], as_flag: Option<&str>, note: Option<&str>, ttl: u64) -> Result<AcquireOutcome> {
    let c = ctx(paths_in, as_flag)?;
    let pid = ident::anchor_pid();
    store::acquire_with(&store::FlockStore::new(), &c.root, &c.holder, &c.paths, pid, note, ttl, now())
}

pub fn check(paths_in: &[String], as_flag: Option<&str>) -> Result<Vec<Conflict>> {
    let c = ctx(paths_in, as_flag)?;
    store::check_with(&store::FlockStore::new(), &c.root, &c.holder, &c.paths, now())
}

pub fn release(paths_in: &[String], as_flag: Option<&str>, force: bool) -> Result<(Vec<String>, Vec<String>)> {
    let c = ctx(paths_in, as_flag)?;
    store::release_with(&store::FlockStore::new(), &c.root, &c.holder, &c.paths, force)
}

pub fn release_all(as_flag: Option<&str>) -> Result<Vec<String>> {
    let c = ctx(&[], as_flag)?;
    store::release_all_with(&store::FlockStore::new(), &c.root, &c.holder)
}

pub fn status(all: bool) -> Result<Vec<LockEntry>> {
    let root = find_root()?.to_string_lossy().into_owned();
    store::status_with(&store::FlockStore::new(), &root, all, now())
}

pub fn prune() -> Result<usize> {
    store::prune_with(&store::FlockStore::new(), now())
}
```
The existing `store::with_lock` becomes unused by the facade; keep it (it stays a valid direct RMW helper) but route it through the gate so it can't write behind a daemon:
```rust
pub fn with_lock<T>(f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
    FlockStore::new().commit(f)
}
```
If clippy flags `with_lock` as dead code, mark it `#[allow(dead_code)]` with a one-line note that it is the gated direct RMW entry point retained for parity with the port registry.

- [ ] **Step 10: Full suite + clippy**

```sh
cargo test -p devkit-locks
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: all existing lock behavior tests (in `model.rs`, `lib.rs`) plus the new seam/gate tests pass; zero warnings.

- [ ] **Step 11: Commit**

```sh
git add -A
git commit -m "feat(locks): add Store seam, devkitd.lock gate, and generic *_with ops"
```

---

### Task 4: `MemoryStore` driver + `load()` for the lock registry

Add the daemon's in-memory driver with the write-through commit point, plus a one-shot `load()` for the daemon to populate memory at startup.

**Files:**
- Modify: `crates/devkit-locks/src/store.rs` (add `MemoryStore`, `load`)
- Test: `crates/devkit-locks/tests/memory_store.rs` (new integration test)

**Interfaces:**
- Produces (consumed by Task 7):
  - `devkit_locks::store::MemoryStore::new(state: Arc<Mutex<Data>>, data_path: PathBuf) -> MemoryStore` (implements `Store`).
  - `devkit_locks::store::load() -> Data`.

- [ ] **Step 1: Write a failing integration test**

`crates/devkit-locks/tests/memory_store.rs`:
```rust
use devkit_locks::store::{MemoryStore, Store, acquire_with};
use devkit_locks::model::Data;
use std::sync::{Arc, Mutex};

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("devkit-lockmem-{}-{}", std::process::id(), tag));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn commit_writes_through_then_updates_memory() {
    let dir = tmp("ok");
    let state = Arc::new(Mutex::new(Data::default()));
    let store = MemoryStore::new(state.clone(), dir.join("locks.json"));
    acquire_with(&store, "/repo", "alice", &["scenes".into()], None, None, 1800, 100).unwrap();
    assert_eq!(state.lock().unwrap().locks.len(), 1, "memory updated");
    let on_disk: Data = devkit_common::store::load(&dir.join("locks.json"));
    assert_eq!(on_disk.locks.len(), 1, "file written through");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commit_failure_leaves_memory_unchanged() {
    let dir = tmp("fail");
    let state = Arc::new(Mutex::new(Data::default()));
    // Point the data path at a directory so the file write fails.
    let bad = dir.join("as-dir");
    std::fs::create_dir_all(&bad).unwrap();
    let store = MemoryStore::new(state.clone(), bad);
    let r = acquire_with(&store, "/repo", "alice", &["scenes".into()], None, None, 1800, 100);
    assert!(r.is_err(), "write-through failure must error");
    assert!(state.lock().unwrap().locks.is_empty(), "memory unchanged on write failure");
    let _ = std::fs::remove_dir_all(&dir);
}
```
This requires `devkit-locks` to depend on `devkit-common` as a dev-dependency for the test's `devkit_common::store::load`. It already depends on it as a normal dependency, which `tests/` can use — no Cargo change needed.

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test -p devkit-locks --test memory_store`
Expected: FAIL — `MemoryStore` not found.

- [ ] **Step 3: Implement `MemoryStore` and `load`**

Add to `crates/devkit-locks/src/store.rs`:
```rust
use std::sync::{Arc, Mutex};

/// The daemon's authoritative in-memory lock registry. Reads serve from memory;
/// a mutation writes the file through (atomic rename) and updates memory only if
/// that write succeeded — the file is the commit point, so memory and file never
/// diverge.
pub struct MemoryStore {
    state: Arc<Mutex<Data>>,
    data_path: PathBuf,
}

impl MemoryStore {
    pub fn new(state: Arc<Mutex<Data>>, data_path: PathBuf) -> Self {
        Self { state, data_path }
    }
}

impl Store for MemoryStore {
    fn snapshot(&self) -> Result<Data> {
        Ok(self.state.lock().expect("lock registry mutex poisoned").clone())
    }
    fn commit<T>(&self, f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        let mut guard = self.state.lock().expect("lock registry mutex poisoned");
        let mut next = guard.clone();
        let out = f(&mut next)?;
        next.stamp_version();
        store::save(&self.data_path, &next)?; // commit point: persist before memory
        *guard = next;
        Ok(out)
    }
}

/// Load the lock-registry file into a `Data` for an owner with its own exclusion
/// (the daemon, holding `devkitd.lock` exclusive, at startup).
pub fn load() -> Data {
    store::load(&paths::locks_file())
}
```
`Data::stamp_version` already exists (the `Document` impl). `store::save` is the shared atomic writer.

- [ ] **Step 4: Run it — verify it passes**

Run: `cargo test -p devkit-locks --test memory_store`
Expected: PASS (both cases).

- [ ] **Step 5: Full suite + clippy**

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 6: Commit**

```sh
git add -A
git commit -m "feat(locks): add MemoryStore write-through driver and startup load"
```

---

### Task 5: Lock daemon proto + client (`devkit-locks::daemon`)

Define the lock wire protocol and a thin client bound to `locks.sock`, both on the shared `devkit-common::daemon` framework.

**Files:**
- Modify: `crates/devkit-locks/Cargo.toml` (optional `interprocess`, `daemon` feature)
- Modify: `crates/devkit-locks/src/lib.rs` (gated `pub mod daemon;`)
- Create: `crates/devkit-locks/src/daemon/mod.rs`
- Create: `crates/devkit-locks/src/daemon/proto.rs`
- Create: `crates/devkit-locks/src/daemon/client.rs`

**Interfaces:**
- Consumes: `devkit_common::daemon::{self, Client}`, `devkit_common::paths::lock_socket_file`.
- Produces (consumed by Tasks 6–7):
  - `devkit_locks::daemon::proto::{PROTO, Request, Response}` (variants below).
  - `devkit_locks::daemon::client::try_existing() -> Option<Client>`.

- [ ] **Step 1: Add feature + module wiring**

`crates/devkit-locks/Cargo.toml`:
```toml
[dependencies]
interprocess = { workspace = true, optional = true }
# ...existing deps...

[features]
daemon = ["dep:interprocess", "devkit-common/daemon"]
```
`crates/devkit-locks/src/lib.rs` — add near the other `pub mod`s:
```rust
#[cfg(feature = "daemon")]
pub mod daemon;
```

- [ ] **Step 2: Write a failing proto roundtrip test**

`crates/devkit-locks/src/daemon/proto.rs`:
```rust
//! Lock-registry wire protocol. Payloads carry context the daemon cannot resolve
//! itself (project root, holder, anchor pid); the daemon stamps `now`.

use crate::model::{AcquireOutcome, Conflict, LockEntry};
use serde::{Deserialize, Serialize};

/// Wire-format version, independent of the port proto. Bump on any incompatible change.
pub const PROTO: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Ping { proto: u32 },
    Acquire { root: String, holder: String, paths: Vec<String>, pid: Option<u32>, note: Option<String>, ttl: u64 },
    Check { root: String, holder: String, paths: Vec<String> },
    Release { root: String, holder: String, paths: Vec<String>, force: bool },
    ReleaseAll { root: String, holder: String },
    Status { root: String, all: bool },
    Prune,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Pong { proto: u32, pid: u32 },
    Acquired(AcquireOutcome),
    Conflicts(Vec<Conflict>),
    Released { released: Vec<String>, refused: Vec<String> },
    Freed(Vec<String>),
    Locks(Vec<LockEntry>),
    Pruned(usize),
    Ok,
    Err(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::daemon::framing::{recv, send};

    #[test]
    fn acquire_frame_roundtrips() {
        let msg = Request::Acquire {
            root: "/repo".into(),
            holder: "alice".into(),
            paths: vec!["scenes".into()],
            pid: Some(42),
            note: Some("refactor".into()),
            ttl: 1800,
        };
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut rdr = std::io::BufReader::new(&buf[..]);
        let back: Request = recv(&mut rdr).unwrap().expect("one frame");
        match back {
            Request::Acquire { root, holder, pid, .. } => {
                assert_eq!(root, "/repo");
                assert_eq!(holder, "alice");
                assert_eq!(pid, Some(42));
            }
            _ => panic!("wrong variant"),
        }
    }
}
```
This requires the proto's payload types to round-trip through serde. Current derive state in `model.rs`: `Acquired` and `Conflict` derive only `Serialize`; `AcquireOutcome` derives **neither** `Serialize` nor `Deserialize`; `LockEntry` already derives both. Step 4 fixes the gaps.

- [ ] **Step 3: Run it — verify it fails**

Run: `cargo test -p devkit-locks --features daemon proto`
Expected: FAIL — module/`Deserialize` not present.

- [ ] **Step 4: Add `Deserialize` derives, `mod.rs`, and `client.rs`**

In `crates/devkit-locks/src/model.rs`, extend the derives so every proto payload round-trips (add `Deserialize` where missing, and both for `AcquireOutcome`):
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acquired { /* unchanged fields */ }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict { /* unchanged fields */ }

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AcquireOutcome { /* unchanged fields */ }
```
Adding `Serialize` to `AcquireOutcome` does not affect `lockm.rs`'s existing `serde_json::json!` output, which serializes its fields individually.
`crates/devkit-locks/src/daemon/mod.rs`:
```rust
pub mod client;
pub mod proto;
```
`crates/devkit-locks/src/daemon/client.rs`:
```rust
//! Lock daemon client: connect over `locks.sock` with the lock-proto handshake.
//! `try_existing` only — `lockm` never autostarts the daemon.

use crate::daemon::proto::{PROTO, Request, Response};
use devkit_common::daemon::{self, Client};
use devkit_common::paths;

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

fn shake(mut c: Client) -> Option<Client> {
    match c.request::<Request, Response>(&Request::Ping { proto: PROTO }) {
        Ok(Response::Pong { proto, .. }) if handshake_ok(proto) => Some(c),
        _ => None,
    }
}

/// Connect to an already-running daemon's lock socket; `None` if none is up or
/// the handshake fails. Never autostarts.
pub fn try_existing() -> Option<Client> {
    shake(daemon::connect(&paths::lock_socket_file())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proto_match_decision() {
        assert!(handshake_ok(PROTO));
        assert!(!handshake_ok(PROTO + 1));
    }
}
```

- [ ] **Step 5: Run it — verify it passes**

Run: `cargo test -p devkit-locks --features daemon`
Expected: PASS.

- [ ] **Step 6: Full suite + clippy (with daemon feature)**

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green. (The root `devkit` crate's `daemon` feature must also enable `devkit-locks/daemon` — handled in Task 7 Step 1; until then `devkit-locks` is built with `daemon` via the workspace test above.)

- [ ] **Step 7: Commit**

```sh
git add -A
git commit -m "feat(locks): add lock daemon proto and locks.sock client"
```

---

### Task 6: Facade `via_daemon` split for the lock registry

Route each public facade op through the daemon when one is up (over `locks.sock`), else through `FlockStore`. Mirrors the port registry's `daemon_request` split.

**Files:**
- Modify: `crates/devkit-locks/src/lib.rs` (add `daemon_request` + route the six facade fns)

**Interfaces:**
- Consumes: `devkit_locks::daemon::{client::try_existing, proto}` (Task 5), the `*_with` ops (Task 3).
- Produces: unchanged public signatures (`acquire/check/release/release_all/status/prune`).

- [ ] **Step 1: Write a failing test that the no-daemon path still uses FlockStore**

The split must be transparent when no daemon runs. Add to `crates/devkit-locks/src/lib.rs` `tests`:
```rust
#[test]
fn facade_without_daemon_uses_flock_path() {
    // With DEVKITD_SELF set, daemon_request short-circuits to the flock path even
    // if a socket existed — proving the split's fallback is wired.
    // (No daemon is running in unit tests; this asserts the call still succeeds.)
    let n = prune().expect("prune via flock path");
    let _ = n; // count depends on ambient registry; success is the assertion
}
```
Note: this is a smoke test of the routing; full daemon behavior is covered by Task 7's integration test.

- [ ] **Step 2: Run it — verify current behavior**

Run: `cargo test -p devkit-locks --features daemon facade_without_daemon`
Expected: PASS already (facade currently uses FlockStore) — this test guards that the Step 3 refactor keeps the fallback working. If you prefer strict RED, write it after Step 3; either way it must be green at Step 4.

- [ ] **Step 3: Add `daemon_request` and route the facade**

In `crates/devkit-locks/src/lib.rs`:
```rust
/// Try a running daemon over `locks.sock`. `Ok(None)` = no daemon (caller uses
/// the flock path). `Ok(Some(resp))` = the daemon answered. `Err` = a live daemon
/// failed mid-request — surfaced rather than written behind its back. Inside the
/// daemon itself (`DEVKITD_SELF`) returns `Ok(None)` so its own ops stay local.
#[cfg(feature = "daemon")]
fn daemon_request(req: daemon::proto::Request) -> Result<Option<daemon::proto::Response>> {
    if std::env::var_os("DEVKITD_SELF").is_some() {
        return Ok(None);
    }
    let Some(mut c) = daemon::client::try_existing() else {
        return Ok(None);
    };
    Ok(Some(c.request(&req)?))
}
```
Then route each facade fn — daemon fast path first, FlockStore fallback. Example for `acquire` (apply the same shape to all six):
```rust
pub fn acquire(paths_in: &[String], as_flag: Option<&str>, note: Option<&str>, ttl: u64) -> Result<AcquireOutcome> {
    let c = ctx(paths_in, as_flag)?;
    let pid = ident::anchor_pid();
    #[cfg(feature = "daemon")]
    if let Some(resp) = daemon_request(daemon::proto::Request::Acquire {
        root: c.root.clone(),
        holder: c.holder.clone(),
        paths: c.paths.clone(),
        pid,
        note: note.map(str::to_string),
        ttl,
    })? {
        return match resp {
            daemon::proto::Response::Acquired(o) => Ok(o),
            daemon::proto::Response::Err(e) => Err(anyhow::anyhow!(e)),
            other => Err(anyhow::anyhow!("unexpected daemon response: {other:?}")),
        };
    }
    store::acquire_with(&store::FlockStore::new(), &c.root, &c.holder, &c.paths, pid, note, ttl, now())
}
```
Routing map for the other five:
- `check` → `Request::Check { root, holder, paths }` → `Response::Conflicts(v) => Ok(v)`.
- `release` → `Request::Release { root, holder, paths, force }` → `Response::Released { released, refused } => Ok((released, refused))`.
- `release_all` → `Request::ReleaseAll { root, holder }` → `Response::Freed(v) => Ok(v)`.
- `status` → `Request::Status { root, all }` → `Response::Locks(v) => Ok(v)`. (`status` resolves only `root`/`all`; reuse `find_root()` for the root.)
- `prune` → `Request::Prune` → `Response::Pruned(n) => Ok(n)`.
Each maps `Response::Err(e) => Err(anyhow!(e))` and any other variant to an "unexpected daemon response" error, then falls through to the `*_with(&FlockStore::new(), …)` call.

- [ ] **Step 4: Run the facade test + full suite**

```sh
cargo test -p devkit-locks --features daemon
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: green.

- [ ] **Step 5: Commit**

```sh
git add -A
git commit -m "feat(locks): route the facade through the daemon when one is up"
```

---

### Task 7: `devkitd` serves the lock registry

The daemon loads the lock registry into memory, binds `locks.sock`, and dispatches lock requests against its `MemoryStore` over `Data`. A sibling accept thread serves the lock socket; idle-shutdown wakes both listeners.

**Files:**
- Modify: `Cargo.toml` (root `daemon` feature enables `devkit-locks/daemon`)
- Modify: `src/bin/devkitd/main.rs` (`Daemon.locks`, load before bind, bind + accept lock socket, wake both on shutdown)
- Create: `src/bin/devkitd/lock_server.rs` (lock request dispatch)
- Modify: `tests/common/mod.rs` (add lock-socket helpers)
- Create: `tests/lock_daemon.rs` (integration test)

**Interfaces:**
- Consumes: `devkit_locks::{store::{MemoryStore, load, acquire_with, check_with, release_with, release_all_with, status_with, prune_with}, model::Data, daemon::proto}`, `devkit_common::daemon::{framing, transport}`.

The integration test drives the daemon **directly over the lock socket** (like `tests/lifecycle.rs` does for the port socket), not through the `devkit_locks` facade — so it never depends on the test process's CWD/identity and stays hermetic.

- [ ] **Step 1: Enable the lock daemon feature for the binary**

Root `Cargo.toml`:
```toml
[features]
default = ["daemon"]
daemon = ["devkit-ports/daemon", "devkit-locks/daemon"]
```

- [ ] **Step 2: Add lock-socket helpers to the shared harness**

In `tests/common/mod.rs`, add (the `#![allow(dead_code)]` at the top already covers cross-binary unused helpers):
```rust
use devkit_locks::daemon::proto::{Request as LockRequest, Response as LockResponse};

impl Harness {
    /// Path of the lock control socket the daemon binds.
    pub fn lock_socket(&self) -> PathBuf {
        self.xdg_state.join("devkit/locks.sock")
    }

    fn lock_connect(&self) -> Option<Stream> {
        let name = transport::socket_name(&self.lock_socket()).ok()?;
        Stream::connect(name).ok()
    }

    /// Poll until the lock socket accepts a connection, or panic.
    pub fn wait_for_lock_socket(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if self.lock_connect().is_some() {
                return;
            }
            if Instant::now() >= deadline {
                panic!("devkitd lock socket never came up");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Open a fresh lock connection, send one request, receive one response.
    pub fn lock_request(&self, req: &LockRequest) -> LockResponse {
        use devkit_common::daemon::framing;
        let stream = self.lock_connect().expect("connect to locks socket");
        let (recv, send) = stream.split();
        let mut writer = BufWriter::new(send);
        let mut reader = BufReader::new(recv);
        framing::send(&mut writer, req).expect("send lock request");
        framing::recv::<_, LockResponse>(&mut reader)
            .expect("recv lock response")
            .expect("EOF before response")
    }

    /// Read the locks.json content, or empty string if absent.
    pub fn locks_json(&self) -> String {
        std::fs::read_to_string(self.xdg_state.join("devkit/locks.json")).unwrap_or_default()
    }
}
```

- [ ] **Step 3: Write the failing integration test**

`tests/lock_daemon.rs`:
```rust
mod common;

use common::Harness;
use devkit_locks::daemon::proto::{Request, Response};
use std::time::Duration;

/// A lock acquired through the daemon is held in memory and visible to a later
/// `check` from a different holder.
#[test]
fn acquire_through_daemon_is_visible_to_check() {
    let mut h = Harness::start();
    h.wait_for_lock_socket(Duration::from_secs(5));

    let acq = h.lock_request(&Request::Acquire {
        root: "/repo".into(),
        holder: "alice".into(),
        paths: vec!["scenes".into()],
        pid: None,
        note: Some("refactor".into()),
        ttl: 1800,
    });
    assert!(
        matches!(&acq, Response::Acquired(o) if o.acquired.len() == 1 && o.conflicts.is_empty()),
        "expected one acquired lock, got {acq:?}"
    );

    let chk = h.lock_request(&Request::Check {
        root: "/repo".into(),
        holder: "bob".into(),
        paths: vec!["scenes/player.tscn".into()],
    });
    match chk {
        Response::Conflicts(c) => {
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].held_by, "alice");
        }
        other => panic!("expected a conflict held by alice, got {other:?}"),
    }
    h.shutdown();
}
```
The gate-refusal-behind-a-live-daemon case is covered hermetically by Task 3's `commit_refused_while_gate_held_exclusive` unit test (a real exclusive hold on `devkitd.lock`); repeating it here would require mutating the test process's global `XDG_STATE_HOME`, so it is deliberately left to the unit test.

- [ ] **Step 4: Run it — verify it fails**

Run: `cargo test --test lock_daemon`
Expected: FAIL — `wait_for_lock_socket` panics (the daemon binds no lock socket yet).

- [ ] **Step 5: Add `locks` state + `load` before bind**

In `src/bin/devkitd/main.rs`, add to `Daemon`:
```rust
/// Authoritative lock registry, served from memory; the file is write-through.
pub(crate) locks: std::sync::Arc<std::sync::Mutex<devkit_locks::model::Data>>,
```
Add an accessor mirroring `port_store`:
```rust
pub(crate) fn lock_store(&self) -> devkit_locks::store::MemoryStore {
    devkit_locks::store::MemoryStore::new(self.locks.clone(), devkit_common::paths::locks_file())
}
```
In `main`, right after the existing `registry::load()` line (still holding `devkitd.lock`, before any bind):
```rust
let locks = std::sync::Arc::new(std::sync::Mutex::new(devkit_locks::store::load()));
```
and add `locks` to the `Daemon { … }` constructor.

- [ ] **Step 6: Implement `lock_server::dispatch`**

`src/bin/devkitd/lock_server.rs`:
```rust
//! Lock-registry request handlers. Ops run through the daemon's authoritative
//! `MemoryStore`; reads serve from memory, mutations write through to the file.
//! The daemon stamps `now`; clients supply resolved root/holder/paths/pid.

use crate::Daemon;
use devkit_locks::daemon::proto::{PROTO, Request, Response};
use devkit_locks::store;
use std::sync::Arc;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub(crate) fn dispatch(daemon: &Arc<Daemon>, req: Request) -> Response {
    let s = daemon.lock_store();
    match req {
        Request::Ping { .. } => Response::Pong { proto: PROTO, pid: std::process::id() },
        Request::Acquire { root, holder, paths, pid, note, ttl } => {
            match store::acquire_with(&s, &root, &holder, &paths, pid, note.as_deref(), ttl, now()) {
                Ok(o) => Response::Acquired(o),
                Err(e) => Response::Err(format!("{e:#}")),
            }
        }
        Request::Check { root, holder, paths } => {
            match store::check_with(&s, &root, &holder, &paths, now()) {
                Ok(v) => Response::Conflicts(v),
                Err(e) => Response::Err(format!("{e:#}")),
            }
        }
        Request::Release { root, holder, paths, force } => {
            match store::release_with(&s, &root, &holder, &paths, force) {
                Ok((released, refused)) => Response::Released { released, refused },
                Err(e) => Response::Err(format!("{e:#}")),
            }
        }
        Request::ReleaseAll { root, holder } => match store::release_all_with(&s, &root, &holder) {
            Ok(v) => Response::Freed(v),
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::Status { root, all } => match store::status_with(&s, &root, all, now()) {
            Ok(v) => Response::Locks(v),
            Err(e) => Response::Err(format!("{e:#}")),
        },
        Request::Prune => match store::prune_with(&s, now()) {
            Ok(n) => Response::Pruned(n),
            Err(e) => Response::Err(format!("{e:#}")),
        },
    }
}
```
Register the module in `main.rs`: `mod lock_server;`.

- [ ] **Step 7: Bind `locks.sock` and serve it on a sibling thread**

In `main.rs`, after binding the port listener and before the main port accept loop, bind the lock listener and spawn its accept thread. Mirror the port `handle_conn` but dispatch via `lock_server`:
```rust
// Lock control channel — second socket, same process and lifecycle.
let lock_sock = paths::lock_socket_file();
let _ = std::fs::remove_file(&lock_sock);
let lock_name = transport::socket_name(&lock_sock).with_context(|| "building lock socket name")?;
let lock_listener = ListenerOptions::new()
    .name(lock_name)
    .create_sync()
    .with_context(|| format!("binding {}", lock_sock.display()))?;
{
    let d = Arc::clone(&daemon);
    std::thread::spawn(move || {
        for stream in lock_listener.incoming() {
            if d.shutdown.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let d2 = Arc::clone(&d);
            std::thread::spawn(move || {
                d2.active_conns.fetch_add(1, Ordering::SeqCst);
                d2.touch();
                if let Err(e) = handle_lock_conn(&d2, stream) {
                    log_line(&format!("lock connection error: {e:#}"));
                }
                d2.active_conns.fetch_sub(1, Ordering::SeqCst);
                d2.touch();
            });
        }
    });
}
```
Add the lock connection handler:
```rust
fn handle_lock_conn(daemon: &Arc<Daemon>, stream: Stream) -> Result<()> {
    use devkit_common::daemon::framing;
    use devkit_locks::daemon::proto::Request as LockRequest;
    let (recv, send) = stream.split();
    let mut reader = BufReader::new(recv);
    let mut writer = BufWriter::new(send);
    while let Some(req) = framing::recv::<_, LockRequest>(&mut reader)? {
        daemon.touch();
        let resp = lock_server::dispatch(daemon, req);
        framing::send(&mut writer, &resp)?;
    }
    Ok(())
}
```
Note the port `handle_conn` keeps using `proto::send/recv`; the lock handler uses `framing` directly (no port-proto `Shutdown`/close semantics — the lock channel has no per-request close).

- [ ] **Step 8: Wake both listeners on shutdown**

The idle watcher and the `Shutdown` handler currently nudge only the port socket. Update both nudge sites (the idle watcher in `main.rs` and `server.rs::dispatch`'s `Request::Shutdown` arm) to also connect to the lock socket so its accept loop observes the flag:
```rust
for sock in [paths::port_socket_file(), paths::lock_socket_file()] {
    if let Ok(name) = transport::socket_name(&sock) {
        let _ = Stream::connect(name);
    }
}
```
Apply the same two-socket nudge in `server.rs::dispatch` (it already imports `transport` and `Stream`; add `paths::lock_socket_file()`).

- [ ] **Step 9: Run the integration test + full suite**

```sh
cargo build
cargo test --test lock_daemon
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: the integration test passes (acquire visible to check); all existing tests stay green.

- [ ] **Step 10: Commit**

```sh
git add -A
git commit -m "feat(devkitd): serve the lock registry from memory over locks.sock"
```

---

### Task 8: Documentation and end-to-end verification

Update the project docs to the unified-daemon model and the mirrored names, and add the idle-exit-then-flock-fallback integration assertion.

**Files:**
- Modify: `CLAUDE.md` (Layout table, Registry-facade section, File-locks section)
- Modify: `README.md` (CLI names, daemon description)
- Modify: `docs/next-steps.md` (reframe the lock follow-up as done)
- Modify: `tests/lock_daemon.rs` (add the idle-exit fallback case)

- [ ] **Step 1: Add a write-through durability assertion**

Append to `tests/lock_daemon.rs` a case proving a daemon-held lock survives in the file for the flock fallback after the daemon exits:
```rust
/// A lock acquired through the daemon is written through to locks.json, so after
/// the daemon exits the flock fallback still sees it.
#[test]
fn acquired_lock_persists_to_file_after_daemon_exits() {
    let mut h = Harness::start();
    h.wait_for_lock_socket(Duration::from_secs(5));
    h.lock_request(&Request::Acquire {
        root: "/repo".into(),
        holder: "alice".into(),
        paths: vec!["scenes".into()],
        pid: None,
        note: None,
        ttl: 0, // no expiry, so it can't be pruned out before we read the file
    });
    h.shutdown(); // daemon exits; in-memory state is gone, the file must remain
    let body = h.locks_json();
    assert!(
        body.contains("\"holder\": \"alice\""),
        "written-through locks.json missing the lock: {body}"
    );
    assert!(body.contains("scenes"), "lock path not persisted: {body}");
}
```

- [ ] **Step 2: Run it — verify it passes**

Run: `cargo test --test lock_daemon`
Expected: PASS — `MemoryStore::commit` persists on every mutation, so the file holds the lock after exit. If it fails, the write-through path has a gap; fix it in `MemoryStore::commit` / the daemon load-before-bind ordering.

- [ ] **Step 3: Update `CLAUDE.md`**

- Layout table: replace the `src/bin/portman`, `src/bin/lock.rs`, `src/bin/devkit-portd` rows with `src/bin/portm`, `src/bin/lockm`, `src/bin/devkitd`, and note `devkitd` serves both the port and lock registries.
- "Registry facade" section: add a sentence that the daemon now holds both registries in memory over two sockets (`ports.sock`, `locks.sock`), gated by `devkitd.lock`, and that `devkit-locks` has the same `Store` seam.
- "File locks" section: `lock` → `lockm` in the command examples.

- [ ] **Step 4: Update `README.md` and `docs/next-steps.md`**

`README.md`: CLI names (`portman`→`portm`, `lock`→`lockm`, `devkit-portd`→`devkitd`) and the one-line daemon description (serves both registries). `docs/next-steps.md`: replace the "## Authoritative in-memory mode for the lock registry" section body with a one-line note that it shipped, pointing at this plan and the spec (keep the MCP follow-up section intact).

- [ ] **Step 5: Final full gate**

```sh
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p devkit-ports --test registry
```
Expected: all green, including the multiprocess flock race test.

- [ ] **Step 6: Commit**

```sh
git add -A
git commit -m "docs(locks): document the unified devkitd in-memory lock registry"
```

---

## Notes for the implementer

- The port registry (`crates/devkit-ports/src/registry.rs`) is the **proven template** for Tasks 3–4 and the daemon dispatch in Task 7. When in doubt about a pattern (gate construction, best-effort prune, write-through commit point, dispatch shape), read it — `devkit-locks` is deliberately parallel, differing only in the resolved-context split (the facade resolves root/holder/pid client-side) and the read-vs-write op semantics noted in Task 3.
- rust-analyzer (LSP) diagnostics can lag mid-edit; trust `cargo build`/`cargo test` over the editor's squiggles.
- Keep all comments timeless — describe what the code does, never that it was renamed/added/moved.
