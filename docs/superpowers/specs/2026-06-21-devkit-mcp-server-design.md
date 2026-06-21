# devkit MCP server — design

## Goal

Expose devkit's port-allocation and file-lock capabilities to coding agents over
the Model Context Protocol (MCP) so an agent can claim files, allocate ports, and
inspect both registries directly instead of shelling out to the `portm`/`lockm`
CLIs. The server is a thin MCP adapter over the existing library facades — it adds
no new coordination logic.

## Scope

**v1 exposes ports + locks, full (9 actions), read and mutate**, over a **meta-MCP**
tool shape (two tools: `devkit_describe` + `devkit_call`). `devrun` and `issue` are
**deferred** to a later phase; the internal action registry is built so they slot in
as new entries without changing the tool shape.

### Non-goals (v1)

- `devrun` and `issue` actions. They spawn/supervise processes and drive git/gh and
  worktree removal — higher blast radius, deferred behind a deliberate later phase.
- The MCP server auto-spawning or keeping `devkitd` warm. The facades already use a
  running daemon transparently (see "Daemon relationship"); a dedicated warm-keeper
  is a separate, deferred concern.
- HTTP/SSE transport. stdio only — every target host launches local MCP servers over
  stdio.
- Adopting the `rmcp` async SDK. The v1 protocol surface is small enough to implement
  directly and the workspace is deliberately sync (see "Runtime").

## Background — what the server builds on

The library facades and daemon transport already exist (the CLI-rename and
authoritative-in-memory work is merged):

- **Ports facade** (`crates/devkit-ports/src/registry.rs`): `snapshot`, `alloc`,
  `record_pid`, `release`, `prune`, `status_table`.
- **Locks facade** (`crates/devkit-locks/src/lib.rs`): `acquire`, `check`, `release`,
  `release_all`, `status`, `prune` — plus the lower-level `*_with` store ops in
  `crates/devkit-locks/src/store.rs` that take an explicit `root` and `holder`
  instead of resolving them from CWD/process identity.
- **JSON framing** (`crates/devkit-common/src/daemon/framing.rs`): newline-delimited
  `serde_json` `send`/`recv`. MCP over stdio is also newline-delimited JSON, so the
  same approach applies.
- **Identity resolution** (`crates/devkit-locks/src/ident.rs`): the CLI resolves a
  `holder` from `--as` → `$DEVKIT_SESSION` → tmux/tty/ppid. For a long-lived MCP
  server, process identity is meaningless, so holder is handled differently (see
  "Identity and targeting").

Two facts from the codebase drive the design:

1. **No async runtime exists.** The workspace is fully sync (blocking sockets,
   `fd-lock`, `panic=abort`, stripped release binaries, ~13 lean deps). The MCP
   server stays sync to match.
2. **The facades resolve project root from CWD and holder from process identity.**
   A long-lived server has neither in a meaningful sense, so `root` becomes an
   explicit per-call input and `holder` is bound to the server's session.

## Architecture

### Crate and binary layout

- **New library crate `crates/devkit-mcp`** holds all testable logic: the JSON-RPC
  protocol loop, the action registry, argument validation, and the dispatch handlers.
- **New thin binary `src/bin/devkit-mcp/main.rs`** wires stdin/stdout to the protocol
  loop and installs `report::install_panic_hook`. No logic beyond wiring.

This follows the workspace convention (libraries in `crates/`, binaries in `src/bin/`)
and keeps the protocol logic unit-testable without spawning a process —
`cargo test --workspace` is the merge gate.

**Binary name:** `devkit-mcp`.

### Runtime: hand-rolled sync JSON-RPC over stdio

A blocking read-loop on stdin processes one JSON-RPC 2.0 request at a time and writes
one response per request to stdout, newline-delimited, reusing `serde_json` and the
framing approach already in `devkit-common`. No tokio. Handlers call the blocking
library facades directly; serialized request handling means no concurrency concerns
inside the server.

Protocol methods implemented in v1:

- `initialize` — returns server info and capabilities (`tools`).
- `tools/list` — returns the two tools and their input schemas.
- `tools/call` — dispatches to `devkit_describe` or `devkit_call`.

Notifications that require no response (e.g. `notifications/initialized`) are accepted
and ignored.

### The two tools

- **`devkit_describe(action?)`** — progressive disclosure.
  - No `action`: returns an array of `{ action, summary }` for every registered action.
  - With `action`: returns the full JSON Schema for that action's arguments.
- **`devkit_call(action, args)`** — validates `args` against the registered schema for
  `action`, dispatches to the handler, returns a structured result.

### The action registry — single source of truth

One in-memory registry maps each action name to an entry:

```
ActionEntry {
    name:    &str,            // e.g. "locks.acquire"
    summary: &str,            // one-line description, shown by describe()
    schema:  serde_json::Value, // JSON Schema for the args object
    handler: fn(Args) -> Result<serde_json::Value>,
}
```

`devkit_describe` reads `summary`/`schema`; `devkit_call` validates against `schema`
then invokes `handler`. Adding `devrun`/`issue` later is purely additional registry
entries — the tool shape never changes. Action names use the `binary.action`
convention (`ports.alloc`, `locks.acquire`).

## Action catalog (v1)

| Action | Args | Root | Facade call |
|---|---|---|---|
| `ports.status` | — | — | `registry::snapshot` + `registry::status_table` |
| `ports.alloc` | `apps[]`, `role?` | **required** (load app catalog from `devkit.toml`) | `registry::alloc(holder, reqs, role)` |
| `ports.release` | `role?` | — | `registry::release(holder, role)` |
| `ports.prune` | — | — | `registry::prune` |
| `locks.acquire` | `paths[]`, `note?`, `ttl?` | **required** | `acquire_with(root, holder, paths, note, ttl)` |
| `locks.check` | `paths[]` | **required** | `check_with(root, holder, paths)` |
| `locks.release` | `paths[]?`, `all?`, `force?` | **required** | `release_with` / `release_all_with` |
| `locks.status` | `all?` | required unless `all` is set | `status` (current root) / all-projects |
| `locks.prune` | — | — | `prune` |

**Argument detail:**

- `role` is the `Role` enum (`issue` | `baseline`), default `issue`, matching the CLI.
- `ttl` is seconds; default `1800` (30 min), `0` = no expiry — matching `lockm acquire`.
- `apps` is a list of app names; `ports.alloc` resolves each name to its base port from
  the project's `devkit.toml` catalog, then calls `registry::alloc` with the resolved
  `(app, base_port)` reqs. This is why `ports.alloc` needs `root` even though the port
  registry itself is global.
- `locks.release` requires either `paths[]` or `all: true` (mirrors `lockm release`).

**Root applicability:** `root` is required by all `locks.*` actions and by
`ports.alloc` (catalog lookup). `ports.status`, `ports.release`, `ports.prune`, and
`locks.prune` are global and take no `root`. `locks.status` needs `root` unless `all`
is set (all-projects view).

## Identity and targeting

- **`root`** — an explicit argument the agent supplies on each action that needs it.
  No CWD inference. When required and absent, the call fails with a clear error rather
  than guessing a repo. Lock actions route through the `*_with` store ops so the
  explicit root is honored.
- **`holder`** — the server mints a stable holder at startup: `$DEVKIT_SESSION` if set,
  otherwise a generated stable id. One stdio server process maps to one agent session,
  so this id is stable for the session's life. The agent may override `holder` per call.
  Because acquire and release both use the same server-bound holder by default, a
  release always matches its acquire.

## Daemon relationship

MCP handlers call the **facades**, which already try `devkitd` first and fall back to
the direct flock path when no daemon is running. The MCP server therefore benefits
from a running daemon transparently and contains no daemon-transport code of its own.
v1 does **not** auto-spawn `devkitd`. If a daemon holds the registry lock and a write
arrives, the existing `DaemonHoldsLock` handling in the facades applies unchanged and
surfaces as an action error (see below).

## Error and conflict semantics

- **Protocol-level failures** — malformed JSON-RPC, unknown method, unknown action, or
  arguments that fail schema validation → a JSON-RPC **error response**.
- **Action ran but failed** — a facade returning an `anyhow` error → a `tools/call`
  result with `isError: true` and the full error chain (`{e:#}`) as text content. This
  keeps execution failures inside the tool-result envelope where the agent can read them.
- **Lock conflict is a normal result, not an error.** `locks.acquire` and `locks.check`
  return structured conflict data (`path`, `held_by`, `age_secs`, `note`) as a success
  result, so the agent can branch on contention. This is the agent-facing upgrade over
  the CLI's exit-code-1-on-conflict.

All structured results are returned as JSON (serialized facade output types:
`AcquireOutcome`, `Conflict`, `LockEntry`, the ports `Data`/`Entry` snapshot, freed-port
lists, etc.) so the agent receives machine-readable data, not rendered tables.

## Distribution and packaging

The server ships in the **same multi-agent plugin** as the `using-devkit` skill (see
`docs/superpowers/specs/2026-06-21-multi-agent-skill-packaging-design.md`). Each host
plugin manifest gains an MCP-server registration pointing at the `devkit-mcp` binary,
which installs via `cargo install --path .` alongside the other binaries. The exact
per-host wiring (a `.mcp.json` entry vs an `mcp`/`mcpServers` block in the plugin
manifest, and how the binary path is resolved on each host) is verified against each
agent's current plugin spec during the plan phase — the same "verify live details
against current specs" stance the packaging plan used.

Version sync: `devkit-mcp` is part of the workspace, so its crate version follows the
existing release-please flow with the other crates. No new version source.

## Testing

TDD throughout; `cargo test --workspace` is the gate. Unit tests, no process spawning:

- **Registry / describe:** `devkit_describe()` lists all 9 actions; `devkit_describe(a)`
  returns the registered schema for each.
- **Argument validation:** valid args pass; missing-required-`root`, wrong types, and
  unknown actions are rejected with the expected error.
- **Handlers:** each action against a temp state dir (the facades already support this
  in their existing tests), asserting the structured result shape and that mutations
  land in the registry/store.
- **Conflict result:** acquiring an already-held path returns a conflict result with
  `isError: false` and the expected `held_by`/`age_secs` fields.
- **Protocol loop:** feed `initialize`, `tools/list`, and `tools/call` frames into the
  loop and assert the JSON-RPC frames written out, including the `isError` envelope for
  a failing action.

## Sequencing

The rename and authoritative-in-memory work is merged, so there is no upstream gate.
Work proceeds in the `mcp-server` worktree and lands as a PR to `main`.

## Resolved decisions

1. **v1 surface** — ports + locks, full (9 actions), read + mutate. `devrun`/`issue`
   deferred.
2. **Tool shape** — meta-MCP: `devkit_describe` + `devkit_call`, backed by one action
   registry. Stays two tools as the surface grows.
3. **Project targeting** — `root` is an explicit per-call argument; no CWD inference.
4. **Holder identity** — server mints from `$DEVKIT_SESSION` or a generated stable id;
   agent may override.
5. **Runtime** — hand-rolled sync JSON-RPC over stdio; no tokio, no `rmcp`.
6. **Crate/bin** — new `crates/devkit-mcp` lib + thin `src/bin/devkit-mcp` binary,
   named `devkit-mcp`.
7. **Daemon** — call facades (daemon-or-flock transparent); no auto-spawn in v1.

## Open questions

Verified during the plan phase, none blocking:

- The exact per-host MCP-server registration wiring (`.mcp.json` vs manifest block) and
  binary-path resolution for Claude Code, Codex, and Cursor.
- The precise JSON-RPC `initialize` capability/`serverInfo` fields the target hosts
  expect, confirmed against the current MCP spec the hosts implement.
