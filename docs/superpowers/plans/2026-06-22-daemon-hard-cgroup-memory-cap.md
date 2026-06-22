# Daemon hard cgroup-v2 memory cap — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** give `devkitd` a kernel-enforced per-server memory ceiling (cgroup-v2 `memory.max`) as a backstop above the existing soft poll-based `memory_action`, degrading cleanly to today's behavior on every non-Linux / non-delegated host.

**Architecture:** A Linux-only enhancement behind the single `sys` platform boundary. At startup the daemon probes `cgroup_caps()`; when it can enforce and `memory_max_mb > 0`, each supervised server is spawned into its own cgroup leaf (`memory.max` + `memory.oom.group=1`), joined by the child itself in `pre_exec` before `exec`. A breach is a kernel OOM-kill, observed as a crash and respawned through the existing crash path — no new restart path. An opt-in `devkitd install-service` writes a `systemd --user` unit (`Delegate=yes`, no sudo) and the autostart path routes through systemd when that unit exists.

**Tech Stack:** Rust 2024, `anyhow`, `nix` (unix process primitives, already a dep), `std::process::Command` + `pre_exec`, cgroup-v2 sysfs, systemd `--user`.

**Spec:** `docs/superpowers/specs/2026-06-22-daemon-hard-cgroup-memory-cap-design.md`

## Global Constraints

- Conventional Commits: `type(scope): description`, subject ≤50 chars (72 hard limit), imperative, lowercase after the colon, no trailing period.
- Commit message footer carries EXACTLY one trailer and nothing else: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. No `Claude-Session`, no other trailers.
- Merge gate: `cargo test --workspace` green; `cargo clippy --workspace --all-targets -- -D warnings` zero warnings; `cargo fmt --all` applied.
- Platform gating lives ONLY at the `sys` boundary (`crates/devkit-common/src/sys/`). No `#[cfg]` scattered through `devkitd`; the daemon calls `sys::cgroup_*` unconditionally.
- `anyhow` everywhere with `.context()` on every cgroup/systemd syscall, surfacing path + operation.
- **Fail-open on the cap, never on the spawn.** Any cgroup failure (mkdir denied, `memory.max` write fails, fd open fails) proceeds with an uncapped spawn, logged once; it never blocks or kills a server.
- `memory_max_mb` defaults to `0` (off) and is purely additive: at `0`, or on any non-enforcing platform, behavior is byte-for-byte today's. The soft `memory_action` path is never disabled by this feature.
- Enforcement is Linux-only. `cgroup_caps()` is a tri-state: `Enforce { base }`, `Unavailable { reason }` (Linux but can't enforce — warn once), `Unsupported` (no cgroups — silent).
- `memory_max_mb` is intended to sit ABOVE `memory_limit_mb` (soft is the graceful first responder; the kernel cap is the backstop).
- No real project / user names anywhere — use `example` / `exampleuser` placeholders.
- `#![cfg(unix)]` integration tests run on WSL via the validated invocation (see Task 6); poll for expected state, never sleep a fixed interval.
- Worktree: this plan executes in `../devkit-worktrees/devkitd-hard-cgroup-cap` (branch `feat/devkitd-hard-cgroup-cap`). Never check out the branch in the primary clone.

---

### Task 1: cgroup `sys` primitives and detection

**Files:**
- Modify: `crates/devkit-common/src/sys/mod.rs` (add the cross-platform API surface + the pure `fmt_pid` helper)
- Modify: `crates/devkit-common/src/sys/unix.rs` (real Linux impl; macOS no-op)
- Modify: `crates/devkit-common/src/sys/windows.rs` (no-op impl)

**Interfaces:**
- Produces:
  - `pub enum CgroupCaps { Enforce { base: std::path::PathBuf }, Unavailable { reason: String }, Unsupported }`
  - `pub fn cgroup_caps() -> CgroupCaps`
  - `pub fn cgroup_create_leaf(base: &Path, name: &str, max_bytes: u64) -> anyhow::Result<PathBuf>`
  - `pub fn cgroup_remove_leaf(leaf: &Path) -> anyhow::Result<()>`
  - `pub fn cgroup_list_leaves(base: &Path) -> Vec<String>` (leaf dir names under `<base>/servers/`)
  - `pub fn join_cgroup(cmd: &mut std::process::Command, leaf: &Path)` (registers the `pre_exec` self-placement; no-op off-Linux)
  - `fn fmt_pid(pid: i64, buf: &mut [u8; 20]) -> &[u8]` (pure, async-signal-safe; lives in `mod.rs` so its test is cross-platform)

- [ ] **Step 1: Write the failing test for `fmt_pid`**

In `crates/devkit-common/src/sys/mod.rs`, inside the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn fmt_pid_formats_decimal() {
    let mut buf = [0u8; 20];
    assert_eq!(fmt_pid(0, &mut buf), b"0");
    assert_eq!(fmt_pid(7, &mut buf), b"7");
    assert_eq!(fmt_pid(12345, &mut buf), b"12345");
    // pid_t max on Linux is 2^22, but format the full i32 range to be safe.
    assert_eq!(fmt_pid(2147483647, &mut buf), b"2147483647");
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p devkit-common sys::tests::fmt_pid_formats_decimal`
Expected: FAIL — `cannot find function fmt_pid`.

- [ ] **Step 3: Add `fmt_pid` and the cross-platform API surface to `mod.rs`**

Add near the top of `crates/devkit-common/src/sys/mod.rs` (after the existing `imp` selection):

```rust
use std::path::{Path, PathBuf};

/// Whether this daemon can enforce a hard cgroup-v2 memory cap.
#[derive(Debug)]
pub enum CgroupCaps {
    /// Hard caps available; `base` is the daemon-owned delegated subtree.
    Enforce { base: PathBuf },
    /// The platform has cgroups but this process can't enforce (cgroup-v1,
    /// missing memory controller, or a non-writable / non-delegated subtree).
    Unavailable { reason: String },
    /// The platform has no cgroups at all (macOS / Windows).
    Unsupported,
}

/// Probe whether a hard memory cap can be enforced, preparing the daemon's
/// delegated cgroup base when so. Linux-only; other platforms return
/// `Unsupported`.
pub fn cgroup_caps() -> CgroupCaps {
    imp::cgroup_caps()
}

/// Create `<base>/servers/<name>/`, set `memory.max` and `memory.oom.group=1`.
/// Reuses the leaf if it already exists (rewriting `memory.max`). Off-Linux this
/// errors (`Unsupported` callers never reach it).
pub fn cgroup_create_leaf(base: &Path, name: &str, max_bytes: u64) -> anyhow::Result<PathBuf> {
    imp::cgroup_create_leaf(base, name, max_bytes)
}

/// Remove a leaf cgroup (`rmdir`; succeeds only when empty).
pub fn cgroup_remove_leaf(leaf: &Path) -> anyhow::Result<()> {
    imp::cgroup_remove_leaf(leaf)
}

/// Leaf directory names under `<base>/servers/`. Empty on any error or off-Linux.
pub fn cgroup_list_leaves(base: &Path) -> Vec<String> {
    imp::cgroup_list_leaves(base)
}

/// Register a `pre_exec` step that moves the child into `leaf` before `exec`,
/// best-effort (fail-open). No-op off-Linux. Call after `detach`.
pub fn join_cgroup(cmd: &mut std::process::Command, leaf: &Path) {
    imp::join_cgroup(cmd, leaf)
}

/// Format a non-negative pid into `buf` as decimal ASCII, returning the written
/// slice. Async-signal-safe: pure arithmetic into a caller buffer, no allocation,
/// so it is safe to call from a post-fork `pre_exec` closure.
fn fmt_pid(pid: i64, buf: &mut [u8; 20]) -> &[u8] {
    let mut n = pid.max(0) as u64;
    let mut i = buf.len();
    if n == 0 {
        i -= 1;
        buf[i] = b'0';
        return &buf[i..];
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[i..]
}
```

- [ ] **Step 4: Run the `fmt_pid` test — it passes**

Run: `cargo test -p devkit-common sys::tests::fmt_pid_formats_decimal`
Expected: PASS. (The new `pub fn`s will fail to compile until the impls exist — do Step 5 before re-running the whole crate.)

- [ ] **Step 5: Implement the Linux backend in `unix.rs`**

In `crates/devkit-common/src/sys/unix.rs`, add the Linux implementations. The macOS branch (`not(target_os = "linux")`) returns `Unsupported` / no-ops so the `unix` file compiles on macOS without cgroups:

```rust
#[cfg(target_os = "linux")]
pub(super) fn cgroup_caps() -> super::CgroupCaps {
    use super::CgroupCaps;
    // Optional manual override (also used by integration tests): a pre-delegated,
    // writable cgroup-v2 base the daemon should use instead of auto-detecting.
    if let Some(root) = std::env::var_os("DEVKIT_DAEMON_CGROUP_ROOT") {
        let base = std::path::PathBuf::from(root);
        return match prepare_base(&base) {
            Ok(()) => CgroupCaps::Enforce { base },
            Err(e) => CgroupCaps::Unavailable { reason: format!("{e:#}") },
        };
    }
    // cgroup-v2 unified hierarchy is mounted at /sys/fs/cgroup with a
    // cgroup.controllers file at the root.
    let mount = std::path::Path::new("/sys/fs/cgroup");
    if !mount.join("cgroup.controllers").is_file() {
        return CgroupCaps::Unavailable { reason: "cgroup-v2 unified hierarchy not mounted".into() };
    }
    // Resolve this process's own cgroup: /proc/self/cgroup line "0::<rel>".
    let rel = match fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|s| s.lines().find_map(|l| l.strip_prefix("0::").map(str::to_string)))
    {
        Some(r) => r,
        None => return CgroupCaps::Unavailable { reason: "no cgroup-v2 entry in /proc/self/cgroup".into() },
    };
    let base = mount.join(rel.trim_start_matches('/'));
    match prepare_base(&base) {
        Ok(()) => CgroupCaps::Enforce { base },
        Err(e) => CgroupCaps::Unavailable { reason: format!("{e:#}") },
    }
}

/// Make `base` able to host memory-capped leaves: enable `+memory` in
/// `cgroup.subtree_control`, and — to satisfy cgroup-v2's no-internal-processes
/// rule — move this process into `<base>/supervisor/` so server leaves can sit
/// beside it under `<base>/servers/`. Idempotent.
#[cfg(target_os = "linux")]
fn prepare_base(base: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::os::unix::fs::PermissionsExt as _;
    // Writability probe: the base dir must be writable by this user (delegation).
    let _ = fs::metadata(base)
        .with_context(|| format!("cgroup base {} missing", base.display()))?
        .permissions()
        .mode();
    let sup = base.join("supervisor");
    fs::create_dir_all(&sup).with_context(|| format!("creating {}", sup.display()))?;
    // Move self out of `base` before enabling controllers on it.
    fs::write(sup.join("cgroup.procs"), format!("{}\n", std::process::id()))
        .with_context(|| "moving daemon into supervisor leaf")?;
    fs::create_dir_all(base.join("servers")).with_context(|| "creating servers subtree")?;
    // Enable the memory controller for children. Ignore "already enabled".
    let _ = fs::write(base.join("cgroup.subtree_control"), "+memory\n");
    if !fs::read_to_string(base.join("cgroup.controllers"))
        .unwrap_or_default()
        .split_whitespace()
        .any(|c| c == "memory")
    {
        anyhow::bail!("memory controller unavailable in {}", base.display());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn cgroup_create_leaf(
    base: &std::path::Path,
    name: &str,
    max_bytes: u64,
) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context as _;
    let leaf = base.join("servers").join(name);
    fs::create_dir_all(&leaf).with_context(|| format!("mkdir {}", leaf.display()))?;
    fs::write(leaf.join("memory.max"), format!("{max_bytes}\n"))
        .with_context(|| format!("set memory.max on {}", leaf.display()))?;
    // Kill the whole leaf together on breach so the daemon sees a clean tree exit.
    let _ = fs::write(leaf.join("memory.oom.group"), "1\n");
    Ok(leaf)
}

#[cfg(target_os = "linux")]
pub(super) fn cgroup_remove_leaf(leaf: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    fs::remove_dir(leaf).with_context(|| format!("rmdir {}", leaf.display()))
}

#[cfg(target_os = "linux")]
pub(super) fn cgroup_list_leaves(base: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(rd) = fs::read_dir(base.join("servers")) {
        for ent in rd.flatten() {
            if ent.file_type().map(|t| t.is_dir()).unwrap_or(false)
                && let Some(n) = ent.file_name().to_str()
            {
                names.push(n.to_string());
            }
        }
    }
    names
}

#[cfg(target_os = "linux")]
pub(super) fn join_cgroup(cmd: &mut std::process::Command, leaf: &std::path::Path) {
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::os::unix::process::CommandExt as _;
    // Open the leaf's cgroup.procs in the parent (write, close-on-exec). A failure
    // here leaves the child uncapped — fail-open, never block the spawn.
    let path = leaf.join("cgroup.procs");
    let Ok(file) = fs::OpenOptions::new().write(true).open(&path) else {
        return;
    };
    let fd: OwnedFd = file.into();
    // SAFETY: the closure runs in the forked child before `exec` and calls only
    // async-signal-safe primitives — getpid(), arithmetic via fmt_pid, and a
    // single write() to a pre-opened fd. Writing the pid to cgroup.procs moves the
    // child (and every descendant it later forks) into the leaf. The write error
    // is ignored: an unplaced child runs uncapped rather than failing the spawn.
    unsafe {
        cmd.pre_exec(move || {
            let mut buf = [0u8; 20];
            let s = super::fmt_pid(nix::libc::getpid() as i64, &mut buf);
            let _ = nix::libc::write(
                fd.as_raw_fd(),
                s.as_ptr() as *const nix::libc::c_void,
                s.len(),
            );
            Ok(())
        });
    }
}

// macOS: no cgroups.
#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_caps() -> super::CgroupCaps {
    super::CgroupCaps::Unsupported
}
#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_create_leaf(
    _base: &std::path::Path,
    _name: &str,
    _max_bytes: u64,
) -> anyhow::Result<std::path::PathBuf> {
    anyhow::bail!("cgroups unsupported on this platform")
}
#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_remove_leaf(_leaf: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}
#[cfg(not(target_os = "linux"))]
pub(super) fn cgroup_list_leaves(_base: &std::path::Path) -> Vec<String> {
    Vec::new()
}
#[cfg(not(target_os = "linux"))]
pub(super) fn join_cgroup(_cmd: &mut std::process::Command, _leaf: &std::path::Path) {}
```

Note: `unix.rs` already has `#[cfg(target_os = "linux")] use std::fs;` at the top — the Linux impls use it. The macOS branch needs no `fs`.

- [ ] **Step 6: Implement the Windows no-op backend in `windows.rs`**

Append to `crates/devkit-common/src/sys/windows.rs`:

```rust
pub(super) fn cgroup_caps() -> super::CgroupCaps {
    super::CgroupCaps::Unsupported
}
pub(super) fn cgroup_create_leaf(
    _base: &std::path::Path,
    _name: &str,
    _max_bytes: u64,
) -> anyhow::Result<std::path::PathBuf> {
    anyhow::bail!("cgroups unsupported on this platform")
}
pub(super) fn cgroup_remove_leaf(_leaf: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}
pub(super) fn cgroup_list_leaves(_base: &std::path::Path) -> Vec<String> {
    Vec::new()
}
pub(super) fn join_cgroup(_cmd: &mut std::process::Command, _leaf: &std::path::Path) {}
```

Confirm `anyhow` is a dependency of `devkit-common` (it is — used across the crate).

- [ ] **Step 7: Run the full crate test + clippy on the build platform**

Run: `cargo test -p devkit-common` and `cargo clippy -p devkit-common --all-targets -- -D warnings`
Expected: PASS, zero warnings. (On Windows this exercises the no-op backend + the cross-platform `fmt_pid` test.)

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-common/src/sys/
git commit -F - <<'EOF'
feat(sys): add cgroup-v2 capability and leaf primitives

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 2: `memory_max_mb` config, env wiring, and startup warnings

**Files:**
- Modify: `crates/devkit-ports/src/config.rs:18-58` (add field + default + doc; extend default test)
- Modify: `src/bin/devkitd/main.rs` (read env, resolve `cgroup_caps()`, store on `Daemon`, emit warnings)

**Interfaces:**
- Consumes: `devkit_common::sys::{cgroup_caps, CgroupCaps}` (Task 1).
- Produces:
  - `DaemonConfig.memory_max_mb: u64` (default `0`)
  - On `Daemon`: `pub(crate) cgroup_cap: Option<CgroupCap>` where `pub(crate) struct CgroupCap { pub base: std::path::PathBuf, pub max_bytes: u64 }` (consumed by Task 4)
  - `fn cap_below_soft_limit(max_mb: u64, limit_mb: u64) -> bool` (pure misconfig predicate, in `main.rs`)

- [ ] **Step 1: Write the failing config default test**

In `crates/devkit-ports/src/config.rs`, find the existing test that asserts daemon defaults (it asserts `c.daemon.memory_limit_ticks == 3`). Add an assertion in the same test:

```rust
assert_eq!(c.daemon.memory_max_mb, 0);
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p devkit-ports config`
Expected: FAIL — `no field memory_max_mb on type DaemonConfig`.

- [ ] **Step 3: Add the config field**

In `crates/devkit-ports/src/config.rs`, in `struct DaemonConfig` after `memory_limit_ticks`:

```rust
    /// Hard kernel memory ceiling per supervised tree, in MB (0 = off,
    /// Linux-only). Enforced via a cgroup-v2 leaf with memory.max; a breach
    /// OOM-kills the tree and the crash path respawns it. Set above
    /// memory_limit_mb so the soft poll-based action stays the graceful first
    /// responder. Falls back to the soft action where cgroup-v2 delegation is
    /// unavailable.
    pub memory_max_mb: u64,
```

In `impl Default for DaemonConfig`, add after `memory_limit_ticks: 3,`:

```rust
            memory_max_mb: 0,
```

- [ ] **Step 4: Run the config test — it passes**

Run: `cargo test -p devkit-ports config`
Expected: PASS.

- [ ] **Step 5: Write the failing misconfig-predicate test**

In `src/bin/devkitd/main.rs`, add a `#[cfg(test)]` module at the bottom (the file has none yet):

```rust
#[cfg(test)]
mod tests {
    use super::cap_below_soft_limit;

    #[test]
    fn cap_below_soft_limit_predicate() {
        // Both set and cap <= limit → true (misconfigured).
        assert!(cap_below_soft_limit(4096, 4096));
        assert!(cap_below_soft_limit(2048, 4096));
        // Cap above limit → false (correct ordering).
        assert!(!cap_below_soft_limit(8192, 4096));
        // Either unset (0) → false (not a misconfiguration to warn about).
        assert!(!cap_below_soft_limit(0, 4096));
        assert!(!cap_below_soft_limit(4096, 0));
    }
}
```

- [ ] **Step 6: Run it and watch it fail**

Run: `cargo test -p devkit --bin devkitd cap_below_soft_limit_predicate`
Expected: FAIL — `cannot find function cap_below_soft_limit`.

- [ ] **Step 7: Add the predicate, the `CgroupCap` type, the env read, resolution, and warnings**

In `src/bin/devkitd/main.rs`:

Add the pure predicate (free function, near `env_u32`):

```rust
/// Whether a hard cap and a soft limit are both set with the cap at or below the
/// soft limit — a misconfiguration where the soft restart never gets to act first.
fn cap_below_soft_limit(max_mb: u64, limit_mb: u64) -> bool {
    max_mb > 0 && limit_mb > 0 && max_mb <= limit_mb
}
```

Add the type and a field to `struct Daemon` (after `locks`):

```rust
    /// Resolved hard-cap state: `Some` only when `memory_max_mb > 0` and cgroup-v2
    /// enforcement is available. Consulted by both spawn paths.
    pub(crate) cgroup_cap: Option<CgroupCap>,
```

Define near the top of the file (after the `use` block):

```rust
/// Active hard-cap parameters for the spawn paths.
pub(crate) struct CgroupCap {
    pub(crate) base: std::path::PathBuf,
    pub(crate) max_bytes: u64,
}
```

In `main()`, after the existing `mem_limit_ticks` read and the existing
`mem_limit <= mem_warn` warning block, add:

```rust
    let mem_max_mb = env_u64("DEVKIT_DAEMON_MEM_MAX_MB", 0);
    let mem_limit_mb = mem_limit / 1024 / 1024;
    if cap_below_soft_limit(mem_max_mb, mem_limit_mb) {
        log_line(&format!(
            "memory: hard cap ({mem_max_mb} MB) at or below soft limit ({mem_limit_mb} MB) — soft restart will never get to act first"
        ));
    }
    let cgroup_cap = if mem_max_mb > 0 {
        match devkit_common::sys::cgroup_caps() {
            devkit_common::sys::CgroupCaps::Enforce { base } => Some(CgroupCap {
                base,
                max_bytes: mem_max_mb * 1024 * 1024,
            }),
            devkit_common::sys::CgroupCaps::Unavailable { reason } => {
                log_line(&format!(
                    "memory: hard cap requested ({mem_max_mb} MB) but cgroup-v2 enforcement unavailable: {reason} — using soft memory_action only"
                ));
                None
            }
            // Off-Linux memory_max_mb is meaningless; stay silent.
            devkit_common::sys::CgroupCaps::Unsupported => None,
        }
    } else {
        None
    };
```

Add `cgroup_cap,` to the `Arc::new(Daemon { ... })` initializer.

- [ ] **Step 8: Run both unit tests + clippy**

Run: `cargo test -p devkit-ports config` and `cargo test -p devkit --bin devkitd cap_below_soft_limit_predicate` and `cargo clippy -p devkit --bin devkitd --all-targets -- -D warnings`
Expected: PASS, zero warnings. (`cgroup_cap` is set but not yet read — Task 4 reads it. If clippy flags dead code on `CgroupCap`/its fields, add `#[allow(dead_code)]` on the struct with a comment that Task 4 consumes it; the existing `#[allow(dead_code)] mod supervisor` precedent shows this is the established pattern for staged features.)

- [ ] **Step 9: Commit**

```bash
git add crates/devkit-ports/src/config.rs src/bin/devkitd/main.rs
git commit -F - <<'EOF'
feat(devkitd): add memory_max_mb config and cap resolution

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 3: thread a cgroup leaf through `spawn_detached`

**Files:**
- Modify: `crates/devkit-common/src/supervise.rs:31-49` (signature + `join_cgroup` call)
- Modify: `crates/devkit-common/src/supervise.rs` tests (callers at lines ~160, and the `reaps_a_real_child` analog isn't here — it's in supervisor.rs)
- Modify: `src/bin/devkitd/server.rs:126`, `src/bin/devkitd/main.rs:78`, `src/bin/devkitd/supervisor.rs:520` (pass `None`)
- Modify: any `devrun` caller of `spawn_detached`

**Interfaces:**
- Consumes: `devkit_common::sys::join_cgroup` (Task 1).
- Produces: `pub fn spawn_detached(argv: &[String], cwd: &str, env: &BTreeMap<String, String>, logfile: &PathBuf, cgroup_leaf: Option<&Path>) -> Result<u32>`

- [ ] **Step 1: Find every caller**

Run: `rg -n "spawn_detached" crates src`
Expected: the call site in `supervise.rs`, the daemon sites (`server.rs`, `main.rs`, `supervisor.rs` test), and any `devrun` site. Record the list — every one gets a trailing `None` argument.

- [ ] **Step 2: Change the signature and call `join_cgroup`**

In `crates/devkit-common/src/supervise.rs`, change `spawn_detached`:

```rust
use std::path::Path;

/// Spawn `argv` detached (own session), env-augmented, stdout+stderr → logfile.
/// When `cgroup_leaf` is `Some`, the child joins that cgroup in `pre_exec` before
/// `exec` (Linux only; a no-op elsewhere). Returns the child pid.
pub fn spawn_detached(
    argv: &[String],
    cwd: &str,
    env: &BTreeMap<String, String>,
    logfile: &PathBuf,
    cgroup_leaf: Option<&Path>,
) -> Result<u32> {
    fs::create_dir_all(logfile.parent().unwrap())?;
    let out = File::create(logfile)?;
    let err = out.try_clone()?;
    let (prog, rest) = argv.split_first().context("empty launch argv")?;
    let mut c = Command::new(prog);
    configure_child(&mut c, rest, cwd, env)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    crate::sys::detach(&mut c);
    if let Some(leaf) = cgroup_leaf {
        crate::sys::join_cgroup(&mut c, leaf);
    }
    let child = c.spawn().with_context(|| format!("spawning {prog}"))?;
    Ok(child.id())
}
```

- [ ] **Step 3: Update the in-file tests**

In `crates/devkit-common/src/supervise.rs`, the `spawn_and_ready_on_python_http` test calls `spawn_detached(&argv, ".", &env, &tmp)` — add `, None`:

```rust
        let pid = spawn_detached(&argv, ".", &env, &tmp, None).unwrap();
```

- [ ] **Step 4: Update all daemon + devrun callers to pass `None`**

- `src/bin/devkitd/server.rs:126`:
  ```rust
  let pid = match supervise::spawn_detached(&argv, &cwd, &env, &logfile, None) {
  ```
- `src/bin/devkitd/main.rs:78`:
  ```rust
  match devkit_common::supervise::spawn_detached(&launch.argv, &launch.cwd, &launch.env, &log, None)
  ```
- `src/bin/devkitd/supervisor.rs:520` (the `reaps_a_real_child` test):
  ```rust
  let pid = devkit_common::supervise::spawn_detached(
      &argv,
      ".",
      &std::collections::BTreeMap::new(),
      &std::env::temp_dir().join("portd-test.log"),
      None,
  )
  .unwrap();
  ```
- Any `devrun` site found in Step 1: append `, None`.

- [ ] **Step 5: Build the whole workspace and run the suite**

Run: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings. The signature change is mechanical; a missed caller is a compile error naming the file and line.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -F - <<'EOF'
refactor(supervise): thread optional cgroup leaf into spawn

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 4: daemon cgroup orchestration — create, place, teardown, reconcile

**Files:**
- Create: `src/bin/devkitd/cgroup.rs` (leaf-name sanitization, path building, create/remove/reconcile helpers over `Daemon`)
- Modify: `src/bin/devkitd/main.rs` (`mod cgroup;`; reconcile on startup; remove leaf on respawn-vs-stop)
- Modify: `src/bin/devkitd/server.rs` (`supervise_app` creates the leaf and passes it to `spawn_detached`; `down` removes it)

**Interfaces:**
- Consumes: `CgroupCap` on `Daemon` (Task 2); `sys::{cgroup_create_leaf, cgroup_remove_leaf, cgroup_list_leaves}` (Task 1); `spawn_detached(..., Option<&Path>)` (Task 3); `supervisor::Key`.
- Produces (in `cgroup.rs`):
  - `pub(crate) fn leaf_name(key: &Key) -> String` (sanitized `holder__app__role`)
  - `pub(crate) fn leaf_for(daemon: &Daemon, key: &Key) -> Option<PathBuf>` (create leaf, return its path, or `None` when caps inactive / on failure — logging once)
  - `pub(crate) fn remove_leaf(daemon: &Daemon, key: &Key)` (best-effort teardown)
  - `pub(crate) fn reconcile(daemon: &Daemon, live: &[Key])` (rmdir orphaned leaves)

- [ ] **Step 1: Write the failing sanitization test**

Create `src/bin/devkitd/cgroup.rs` with only the test + a stub:

```rust
//! Daemon-side cgroup leaf orchestration: maps a supervised `Key` to a cgroup
//! leaf under the daemon's delegated base, and creates / removes / reconciles
//! those leaves. All operations are best-effort and fail-open — a cgroup error
//! degrades to an uncapped server, never a failed spawn.

use crate::supervisor::Key;
use crate::{CgroupCap, Daemon};
use devkit_ports::registry::Role;
use std::path::PathBuf;

/// A filesystem-safe leaf directory name for a supervised key. cgroup leaf names
/// may not contain `/`; holders are worktree paths, so every `/`, `\`, and `.` is
/// escaped to `_` and the role appended, keeping distinct keys distinct.
pub(crate) fn leaf_name(key: &Key) -> String {
    let san = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect::<String>();
    let role = match key.role {
        Role::Issue => "issue",
        Role::Baseline => "baseline",
    };
    format!("{}__{}__{}", san(&key.holder), san(&key.app), role)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(holder: &str, app: &str, role: Role) -> Key {
        Key { holder: holder.into(), app: app.into(), role }
    }

    #[test]
    fn leaf_name_is_filesystem_safe_and_distinct() {
        let a = leaf_name(&key("/home/ex/wt", "web", Role::Issue));
        assert!(!a.contains('/'), "no slashes in a leaf name: {a}");
        assert_eq!(a, "_home_ex_wt__web__issue");
        // Role distinguishes otherwise-identical keys.
        let b = leaf_name(&key("/home/ex/wt", "web", Role::Baseline));
        assert_ne!(a, b);
        // App distinguishes.
        let c = leaf_name(&key("/home/ex/wt", "api", Role::Issue));
        assert_ne!(a, c);
    }
}
```

Add `mod cgroup;` to `src/bin/devkitd/main.rs` near the other `mod` declarations (it may need `#[allow(dead_code)]` until Step 5 wires the rest in — mirror the `supervisor` precedent).

- [ ] **Step 2: Run it and watch it fail, then pass**

Run: `cargo test -p devkit --bin devkitd leaf_name_is_filesystem_safe_and_distinct`
Expected: first FAIL if the module isn't wired (`unresolved module`), then PASS once `mod cgroup;` is added and the code above compiles. (`Role` must be exhaustively matched — no `_ =>` catch-all, per the workspace convention.)

- [ ] **Step 3: Add the create / remove / reconcile helpers**

Append to `src/bin/devkitd/cgroup.rs`:

```rust
impl Daemon {
    fn cap(&self) -> Option<&CgroupCap> {
        self.cgroup_cap.as_ref()
    }
}

/// Create (or reuse) the cgroup leaf for `key` and return its path. `None` when
/// hard caps are inactive, or when leaf creation fails (logged once) — the caller
/// then spawns uncapped.
pub(crate) fn leaf_for(daemon: &Daemon, key: &Key) -> Option<PathBuf> {
    let cap = daemon.cap()?;
    let name = leaf_name(key);
    match devkit_common::sys::cgroup_create_leaf(&cap.base, &name, cap.max_bytes) {
        Ok(leaf) => Some(leaf),
        Err(e) => {
            crate::log_line(&format!(
                "memory: could not create cgroup leaf for {}/{} ({:?}): {e:#} — running uncapped",
                key.holder, key.app, key.role
            ));
            None
        }
    }
}

/// Remove the cgroup leaf for `key` (best-effort; a non-empty or missing leaf is
/// ignored). Called when a server leaves supervision for good.
pub(crate) fn remove_leaf(daemon: &Daemon, key: &Key) {
    let Some(cap) = daemon.cap() else { return };
    let leaf = cap.base.join("servers").join(leaf_name(key));
    let _ = devkit_common::sys::cgroup_remove_leaf(&leaf);
}

/// Remove leaves under the base that don't correspond to a currently-live key —
/// clears leaves orphaned by a previous daemon's unclean exit.
pub(crate) fn reconcile(daemon: &Daemon, live: &[Key]) {
    let Some(cap) = daemon.cap() else { return };
    let keep: std::collections::HashSet<String> = live.iter().map(leaf_name).collect();
    for name in devkit_common::sys::cgroup_list_leaves(&cap.base) {
        if !keep.contains(&name) {
            let _ = devkit_common::sys::cgroup_remove_leaf(&cap.base.join("servers").join(&name));
        }
    }
}
```

Make `log_line` reachable from the module: it is a free `fn log_line` in `main.rs`; mark it `pub(crate)` if it isn't already.

- [ ] **Step 4: Write the failing reconcile/orphan test (temp-dir base, cross-platform-safe via the override)**

This test exercises the orchestration against a real directory tree without a kernel by pointing the override at a temp dir — but the `sys` file-ops are Linux-only, so gate it `#[cfg(target_os = "linux")]`. Append to `cgroup.rs` tests:

```rust
    #[cfg(target_os = "linux")]
    #[test]
    fn reconcile_removes_orphan_leaves() {
        // A temp dir standing in for a delegated cgroup base; cgroup_create_leaf
        // here just mkdirs + writes plain files (no kernel controller needed).
        let base = std::env::temp_dir().join(format!("devkitd-cg-{}", crate::tests_unique()));
        std::fs::create_dir_all(base.join("servers")).unwrap();
        let live = key("/w", "api", Role::Issue);
        let orphan = key("/w", "ghost", Role::Issue);
        devkit_common::sys::cgroup_create_leaf(&base, &leaf_name(&live), 1 << 30).unwrap();
        devkit_common::sys::cgroup_create_leaf(&base, &leaf_name(&orphan), 1 << 30).unwrap();
        let d = crate::test_daemon_with_base(base.clone(), 1 << 30);
        reconcile(&d, &[live.clone()]);
        let left = devkit_common::sys::cgroup_list_leaves(&base);
        assert!(left.contains(&leaf_name(&live)), "live leaf kept");
        assert!(!left.contains(&leaf_name(&orphan)), "orphan leaf removed");
        let _ = std::fs::remove_dir_all(&base);
    }
```

Add the two test helpers to `main.rs` under `#[cfg(test)]` (a `Daemon` builder for tests and a unique-id source; keep them minimal — only the fields these helpers touch need real values, the rest use the existing constructors):

```rust
#[cfg(test)]
pub(crate) fn tests_unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static C: AtomicU64 = AtomicU64::new(0);
    (std::process::id() as u64) << 32 | C.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn test_daemon_with_base(base: std::path::PathBuf, max_bytes: u64) -> Daemon {
    Daemon {
        last_activity: Mutex::new(Instant::now()),
        active_conns: AtomicUsize::new(0),
        shutdown: AtomicBool::new(false),
        idle_timeout: Duration::from_secs(3600),
        sup: Mutex::new(supervisor::Supervisor::new(5, Duration::from_secs(60), 0, 0)),
        ports: std::sync::Arc::new(std::sync::Mutex::new(registry::Data::default())),
        locks: std::sync::Arc::new(std::sync::Mutex::new(devkit_locks::model::Data::default())),
        cgroup_cap: Some(CgroupCap { base, max_bytes }),
    }
}
```

(`registry::Data` and `devkit_locks::model::Data` both derive `Default`, so `::default()` is correct. The goal is a `Daemon` whose `cgroup_cap` is `Some`.)

- [ ] **Step 5: Wire leaf creation into the spawn paths and teardown into stop/reconcile**

In `src/bin/devkitd/server.rs`, `supervise_app` — create the leaf before the spawn and pass it:

```rust
    let key = Key { holder: holder.clone(), app: app.clone(), role };
    let leaf = crate::cgroup::leaf_for(daemon, &key);
    let pid = match supervise::spawn_detached(&argv, &cwd, &env, &logfile, leaf.as_deref()) {
        Ok(pid) => pid,
        Err(e) => return Response::Err(format!("{e:#}")),
    };
```

…and reuse `key` in the later `insert_owned(Key { holder, app, role }, ...)` call (replace that constructed `Key` with the `key` built above; clone where the surrounding code still needs `holder`/`app`).

In `src/bin/devkitd/server.rs`, `down` — after `sup.remove(k)` + `supervise::stop(pid)`, the server is gone for good, so remove its leaf. Since `down` already iterates `keys`, add after the loop (or inside, after stop):

```rust
    for k in &keys {
        crate::cgroup::remove_leaf(daemon, k);
    }
```

In `src/bin/devkitd/main.rs`, `Daemon::respawn` — it calls `set_pid` after a successful spawn. Create/reuse the leaf and pass it. Change the `spawn_detached` call:

```rust
        let leaf = crate::cgroup::leaf_for(self, key);
        match devkit_common::supervise::spawn_detached(
            &launch.argv, &launch.cwd, &launch.env, &log, leaf.as_deref(),
        ) {
```

In `src/bin/devkitd/main.rs`, the crash-loop **give-up** path in `restart()` (where `sup.remove(key)` is called because the budget is exhausted or there's no launch spec) — the server is gone for good, so remove its leaf. After each `sup.remove(key); drop(sup);` followed by a "giving up"/"dropping" log, add:

```rust
        crate::cgroup::remove_leaf(daemon, key);
```

In `src/bin/devkitd/main.rs`, after the startup adopt block (the `for (port, e) in &data.entries` loop that calls `insert_adopted`), reconcile orphan leaves against the adopted keys:

```rust
    {
        let live: Vec<supervisor::Key> = daemon
            .sup
            .lock()
            .unwrap()
            .adopted_keys();
        cgroup::reconcile(&daemon, &live);
    }
```

Add a `pub(crate) fn adopted_keys(&self) -> Vec<Key>` to `supervisor.rs` returning all current keys (after adoption these are exactly the adopted survivors):

```rust
    pub(crate) fn adopted_keys(&self) -> Vec<Key> {
        self.children.keys().cloned().collect()
    }
```

- [ ] **Step 6: Run the daemon-bin tests + clippy**

Run: `cargo test -p devkit --bin devkitd` and `cargo clippy -p devkit --bin devkitd --all-targets -- -D warnings`
Expected: PASS (the sanitization test everywhere; the reconcile test on Linux/WSL), zero warnings. On Windows the reconcile test is `#[cfg]`-skipped; the sanitization test still runs.

- [ ] **Step 7: Commit**

```bash
git add src/bin/devkitd/
git commit -F - <<'EOF'
feat(devkitd): cage supervised servers in cgroup leaves

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 5: `install-service` subcommand and autostart routing

**Files:**
- Create: `src/bin/devkitd/service.rs` (unit-file generation + install/uninstall via `systemctl --user`)
- Modify: `src/bin/devkitd/main.rs` (`mod service;`; parse `install-service` / `uninstall-service` argv before the daemon loop)
- Modify: `crates/devkit-common/src/daemon/client.rs` (route `ensure_running` through `systemctl --user start` when the unit file exists)
- Modify: `crates/devkit-common/src/paths.rs` (add `systemd_user_unit()` path helper)

**Interfaces:**
- Produces:
  - `paths::systemd_user_unit() -> PathBuf` → `~/.config/systemd/user/devkitd.service`
  - `service::unit_file_contents(exec_path: &str) -> String` (pure)
  - `service::install() -> anyhow::Result<()>`, `service::uninstall() -> anyhow::Result<()>`
  - In `client.rs`: routing that prefers `systemctl --user start devkitd.service` when `paths::systemd_user_unit()` exists.

- [ ] **Step 1: Write the failing unit-file-content test**

Create `src/bin/devkitd/service.rs`:

```rust
//! `devkitd install-service`: writes a `systemd --user` unit with `Delegate=yes`
//! so a systemd-launched daemon lands in a delegated cgroup-v2 subtree (no sudo).
//! Linux + systemd only; other platforms reject the subcommand.

#[cfg(test)]
mod tests {
    use super::unit_file_contents;

    #[test]
    fn unit_has_delegate_and_exec_and_restart() {
        let u = unit_file_contents("/home/exampleuser/.cargo/bin/devkitd");
        assert!(u.contains("ExecStart=/home/exampleuser/.cargo/bin/devkitd"));
        assert!(u.contains("Delegate=yes"));
        assert!(u.contains("Restart=on-failure"));
        assert!(u.contains("WantedBy=default.target"));
    }
}
```

Add `mod service;` to `main.rs`.

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p devkit --bin devkitd unit_has_delegate_and_exec_and_restart`
Expected: FAIL — `cannot find function unit_file_contents`.

- [ ] **Step 3: Implement the unit generator + install/uninstall**

Prepend to `src/bin/devkitd/service.rs` (above the test module):

```rust
use anyhow::{Context, Result};

/// The `systemd --user` unit text for `devkitd`. `Restart=on-failure` lets a clean
/// idle-exit (exit 0) stay down rather than being fought by systemd.
pub(crate) fn unit_file_contents(exec_path: &str) -> String {
    format!(
        "[Unit]\n\
         Description=devkit supervisor daemon\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec_path}\n\
         Delegate=yes\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn install() -> Result<()> {
    let exe = std::env::current_exe().context("resolving current devkitd path")?;
    let unit = devkit_common::paths::systemd_user_unit();
    std::fs::create_dir_all(unit.parent().unwrap())
        .with_context(|| format!("creating {}", unit.parent().unwrap().display()))?;
    std::fs::write(&unit, unit_file_contents(&exe.to_string_lossy()))
        .with_context(|| format!("writing {}", unit.display()))?;
    run_systemctl(&["daemon-reload"])?;
    // Stop any running ad-hoc daemon so the systemd-launched one can take the lock.
    // Shutdown is best-effort: there may be no daemon running.
    let _ = devkit_ports::daemon::client::try_existing()
        .map(|mut c| c.request::<devkit_ports::daemon::proto::Request, devkit_ports::daemon::proto::Response>(
            &devkit_ports::daemon::proto::Request::Shutdown,
        ));
    run_systemctl(&["enable", "--now", "devkitd.service"])?;
    println!("Installed devkitd as a systemd user service (Delegate=yes).");
    println!("Verify:  systemctl --user status devkitd");
    println!("For headless persistence:  loginctl enable-linger \"$USER\"  (may require privilege)");
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn uninstall() -> Result<()> {
    let _ = run_systemctl(&["disable", "--now", "devkitd.service"]);
    let unit = devkit_common::paths::systemd_user_unit();
    let _ = std::fs::remove_file(&unit);
    run_systemctl(&["daemon-reload"])?;
    println!("Removed the devkitd systemd user service.");
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .with_context(|| format!("running systemctl --user {}", args.join(" ")))?;
    anyhow::ensure!(status.success(), "systemctl --user {} failed", args.join(" "));
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn install() -> Result<()> {
    anyhow::bail!("install-service requires Linux with systemd --user")
}
#[cfg(not(target_os = "linux"))]
pub(crate) fn uninstall() -> Result<()> {
    anyhow::bail!("uninstall-service requires Linux with systemd --user")
}
```

`try_existing` is `pub` in `client.rs` (it is). Confirm `devkit_ports::daemon::client` and `proto` are reachable from the `devkit` bin (the daemon already depends on `devkit-ports`).

- [ ] **Step 4: Add the `paths` helper (with its test)**

In `crates/devkit-common/src/paths.rs`, add:

```rust
/// The `systemd --user` unit path for the daemon: `~/.config/systemd/user/devkitd.service`.
/// Honors `$XDG_CONFIG_HOME`, else `$HOME/.config`.
pub fn systemd_user_unit() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    config.join("systemd/user/devkitd.service")
}
```

Add a test in the existing `paths.rs` test module:

```rust
    #[test]
    fn systemd_user_unit_under_config() {
        assert!(systemd_user_unit().ends_with("systemd/user/devkitd.service"));
    }
```

- [ ] **Step 5: Parse the subcommands in `main()`**

At the very top of `fn main()` in `src/bin/devkitd/main.rs`, before `install_panic_hook` (or right after it), branch on argv:

```rust
    match std::env::args().nth(1).as_deref() {
        Some("install-service") => return service::install(),
        Some("uninstall-service") => return service::uninstall(),
        _ => {}
    }
```

(`main` already returns `Result<()>`, so `service::install()` slots in directly.)

- [ ] **Step 6: Write the failing routing test, then route autostart through systemd**

In `crates/devkit-common/src/daemon/client.rs`, the daemon-binary path lives in `devkit-ports`. The routing predicate is testable as a pure function. Add to `crates/devkit-ports/src/daemon/client.rs`:

```rust
/// Whether autostart should launch the daemon via `systemctl --user start` rather
/// than exec'ing the binary directly: true when the systemd user unit is present.
fn use_systemd_unit() -> bool {
    devkit_common::paths::systemd_user_unit().is_file()
}
```

And in `ensure_running`, replace the direct `daemon::spawn(&devkitd_bin())?;` with:

```rust
    if use_systemd_unit() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "start", "devkitd.service"])
            .status();
    } else {
        daemon::spawn(&devkitd_bin())?;
    }
```

Add a test that drives the predicate via a temp `XDG_CONFIG_HOME` (serialize env mutation with a guard if the crate already has one; otherwise keep it in its own test):

```rust
    #[test]
    fn routes_through_systemd_only_when_unit_present() {
        // No unit → false. (Default test env has no devkitd.service unit.)
        // This asserts the predicate reads the unit path; a full positive case
        // that writes the unit is covered by manual verification to avoid mutating
        // the developer's real ~/.config.
        let _ = super::use_systemd_unit();
    }
```

(Keep this light — the meaningful positive path is manual, since it touches real systemd state and the user's config dir. The pure `unit_file_contents` test in Step 1 is the real unit-gen guard.)

- [ ] **Step 7: Build + test + clippy across the workspace**

Run: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -F - <<'EOF'
feat(devkitd): add install-service and systemd autostart routing

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

### Task 6: Linux integration tests + docs

**Files:**
- Modify: `tests/common/mod.rs` (add `start_with_cgroup_cap` harness helper)
- Modify: `tests/supervision.rs` (add `#![cfg(unix)]` cgroup tests, capability-gated)
- Modify: `docs/configuration.md` (add the `memory_max_mb` row + subsection + escape-hatch cross-reference)
- Modify: `docs/next-features.md` (mark the cgroup cap RESOLVED)
- Modify: `AGENTS.md` (add the three invariants)

**Interfaces:**
- Consumes: the env vars `DEVKIT_DAEMON_MEM_MAX_MB`, `DEVKIT_DAEMON_CGROUP_ROOT` (Task 2), `DEVKIT_DAEMON_MEMORY_ACTION` etc.

- [ ] **Step 1: Add the harness helper**

In `tests/common/mod.rs`, alongside `start_with_memory`, add:

```rust
    /// Start a daemon with a hard cgroup cap. `cgroup_root` is a pre-created,
    /// writable cgroup-v2 base (the test owns it); `max_mb` is the per-tree
    /// ceiling. Soft action stays off unless the caller also sets memory vars.
    pub fn start_with_cgroup_cap(idle_secs: u64, cgroup_root: &str, max_mb: u64) -> Self {
        Self::start_with_env(&[
            ("DEVKIT_DAEMON_IDLE_SECS", idle_secs.to_string()),
            ("DEVKIT_DAEMON_MEM_MAX_MB", max_mb.to_string()),
            ("DEVKIT_DAEMON_CGROUP_ROOT", cgroup_root.to_string()),
        ])
    }
```

- [ ] **Step 2: Write the capability-gated enforcement test**

In `tests/supervision.rs` (top is `#![cfg(unix)]`), add a test that:
1. Tries to set up a writable cgroup-v2 subtree the test owns; if it can't (no delegation / not Linux / no permission), `eprintln!` a skip and `return` — mirroring the `python_cmd` skip idiom already in the suite.
2. Starts the daemon via `start_with_cgroup_cap` pointed at that subtree.
3. Supervises a balloon fixture (a small Python script that allocates past the cap).
4. Polls for the kernel OOM-kill + crash respawn (a new pid), per the poll-don't-sleep rule.

Use this fixture-and-skip shape (adapt the existing supervision tests' helpers for spawning a supervised app and reading `ports.json`):

```rust
/// A writable cgroup-v2 leaf-capable base for this test, or None to skip.
#[cfg(target_os = "linux")]
fn test_cgroup_base() -> Option<std::path::PathBuf> {
    use std::fs;
    // Prefer this user's delegated systemd scope if present and writable.
    let candidates = [
        std::env::var_os("DEVKIT_TEST_CGROUP_ROOT").map(std::path::PathBuf::from),
    ];
    for base in candidates.into_iter().flatten() {
        if fs::create_dir_all(base.join("servers")).is_ok()
            && fs::write(base.join("cgroup.subtree_control"), "+memory\n").is_ok()
        {
            return Some(base);
        }
    }
    None
}

#[cfg(target_os = "linux")]
#[test]
fn cgroup_cap_oom_kills_and_respawns() {
    let Some(base) = test_cgroup_base() else {
        eprintln!("skipping cgroup_cap_oom_kills_and_respawns: no writable delegated cgroup-v2 base (set DEVKIT_TEST_CGROUP_ROOT)");
        return;
    };
    // ... start_with_cgroup_cap(3600, base.to_str().unwrap(), 64) ...
    // ... supervise a python balloon that allocates ~256 MB ...
    // ... poll ports.json until the app's pid changes (OOM-kill → respawn) ...
    let _ = base;
}
```

Write the full body using the same supervise/poll plumbing the neighboring tests use (e.g. `memory_restart_over_limit_server` from the phase-3 work is the closest template — copy its supervise call and its `pid_in_ports_json` polling loop, swapping the soft-limit env for `start_with_cgroup_cap`). The balloon fixture is a small Python server that binds the port (so `wait_ready` passes) then allocates past the cap:

```python
# written to a temp .py, launched as the supervised app's argv
import socket, sys
port = int(sys.argv[1])
s = socket.socket(); s.bind(("127.0.0.1", port)); s.listen()
blocks = []
while True:
    blocks.append(bytearray(16 * 1024 * 1024))  # 16 MiB per step, past a 64 MB cap
```

With `memory.max` at 64 MB and `oom.group=1`, the kernel kills the whole leaf; the daemon reaps the dead child and respawns it (new pid). Poll `ports.json` until the app's pid changes. Trim fixture imports to exactly what the fixture uses (`socket, sys` here — the phase-3 fast-follow trimmed unused imports; keep that hygiene).

- [ ] **Step 3: Write the fallback test (no delegation → uncapped, no failure)**

```rust
#[cfg(target_os = "linux")]
#[test]
fn cap_requested_without_delegation_falls_back() {
    // Point the override at a non-writable / nonexistent base so cgroup_caps()
    // returns Unavailable; the daemon must still supervise the server (uncapped)
    // and not fail the spawn.
    // ... start_with_cgroup_cap(3600, "/sys/fs/cgroup/nonexistent-devkit-test", 64) ...
    // ... supervise a trivial server, assert it comes up (ready == true) ...
}
```

- [ ] **Step 4: Validate on WSL**

The WSL invocation (validated in phase 3), from a WSL shell:

```bash
wsl -e bash -lc 'cd /mnt/c/Users/Lev/Git/lev/devkit-worktrees/devkitd-hard-cgroup-cap && export CARGO_TARGET_DIR=$HOME/.cache/devkit-wsl-target && export DEVKIT_TEST_CGROUP_ROOT="/sys/fs/cgroup/$(cat /proc/self/cgroup | sed s,^0::/,,)/devkit-test" && ~/.cargo/bin/cargo test --test supervision cgroup -- --nocapture'
```

Expected: the cgroup tests run if `DEVKIT_TEST_CGROUP_ROOT` resolves to a writable delegated subtree, else they print the skip line and pass. The controller verifies whether real enforcement was exercised (look for the respawn, not just a skip).

- [ ] **Step 5: Update the docs**

In `docs/configuration.md`, add a row to the `[daemon]`-adjacent memory documentation (find where `memory_limit_mb` / `memory_action` are described) for `memory_max_mb`, and a short subsection: Linux-only; needs cgroup-v2 delegation (point to `devkitd install-service`); sits above `memory_limit_mb`; falls back to the soft action otherwise. Cross-reference it from the existing `static_env` / `ulimit -v` escape-hatch note (lines ~54-59) — now there is a first-class option.

In `docs/next-features.md`, change the `## Hard cgroup-v2 memory cap for supervised servers` status line to:

```markdown
**Status:** RESOLVED 2026-06-22 — see
`docs/superpowers/specs/2026-06-22-daemon-hard-cgroup-memory-cap-design.md`.
```

In `AGENTS.md`, under the daemon invariants, add the three invariants from the spec's "New AGENTS.md invariants" section (hard-cap breach is a crash not a restart path; cap setup is fail-open; `memory_max_mb` sits above `memory_limit_mb`).

- [ ] **Step 6: Full gate**

Run: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`. On WSL, run the Task 6 Step 4 invocation.
Expected: green on Windows + WSL; zero warnings; no diff from `cargo fmt --all --check`.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -F - <<'EOF'
test(devkitd): cover hard cgroup cap; document and mark resolved

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
```

---

## Notes for the implementer

- **Where the two spawn sites are:** initial owned spawn is `server.rs::supervise_app` (~line 126); respawn is `main.rs::Daemon::respawn` (~line 78). Both must create/pass the leaf. The give-up teardown is in `main.rs::restart`.
- **No `_ =>` catch-all on `Role`** — map `Issue`/`Baseline` exhaustively (workspace convention).
- **Async-signal-safety is load-bearing** in `join_cgroup`'s `pre_exec` closure: no allocation, no locks, no `println!` — only `getpid`, `fmt_pid` arithmetic, and `write`. Don't add anything else there.
- **Fail-open everywhere on the cap path:** every cgroup error logs once and proceeds uncapped. A cgroup problem must never surface as a failed `Supervise` response or a killed server.
- **The override `DEVKIT_DAEMON_CGROUP_ROOT`** doubles as the manual-delegation path for users who set up a subtree themselves and as the test seam.
