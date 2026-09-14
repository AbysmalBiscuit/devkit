---
name: using-devkit
description: "Use when editing files in a checkout several agents or sessions share, when a write or shell command is denied naming another holder, when running a project build or verification command by hand, when starting local dev servers or allocating ports, and when setting up issue worktrees or shipping them for review. Covers the devkit CLIs: `devkit`, `devrun`, `issue`, `lockm`, `portm`."
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
| `references/tasks.md` | a `devrun task` needs an `--arg`, hit a `require_live` gate, or you need to override its env |
| `references/issues.md` | starting an issue worktree, checking out a PR, shipping for review, tearing down |
| `references/diagnostics.md` | a `devkit.toml` key's name, type or default is in question, a credential is missing, or you need `doctor` or `brief` |

Global flags go **before** the subcommand (`issue -C ~/git/acme/app status`): `-C/--dir <path>` on `issue`/`devrun`/`portm`, `--config <file>` and `--timing[=trace]` / `--timing-log <FILE>` on `issue`/`devrun`.

## Locks

Which mode a checkout is in decides everything below, so settle that first. `devkit brief` says so at session start: an enforced checkout's brief carries the sentence "Writes here are lock-enforced". Without it, locking is yours to drive. `devkit doctor` reports the same.

Locks are **advisory** in both modes. They coordinate cooperating sessions rather than enforcing at the filesystem level. Respect them.

### Enforced checkouts

The plugin's `PreToolUse` hooks claim a lock on each file a write touches, structured edits and resolvable shell writes alike, and release them when the session or sub-agent ends. Edit directly.

A blocked write comes back as a **deny** naming the holder. Move to another file, or wait for that session to finish.

A shell write is claimed automatically whenever devkit can trace its target back to a literal path: written straight into the command, assigned to a variable earlier in the same command, or reaching an inline `python3 -`, `node -e` or PowerShell script, either as an argument or as a literal inside the script's own body. A heredoc fed to a script counts as that body, so `python3 - <<'EOF'` writing to a path spelled out in the script is covered. When devkit cannot resolve a target (a path built at runtime, a script file, `perl -e`), the call is refused with the reason; rewrite the edit with an explicit path or use `Edit`/`Write`. Claiming some other path with `lockm acquire` does not get an unresolved write through.

One manual claim still earns its place here: `lockm acquire <dir>` over a subtree you are churning through takes a single row covering every path under it.

### Unenforced checkouts

Claim every file you will touch, then release when done.

**1. Look at the board, then claim everything in one call.** Your holder id is detected from the coding-agent session you are running in, so acquire and release already agree with each other. `acquire` is all-or-nothing: it claims every path, or if *any* is held it claims none and exits non-zero.

```sh
lockm status                                  # who holds what right now
lockm acquire src/auth/session.rs src/auth/mod.rs --note "refactoring auth session"
echo $?                                       # 0 = you hold them; 1 = conflict
```

Lock a directory (`src/auth/`) to claim a subtree, or individual files for finer-grained sharing.

**2. Branch on the exit code.** It is a gate, not a formality.

- **Exit 0** (`locked ...`, or `already held on this session line: ...` where a sub-agent of yours got there first) means you may edit the paths.
- **Exit 1** (`conflict: ...`) means another session holds one. Edit something else.
- **Exit 2** (`ambiguous session identity: ...`) means two nested harnesses expose different session ids and devkit refuses to guess. Re-run with `--as <id>`, choosing the **inner** harness's id.

**3. Release once the edit *and* its verification are done.** Others may be waiting.

```sh
lockm release src/auth/session.rs src/auth/mod.rs
lockm release --all                           # or: drop everything you hold
```

`release --all` also drops the write hook's automatic claims for this session, so it belongs at the end of a work unit rather than between edits.

### When a claim conflicts

`acquire` and `check` print the holder, age, and note:

```
conflict: 1 path(s) held by another session:
  src/auth/mod.rs held by agent-bob (12s ago) - wiring new endpoint
```

Work on an unblocked file first, then poll with `lockm check <paths>` (read-only, takes no claim) and re-run `acquire`. When a holder looks stuck, `references/locks.md` covers the TTL, `prune`, and when `release --force` is the right answer. It also has the full enforcement mechanism, including how a checkout turns it on.
