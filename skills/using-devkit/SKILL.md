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
| `devrun` | Run and supervise local dev servers for a worktree: `up`, `down`, `status`, `logs`, `config`. |
| `issue` | Issue lifecycle: `setup` a worktree, `status`, `end`, `prs`, `dashboard`, `review`. |
| `devkitd` | Background daemon owning the port registry. Started automatically by `portm`/`devrun`; you rarely invoke it directly. |

**Full command and flag reference → `cli-reference.md`** (in this skill directory).
Each user-facing CLI also has `--help` on every subcommand. The workflow below is the
common path; reach for the reference when you need a specific flag.

## Dev-server & issue-worktree workflow

`issue` and `devrun` act on the **current working directory's worktree** by default
(override with `-C/--dir <path>`), and `issue review` ships the branch checked out
there. So `cd` into the right worktree first. The handoffs that aren't obvious from
per-command help:

**Start an issue → run its servers.** `issue setup` prints a JSON summary; read
`worktree` (where to `cd`) and `ports` (already reserved for you):

```sh
issue setup --issue ENG-123 --slug fix-auth --apps web,api
#  → {"issue":"ENG-123","worktree":"/abs/path/…","branch":"lev/eng-123-fix-auth","ports":{…}}
cd /abs/path/…                                # the printed worktree
devrun up web api                             # name apps explicitly — a fresh worktree has no diff to auto-detect
```

`devrun up` defaults to `--role issue` and reuses `setup`'s reserved ports. Selecting a
webapp pulls in `api` automatically.

**Stop your servers (without touching other worktrees).** `devrun down` stops servers
*and releases their ports*, scoped to **this worktree only** by default:

```sh
portm status                                  # who holds which ports (this project)
devrun down                                   # stop + release this worktree's servers
```

Reaching another worktree needs an explicit scope flag (`--all`/`--others`/`--holder`)
*and* an interactive terminal — an agent (no PTY) cannot stop another worktree's
servers. The holder is the **worktree root path**; get yours with
`git rev-parse --show-toplevel`.

**Ship for review.** `issue review` pushes (never force-pushes), opens/reuses the PR,
requests a reviewer, and Slacks them the link:

```sh
issue review "Auth fix ready — please review session handling." --to bob --reviewer octocat
```

See `cli-reference.md` for every flag of `setup`, `review`, `down`, and the rest.

## Enforced mode (automatic write locks)

Some checkouts turn on write enforcement, where the devkit plugin's `PreToolUse` hook
owns the lock protocol. **In an enforced checkout, do not call `lockm acquire`/`release`
yourself — the harness auto-locks each file on your first `Edit`/`Write` and releases
when the session (or sub-agent) ends.** Manual calls are harmless but redundant.

Enforcement turns on from any of (env var overrides the files):

- `DEVKIT_ENFORCE_WRITES=1` — machine-wide master switch (`0`/`false` forces off).
- `[harness] enforce_writes = true` in the **global** config (`$DEVKIT_CONFIG`, else
  `~/.config/devkit/config.toml`) — every checkout.
- `[harness] enforce_writes = true` in a **checkout's own** `devkit.toml` — that one.

What this means in practice:

- A blocked write returns a **deny** naming the holder — wait for them, or edit a
  different file.
- **`Bash` writes are not covered** — only structured write tools (`Edit`/`MultiEdit`/
  `Write`/`NotebookEdit`).
- When enforcement is off (or `lockm` isn't on `PATH`) the hook fails open and blocks
  nothing.

The full mechanism (holder identity, sub-agent delegation, release lifecycle, fail-open
vs fail-closed) is in `cli-reference.md`.

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
