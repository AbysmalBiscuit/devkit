# devkit MCP server — `devrun` actions (phase 2) — design

## Goal

Extend the devkit MCP server's action registry with a synchronous, library-backed
subset of `devrun`, so a coding agent can inspect, start, stop, and read the logs of
dev servers for a worktree over MCP. The two-tool meta shape (`devkit_describe` +
`devkit_call`) does not change — phase 2 is purely new registry entries plus the
library extraction that makes them possible without shelling out.

## Scope

Phase 2 adds four `devrun` actions: `devrun.status`, `devrun.up`, `devrun.down`, and
`devrun.logs`. `up` is a **non-blocking kick-and-poll**: it spawns servers and returns
immediately, leaving the agent to poll `devrun.status` for readiness.

### Non-goals (phase 2)

- **Blocking `up`.** `devrun up` blocks ~120s on readiness (`wait_ready`); a synchronous
  MCP tool cannot. The MCP variant returns after spawning and never waits.
- **`baseline` role for `up`.** Baseline A/B is a niche human workflow whose
  `ensure_fresh` git step is slow. MCP `up` is `issue`-role only. `down`, `status`, and
  `logs` still see `baseline` servers (they only read/stop existing state).
- **`devrun logs --follow`.** Streaming/tail-follow is not a request/response fit. Only
  the query mode (last N lines) is exposed.
- **`issue` actions.** Deferred — `issue`'s logic lives in the binary modules, not in a
  library facade, so it needs extraction first. Tracked in `docs/next-steps.md`.

## Background — what phase 2 builds on

The building blocks already exist as library functions; only `devrun`'s *orchestration*
lives in the binary.

- **Ports facade** (`crates/devkit-ports/src/registry.rs`): `snapshot`, `alloc`,
  `release`, `prune`, `status_table`, and `listening(port)` — a one-shot TCP connect
  that decides "accepting connections."
- **Supervision** (`crates/devkit-common/src/supervise.rs`): `spawn_detached` (spawn,
  returns immediately), `wait_ready` (the blocking readiness poll — separable),
  `probe_port` (one-shot readiness check), `stop(pid)`, `tail(logfile, lines)`.
- **`devrun` binary** (`src/bin/devrun/`): `cmd_up` (orchestration at `main.rs`),
  `env.rs` (doppler/env construction), `baseline.rs::ensure_fresh` (git worktree).
  `down`/`status`/`logs` are thin wrappers over the facades; `up` is the orchestration
  that must be extracted.

Two facts drive the design:

1. **Readiness is observable without blocking.** `registry::listening` /
   `supervise::probe_port` report whether a port accepts connections, so the agent can
   poll instead of the server holding the call open. `status_table` already uses
   `listening` for its ready column.
2. **Spawn and wait are already separate functions.** A non-blocking `up` is
   `spawn_detached` minus `wait_ready` — no new spawning machinery, just skipping the
   wait.

## Architecture

### Facade extraction, no shelling

The MCP handler must not shell out to the `devrun` binary (matching v1's "thin adapter
over facades" stance, and to keep structured output and avoid a binary-path
dependency). The `up` orchestration currently inline in `src/bin/devrun/main.rs`
(`cmd_up`) plus the env construction in `src/bin/devrun/env.rs` is extracted into a
library function that both callers share:

```
// in devkit-ports (or a new devkit-ports::run module)
fn bring_up(root: &Path, apps: &[String], env: &EnvOverrides, wait: bool) -> Result<Vec<ServerStatus>>
```

- The `devrun` binary's `cmd_up` refactors to call `bring_up(..., wait: true)` so the
  CLI keeps its blocking readiness behavior unchanged.
- The MCP handler calls `bring_up(..., wait: false)` and returns the `ServerStatus`
  list immediately.

`status`, `down`, and `logs` are thin enough that their handlers call the existing
facades directly — no extraction needed beyond a small structured-status helper (the
existing `status_table` renders a table; the MCP handler needs the structured rows).

### Crate layout

A new module `crates/devkit-mcp/src/devrun.rs` exposes `actions() -> Vec<Action>`,
wired into the registry with one `extend` line in `actions::actions()`. This mirrors
how `ports` and `locks` register today; the tool shape is untouched.

## Action catalog (phase 2)

| Action | Args | Root | Facade call |
|---|---|---|---|
| `devrun.status` | `all?` | required unless `all` | `snapshot` → structured per-app rows with readiness (`listening`) |
| `devrun.up` | `apps[]`, `env?` | **required** (holder = root) | `bring_up(root, apps, env, wait=false)` |
| `devrun.down` | `role?` | **required** (holder = root) | `release(root, role)` + `stop(pid)` per server |
| `devrun.logs` | `app`, `lines?`, `role?` | **required** | `snapshot` → log path + `supervise::tail(path, lines)` |

**Argument detail:**

- `apps` is a list of app names resolved against the project's `devkit.toml` catalog
  (same as `ports.alloc`), which is why `up` needs `root`.
- `env` is an optional `{KEY: VALUE}` map of per-launch overrides (the MCP equivalent of
  `devrun up --env K=V`).
- `role` is the `Role` enum (`issue` | `baseline`). `up` is `issue`-only and takes no
  `role`. `down` with no `role` stops **all** roles for that root (mirrors
  `devrun down`). `logs` defaults to `issue`.
- `lines` is the tail length for `logs`; default `200`.
- `all` on `status` returns every tracked server across worktrees; omitted, status is
  filtered to `root`.

## The non-blocking `up` (kick-and-poll)

`up` runs the fast synchronous prelude — load the app catalog (`root`), allocate ports
(`registry::alloc`, holder = root), build env, `spawn_detached` — then returns without
calling `wait_ready`:

```json
{
  "servers": [
    { "app": "web", "role": "issue", "port": 3000, "pid": 1234, "state": "starting" }
  ],
  "hint": "poll devrun.status for readiness"
}
```

The agent polls `devrun.status`, where each app reports one of:

- `ready` — the port accepts connections (`listening` true).
- `starting` — the pid is alive but the port is not yet accepting.
- `crashed` — the pid is gone and the port is not accepting.

`up` uses **daemon supervision when `devkitd` is running** (servers survive the agent's
session and auto-restart within the crash-loop budget), and plain detached spawn
otherwise (servers survive session end because `spawn_detached` detaches them, but
nothing restarts them). This mirrors `devrun`'s existing behavior; the only difference
from the CLI is skipping the readiness wait.

## Identity and targeting

- **`root`** — an explicit argument on all four actions; the worktree the operation
  targets. No CWD inference (matching v1); when required and absent the call fails with
  a clear error.
- **`holder`** — ports allocated/released by `up`/`down` use `root` as the holder, the
  same convention v1's `ports.alloc`/`ports.release` use (the registry's liveness check
  treats the holder as an existing filesystem path).

## Daemon relationship

`status` reads and `down` writes go through the facades, which already use `devkitd`
transparently when it is running and fall back to the direct flock path otherwise.
`up`'s supervision handoff follows the same rule: hand spawned servers to the daemon
when present, else spawn detached. Phase 2 does not auto-spawn `devkitd`.

## Error and conflict semantics

Same as v1:

- **Protocol/validation failures** (malformed JSON-RPC, unknown action, schema
  mismatch) → a JSON-RPC **error response**.
- **Action ran but failed** (a facade `anyhow` error — e.g. a port-allocation conflict,
  an unknown app name, a missing log file) → a `tools/call` result with `isError: true`
  and the full error chain.
- **`up` returning before readiness is success, not error.** `state: "starting"` is the
  normal kick-and-poll outcome; the agent learns readiness from `status`, not from an
  error.

Structured results (the `ServerStatus` rows, freed-port lists, log text) are returned as
JSON, not rendered tables — the agent receives machine-readable data.

## Testing

TDD throughout; `cargo test --workspace` is the gate. Tests poll for expected state
rather than sleeping a fixed interval (the CI Windows rule — a loaded runner spawns and
reaps later than a short sleep allows).

- **Registry / describe:** `devkit_describe()` now lists the four `devrun` actions;
  `devkit_describe(a)` returns each one's schema.
- **`devrun.up`:** kick a trivial listener via the handler, assert the result is
  `state: "starting"` with allocated ports, then poll `devrun.status` until the app
  flips to `ready` once the test process binds the port. Assert the allocation landed in
  the registry.
- **`devrun.down`:** with a server tracked, `down` stops the pid and releases the ports;
  `status` afterward shows it gone.
- **`devrun.status`:** structured per-app rows with the readiness field; `all` widens
  the view past a single root.
- **`devrun.logs`:** returns the last N lines of an app's log; a missing log surfaces as
  an `isError: true` result.
- **Argument validation:** missing-required-`root`, unknown app, and unknown action are
  rejected with the expected errors.

## Sequencing

Phase 2 lands as a PR to `main` after v1 (already merged). The `up` extraction is the
one cross-cutting change (it touches the `devrun` binary); `status`/`down`/`logs` are
additive. The extraction is done first so the binary and the MCP handler share one code
path before the handlers are written.

## Resolved decisions

1. **Surface** — four `devrun` actions: `status`, `up` (non-blocking), `down`, `logs`
   (query mode). `issue` deferred.
2. **`up` is non-blocking** — kick-and-poll; the agent polls `devrun.status` for
   readiness.
3. **`up` is `issue`-role only** — `baseline` excluded (slow git step, niche).
4. **`down` with no `role`** stops all roles for the root.
5. **`up` uses daemon-if-present, else detached** — not daemon-required.
6. **No shelling** — extract `devrun`'s `up` orchestration into a shared library
   function with a `wait` flag; the CLI keeps blocking, the MCP handler does not.
7. **Targeting** — `root` explicit per call; holder = root for ports.

## Open questions

None blocking. To confirm during the plan phase:

- The exact home and signature of the extracted `bring_up` function (a new
  `devkit-ports::run` module vs. extending an existing module), and how much of `env.rs`
  moves with it.
- Whether `devrun.status`'s structured rows reuse a refactored `status_table` internals
  or a separate structured builder, so the CLI table and the MCP rows stay in sync.
