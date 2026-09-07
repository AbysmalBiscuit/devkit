---
name: using-devkit
description: "Use before editing files in a checkout several agents or sessions share (claim them first), before running a project build or verification command by hand (a canned `devrun task` may already have the ports and env wired in), when running local dev servers or allocating ports, or when setting up issue worktrees and shipping them for review. Covers the devkit CLIs: `devkit`, `devrun`, `issue`, `lockm`, `portm`."
allowed-tools: Bash(devkit:*), Bash(devrun:*), Bash(issue:*), Bash(lockm:*), Bash(portm:*), Bash(rg:*), Bash(ast-grep:*), Glob, Grep, mcp__devkit__devkit_describe, mcp__devkit__devkit_call
disable-model-invocation: false
user-invocable: true
---

# Using devkit

`devkit` coordinates many concurrent local dev sessions, human and agent, on one machine. The engine is project-agnostic; every project-specific detail lives in `devkit.toml`.

Every command spells two ways: `portm status` and `devkit ports status` run the same code, reachable through hardlinks `devkit install-links` creates. Each subcommand has `-h`, and when these docs and a `-h` disagree, the `-h` wins.

Config keys resolve the same way. `devkit schema` prints the JSON Schema derived from the config types, so it holds every key's name, type and default. Dump it when a key is in question instead of trusting a reference doc to be current.

## Check for a canned task first

Before running a project build, profiling flow, or verification command by hand, run `devrun task` and look for one already configured. A task renders **real registry ports** into its command, so a hand-typed port is exactly the drift devkit exists to prevent.

## Reaching the details

| Reference | Reach for it when |
|---|---|
| `references/locks.md` | full `lockm` flags, holder identity, TTL, or an enforced-write hook denied your edit |
| `references/servers.md` | starting or stopping dev servers, allocating ports, reading logs, another worktree's servers |
| `references/tasks.md` | a `devrun task` hit a `require_live` gate, or you need to override its env |
| `references/issues.md` | starting an issue worktree, checking out a PR, shipping for review, tearing down |
| `references/diagnostics.md` | a `devkit.toml` key's name, type or default is in question, a credential is missing, or you need `doctor` or `brief` |

Global flags go **before** the subcommand (`issue -C ~/git/acme/app status`): `-C/--dir <path>` on `issue`/`devrun`/`portm`, `--config <file>` and `--timing[=trace]` / `--timing-log <FILE>` on `issue`/`devrun`.

## Claim before you edit

In a shared checkout, claim every file you will touch, then release when done. Locks are **advisory** — they coordinate cooperating sessions rather than enforcing at the filesystem level. Respect them.

**1. Look at the board, then claim everything in one call.** Your holder id is detected from the coding-agent session you are running in, so acquire and release already agree with each other and with the write hook. `acquire` is all-or-nothing: it claims every path, or if *any* is held it claims none and exits non-zero.

```sh
lockm status                                  # who holds what right now
lockm acquire src/auth/session.rs src/auth/mod.rs --note "refactoring auth session"
echo $?                                       # 0 = you hold them; 1 = conflict
```

Lock a directory (`src/auth/`) to claim a subtree, or individual files for finer-grained sharing.

**2. Branch on the exit code.** It is a gate, not a formality.

- **Exit 0** (`locked …`) — you hold the paths. Edit them.
- **Exit 1** (`conflict: …`) — another session holds one. Edit something else.

**3. Release once the edit *and* its verification are done.** Others may be waiting.

```sh
lockm release src/auth/session.rs src/auth/mod.rs
lockm release --all                           # or: drop everything you hold
```

### When a claim conflicts

`acquire` and `check` print the holder, age, and note:

```
conflict: 1 path(s) held by another session:
  src/auth/mod.rs held by agent-bob (12s ago) — wiring new endpoint
```

Work on an unblocked file first, then poll with `lockm check <paths>` (read-only, takes no claim) and re-run `acquire`. When a holder looks stuck, `references/locks.md` covers the TTL, `prune`, and when `release --force` is the right answer.

## Enforced checkouts

Some checkouts turn on write enforcement, where the plugin's `PreToolUse` hook auto-locks each file on your first `Edit`/`Write` and releases at session end. Acquiring manually there is redundant for `Edit`/`Write`, which the hook already covers, and it is safe: `lockm` resolves the same session id the hook does, so your own claims are recognised as your own. Claim by hand only for a `Bash` write the hook never sees.

What changes is the failure: a blocked write comes back as a **deny** naming the holder. Treat that exactly like an `acquire` conflict — edit a different file, or wait. `Bash` writes are not covered, so a `sed` or heredoc edit slips past the hook; claim the path with `lockm acquire` before editing that way. `references/locks.md` has the full mechanism, including how a checkout turns enforcement on.
