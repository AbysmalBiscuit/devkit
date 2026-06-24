# devkit

A Rust workspace (edition 2024): a root `devkit` binary package whose six CLIs live
in `src/bin/`, plus three library crates, coordinating local development for a monorepo.
The engine is project-agnostic; every project-specific detail lives in `devkit.toml`.
See `README.md` for user-facing CLI docs.

## Commands

```sh
cargo build --release                       # all six binaries → target/release
cargo install --path .                       # install all six into ~/.cargo/bin
cargo test --workspace                       # full gate — 327 tests, must stay green
cargo clippy --workspace --all-targets -- -D warnings   # zero-warning policy
cargo test -p devkit-ports --test registry   # multiprocess flock race test
```

Run all three before committing: CI runs them on every push and PR, and a push to `main`
also drives release-please. Format with `cargo fmt --all` (the `--check` above only
verifies) using the stable toolchain CI uses, so formatting matches.

## Layout

The workspace root is the `devkit` binary package; its CLIs live in `src/bin/` and
install together via `cargo install --path .`. Three library crates are members.

| Unit | Role |
|---|---|
| `crates/devkit-common` | shared lib: `paths`, `cmd` (git/gh wrappers), `worktree`, `ui` (tables/links), `linear`, `slack`, `supervise` |
| `crates/devkit-ports` | lib: `config` (toml), `doppler` (yaml), `apps` (catalog), `registry` (flock'd port store), `load`, `daemon` |
| `crates/devkit-locks` | file-lock registry: model + flock'd JSON store |
| `crates/devkit-issue` | lib: read-only issue triage facade — `status` (worktree + PR + Linear state with the finished verdict) and `prs` (PR triage); serializable, no rendering, no mutations |
| `crates/devkit-mcp` | lib: stdio MCP server (`jsonrpc`, action `registry`, `ports`/`locks`/`devrun`/`issue` handlers) over the port + lock facades, the `devkit-ports::run` server-lifecycle facade, and the `devkit-issue` triage facade |
| `src/bin/portm.rs` | CLI over the port registry |
| `src/bin/devrun` | supervised dev-server runner (`env`, `supervise`, `baseline`) |
| `src/bin/issue` | issue lifecycle: `setup`, `status`, `end`, `prs`, `dashboard`, `review` |
| `src/bin/lockm.rs` | advisory file-lock CLI |
| `src/bin/devkit` | credential setup + diagnostics: `auth` (validate + store Linear/Slack tokens), `doctor` |
| `src/bin/devkit-mcp` | meta-MCP stdio server exposing the port + lock facades to coding agents |
| `src/bin/devkitd` | supervisor daemon serving both the port registry (`ports.sock`) and the lock registry (`locks.sock`), authoritative in memory, write-through to the files, gated by `devkitd.lock`; bin gated by the `daemon` feature (on by default) |

The five user-facing CLIs (`portm`, `devrun`, `issue`, `lockm`, `devkit`) each
expose a `completions <shell>` subcommand via `clap_complete`.

## Invariants (do not break)

- **Reserve before bind.** `registry::alloc_one` writes a pid-less reservation row *before*
  any process binds the port; this is what prevents the allocation race across concurrent
  callers. `record_pid` then upserts the pid — and re-inserts the row if it was pruned in
  the gap, so a live process is never left untracked (otherwise `devrun down` can't stop it).
- **`RESERVATION_GRACE_SECS` (300) must exceed `devrun`'s readiness timeout (120s)** so a
  reservation cannot expire while its own server is still coming up. Don't lower it below the
  timeout.
- **`with_lock` holds an exclusive advisory lock for the whole read-modify-write.** Keep work
  inside it minimal; avoid slow/network calls under the lock.
- **`devrun down` stops then releases without pruning first** — a still-running server whose
  reservation looks stale must still receive SIGTERM.
- **Cross-worktree `devrun down` is TTY-gated.** A selection touching a holder
  other than the current worktree is refused unless stdin is an interactive
  terminal (`cmd_down` in `src/bin/devrun/main.rs`), and is reachable only via the
  named scope flags `--all`/`--others`/`--holder` — so an agent (no PTY) cannot
  stop another worktree's servers, and a harness can deny those flags by name. The
  MCP `devrun.down` handler stays root-scoped and never gains a cross-holder arg.
- **The supervisor table — not the registry row — decides crash vs. stop.** A child the
  `devkitd` supervision thread reaps is a crash and is restarted (within the crash-loop
  budget); an intentional `Down` removes the key from the table *before* signalling the
  child, so a stopped server is never reaped as a crash. Don't make the restart decision
  read `ports.json`/`d.ports` — a concurrent prune would race it.
- **A non-crash restart goes through the crash path, not its own.** When the
  health probe (`DEVKIT_DAEMON_HEALTH_PROBE_SECS` > 0) judges a server hung, or
  the memory action (`memory_action = "restart"`) finds one over
  `memory_limit_mb` for `memory_limit_ticks` ticks, it only SIGTERMs the
  server; the supervision tick then reaps and respawns it within the crash-loop
  budget. Neither path gets its own respawn — two respawners would race on the
  same key. The memory path *peeks* the budget (`can_restart`) before killing so
  the kill is skipped once exhausted (warn and leave alive), but the budget is
  recorded only in `restart()`, so a restart counts exactly once.
- **A hard-cap breach is a crash, not a restart path.** `memory.max` +
  `memory.oom.group=1` OOM-kills the supervised leaf; the reap → crash → respawn
  path handles it within the crash-loop budget. No dedicated restart path exists
  for the hard cap — the same rule already established for health-probe and the
  soft memory restart.
- **Cap setup is fail-open.** Any cgroup error (mkdir denied, `memory.max` write
  fails, fd open fails) logs once and proceeds with an uncapped spawn; it never
  blocks or kills a server. A broken cgroup configuration degrades to the soft
  `memory_action` path.
- **`memory_max_mb` sits above `memory_limit_mb`.** The soft poll-based action
  (`memory_action = "restart"`) is the graceful first responder; the kernel cap
  (`memory_max_mb`) is the backstop. Set `memory_max_mb` higher than
  `memory_limit_mb` so the soft restart gets to act first.
- **A `prd` doppler launch is rejected.** `launch` is run verbatim, so devkit
  guards at launch time: for a launch whose program is `doppler`, it resolves the
  config from `-c`/`--config`, else `DOPPLER_CONFIG`, else `doppler configure get
  config --scope <app dir>`, and refuses to start a server when that resolves to
  `prd` or cannot be resolved. The guard lives in `run::assert_not_prd`, called
  from `run::launch`, so it covers `devrun`, the MCP `devrun.up`, and both the
  daemon and direct spawn paths.

## Conventions

- Commits follow Conventional Commits. Follow the active workflow skill's commit cadence (a design/plan skill
  commits its own artifact; per-task execution commits per task).
- TDD: write the failing test first; `cargo test --workspace` is the merge gate.
- `anyhow` everywhere — its `.context()` chain and backtrace are the error-reporting
  mechanism. Each binary installs `report::install_panic_hook` for crash diagnostics;
  `RUST_BACKTRACE=1` adds a backtrace to both errors and panics.
- App conventions are config-driven, never hardcoded: the URL-providing app is marked
  `provides_url`; per-app prep files come from `prep_files`; the apps directory is
  `defaults.apps_dir`. Example-specific values live in the personal config at
  `~/.config/devkit/config.toml` (outside the repo; see `docs/configuration.md`).
- `Role` (Issue/Baseline) is defined once in `devkit-ports::registry` with `ValueEnum` +
  `Display`; `devrun`'s CLI uses a separate `RoleSelector` (adds `Both`). No `_ => Issue`
  catch-alls — map roles exhaustively.
- CI runs the `test` job (and `clippy`) on ubuntu, macos, and windows. Tests that spawn or
  reap processes must poll for the expected state, not sleep a fixed interval — a loaded
  Windows runner exits a child later than a short fixed sleep allows.
- **`devkit` configures and diagnoses the toolkit itself** — credentials
  (`auth`) and `doctor`. The operational verbs (`portm`, `devrun`, `issue`,
  `lockm`) stay in their own binaries; `config` stays on `devrun`. Token reads
  resolve through `devkit-common::secrets` (env → `secrets.toml`), never from
  `config.toml`.

## Worktrees

The primary clone (`C:/Users/Lev/Git/lev/devkit`) stays on `main`. Feature work
never checks out a branch in it — every branch lives in its own worktree under
`../devkit-worktrees/`:

- Start work with `git worktree add ../devkit-worktrees/<name> -b <branch> main`,
  not `git checkout -b <branch>` in the primary clone. Several agent sessions
  share this repo at once; an in-place checkout moves the branch under all of
  them and corrupts the others' view of HEAD.
- Land finished work by fast-forwarding `main` from outside its worktree
  (`git -C <primary> switch main && git merge --ff-only <branch>`, or
  `git fetch . <branch>:main` while `main` is checked out nowhere), then
  `git worktree remove` the worktree.
- If you ever find the primary clone on a non-`main` branch, stop and restore it
  (`git switch main`, re-home the stray branch in a worktree) before doing
  anything else. The `post-checkout` guard hook warns when this happens.

## File locks

When multiple sessions share one checkout, claim files before editing them with the
`lockm` binary instead of writing ad-hoc `.lock` files:

- `lockm acquire <paths…> --as <stable-session-id>` before editing; it exits `1` with
  the current holder if any path is taken — branch on that.
- `lockm release <paths…> --as <same-id>` (or `lockm release --all --as <id>`) when done.
- Always pass a consistent `--as <id>` (or set `$DEVKIT_SESSION`) so acquire and
  release refer to the same holder.

## Registry facade

Go through `registry::{alloc, record_pid, release, snapshot, prune, status_table}` — they
keep liveness syscalls (bind/stat/kill) out of the exclusive lock. Don't reintroduce
probing inside `with_lock`. This facade is also the seam the `devkitd` daemon plugs into.

When a `devkitd` daemon is running it is the *authoritative* registry for both the
port and lock registries: it loads `ports.json` and `locks.json` into memory under
`devkitd.lock` (held exclusive for its life), serves reads from memory over two sockets
(`ports.sock` for ports, `locks.sock` for locks), and writes through to the respective
files on each mutation. Direct callers take `devkitd.lock` *shared* before any write
(`FlockStore` / `registry::with_lock`) and hard-error (`DaemonHoldsLock`) if the daemon
holds it — so a non-daemon binary can never modify the files behind a live daemon. Reads
are ungated. `devkit-locks` exposes the same `Store` seam as `devkit-ports`: `FlockStore`
is the direct flock-guarded path; `MemoryStore` is the daemon path.
