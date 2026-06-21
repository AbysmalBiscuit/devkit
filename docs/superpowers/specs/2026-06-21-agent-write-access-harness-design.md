# Agent write-access harness for devkit

## Goal

Turn devkit's **advisory** file-lock registry into an **enforced** one for coding
agents, by shipping a blocking write hook in the existing devkit agent plugin.
Today an agent is *told* (via the `using-devkit` skill) to run `lockm acquire`
before editing a shared checkout, but nothing stops one that forgets — two
sessions, or two parallel sub-agents, can still clobber the same file. The
harness intercepts every structured write tool call and blocks it when the
writing agent does not own the file, auto-acquiring the lock when the file is
free.

The harness covers **writes only**. Read gating is an explicit non-goal (see
Non-goals).

## Background — what already exists

- **`devkit-locks` / `lockm`** — a flock'd (or `devkitd`-served) JSON registry of
  file locks keyed by a holder id. `acquire`/`check`/`release`/`status`/`prune`.
  Locks are advisory: callers cooperate by convention.
- **devkit agent plugin** — `.claude-plugin/` ships the `using-devkit` skill;
  `hooks/` already does SessionStart context injection for Codex/Cursor via
  `announce-skill` + a Windows-safe `run-hook.cmd` runner shim. (See
  `2026-06-21-multi-agent-skill-packaging-design.md`.)
- **`devkit-mcp`** — a meta-MCP server exposing the port + lock facades to agents
  as *tools*. This is a separate surface; the harness does **not** depend on it.

The harness extends the plugin with new hooks and adds a hook-handler subcommand
to `lockm`. It introduces no new crate.

## Non-goals

- **Read gating.** Reads stay open. The model is "you may only *modify* files you
  own", not confidentiality or scope isolation. No shared/exclusive lock modes.
- **`Bash` write coverage.** `echo >`, `sed -i`, `cp`, redirections, etc. cannot
  be parsed from a tool payload reliably, so they are outside the harness. This is
  a documented gap, not a silent hole — the harness governs the structured write
  tools only.
- **Codex/Cursor wiring in this round.** Claude Code first; the enforcement logic
  is kept agent-agnostic so the later port is wiring, not a rewrite (see Agent
  scope).
- **Replacing the advisory CLI workflow.** Manual `lockm acquire`/`release` still
  works and interoperates with the harness (same holder id).

## Activation — opt-in per checkout

The plugin installs globally, but a blocking hook that fires on every edit must
**not** silently engage in every repository. The harness activates only when the
checkout opts in.

- **Default: off.** Installing the plugin never changes write behavior. Where the
  harness is not enabled, the hook exits `0` immediately, before any registry
  work — zero overhead in ordinary repos.
- **Opt-in marker (leading candidate):** a `[harness] enforce_writes = true` flag
  in `devkit.toml`, the file that already gates project-specific behavior. The
  plan settles the exact marker (the alternative is an env var such as
  `DEVKIT_ENFORCE_WRITES`); `devkit.toml` is preferred because enforcement is a
  property of the checkout, not the shell.

## Ownership rule (the core)

On a write to file `F` by an agent with holder id `H`, the hook resolves the lock
registry and decides:

| Registry state for `F`                          | Decision                     |
|-------------------------------------------------|------------------------------|
| free (no live holder)                           | **auto-acquire as `H`**, allow |
| held by `H` (self)                              | allow                        |
| held only by an **ancestor** of `H`             | allow                        |
| held by any other live agent (sibling / unrelated) | **deny**, naming the holder |

**Auto-acquire.** The agent never thinks about `lockm`; the hook claims the lock
on first write and allows the edit. Locks release automatically (see Lifecycle).

**The ancestor clause** is what makes cross-session and intra-session contention
one rule: *a write is allowed iff every live holder of `F` is an ancestor-or-self
of the writer.* In Claude Code a parent agent is **suspended while its sub-agent
runs**, so a parent and its own descendant are never truly concurrent — letting a
child write a file its parent holds is safe and avoids a within-session deadlock.
Two **parallel** sub-agents are siblings, not ancestors, so they correctly block
each other, as do two unrelated top-level sessions.

## Holder identity

The hook derives the holder id from the agent's runtime context; the agent never
passes `--as`. The id is **composite and ancestry-encoding** so the ancestor test
is a path-prefix check:

```
S            top-level session
S/a1         sub-agent a1 of S
S/a1/b2      sub-agent b2 of a1
```

`X` is an ancestor of `Y` iff `X` is a path-prefix segment boundary of `Y`
(`S` is an ancestor of `S/a1`; `S/a1` is not an ancestor of `S/b2`).

The hook also exports this value as `$DEVKIT_SESSION`, so any **manual** `lockm`
call the agent makes resolves to the **same holder** — no self-contention between
the hook and explicit CLI use.

**Key verification task (plan phase):** confirm exactly what Claude Code's
PreToolUse payload exposes for sub-agents — whether `session_id` differs per
sub-agent and whether a parent pointer exists, or only `transcript_path` is
available to reconstruct ancestry. The rule is sound; this derivation is the open
detail. The handler is fed canned payloads in tests, so the derivation is
isolated behind a parse step.

## Lifecycle

- **PreToolUse** (`Edit`, `MultiEdit`, `Write`, `NotebookEdit`) → the ownership
  check + auto-acquire above.
- **SubagentStop** → release locks held by that sub-agent's holder id.
- **SessionEnd** → release locks held by the session's holder id, including any
  orphaned descendant segments.
- **Backstop:** the existing `lockm prune` / reservation-grace machinery still
  reclaims locks from a crashed agent that never fired its stop hook.

## Implementation stance — logic in `lockm`, not bash

The hook entrypoint is a new **`lockm hook <event>`** subcommand that reads the
JSON hook payload on stdin and writes the hook-decision JSON on stdout. The
`hooks/*` files are thin shims that invoke it.

Rationale: the ancestor-or-self logic is non-trivial and belongs in unit-tested
Rust, not jq-parsing bash; it reuses the `devkit-locks` facade directly instead
of re-entering its own CLI; and it stays cross-platform with no jq dependency.
The hooks stay declarative; the decision logic is testable.

A new `lockm check` mode may be needed if the ancestor-or-self distinction cannot
be expressed with today's `acquire`/`check` exit codes — but since the logic now
lives inside `lockm hook`, it can call the facade directly and likely needs no
new public CLI verb. Settled in the plan.

## Components / files

Extends the existing plugin, mirroring the `announce-skill` hook pattern.

```
src/bin/lockm.rs            EDIT — add `hook <event>` subcommand (pretooluse | stop)
crates/devkit-locks/        EDIT — ancestor-or-self decision + holder-tree release,
                                   exposed through the facade; unit-tested
hooks/
  enforce-write            NEW  — shim: run-hook.cmd → `lockm hook pretooluse`
  release-locks            NEW  — shim: run-hook.cmd → `lockm hook stop`
  run-hook.cmd             EXISTS — Windows-safe runner shim, reused
.claude-plugin/
  (hooks wiring)           NEW/EDIT — PreToolUse + SubagentStop + SessionEnd
skills/using-devkit/SKILL.md   EDIT — document the enforced mode + opt-in flag
docs/configuration.md          EDIT — the [harness] enforce_writes flag
```

The enforcement brains live in one place (`lockm hook` over the `devkit-locks`
facade), so a later Codex/Cursor port is hook wiring only, not a logic rewrite.

## Failure modes

- **devkit / `lockm` not on PATH** → shim exits `0` (allow), logged. The harness
  can't enforce what isn't installed; bricking every write in a non-devkit repo is
  worse than a missed lock.
- **Harness disabled** (no opt-in marker) → exit `0` immediately, before any
  registry work.
- **Registry / daemon error mid-check** → **fail closed** (deny, with a clear
  reason). Once a checkout has opted into enforcement, a registry hiccup must not
  silently reopen the clobbering window. This is the deliberate difference between
  "harness off" (open) and "harness on but broken" (closed).
- **Malformed or absent `file_path`** in the payload → allow (not a structured
  write the harness governs).

## Agent scope

**Claude Code first, designed for portability.** Ship and prove the PreToolUse /
SubagentStop / SessionEnd hooks on Claude Code (richest deny semantics). Because
the decision logic is in `lockm hook` and the hooks are thin shims, adding Codex
and Cursor later is a matter of wiring their hook envelopes to the same
subcommand — verified against each agent's then-current spec.

## Testing

`cargo test --workspace` is the merge gate.

- **Decision logic (unit):** self / ancestor / sibling / free → allow / deny /
  auto-acquire, fed canned payloads including a real captured Claude Code
  PreToolUse JSON.
- **Ancestor-prefix matching:** `S/a` may write a file held by `S`; `S/a` is
  denied a file held by `S/b`; `S` is denied a file held by `S/a` (a parent does
  not own a descendant's lock).
- **Lifecycle:** SubagentStop releases only the sub-agent's segment, leaving the
  parent's locks intact; SessionEnd releases the whole tree.
- **Multiprocess race:** in the spirit of the existing `devkit-ports::registry`
  flock test — two holders concurrently acquire the same path; exactly one wins.
  Poll for state (no fixed sleeps), per the CI convention.
- **Manual end-to-end** (the one thing unit tests can't stand in for): two Claude
  Code sessions on one checkout, then a parent + parallel sub-agents, confirming
  block/allow and auto-release on a real install. Test on Windows (the dev
  platform; the runner shim must work there).

## Resolved decisions

1. **Scope** — writes only; reads stay open; no shared/exclusive modes.
2. **Unlocked-file behavior** — auto-acquire as the agent, then allow.
3. **Contention model** — both cross-session and intra-session (parallel
   sub-agents), unified by the ancestor-or-self ownership rule.
4. **Release timing** — SubagentStop releases the sub-agent's locks; SessionEnd
   releases the session's; `lockm prune` is the crash backstop.
5. **Activation** — opt-in per checkout, default off (leading marker:
   `[harness] enforce_writes` in `devkit.toml`).
6. **Implementation** — logic in a `lockm hook` subcommand over the `devkit-locks`
   facade; `hooks/*` are thin shims; no new crate.
7. **Failure stance** — fail open when the harness is off or devkit is absent;
   fail closed on a registry error when the harness is on.
8. **Agent scope** — Claude Code first, logic kept agent-agnostic for a later
   Codex/Cursor port.

## Open questions (for the plan phase)

1. **Sub-agent identity derivation** — exactly which fields of the Claude Code
   PreToolUse / SubagentStop payload yield a stable, ancestry-encoding holder id.
   Does `session_id` differ per sub-agent, or must ancestry be reconstructed from
   `transcript_path` or a parent pointer? This is the single most important live
   detail to verify before implementation.
2. **Opt-in marker** — `devkit.toml` `[harness] enforce_writes` vs. an env var;
   confirm the `devkit.toml` loader is reachable from the hook's working directory
   (the session cwd, which is the checkout root).
3. **Hook-decision envelope** — the exact PreToolUse deny JSON Claude Code expects
   (decision/permission field names and the message surface the agent sees).
4. **Within-session re-acquire after SubagentStop** — confirm a parent that
   resumes after a sub-agent released a shared file can re-acquire it cleanly (no
   stale-holder false block).
