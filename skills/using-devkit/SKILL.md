---
name: using-devkit
description: Use when multiple agents or sessions share one local git checkout and edit files concurrently (coordinating who edits what without clobbering), or when running local dev servers, allocating ports, or managing issue worktrees with the devkit CLI suite — binaries `lockm`, `portm`, `devrun`, `issue`, `devkitd`.
---

# Using devkit

## Overview

devkit is a suite of CLIs that coordinate local development for a monorepo. The
engine is project-agnostic; project-specific details live in `devkit.toml`.

The part that matters when **several agents work in the same checkout at once** is
`lockm`: advisory file locks that let parallel sessions claim files before editing,
so two agents never overwrite each other's in-flight work. Locks are **advisory** —
they coordinate cooperating sessions; they do not enforce at the filesystem level.
Respect them.

## When to use

- You are one of several agents/sessions sharing **one working directory on disk**
  and about to edit files → claim them first (the workflow below).
- You need to run local dev servers, allocate/inspect ports, or set up and tear
  down issue worktrees → see Tool overview.
- You see `conflict: N path(s) held by another session` → another holder has the
  file; do not edit it (see Handling a conflict).

## The collaboration workflow (file locking)

This is the core. In a shared checkout, **claim before you edit, release when done.**

**1. Set one stable holder id per session.** `acquire` and `release` must use the
*same* id or you leak a lock that only clears on expiry. Export it once so every
call picks it up automatically (the `lockm` binary reads `$DEVKIT_SESSION`):

```sh
export DEVKIT_SESSION="agent-<your-role>"   # e.g. agent-auth-refactor
```

(Or pass `--as <id>` on every call — same id every time. Identity precedence:
`--as` > `$DEVKIT_SESSION` > `$TMUX_PANE` > controlling tty > parent pid.)

**2. Look at the board, then claim every file you'll touch in one call.** `acquire`
is **all-or-nothing**: it claims every path or, if *any* is already held, claims
none and exits non-zero. Claim related files together so you don't get a partial
hold and stall mid-edit:

```sh
lockm status                                  # who holds what right now
lockm acquire src/auth/session.rs src/auth/mod.rs --note "refactoring auth session"
echo $?                                       # 0 = you hold them; 1 = conflict
```

You may lock directories (`src/auth/`) to claim a whole subtree, or individual
files for finer-grained sharing.

**3. Branch on the exit code — it is a gate, not a formality.**

- **Exit 0** (`locked …`): you hold the paths. Edit them.
- **Exit 1** (`conflict: …`): see Handling a conflict. Do **not** edit those paths.

**4. Release as soon as you're done** (don't sit on locks — others may be waiting):

```sh
lockm release src/auth/session.rs src/auth/mod.rs
lockm release --all                            # or: drop everything you hold
```

### Handling a conflict

`acquire`/`check` print the holder, age, and note on a conflict:

```
conflict: 1 path(s) held by another session:
  src/auth/mod.rs held by agent-bob (12s ago) — wiring new endpoint
```

Then: work on an unblocked file first, or wait and re-run `acquire`. Use
`lockm check <paths>` (read-only, takes no claim) to poll. **Never** use
`lockm release --force` to take a path from a live holder — that defeats the entire
mechanism and clobbers their work.

### Long edits and stale locks

Default TTL is **1800s (30 min)**; a lock auto-expires after that so a dead session
doesn't block the project forever. For a long edit, raise it (`--ttl 3600`) or
re-`acquire` to renew. `--ttl 0` means no expiry. `lockm prune` drops expired/dead
locks.

## Lock command reference

| Command | Purpose |
|---|---|
| `lockm acquire <paths…> [--note S] [--ttl SECS]` | Claim paths (all-or-nothing). Exit 1 if any is held. |
| `lockm check <paths…>` | Read-only: would `acquire` succeed? No claim taken. |
| `lockm release <paths…>` / `lockm release --all` | Drop your claims. |
| `lockm status` / `lockm status --all` | Show held locks (this project / every project). |
| `lockm prune` | Drop expired or dead-session locks. |

Add `--json` to `acquire`/`check`/`status` for machine-readable output. Run
`lockm <cmd> --help` for full flags.

## Tool overview

| Binary | What it does |
|---|---|
| `lockm` | Advisory file locks for parallel sessions — the collaboration tool above. |
| `portm` | Port registry: `alloc`/`release`/`status`/`prune` dev-server ports without collisions. |
| `devrun` | Run and supervise local dev servers for a worktree: `up`, `down`, `status`, `logs`. |
| `issue` | Issue lifecycle: `setup` a worktree (branch, env, deps, reserved ports), `status`, `end`, `prs`, `dashboard`, `review`. |
| `devkitd` | Background daemon owning the port registry. Started automatically by `portm`/`devrun`; you rarely invoke it directly. |

Each user-facing CLI has `--help` on every subcommand and a `completions <shell>`
subcommand for shell completion.

## Enforced mode (automatic write locks)

When a checkout's `devkit.toml` sets:

```toml
[harness]
enforce_writes = true
```

the devkit plugin installs a `PreToolUse` hook that enforces write locks
automatically. **In an enforced checkout, agents do not call `lockm acquire` or
`lockm release` themselves — the harness owns the protocol.**

### How it works

- **Auto-acquire on first write.** Before the first `Edit`, `MultiEdit`, `Write`,
  or `NotebookEdit` that touches a file, the hook acquires a lock on that file on
  behalf of the session. Subsequent writes to the same file by the same session
  (or a sub-agent it delegates to) are allowed without re-acquiring.

- **Holder identity.** Top-level session writes are held under the session id.
  Sub-agent writes are held under `session_id/agent_id`. A parent session that
  holds a file implicitly covers its sub-agents — a sub-agent whose parent already
  owns a file can write it without contention.

- **A blocked write returns a deny.** When another session holds a conflicting
  lock, the hook denies the tool call and surfaces a message naming the holder:
  ```
  devkit write-harness: src/auth.rs (held by <holder>) — locked by another
  agent; coordinate or wait for it to finish
  ```
  The agent should wait for the other session to finish, or work on a different
  file.

- **Automatic release.** Locks acquired by a sub-agent are released when that
  sub-agent stops (`SubagentStop`). All locks held by a session are released when
  the session ends (`SessionEnd`), regardless of how the session exits (normal,
  Ctrl-C, or error). A TTL backstop (30 min by default) cleans up any locks that
  survive a hard kill.

- **`Bash` writes are not covered.** The harness intercepts only the structured
  write tools listed above. Shell-level writes made via `Bash` are outside its
  scope.

- **Fail-open when the harness is off.** In any checkout without the
  `enforce_writes = true` marker, the hook exits immediately without blocking any
  writes. No locks are taken and there is no overhead.

- **Fail-open when `lockm` is absent.** If the `lockm` binary is not on `PATH`,
  the hook invocation fails silently and the write proceeds. Install `lockm` via
  `cargo install --path .` to activate enforcement.

- **Fail-closed on registry errors.** If `lockm` is present but the registry
  returns an error (corruption, permission problem), the hook denies the write
  rather than allowing it through silently.

Manual `lockm acquire`/`release` calls remain the correct approach in checkouts
that do not set `enforce_writes = true`. In an enforced checkout there is no need
to call them — doing so is harmless but redundant.

## Common mistakes

- **Editing a shared file without acquiring it** → you may clobber another agent's
  in-flight work. Always `lockm acquire` first (in non-enforced checkouts).
- **`--force`-ing past a live holder** → defeats coordination. Wait or work
  elsewhere instead.
- **Mismatched `--as`/`$DEVKIT_SESSION` between acquire and release** → you can't
  release your own lock; it lingers until TTL. Set the id once and reuse it.
- **Acquiring files one at a time** → partial holds and stalls. Claim everything a
  unit of work touches in a single `lockm acquire`.
- **Forgetting to release** → blocks others until the TTL expires. Release as soon
  as the edit (and any verification) is done.
