# Full Windows support for devkit

**Status:** implemented. All five binaries build, lint, test, and run on
`x86_64-pc-windows-msvc`. Shipped as a single native pass rather than the three
phased PRs originally planned (see Delivery). The one item still open is adding
Windows targets to the *release* build matrix.
**Date:** 2026-06-20

## Goal

Make all five binaries (`portman`, `devrun`, `issue`, `lock`, `devkit-portd`)
build and run natively on Windows (`x86_64-pc-windows-msvc`), while keeping the
existing Unix behavior byte-for-byte unchanged.

Today the workspace is Unix-only: `nix` is an unconditional dependency and core
code reaches for Unix syscalls (signals, sessions, `waitpid`, `/proc`) and
Unix-domain sockets directly. `fd-lock` is already cross-platform, so the file
locks come for free; everything else needs a portable seam.

## Principle

OS-specific code lives behind **one boundary**, not scattered as `#[cfg]` across
business logic. Callers stay platform-agnostic. This is the load-bearing
constraint: if a reader finds a raw `nix::` call or `std::os::unix` path outside
the boundary module, the design has been violated.

## Architecture: the `devkit_common::sys` boundary

A new module concentrates every OS primitive:

```
crates/devkit-common/src/sys/
├── mod.rs       cross-platform API (signatures + docs), platform dispatch
├── unix.rs      #[cfg(unix)]    — today's nix / /proc code, moved verbatim
└── windows.rs   #[cfg(windows)] — windows-sys FFI
```

`devkit-common` is the shared base crate (both `devkit-locks` and `devkit-ports`
depend on it), so the boundary belongs there. After this change `devkit-locks`
and `devkit-ports` hold **no** direct `nix` dependency; `nix` survives only as a
`devkit-common` Unix-target dependency.

### API

| Function | Unix | Windows |
|---|---|---|
| `process_alive(pid: u32) -> bool` | `kill(pid, None)` | `OpenProcess` + `GetExitCodeProcess` (alive iff `STILL_ACTIVE`) |
| `reap_owned(pid: u32) -> bool` (true = gone) | `waitpid(pid, WNOHANG)` | `!process_alive(pid)` — Windows has no zombies |
| `terminate(pid: u32)` | `kill(pid, SIGTERM)` | `GenerateConsoleCtrlEvent(CTRL_BREAK, pid)` → grace → `TerminateProcess` |
| `detach(&mut Command)` | `pre_exec(setsid)` | `creation_flags(CREATE_NEW_PROCESS_GROUP)` |
| `parent_pid() -> Option<u32>` | `getppid` | Toolhelp32 snapshot (`th32ParentProcessID` of self) |
| `controlling_tty() -> Option<String>` | `ttyname(stdin)` | `None` |
| `tree_rss_bytes(root: u32) -> u64` | `/proc` subtree walk | Toolhelp32 tree + `GetProcessMemoryInfo` |

### Call-site migration (no logic change)

- `devkit_ports::registry::pid_alive`, `devkit_locks::model::pid_alive` → `sys::process_alive`
- `devkit_common::supervise::stop` → `sys::terminate`
- `devkit_common::supervise::spawn_detached` → calls `sys::detach(&mut cmd)` instead of inline `pre_exec`
- `devkit_common::supervise::tree_rss_bytes` → `sys::tree_rss_bytes`
- `devkit_locks::ident` parent-pid / tty → `sys::parent_pid` / `sys::controlling_tty`
- `devkit-portd::supervisor::reap_once` → `sys::reap_owned` (Owned) / `sys::process_alive` (Adopted); drops `nix` from the daemon binary

## Daemon IPC: `interprocess`

Replace `std::os::unix::net::{UnixListener, UnixStream}` with
`interprocess::local_socket` on **both** platforms — a Unix-domain socket on
Unix, a named pipe (`\\.\pipe\devkit-portd-<user>`) on Windows — behind one API.
This removes the IPC `#[cfg]` entirely. Touched: `daemon/client.rs`,
`devkit-portd/main.rs`, `devkit-portd/server.rs`, `tests/common/mod.rs`. A small
helper resolves the platform socket name from `paths`.

The transport landed and was verified on Unix before the Windows named-pipe arm
was added, isolating the transport-swap risk from the Windows work.

## Config-driven `issue setup`

`issue setup` currently symlinks each `app/.env → worktree/.env.local`
(`setup.rs:79`) and hardcodes `bun install` (`setup.rs:94`). Both are
project-specific assumptions baked into the binary, and the symlink is the only
reason a Windows `symlink()` primitive would be needed. The run path
(`devrun`/`env.rs`) already does env correctly and config-driven via
`doppler run -p <project> -c <config> -- <launch>`; the bootstrap path should
match it.

Change:

- Add `setup: Vec<Vec<String>>` to `AppConfig` (default empty). Each inner array
  is one command, run verbatim in that app's directory, in order. Example:
  `setup = [["doppler", "run", "-c", "local_config", "--", "bun", "install"]]`.
- `issue setup` runs each app's `setup` commands instead of the hardcoded
  `bun install`.
- **Delete** the `.env` symlink block. `prep_env` stays (already config-driven;
  still writes `<app>/.env.local`, which frameworks auto-load).

Consequence: removing the symlink also removes `sys::symlink` from the boundary —
it is never needed. Apps started outside `devrun` no longer get an auto-symlinked
`.env`; env now flows through doppler (run path) or `prep_env`/`setup` (bootstrap).

## Path resolution

`paths.rs` assumes XDG/`HOME`. Windows has neither, so state, log, and socket
bases resolve to `%LOCALAPPDATA%` on Windows (`HOME` falls back to `USERPROFILE`),
while `XDG_STATE_HOME` is still honored everywhere so test isolation is preserved;
Unix is unchanged.

## Dependencies

- `devkit-common`: `nix` under `[target.'cfg(unix)'.dependencies]` (already
  there); add `windows-sys` under `[target.'cfg(windows)'.dependencies]` with
  features `Win32_Foundation`, `Win32_System_Threading`, `Win32_System_Console`,
  `Win32_System_Diagnostics_ToolHelp`, `Win32_System_ProcessStatus`.
- `devkit-ports` and the root `devkit` package: add `interprocess`; drop direct
  `nix`.
- `devkit-locks`: drop direct `nix`.

## Delivery

The design was planned as three phased PRs but shipped as a single native-Windows
pass on a Windows host, with the transport swap and the `sys` boundary still
landing first within that pass so the Unix-risk ordering held. What shipped:

**Done**
- `devkit_common::sys` boundary with both `unix.rs` and `windows.rs`; every call
  site migrated. `nix` is now a `devkit-common` `cfg(unix)`-only dependency.
- Daemon transport unified on `interprocess` — Unix-domain socket on Unix, named
  pipe on Windows.
- `issue setup` is config-driven (`setup: Vec<Vec<String>>`); the `.env` symlink
  and hardcoded `bun install` are gone, so no `symlink` primitive is needed.
- `%LOCALAPPDATA%` path bases on Windows; `devrun logs --follow` no longer uses
  an `os::unix` exec; the `libc` winsize ioctl is gated to Unix (Windows uses a
  width fallback).
- The Unix-only integration test (`tests/supervision.rs`, POSIX signals + python
  http server) stays `#[cfg(unix)]`; the python-backed `supervise` unit test
  skips gracefully when no launchable interpreter exists.
- CI: `windows-latest` is in the test matrix and clippy runs on it under
  `-D warnings` (clippy fans out over both platform branches).

**Open**
- The *release* build matrix (`.github/workflows/release-please.yml`) still
  builds only Linux + macOS. Adding `x86_64-pc-windows-msvc` (and
  `aarch64-pc-windows-msvc` build-only) is the remaining work.

## Known limitations (to document in code/README)

- **Graceful-stop fidelity:** `GenerateConsoleCtrlEvent(CTRL_BREAK)` reaches only
  processes sharing a console and process group. Children spawn with
  `CREATE_NEW_PROCESS_GROUP` to maximize reach, but a fully console-less child
  may receive only `TerminateProcess` (no clean flush).
- **`aarch64-pc-windows-msvc`** ships build-only until a native test runner
  exists.

## Testing

- Unix: the full workspace test suite stays the merge gate on Linux + macOS and
  must stay green unchanged.
- Windows: the daemon integration tests (`lifecycle`, `parity`) run over the
  named-pipe transport on `windows-latest`; `supervision` is `#[cfg(unix)]`
  (POSIX signals + python http server) and compiles to nothing on Windows.
- `sys/mod.rs` carries focused unit tests for the boundary primitives
  (`process_alive` of self vs. pid 0, `parent_pid` present, `tree_rss_bytes(self) > 0`).

## Out of scope

- Windows ARM64 test coverage (build-only).
- Any change to Unix runtime behavior.
- Broader config-schema work beyond the `setup` field.
