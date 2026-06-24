# devrun cross-worktree `down` — design

## Goal

Let a **human** at a terminal use `devrun down` to stop dev servers belonging to
worktrees other than the current one — selected by a fuzzy positional token (like
`issue end`) or by precise column flags — while keeping an **agent** confined to
its own worktree. The human/agent boundary is enforced by an interactive
confirmation: stopping anything outside the current worktree requires a `y/N`
answer read from a TTY, and a non-interactive caller (an agent shelling out via a
tool with no TTY) is refused.

## Scope

`devrun down` gains:

- optional **positional selectors** (`devrun down [SELECTOR...]`) that fuzzy-match
  across registry columns, mirroring `issue end`'s positional UX;
- precise **column flags** as an alternative to a positional token;
- a **cross-worktree confirmation gate** (TTY-gated; refuse when non-interactive);
- a new daemon proto request, `DownPorts { ports }`, so a precise port-set stop
  works while `devkitd` holds the write gate.

### Non-goals

- **No change to the MCP `devrun.down` handler.** It stays root-scoped (it takes a
  `root` holder and has no `--all`/cross-holder argument), so an agent on the MCP
  path cannot express a foreign target. This is the primary agent path and needs
  no gate of its own.
- **No `--yes`/`--force` bypass for cross-worktree.** A bypass flag would let an
  agent skip the gate, defeating its purpose. Cross-worktree stops are always
  interactive.
- **No fuzzy-ranking / typo-tolerance.** "Fuzzy" here means case-insensitive
  substring match across columns, not edit-distance ranking.

## Background

- `devrun down` today calls `run::bring_down(holder, role)` from `cmd_down`
  (`src/bin/devrun/main.rs`), where `holder` is always `toplevel(cwd)` — the
  current worktree. There is no way to target another holder from the CLI even
  though `bring_down` accepts an arbitrary holder.
- The registry (`crates/devkit-ports/src/registry.rs`) keys every server by
  `holder`. `Entry` carries `app, holder, role, pid, logfile, ts`; the port is the
  `BTreeMap` key. `status_table` renders the columns `PORT APP ROLE HOLDER PID
  LISTENING AGE`, deriving LISTENING from `listening(port)` and AGE from
  `now - ts`. `snapshot()` is an **ungated read** (works whether or not a daemon
  holds the write lock).
- `bring_down` prefers a running `devkitd` (`Request::Down { holder, role }`) and
  otherwise stops + releases directly under `with_lock`, without pruning first
  (an invariant: a still-running server whose reservation looks stale must still
  receive SIGTERM).
- `issue end` (`src/bin/issue/end.rs`) takes positional selectors and matches each
  against several identity fields of each worktree row, with a `confirm(label)`
  helper that reads a `y/N` line from stdin. `issue/spin.rs` already uses
  `std::io::IsTerminal` (`stderr().is_terminal()`).

## Architecture

### Selection model

`down` resolves a **selector** against a registry snapshot into a set of affected
ports. Inputs (positional and flags are mutually exclusive):

- **Positional `SELECTOR`** (repeatable): each token is lowercased and
  **substring-matched** against the identity columns of every row — the holder
  leaf name *and* full holder path, APP, PORT (as text), ROLE (`issue`/`baseline`),
  and PID (as text). A row matches if any column contains the token. Multiple
  tokens are **OR'd** (union of matches), as in `issue end`.
- **Column flags** (AND-combined), used only when no positional token is given:
  - `--all` — lift the "current worktree only" default (consider every holder).
  - `--holder <path>` — resolved to its git toplevel; exact holder match.
  - `--app <name>` — repeatable; exact app match.
  - `--port <p>` — repeatable; exact port match.
  - `--role <issue|baseline>` — existing flag (a `RoleSelector` filter).
  - `--pid <pid>` — exact pid match.
  - `--listening` / `--not-listening` — LISTENING predicate.
  - `--older-than <dur>` — AGE predicate (`30m`, `2h`, `90s`); rows whose
    `now - ts` exceeds the threshold.
- **Neither positional nor flags → unchanged default:** restrict to the current
  worktree's holder and stop all of its servers (today's behavior).

Default scoping rule: unless `--all`, `--holder`, or a positional token is given,
the selection is restricted to the current worktree. (A positional token is
inherently cross-worktree-capable, so giving one lifts the restriction; the gate
below still applies to whatever it resolves to.)

The selection logic lives in `devkit-ports` (library, unit-testable), not in the
binary: a `DownSelector` value plus `fn select(data: &Data, sel: &DownSelector,
now: u64) -> Vec<u16>` returning the matching ports. The binary builds the
`DownSelector` from clap args; the daemon and direct paths consume the resolved
port set.

### The human/agent gate

After resolving the selection to ports, the CLI partitions them by holder and
computes whether any matched row's holder differs from the current worktree's
toplevel.

- **All matches are in the current worktree** → behave exactly as today: stop +
  release, no prompt. (This preserves the existing UX and means an agent's
  in-worktree `down` is never blocked.)
- **The selection touches a foreign holder** → print a preview table (reuse
  `registry::status_table` filtered to the matched rows) and prompt
  `Stop N server(s) across M worktree(s)? [y/N]`, reading the answer from stdin.
  - A blank / `n` / EOF answer aborts with nothing stopped.
  - **If stdin is not an interactive terminal** (`!std::io::stdin().is_terminal()`),
    do not prompt: print the preview, print
    `cross-worktree down requires an interactive terminal` to stderr, and exit
    non-zero. This is the agent gate — a tool-invoked `devrun` has no TTY.

There is no flag to skip the prompt for cross-worktree targets.

### Resolving and executing the stop

The whole operation reduces to "stop + release a set of ports," computed
client-side from the ungated `snapshot()`:

1. `snapshot()` → `select(...)` → `Vec<u16>` ports (+ the matched `Entry` rows for
   the preview/partition).
2. Gate (above).
3. Execute:
   - **Daemon running** → send the new `Request::DownPorts { ports }`. The daemon,
     which holds the entries in memory, performs an *intentional* stop for each
     port (removing the supervision-table key before signalling, per the existing
     crash-vs-stop invariant) and releases the reservations, returning
     `Response::Freed(ports)`.
   - **No daemon** → `registry::with_lock`: for each selected port still present,
     `supervise::stop(pid)` if it has a pid, then remove the entry. No prune first
     (the still-running-but-stale invariant). Return the freed ports.

`run::bring_down(holder, role)` stays as-is for the MCP path and is the natural
implementation of the default no-arg CLI case (holder = current worktree). The new
port-set path is a sibling, e.g. `run::bring_down_ports(ports)`, sharing the same
daemon-preferred / direct-fallback structure.

### Daemon protocol change

Add to `daemon/proto.rs`:

```rust
DownPorts { ports: Vec<u16> },
```

and bump `PROTO` to `2` (the client and daemon ship together, so the handshake
version check covers a stale daemon). The handler resolves each port to its
in-memory entry, runs the intentional-stop path, releases it, and replies
`Response::Freed`. The existing `Down { holder, role }` request is retained for the
MCP path and back-compat.

## CLI surface

```
devrun down                       # current worktree (unchanged)
devrun down --role baseline       # current worktree, baseline only (unchanged)
devrun down feat-login            # fuzzy: everything whose holder/app/port/... matches
devrun down api                   # every `api` server, all worktrees (prompts)
devrun down 9100                  # whatever is on port 9100 (prompts if foreign)
devrun down --all                 # every tracked server, every worktree (prompts)
devrun down --holder ../wt/feat-x # one specific worktree (prompts)
devrun down --app api --role issue --older-than 1h   # precise AND filter
```

`down --help` documents that a positional selector and the column flags are
mutually exclusive, and that any selection reaching outside the current worktree
prompts for confirmation and requires a terminal.

## Error handling

- **Empty selection** (no rows match) → print `no tracked servers match
  <selector>` and exit zero (nothing to do; not an error). The no-arg default with
  an empty current worktree stays as today.
- **Positional + flags both given** → clap-level conflict error.
- **`--holder` path with no git toplevel** → resolve best-effort to the given path;
  if it matches no rows, falls through to the empty-selection message.
- **Bad `--older-than`** → parse error naming the accepted forms.
- **Daemon proto mismatch** (old daemon without `DownPorts`) → the handshake
  version check already fails the connection; surface the existing daemon-version
  error rather than silently falling back to a write the daemon's lock forbids.

## Testing (TDD)

Library (`devkit-ports`):

- `select` with a positional token matches across each column (holder leaf, holder
  path, app, port-as-text, role, pid-as-text) and OR's multiple tokens.
- `select` with AND flags intersects correctly; `--older-than` / `--listening`
  predicates filter on `ts`/`listening`.
- Default selector (no positional, no `--all`/`--holder`) restricts to the current
  holder.
- `bring_down_ports` direct path stops a pid-bearing entry and releases a pidless
  reservation; idempotent on a second call (mirrors the existing
  `bring_down_releases_a_pidless_reservation` test).
- `DownPorts` frame round-trips over a pipe (mirrors the proto round-trip test).

Binary (`devrun`):

- A foreign-touching selection with stdin not a TTY refuses (non-zero, nothing
  stopped) — drive via the existing process-spawning test style, polling state.
- An all-current-worktree selection never prompts.

`cargo test --workspace` and `cargo clippy --workspace --all-targets -D warnings`
stay green; the multiprocess `devkit-ports --test registry` race test still passes.

## Unresolved questions

1. **Fuzzy = substring** (chosen) vs. exact-field equality like `issue end`'s
   current matching. Substring is the assumption in this spec; confirm before
   implementing if exact is preferred.
2. **`--older-than` and `--listening` predicates** — included for column-complete
   targeting, but they're the least-requested. Drop from v1 if you'd rather keep
   the first cut to identity columns (holder/app/port/role/pid + `--all`)?
3. **Preview detail** — single batch confirm (chosen) vs. per-worktree confirm
   like `issue end`. Batch is simpler; per-worktree is finer-grained. Confirm the
   batch choice.
