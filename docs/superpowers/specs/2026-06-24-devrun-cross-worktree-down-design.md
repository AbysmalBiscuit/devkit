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
- precise **column filters** as an alternative to a positional token;
- **denyable scope flags** (`--all` / `--others` / `--holder`) — the only way to
  reach another worktree — so a harness can block cross-worktree `down` by flag name;
- a **cross-worktree confirmation gate** (TTY-gated; refuse when non-interactive),
  per-worktree by default or batched under `--all`/`--batch`;
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
ports. Selection has two layers: a **scope** (which holders are eligible) and a
**filter** (which of the in-scope rows to stop). The scope defaults to the current
worktree and is widened only by an explicit, denyable flag.

**Scope flags** — the *only* way to reach another worktree. Any one of them lifts
the current-worktree-only default; a harness blocks cross-worktree `down` entirely
by denying these flag names (see "Harness denyability"):

- `--all` — every holder, including the current worktree.
- `--others` — every holder *except* the current worktree (clean up other
  worktrees, leave mine running).
- `--holder <path>` (repeatable) — exactly the given holders (each resolved to its
  git toplevel).

With **no scope flag**, scope is the current worktree's holder — so a bare
positional or column filter can never match a foreign holder. This is what keeps an
agent's plain `devrun down …` self-confined regardless of TTY.

**Filters** — narrow the in-scope rows. Positional and column filters are mutually
exclusive with each other, but either combines freely with the scope/confirm flags:

- **Positional `SELECTOR`** (repeatable): each token is lowercased and
  **substring-matched** against the identity columns of every in-scope row — the
  holder leaf name *and* full holder path, APP, PORT (as text), ROLE
  (`issue`/`baseline`), and PID (as text). A row matches if any column contains the
  token. Multiple tokens are **OR'd**, as in `issue end`.
- **Column flags** (AND-combined): `--app <name>` (repeatable), `--port <p>`
  (repeatable), `--role <issue|baseline>` (the existing `RoleSelector` filter),
  `--pid <pid>`, `--listening` / `--not-listening` (LISTENING predicate),
  `--older-than <dur>` (`30m`, `2h`, `90s`; rows whose `now - ts` exceeds the
  threshold).

**No scope flag and no filter → unchanged default:** stop every server in the
current worktree (today's behavior).

The selection logic lives in `devkit-ports` (library, unit-testable), not in the
binary: a `DownSelector` value (scope + filter, plus the current holder) plus
`fn select(data: &Data, sel: &DownSelector, now: u64) -> Vec<u16>` returning the
matching ports. The binary builds the `DownSelector` from clap args; the daemon and
direct paths consume the resolved port set.

### Harness denyability

Cross-worktree reach is gated by *two* independent layers, so blocking it never
depends on a single assumption:

1. **Intrinsic (always on):** the TTY confirmation below. A no-TTY caller is
   refused with no configuration, which covers the MCP path (root-scoped, can't
   name a foreign holder) and a Bash-tool shell-out (no PTY).
2. **Extrinsic (opt-in defense-in-depth):** every cross-worktree path requires one
   of the named scope flags `--all` / `--others` / `--holder`. A harness that wants
   a hard guarantee — e.g. one that allocates a PTY and could otherwise answer the
   prompt — denies `devrun down` invocations carrying those flags. A bare positional
   can never escape the current worktree, so there is no flag-less cross-worktree
   path to miss.

### The human/agent gate

After resolving the selection to ports, the CLI partitions them by holder and
computes whether any matched row's holder differs from the current worktree's
toplevel.

- **All matches are in the current worktree** → behave exactly as today: stop +
  release, no prompt. (This preserves the existing UX and means an agent's
  in-worktree `down` is never blocked.)
- **The selection touches a foreign holder** → confirm interactively before
  stopping anything. Two confirm modes:
  - **Per-worktree (default):** for each foreign worktree, print its matched rows
    (a `status_table` slice) and prompt `Stop N server(s) in <worktree>? [y/N]`, so
    you can stop two of three. Servers in the current worktree (if `--all` pulled
    them in) stop without a prompt.
  - **Batch:** when `--all` or `--batch` is given (`--all` implies `--batch`),
    print **one** preview table listing every server to be stopped across all
    worktrees, then a single `Stop N server(s) across M worktree(s)? [y/N]`.
  - A blank / `n` / EOF answer aborts that prompt's worktree(s) with nothing
    stopped there.
  - **If stdin is not an interactive terminal** (`!std::io::stdin().is_terminal()`),
    do not prompt: print the preview, print
    `cross-worktree down requires an interactive terminal` to stderr, and exit
    non-zero. This is the agent gate — a tool-invoked `devrun` has no TTY.

There is no flag to skip the prompt for cross-worktree targets; `--batch` only
collapses the prompts into one, it does not suppress them.

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
devrun down                          # current worktree, all servers (unchanged)
devrun down --role baseline          # current worktree, baseline only (unchanged)
devrun down api                      # current worktree, fuzzy match `api` (no prompt)
devrun down --app api --older-than 1h# current worktree, precise AND filter (no prompt)

# cross-worktree — needs a scope flag; prompts; needs a TTY:
devrun down --all                    # every server, every worktree (batch prompt)
devrun down --others                 # every server in every OTHER worktree (batch prompt)
devrun down --others api             # `api` in other worktrees (per-worktree prompts)
devrun down --holder ../wt/feat-x    # one specific worktree (per-worktree prompt)
devrun down --all --app api          # `api` everywhere (batch prompt)
```

`down --help` documents that (a) a positional selector and the column filters are
mutually exclusive, (b) reaching another worktree requires `--all`/`--others`/
`--holder`, and (c) any cross-worktree selection prompts for confirmation and
requires a terminal.

## Error handling

- **Empty selection** (no rows match) → print `no tracked servers match
  <selector>` and exit zero (nothing to do; not an error). The no-arg default with
  an empty current worktree stays as today.
- **Positional + column filters both given** → clap-level conflict error. (Scope
  flags `--all`/`--others`/`--holder` and `--batch` are *not* in the conflict set;
  they combine with either.)
- **Scope flag implies the gate, even if it resolves to zero foreign rows** — e.g.
  `--others` when no other worktree has servers is just an empty selection (exit
  zero), never an error.
- **`--holder` path with no git toplevel** → resolve best-effort to the given path;
  if it matches no rows, falls through to the empty-selection message.
- **Bad `--older-than`** → parse error naming the accepted forms.
- **Daemon proto mismatch** (old daemon without `DownPorts`) → the handshake
  version check already fails the connection; surface the existing daemon-version
  error rather than silently falling back to a write the daemon's lock forbids.

## Testing (TDD)

Library (`devkit-ports`):

- **Scope** — default selector (no scope flag) restricts to the current holder;
  `--all` includes every holder; `--others` excludes the current holder;
  `--holder X` keeps only X.
- **Positional filter** — a token substring-matches across each column (holder
  leaf, holder path, app, port-as-text, role, pid-as-text) and OR's multiple
  tokens, within scope.
- **Column filters** — AND-combined; `--older-than` / `--listening` predicates
  filter on `ts` / `listening`.
- `bring_down_ports` direct path stops a pid-bearing entry and releases a pidless
  reservation; idempotent on a second call (mirrors the existing
  `bring_down_releases_a_pidless_reservation` test).
- `DownPorts` frame round-trips over a pipe (mirrors the proto round-trip test).

Binary (`devrun`):

- A cross-worktree selection (scope flag) with stdin not a TTY refuses (non-zero,
  nothing stopped) — drive via the existing process-spawning test style, polling
  state.
- A current-worktree-only selection never prompts.
- A bare positional never matches a foreign holder (scope confinement).

`cargo test --workspace` and `cargo clippy --workspace --all-targets -D warnings`
stay green; the multiprocess `devkit-ports --test registry` race test still passes.

## Decisions (resolved)

- **Enforcement:** TTY gate as the intrinsic mechanism + named scope flags
  (`--all`/`--others`/`--holder`) as a denyable extrinsic seam. No separate
  human-only subcommand.
- **Fuzzy = substring**, case-insensitive, across columns.
- **Predicates kept:** `--older-than` and `--listening`/`--not-listening` ship in v1.
- **Confirm mode:** per-worktree by default; `--all` or `--batch` collapses to one
  combined prompt listing every server (`--all` implies `--batch`). No prompt-skip
  flag.

## Unresolved questions

None outstanding — ready to plan implementation.
