# Daemon Health-Probe Restarts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restart a supervised dev server that was once ready but has gone hung (accepting no TCP connections), without ever killing a server that is merely slow to start.

**Architecture:** A dedicated health-probe thread runs beside the existing 500ms supervision thread in `devkitd`. Each cycle it TCP-probes every owned server's port; a server that fails `K` consecutive probes *after it has once connected* is SIGTERM'd. The dying process is then reaped and respawned by the existing supervision tick exactly like a crash. The probe thread's only mutations are its own per-child counters and that one signal — it never calls `restart` or respawns, so it cannot race the reap thread.

**Tech Stack:** Rust 2024, `anyhow`, `interprocess` local sockets, `nix` (test signals), `python3` (test fixture). Spec: `docs/superpowers/specs/2026-06-21-daemon-health-probe-restarts-design.md`.

## Global Constraints

- **Commits:** Conventional Commits. Each commit footer is EXACTLY `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` — no `Claude-Session` or other trailers.
- **Probe runs outside the lock.** A 300ms TCP connect must never run while the `sup` mutex is held — it would stall every RPC dispatch (which locks `sup`). Snapshot under a brief lock, release, probe, re-lock to fold the result.
- **Probe thread is SIGTERM-only.** It never calls `restart()` and never respawns. The existing reap path (`reap_once` → `restart`) owns respawn, crash-loop budget, and zombie reaping.
- **Owned children only.** Only children with a launch spec (`launch.is_some()`) are probed; adopted survivors are left untouched.
- **Arm on first success.** A child is eligible for a hang verdict only after one successful connect; failures before that are ignored.
- **Defaults:** `health_probe_secs = 0` (probing off → thread not spawned), `health_fail_threshold = 3`.
- **Timeless comments.** Describe behavior, not the change. No "now"/"previously"/PR/task references.
- **Search with `rg`/`fd`**, never `grep`/`find`.
- **Gate before every commit:** `cargo fmt --all`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` all green.
- **Unix-gated integration test:** `tests/supervision.rs` is `#![cfg(unix)]` and compiled out on Windows. Its RED→GREEN must be observed on WSL (see "Notes for the executor"). The Task 1–3 unit tests run on every platform including Windows.

---

## Notes for the executor

- **Lock discipline (deadlock hazard).** In the probe loop, bind each lock result to a `let` so the `MutexGuard` drops at the `;` before the next blocking call. `let targets = d.sup.lock().unwrap().probe_targets();` and `let hung = d.sup.lock().unwrap().record_probe(...);` are correct. Do **not** hold a `sup` guard across `probe_port` or `supervise::stop`. This mirrors the existing reap loop's `let dead = d.sup.lock().unwrap().reap_once();`.
- **Windows can't verify Task 4.** The integration test is `#![cfg(unix)]`. On Windows it compiles out, so `cargo test --workspace` passes without exercising it. The controller verifies Task 4 RED→GREEN on WSL:
  ```sh
  wsl bash -c 'cd /mnt/c/Users/Lev/Git/lev/devkit/.worktrees/devkitd-health-probe-restarts && \
    export CARGO_TARGET_DIR="$HOME/devkit-wsl-target" && \
    ~/.cargo/bin/cargo test --test supervision -- --nocapture'
  ```
  (`cargo` is at `~/.cargo/bin/cargo`, not on the login PATH; `python3` is present.) Windows-only green is NOT sufficient evidence for Task 4.
- **No adopted probing.** `probe_targets` filters on `launch.is_some()`. A hung adopted survivor (no launch spec) is intentionally left alone — SIGTERMing it would drop a server nothing can respawn.

---

### Task 1: Shared one-shot TCP probe helper

Factor `wait_ready`'s connect logic into a reusable one-shot `probe_port`, so the readiness wait and the health probe judge "accepting connections" identically.

**Files:**
- Modify: `crates/devkit-common/src/supervise.rs:51-66` (refactor `wait_ready`, add `probe_port`)
- Test: `crates/devkit-common/src/supervise.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub fn probe_port(port: u16) -> bool` — single `TcpStream::connect_timeout` to `127.0.0.1:port`, 300ms timeout, `true` iff it connects.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/devkit-common/src/supervise.rs`:

```rust
#[test]
fn probe_port_true_when_listening_false_when_free() {
    use std::net::TcpListener;
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    assert!(probe_port(port), "connects to a bound listener");
    drop(l);
    assert!(!probe_port(port), "fails on a freed port");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-common probe_port_true_when_listening_false_when_free`
Expected: FAIL — `cannot find function 'probe_port'`.

- [ ] **Step 3: Add `probe_port` and route `wait_ready` through it**

Replace the existing `wait_ready` (lines 51-66) with:

```rust
/// One-shot TCP liveness check: does `127.0.0.1:port` accept a connection within
/// 300 ms? This is the single attempt `wait_ready` polls and the health-probe
/// thread fires once per cycle, so both judge "accepting connections" identically.
pub fn probe_port(port: u16) -> bool {
    TcpStream::connect_timeout(
        &(std::net::Ipv4Addr::LOCALHOST, port).into(),
        Duration::from_millis(300),
    )
    .is_ok()
}

/// Poll localhost:port until it accepts a TCP connection or times out.
pub fn wait_ready(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if probe_port(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-common probe_port_true_when_listening_false_when_free`
Expected: PASS.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo test -p devkit-common
cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-common/src/supervise.rs
git commit -F - <<'EOF'
refactor(supervise): extract one-shot probe_port from wait_ready

The health-probe thread needs the same single TCP connect wait_ready
polls. Factor it into probe_port so the readiness wait and the health
probe judge "accepting connections" identically.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 2: Supervisor probe state and accessors

Give each owned child the arming flag and consecutive-failure counter, and the two accessors the probe thread drives. All logic here is deterministic and unit-tested on every platform.

**Files:**
- Modify: `src/bin/devkitd/supervisor.rs` — `Child` struct (lines 33-41), `insert_owned` (66-86), `insert_adopted` (88-101), `set_pid` (126-131); add `probe_targets` and `record_probe`.
- Test: `src/bin/devkitd/supervisor.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Key`, `Watch`, `Child`, `Launch` (existing in this file).
- Produces:
  - `pub(crate) fn probe_targets(&self) -> Vec<(Key, u16)>` — owned children (`launch.is_some()`) with `pid != 0`, as `(key, port)`.
  - `pub(crate) fn record_probe(&mut self, key: &Key, ok: bool, threshold: u32) -> Option<u32>` — folds one probe result; returns `Some(pid)` to SIGTERM when an armed child reaches `threshold` consecutive failures (and resets its counter), else `None`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `src/bin/devkitd/supervisor.rs`. The `sup()` and `live()` helpers already exist there (`sup()` builds `Supervisor::new(2, 60s, 0, 0)`; `live(&mut s, app, pid, port)` inserts an owned child with holder `/w`, role `Issue`).

```rust
fn key_for(app: &str) -> Key {
    Key { holder: "/w".into(), app: app.into(), role: Role::Issue }
}

#[test]
fn probe_failures_before_arming_are_ignored() {
    let mut s = sup();
    live(&mut s, "api", 1, 9100);
    let k = key_for("api");
    assert_eq!(s.record_probe(&k, false, 2), None);
    assert_eq!(s.record_probe(&k, false, 2), None);
    assert_eq!(s.record_probe(&k, false, 2), None);
}

#[test]
fn arms_on_success_then_signals_on_threshold() {
    let mut s = sup();
    live(&mut s, "api", 7, 9100);
    let k = key_for("api");
    assert_eq!(s.record_probe(&k, true, 2), None); // arm
    assert_eq!(s.record_probe(&k, false, 2), None); // failure 1
    assert_eq!(s.record_probe(&k, false, 2), Some(7)); // failure 2 → signal pid
    assert_eq!(s.record_probe(&k, false, 2), None); // counter reset: fresh run
}

#[test]
fn success_resets_failure_run() {
    let mut s = sup();
    live(&mut s, "api", 7, 9100);
    let k = key_for("api");
    s.record_probe(&k, true, 2); // arm
    s.record_probe(&k, false, 2); // failure 1
    assert_eq!(s.record_probe(&k, true, 2), None); // success resets the run
    assert_eq!(s.record_probe(&k, false, 2), None); // back to failure 1, not threshold
}

#[test]
fn set_pid_redisarms() {
    let mut s = sup();
    live(&mut s, "api", 7, 9100);
    let k = key_for("api");
    s.record_probe(&k, true, 2); // armed
    s.set_pid(&k, 99); // respawn → disarm
    assert_eq!(s.record_probe(&k, false, 2), None); // ignored until re-armed
    assert_eq!(s.record_probe(&k, false, 2), None);
    assert_eq!(s.record_probe(&k, true, 2), None); // re-arm
    assert_eq!(s.record_probe(&k, false, 2), None);
    assert_eq!(s.record_probe(&k, false, 2), Some(99)); // threshold on the new pid
}

#[test]
fn record_probe_on_missing_key_is_none() {
    let mut s = sup();
    let k = key_for("ghost");
    assert_eq!(s.record_probe(&k, false, 2), None);
    assert_eq!(s.record_probe(&k, true, 2), None);
}

#[test]
fn probe_targets_includes_owned_excludes_adopted() {
    let mut s = sup();
    live(&mut s, "api", 7, 9100); // owned
    s.insert_adopted(key_for("legacy"), 8, 9200, PathBuf::new()); // adopted, no launch spec
    let targets = s.probe_targets();
    assert!(targets.iter().any(|(k, p)| k.app == "api" && *p == 9100));
    assert!(
        !targets.iter().any(|(k, _)| k.app == "legacy"),
        "adopted survivors must not be probed"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit --bin devkitd record_probe`
Expected: FAIL — `no method named 'record_probe'` / `probe_targets`.

- [ ] **Step 3: Add the `Child` fields**

In `src/bin/devkitd/supervisor.rs`, extend `Child` (lines 33-41):

```rust
struct Child {
    pid: u32,
    port: u16,
    logfile: PathBuf,
    watch: Watch,
    restarts: Vec<Instant>,
    warned_mem: bool,
    launch: Option<Launch>,
    /// Has this process accepted a health probe at least once? Until it has, probe
    /// failures are ignored, so a slow-starting server is never judged hung.
    armed: bool,
    /// Consecutive failed probes since arming or the last success.
    probe_failures: u32,
}
```

In `insert_owned`, add to the `Child { … }` literal: `armed: false, probe_failures: 0,`.
In `insert_adopted`, add the same: `armed: false, probe_failures: 0,`.

- [ ] **Step 4: Reset the probe state on respawn in `set_pid`**

Replace `set_pid` (lines 126-131):

```rust
/// Update a key's pid after a successful respawn; marks the child as owned and
/// disarms its health probe — a fresh process must re-prove readiness before it
/// can be judged hung.
pub(crate) fn set_pid(&mut self, key: &Key, pid: u32) {
    if let Some(c) = self.children.get_mut(key) {
        c.pid = pid;
        c.watch = Watch::Owned;
        c.armed = false;
        c.probe_failures = 0;
    }
}
```

- [ ] **Step 5: Add `probe_targets` and `record_probe`**

Add as methods on `Supervisor` (e.g. after `reap_once`):

```rust
/// Owned children eligible for health probing: respawnable (a launch spec) and
/// with a live pid. Adopted survivors and pid-less reservations are excluded — a
/// probe restart needs a launch spec to respawn from.
pub(crate) fn probe_targets(&self) -> Vec<(Key, u16)> {
    self.children
        .iter()
        .filter(|(_, c)| c.launch.is_some() && c.pid != 0)
        .map(|(k, c)| (k.clone(), c.port))
        .collect()
}

/// Fold one probe result into a child's health state. A successful connect arms
/// the child and clears its failure run; a failure on an armed child grows the
/// consecutive-failure count. Returns the pid to SIGTERM once that count reaches
/// `threshold` — resetting the count in the same call, so a hung child is
/// signalled once per K-failure run rather than every cycle. Returns `None` for a
/// child below threshold, an unarmed child, or a key removed since the snapshot.
pub(crate) fn record_probe(&mut self, key: &Key, ok: bool, threshold: u32) -> Option<u32> {
    let c = self.children.get_mut(key)?;
    if ok {
        c.armed = true;
        c.probe_failures = 0;
        return None;
    }
    if !c.armed {
        return None;
    }
    c.probe_failures += 1;
    if c.probe_failures >= threshold {
        c.probe_failures = 0;
        Some(c.pid)
    } else {
        None
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin devkitd`
Expected: PASS — the new tests plus the existing supervisor unit tests.

- [ ] **Step 7: Gate and commit**

```bash
cargo fmt --all
cargo test -p devkit --bin devkitd
cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/devkitd/supervisor.rs
git commit -F - <<'EOF'
feat(devkitd): track health-probe state per supervised child

Add an arming flag and consecutive-failure counter to each child, plus
probe_targets (owned children to probe) and record_probe (fold one probe
result, returning the pid to SIGTERM at K failures). Arming ignores
failures until the first success, so a slow-starting server is never
judged hung; set_pid disarms a respawned process so it re-proves
readiness.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 3: Config fields for the health-probe knobs

Mirror the two knobs as `DaemonConfig` fields with defaults, matching how `max_restarts` and the memory knobs are declared (the daemon binary reads the env vars directly; these fields keep the config struct complete and default-tested).

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` — `DaemonConfig` (lines 18-33), its `Default` (35-47), and the `daemon_defaults_when_absent` test (~239-247).

**Interfaces:**
- Produces: `DaemonConfig::health_probe_secs: u64` (default `0`) and `DaemonConfig::health_fail_threshold: u32` (default `3`).

- [ ] **Step 1: Extend the defaults test**

In `crates/devkit-ports/src/config.rs`, add to `daemon_defaults_when_absent` after the `memory_action` assertion:

```rust
        assert_eq!(c.daemon.health_probe_secs, 0);
        assert_eq!(c.daemon.health_fail_threshold, 3);
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports daemon_defaults_when_absent`
Expected: FAIL — `no field 'health_probe_secs' on type '&DaemonConfig'`.

- [ ] **Step 3: Add the fields and defaults**

In `DaemonConfig` (after `memory_action`):

```rust
    /// Health-probe interval in seconds; 0 disables probing (no probe thread).
    pub health_probe_secs: u64,
    /// Consecutive post-arming probe failures before a server is judged hung.
    pub health_fail_threshold: u32,
```

In `impl Default for DaemonConfig` (after `memory_action: "warn".to_string(),`):

```rust
            health_probe_secs: 0,
            health_fail_threshold: 3,
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-ports daemon_defaults_when_absent`
Expected: PASS.

- [ ] **Step 5: Gate and commit**

```bash
cargo fmt --all
cargo test -p devkit-ports
cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-ports/src/config.rs
git commit -F - <<'EOF'
feat(config): add health-probe daemon knobs

Mirror DEVKIT_DAEMON_HEALTH_PROBE_SECS and _FAIL_THRESHOLD as
DaemonConfig fields with defaults (0 = off, K = 3), matching the existing
daemon knobs.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 4: Probe thread and integration test

Wire the probe thread into `devkitd` and prove end-to-end that a hung-but-alive server is restarted. TDD order: the Unix-gated integration test first (RED), then the thread (GREEN). **The implementer verifies RED→GREEN on WSL** (see "Notes for the executor").

**Files:**
- Modify: `tests/common/mod.rs` — add `start_with_health` + extract a shared `start_with_env`.
- Modify: `tests/supervision.rs` — add `health_probe_restarts_hung_server` + the hang fixture.
- Modify: `src/bin/devkitd/main.rs` — read the env knobs (~line 161), spawn the probe thread (after the supervision-thread block, ~line 239).

**Interfaces:**
- Consumes: `supervise::probe_port` (Task 1), `Supervisor::{probe_targets, record_probe}` (Task 2), `supervise::stop` (existing), `Request::Supervise`/`Down`, `Harness`, `pid_in_ports_json` (existing test infra).

- [ ] **Step 1: Add the harness constructor**

In `tests/common/mod.rs`, replace `start_with_idle` (lines 75-101) with a delegating pair plus a shared spawn helper:

```rust
    /// Start a daemon that idle-exits after `idle_secs` seconds of inactivity.
    pub fn start_with_idle(idle_secs: u64) -> Self {
        Self::start_with_env(&[("DEVKIT_DAEMON_IDLE_SECS", idle_secs.to_string())])
    }

    /// Start a daemon with health probing on: `probe_secs` interval, restart after
    /// `fail_threshold` consecutive post-arming probe failures.
    pub fn start_with_health(idle_secs: u64, probe_secs: u64, fail_threshold: u32) -> Self {
        Self::start_with_env(&[
            ("DEVKIT_DAEMON_IDLE_SECS", idle_secs.to_string()),
            ("DEVKIT_DAEMON_HEALTH_PROBE_SECS", probe_secs.to_string()),
            ("DEVKIT_DAEMON_HEALTH_FAIL_THRESHOLD", fail_threshold.to_string()),
        ])
    }

    /// Spawn a daemon bound to a throwaway HOME, with `extra` env on top of the
    /// fixed test env, then wait for its socket.
    fn start_with_env(extra: &[(&str, String)]) -> Self {
        let home = std::env::temp_dir().join(format!("portd-test-{}", unique()));
        // XDG_STATE_HOME is set explicitly so the daemon's state_dir() resolves
        // to a path inside the throwaway temp dir, even when the real user's
        // XDG_STATE_HOME env var is set in the surrounding shell.
        let xdg_state = home.join("state");
        std::fs::create_dir_all(xdg_state.join("devkit/logs")).expect("create test HOME dirs");

        let bin = env!("CARGO_BIN_EXE_devkitd");
        let mut cmd = Command::new(bin);
        cmd.env("HOME", &home)
            .env("XDG_STATE_HOME", &xdg_state)
            // The daemon sets this itself; pre-setting it keeps facade calls in the
            // child resolving locally rather than connecting back over the socket.
            .env("DEVKITD_SELF", "1");
        for (k, v) in extra {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawn devkitd");

        let h = Harness {
            home,
            xdg_state,
            child,
        };
        h.wait_for_socket(Duration::from_secs(5));
        h
    }
```

- [ ] **Step 2: Write the failing integration test**

Add to `tests/supervision.rs`:

```rust
/// A server that was once ready but stops accepting connections is restarted by
/// the health probe: after K consecutive failed probes the daemon SIGTERMs the
/// hung (but still alive) process, and the reap path respawns it. The fixture
/// hangs only on its first run (guarded by a sentinel file it creates), so the
/// respawn is a healthy server and the pid in ports.json changes.
#[test]
fn health_probe_restarts_hung_server() {
    // Probe every 1 s; restart after 2 consecutive post-arming failures.
    let mut h = Harness::start_with_health(3600, 1, 2);
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();
    let sentinel = h.home.join("hung-once");

    // Listen on `port`, accepting connections. On first run (sentinel absent),
    // serve ~3 s — long enough for the 1 s probe to arm — then stop accepting but
    // stay alive (hung). On later runs (sentinel present) serve forever (healthy).
    let script = r#"
import socket, os, sys, time
port = int(sys.argv[1]); sentinel = sys.argv[2]
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port)); srv.listen(16); srv.settimeout(0.2)
hang_at = None
if not os.path.exists(sentinel):
    open(sentinel, "w").close(); hang_at = time.time() + 3
while True:
    if hang_at is not None and time.time() >= hang_at:
        srv.close()
        while True:
            time.sleep(1)
    try:
        c, _ = srv.accept(); c.close()
    except socket.timeout:
        pass
"#;

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-c".into(),
            script.into(),
            port.to_string(),
            sentinel.to_str().unwrap().to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("hung.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // The fixture serves ~3 s (probe arms), then hangs; 2 failed probes →
    // SIGTERM → respawn. Poll ports.json for the new pid.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut pid2: Option<u32> = None;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if let Some(p) = pid_in_ports_json(&h.ports_json(), "api")
            && p != pid1
        {
            pid2 = Some(p);
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
    }

    assert!(
        pid2.is_some(),
        "daemon did not restart the hung server within 15 s (pid1={pid1})"
    );
    assert_ne!(pid2.unwrap(), pid1, "pid did not change after health restart");

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}
```

- [ ] **Step 3: Run the test to verify it fails (WSL)**

Run (on WSL — see "Notes for the executor"):
`~/.cargo/bin/cargo test --test supervision health_probe_restarts_hung_server -- --nocapture`
Expected: FAIL — no probe thread exists, so the hung server is never restarted; the pid never changes and the test times out after 15 s.

- [ ] **Step 4: Read the probe knobs in `main.rs`**

In `src/bin/devkitd/main.rs`, after the memory-knob reads (~line 161):

```rust
    let health_probe = Duration::from_secs(env_u64("DEVKIT_DAEMON_HEALTH_PROBE_SECS", 0));
    let health_fail_threshold = env_u32("DEVKIT_DAEMON_HEALTH_FAIL_THRESHOLD", 3);
```

- [ ] **Step 5: Spawn the probe thread**

In `src/bin/devkitd/main.rs`, after the supervision-thread block (the one ending at ~line 239, before the lock control channel):

```rust
    // Health-probe thread (enabled by DEVKIT_DAEMON_HEALTH_PROBE_SECS > 0): TCP-probe
    // each owned server's port and restart one that was once ready but has stopped
    // accepting. It runs separately from the reap loop so its blocking 300 ms connects
    // never delay reaping or idle-exit, and its only mutations are each child's probe
    // counters and a SIGTERM — the reap tick does the respawn through the crash path,
    // so the two threads never race on restart.
    if !health_probe.is_zero() {
        let d = Arc::clone(&daemon);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(health_probe);
                if d.shutdown.load(Ordering::SeqCst) {
                    break;
                }
                // Snapshot eligible (key, port) under a brief lock, then release it
                // before any connect — a 300 ms probe must never run under `sup`.
                let targets = d.sup.lock().unwrap().probe_targets();
                for (key, port) in targets {
                    let ok = devkit_common::supervise::probe_port(port);
                    // Bind the result so the `sup` guard drops before `stop`. A
                    // returned pid means K consecutive post-arming failures: the
                    // server is hung. SIGTERM it; the reap tick respawns it.
                    let hung = d.sup.lock().unwrap().record_probe(&key, ok, health_fail_threshold);
                    if let Some(pid) = hung {
                        log_line(&format!(
                            "health: {}/{} ({:?}) unresponsive — restarting",
                            key.holder, key.app, key.role
                        ));
                        devkit_common::supervise::stop(pid);
                    }
                }
            }
        });
    }
```

- [ ] **Step 6: Run the test to verify it passes (WSL)**

Run (on WSL):
`~/.cargo/bin/cargo test --test supervision -- --nocapture`
Expected: PASS — all four prior supervision tests plus `health_probe_restarts_hung_server` (pid changes from pid1 → pid2 within 15 s).

- [ ] **Step 7: Gate and commit**

Windows gate (the integration test compiles out; the unit suites and clippy still run):

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git add tests/common/mod.rs tests/supervision.rs src/bin/devkitd/main.rs
git commit -F - <<'EOF'
feat(devkitd): restart hung servers via a health probe

Add a probe thread that TCP-probes each owned server's port on a
configurable interval. A server that was once ready but fails K
consecutive probes is SIGTERMed; the existing reap tick respawns it
within the crash-loop budget. The thread runs separately from the reap
loop and only signals — never respawns — so it can't race the reaper.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 5: Document the resolved feature

Mark the phase-2 entry resolved and record the don't-make-the-probe-respawn invariant.

**Files:**
- Modify: `docs/next-features.md` — the "Health-probe restarts (daemon phase 2)" entry (lines 88-95).
- Modify: `AGENTS.md` — add an invariant after the supervisor-table bullet (lines 54-58).

- [ ] **Step 1: Mark the next-features entry resolved**

In `docs/next-features.md`, replace the `**Status:** deferred — spec §7.` line under "## Health-probe restarts (daemon phase 2)" with (note the blank line before `**Want:**` stays, so the paragraphs don't merge):

```markdown
**Status:** RESOLVED 2026-06-21 — see
`docs/superpowers/specs/2026-06-21-daemon-health-probe-restarts-design.md`. A
separate probe thread arms each owned server on its first successful connect and
SIGTERMs it after K consecutive failures; the reap path respawns it within the
crash-loop budget. Enabled via `DEVKIT_DAEMON_HEALTH_PROBE_SECS` (0 = off). The
analysis below is kept for context.
```

Leave the `**Want:**` paragraph that follows unchanged. Verify there is exactly one blank line between the RESOLVED block and `**Want:**`.

- [ ] **Step 2: Add the AGENTS.md invariant**

In `AGENTS.md`, after the supervisor-table bullet (the one ending "a concurrent prune would race it.", line 58), add:

```markdown
- **A health-probe restart goes through the crash path, not its own.** When probing
  is enabled (`DEVKIT_DAEMON_HEALTH_PROBE_SECS` > 0), the probe thread only SIGTERMs a
  server that was once ready and has stopped accepting; the supervision tick then reaps
  and respawns it within the crash-loop budget. Don't give the probe thread its own
  respawn — two respawners would race on the same key.
```

- [ ] **Step 3: Verify the docs render correctly**

Run: `rg -n "RESOLVED 2026-06-21|health-probe restart goes through" docs/next-features.md AGENTS.md`
Expected: both lines found, in their respective files.

- [ ] **Step 4: Commit**

```bash
git add docs/next-features.md AGENTS.md
git commit -F - <<'EOF'
docs: record health-probe restarts as resolved

Mark the daemon phase-2 health-probe entry resolved and note the
invariant that the probe thread only signals — the crash path owns the
respawn.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Self-Review

**Spec coverage:**
- §2 arming gate / arm-on-success → Task 2 (`record_probe` ignores pre-arm failures; tests `probe_failures_before_arming_are_ignored`, `arms_on_success_then_signals_on_threshold`).
- §2 separate probe thread → Task 4 (`main.rs` thread).
- §2 SIGTERM-only / reuse reap path → Task 4 (thread calls `stop`, never `restart`); §5 invariant in Task 5.
- §2 owned-only scope → Task 2 (`probe_targets` filter; test `probe_targets_includes_owned_excludes_adopted`).
- §3.1 `Child` fields + `set_pid` reset → Task 2 (fields; test `set_pid_redisarms`).
- §3.2 `probe_targets` / `record_probe` → Task 2.
- §4 probe thread loop + shared connect helper → Task 1 (`probe_port`) + Task 4 (thread).
- §6 config knobs + defaults → Task 3 (`DaemonConfig`) + Task 4 (env reads).
- §7 concurrency (probe outside lock, bound `let`) → Task 4 code + Notes for the executor.
- §8.1 unit tests → Task 2. §8.2 integration test + hang fixture → Task 4.
- §9 YAGNI → respected (no HTTP probe, no per-app overrides, no adopted probing, no extra backoff).

**Placeholder scan:** none — every code step shows complete code; every command has an expected result.

**Type consistency:** `probe_port(u16) -> bool` (Task 1) used in Task 4; `record_probe(&Key, bool, u32) -> Option<u32>` and `probe_targets(&self) -> Vec<(Key, u16)>` (Task 2) used in Task 4; `health_probe_secs: u64` / `health_fail_threshold: u32` (Task 3) match `env_u64` / `env_u32` (Task 4). `start_with_health(u64, u64, u32)` (Task 4 infra) matches its call site. Consistent.
