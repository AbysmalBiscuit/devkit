# devrun cross-worktree `down` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a human at a terminal stop dev servers in other worktrees via `devrun down` (fuzzy positional or column filters, gated by an interactive confirmation), while an agent stays confined to its own worktree.

**Architecture:** A pure selection model in `devkit-ports` resolves CLI args (scope + filter) against an ungated registry snapshot into a set of ports. The `devrun` CLI gates any selection that touches a foreign holder behind a TTY confirmation (per-worktree, or batched under `--all`/`--batch`); a non-interactive caller is refused. Execution stops + releases the resolved ports via a new `run::bring_down_ports`, which prefers a running `devkitd` through a new `DownPorts { ports }` proto request and otherwise stops + releases directly under one flock. The MCP `devrun.down` handler is unchanged.

**Tech Stack:** Rust (edition 2024), `clap`/`clap_complete`, `anyhow`, `serde`, `flock`-guarded JSON registry, `interprocess` unix-socket daemon.

**Spec:** `docs/superpowers/specs/2026-06-24-devrun-cross-worktree-down-design.md`

**Conventions for every task:** TDD (failing test first). Run `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` before each commit; both must be green. Commit messages follow Conventional Commits. Do all work in a worktree, not the primary clone (`git worktree add ../devkit-worktrees/cross-worktree-down -b feat/cross-worktree-down main`).

---

## File Structure

- **Modify** `crates/devkit-ports/src/registry.rs` — add `release_ports`/`release_ports_with`; add the selection model (`Scope`, `Filter`, `ColumnFilter`, `DownSelector`, `select`).
- **Modify** `crates/devkit-ports/src/daemon/proto.rs` — add `Request::DownPorts`; bump `PROTO`.
- **Modify** `crates/devkit-ports/src/run.rs` — add `bring_down_ports`.
- **Modify** `src/bin/devkitd/server.rs` — dispatch `DownPorts` to a new `down_ports` handler.
- **Modify** `src/bin/devrun/main.rs` — new `down` CLI args, `DownArgs`, `build_selector`, `parse_age`, gate + confirm, `cmd_down` rewrite.
- **Create** `tests/down_ports.rs` — daemon `DownPorts` release path.
- **Create** `tests/devrun_down_gate.rs` — CLI cross-worktree refusal without a TTY.
- **Modify** `README.md`, `AGENTS.md` — document the new `down` surface and the gate invariant.

---

## Task 1: `release_ports` on the registry

Stop/release operates on an explicit port set; the registry only releases by holder+role today.

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs` (`impl Data` near line 372; facade near line 533)

- [ ] **Step 1: Write the failing test**

Add to the `ops_tests` module in `crates/devkit-ports/src/registry.rs` (near the other `release_*` unit tests):

```rust
#[test]
fn release_ports_removes_only_listed_present_ports() {
    let mut d = Data::default();
    d.alloc_one("/w", "api", 9100, Role::Issue);
    d.alloc_one("/w", "web", 9200, Role::Issue);
    // Release one present port and one absent port.
    let freed = d.release_ports(&[9100, 9999]);
    assert_eq!(freed, vec![9100], "only the present listed port is freed");
    assert!(d.entries.contains_key(&9200), "unlisted ports stay");
    assert!(!d.entries.contains_key(&9100));
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-ports release_ports_removes_only_listed_present_ports`
Expected: FAIL — `no method named release_ports`.

- [ ] **Step 3: Implement `release_ports` + `release_ports_with`**

Add to `impl Data` (right after the existing `release` method, ~line 383):

```rust
    /// Release exactly the listed ports that are still present. Returns freed ports.
    pub fn release_ports(&mut self, ports: &[u16]) -> Vec<u16> {
        let freed: Vec<u16> = ports
            .iter()
            .copied()
            .filter(|p| self.entries.contains_key(p))
            .collect();
        for p in &freed {
            self.entries.remove(p);
        }
        freed
    }
```

Add the store facade right after `release_with` (~line 535):

```rust
pub fn release_ports_with(store: &impl Store, ports: &[u16]) -> Result<Vec<u16>> {
    store.commit(|d| Ok(d.release_ports(ports)))
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p devkit-ports release_ports_removes_only_listed_present_ports`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "feat(registry): release ports by explicit set"
```

---

## Task 2: Selection model (`Scope`, `Filter`, `ColumnFilter`, `DownSelector`, `select`)

Pure resolution of a selector against a registry snapshot. Lives in the library so it is unit-testable and shared.

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs` (add types + `select` after `status_table`, ~line 650; add a unit-test module)

- [ ] **Step 1: Write the failing tests**

Add a new test module at the end of `crates/devkit-ports/src/registry.rs`:

```rust
#[cfg(test)]
mod select_tests {
    use super::*;

    fn entry(app: &str, holder: &str, role: Role, pid: Option<u32>, ts: u64) -> Entry {
        Entry {
            app: app.into(),
            holder: holder.into(),
            role,
            pid,
            logfile: None,
            ts,
        }
    }

    fn data() -> Data {
        let mut d = Data::default();
        // current worktree
        d.entries.insert(9100, entry("api", "/wt/feat-a", Role::Issue, Some(11), 1000));
        d.entries.insert(9200, entry("web", "/wt/feat-a", Role::Issue, Some(12), 1000));
        // another worktree
        d.entries.insert(9300, entry("api", "/wt/feat-b", Role::Baseline, None, 100));
        d
    }

    fn sorted(mut v: Vec<u16>) -> Vec<u16> {
        v.sort_unstable();
        v
    }

    #[test]
    fn scope_current_keeps_only_that_holder() {
        let sel = DownSelector {
            scope: Scope::Current("/wt/feat-a".into()),
            filter: Filter::All,
        };
        assert_eq!(sorted(select(&data(), &sel, 2000)), vec![9100, 9200]);
    }

    #[test]
    fn scope_others_excludes_current() {
        let sel = DownSelector {
            scope: Scope::Others("/wt/feat-a".into()),
            filter: Filter::All,
        };
        assert_eq!(sorted(select(&data(), &sel, 2000)), vec![9300]);
    }

    #[test]
    fn scope_all_keeps_every_holder() {
        let sel = DownSelector { scope: Scope::All, filter: Filter::All };
        assert_eq!(sorted(select(&data(), &sel, 2000)), vec![9100, 9200, 9300]);
    }

    #[test]
    fn scope_holders_keeps_listed() {
        let sel = DownSelector {
            scope: Scope::Holders(vec!["/wt/feat-b".into()]),
            filter: Filter::All,
        };
        assert_eq!(sorted(select(&data(), &sel, 2000)), vec![9300]);
    }

    #[test]
    fn token_matches_across_columns_and_ors() {
        // "api" matches app on 9100 and 9300; "9200" matches port. Scope All.
        let sel = DownSelector {
            scope: Scope::All,
            filter: Filter::Tokens(vec!["api".into(), "9200".into()]),
        };
        assert_eq!(sorted(select(&data(), &sel, 2000)), vec![9100, 9200, 9300]);
    }

    #[test]
    fn token_matches_holder_leaf_and_role_and_pid() {
        let d = data();
        let by_leaf = DownSelector { scope: Scope::All, filter: Filter::Tokens(vec!["feat-b".into()]) };
        assert_eq!(sorted(select(&d, &by_leaf, 2000)), vec![9300]);
        let by_role = DownSelector { scope: Scope::All, filter: Filter::Tokens(vec!["baseline".into()]) };
        assert_eq!(sorted(select(&d, &by_role, 2000)), vec![9300]);
        let by_pid = DownSelector { scope: Scope::All, filter: Filter::Tokens(vec!["11".into()]) };
        assert_eq!(sorted(select(&d, &by_pid, 2000)), vec![9100]);
    }

    #[test]
    fn columns_and_combine() {
        let sel = DownSelector {
            scope: Scope::All,
            filter: Filter::Columns(ColumnFilter {
                app: vec!["api".into()],
                role: Some(Role::Issue),
                ..Default::default()
            }),
        };
        assert_eq!(sorted(select(&data(), &sel, 2000)), vec![9100]);
    }

    #[test]
    fn older_than_filters_on_ts() {
        // now=2000; 9300 ts=100 (age 1900), 9100/9200 ts=1000 (age 1000).
        let sel = DownSelector {
            scope: Scope::All,
            filter: Filter::Columns(ColumnFilter {
                older_than_secs: Some(1500),
                ..Default::default()
            }),
        };
        assert_eq!(sorted(select(&data(), &sel, 2000)), vec![9300]);
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test -p devkit-ports select_tests`
Expected: FAIL — `cannot find type DownSelector`, `Scope`, `Filter`, `ColumnFilter`, function `select`.

- [ ] **Step 3: Implement the selection model**

Add after `status_table` (the function ends ~line 650) in `crates/devkit-ports/src/registry.rs`:

```rust
/// Which holders a `down` selection considers.
#[derive(Debug, Clone)]
pub enum Scope {
    /// Only this holder's rows (the default — current worktree).
    Current(String),
    /// Every holder.
    All,
    /// Every holder except this one.
    Others(String),
    /// Exactly these holders.
    Holders(Vec<String>),
}

impl Scope {
    fn includes(&self, holder: &str) -> bool {
        match self {
            Scope::Current(h) => holder == h,
            Scope::All => true,
            Scope::Others(h) => holder != h,
            Scope::Holders(hs) => hs.iter().any(|h| h == holder),
        }
    }
}

/// AND-combined column predicates. Empty `Vec`s / `None`s match anything.
#[derive(Debug, Clone, Default)]
pub struct ColumnFilter {
    pub app: Vec<String>,
    pub port: Vec<u16>,
    pub role: Option<Role>,
    pub pid: Option<u32>,
    pub listening: Option<bool>,
    pub older_than_secs: Option<u64>,
}

impl ColumnFilter {
    fn matches(&self, port: u16, e: &Entry, now: u64) -> bool {
        if !self.app.is_empty() && !self.app.iter().any(|a| a == &e.app) {
            return false;
        }
        if !self.port.is_empty() && !self.port.contains(&port) {
            return false;
        }
        if let Some(r) = self.role
            && e.role != r
        {
            return false;
        }
        if let Some(pid) = self.pid
            && e.pid != Some(pid)
        {
            return false;
        }
        if let Some(want) = self.listening
            && listening(port) != want
        {
            return false;
        }
        if let Some(secs) = self.older_than_secs
            && now.saturating_sub(e.ts) < secs
        {
            return false;
        }
        true
    }
}

/// How to narrow the in-scope rows.
#[derive(Debug, Clone)]
pub enum Filter {
    /// Every in-scope row.
    All,
    /// Case-insensitive substring tokens, OR'd across columns.
    Tokens(Vec<String>),
    /// AND-combined column predicates.
    Columns(ColumnFilter),
}

/// Does any identity column of this row contain `token` (already lowercased)?
fn row_contains(port: u16, e: &Entry, token: &str) -> bool {
    let leaf = devkit_common::paths::leaf(&e.holder).unwrap_or(&e.holder);
    leaf.to_lowercase().contains(token)
        || e.holder.to_lowercase().contains(token)
        || e.app.to_lowercase().contains(token)
        || port.to_string().contains(token)
        || e.role.as_str().contains(token)
        || e.pid.is_some_and(|p| p.to_string().contains(token))
}

impl Filter {
    fn matches(&self, port: u16, e: &Entry, now: u64) -> bool {
        match self {
            Filter::All => true,
            Filter::Tokens(toks) => toks
                .iter()
                .any(|t| row_contains(port, e, &t.to_lowercase())),
            Filter::Columns(c) => c.matches(port, e, now),
        }
    }
}

/// A resolved `down` selection: which holders, narrowed by which filter.
#[derive(Debug, Clone)]
pub struct DownSelector {
    pub scope: Scope,
    pub filter: Filter,
}

/// Resolve a selector against a snapshot to the matching ports. Pure except for
/// `listening()` syscalls when the `--listening` predicate is set; callers pass an
/// already-pruned `snapshot()` and the current `now()`.
pub fn select(data: &Data, sel: &DownSelector, now: u64) -> Vec<u16> {
    data.entries
        .iter()
        .filter(|(_, e)| sel.scope.includes(&e.holder))
        .filter(|(port, e)| sel.filter.matches(**port, e, now))
        .map(|(port, _)| *port)
        .collect()
}
```

- [ ] **Step 4: Run them to verify they pass**

Run: `cargo test -p devkit-ports select_tests`
Expected: PASS (8 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "feat(registry): add down selection model"
```

---

## Task 3: `DownPorts` proto request

The daemon is authoritative when running; a precise port-set stop needs its own request.

**Files:**
- Modify: `crates/devkit-ports/src/daemon/proto.rs` (`PROTO` ~line 7; `Request` enum ~line 46; tests ~line 78)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-ports/src/daemon/proto.rs`:

```rust
    #[test]
    fn down_ports_frame_roundtrips() {
        let msg = Request::DownPorts { ports: vec![9100, 9200] };
        let mut buf: Vec<u8> = Vec::new();
        send(&mut buf, &msg).unwrap();
        let mut rdr = std::io::BufReader::new(&buf[..]);
        let back: Request = recv(&mut rdr).unwrap().expect("one frame");
        match back {
            Request::DownPorts { ports } => assert_eq!(ports, vec![9100, 9200]),
            _ => panic!("wrong variant"),
        }
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-ports --lib down_ports_frame_roundtrips`
Expected: FAIL — `no variant named DownPorts`.

- [ ] **Step 3: Add the variant and bump `PROTO`**

In `crates/devkit-ports/src/daemon/proto.rs`, change the version:

```rust
/// Wire-format version. Bump on any incompatible change to these types.
pub const PROTO: u32 = 2;
```

Add the variant to `Request`, right after the existing `Down { holder, role }` arm:

```rust
    /// Stop + release exactly these ports (precise cross-worktree down). The daemon
    /// resolves each port to its supervised key and stops it intentionally.
    DownPorts {
        ports: Vec<u16>,
    },
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p devkit-ports --lib down_ports_frame_roundtrips`
Expected: PASS.

- [ ] **Step 5: Do NOT commit yet**

Adding the variant makes `devkitd`'s exhaustive `Request` match in `src/bin/devkitd/server.rs` non-exhaustive, so `cargo build --workspace` is now broken until Task 4 adds the dispatch arm. **Tasks 3 and 4 share one commit** (landed at the end of Task 4). Do not run `cargo test --workspace` between them.

---

## Task 4: daemon `down_ports` handler

Mirror the existing `down` handler's intentional-stop ordering, but scoped to a port set.

**Files:**
- Modify: `src/bin/devkitd/server.rs` (dispatch ~line 79; new fn after `down` ~line 194)
- Create: `tests/down_ports.rs`

- [ ] **Step 1: Write the failing integration test**

Create `tests/down_ports.rs`:

```rust
//! The daemon's `DownPorts` request releases exactly the listed ports.
mod common;

use common::Harness;
use devkit_ports::daemon::proto::{Request, Response};
use devkit_ports::registry::Role;

#[test]
fn down_ports_releases_listed_reservations() {
    let mut h = Harness::start();

    // Two pidless reservations under one holder.
    let alloc = h.request(&Request::Alloc {
        holder: "/wt/x".into(),
        reqs: vec![("api".into(), 9100), ("web".into(), 9200)],
        role: Role::Issue,
    });
    let ports: Vec<u16> = match alloc {
        Response::Ports(v) => v.into_iter().map(|(_, p)| p).collect(),
        other => panic!("unexpected alloc response: {other:?}"),
    };
    assert_eq!(ports.len(), 2);

    // Down exactly one of them.
    let resp = h.request(&Request::DownPorts { ports: vec![ports[0]] });
    match resp {
        Response::Freed(freed) => assert_eq!(freed, vec![ports[0]]),
        other => panic!("unexpected DownPorts response: {other:?}"),
    }

    // The other reservation is still tracked.
    let snap = h.request(&Request::Snapshot);
    match snap {
        Response::Snapshot(d) => {
            assert!(d.entries.contains_key(&ports[1]), "unlisted port survives");
            assert!(!d.entries.contains_key(&ports[0]), "listed port released");
        }
        other => panic!("unexpected snapshot response: {other:?}"),
    }

    h.shutdown();
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --test down_ports`
Expected: FAIL — the daemon returns `Response::Err`/unexpected because `DownPorts` is unhandled (compile of the bin succeeds; the match has no arm → it will not compile if the match is exhaustive). If the bin fails to compile with "non-exhaustive patterns", that is the expected failure.

- [ ] **Step 3: Implement the dispatch arm and handler**

In `src/bin/devkitd/server.rs`, add a dispatch arm right after the `Request::Down` arm (~line 79):

```rust
        Request::DownPorts { ports } => (down_ports(daemon, ports), false),
```

Add `down_ports` right after the `down` function (~line 194):

```rust
/// Like `down`, but for an explicit port set: resolve each port to its supervised
/// key, remove it from the table before signalling (so the supervision thread does
/// not restart it), then release exactly those ports.
fn down_ports(daemon: &Arc<Daemon>, ports: Vec<u16>) -> Response {
    use std::collections::BTreeSet;
    let want: BTreeSet<u16> = ports.iter().copied().collect();
    let keys: Vec<Key> = registry::snapshot_with(&daemon.port_store())
        .map(|d| {
            d.entries
                .iter()
                .filter(|(p, _)| want.contains(p))
                .map(|(_, e)| Key {
                    holder: e.holder.clone(),
                    app: e.app.clone(),
                    role: e.role,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut sup = daemon.sup.lock().unwrap();
    for k in &keys {
        if let Some(pid) = sup.remove(k) {
            supervise::stop(pid);
        }
    }
    drop(sup);
    for k in &keys {
        crate::cgroup::remove_leaf(daemon, k);
    }
    match registry::release_ports_with(&daemon.port_store(), &ports) {
        Ok(freed) => Response::Freed(freed),
        Err(e) => Response::Err(format!("{e:#}")),
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --test down_ports`
Expected: PASS.

- [ ] **Step 5: Full gate + commit (Tasks 3 + 4 together)**

Run: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, no warnings (the workspace compiles again now that the dispatch arm exists).

```bash
git add crates/devkit-ports/src/daemon/proto.rs src/bin/devkitd/server.rs tests/down_ports.rs
git commit -m "feat(devkitd): add DownPorts request and handler"
```

---

## Task 5: `run::bring_down_ports` facade

The shared stop-by-ports path: daemon-preferred, direct flock fallback.

**Files:**
- Modify: `crates/devkit-ports/src/run.rs` (add after `bring_down` ~line 306; test in the `tests` module)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-ports/src/run.rs` (near `bring_down_releases_a_pidless_reservation`):

```rust
    #[test]
    fn bring_down_ports_releases_listed_reservations() {
        let holder = format!("/down-ports-test-{}", std::process::id());
        let got = registry::alloc(
            &holder,
            &[("api".to_string(), 7300), ("web".to_string(), 7400)],
            Role::Issue,
        )
        .unwrap();
        let ports: Vec<u16> = got.into_iter().map(|(_, p)| p).collect();

        // Down just the first port.
        let out = bring_down_ports(&[ports[0]]).unwrap();
        assert_eq!(out.stopped, 0, "pidless reservation, nothing to SIGTERM");
        assert_eq!(out.freed, vec![ports[0]]);

        // The second is still reserved; clean it up.
        let rest = registry::release(&holder, None).unwrap();
        assert_eq!(rest, vec![ports[1]]);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-ports bring_down_ports_releases_listed_reservations`
Expected: FAIL — `cannot find function bring_down_ports`.

- [ ] **Step 3: Implement `bring_down_ports`**

Add to `crates/devkit-ports/src/run.rs` right after `bring_down` (~line 306):

```rust
/// Stop + release exactly the listed ports. Prefers a running daemon (precise
/// `DownPorts`); otherwise SIGTERMs each port's pid and removes its row under one
/// lock, without pruning first (the still-running-but-stale invariant).
pub fn bring_down_ports(ports: &[u16]) -> Result<DownOutcome> {
    #[cfg(feature = "daemon")]
    if let Some(mut client) = crate::daemon::client::try_existing() {
        let resp = client.request(&crate::daemon::proto::Request::DownPorts {
            ports: ports.to_vec(),
        })?;
        if let crate::daemon::proto::Response::Freed(freed) = resp {
            return Ok(DownOutcome {
                stopped: freed.len(),
                freed,
                via_daemon: true,
            });
        }
    }
    let want: std::collections::BTreeSet<u16> = ports.iter().copied().collect();
    let mut stopped = 0;
    let freed = registry::with_lock(|d| {
        for (port, e) in d.entries.iter() {
            if want.contains(port)
                && let Some(pid) = e.pid
            {
                supervise::stop(pid);
                stopped += 1;
            }
        }
        Ok(d.release_ports(ports))
    })?;
    Ok(DownOutcome {
        stopped,
        freed,
        via_daemon: false,
    })
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p devkit-ports bring_down_ports_releases_listed_reservations`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/run.rs
git commit -m "feat(run): add bring_down_ports facade"
```

---

## Task 6: `parse_age` duration helper (devrun)

`--older-than` accepts `90s`, `30m`, `2h`, `1d`.

**Files:**
- Modify: `src/bin/devrun/main.rs` (add fn near `parse_user_env` ~line 145; add to `tests` module ~line 444)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module at the bottom of `src/bin/devrun/main.rs`:

```rust
    #[test]
    fn parse_age_handles_units() {
        use super::parse_age;
        assert_eq!(parse_age("90s").unwrap(), 90);
        assert_eq!(parse_age("30m").unwrap(), 1800);
        assert_eq!(parse_age("2h").unwrap(), 7200);
        assert_eq!(parse_age("1d").unwrap(), 86400);
        assert_eq!(parse_age("45").unwrap(), 45, "bare number is seconds");
        assert!(parse_age("nope").is_err());
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --bin devrun parse_age_handles_units`
Expected: FAIL — `cannot find function parse_age`.

- [ ] **Step 3: Implement `parse_age`**

Add near `parse_user_env` in `src/bin/devrun/main.rs`:

```rust
/// Parse an age threshold like `90s`, `30m`, `2h`, `1d` (bare number = seconds) to seconds.
fn parse_age(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix('s') {
        (n, 1u64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86_400)
    } else {
        (s, 1)
    };
    let v: u64 = num.trim().parse().with_context(|| {
        format!("invalid --older-than `{s}`: expected e.g. 90s, 30m, 2h, 1d")
    })?;
    Ok(v * mult)
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --bin devrun parse_age_handles_units`
Expected: PASS. (The `cfg(test)` build sees `parse_age` used by its test, so no `dead_code`.)

- [ ] **Step 5: Do NOT commit yet**

`parse_age` is unused in a non-test build until Task 8 wires it in, so `cargo clippy --all-targets -- -D warnings` would fail on `dead_code` now. **Tasks 6, 7, and 8 share one commit** (landed at the end of Task 8); do not run clippy or `cargo test --workspace` between them.

---

## Task 7: `down` CLI args + `build_selector`

Replace the `Down { role }` variant with the full selector surface, and add a pure args→`DownSelector` builder.

**Files:**
- Modify: `src/bin/devrun/main.rs` (`Cmd::Down` ~line 44; `main` dispatch ~line 229; add `DownArgs`/`build_selector`; `tests` module)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/bin/devrun/main.rs`:

```rust
    #[test]
    fn build_selector_maps_scope_and_filter() {
        use super::{build_selector, DownArgs};
        use devkit_ports::registry::{Filter, Scope};

        // Default: current worktree, no filter.
        let a = DownArgs::default();
        let s = build_selector(&a, "/wt/cur").unwrap();
        assert!(matches!(s.scope, Scope::Current(ref h) if h == "/wt/cur"));
        assert!(matches!(s.filter, Filter::All));

        // --all + positional token.
        let a = DownArgs { all: true, selectors: vec!["api".into()], ..Default::default() };
        let s = build_selector(&a, "/wt/cur").unwrap();
        assert!(matches!(s.scope, Scope::All));
        assert!(matches!(s.filter, Filter::Tokens(ref t) if t == &vec!["api".to_string()]));

        // --others + column filter.
        let a = DownArgs { others: true, app: vec!["web".into()], ..Default::default() };
        let s = build_selector(&a, "/wt/cur").unwrap();
        assert!(matches!(s.scope, Scope::Others(ref h) if h == "/wt/cur"));
        match s.filter {
            Filter::Columns(c) => assert_eq!(c.app, vec!["web".to_string()]),
            _ => panic!("expected Columns filter"),
        }
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --bin devrun build_selector_maps_scope_and_filter`
Expected: FAIL — `cannot find type DownArgs` / function `build_selector`.

- [ ] **Step 3: Replace the `Down` CLI variant**

In `src/bin/devrun/main.rs`, replace the existing `Down` arm of `enum Cmd` (lines ~44-48):

```rust
    /// Stop servers and release ports. Defaults to this worktree; reaching another
    /// worktree needs --all/--others/--holder and prompts (requires a terminal).
    Down {
        /// Fuzzy selectors matched (substring) across columns. Mutually exclusive
        /// with the column filters below.
        #[arg(conflicts_with_all = ["app", "port", "role", "pid", "listening", "not_listening", "older_than"])]
        selectors: Vec<String>,
        /// Every holder, including this worktree.
        #[arg(long)]
        all: bool,
        /// Every holder except this worktree.
        #[arg(long, conflicts_with = "all")]
        others: bool,
        /// One specific worktree (repeatable), by path.
        #[arg(long = "holder", conflicts_with_all = ["all", "others"])]
        holders: Vec<String>,
        /// Collapse cross-worktree confirmation into one combined prompt.
        #[arg(long)]
        batch: bool,
        /// Filter: app name (repeatable).
        #[arg(long)]
        app: Vec<String>,
        /// Filter: port (repeatable).
        #[arg(long)]
        port: Vec<u16>,
        /// Filter: role.
        #[arg(long, value_enum)]
        role: Option<RoleSelector>,
        /// Filter: pid.
        #[arg(long)]
        pid: Option<u32>,
        /// Filter: only servers currently listening.
        #[arg(long, conflicts_with = "not_listening")]
        listening: bool,
        /// Filter: only servers not currently listening.
        #[arg(long = "not-listening")]
        not_listening: bool,
        /// Filter: only servers older than this (90s, 30m, 2h, 1d).
        #[arg(long = "older-than")]
        older_than: Option<String>,
    },
```

- [ ] **Step 4: Add `DownArgs` + `build_selector`**

Add near the other helpers in `src/bin/devrun/main.rs` (e.g. after `parse_age`):

```rust
/// CLI inputs for `down`, normalized (role already collapsed to a registry `Role`,
/// `--older-than` already parsed to seconds). Kept separate from the clap variant so
/// the selector builder is unit-testable.
#[derive(Default)]
struct DownArgs {
    selectors: Vec<String>,
    all: bool,
    others: bool,
    holders: Vec<String>,
    batch: bool,
    app: Vec<String>,
    port: Vec<u16>,
    role: Option<Role>,
    pid: Option<u32>,
    listening: bool,
    not_listening: bool,
    older_than_secs: Option<u64>,
}

/// Build the registry selector from CLI args. `--holder` paths resolve to their git
/// toplevel when possible, else are used verbatim.
fn build_selector(a: &DownArgs, current: &str) -> Result<registry::DownSelector> {
    let scope = if !a.holders.is_empty() {
        registry::Scope::Holders(
            a.holders
                .iter()
                .map(|h| toplevel(h).unwrap_or_else(|_| h.clone()))
                .collect(),
        )
    } else if a.all {
        registry::Scope::All
    } else if a.others {
        registry::Scope::Others(current.to_string())
    } else {
        registry::Scope::Current(current.to_string())
    };

    let has_columns = !a.app.is_empty()
        || !a.port.is_empty()
        || a.role.is_some()
        || a.pid.is_some()
        || a.listening
        || a.not_listening
        || a.older_than_secs.is_some();

    let filter = if !a.selectors.is_empty() {
        registry::Filter::Tokens(a.selectors.clone())
    } else if has_columns {
        let listening = if a.listening {
            Some(true)
        } else if a.not_listening {
            Some(false)
        } else {
            None
        };
        registry::Filter::Columns(registry::ColumnFilter {
            app: a.app.clone(),
            port: a.port.clone(),
            role: a.role,
            pid: a.pid,
            listening,
            older_than_secs: a.older_than_secs,
        })
    } else {
        registry::Filter::All
    };

    Ok(registry::DownSelector { scope, filter })
}
```

- [ ] **Step 5: Update the `main` dispatch to build `DownArgs`**

In `main`, replace the `Cmd::Down { role } => cmd_down(...)` arm (~line 229) with:

```rust
        Cmd::Down {
            selectors,
            all,
            others,
            holders,
            batch,
            app,
            port,
            role,
            pid,
            listening,
            not_listening,
            older_than,
        } => {
            let older_than_secs = match older_than {
                Some(s) => Some(parse_age(s)?),
                None => None,
            };
            let args = DownArgs {
                selectors: selectors.clone(),
                all: *all,
                others: *others,
                holders: holders.clone(),
                batch: *batch,
                app: app.clone(),
                port: port.clone(),
                role: role.and_then(RoleSelector::filter),
                pid: *pid,
                listening: *listening,
                not_listening: *not_listening,
                older_than_secs,
            };
            cmd_down(&cli, &cwd, &args)
        }
```

(Leave `cmd_down`'s body untouched for now — Task 8 rewrites it. To keep this task compiling, temporarily adapt the call only if the signature differs; Task 8 lands the real body. If the compiler complains about an unused `cmd` import or signature mismatch, proceed to Task 8 in the same session before running the full suite.)

- [ ] **Step 6: Do NOT run tests or commit yet**

After this step the `devrun` bin does **not** compile: the new `Cmd::Down` variant and `main` dispatch now call `cmd_down(&cli, &cwd, &args)`, but the old `cmd_down(cwd, role)` body still has the previous signature. That is expected — Task 8 lands the new `cmd_down` and the `build_selector` test runs there. Do not run `cargo test`/`cargo build` for the bin until Task 8.

---

## Task 8: gate + confirm + `cmd_down` rewrite

Resolve the selection, gate any foreign-holder match behind a TTY confirmation, then stop the chosen ports.

**Files:**
- Modify: `src/bin/devrun/main.rs` (rewrite `cmd_down` ~line 391; add helpers + `use std::io::{IsTerminal, Write}`)
- Create: `tests/devrun_down_gate.rs`

- [ ] **Step 1: Write the failing helper test**

Add to the `tests` module in `src/bin/devrun/main.rs`:

```rust
    #[test]
    fn touches_foreign_detects_other_holders() {
        use super::touches_foreign;
        use devkit_ports::registry::{Entry, Role};
        let e = |holder: &str| Entry {
            app: "api".into(),
            holder: holder.into(),
            role: Role::Issue,
            pid: None,
            logfile: None,
            ts: 0,
        };
        let cur = e("/wt/cur");
        let other = e("/wt/other");
        assert!(!touches_foreign(&[(1, &cur)], "/wt/cur"));
        assert!(touches_foreign(&[(1, &cur), (2, &other)], "/wt/cur"));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --bin devrun touches_foreign_detects_other_holders`
Expected: FAIL — `cannot find function touches_foreign`.

- [ ] **Step 3: Rewrite `cmd_down` and add helpers**

At the top of `src/bin/devrun/main.rs`, ensure the imports include `IsTerminal` and `Write`:

```rust
use std::io::{IsTerminal, Write};
```

Replace the existing `cmd_down` (lines ~391-403) with:

```rust
/// True if any matched row belongs to a holder other than `current`.
fn touches_foreign(matched: &[(u16, &registry::Entry)], current: &str) -> bool {
    matched.iter().any(|(_, e)| e.holder != current)
}

/// Render a status table limited to the given ports.
fn preview_table(data: &registry::Data, ports: &[u16]) -> String {
    let mut d = registry::Data::default();
    for p in ports {
        if let Some(e) = data.entries.get(p) {
            d.entries.insert(*p, e.clone());
        }
    }
    registry::status_table(&d, None)
}

/// Foreign holders among the matched rows, in first-seen order.
fn foreign_holders(matched: &[(u16, &registry::Entry)], current: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for (_, e) in matched {
        if e.holder != current && !seen.contains(&e.holder) {
            seen.push(e.holder.clone());
        }
    }
    seen
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn report_down(out: &run::DownOutcome) {
    if out.via_daemon {
        println!("stopped via daemon; released ports {:?}", out.freed);
    } else {
        println!(
            "stopped {} process(es); released ports {:?}",
            out.stopped, out.freed
        );
    }
}

fn cmd_down(_cli: &Cli, cwd: &str, args: &DownArgs) -> Result<()> {
    let current = toplevel(cwd)?;
    let selector = build_selector(args, &current)?;
    let data = registry::snapshot()?;
    let now = registry::now();
    let ports = registry::select(&data, &selector, now);
    if ports.is_empty() {
        println!("no tracked servers match the selection");
        return Ok(());
    }
    let matched: Vec<(u16, &registry::Entry)> = ports
        .iter()
        .filter_map(|p| data.entries.get(p).map(|e| (*p, e)))
        .collect();

    // All in the current worktree → today's behavior, no prompt.
    if !touches_foreign(&matched, &current) {
        let out = run::bring_down_ports(&ports)?;
        report_down(&out);
        return Ok(());
    }

    // Foreign holders present: require an interactive terminal.
    if !std::io::stdin().is_terminal() {
        eprintln!("{}", preview_table(&data, &ports));
        anyhow::bail!("cross-worktree down requires an interactive terminal");
    }

    let batch = args.batch || args.all;
    let mut chosen: Vec<u16> = Vec::new();
    if batch {
        println!("{}", preview_table(&data, &ports));
        let holders = foreign_holders(&matched, &current);
        // current-worktree count included in the batch total
        if confirm(&format!(
            "Stop {} server(s) across {} worktree(s)?",
            ports.len(),
            holders.len() + usize::from(matched.iter().any(|(_, e)| e.holder == current))
        )) {
            chosen = ports.clone();
        }
    } else {
        // Per-worktree prompts for foreign holders; current worktree stops silently.
        for holder in foreign_holders(&matched, &current) {
            let group: Vec<u16> = matched
                .iter()
                .filter(|(_, e)| e.holder == holder)
                .map(|(p, _)| *p)
                .collect();
            println!("{}", preview_table(&data, &group));
            let label = devkit_common::paths::leaf(&holder).unwrap_or(&holder);
            if confirm(&format!("Stop {} server(s) in {label}?", group.len())) {
                chosen.extend(group);
            } else {
                println!("    skipped");
            }
        }
        for (p, e) in &matched {
            if e.holder == current {
                chosen.push(*p);
            }
        }
    }

    if chosen.is_empty() {
        println!("nothing stopped");
        return Ok(());
    }
    let out = run::bring_down_ports(&chosen)?;
    report_down(&out);
    Ok(())
}
```

- [ ] **Step 4: Run the deferred unit tests + full build**

The bin compiles again now that `cmd_down` matches its new call site, so the Task 6/7 tests can finally run alongside this task's:

Run: `cargo test --bin devrun parse_age_handles_units build_selector_maps_scope_and_filter touches_foreign_detects_other_holders`
Expected: PASS (3 tests).
Run: `cargo build --workspace`
Expected: builds clean.

- [ ] **Step 5: Write the failing CLI gate integration test**

Create `tests/devrun_down_gate.rs`:

```rust
//! `devrun down --all` refuses to touch another worktree without a terminal.
use std::path::PathBuf;
use std::process::Command;

mod common;
use common::unique;

#[test]
fn down_all_without_tty_refuses() {
    // Throwaway state home with a seeded foreign reservation.
    let home = std::env::temp_dir().join(format!("devrun-gate-{}", unique()));
    let xdg_state = home.join("state");
    let devkit_dir = xdg_state.join("devkit");
    std::fs::create_dir_all(devkit_dir.join("logs")).unwrap();

    // A foreign holder dir that exists on disk (so snapshot() does not prune it).
    let foreign = home.join("foreign-wt");
    std::fs::create_dir_all(&foreign).unwrap();

    // A current worktree that is a real git repo (toplevel resolves), distinct from foreign.
    let cur = home.join("cur-wt");
    std::fs::create_dir_all(&cur).unwrap();
    run_git(&cur, &["init", "-q"]);

    // Seed ports.json: one pidless, fresh reservation under the foreign holder.
    let now = now_secs();
    let ports_json = format!(
        r#"{{"version":1,"entries":{{"9100":{{"app":"api","holder":{holder:?},"role":"issue","pid":null,"logfile":null,"ts":{now}}}}}}}"#,
        holder = foreign.to_string_lossy(),
    );
    std::fs::write(devkit_dir.join("ports.json"), ports_json).unwrap();

    let bin = env!("CARGO_BIN_EXE_devrun");
    let out = Command::new(bin)
        .arg("-C")
        .arg(&cur)
        .args(["down", "--all"])
        .env("HOME", &home)
        .env("XDG_STATE_HOME", &xdg_state)
        // No daemon; force the direct path.
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run devrun down --all");

    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires an interactive terminal"),
        "stderr was: {stderr}"
    );

    // The reservation must be untouched.
    let after = std::fs::read_to_string(devkit_dir.join("ports.json")).unwrap();
    assert!(after.contains("9100"), "reservation must survive a refusal");

    let _ = std::fs::remove_dir_all(&home);
}

fn run_git(dir: &PathBuf, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}
```

- [ ] **Step 6: Run the gate test to verify it passes**

Run: `cargo test --test devrun_down_gate`
Expected: PASS (refuses, non-zero, reservation survives).

- [ ] **Step 7: Full gate + commit**

Run: `cargo test --workspace`
Expected: PASS.
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

This single commit lands Tasks 6, 7, and 8 (the devrun CLI is only compilable and warning-free as a unit):

```bash
git add src/bin/devrun/main.rs tests/devrun_down_gate.rs
git commit -m "feat(devrun): cross-worktree down with tty-gated confirmation"
```

---

## Task 9: docs + manual verification

**Files:**
- Modify: `README.md` (devrun `down` section), `AGENTS.md` (invariants)

- [ ] **Step 1: Document the new `down` surface in `README.md`**

Find the `devrun down` documentation in `README.md` (search for `devrun down`) and replace its description with the cross-worktree usage. Include:

```markdown
### Stopping servers — `devrun down`

By default `devrun down` stops every server in the **current worktree**. Reaching
another worktree requires an explicit, denyable scope flag and a confirmation read
from a terminal:

| Command | Effect |
|---|---|
| `devrun down` | stop all servers in this worktree |
| `devrun down --role baseline` | this worktree, baseline only |
| `devrun down api` | this worktree, fuzzy-match `api` across columns |
| `devrun down --all` | every server, every worktree (one batch prompt) |
| `devrun down --others` | every server in every *other* worktree |
| `devrun down --others api` | `api` in other worktrees (per-worktree prompts) |
| `devrun down --holder ../wt/feat-x` | one specific worktree |
| `devrun down --all --app api --older-than 1h` | precise filter, all worktrees |

A positional selector substring-matches across `HOLDER`/`APP`/`PORT`/`ROLE`/`PID`
and is mutually exclusive with the column filters (`--app`, `--port`, `--role`,
`--pid`, `--listening`/`--not-listening`, `--older-than`). Any selection that
reaches outside the current worktree prints a preview and prompts; with no
interactive terminal it is refused. `--all`/`--batch` collapse the prompts into one.
```

- [ ] **Step 2: Add the gate invariant to `AGENTS.md`**

In the `## Invariants (do not break)` section of `AGENTS.md`, add:

```markdown
- **Cross-worktree `devrun down` is TTY-gated.** A selection touching a holder
  other than the current worktree is refused unless stdin is an interactive
  terminal (`cmd_down` in `src/bin/devrun/main.rs`), and is reachable only via the
  named scope flags `--all`/`--others`/`--holder` — so an agent (no PTY) cannot
  stop another worktree's servers, and a harness can deny those flags by name. The
  MCP `devrun.down` handler stays root-scoped and never gains a cross-holder arg.
```

- [ ] **Step 3: Verify completions still generate**

Run: `cargo run --bin devrun -- completions bash > /dev/null`
Expected: exit 0 (the new args generate without error).

- [ ] **Step 4: Manual verification of the interactive happy path**

The interactive confirm cannot be unit-tested. Verify by hand from a real terminal in a scratch checkout with two worktrees that each have a running server:

```sh
devrun down --others        # expect a preview + per-worktree y/N; answering n stops nothing
devrun down --all           # expect ONE combined preview + a single y/N
devrun down api             # current worktree only, no prompt
```

Confirm: answering `n` leaves servers running (`devrun status --all`), answering `y` stops them and frees the ports.

- [ ] **Step 5: Commit**

```bash
git add README.md AGENTS.md
git commit -m "docs: document cross-worktree devrun down"
```

---

## Self-Review notes (for the implementer)

- **Spec coverage:** scope flags (Task 7), substring tokens + column filters incl. `--older-than`/`--listening` (Tasks 2, 6, 7), per-worktree vs batch confirm (Task 8), TTY refuse (Task 8), `DownPorts` daemon path + PROTO bump (Tasks 3-5), MCP unchanged (no task — verified by not touching `crates/devkit-mcp`), harness denyability via named flags (Tasks 7-9).
- **Empty selection** prints a message and exits zero (Task 8) — matches the spec's error-handling section.
- **`PROTO` bump to 2** (Task 3): a stale daemon fails the handshake and is asked to shut down (existing `shake` logic) — no silent fallback behind the daemon's lock.
- **Commit groups (compile coupling):** adding the `DownPorts` proto variant breaks `devkitd`'s exhaustive match until its handler exists, so **Tasks 3+4 share one commit**. Replacing the `down` clap variant breaks `cmd_down` and leaves `parse_age`/`build_selector` momentarily unused, so **Tasks 6+7+8 share one commit**. Within each group, run only the named per-task tests; run `cargo test --workspace` + clippy and commit at the group's final task.
```
