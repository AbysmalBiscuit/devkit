# File Locking (`lock`) — Design

**Status:** approved design, pre-plan
**Date:** 2026-06-20

## Goal

Give parallel local sessions a systematic way to coordinate file ownership by
acquiring advisory locks on paths, instead of agents hand-writing ad-hoc markdown
`.lock` files. The motivating case: projects (e.g. Godot) where per-session git
worktrees are too expensive, but multiple sessions still edit the same checkout
concurrently and must not stomp each other's files.

This is **advisory coordination between cooperating sessions** — a registry of
"I'm working on these paths" claims — not OS-level mandatory locking of the files
themselves.

## Scope

A single new subsystem, structurally a twin of the existing `portman` port
registry: a flock-guarded JSON registry of exclusive claims, keyed by **path**
instead of **port**, with liveness-based pruning, a status table, and a small CLI.

Out of scope for v1 (captured in `docs/next-steps.md`): routing through the
`devkit-portd` daemon, and unifying the two flock'd-JSON stores into one shared
helper.

## Architecture

### New crate `crates/devkit-locks` (library)

Keeps naming honest — file-claims are not ports — and reuses the *pattern* of the
port registry without overloading the `devkit-ports` crate.

- `store.rs` — flock'd-JSON store: ensure dir → exclusive `fd_lock` → read-or-default
  (salvage then back up on unreadable input) → atomic temp-file rename write. Mirrors
  the private plumbing in `devkit-ports/src/registry.rs`.
- `model.rs` — `LockEntry`, `Data`, and the pure logic (`acquire`, `release`,
  `prune`, overlap detection). No I/O; fully unit-testable.
- `ident.rs` — session-identity and anchor-PID resolution.
- `lib.rs` — project-root detection, path normalization, and the public ops the CLI
  calls.

### CLI at `src/bin/lock.rs`

Thin, single-file (like `src/bin/portman.rs`): parse args, call the crate, format
output. The root `devkit` package gains a `devkit-locks` dependency. `lock` is a
plain binary with **no** `daemon` feature gate, so `cargo install --no-default-features`
still ships it. This makes devkit **five binaries**: `portman`, `devrun`, `issue`,
`lock` (user-facing) plus the `devkit-portd` daemon.

## State home — agent-neutral, with migration

devkit is agent-agnostic, so its state must not live under `~/.claude/`. The
canonical state home becomes the XDG state dir, mirroring how `cache_dir()` already
honors `$XDG_CACHE_HOME`:

- Canonical: `$XDG_STATE_HOME/devkit`, default `~/.local/state/devkit`.
- **Migration with fallback:** on first access, if the canonical dir is missing and
  the legacy `~/.claude/state/devkit` exists, migrate it (directory rename,
  best-effort, idempotent). If the rename cannot complete (e.g. cross-device
  `EXDEV`, permissions), devkit keeps using the legacy dir in place so live state is
  never orphaned.

This is a single change to `state_dir()` in `devkit-common/src/paths.rs`; every
derived path moves with it — `ports.json`, `ports.lock`, `logs/`, the daemon
socket/lock/log, and the new `locks.json`/`locks.lock`. The port registry is
migrated by the same mechanism. Path unit tests and the README "State & Cache
Locations" table are updated to the new home.

New path helpers: `locks_file()` → `state_dir()/locks.json`, `locks_lock()` →
`state_dir()/locks.lock`.

## Data model — one registry, per-project scoping

A single global file `locks.json` (guarded by `locks.lock`), each entry carrying its
project `root` so projects never collide and `status` filters by the current root.

```rust
struct LockEntry {
    path: String,          // project-root-relative, normalized
    root: String,          // absolute project root
    holder: String,        // resolved session id
    pid: Option<u32>,      // durable anchor pid, best-effort
    note: Option<String>,  // optional human-readable intent
    ts: u64,               // acquired/renewed at, unix secs
    ttl: u64,              // seconds; 0 = no expiry
}

struct Data {
    version: u32,
    locks: BTreeMap<String, LockEntry>,  // key = format!("{root}\0{path}")
}
```

Locks are **exclusive** — a path is held by at most one holder.

### Project-root detection & path normalization

- Root = nearest ancestor directory of the cwd containing a `.git` entry
  (dependency-free walk-up); if none is found, root = cwd.
- Each input path is resolved to absolute, then stored relative to the root and
  normalized (lexically; `.`/`..` collapsed, no symlink resolution required).

### Overlap detection

Path `P` conflicts with an existing claim `Q` (same root) when `P == Q`, or one is a
**path-component ancestor** of the other. Comparison is component-wise so `scenes`
never matches `scenes-old`. This lets a session lock a whole directory
(`scenes/level1/`) and have it conflict with a file under it (`scenes/level1/player.tscn`),
and vice versa — matching the Godot "scene + script + resources" bundle case.

## Identity & liveness

### Identity precedence

`--as <id>` → `$DEVKIT_SESSION` → `$TMUX_PANE` → controlling tty → `$PPID`.

`$TMUX_PANE` is the key zero-config anchor: tmux exports a stable, unique pane id
into every shell and child process in a pane, so parallel panes get distinct ids
automatically and it stays stable across calls within a pane. `--as` and
`$DEVKIT_SESSION` both work for explicit control (the agent or wrapper sets one).

### Anchor PID (opportunistic)

A durable anchor PID is recorded **only** when one can be trusted:

- Under tmux (`$TMUX_PANE` set): the pane's process via
  `tmux display-message -p -F '#{pane_pid}'`.
- Otherwise a live interactive `$PPID`.

For the agent-via-Bash path the parent is an ephemeral per-call shell, so **no** PID
is recorded — otherwise pruning would kill the lock the instant the call returns.
Those sessions rely on TTL + explicit release.

### When a lock is dead

A lock is dead when **its anchor PID is known and gone, OR its TTL has lapsed.**

- Default TTL **30 minutes**; override with `--ttl <dur>` (`0` = no expiry, rely on
  PID/explicit release only).
- Re-`acquire` of a path already held by the same holder is **idempotent and renews**
  `ts` (no separate renew command).
- `release` always works regardless of TTL/PID.

Liveness is probed on a snapshot **outside** the registry lock, then removals are
committed under the lock — the same discipline the port registry uses to keep the
exclusive lock free of blocking syscalls.

## CLI surface

```
lock acquire <paths…> [--as <id>] [--note <msg>] [--ttl <dur>] [--json]
lock release <paths…> [--as <id>]          # or: release --all  (your locks, this project)
lock check   <paths…> [--json]             # read-only: would acquire succeed?
lock status  [--all] [--json]              # this project; --all spans all projects
lock prune                                 # drop expired/dead locks
lock completions <shell>
```

- **Conflict = fail fast.** `acquire` and `check` exit **1** when any requested path
  is held by another live holder, reporting holder, age, and note. There is no
  `--wait`.
- **Exit codes:** `0` success, `1` conflict, `2` usage error.
- `release` refuses to free another holder's lock unless `--force`.
- `acquire`, `status`, and `prune` auto-prune dead+expired entries first.
- Acquiring multiple paths is **all-or-nothing**: if any conflicts, none are taken
  and the command reports every conflict.

### Agent-facing `--json`

The ergonomic win over markdown `.lock` files — structured, branchable output:

```jsonc
// acquire success
{ "ok": true, "acquired": [{ "path": "scenes/level1", "ttl_secs": 1800 }] }

// acquire / check conflict
{ "ok": false, "conflicts": [
    { "path": "scenes/level1", "held_by": "feat-x", "age_secs": 412,
      "note": "refactoring player controller" }
] }
```

## Error handling

- `install_panic_hook("lock")` like the other binaries.
- Recoverable errors carry an `anyhow` context chain.
- An unreadable registry is salvaged where possible, otherwise backed up to
  `locks.json.bak` and reinitialized — the same policy as `registry.rs`.

## Testing (TDD)

**Unit (`devkit-locks`):**
- overlap detection — exact, ancestor, descendant, and sibling-prefix
  (`scenes` vs `scenes-old`) non-match
- idempotent re-acquire renews `ts`, does not duplicate
- TTL expiry marks a lock dead; `ttl = 0` never expires
- release-by-holder frees only that holder's locks; refuses others without `--force`
- prune drops a dead-PID entry; keeps a live one
- identity precedence resolution (`--as` > `$DEVKIT_SESSION` > `$TMUX_PANE` > tty >
  `$PPID`) driven by env manipulation
- project-root detection (`.git` walk-up; cwd fallback) and path normalization

**Integration (`tests/`):**
- second-holder `acquire` of a held path exits 1 with holder info
- `--json` success and conflict shapes
- `release` frees a held path so a different holder can then acquire it
- a `lock` case added to the existing `tests/completions.rs`

## Deliverables beyond code

- **`docs/next-steps.md`** — add: route `lock` acquire/release/status through
  `devkit-portd`; and unify the two flock'd-JSON stores into one shared helper.
- **README** — new "## File locks" section; install list and binary count updated to
  five; "State & Cache Locations" table updated to the new state home.
- **CLAUDE.md** — binary count/layout updated; agent lock-usage guidance (acquire
  before editing shared files, branch on exit code / `ok`, release when done).

## Open questions

None outstanding — TTL default (30m), single global `locks.json`, the `lock` binary
name, and the agent-neutral state home with ports migration are all settled.
