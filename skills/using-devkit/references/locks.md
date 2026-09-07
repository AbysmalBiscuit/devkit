# `lockm` — advisory file locks

`SKILL.md` carries the claim-before-you-edit workflow. This file is the lookup: every flag, the identity rules, and the enforced-write hook's mechanics.

## Commands

| Command | Purpose |
|---|---|
| `lockm acquire <paths…> [--note S] [--ttl SECS]` | Claim paths, all-or-nothing. Exit 1 if any is held. |
| `lockm check <paths…>` | Read-only: would `acquire` succeed? Takes no claim. |
| `lockm release <paths…>` / `lockm release --all` | Drop your claims. |
| `lockm status` / `lockm status --all` | Held locks for this project, or every project. Alias: `list`. |
| `lockm prune` | Drop expired or dead-session locks. |

`--json` works on `acquire`, `check`, and `status`.

## Holder identity

The holder id resolves in this order, first hit wins:

1. `--as <id>` on the call
2. the coding-agent session id (`$CLAUDE_CODE_SESSION_ID`, `$CODEX_SESSION_ID`)
3. `$DEVKIT_SESSION`
4. `$TMUX_PANE`
5. the controlling tty
6. the parent pid

Inside a coding-agent session this needs no setup: the detected id is the one the write hook holds locks under, so a manual claim and an automatic one are the same holder. `$DEVKIT_SESSION` still applies outside one. When two harnesses are nested and expose different ids, `acquire` and `release` refuse rather than guess, and name both so you can pass `--as`.

A claim made by hand inside a sub-agent is recorded at session granularity, because no harness exposes a sub-agent id to a subprocess. It blocks every other session and does not block sibling sub-agents of your own; that isolation comes from the write hook, which does have sub-agent ids. For the same reason `lockm release --all` inside a sub-agent frees the whole session's manual claims.

## TTL

Default 1800s (30 min). A lock auto-expires after that so a dead session cannot block the project forever.

- `--ttl 3600` for a long edit, or re-`acquire` to renew.
- `--ttl 0` means no expiry.
- `lockm prune` drops what has already expired.

## `release --force`

Takes a path from its current holder. Reserve it for a holder you have confirmed is dead and that `prune` did not clear. Forcing past a live holder clobbers their in-flight work.

## Enforced checkouts

Some checkouts turn on write enforcement, where the devkit plugin's `PreToolUse` hook owns the protocol.

Enforcement turns on from any of these, with the env var overriding the files:

- `DEVKIT_ENFORCE_WRITES=1` — machine-wide master switch (`0`/`false` forces off).
- `[harness] enforce_writes = true` in the global config (`$DEVKIT_CONFIG`, else `~/.config/devkit/config.toml`) — every checkout.
- `[harness] enforce_writes = true` in a checkout's own `devkit.toml` — that one.

Mechanics:

- **Auto-acquire on first write.** Before the first `Edit`/`MultiEdit`/`Write`/`NotebookEdit` to a file, the hook locks it for the session. Later writes to the same file by the same session, or by a sub-agent it delegates to, need no re-acquire.
- **Holder identity.** Top-level writes are held under the session id; sub-agent writes under `session_id/agent_id`. A parent holding a file implicitly covers its sub-agents.
- **A blocked write returns a deny** naming the holder:
  ```
  devkit write-harness: src/auth.rs (held by <holder>) — locked by another
  agent; coordinate or wait for it to finish
  ```
- **Automatic release.** Sub-agent locks release on `SubagentStop`; all session locks release on `SessionEnd`, whether that is a normal exit, Ctrl-C, or an error. The 30-min TTL backstops a hard kill.
- **`Bash` writes are not covered** — only the structured write tools above.
- **Fail-open when off or when `lockm` is absent.** The hook exits without blocking and takes no locks.
- **Fail-closed on registry errors.** With `lockm` present but the registry erroring (corruption, permissions), the hook denies the write rather than allowing it silently.
