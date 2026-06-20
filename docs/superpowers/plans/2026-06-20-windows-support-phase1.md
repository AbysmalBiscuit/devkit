# Windows Support — Phase 1 Implementation Plan

> **Status: implemented (historical record).** Every task below shipped, and the
> work originally scoped as Phase 2 (the `sys/windows.rs` backend, `windows-sys`,
> `%LOCALAPPDATA%` paths, `devrun` `cmd_logs`, the Windows handling of the test
> helpers) was folded into the same native pass. The codebase — not this
> document — is the source of truth; the unchecked `- [ ]` boxes are preserved as
> the plan that was followed, not outstanding work. The only item not yet done is
> adding Windows targets to the release build matrix (see the design doc's
> Delivery → Open).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor all OS-specific code behind a single `devkit_common::sys` boundary and unify the daemon transport on `interprocess`, with zero change to Unix behavior — the groundwork that makes the later Windows port a matter of adding one file.

**Architecture:** A new `devkit_common::sys` module owns every process primitive (`mod.rs` declares the API; `sys/unix.rs` holds today's `nix`/`/proc` code moved verbatim). Callers lose their direct `nix`/`#[cfg]`. The daemon's Unix-domain socket is replaced by `interprocess::local_socket` (same API on every platform), and `issue setup` becomes config-driven, deleting the `.env` symlink and hardcoded `bun install`.

**Tech Stack:** Rust (edition 2024), `nix` (Unix-only), `interprocess` (new), `anyhow`, `clap`, `serde`/`toml`, `fd-lock`.

## Global Constraints

- Edition 2024; resolver 3. Existing release profile (`panic = "abort"`, `strip`, `lto = "thin"`) unchanged.
- **Merge gate:** `cargo test --workspace` (128 tests) stays green on Linux + macOS, and `cargo clippy --workspace --all-targets -- -D warnings` is clean, after every task.
- After this phase, `nix` appears **only** in `crates/devkit-common/Cargo.toml` under `[target.'cfg(unix)'.dependencies]`. No `nix::` or `std::os::unix` reference may exist outside `crates/devkit-common/src/sys/` — **except** two items explicitly deferred to Phase 2: `src/bin/devrun/main.rs` `cmd_logs` (`exec` via `CommandExt`) and `tests/supervision.rs` (`nix::kill` test helper). Do not touch those two in Phase 1.
- No Windows code in this phase. `sys/mod.rs` references only the `unix` impl; the `windows` arm is added in Phase 2.
- New public `sys` API names are fixed by this plan; later tasks depend on them verbatim.

---

## File Structure

- `crates/devkit-common/src/sys/mod.rs` — **new**, cross-platform API + dispatch.
- `crates/devkit-common/src/sys/unix.rs` — **new**, Unix impls (moved from existing code).
- `crates/devkit-common/src/lib.rs` — add `pub mod sys;`.
- `crates/devkit-common/src/supervise.rs` — `stop`/`spawn_detached` delegate to `sys`; `tree_rss_bytes` (+ `read_ppid`, `resident_pages`) move out to `sys/unix.rs`.
- `crates/devkit-common/Cargo.toml` — move `nix` to `[target.'cfg(unix)'.dependencies]`.
- `crates/devkit-ports/src/registry.rs` — `pid_alive` delegates to `sys`.
- `crates/devkit-ports/Cargo.toml` — drop `nix`; add `interprocess`.
- `crates/devkit-locks/src/model.rs` — `pid_alive` delegates to `sys`.
- `crates/devkit-locks/src/ident.rs` — parent-pid / tty via `sys`.
- `crates/devkit-locks/Cargo.toml` — drop `nix`.
- `crates/devkit-ports/src/daemon/transport.rs` — **new**, the one place the socket name form is chosen.
- `crates/devkit-ports/src/daemon/mod.rs` — `pub mod transport;`.
- `crates/devkit-ports/src/daemon/client.rs` — `interprocess` instead of `UnixStream`.
- `src/bin/devkit-portd/main.rs`, `src/bin/devkit-portd/supervisor.rs`, `src/bin/devkit-portd/server.rs` — `interprocess` listener/stream; `reap_once` via `sys`.
- `Cargo.toml` (root) — drop `nix`; add `interprocess`.
- `crates/devkit-ports/src/config.rs`, `crates/devkit-ports/src/apps.rs`, `src/bin/issue/setup.rs` — config-driven `setup`.

---

## Task 1: `sys` boundary + `process_alive`, migrate `pid_alive`

**Files:**
- Create: `crates/devkit-common/src/sys/mod.rs`
- Create: `crates/devkit-common/src/sys/unix.rs`
- Modify: `crates/devkit-common/src/lib.rs` (add `pub mod sys;`)
- Modify: `crates/devkit-common/Cargo.toml` (`nix` → cfg(unix) target)
- Modify: `crates/devkit-ports/src/registry.rs` (`pid_alive` body)
- Modify: `crates/devkit-ports/Cargo.toml` (drop `nix`)
- Modify: `crates/devkit-locks/src/model.rs` (`pid_alive` body)

**Interfaces:**
- Produces: `devkit_common::sys::process_alive(pid: u32) -> bool`.

- [ ] **Step 1: Write the failing test** — append to a new `tests` module at the bottom of `crates/devkit-common/src/sys/mod.rs` (created in Step 3, but write the test first as its own file content you will paste):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_not_alive() {
        assert!(!process_alive(0));
    }
}
```

- [ ] **Step 2: Create `crates/devkit-common/src/sys/mod.rs`** with the API surface plus the test module:

```rust
//! Platform abstraction boundary. Every OS-specific primitive lives behind this
//! module so the rest of the workspace stays platform-agnostic. The `unix`
//! implementation is the only backend today; a `windows` backend is added later.

#[cfg(unix)]
#[path = "unix.rs"]
mod imp;

/// True if a process with `pid` currently exists.
pub fn process_alive(pid: u32) -> bool {
    imp::process_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_is_alive() {
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn pid_zero_is_not_alive() {
        assert!(!process_alive(0));
    }
}
```

- [ ] **Step 3: Create `crates/devkit-common/src/sys/unix.rs`** with the moved liveness probe:

```rust
//! Unix implementations of the primitives declared in `super`.

pub(super) fn process_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    // Pids that do not fit in a positive i32 are invalid on Linux/macOS.
    let Ok(signed) = i32::try_from(pid) else {
        return false;
    };
    if signed <= 0 {
        return false;
    }
    kill(Pid::from_raw(signed), None).is_ok()
}
```

- [ ] **Step 4: Register the module** — in `crates/devkit-common/src/lib.rs`, add `pub mod sys;` in alphabetical position (after `pub mod supervise;`... actually before — place it after `pub mod report;` line so the list stays sorted: `paths, platform?`—no; insert `pub mod sys;` after `pub mod supervise;`). Final relevant lines:

```rust
pub mod report;
pub mod slack;
pub mod supervise;
pub mod sys;
pub mod ui;
pub mod worktree;
```

- [ ] **Step 5: Make `nix` Unix-only** in `crates/devkit-common/Cargo.toml`. Replace the line `nix.workspace = true` (under `[dependencies]`) by deleting it there and appending:

```toml
# Unix process primitives (signals, sessions); unavailable on Windows.
[target.'cfg(unix)'.dependencies]
nix.workspace = true
```

- [ ] **Step 6: Run the new tests to verify they pass**

Run: `cargo test -p devkit-common sys::`
Expected: `self_is_alive` and `pid_zero_is_not_alive` PASS.

- [ ] **Step 7: Delegate `registry::pid_alive`** — in `crates/devkit-ports/src/registry.rs`, replace the existing body:

```rust
pub fn pid_alive(pid: u32) -> bool {
    devkit_common::sys::process_alive(pid)
}
```

- [ ] **Step 8: Delegate `model::pid_alive`** — in `crates/devkit-locks/src/model.rs`, replace the existing body:

```rust
/// True if a process with this pid currently exists (signal 0 probe).
pub fn pid_alive(pid: u32) -> bool {
    devkit_common::sys::process_alive(pid)
}
```

- [ ] **Step 9: Drop `nix` from `devkit-ports`** — in `crates/devkit-ports/Cargo.toml`, delete the line `nix.workspace = true`. (`devkit-locks` still uses `nix` in `ident.rs`; leave its dep until Task 4.)

- [ ] **Step 10: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all tests PASS (128), clippy clean. (`registry`/`model` `pid_alive` behavior is unchanged.)

- [ ] **Step 11: Commit**

```bash
git add crates/devkit-common crates/devkit-ports crates/devkit-locks/src/model.rs
git commit -m "refactor(sys): add platform boundary with process_alive"
```

---

## Task 2: `terminate` + `detach`, migrate `supervise`

**Files:**
- Modify: `crates/devkit-common/src/sys/mod.rs` (add two fns)
- Modify: `crates/devkit-common/src/sys/unix.rs` (add two impls)
- Modify: `crates/devkit-common/src/supervise.rs` (`stop`, `spawn_detached`)

**Interfaces:**
- Consumes: nothing new.
- Produces: `sys::terminate(pid: u32)`, `sys::detach(cmd: &mut std::process::Command)`.

- [ ] **Step 1: Add the API to `sys/mod.rs`** — under the `process_alive` fn add:

```rust
/// Ask `pid` to terminate, gracefully where the platform supports it.
pub fn terminate(pid: u32) {
    imp::terminate(pid)
}

/// Configure `cmd` to start detached from the caller's session/process group.
/// Must be called before `spawn`.
pub fn detach(cmd: &mut std::process::Command) {
    imp::detach(cmd)
}
```

- [ ] **Step 2: Add the impls to `sys/unix.rs`**:

```rust
pub(super) fn terminate(pid: u32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

pub(super) fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // Start a new session so the child outlives the launching shell and is
    // insulated from its controlling terminal's signals.
    // SAFETY: setsid only mutates the child after fork; it is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(|e| e.into()));
    }
}
```

- [ ] **Step 3: Route `supervise::spawn_detached` through `sys::detach`** — in `crates/devkit-common/src/supervise.rs`, replace the inline `pre_exec` block. The function becomes:

```rust
pub fn spawn_detached(
    argv: &[String], cwd: &str, env: &BTreeMap<String, String>, logfile: &PathBuf,
) -> Result<u32> {
    fs::create_dir_all(logfile.parent().unwrap())?;
    let out = File::create(logfile)?;
    let err = out.try_clone()?;
    let (prog, rest) = argv.split_first().context("empty launch argv")?;
    let mut c = Command::new(prog);
    c.args(rest).current_dir(cwd).envs(env)
        .stdin(Stdio::null()).stdout(out).stderr(err);
    crate::sys::detach(&mut c);
    let child = c.spawn().with_context(|| format!("spawning {prog}"))?;
    Ok(child.id())
}
```

- [ ] **Step 4: Route `supervise::stop` through `sys::terminate`** — replace its body:

```rust
/// SIGTERM a pid (ignore if already gone).
pub fn stop(pid: u32) {
    crate::sys::terminate(pid);
}
```

- [ ] **Step 5: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS. `supervise::tests::spawn_and_ready_on_python_http` exercises `spawn_detached` + `stop`; it must still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-common
git commit -m "refactor(sys): move terminate and detach behind the boundary"
```

---

## Task 3: move `tree_rss_bytes`, add `reap_owned`, migrate the daemon supervisor

**Files:**
- Modify: `crates/devkit-common/src/sys/mod.rs` (add two fns)
- Modify: `crates/devkit-common/src/sys/unix.rs` (add impls; receive moved code)
- Modify: `crates/devkit-common/src/supervise.rs` (remove `tree_rss_bytes`, `read_ppid`, `resident_pages`; re-export)
- Modify: `src/bin/devkit-portd/supervisor.rs` (`reap_once`, `tree_rss_bytes` import)
- Modify: `Cargo.toml` (root — drop `nix`)

**Interfaces:**
- Produces: `sys::tree_rss_bytes(root: u32) -> u64`, `sys::reap_owned(pid: u32) -> bool` (`true` once exited).

- [ ] **Step 1: Add the API to `sys/mod.rs`**:

```rust
/// Non-blocking reap/poll of an owned child. Returns `true` once it has exited.
pub fn reap_owned(pid: u32) -> bool {
    imp::reap_owned(pid)
}

/// Resident set size (bytes) summed over the process subtree rooted at `root`
/// (the process plus every descendant). Returns 0 if the root is gone.
pub fn tree_rss_bytes(root: u32) -> u64 {
    imp::tree_rss_bytes(root)
}
```

- [ ] **Step 2: Add `reap_owned` to `sys/unix.rs`**:

```rust
pub(super) fn reap_owned(pid: u32) -> bool {
    use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
    use nix::unistd::Pid;
    // A pid of 0 would make waitpid(0) reap any process-group member; never probe it.
    if pid == 0 {
        return false;
    }
    match waitpid(Pid::from_raw(pid as i32), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::StillAlive) => false,
        Ok(_) => true,  // exited/signaled → reaped
        Err(_) => true, // ECHILD etc. → treat as gone
    }
}
```

- [ ] **Step 3: Move `tree_rss_bytes` + helpers into `sys/unix.rs`** — cut the `tree_rss_bytes`, `read_ppid`, and `resident_pages` functions from `crates/devkit-common/src/supervise.rs` and paste them into `sys/unix.rs`, changing `pub fn tree_rss_bytes` to `pub(super) fn tree_rss_bytes` and adding `use std::collections::{HashMap, HashSet};` at the top of `sys/unix.rs`. The moved bodies are unchanged:

```rust
use std::collections::{HashMap, HashSet};
use std::fs;

pub(super) fn tree_rss_bytes(root: u32) -> u64 {
    let mut parent: HashMap<u32, u32> = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else { return 0 };
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if let Some(ppid) = read_ppid(pid) {
            parent.insert(pid, ppid);
        }
    }
    let mut total = 0u64;
    let mut stack = vec![root];
    let mut seen = HashSet::new();
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

fn read_ppid(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let mut it = rest.split_whitespace();
    let _state = it.next()?;
    it.next()?.parse::<u32>().ok()
}

fn resident_pages(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|n| n.parse::<u64>().ok()))
        .unwrap_or(0)
}
```

- [ ] **Step 4: Re-export `tree_rss_bytes` from `supervise`** — `supervisor.rs` imports `devkit_common::supervise::tree_rss_bytes`. Keep that path working by adding, at the top of `crates/devkit-common/src/supervise.rs` (replacing the just-removed function with a re-export):

```rust
pub use crate::sys::tree_rss_bytes;
```

Also move the `tree_rss_counts_self` test: delete it from `supervise.rs`'s `tests` module and add to `sys/mod.rs`'s `tests` module:

```rust
    #[test]
    fn tree_rss_of_self_is_nonzero() {
        assert!(tree_rss_bytes(std::process::id()) > 0);
    }
```

- [ ] **Step 5: Migrate `reap_once`** — in `src/bin/devkit-portd/supervisor.rs`, replace the `use nix::...` lines and the `Watch::Owned` arm. The function becomes:

```rust
    pub(crate) fn reap_once(&mut self) -> Vec<Key> {
        let mut dead = Vec::new();
        for (key, child) in self.children.iter() {
            if child.pid == 0 {
                continue;
            }
            let gone = match child.watch {
                Watch::Owned => devkit_common::sys::reap_owned(child.pid),
                Watch::Adopted => !registry::pid_alive(child.pid),
            };
            if gone {
                dead.push(key.clone());
            }
        }
        dead
    }
```

(The `use devkit_common::supervise::tree_rss_bytes;` import at the top of `supervisor.rs` stays valid via the Step 4 re-export.)

- [ ] **Step 6: Drop `nix` from the root crate** — in the root `Cargo.toml`, delete the line `nix.workspace = true` from `[dependencies]`. (The daemon no longer calls `nix`; `devrun`'s `cmd_logs` uses `std::os::unix`, not the `nix` crate, so it is unaffected.)

- [ ] **Step 7: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, including `supervision` integration tests and `sys::tests::tree_rss_of_self_is_nonzero`.

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-common src/bin/devkit-portd/supervisor.rs Cargo.toml
git commit -m "refactor(sys): move tree-RSS and reap behind the boundary; drop nix from daemon"
```

---

## Task 4: `parent_pid` + `controlling_tty`, migrate `ident`, drop `nix` from `devkit-locks`

**Files:**
- Modify: `crates/devkit-common/src/sys/mod.rs` (add two fns)
- Modify: `crates/devkit-common/src/sys/unix.rs` (add two impls)
- Modify: `crates/devkit-locks/src/ident.rs` (use `sys`)
- Modify: `crates/devkit-locks/Cargo.toml` (drop `nix`)

**Interfaces:**
- Produces: `sys::parent_pid() -> Option<u32>`, `sys::controlling_tty() -> Option<String>`.

- [ ] **Step 1: Add the API to `sys/mod.rs`**:

```rust
/// Parent process id, on platforms that expose one.
pub fn parent_pid() -> Option<u32> {
    imp::parent_pid()
}

/// The controlling terminal's name, when stdin is attached to one.
pub fn controlling_tty() -> Option<String> {
    imp::controlling_tty()
}
```

Add to the `tests` module of `sys/mod.rs`:

```rust
    #[test]
    fn parent_pid_is_present() {
        assert!(parent_pid().is_some());
    }
```

- [ ] **Step 2: Add the impls to `sys/unix.rs`**:

```rust
pub(super) fn parent_pid() -> Option<u32> {
    Some(nix::unistd::getppid().as_raw() as u32)
}

pub(super) fn controlling_tty() -> Option<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return None;
    }
    nix::unistd::ttyname(std::io::stdin())
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}
```

- [ ] **Step 3: Migrate `ident.rs`** — in `crates/devkit-locks/src/ident.rs`: in `Env::from_process`, set `tty: devkit_common::sys::controlling_tty(),` and `ppid: devkit_common::sys::parent_pid().map(|p| p.to_string()),`. Delete the local `fn ttyname()` entirely. In `anchor_pid`, replace the third argument:

```rust
pub fn anchor_pid() -> Option<u32> {
    use std::io::IsTerminal;
    decide_anchor_pid(
        tmux_pane_pid(),
        std::io::stdin().is_terminal(),
        devkit_common::sys::parent_pid().unwrap_or(0),
    )
}
```

- [ ] **Step 4: Drop `nix` from `devkit-locks`** — in `crates/devkit-locks/Cargo.toml`, delete `nix.workspace = true`.

- [ ] **Step 5: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS. `ident.rs`'s resolution tests are pure (operate on a constructed `Env`) and are unaffected; `sys::tests::parent_pid_is_present` passes.

- [ ] **Step 6: Verify the dependency invariant**

Run: `rg -n "nix(\.workspace|::|\s*=)" Cargo.toml crates/*/Cargo.toml`
Expected: a single match — `nix.workspace = true` under `[target.'cfg(unix)'.dependencies]` in `crates/devkit-common/Cargo.toml`.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-common crates/devkit-locks
git commit -m "refactor(sys): parent-pid and tty behind the boundary; nix now devkit-common-only"
```

---

## Task 5: Daemon transport → `interprocess`

**Files:**
- Modify: `Cargo.toml` (root, workspace deps) + `crates/devkit-ports/Cargo.toml` + root `[dependencies]`
- Create: `crates/devkit-ports/src/daemon/transport.rs`
- Modify: `crates/devkit-ports/src/daemon/mod.rs`
- Modify: `crates/devkit-ports/src/daemon/client.rs`
- Modify: `src/bin/devkit-portd/main.rs`, `src/bin/devkit-portd/server.rs`
- Modify: `tests/common/mod.rs`

**Interfaces:**
- Produces: `devkit_ports::daemon::transport::socket_name(path: &std::path::Path) -> std::io::Result<interprocess::local_socket::Name<'static>>`.
- Consumes: `proto::send`/`proto::recv` (unchanged; generic over `Write`/`BufRead`).

> **API note:** uses `interprocess` 2.x. Three identifiers should be confirmed against docs.rs for the pinned version before/while implementing, since the crate's surface is version-sensitive: (a) `Path::to_fs_name::<GenericFilePath>()`, (b) `ListenerOptions::new().name(..).create_sync()`, (c) `Stream::split() -> (RecvHalf, SendHalf)`. The compiler reports the exact names immediately; the data flow below is correct regardless.

- [ ] **Step 1: Add the dependency.** In the root `Cargo.toml` `[workspace.dependencies]` add:

```toml
interprocess = { version = "2", default-features = false }
```

In `crates/devkit-ports/Cargo.toml` `[dependencies]` add `interprocess.workspace = true`. In the root `Cargo.toml` `[dependencies]` add `interprocess.workspace = true`.

- [ ] **Step 2: Create `crates/devkit-ports/src/daemon/transport.rs`** — the single place the socket form is decided:

```rust
//! Local-socket naming for the daemon control channel. The one place the
//! platform socket form is chosen, so the rest of the daemon code is
//! transport-agnostic.

use interprocess::local_socket::{GenericFilePath, Name, ToFsName};
use std::path::Path;

/// Build the local-socket name for the daemon endpoint backed by `path`.
///
/// Unix: a filesystem-path socket at `path`. (A Windows namespaced-pipe arm is
/// added when the Windows backend lands.)
pub fn socket_name(path: &Path) -> std::io::Result<Name<'static>> {
    path.to_path_buf().to_fs_name::<GenericFilePath>()
}
```

- [ ] **Step 3: Export it** — in `crates/devkit-ports/src/daemon/mod.rs`:

```rust
pub mod proto;
pub mod transport;
pub mod client;
```

- [ ] **Step 4: Rewrite `daemon/client.rs`** to use `interprocess`:

```rust
//! Daemon client: connects to the supervisor over its local socket.

use crate::daemon::proto::{self, Request, Response, PROTO};
use crate::daemon::transport;
use anyhow::{anyhow, Context, Result};
use devkit_common::paths;
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::{Stream, RecvHalf, SendHalf};
use std::io::{BufReader, BufWriter};
use std::time::{Duration, Instant};

pub struct Client {
    reader: BufReader<RecvHalf>,
    writer: BufWriter<SendHalf>,
}

pub fn handshake_ok(server_proto: u32) -> bool {
    server_proto == PROTO
}

impl Client {
    fn from_stream(stream: Stream) -> Result<Self> {
        let (recv, send) = stream.split();
        let mut c = Client { reader: BufReader::new(recv), writer: BufWriter::new(send) };
        match c.request(&Request::Ping { proto: PROTO })? {
            Response::Pong { proto, .. } if handshake_ok(proto) => Ok(c),
            Response::Pong { .. } => {
                let _ = c.request(&Request::Shutdown);
                Err(anyhow!("daemon proto mismatch"))
            }
            other => Err(anyhow!("unexpected handshake response: {other:?}")),
        }
    }

    pub fn request(&mut self, req: &Request) -> Result<Response> {
        proto::send(&mut self.writer, req)?;
        proto::recv(&mut self.reader)?.ok_or_else(|| anyhow!("daemon closed connection"))
    }
}

pub fn try_existing() -> Option<Client> {
    let name = transport::socket_name(&paths::socket_file()).ok()?;
    let stream = Stream::connect(name).ok()?;
    Client::from_stream(stream).ok()
}

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

pub fn ensure_running() -> Result<Client> {
    if let Some(c) = try_existing() {
        return Ok(c);
    }
    std::process::Command::new(portd_bin())
        .spawn()
        .with_context(|| "spawning devkit-portd")?;
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
        assert!(handshake_ok(crate::daemon::proto::PROTO));
        assert!(!handshake_ok(crate::daemon::proto::PROTO + 1));
    }
}
```

- [ ] **Step 5: Rewrite the listener in `src/bin/devkit-portd/main.rs`.** Replace the import line 13 `use std::os::unix::net::{UnixListener, UnixStream};` with:

```rust
use devkit_ports::daemon::transport;
use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
use interprocess::local_socket::{ListenerOptions, Stream};
```

Replace the bind block (the `let sock = ...; remove_file; let listener = UnixListener::bind(...)` lines) with:

```rust
    let sock = paths::socket_file();
    let _ = std::fs::remove_file(&sock); // clear a stale socket file before binding
    let name = transport::socket_name(&sock).with_context(|| "building socket name")?;
    let listener = ListenerOptions::new()
        .name(name)
        .create_sync()
        .with_context(|| format!("binding {}", sock.display()))?;
```

Replace the idle-exit wakeup line `let _ = UnixStream::connect(paths::socket_file());` with:

```rust
                if let Ok(name) = transport::socket_name(&paths::socket_file()) {
                    let _ = Stream::connect(name);
                }
```

Change `handle_conn`'s signature and body to take an `interprocess` `Stream`:

```rust
fn handle_conn(daemon: &Arc<Daemon>, stream: Stream) -> Result<()> {
    let (recv, send) = stream.split();
    let mut reader = BufReader::new(recv);
    let mut writer = BufWriter::new(send);
    while let Some(req) = proto::recv::<_, Request>(&mut reader)? {
        daemon.touch();
        let (resp, close) = server::dispatch(daemon, req);
        proto::send(&mut writer, &resp)?;
        if close {
            break;
        }
    }
    Ok(())
}
```

The `for stream in listener.incoming()` loop body is unchanged (it already does `let Ok(stream) = stream else { continue };` then `handle_conn(&d, stream)`).

- [ ] **Step 6: Fix `src/bin/devkit-portd/server.rs` line 49** — replace `std::os::unix::net::UnixStream::connect(devkit_common::paths::socket_file())` with:

```rust
            if let Ok(name) = devkit_ports::daemon::transport::socket_name(&devkit_common::paths::socket_file()) {
                let _ = interprocess::local_socket::traits::Stream::connect(name);
            }
```

(Add `use interprocess::local_socket::Stream;` only if it reads cleaner; the fully-qualified `traits::Stream::connect` form above needs the `Stream` trait in scope — confirm the exact trait path while implementing per the API note.)

- [ ] **Step 7: Migrate `tests/common/mod.rs`.** Replace `use std::os::unix::net::UnixStream;` with:

```rust
use devkit_ports::daemon::transport;
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::Stream;
```

Replace `wait_for_socket`'s probe `if sock.exists() && UnixStream::connect(&sock).is_ok()` with:

```rust
            let connectable = transport::socket_name(&sock)
                .ok()
                .and_then(|name| Stream::connect(name).ok())
                .is_some();
            if sock.exists() && connectable {
                return;
            }
```

Replace `request`'s connection setup:

```rust
    pub fn request(&self, req: &Request) -> Response {
        let name = transport::socket_name(&self.socket()).expect("socket name");
        let stream = Stream::connect(name).expect("connect to portd socket");
        let (recv, send) = stream.split();
        let mut writer = BufWriter::new(send);
        let mut reader = BufReader::new(recv);
        proto::send(&mut writer, req).expect("send request");
        proto::recv::<_, Response>(&mut reader)
            .expect("recv response")
            .expect("EOF before response")
    }
```

(Add `use std::io::BufWriter;` to the imports.) Replace `shutdown`'s body similarly:

```rust
    pub fn shutdown(&mut self) {
        if let Ok(name) = transport::socket_name(&self.socket())
            && let Ok(stream) = Stream::connect(name)
        {
            let (recv, send) = stream.split();
            let mut writer = BufWriter::new(send);
            let mut reader = BufReader::new(recv);
            let _ = proto::send(&mut writer, &Request::Shutdown);
            let _ = proto::recv::<_, Response>(&mut reader);
        }
        let _ = self.child.wait();
    }
```

- [ ] **Step 8: Build and run the daemon integration tests**

Run: `cargo test --workspace`
Expected: PASS — `lifecycle`, `parity`, and `supervision` all exercise the live daemon over the new transport.

- [ ] **Step 9: Clippy and the Unix-ism check**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Then: `rg -n "std::os::unix" crates src tests`
Expected: clippy clean; the only `std::os::unix` hits are `src/bin/devrun/main.rs` (`cmd_logs`) and `src/bin/issue/setup.rs` (removed in Task 6). No daemon/test-harness hits remain.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml crates/devkit-ports src/bin/devkit-portd tests/common/mod.rs
git commit -m "refactor(daemon): unify transport on interprocess local sockets"
```

---

## Task 6: Config-driven `issue setup`

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (`AppConfig.setup` + test)
- Modify: `crates/devkit-ports/src/apps.rs` (`App.setup` + catalog wiring + test)
- Modify: `src/bin/issue/setup.rs` (run `setup`; delete symlink + `bun install`)

**Interfaces:**
- Consumes: `App` (gains a `setup: Vec<Vec<String>>` field).
- Produces: per-app `setup` commands run during `issue setup`.

- [ ] **Step 1: Write the failing config test** — add to the `tests` module in `crates/devkit-ports/src/config.rs`:

```rust
    #[test]
    fn parses_app_setup_commands() {
        let src = format!(
            "{SAMPLE}setup = [[\"doppler\", \"run\", \"-c\", \"local\", \"--\", \"bun\", \"install\"]]\n"
        );
        let c = Config::parse(&src).unwrap();
        assert_eq!(
            c.apps["api"].setup,
            vec![vec![
                "doppler".to_string(), "run".to_string(), "-c".to_string(),
                "local".to_string(), "--".to_string(), "bun".to_string(),
                "install".to_string(),
            ]]
        );
    }

    #[test]
    fn setup_defaults_empty() {
        let c = Config::parse(SAMPLE).unwrap();
        assert!(c.apps["api"].setup.is_empty());
    }
```

(The `SAMPLE` const ends with the `[apps.api]` block, so appending `setup = [[...]]` adds the field to that app.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p devkit-ports config::tests::parses_app_setup_commands`
Expected: FAIL — `no field `setup` on type `AppConfig``.

- [ ] **Step 3: Add the field to `AppConfig`** — in `crates/devkit-ports/src/config.rs`, inside `struct AppConfig`, after `prep_env`:

```rust
    /// Commands run in the app's directory during `issue setup`, in order. Each
    /// inner array is one argv (program + args), e.g.
    /// `[["doppler","run","-c","local","--","bun","install"]]`.
    #[serde(default)]
    pub setup: Vec<Vec<String>>,
```

- [ ] **Step 4: Run to verify the config tests pass**

Run: `cargo test -p devkit-ports config::tests::parses_app_setup_commands config::tests::setup_defaults_empty`
Expected: PASS.

- [ ] **Step 5: Carry `setup` into the catalog** — in `crates/devkit-ports/src/apps.rs`, add `pub setup: Vec<Vec<String>>,` to `struct App` (after `prep_env`), and in `catalog`'s `App { ... }` literal add `setup: a.setup.clone(),`. Update the two test helpers that build an `App` literal in `apps.rs` and `src/bin/devrun/env.rs` to include `setup: vec![]` — search for `prep_env: HashMap::new()` in those test modules and add `setup: vec![],` alongside.

Run: `cargo test -p devkit-ports apps::`
Expected: PASS.

- [ ] **Step 6: Replace the setup body** — in `src/bin/issue/setup.rs`, delete the entire `// env symlinks ...` loop (the block that creates `app_dir`, the `.env` symlink, and writes `prep_env` to `.env.local`) **and** the hardcoded `bun install` block (`// bun install once ...`). Replace both with one config-driven loop placed where the symlink loop was:

```rust
    // Per-app bootstrap: write prep_env, then run the app's configured setup
    // commands in its directory. Everything project-specific lives in config.
    let env_local = wt_root.join(".env.local");
    let _ = &env_local; // reserved for prep_env layering below
    for a in &args.apps {
        let app = &catalog[a];
        let app_dir = worktree.join(&app.path);
        std::fs::create_dir_all(&app_dir).ok();

        if !app.prep_env.is_empty() {
            let f = app_dir.join(".env.local");
            if !f.exists() {
                let body: String = app.prep_env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
                std::fs::write(&f, body)?;
            }
        }

        for cmd in &app.setup {
            let (prog, rest) = cmd.split_first().context("empty setup command")?;
            capture(prog, &rest.iter().map(String::as_str).collect::<Vec<_>>(), app_dir.to_str())
                .with_context(|| format!("running setup `{}` for app `{a}`", cmd.join(" ")))?;
        }
    }
```

Then delete the now-unused `env_local` line if the compiler flags it (it is only kept above as a no-op for readability; remove both the binding and the `let _ =` line if `prep_env` no longer references it — it does not, so delete those two lines). Final loop has no `env_local`:

```rust
    for a in &args.apps {
        let app = &catalog[a];
        let app_dir = worktree.join(&app.path);
        std::fs::create_dir_all(&app_dir).ok();

        if !app.prep_env.is_empty() {
            let f = app_dir.join(".env.local");
            if !f.exists() {
                let body: String = app.prep_env.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
                std::fs::write(&f, body)?;
            }
        }

        for cmd in &app.setup {
            let (prog, rest) = cmd.split_first().context("empty setup command")?;
            capture(prog, &rest.iter().map(String::as_str).collect::<Vec<_>>(), app_dir.to_str())
                .with_context(|| format!("running setup `{}` for app `{a}`", cmd.join(" ")))?;
        }
    }
```

(`capture` is already imported at the top of `setup.rs`: `use devkit_common::cmd::{capture, git};`. Confirm `capture(prog: &str, args: &[&str], cwd: Option<&str>)` matches the existing call `capture("bun", &["install"], Some(app_dir.to_str().unwrap()))` — it does.)

- [ ] **Step 7: Confirm no Unix-ism remains in setup**

Run: `rg -n "std::os::unix|symlink" src/bin/issue/setup.rs`
Expected: no matches.

- [ ] **Step 8: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS (the `setup.rs` `branch_uses_prefix_and_slug` test still passes; new config tests pass).

- [ ] **Step 9: Document the field** — append to `docs/configuration.md` a short `setup` entry under the per-app keys (one paragraph + the `[apps.api] setup = [[...]]` example). If `docs/configuration.md` lacks a per-app section, add the example under the apps heading.

- [ ] **Step 10: Commit**

```bash
git add crates/devkit-ports src/bin/issue/setup.rs docs/configuration.md
git commit -m "feat(issue): config-driven setup commands; drop .env symlink and hardcoded bun install"
```

---

## Phase 1 done — definition of done

- `cargo test --workspace` green on Linux + macOS (128 tests, adjusted for the moved `tree_rss`/added `sys`/`config` tests).
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `nix` only in `crates/devkit-common/Cargo.toml` under `cfg(unix)`; the only `std::os::unix` left is `devrun` `cmd_logs` and `tests/supervision.rs` (both Phase 2).
- Daemon speaks over `interprocess`; `issue setup` is config-driven with no symlink.

**What followed:** the Phase 2 work (`sys/windows.rs` + `windows-sys`, `%LOCALAPPDATA%` paths, `cmd_logs`/`tests/supervision` Windows handling, build+test on `x86_64-pc-windows-msvc`) was implemented directly in the same native pass rather than via a separate plan, and CI now gates `windows-latest`. The only remaining item is adding Windows targets to the release build matrix.

## Self-review notes

- **Spec coverage:** `sys` boundary (Tasks 1–4) ✓; `interprocess` transport (Task 5) ✓; config-driven setup / symlink removal (Task 6) ✓; `nix` consolidation ✓. Path `%LOCALAPPDATA%`, `sys/windows.rs`, CI matrix are Phase 2/3 by design — not in this plan.
- **Type consistency:** `process_alive`, `reap_owned`, `terminate`, `detach`, `parent_pid`, `controlling_tty`, `tree_rss_bytes`, `transport::socket_name`, `App.setup: Vec<Vec<String>>` are used identically wherever referenced across tasks.
- **External-API caveat:** the three `interprocess` identifiers flagged in Task 5 are the only names to confirm against the pinned version's docs; the compiler surfaces them immediately.
