# Lock holder identity — design

## Goal

One rule for who holds a lock, shared by the write hook, the `lockm` CLI and the MCP server, so a session's own claims are recognised as its own wherever they were taken.

Today two rules exist. The hook holds locks under the harness payload's `session_id`; `lockm` and the MCP server mint an id from the environment, which in an agent's shell falls through to the parent pid. A session that claims a file by hand is therefore refused when it writes that file through `Edit`, and the orphaned claim survives the session because the lifecycle sweep does not recognise it either. Alongside that, the lock lifecycle carries four defects that are independent of identity but sit in the same code and would be untestable around.

## Scope

- Harness session detection in `devkit-locks::ident`, with the harness-specific part as a table.
- Precedence for the detected id, and a refusal when nesting makes it ambiguous.
- `devkit-mcp::mint_holder` routed through the same resolution.
- `Data::check` aligned to the ancestry rule `decide_write` already uses.
- Lifecycle release moved ahead of the enforcement gate.
- Fail-closed handling for a write payload the hook cannot use.
- `SubagentStop` without an `agent_id` no longer releasing the whole session.
- Renewal of an ancestor lock by writes to paths underneath it.
- Runtime staleness detection for the table, surfaced by `devkit doctor`.
- A shared test-environment scrub helper, and the doc corrections.

### Non-goals

- **Cursor.** `hooks/hooks-cursor.json` registers no write hook and no lifecycle hooks, so there is no holder for `lockm` to agree with. Cursor needs a write hook before identity means anything there, and that is its own change.
- **A pid anchor for harness locks.** The harness-neutral candidate is the hook process's parent pid, and if a harness spawns hooks through a transient shell that pid dies at once, every lock reads dead, and enforcement silently stops. Guessing at that inside an identity change is the wrong trade. The TTL already bounds the case.
- **Per-subagent identity for manual claims.** No harness exposes an agent id to a subprocess, so a claim taken by hand inside a subagent can only be expressed at session granularity. Consequences are specified under Concurrency rather than papered over.
- **Automatic resolution of nested harnesses.** A Claude session running `codex exec` exposes both harnesses' variables. No fixed table order is right in both nesting directions, and the only mechanism that would be is process ancestry, which this design rejects below.
- **Nesting on the hook write path.** Nested harnesses are already broken there, before and after this change. An outer Claude session that edits a file takes a row under its own id, and the inner Codex session's `apply_patch` hook then writes under a different id, which `write_blockers` denies because the two are not on one ancestry line. Fixing that means relating two harnesses' session ids to each other, which nothing available can do.
- **The root-resolution asymmetry.** The hook resolves a lock's root from the target file's own directory while `lockm release` resolves it from the process cwd, so the hook can create a lock in a root the CLI refuses to address. Real, observed, and a different axis; the lifecycle sweep still frees such a lock because `release_prefix` is root-agnostic.

## Background

### What is already true

Verified against the source and a live session rather than assumed.

The hook derives its holder from the payload: `session_id`, or `session_id/agent_id` for a subagent (`devkit-locks::hook::holder_from_fields`). It reads no environment variable.

`lockm` resolves `--as` > `$DEVKIT_SESSION` > `$TMUX_PANE` > controlling tty > parent pid (`devkit-locks::ident::resolve_identity`). In a Claude Code Bash call there is no tmux pane and no controlling tty, so it lands on the parent pid, which is the ephemeral per-call shell. `ident::decide_anchor_pid` records no anchor pid without tmux or a tty, so the resulting row is reclaimed only by its TTL.

On Claude Code the payload's `session_id` and `$CLAUDE_CODE_SESSION_ID` are the same value. Confirmed by writing one file in an enforced checkout and reading the resulting row's holder back out of the registry.

On Codex the `PreToolUse` payload's `session_id` is byte-identical to `$CODEX_SESSION_ID` in the environment of the commands that session runs, and `CODEX_THREAD_ID` carries the same value. Observed in a real TUI session on 0.153.4 by dumping the payload from a throwaway project, and again under `exec resume` and `exec fork`, where all three ids move together. `turn_id` is the only id that varies within a session.

The two harnesses differ in what the hook process itself can see. A Claude Code hook inherits `CLAUDE_CODE_SESSION_ID`. A Codex hook inherits `CODEX_HOME` and nothing else: the session variables are injected into the *tool command's* environment, not the hook's. Detection therefore works for `lockm` and the MCP server on both harnesses, because both run inside the tool command, while a hook can compare the two sides only on Claude Code. The staleness design below follows from that asymmetry.

`is_ancestor_or_self` compares on segment boundaries, so `S1` is not an ancestor of `S10`. Every conclusion in Concurrency depends on this.

### Why not a hook-published state file

The alternative to reading the environment is for a hook to write its `session_id` somewhere `lockm` can find it. Rejected: the lookup key would have to be process ancestry, which is precisely the channel already broken here. It would need an ancestry walk of varying depth (`bash -c` exec-optimises a single command, a task runner adds a level), process start times to defend against pid reuse, and three platform implementations. It would also depend on the Bash guard having run and been permitted to write, which contradicts the invariant that the command guard never mutates, and the guard's fail-open contract means a skipped write reverts `lockm` to the parent pid with no signal. That is the same silent failure as a renamed variable, reached through more machinery.

## Design

### Identity resolution

`Env` gains a `harness_session: Option<String>` field, populated by a new `detect_harness_session()` that reads the table below. `resolve_identity` gains one arm. The function stays pure and table-driven, so a new harness is a row.

```rust
const HARNESS_SESSION_VARS: [&str; 2] = ["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"];
```

`CODEX_THREAD_ID` is deliberately absent: it carries the same value as `CODEX_SESSION_ID` today, and listing both would manufacture the ambiguity case below out of a single harness.

### Precedence

`--as` > harness session > `$DEVKIT_SESSION` > `$TMUX_PANE` > controlling tty > parent pid. Left is highest, first non-empty wins.

The detected id sits above `$DEVKIT_SESSION` because the hook ignores that variable outright, so any value visible under a harness already disagrees with what enforcement decided. It also sits above `$TMUX_PANE` and the tty, which identify a terminal rather than a session: a human running Claude Code inside tmux inherits `TMUX_PANE` into every Bash call, and would otherwise resolve to a stable pane id that is still foreign to the hook.

`--as` stays on top. It is per-call and cannot be inherited by accident, so it remains the escape hatch.

Outside a harness the chain is unchanged.

### Ambiguity

When more than one distinct harness session value is visible, devkit does not guess. Equal values across variables collapse to one candidate first; two distinct values are ambiguous.

An ambiguous resolution fails any mutating call (`acquire`, `release`) with an error naming each candidate as `VARIABLE=value`. The value alone is what gets pasted back as `--as`, but the variable is what tells the reader which harness contributed which id, and without it the message reads as two unlabelled strings. `lockm status` shows the HOLDER column as the second way to pick between them. Read-only calls (`check`, `status`) proceed, since identity only labels their output. The hook is unaffected in every case: it uses its payload.

Guessing wrong on acquire reproduces the original defect, because the lock lands under the outer session's id while the inner harness's hook writes under its own.

Release is refused for the same reason rather than allowed as a harmless no-op. `release_all` filters on root and holder alone, so a guessed id inside a nested session frees the outer session's rows in that root while it is alive and mid-task. A named-path release under the wrong id is not harmless either: `do_release` reports the path as refused, and the CLI prints "held by another session; use `--force`", which an agent reads as an instruction. An explicit ambiguity error naming both ids is strictly better than a nudge toward `--force`. Carving `--all` out while letting named-path release guess would be a second rule and more code for a case whose fix is one flag.

Nobody is stranded by this. Only a process running under the inner harness sees two ids; the outer session's shell sees one. Any manual claim the inner caller holds was necessarily taken with `--as`, because acquire already refused, so the caller knows the id. Its hook rows sit under the inner payload id, which is one of the two the error prints.

### Acquire ancestry

`Data::check` compares holders with exact inequality while `Data::decide_write` uses `is_ancestor_or_self`, so the CLI acquire path is stricter than the hook write path. After detection lands, a subagent resolves to the bare session id, and a subagent that writes a file through `Edit` first (row under `S/a1`) is then refused when it claims that same file by hand.

`check` adopts one rule: an overlapping live lock blocks an acquire unless its holder and the acquirer lie on the same ancestry line, in either direction. Four cases, which together are what "the same session" has to mean:

| Existing holder | Acquirer | Result |
|---|---|---|
| `S` | `S/a1` | allowed, the parent delegated |
| `S/a1` | `S` | allowed, this is the subagent self-conflict |
| `S/a1` | `S/a2` | blocked, siblings stay isolated |
| `S1` | `S2` | blocked, cross-session |

Permitting the acquire is not permission to overwrite the row. `try_acquire` inserts unconditionally today, and `key_for(root, path)` is the same key for every holder, so a parent claiming a path its subagent holds would replace `S/a1` with `S`, silently widening the claim until every sibling could write it. When an acquire is permitted by an ancestry relation rather than an exact holder match, the existing row stays as it is and is reported as already held. Only an exact holder-and-path match is renewed. This mirrors `decide_write`, where an ancestor's lock is already never overwritten.

`do_release` is untouched and still refuses to free a row held by a different holder without `--force`, so a hook-created `S/a1` row is freed by the lifecycle sweep rather than by hand.

### Lifecycle release ahead of the enforcement gate

`run_hook` evaluates `enforcement_enabled` before parsing the event, so a `SessionEnd` fired from a non-enforced directory, or with a config that will not read, never reaches `release_prefix`. The session's locks then wait out their TTL.

Event parsing moves ahead of the gate. `Write` events stay gated; `SessionEnd` and `SubagentStop` run unconditionally. Releasing locks a session already holds is correct whether or not enforcement is currently on for the directory the session happens to be sitting in.

### Fail closed on an unusable write payload

A write payload with no `session_id`, or whose targets do not parse, currently returns `Ignore`, which `run_hook` treats as allow. That is a fail-open path inside the component whose contract is to fail closed, and a payload format change would disable enforcement with no signal.

`HookEvent` gains an `Unusable { reason }` variant, returned when the tool is a write tool and either the session id is missing or no target could be extracted. `run_hook` denies on it with the reason. Non-write tools keep returning `Ignore`, which is the correct answer and the common case.

### SubagentStop needs an agent id

`parse_event` composes the holder before it knows the event, so a `SubagentStop` with no `agent_id` yields the bare session holder and `release_prefix` frees the entire session, including the parent's claims and every sibling's.

`SubagentStop` without a non-empty `agent_id` releases nothing and returns `Ignore`. A subagent whose stop event cannot be attributed is covered by `SessionEnd` and the TTL.

### Ancestor lock renewal

`decide_write` renews only the exact-path row for the writer. A claim over a directory is therefore never renewed by writes to files under it, and expires mid-session while its holder is still working inside it. This directly undercuts the documented advice to lock a subtree.

When the write is allowed by ownership, the overlapping row is renewed when its holder is the writer or an ancestor of the writer. An ancestor's row has its timestamp bumped; its holder is never rewritten.

### Staleness detection

Nothing in CI can catch a vendor renaming a session variable, because a test exercises devkit's table against itself. Only the hook sees both sides at once.

The check cannot live in the hook. A Codex hook never sees a session variable, so a hook that flagged "payload id matches no candidate" would stamp a mismatch on every Codex write. Narrowing it to "a candidate is visible but differs" would make it silent on Codex entirely, and silent on the rename it exists to catch, since a renamed variable is an absent one.

It lives in `devkit doctor`, which runs inside the tool command and therefore sees what `lockm` sees.

Comparing the resolved id against the lock rows in the checkout does not work, and this is worth stating because it is the obvious design. Rows belong to whichever sessions have written in that checkout. A second session that has not yet edited a file owns none of them; neither does a human at a plain terminal, where resolution falls through to a tty or a parent pid. So "no row is mine" is the ordinary state in exactly the case devkit exists to serve, several sessions sharing one checkout, and it cannot be told apart from a genuine desync. A row that warns during normal operation is a row people learn to ignore.

The rename has a sharper signature that needs no rows at all. When a vendor renames its session variable, that variable drops out of the table while the harness keeps stamping its other variables on the environment. So a process carrying a `CLAUDE_CODE_`- or `CODEX_`-prefixed variable while none of the exact session variables is set is running under a harness devkit no longer recognises, and its locks will fall back to a parent pid the hook will never match. A plain terminal carries neither and the row stays silent; a working harness sets its session variable and the row reports the id. An ambiguous nested pair warns too, naming both variables, because that state also blocks `acquire` and `release`.

This works identically on both harnesses, needs no new file, no note constant, no daemon protocol change and no aging code.

Two limitations, both accepted. The row says which id this session resolves, not whether it agrees with any other session, because as argued above no honest comparison exists. And it reports only when someone runs `doctor`. Neither costs anything while no session is desynced.

Stderr is not used: an exit-0 hook's stderr is not reliably surfaced to either the agent or the user.

### MCP

`devkit_mcp::mint_holder` returns `mcp-<pid>` when `$DEVKIT_SESSION` is unset, so an agent claiming a file through the MCP `locks.acquire` action collides with its own write hook exactly as `lockm` does. It routes through the same resolution. Stdio MCP servers receive the harness session variable, so this needs no new plumbing.

Ambiguity has to be deferred there rather than raised at startup. `mint_holder` runs once into `ServerCtx::default_holder`, so an ambiguous environment would otherwise fail the server's construction and take every unrelated action down with it. The server stores the resolution outcome instead and fails an individual call only when that call omits `holder`. The `holder` override already exists on `locks.acquire`, `locks.check` and `locks.release`, so the escape hatch needs no new argument.

## Concurrency

The two questions this design has to answer.

### One session, fanning out subagents

Holders: the parent writes as `S`, subagents as `S/a1`, `S/a2`.

Hook-mediated writes are correct today and stay correct. `a2` writing a file `a1` holds is refused, because `S/a1` is not an ancestor of `S/a2`. The parent writing a file a subagent holds is refused for the same reason. A subagent writing a file the parent holds is allowed, because `S` is an ancestor of `S/a1`, which is what delegation should mean.

Manual claims degrade to session granularity. A subagent's shell sees the parent's session id and no agent id exists in the environment, so a hand-taken claim is recorded under `S`. It protects the path from every other session, and does not protect it from siblings, because `S` is an ancestor of `S/a2`.

This is specified rather than fixed. A manual claim is a session-level claim, which is the only thing devkit can express without an agent id. The consequences are that sibling isolation for a hand-claimed path comes from the hook alone, and that `lockm release --all` run inside a subagent frees the whole session's manual claims. Both go in `references/locks.md`.

The acquire-ancestry change above removes the one case that would otherwise be a hard failure: a subagent claiming by hand a file it has already written through the hook.

### Two sessions in one repo, each fanning out subagents

Holders: `S1`, `S1/a1`, `S2`, `S2/b1`.

Every cross-session path is correct, and none of it depends on the manual-claim degradation above.

A write by `S2/b1` to a file held by `S1/a1` is refused: `S1/a1` is not an ancestor of `S2/b1`. A manual acquire by `S2` over anything held by `S1` or its subagents is refused, both before and after the ancestry change, because neither holder lies on the other's ancestry line.

Release is isolated. `release_prefix("S1")` frees `S1` and `S1/*` and nothing else, because `is_ancestor_or_self` matches on segment boundaries. A session id that is a textual prefix of another session's id (`S1` against `S10`) does not leak, for the same reason.

The lifecycle change strengthens this: `S1` ending in a directory where enforcement is off now releases `S1`'s subtree instead of leaving it to the TTL, and still touches nothing of `S2`'s.

The failure that would break this case is detection silently not working, since both sessions would fall back to per-call parent pids and every claim would be a fresh stranger. That is what the staleness note reports.

### Nested harnesses

A Claude session running `codex exec` in the same repo exposes both variables with different values. Manual mutating calls refuse and ask for `--as`. The inner harness's hook is unaffected and continues to write under its own payload's session id. No silent misattribution.

## Testing

Unit, on pure functions: `resolve_identity` per precedence pair including the new arm; the dedupe-then-refuse behaviour of ambiguity; `check` across all four ancestry cases; and an acquire permitted by ancestry leaving the existing row's holder unchanged, which is the case a naive fix gets wrong.

Integration, at the layer the bug surfaced: drive `lockm hook pretooluse` with a synthetic payload, then run `lockm acquire` on the same path in the same session, and assert it is allowed rather than denied. Then the reverse order. Both fail today.

Integration, for the lifecycle fixes: a `SessionEnd` delivered with enforcement off still releases; a `SubagentStop` with no `agent_id` releases nothing; a directory claim survives a write to a file underneath it past what would have been its expiry.

Cross-session, covering the second Concurrency case: two synthetic sessions with subagent holders in one root, asserting the refusal matrix and that a release of one leaves the other's rows intact.

Eight test files scrub `DEVKIT_SESSION` and `TMUX_PANE` from the child environment across nine call sites (`tests/locks.rs` in both `run` and `run_hook`, `mcp.rs`, `cli_ergonomics.rs`, `sync_includes.rs`, `brief_main_checkout.rs` twice, `issue_setup_root.rs`, `issue_bare_main_root.rs`, `common/baselinetest.rs`). Each needs the harness variables too, or a run started from an agent's Bash tool resolves a different default identity than CI does. One shared helper, not a ninth copy. Regenerate the list with `rg -n 'env_remove\("DEVKIT_SESSION"\)' tests/`, which must come back empty once the helper is in place.

Windows: `sys::windows::controlling_tty` always returns `None`, so a missing harness variable falls straight through to the parent pid there. The detection path needs a real Windows test rather than resolver unit tests alone.

## Documentation

`skills/using-devkit/SKILL.md`: the claim that acquiring manually under enforcement is "harmless and redundant" is false today and becomes true after this change; the instruction to export a role name as `$DEVKIT_SESSION` is removed, since the detected id now outranks it and the role name only served to paper over the defect.

`skills/using-devkit/references/locks.md`: the identity list gains the harness row, plus the session-granularity semantics of a manual claim inside a subagent.

`docs/commands.md`: the identity sentence gains the same row.

## Open questions

1. Is devkit's `lockm hook pretooluse` actually running under Codex? Codex skips an untrusted hook silently, with no prompt and no warning under `codex exec`, recording trust as a `trusted_hash` under `[hooks.state]`. In one observed configuration only a `session_start` entry carried `enabled = true` while the `pre_tool_use`, `session_end` and `subagent_stop` entries carried a hash alone, and a `codex exec` run fired `SessionStart` and no `PreToolUse`. If `enabled` is required rather than defaulted, Codex-side write enforcement is off wherever it has not been trusted interactively, and every Codex claim in this design is theory. This gates the value of the work, not its correctness, and wants one check before implementation.
