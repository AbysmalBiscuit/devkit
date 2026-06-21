# Daemon-Authoritative Liveness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `devkitd`'s in-memory supervisor table the sole authority on whether a reaped child is a crash (restart) or an intentional stop (let die), removing a daemon-internal race where a concurrent registry prune suppresses a legitimate restart.

**Architecture:** The supervision tick stops re-reading the `ports.json`/`d.ports` row to classify an exit. Any key returned by `Supervisor::reap_once()` is treated as a crash and handed to `restart()`, because the only way a supervised key leaves the table is an explicit `Down` (which removes it before killing the child) or a give-up. The registry row becomes derived state that `record_pid` rebuilds on respawn.

**Tech Stack:** Rust (edition 2024); the existing `devkitd` binary (`src/bin/devkitd/`), its `Supervisor` table, and the `tests/supervision.rs` integration harness. No new dependencies, no schema change, no wire-protocol change.

Spec: `docs/superpowers/specs/2026-06-21-daemon-authoritative-liveness-design.md`

## Global Constraints

- Conventional Commits. Commit footer EXACTLY `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` and NO other trailers (do NOT add `Claude-Session`).
- TDD: write the failing test first. `cargo test --workspace` is the merge gate; it must stay green.
- `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings. `cargo fmt --all` before each commit (CI enforces `--check`).
- Comments are timeless: describe what the code does and why, never the change/PR/task. No "used to", "now", "previously", no debounce/old-mechanism references left dangling.
- No real project names anywhere — use `example`/`exampleuser` placeholders only.
- Tests that spawn or reap processes poll for the expected state; never sleep a fixed interval and assert (a loaded Windows runner exits a child later than a short sleep allows).
- Supervision integration tests are Unix-only (`#![cfg(unix)]` already at the top of `tests/supervision.rs`); they drive POSIX signals and a `python3` http server.
- Search with `rg`/`fd`, never `grep`/`find`.

---

### Task 1: Make the restart decision table-authoritative

Replace the racy reap→debounce→row-read→classify block with a direct `reap_once → restart` dispatch, add the `Supervisor::contains` accessor that lets `restart()` distinguish a concurrently-removed key from an adopted survivor, mark the `down()` remove-before-kill ordering load-bearing, and prove it with a regression test that restarts under concurrent registry pruning. Update the two existing supervision-test comments that describe the retired mechanism.

**Files:**
- Modify: `src/bin/devkitd/main.rs` — supervision tick (the `reap_once` block, ~lines 216-237) and `restart()` (~lines 345-374)
- Modify: `src/bin/devkitd/supervisor.rs` — add `contains()` near `remove()` (~line 103)
- Modify: `src/bin/devkitd/server.rs` — add a load-bearing comment in `down()` at the remove-before-stop loop (~lines 170-176)
- Modify/Test: `tests/supervision.rs` — add `restart_survives_concurrent_snapshot`; rewrite the doc comments on `restart_after_kill` and `down_does_not_restart` and the debounce comment inside `restart_after_kill`

**Interfaces:**
- Consumes (existing, unchanged signatures):
  - `Supervisor::reap_once(&mut self) -> Vec<Key>` — keys whose process is gone.
  - `Supervisor::remove(&mut self, key: &Key) -> Option<u32>`
  - `Supervisor::launch_of(&self, key: &Key) -> Option<(Launch, PathBuf, u16)>`
  - `Supervisor::may_restart(&mut self, holder: &str, app: &str, role: Role) -> bool`
  - `Daemon::respawn(self: &Arc<Self>, key: &Key)`
  - Harness API in `tests/common/mod.rs`: `Harness::start()`, `h.request(&Request) -> Response`, `h.socket() -> PathBuf`, `h.home`, `h.ports_json() -> String`, `h.request(&Request::Down{..})`, `h.shutdown()`; free functions `common::free_port() -> u16`, `common::pid_in_ports_json(&str, &str) -> Option<u32>`.
- Produces (for Task 2's docs to describe accurately):
  - `Supervisor::contains(&self, key: &Key) -> bool`
  - Invariant: a reaped key is a crash; `Down` removes the key from the table before signalling.

- [ ] **Step 1: Write the failing regression test**

Add this test to `tests/supervision.rs` (after the existing `down_does_not_restart`, before the final `}` of the file is not needed — tests are top-level `fn`s):

```rust
/// A supervised child that is SIGKILLed restarts even while clients are pruning
/// the registry. The supervisor table — not the registry row — decides crash vs.
/// stop, so a concurrent `Snapshot` (which drops the dead-pid row inside the
/// daemon) cannot make a crash look like an intentional stop.
#[test]
fn restart_survives_concurrent_snapshot() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();

    let resp = h.request(&Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-m".into(),
            "http.server".into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("snap.log"),
        base_port: port,
    });
    assert!(
        matches!(&resp, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "supervise did not become ready: {resp:?}"
    );

    let pid1 =
        pid_in_ports_json(&h.ports_json(), "api").expect("no pid in ports.json after supervise");

    // Hammer Snapshot from independent connections: each snapshot prunes the
    // dead-pid row inside the daemon, reproducing the prune that races the restart.
    let sock = h.socket();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let hammer = std::thread::spawn(move || {
        use devkit_ports::daemon::proto;
        use devkit_ports::daemon::transport;
        use interprocess::local_socket::Stream;
        use interprocess::local_socket::traits::Stream as _;
        use std::io::{BufReader, BufWriter};
        while !stop_thread.load(Ordering::Relaxed) {
            if let Ok(name) = transport::socket_name(&sock)
                && let Ok(stream) = Stream::connect(name)
            {
                let (recv, send) = stream.split();
                let mut writer = BufWriter::new(send);
                let mut reader = BufReader::new(recv);
                if proto::send(&mut writer, &Request::Snapshot).is_ok() {
                    let _ = proto::recv::<_, Response>(&mut reader);
                }
            }
        }
    });

    // SIGKILL the child to simulate a crash.
    kill(Pid::from_raw(pid1 as i32), Signal::SIGKILL).expect("SIGKILL failed");

    // The daemon must restart it despite the concurrent pruning. Poll for a new pid.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut pid2: Option<u32> = None;
    loop {
        std::thread::sleep(Duration::from_millis(150));
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

    stop.store(true, Ordering::Relaxed);
    let _ = hammer.join();

    assert!(
        pid2.is_some(),
        "daemon did not restart the killed server under concurrent snapshots (pid1={pid1})"
    );
    assert_ne!(pid2.unwrap(), pid1, "pid did not change after respawn");

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}
```

- [ ] **Step 2: Run the test and watch it fail**

Run: `cargo test -p devkit --test supervision restart_survives_concurrent_snapshot -- --nocapture`
(If the binary package name differs, the workspace form also works: `cargo test --test supervision restart_survives_concurrent_snapshot`.)
Expected: FAIL. The continuous hammer reliably lands a `Snapshot` in the daemon's 200 ms post-reap debounce window, pruning the dead-pid row before the tick re-reads it, so no restart fires: the assertion `daemon did not restart the killed server under concurrent snapshots` trips (pid2 is `None`).

- [ ] **Step 3: Add the `Supervisor::contains` accessor**

In `src/bin/devkitd/supervisor.rs`, add directly below the existing `remove()` method (~line 105):

```rust
    /// Whether a key is still tracked. Lets the restart path tell a concurrent
    /// `Down`/give-up (key gone) apart from an adopted survivor with no launch
    /// spec (key present) when deciding what to do with a reaped child.
    pub(crate) fn contains(&self, key: &Key) -> bool {
        self.children.contains_key(key)
    }
```

- [ ] **Step 4: Rewrite the supervision tick to be table-authoritative**

In `src/bin/devkitd/main.rs`, replace this block (the comment plus the `let dead = ...` through the closing brace of the `if !dead.is_empty()`):

```rust
                // Reap exited children; restart only those whose ports.json row survives
                // (the cross-tool stop signal). Debounce the read so a concurrent legacy
                // `down` that removes the row just before the exit isn't misread as a crash.
                // Use a raw read (no liveness prune) so a row with a now-dead pid is still
                // visible here — it's the daemon's signal that the exit was a crash, not an
                // intentional stop. `snapshot()` prunes dead-pid rows before we can see them.
                let dead = d.sup.lock().unwrap().reap_once();
                if !dead.is_empty() {
                    std::thread::sleep(Duration::from_millis(200)); // debounce
                    let snap = d.ports.lock().unwrap().clone();
                    for key in dead {
                        let row = snap.entries.values().find(|e| {
                            e.holder == key.holder && e.app == key.app && e.role == key.role
                        });
                        match row {
                            Some(_) => restart(&d, &key),
                            None => {
                                d.sup.lock().unwrap().remove(&key);
                            } // intentional stop
                        }
                    }
                }
```

with:

```rust
                // The supervisor table is the authority on crash vs. stop. An intentional
                // `Down` removes the key from the table before stopping the child, so a
                // stopped server never surfaces from `reap_once`; anything reaped exited on
                // its own and is a crash. `restart` enforces the crash-loop budget and drops
                // adopted survivors that have no launch spec. The bound `let` releases the
                // `sup` lock before the loop, so `restart` (which re-locks `sup`) cannot
                // deadlock.
                let dead = d.sup.lock().unwrap().reap_once();
                for key in dead {
                    restart(&d, &key);
                }
```

- [ ] **Step 5: Split `restart()` so a concurrently-removed key is handled quietly**

In `src/bin/devkitd/main.rs`, replace the `restart()` function (~lines 345-374) with:

```rust
/// Respawn a crashed child if its crash-loop budget allows; otherwise drop it and log.
fn restart(daemon: &Arc<Daemon>, key: &supervisor::Key) {
    let mut sup = daemon.sup.lock().unwrap();
    // A `Down` (or a give-up) can remove the key between the reap and here. The
    // child is already gone and untracked, so there is nothing to restart — return
    // without logging a spurious drop.
    if !sup.contains(key) {
        return;
    }
    // An adopted survivor has no stored launch spec, so it can't be respawned —
    // drop it on exit rather than charging the crash-loop budget for a spawn that
    // can never happen.
    if sup.launch_of(key).is_none() {
        sup.remove(key);
        drop(sup);
        log_line(&format!(
            "dropping {}/{} ({:?}) — no launch spec to respawn",
            key.holder, key.app, key.role
        ));
        return;
    }
    if !sup.may_restart(&key.holder, &key.app, key.role) {
        sup.remove(key);
        drop(sup);
        log_line(&format!(
            "giving up on {}/{} ({:?}) — crash-loop budget exhausted",
            key.holder, key.app, key.role
        ));
        return;
    }
    drop(sup);
    log_line(&format!(
        "restart: {}/{} ({:?})",
        key.holder, key.app, key.role
    ));
    daemon.respawn(key);
}
```

- [ ] **Step 6: Mark the `down()` remove-before-stop ordering load-bearing**

In `src/bin/devkitd/server.rs`, in `down()`, add a comment immediately above the `let mut sup = daemon.sup.lock().unwrap();` that precedes the `for k in &keys` stop loop (~line 170):

```rust
    // Remove each key from the supervisor table BEFORE signalling its child. The
    // supervision thread restarts anything it reaps; removing the key first is what
    // marks this exit intentional so the child is not respawned. This ordering is
    // load-bearing — do not signal before removing.
    let mut sup = daemon.sup.lock().unwrap();
```

- [ ] **Step 7: Run the regression test and watch it pass**

Run: `cargo test --test supervision restart_survives_concurrent_snapshot -- --nocapture`
Expected: PASS. The restart decision no longer reads the registry row, so concurrent pruning cannot suppress it; the pid changes within the deadline.

- [ ] **Step 8: Rewrite the stale comments on the two existing tests**

In `tests/supervision.rs`, replace the doc comment on `restart_after_kill` (currently mentions "debounces (200 ms), sees the row in ports.json"):

```rust
/// After SIGKILLing the supervised child, the daemon's supervision thread reaps
/// the exit and respawns the server, because the child is still tracked in the
/// supervisor table (it was not stopped via `Down`). The pid in ports.json changes.
```

and inside `restart_after_kill`, replace the poll comment that mentions the debounce:

```rust
    // Poll ports.json for up to 8 s until the pid changes (daemon restarted it).
    // Supervision tick is 500 ms + python startup ≈ 1–2 s total.
```

and replace the doc comment on `down_does_not_restart`:

```rust
/// After a clean `Down`, the supervision thread must NOT restart the server —
/// `Down` removes the key from the supervisor table before stopping the child,
/// so the exit is never reaped as a crash.
```

- [ ] **Step 9: Run the full gate**

Run, in order:
- `cargo fmt --all`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`

Expected: all green; `supervision` suite passes including the new test and the two existing ones (`restart_after_kill`, `down_does_not_restart`); zero clippy warnings; fmt clean.

- [ ] **Step 10: Commit**

```bash
git add src/bin/devkitd/main.rs src/bin/devkitd/supervisor.rs src/bin/devkitd/server.rs tests/supervision.rs
git commit -m "$(cat <<'EOF'
fix(devkitd): make supervisor table authoritative for restarts

The supervision tick classified a reaped child as crash vs. stop by
re-reading the in-memory registry row after a debounce. A concurrent
client Snapshot prunes the dead-pid row in that window, so a genuine
crash looked like an intentional stop and the server was not restarted.

Decide from the supervisor table instead: a reaped key is a crash, since
Down removes the key before stopping the child. Drop the debounce and the
row re-read; split restart() so a key removed concurrently by Down returns
quietly rather than logging a spurious drop.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Record the resolution and the new invariant in docs

Mark the race resolved where it was tracked, and capture the restart-authority rule as a daemon invariant so a future edit doesn't reintroduce the row-read.

**Files:**
- Modify: `docs/next-features.md` — the "Crash-restart vs. external registry prune (race)" entry
- Modify: `AGENTS.md` — add an invariant under "Invariants (do not break)"

**Interfaces:**
- Consumes: the behavior and accessor from Task 1 (`Supervisor::contains`; reaped key = crash; `Down` removes-before-signal).
- Produces: nothing code-facing (documentation only).

- [ ] **Step 1: Mark the race resolved in `docs/next-features.md`**

Find the section beginning `## Crash-restart vs. external registry prune (race)`. Replace its `**Status:**` line:

```markdown
**Status:** known limitation — daemon supervision.
```

with:

```markdown
**Status:** RESOLVED 2026-06-21 — see
`docs/superpowers/specs/2026-06-21-daemon-authoritative-liveness-design.md`. The
daemon now decides crash vs. stop from its supervisor table, not the registry row,
so a concurrent prune can no longer suppress a restart. The analysis below is kept
for context.
```

Leave the rest of the section (the race description and the "Why deferred" analysis) unchanged.

- [ ] **Step 2: Add the restart-authority invariant to `AGENTS.md`**

In `AGENTS.md`, under `## Invariants (do not break)`, add this bullet immediately after the `**devrun down** stops then releases…` bullet (~line 53):

```markdown
- **The supervisor table — not the registry row — decides crash vs. stop.** A child the
  `devkitd` supervision thread reaps is a crash and is restarted (within the crash-loop
  budget); an intentional `Down` removes the key from the table *before* signalling the
  child, so a stopped server is never reaped as a crash. Don't make the restart decision
  read `ports.json`/`d.ports` — a concurrent prune would race it.
```

- [ ] **Step 3: Verify the docs build/read cleanly**

Run: `rg -n "debounce|cross-tool stop signal" docs/next-features.md AGENTS.md`
Expected: no matches in `AGENTS.md`; in `docs/next-features.md` only matches inside the preserved historical analysis (not in any new text). Confirm the new invariant bullet and the RESOLVED status line read correctly.

- [ ] **Step 4: Commit**

```bash
git add docs/next-features.md AGENTS.md
git commit -m "$(cat <<'EOF'
docs: record daemon-authoritative restart invariant

Mark the crash-restart vs. registry-prune race resolved in next-features
and state the supervisor-table-is-authority rule as a daemon invariant so
the restart decision is not wired back to the registry row.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Notes for the executor

- **Lock-deadlock hazard (Step 4).** Keep the `let dead = d.sup.lock().unwrap().reap_once();` as a bound statement. Do NOT inline it as `for key in d.sup.lock().unwrap().reap_once()` — a temporary `MutexGuard` in a `for`-head lives until the loop ends, and `restart()` re-locks `sup`, which would deadlock (std `Mutex` is not reentrant).
- **Why no row protection.** The fix deliberately removes the row consultation rather than guarding supervised rows from pruning; the prune still happens and `record_pid` re-establishes the row on respawn. This is the spec's chosen approach (B); do not add a `supervised` schema marker or a protected-port set.
- **RED reliability (Step 2).** The hammer connects-snapshots-receives in a tight loop; over the 200 ms debounce window it lands many snapshots, so the pre-fix failure is reliable, not flaky. If the test unexpectedly passes before Step 3, confirm the hammer thread actually started and that `Snapshot` is reaching the daemon (the daemon prunes dead-pid rows in `snapshot_with`).
- **Platform.** `tests/supervision.rs` is `#![cfg(unix)]`; the new test uses `python3` and `nix` signals like its neighbors. Windows supervision coverage is separate and unaffected.
- **Verification host (important).** Because the whole supervision file is `#![cfg(unix)]`, on a Windows dev host the new test is *compiled out* — `cargo test --workspace` there neither type-checks nor runs it, so it cannot show RED (Step 2) or GREEN (Step 7), and a typo in the test would silently not surface. The RED→GREEN evidence for Steps 2 and 7 MUST come from a Unix host (WSL, a Linux/macOS box, or CI). On Windows, the implementer still runs Step 9's `cargo fmt`/`clippy`/`cargo test --workspace` to prove the daemon code compiles clean and the cross-platform suite passes, and explicitly reports that the Unix-gated supervision proof was deferred to a Unix run. The controller must ensure that Unix run happens (locally or via CI) and treat its result as the task's test evidence — do not mark Task 1 verified on a Windows-only green.
- **The code change itself is platform-agnostic.** `main.rs`/`supervisor.rs`/`server.rs` compile and are type-checked on Windows; only the integration *proof* is Unix-gated.
```
