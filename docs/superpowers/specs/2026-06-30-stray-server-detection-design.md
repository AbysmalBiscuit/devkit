# Stray dev-server detection & reaping

**Status:** design / awaiting review
**Date:** 2026-06-30
**Branch:** `feat/stray-servers`

## Problem

Agents sometimes start dev servers directly (`doppler run -- bun nitro dev --port 9200`,
`uvicorn …`) instead of through `devrun up`. Those processes are invisible to the port
registry: `devrun status` doesn't list them, `devrun down` can't stop them, and they leak —
holding ports and memory across a worktree's life. The `devrun-nudge` hook is the
prevention; this feature is the **detection and cleanup** for when prevention fails.

A standalone script (`~/.local/bin/reap-stray-servers.sh`) already does this on Linux with a
hardcoded Adaptyv regex. This integrates the capability into devkit so it is **config-driven**
(no hardcoded app patterns), **visible** in the normal status surfaces, and **safe against
agents** triggering mass kills.

## Goals

- Detect dev servers running outside the registry, by two complementary signals.
- Surface them read-only in `devrun status` and `devkit doctor` (safe on MCP).
- Provide `devrun reap` to kill them — human-gated, never agent-triggerable.
- Stay green on the ubuntu/macos/windows CI gate.

## Non-goals

- Preventing strays (that's the nudge hook).
- Killing registry-tracked servers (that's `devrun down`).
- Exposing any mutating reap operation on the MCP surface.

## Detection model — two passes

A *stray* is a dev server that is listening and is **not** owned by a live registry row
(no `Entry` for its port, and its pid is not a tracked pid or a child of one).

### Pass A — port-band scan (cross-platform)

Reuses `registry::listening(port) -> bool` (a localhost TCP-connect probe that already works
on all three OSes). For each configured app, scan the band `[base_port, base_port + WIDTH)`;
a port that is listening but has no registry `Entry` is a candidate. `WIDTH` defaults to **64**
and is configurable. Bands may overlap when two apps' `base_port`s are close — harmless, since
detection only asks "listening and untracked?"; a candidate is found regardless of which band
it falls in. App attribution from the band is a best-guess label only.

This pass is the Windows/macOS backstop. It finds listeners but cannot, on its own, attribute a
pid/holder.

### Pass B — process scan (`#[cfg(unix)]`)

The `ps aux` equivalent: enumerate the process table (read `/proc` directly — no new
dependency, matching the hand-rolled style of `listening()`). For each process, match its argv
against **signatures derived from each app's `launch` command**, not a hardcoded regex.

**Signature derivation:** parse `launch`, drop wrapper/substitution noise — `doppler`, `run`,
`-p <x>`, `-c <x>`, `--preserve-env=…`, `--`, and the `--port {port}` placeholder — and keep the
server tokens:

| `launch` (abridged) | signature |
|---|---|
| `doppler run … -- bun nitro dev --port {port}` | `bun nitro dev` |
| `doppler run … -- uv run uvicorn server.main:create_app …` | `uvicorn` |
| `doppler run … -- bun vite --port {port}` | `bun vite` |
| `doppler run … -- bun mintlify dev --port {port}` | `bun mintlify dev` |
| `doppler run … -- bun wrangler dev --port {port}` | `bun wrangler dev` |
| `doppler run … -- uv run flask --app src/server.py run …` | `flask … run` |

A process is a candidate when: its argv contains a signature, its cwd (`/proc/<pid>/cwd`) is
under a managed root, and its pid is not in the tracked-pid tree. From the candidate, climb to
the **launch root** — the highest consecutive wrapper ancestor (`doppler`/runtime), stopping at
any shell, Claude, or the supervisor — exactly as the script does. The launch root and its
subtree are what `reap` would target.

This pass works on any port (catches the strays a band would miss), and supplies pid, cwd,
holder, and command.

### Managed roots & holder attribution

Managed roots come from config (`defaults.worktree_root`, `defaults.baseline_path`) plus the
set of live registry holder paths — keeping this project-agnostic. A candidate's holder is the
worktree root that encloses its cwd. A candidate whose cwd is under no managed root is ignored,
so dev servers in unrelated projects are never touched.

### Merge

Passes A and B union into one `Vec<Stray>`, deduped by pid and by port (a hit in both →
`source = Both`). The `--port N` token in a process's argv links a Pass-B hit to its Pass-A
port without needing `/proc/net/tcp` inode walking.

## Data model

```rust
// devkit-ports::strays — read-only, serializable; no mutation, no rendering.
pub struct Stray {
    pub port: Option<u16>,        // listening port, if known
    pub pid: Option<u32>,         // launch-root pid, if resolved (unix)
    pub holder: Option<String>,   // worktree root the stray belongs to
    pub app: Option<String>,      // best-guess app name
    pub command: Option<String>,  // launch-root command line (unix)
    pub source: Source,
}
pub enum Source { PortBand, ProcessPattern, Both }
```

Mirrors the `devkit-issue` facade convention: serializable, no rendering, no mutations.

## Testability (design-for-isolation)

The scan must not require real processes or sockets in tests. Following the
`alloc_with`/`alloc` split in `registry`, the core takes injectable seams:

```rust
pub trait PortProbe { fn listening(&self, port: u16) -> bool; }
pub trait ProcTable { fn snapshot(&self) -> Vec<Proc>; } // pid, ppid, argv, cwd

pub fn scan_with(cfg: &Config, data: &Data, ports: &dyn PortProbe, procs: &dyn ProcTable) -> Vec<Stray>;
pub fn scan(cfg: &Config, data: &Data) -> Vec<Stray>; // wires real OS impls
```

Unit tests feed a synthetic `Data` + fake probes and assert the resulting `Stray` set.
Separately tested as pure functions: signature derivation from `launch`, band computation,
launch-root climbing, merge/dedup, and `Scope` filtering.

## Surfacing (read-only)

- **`devrun status`** gains an *untracked* section below the tracked table, holder-scoped by
  default (current worktree via `toplevel(cwd)`), `--all` for every holder. Reuses the
  `Scope` enum.
- **`devkit doctor`** gains a check that counts strays and warns (`✗`/`·`), respecting the
  existing `Check` enum + `--json` output.
- Because both are read-only, a root-scoped detection call is **safe to expose on MCP** — an
  agent can *see* its strays even though it cannot reap them.

## `devrun reap` (CLI-only, human-gated)

New subcommand. Selection comes from `strays::scan` filtered by `Scope` (default
`Current(holder)`, `--all` = every holder). Behavior:

1. Print the stray process tree(s) it would kill (port, pid, holder, command).
2. **Require an interactive TTY** and prompt for confirmation. If stdin is not a terminal
   (agent, pipe, CI), **refuse** — do not fall through to proceed/assume-yes.
3. On confirmation, SIGTERM each launch-root subtree, poll briefly, SIGKILL survivors.

**Agent-safety invariant:** there is **no `--yes`/`--force` flag** that bypasses the TTY
requirement. The TTY gate is the same mechanism that already protects cross-worktree
`devrun down`; with no non-interactive kill path, an agent (no PTY) cannot reap. `reap` is
**never** added to the MCP server.

A stray with no resolvable pid (port-only, e.g. an unrecognized command on a managed port) is
listed by detection but reported **unreapable** with guidance, rather than guessed at. Resolving
port→pid via `/proc/net/tcp` is a possible follow-up, out of scope for v1.

## Cross-platform strategy

| Capability | unix | macOS/windows |
|---|---|---|
| `listening()` port-band scan | ✓ | ✓ |
| process scan, holder attribution, tree-kill | ✓ (`/proc`) | no-op fallback |

On non-unix, detection degrades to port-band only (listed without pid/holder) and `reap` has
nothing it can safely kill. The monorepo is WSL-only, so nothing is lost in practice; the code
must still compile and pass tests everywhere — the `#[cfg(unix)]` blocks have inert fallbacks so
the CI gate stays green.

## Config

- `WIDTH` (port-band width per app) — default 64, configurable under `[defaults]` as
  `stray_scan_width`.
- Managed roots derive from existing `defaults.worktree_root` / `defaults.baseline_path`; no
  new required config.

## Affected units

- `crates/devkit-ports/src/strays.rs` (new) — facade + data model + tests.
- `crates/devkit-ports/src/registry.rs` — reuse `listening`, `Data`, `Scope`; possibly expose a
  tracked-pid-tree helper.
- `crates/devkit-ports/src/config.rs` — add the width default.
- `src/bin/devrun/main.rs` — `status` untracked section; new `reap` subcommand + TTY gate.
- `src/bin/devkit/doctor.rs` — stray-count check.
- `crates/devkit-mcp/…` — read-only detection handler only; **no** reap handler.

## Testing

TDD per `AGENTS.md`: failing test first. `cargo test --workspace`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo fmt --all` before each commit. Process-spawning
tests poll for state, never sleep fixed intervals (Windows-runner rule).

## Decisions locked

- reap = print rows + interactive-TTY confirmation; no `--yes` bypass; CLI-only, not on MCP.
- Detection = port-band (cross-platform) **and** config-derived process scan (unix); union.
- doctor integration included in this iteration.
- Band width default 64, configurable.

## Open questions

- Whether the read-only MCP detection handler ships in this iteration or a follow-up.
