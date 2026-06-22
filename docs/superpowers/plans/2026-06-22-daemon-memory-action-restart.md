# Daemon `memory_action = "restart"` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restart a supervised dev server whose process-tree RSS stays over `memory_limit_mb` for `memory_limit_ticks` consecutive supervision ticks — through the existing crash path, within the crash-loop budget — falling back to warn-and-leave-alive once the budget is exhausted.

**Architecture:** The action runs inside the existing 500 ms supervision tick (no new thread). Per owned, live child the supervisor reads tree-RSS once, maintains a consecutive-breach counter, and — when `memory_action == "restart"` — decides per child: SIGTERM if the crash-loop budget allows (the reap path respawns it), else warn once and leave it running. The budget is *peeked* (`can_restart`) before the kill and *recorded* (`may_restart`) in `restart()` on the reap tick, so it is counted exactly once.

**Tech Stack:** Rust 2024, `anyhow`, the `devkit` workspace; daemon binary `src/bin/devkitd` behind the `daemon` feature; cross-platform `tree_rss_bytes` in `devkit-common::sys`; integration tests under `tests/` (`nix` + `python3`, Unix-gated).

## Global Constraints

Copy these values verbatim; every task implicitly includes them.

- **Spec:** `docs/superpowers/specs/2026-06-22-daemon-memory-action-restart-design.md` is the source of truth.
- **Defaults:** `memory_limit_ticks` default `3`; `memory_action` default `"warn"`; `memory_limit_mb` default `0` (off). Env vars: `DEVKIT_DAEMON_MEMORY_ACTION` (string), `DEVKIT_DAEMON_MEM_LIMIT_MB` (u64 MB, already read), `DEVKIT_DAEMON_MEM_LIMIT_TICKS` (u32, default 3).
- **Invariant — crash path only.** A memory-triggered restart only ever SIGTERMs; the reap tick respawns it. The memory path never calls `restart` or respawns. (Generalizes the phase-2 health-probe invariant.)
- **Budget counted once.** Peek with `can_restart` (no record) in the memory path; the sole record stays in `restart()` via `may_restart`. Single supervision thread → peek-then-record is consistent.
- **`Supervisor::new` signature is unchanged.** The action string and tick count are owned by `main` (the tick loop only calls `mem_limit_actions` when `memory_action == "restart"`), mirroring how `main` only spawns the probe thread when probing is enabled.
- **Commits:** Conventional Commits. Footer is exactly one trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` — no `Claude-Session`, no other trailers.
- **Comments are timeless** — describe behavior, never the change/PR/task; no "now"/"previously"/"used to".
- **Tooling:** `rg`/`fd`, never `grep`/`find`. No real project names — `example`/`exampleuser` placeholders only.
- **Gate per task:** `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` (zero warnings). Unix-gated integration tests (`#![cfg(unix)]`) are verified RED→GREEN on WSL; on Windows they compile out.
- **CI rule:** tests that spawn/reap processes poll for the expected state — never sleep a fixed interval and assert.

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `crates/devkit-ports/src/config.rs` | `memory_limit_ticks` field + default + comment + default-test assertion | 1 |
| `src/bin/devkitd/supervisor.rs` | `can_restart` peek; `Child` fields; `MemAction`; `mem_limit_actions`; unit tests | 2, 3 |
| `src/bin/devkitd/main.rs` | env reads; misconfiguration warning; limit-action loop in the supervision tick | 4 |
| `tests/common/mod.rs` | `start_with_memory` harness helper | 4 |
| `tests/supervision.rs` | Unix-gated restart + budget-exhaustion integration tests + balloon fixture | 4 |
| `docs/configuration.md` | no-kill escape-hatch note | 5 |
| `docs/next-features.md` | mark the phase-3 entry resolved | 5 |
| `AGENTS.md` | generalize the crash-path invariant to cover memory restart | 5 |

---

## Task 1: Config field `memory_limit_ticks`

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (struct ~16-37, `Default` ~39-53, comment ~30-32, test ~244-256)

**Interfaces:**
- Consumes: nothing.
- Produces: `DaemonConfig::memory_limit_ticks: u32` (default `3`). `main.rs` reads the env var directly (Task 4); this field is for completeness and the default test.

- [ ] **Step 1: Extend the failing default test**

In `crates/devkit-ports/src/config.rs`, add an assertion to `daemon_defaults_when_absent` (after the `health_fail_threshold` line, ~255):

```rust
        assert_eq!(c.daemon.health_fail_threshold, 3);
        assert_eq!(c.daemon.memory_limit_ticks, 3);
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports --lib daemon_defaults_when_absent`
Expected: FAIL to compile — `no field memory_limit_ticks on type DaemonConfig`.

- [ ] **Step 3: Add the field and its default, and update the `memory_action` comment**

Add the field to `DaemonConfig` (after `memory_action`, before `health_probe_secs`, ~32):

```rust
    /// Action when tree-RSS crosses `memory_limit_mb`: "warn" (log a line) or
    /// "restart" (SIGTERM and let the crash path respawn). Any other value falls
    /// back to warn.
    pub memory_action: String,
    /// Consecutive supervision ticks at or over `memory_limit_mb` before the
    /// restart action fires (debounces transient allocation spikes).
    pub memory_limit_ticks: u32,
```

Add the default to `impl Default for DaemonConfig` (after `memory_action`, ~48):

```rust
            memory_action: "warn".to_string(),
            memory_limit_ticks: 3,
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p devkit-ports --lib daemon_defaults_when_absent`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs
git commit -m "feat(devkitd): add memory_limit_ticks daemon config" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: `can_restart` budget peek

**Files:**
- Modify: `src/bin/devkitd/supervisor.rs` (add method after `may_restart` ~168; add tests in the `tests` mod ~285+)

**Interfaces:**
- Consumes: `Supervisor` fields `children`, `window`, `max_restarts`; `Key`; `Role` (Copy).
- Produces: `pub(crate) fn can_restart(&mut self, holder: &str, app: &str, role: Role) -> bool` — a non-recording counterpart to `may_restart`, used by `mem_limit_actions` (Task 3).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/bin/devkitd/supervisor.rs` (after `restart_budget_is_per_child`, ~308):

```rust
    #[test]
    fn can_restart_peeks_without_consuming_budget() {
        let mut s = sup(); // max_restarts = 2
        live(&mut s, "api", 1, 9100);
        // Peeking any number of times must not consume the budget.
        assert!(s.can_restart("/w", "api", Role::Issue));
        assert!(s.can_restart("/w", "api", Role::Issue));
        assert!(s.can_restart("/w", "api", Role::Issue));
        // Two real attempts still succeed; the third is blocked.
        assert!(s.may_restart("/w", "api", Role::Issue));
        assert!(s.may_restart("/w", "api", Role::Issue));
        assert!(!s.may_restart("/w", "api", Role::Issue));
        // Once exhausted, the peek reports false too.
        assert!(!s.can_restart("/w", "api", Role::Issue));
    }

    #[test]
    fn can_restart_unknown_key_is_false() {
        let mut s = sup();
        assert!(!s.can_restart("/w", "ghost", Role::Issue));
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit --bin devkitd can_restart`
Expected: FAIL to compile — `no method named can_restart`.

- [ ] **Step 3: Implement `can_restart`**

Add after `may_restart` (~168) in `src/bin/devkitd/supervisor.rs`:

```rust
    /// Whether a restart is currently allowed for `key` under the crash-loop
    /// budget, WITHOUT recording one. Prunes timestamps outside the window (an
    /// idempotent cleanup) but never pushes. An unknown key returns `false`, like
    /// `may_restart`. The recording counterpart is `may_restart`, called from the
    /// reap path; this peek lets the memory path decide whether to kill before
    /// `restart` charges the budget, so a restart is counted exactly once.
    pub(crate) fn can_restart(&mut self, holder: &str, app: &str, role: Role) -> bool {
        let key = Key {
            holder: holder.into(),
            app: app.into(),
            role,
        };
        let now = Instant::now();
        let window = self.window;
        let Some(entry) = self.children.get_mut(&key) else {
            return false;
        };
        entry.restarts.retain(|t| now.duration_since(*t) < window);
        (entry.restarts.len() as u32) < self.max_restarts
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin devkitd can_restart`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkitd/supervisor.rs
git commit -m "feat(devkitd): add non-recording can_restart budget peek" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: `Child` memory state, `MemAction`, and `mem_limit_actions`

**Files:**
- Modify: `src/bin/devkitd/supervisor.rs` (`Child` ~33-46; `insert_owned` ~71-93; `insert_adopted` ~95-110; `set_pid` ~137-143; add `MemAction` + `mem_limit_actions` near `memory_breaches` ~250; tests ~285+)

**Interfaces:**
- Consumes: `can_restart` (Task 2); `tree_rss_bytes` (already imported in `supervisor.rs`); `Child`, `Key`, `Role`.
- Produces:
  - `pub(crate) enum MemAction { Restart { key: Key, pid: u32, rss: u64 }, GiveUp { key: Key, rss: u64 } }`
  - `pub(crate) fn mem_limit_actions(&mut self, limit_ticks: u32) -> Vec<MemAction>`
  - `Child` fields `mem_over: u32`, `mem_gave_up: bool`, reset by `set_pid`. Consumed by `main.rs` (Task 4).

- [ ] **Step 1: Write the failing unit tests**

Add to the `tests` module in `src/bin/devkitd/supervisor.rs` (after the Task 2 tests). These drive the state machine by setting `mem_limit` low against the test process's own tree (the existing `tree_rss_bytes` test confirms `tree_rss_bytes(self) > 0`), and by injecting the child's pid as the current process so its RSS is always over a 1-byte limit:

```rust
    /// A supervisor whose mem_limit is 1 byte — every live child is "over".
    fn sup_mem(max_restarts: u32) -> Supervisor {
        Supervisor::new(max_restarts, Duration::from_secs(60), 0, 1)
    }

    /// Register an owned child whose pid is this test process, so `tree_rss_bytes`
    /// returns a real, non-zero RSS that exceeds the 1-byte limit every tick.
    fn live_self(s: &mut Supervisor, app: &str) {
        live(s, app, std::process::id(), 9100);
    }

    #[test]
    fn mem_no_action_below_threshold() {
        let mut s = sup_mem(5);
        live_self(&mut s, "api");
        // limit_ticks = 3: first two over-limit ticks produce nothing.
        assert!(s.mem_limit_actions(3).is_empty());
        assert!(s.mem_limit_actions(3).is_empty());
    }

    #[test]
    fn mem_restart_at_threshold_then_resets() {
        let mut s = sup_mem(5);
        live_self(&mut s, "api");
        let pid = std::process::id();
        assert!(s.mem_limit_actions(3).is_empty()); // tick 1
        assert!(s.mem_limit_actions(3).is_empty()); // tick 2
        let acts = s.mem_limit_actions(3); // tick 3 → restart
        assert!(
            matches!(acts.as_slice(), [MemAction::Restart { pid: p, .. }] if *p == pid),
            "expected one Restart for our pid, got {acts:?}"
        );
        // Counter reset: it takes another `limit_ticks` ticks to fire again.
        assert!(s.mem_limit_actions(3).is_empty());
    }

    #[test]
    fn mem_gives_up_once_when_budget_exhausted() {
        let mut s = sup_mem(1); // one restart allowed
        live_self(&mut s, "api");
        let k = key_for("api");
        // Exhaust the budget directly.
        assert!(s.may_restart("/w", "api", Role::Issue));
        assert!(!s.may_restart("/w", "api", Role::Issue));
        // Threshold reached → GiveUp exactly once (budget gone).
        s.mem_limit_actions(2);
        let acts = s.mem_limit_actions(2);
        assert!(
            matches!(acts.as_slice(), [MemAction::GiveUp { .. }]),
            "expected one GiveUp, got {acts:?}"
        );
        // Suppressed afterwards (mem_gave_up) while still over the limit.
        s.mem_limit_actions(2);
        assert!(
            s.mem_limit_actions(2).is_empty(),
            "GiveUp must not repeat every breach"
        );
        // set_pid clears the give-up state on respawn (re-arms the warning); the
        // self-pid fixture stays over the 1-byte limit, so this only confirms the
        // reset path runs without panicking — the mem_over reset is asserted in
        // `set_pid_resets_mem_counter`.
        s.set_pid(&k, std::process::id());
    }

    #[test]
    fn mem_actions_skip_adopted_and_empty_when_off() {
        let mut s = sup_mem(5);
        s.insert_adopted(key_for("legacy"), std::process::id(), 9200, PathBuf::new());
        // Adopted survivor has no launch spec → never a candidate.
        for _ in 0..5 {
            assert!(s.mem_limit_actions(3).is_empty());
        }
        // mem_limit == 0 → always empty.
        let mut off = Supervisor::new(5, Duration::from_secs(60), 0, 0);
        off.insert_owned(
            key_for("api"),
            std::process::id(),
            9100,
            PathBuf::new(),
            Launch { argv: vec!["true".into()], cwd: ".".into(), env: std::collections::BTreeMap::new() },
        );
        assert!(off.mem_limit_actions(3).is_empty());
    }

    #[test]
    fn set_pid_resets_mem_counter() {
        let mut s = sup_mem(5);
        live_self(&mut s, "api");
        let k = key_for("api");
        s.mem_limit_actions(3); // tick 1
        s.mem_limit_actions(3); // tick 2 (counter now 2)
        s.set_pid(&k, std::process::id()); // respawn resets mem_over to 0
        assert!(s.mem_limit_actions(3).is_empty()); // back to tick 1, not threshold
        assert!(s.mem_limit_actions(3).is_empty()); // tick 2
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit --bin devkitd mem_`
Expected: FAIL to compile — `MemAction` and `mem_limit_actions` do not exist, and `Child` has no `mem_over` / `mem_gave_up`.

- [ ] **Step 3: Add the `Child` fields and initialise them**

In `Child` (after `probe_failures`, ~45):

```rust
    /// Consecutive supervision ticks this child's tree-RSS has been at or over
    /// `mem_limit`. Reset to 0 when it drops below the limit or when a memory
    /// action is decided for it this tick.
    mem_over: u32,
    /// Whether the "budget exhausted, leaving over-limit server alive" warning
    /// has already fired for the current breach episode. Re-armed (false) when
    /// the child drops below the limit or is respawned.
    mem_gave_up: bool,
```

Initialise in `insert_owned` (after `probe_failures: 0,` ~90) and `insert_adopted` (after `probe_failures: 0,` ~107):

```rust
                armed: false,
                probe_failures: 0,
                mem_over: 0,
                mem_gave_up: false,
```

Reset in `set_pid` (after `c.probe_failures = 0;` ~142):

```rust
            c.armed = false;
            c.probe_failures = 0;
            c.mem_over = 0;
            c.mem_gave_up = false;
```

- [ ] **Step 4: Add `MemAction` and `mem_limit_actions`**

Add `MemAction` just above the `impl Supervisor` block (near `Watch`/`Child`, ~46), so it is a sibling type:

```rust
/// One memory-limit decision for a supervised child this tick.
#[derive(Debug, PartialEq)]
pub(crate) enum MemAction {
    /// Crash-loop budget remains: SIGTERM this pid; the reap tick respawns it.
    Restart { key: Key, pid: u32, rss: u64 },
    /// Budget exhausted: warn once and leave the over-limit server running.
    GiveUp { key: Key, rss: u64 },
}
```

Add the method next to `memory_breaches` (after it, before `mem_limit`, ~250):

```rust
    /// Advance every owned, live child's consecutive-breach counter against
    /// `mem_limit` and return the memory actions to take this tick. Each child's
    /// tree-RSS is read once. A child below `mem_limit` (or any child when
    /// `mem_limit == 0`) has its counter and give-up flag cleared and yields no
    /// action. A child at or over the limit for `limit_ticks` consecutive ticks
    /// yields exactly one action: `Restart` when `can_restart` allows, else
    /// `GiveUp` the first time per episode (suppressed by `mem_gave_up`
    /// afterwards). The counter resets on any decision, so it re-checks roughly
    /// every `limit_ticks` ticks while still over the limit — picking the server
    /// back up once the crash-loop window cools down. Owned children only:
    /// adopted survivors and pid-less reservations have no launch spec to
    /// respawn from and are skipped, like `probe_targets`.
    pub(crate) fn mem_limit_actions(&mut self, limit_ticks: u32) -> Vec<MemAction> {
        if self.mem_limit == 0 {
            return Vec::new();
        }
        // Pass 1: one RSS read per eligible child; advance or reset its breach
        // counter; collect the keys (with their RSS) that have been over long
        // enough to act on.
        let mut candidates: Vec<(Key, u64)> = Vec::new();
        for (key, child) in self.children.iter_mut() {
            if child.launch.is_none() || child.pid == 0 {
                continue;
            }
            let rss = tree_rss_bytes(child.pid);
            if rss >= self.mem_limit {
                child.mem_over = child.mem_over.saturating_add(1);
                if child.mem_over >= limit_ticks {
                    candidates.push((key.clone(), rss));
                }
            } else {
                child.mem_over = 0;
                child.mem_gave_up = false;
            }
        }
        // Pass 2: resolve each candidate against the budget (peek, no record)
        // and build its action. `can_restart` needs `&mut self`, so this runs
        // after pass 1's borrow ends.
        let mut actions = Vec::new();
        for (key, rss) in candidates {
            let allowed = self.can_restart(&key.holder, &key.app, key.role);
            let Some(child) = self.children.get_mut(&key) else {
                continue;
            };
            child.mem_over = 0;
            if allowed {
                actions.push(MemAction::Restart {
                    pid: child.pid,
                    key,
                    rss,
                });
            } else if !child.mem_gave_up {
                child.mem_gave_up = true;
                actions.push(MemAction::GiveUp { key, rss });
            }
        }
        actions
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin devkitd mem_`
Expected: PASS (all five new tests). Then run `cargo test -p devkit --bin devkitd` to confirm no existing supervisor test regressed.

- [ ] **Step 6: Clippy**

Run: `cargo clippy -p devkit --bin devkitd --all-targets -- -D warnings`
Expected: zero warnings.

- [ ] **Step 7: Commit**

```bash
git add src/bin/devkitd/supervisor.rs
git commit -m "feat(devkitd): detect memory-limit breaches and decide restarts" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Wire the action into the daemon + integration tests

**Files:**
- Modify: `src/bin/devkitd/main.rs` (env reads ~158-163; misconfig warning before the supervision thread ~200; the limit-action loop in the supervision tick after the `memory_breaches` loop ~238)
- Modify: `tests/common/mod.rs` (add `start_with_memory` helper after `start_with_health` ~90)
- Modify: `tests/supervision.rs` (add two Unix-gated tests + balloon fixture)

**Interfaces:**
- Consumes: `mem_limit_actions`, `MemAction` (Task 3); `supervise::stop`; `log_line`; `env_u32`.
- Produces: the live behavior; `Harness::start_with_memory(idle_secs, limit_mb, ticks, max_restarts)`.

- [ ] **Step 1: Write the failing integration test (the balloon-restart RED→GREEN)**

Add to `tests/common/mod.rs` after `start_with_health` (~90):

```rust
    /// Start a daemon with memory_action=restart: act past `limit_mb` after
    /// `ticks` consecutive over-limit supervision ticks, within `max_restarts`.
    pub fn start_with_memory(idle_secs: u64, limit_mb: u64, ticks: u32, max_restarts: u32) -> Self {
        Self::start_with_env(&[
            ("DEVKIT_DAEMON_IDLE_SECS", idle_secs.to_string()),
            ("DEVKIT_DAEMON_MEMORY_ACTION", "restart".to_string()),
            ("DEVKIT_DAEMON_MEM_LIMIT_MB", limit_mb.to_string()),
            ("DEVKIT_DAEMON_MEM_LIMIT_TICKS", ticks.to_string()),
            ("DEVKIT_DAEMON_MAX_RESTARTS", max_restarts.to_string()),
        ])
    }
```

Add to `tests/supervision.rs` (after `health_probe_restarts_hung_server`, ~310). The fixture binds and accepts (so `wait_ready` succeeds), then on its first run (sentinel absent) allocates and *touches* ~120 MB so tree-RSS, not just virtual size, grows past the limit; on respawn (sentinel present) it stays small, so the pid stabilises after one restart:

```rust
/// A server whose tree-RSS balloons past `memory_limit_mb` is restarted: after
/// `memory_limit_ticks` consecutive over-limit ticks the daemon SIGTERMs it and
/// the reap path respawns it. The fixture balloons only on its first run
/// (sentinel-guarded), so the respawn is small and the pid in ports.json changes
/// exactly once.
#[test]
fn memory_restart_over_limit_server() {
    // Act past 60 MB after 2 consecutive over-limit ticks; generous restart budget.
    let mut h = Harness::start_with_memory(3600, 60, 2, 5);
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();
    let sentinel = h.home.join("ballooned-once");

    // Bind + accept so wait_ready succeeds. First run (sentinel absent): touch
    // ~120 MB resident, then keep serving (alive, over limit). Later runs: small.
    let script = r#"
import socket, os, sys, time
port = int(sys.argv[1]); sentinel = sys.argv[2]
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port)); srv.listen(16); srv.settimeout(0.2)
buf = None
if not os.path.exists(sentinel):
    open(sentinel, "w").close()
    buf = bytearray(120 * 1024 * 1024)
    for i in range(0, len(buf), 4096):
        buf[i] = 1  # fault in each page so RSS (not just VSZ) grows
while True:
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
        logfile: h.home.join("balloon.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // Poll up to 15 s for the pid to change (daemon restarted the over-limit server).
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
        "daemon did not restart the over-limit server within 15 s (pid1={pid1})"
    );
    assert_ne!(pid2.unwrap(), pid1, "pid did not change after memory restart");

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}
```

- [ ] **Step 2: Run the test to verify it fails (RED, on WSL)**

Run (WSL): `cargo test -p devkit --test supervision memory_restart_over_limit_server`
Expected: FAIL — "daemon did not restart the over-limit server within 15 s" (the action is not wired into `main` yet).

- [ ] **Step 3: Read the env knobs and warn on misconfiguration**

In `src/bin/devkitd/main.rs`, after the existing env reads (~163), add:

```rust
    let health_fail_threshold = env_u32("DEVKIT_DAEMON_HEALTH_FAIL_THRESHOLD", 3);
    let memory_action =
        std::env::var("DEVKIT_DAEMON_MEMORY_ACTION").unwrap_or_else(|_| "warn".to_string());
    let mem_restart = memory_action == "restart";
    let mem_limit_ticks = env_u32("DEVKIT_DAEMON_MEM_LIMIT_TICKS", 3);
    if mem_restart && mem_limit > 0 && mem_warn > 0 && mem_limit <= mem_warn {
        log_line(&format!(
            "memory: limit ({} MB) is at or below warn ({} MB) — warn threshold is redundant",
            mem_limit / 1024 / 1024,
            mem_warn / 1024 / 1024
        ));
    }
```

- [ ] **Step 4: Add the limit-action loop to the supervision tick**

In the combined supervision thread, immediately after the `memory_breaches` warn loop (after its closing `}`, ~238), add:

```rust
                // Memory limit: when the action is "restart", SIGTERM a server
                // that has been over the limit for `mem_limit_ticks` consecutive
                // ticks (the reap tick respawns it within the crash-loop budget);
                // once the budget is exhausted, warn and leave it running.
                if mem_restart {
                    for action in d.sup.lock().unwrap().mem_limit_actions(mem_limit_ticks) {
                        match action {
                            supervisor::MemAction::Restart { key, pid, rss } => {
                                log_line(&format!(
                                    "memory: {}/{} ({:?}) tree-RSS {} MB over limit — restarting",
                                    key.holder,
                                    key.app,
                                    key.role,
                                    rss / 1024 / 1024
                                ));
                                devkit_common::supervise::stop(pid);
                            }
                            supervisor::MemAction::GiveUp { key, rss } => {
                                log_line(&format!(
                                    "memory: {}/{} ({:?}) tree-RSS {} MB over limit but crash-loop budget exhausted — leaving alive",
                                    key.holder,
                                    key.app,
                                    key.role,
                                    rss / 1024 / 1024
                                ));
                            }
                        }
                    }
                }
```

Note the bound-`let` discipline: `mem_limit_actions` is collected into the `for` loop's temporary, dropping the `sup` guard before each `supervise::stop` — never SIGTERM under the lock.

- [ ] **Step 5: Run the integration test to verify it passes (GREEN, on WSL)**

Run (WSL): `cargo test -p devkit --test supervision memory_restart_over_limit_server`
Expected: PASS.

- [ ] **Step 6: Write the budget-exhaustion test (distinguishes GiveUp from drop)**

Add to `tests/supervision.rs` after the previous test. The fixture balloons on *every* run (no sentinel), so it is restarted until the budget (`max_restarts = 1`) is exhausted; the daemon then leaves it alive. Assert the pid changes (at least one restart) and the entry remains present after restarts cease:

```rust
/// A server that re-balloons on every start is restarted only within the
/// crash-loop budget; once exhausted the daemon leaves it alive (warns) rather
/// than dropping it — unlike a health-probe give-up. With max_restarts = 1 the
/// pid changes once, then the entry stays present.
#[test]
fn memory_restart_gives_up_within_budget() {
    // Act past 60 MB after 2 over-limit ticks; allow only ONE restart.
    let mut h = Harness::start_with_memory(3600, 60, 2, 1);
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();

    // Balloon on every run (no sentinel): each respawn re-breaches the limit.
    let script = r#"
import socket, os, sys, time
port = int(sys.argv[1])
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", port)); srv.listen(16); srv.settimeout(0.2)
buf = bytearray(120 * 1024 * 1024)
for i in range(0, len(buf), 4096):
    buf[i] = 1
while True:
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
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("balloon-loop.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // One restart is allowed: wait for the pid to change once.
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
        "daemon did not perform the one allowed restart (pid1={pid1})"
    );

    // After the budget is exhausted the server must stay ALIVE (left running),
    // not dropped. Give it several seconds of further over-limit ticks, then
    // confirm the entry is still present.
    std::thread::sleep(Duration::from_secs(3));
    let json = h.ports_json();
    assert!(
        json.contains("\"api\""),
        "over-limit server was dropped after budget exhaustion — it should be left alive: {json}"
    );

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}
```

- [ ] **Step 7: Run the exhaustion test (GREEN, on WSL)**

Run (WSL): `cargo test -p devkit --test supervision memory_restart_gives_up_within_budget`
Expected: PASS.

- [ ] **Step 8: Full gate**

Run (WSL for the Unix-gated tests): `cargo test --workspace`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: all tests pass; zero warnings. On Windows, the two new tests compile out (`#![cfg(unix)]`); confirm the Windows build/clippy is clean too.

- [ ] **Step 9: Commit**

```bash
git add src/bin/devkitd/main.rs tests/common/mod.rs tests/supervision.rs
git commit -m "feat(devkitd): restart servers over the memory limit" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Documentation and invariant

**Files:**
- Modify: `docs/configuration.md` (after the `[apps.<name>]` table, ~52)
- Modify: `docs/next-features.md` (the `## memory_action = "restart"` entry, ~52-60)
- Modify: `AGENTS.md` (the health-probe invariant bullet, ~61-65)

**Interfaces:** none (docs only).

- [ ] **Step 1: Add the no-kill escape-hatch note to `docs/configuration.md`**

After the `[apps.<name>]` table (after the `setup` row, ~52), add:

```markdown

To enforce a hard per-app memory cap *without* the daemon restarting the server,
set a runtime or OS limit through the app's `static_env` — e.g.
`static_env = { NODE_OPTIONS = "--max-old-space-size=2048" }`, or wrap `launch`
in a `ulimit -v` shell. The runtime/OS aborts the process on breach and the
daemon's crash-restart recovers it; this keeps enforcement in the runtime rather
than the daemon's `memory_action`.
```

- [ ] **Step 2: Mark the phase-3 entry resolved in `docs/next-features.md`**

Replace the body of the `## memory_action = "restart"` entry (~54-60) with:

```markdown
**Status:** RESOLVED 2026-06-22 — see
`docs/superpowers/specs/2026-06-22-daemon-memory-action-restart-design.md`. A
server whose tree-RSS stays over `memory_limit_mb` for `memory_limit_ticks`
consecutive supervision ticks is SIGTERM'd and respawned through the crash path
within the crash-loop budget; once the budget is exhausted the daemon warns and
leaves the server alive (`memory_action = "restart"`, default off).
```

- [ ] **Step 3: Generalize the invariant in `AGENTS.md`**

Replace the health-probe invariant bullet (the "A health-probe restart goes through the crash path, not its own." item, ~61-65) with a generalized one:

```markdown
- **A non-crash restart goes through the crash path, not its own.** When the
  health probe (`DEVKIT_DAEMON_HEALTH_PROBE_SECS` > 0) judges a server hung, or
  the memory action (`memory_action = "restart"`) finds one over
  `memory_limit_mb` for `memory_limit_ticks` ticks, it only SIGTERMs the
  server; the supervision tick then reaps and respawns it within the crash-loop
  budget. Neither path gets its own respawn — two respawners would race on the
  same key. The memory path *peeks* the budget (`can_restart`) before killing so
  the kill is skipped once exhausted (warn and leave alive), but the budget is
  recorded only in `restart()`, so a restart counts exactly once.
```

- [ ] **Step 4: Verify the docs build/reference**

Run: `rg -n "memory_action|memory_limit_ticks|non-crash restart" docs/ AGENTS.md`
Expected: the new references appear; no stale "deferred" status on the memory entry.

- [ ] **Step 5: Commit**

```bash
git add docs/configuration.md docs/next-features.md AGENTS.md
git commit -m "docs: document memory_action restart and escape hatch" \
  -m "Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Final verification (after all tasks)

- [ ] `cargo test --workspace` on WSL (Unix-gated integration tests run) — all green.
- [ ] `cargo test --workspace` on Windows native msvc — green (new integration tests compile out).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings on both.
- [ ] `cargo fmt --all --check` — clean.
- [ ] Whole-branch review on the most capable model, then finishing-a-development-branch.

## Self-Review notes

- **Spec coverage:** activation/config (Task 1, 4) → spec §5; debounce + state (Task 3) → §2/§3; budget peek (Task 2) → §3.2; tick integration + misconfig warn (Task 4) → §4/§5.1; tests (Task 3 unit, Task 4 integration) → §9; escape hatch + invariant + resolved status (Task 5) → §8/§11.
- **Type consistency:** `MemAction { Restart { key, pid, rss }, GiveUp { key, rss } }`, `mem_limit_actions(&mut self, limit_ticks: u32) -> Vec<MemAction>`, `can_restart(&mut self, holder: &str, app: &str, role: Role) -> bool`, `memory_limit_ticks: u32` — used identically in every task that references them.
- **Test-process pid trick:** the unit tests register the test process's own pid as the child so `tree_rss_bytes` returns a real RSS over a 1-byte limit, exercising the state machine without spawning anything — the same spirit as the existing `tree_rss_bytes(self) > 0` test.

## Unresolved questions

1. **Balloon size vs. limit on CI.** The integration tests touch ~120 MB against a 60 MB limit; the Python interpreter's baseline RSS (~10–20 MB) sits well under 60 MB so the small respawn is reliably under-limit. If a loaded WSL/CI runner shows flakiness, widen the gap (e.g. 200 MB balloon / 80 MB limit) rather than shrinking it. Confirm during execution on WSL.
2. **`memory_limit_ticks` env name.** Plan uses `DEVKIT_DAEMON_MEM_LIMIT_TICKS` (matching the `MEM_LIMIT_MB` abbreviation). Flag if you'd prefer `DEVKIT_DAEMON_MEMORY_LIMIT_TICKS` for consistency with `MEMORY_ACTION`.
