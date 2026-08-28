# devkit

A Rust workspace (edition 2024): a root `devkit` binary package whose subcommands
cover the CLI surface, plus the separate `devkitd` daemon and the library crates,
coordinating local development for a monorepo. The engine is project-agnostic;
every project-specific detail lives in `devkit.toml`. See `README.md` for
user-facing CLI docs.

## Commands

```sh
cargo build --release                       # devkit, devkitd → target/release
cargo install --path .                       # install devkit, devkitd into ~/.cargo/bin
cargo test --workspace                       # full gate — must stay green
cargo clippy --workspace --all-targets -- -D warnings   # zero-warning policy
cargo test -p devkit-ports --test registry   # multiprocess flock race test
```

Run all three before committing: CI runs them on every push and PR, and a push to `main`
also drives release-please. Format with `cargo fmt --all` (the `--check` above only
verifies) using the stable toolchain CI uses, so formatting matches.

## Layout

The workspace root is the `devkit` binary package; it and `devkitd` install
together via `cargo install --path .`. The library crates are members.

| Unit | Role |
|---|---|
| `crates/devkit-config` | lib: the `devkit.toml` shape — layer discovery and merge, `${VAR}` expansion and layer-relative path resolution, per-leaf provenance, and the `JsonSchema` derives `devkit schema` renders. A leaf crate with no internal *library* dependencies (its dev-dependencies pull in `devkit-common` for git fixtures in tests only), so `devkit-common` and `devkit-ports` both depend on it |
| `crates/devkit-common` | shared lib: `git`, the single door every git invocation in the workspace goes through, scrubbing the environment variables that could redirect a call at another repository or inject config into it; `paths`, `secrets`, `cmd` (a generic subprocess-capture helper, plus `gh` wrappers) with `github` and `gitfetch`, `worktree` plus the `record` it reads (`.devkit/issue.toml`) and `gitignore`, `slug`, `template`, `ui` (tables/links) with `livetable` and `progress` (TTY-only spinners), `tracker` (the `Tracker` seam and its `linear`, `github` and `none` implementations), `slack`, `store` (flock'd JSON documents), `supervise`, `sys` (the platform boundary), `timing`, `report`, and a `daemon` client behind the `daemon` feature |
| `crates/devkit-ports` | lib: `doppler` (yaml), `apps` (catalog), `load` (config + catalog), `registry` (flock'd port store), `run` (server lifecycle), `strays` (servers outside the registry), `daemon`, `task` (canned oneshot resolution/exec) |
| `crates/devkit-locks` | file-lock registry: model + flock'd JSON store |
| `crates/devkit-issue` | lib: read-only issue triage facade — `status` (worktree + PR + tracker state with the finished verdict) and `prs` (PR triage); serializable, no rendering, no mutations |
| `crates/devkit-mcp` | lib: stdio MCP server (`jsonrpc`, action `registry`, `ports`/`locks`/`devrun`/`issue` handlers) over the port + lock facades, the `devkit-ports::run` server-lifecycle facade, and the `devkit-issue` triage facade |
| `crates/devkit-docs` | lib: version-correct library checkouts — manifest (global `docs.toml` + `devkit.toml` `[docs]`), importer-graph resolution (pnpm/bun/npm/Cargo/uv) matched to git tags, hard-error failure modes instead of a silent default-branch fallback (opt in per run with `--allow-default-branch`), bare-clone cache with ref-named worktrees (`/` encoded as `~`) under a reserved-stem-checked cache root, flock'd reference registry with reference-based prune, per-checkout pins that roll up a workspace root's members (JS lockfiles only — cargo and uv name members in a manifest) and union in the reference registry's rows for this project, and 0.12.x cache migration that moves the layout but hard-errors on a `meta.toml` it cannot parse, naming every such library in one run |
| `src/bin/devkit/` | the merged CLI: `auth` (validate + store Linear/Slack tokens; `auth github` instead *reports* the identity behind the token `GH_TOKEN`/`GITHUB_TOKEN`/`gh auth token` resolve, storing nothing and refusing a `--token`), `doctor`, `brief` (session-hook project summary, silent outside a devkit project), `schema` (JSON Schema for `devkit.toml`, derived from the config types; `schema init` points a config at it, writing a fully-commented starter when absent; `schema/devkit-config.json` is committed, a test fails with a diff when it drifts, `DEVKIT_UPDATE_SCHEMA=1 cargo test` rewrites it, and release-please attaches it to each GitHub Release), `install-links`, and the operational subcommands — `ports` (port registry), `run` (supervised dev-server runner: `env`, `supervise`, `baseline`, `task`; `reap` kills servers started outside it), `issue` (issue lifecycle: `setup`, `checkout-pr`, `status`, `info`, `end`, `sync-includes`, `prs`, `dashboard`, `review`), `locks` (advisory file locks), `docs` (docs cache: `add`, `rm`, `list`, `sync`, `path`, `info`, `forget`, `prune`), and `mcp` (stdio MCP server exposing the port + lock facades to coding agents). Each operational subcommand is also reachable under a short name of its own (`portm`, `devrun`, `issue`, `lockm`, `docm`, `devkit-mcp`) through a hardlink `devkit install-links` creates beside the binary |
| `src/bin/devkitd` | supervisor daemon serving both the port registry (`ports.sock`) and the lock registry (`locks.sock`), authoritative in memory, write-through to the files, gated by `devkitd.lock`; bin gated by the `daemon` feature (on by default) |

`devkit` and its `ports`, `run`, `issue`, `locks`, and `docs` subcommands each
expose a `completions <shell>` subcommand. The shell argument is
`devkit::completions::Shell`, not `clap_complete::Shell`: the latter is closed and
has no nushell variant, so the shared enum adds one and forwards each variant to
whichever crate owns that generator (`clap_complete` for five, the first-party
`clap_complete_nushell` for nushell). Its value strings match `clap_complete::Shell`'s,
so adding a shell must not rename an existing one.

Every completion script goes out through `completions::emit`, which runs
`Generator::try_generate` rather than `clap_complete::generate`. The latter
panics when the write fails, so a reader that closes the pipe early (`… | head`)
crashes the writer; `emit` treats a broken pipe as the reader being done.
`devkit completions --all` emits one script per name through the same call, and
reads the set of names off the command tree (any `SHIMS` entry whose subcommand
has a `completions` of its own) rather than from a second list.

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
  terminal (`cmd_down` in `src/bin/devkit/run/mod.rs`), and is reachable only via the
  named scope flags `--all`/`--others`/`--holder` — so an agent (no PTY) cannot
  stop another worktree's servers, and a harness can deny those flags by name. The
  MCP `devrun.down` handler stays root-scoped and never gains a cross-holder arg.
- **`devrun reap` is TTY-gated with no bypass, and never on MCP.** Reap kills
  servers running outside the registry; it always requires an interactive
  terminal (no `--yes`/`--force`/env path), so an agent without a PTY cannot
  trigger it. Only read-only detection is exposed to agents — `devrun status`'s
  untracked section, `devkit doctor`'s `devrun_strays` row, and the
  `ports.strays` MCP action. No mutating reap/kill handler is ever added to the
  MCP surface.
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
  daemon and direct spawn paths. `run::assert_not_prd` is also called during
  task resolution (`task::resolve_command`), so a `devrun task` command step
  gets the same guard.
- **`up` is idempotent for a live server.** Both `run::launch` (direct path) and
  the daemon's `Supervise` handler skip the spawn when the (holder, app, role)
  row already has a live pid, reporting the existing server instead. A duplicate
  spawn would fail to bind, and on the daemon path would repoint the supervision
  table at the doomed pid. Sequence-task `up` steps rely on this.
- **Sequence steps re-resolve at execution time; the upfront pass never
  gates.** `task::resolve` validates every step before anything spawns, but
  its rendered plans are for validation and `--dry-run` display only —
  execution calls `task::resolve_step` per command step (fresh allocation +
  render, `require_live` enforced) immediately before spawning it. Don't
  execute the upfront plans: a build step longer than
  `RESERVATION_GRACE_SECS` would let a t=0 reservation expire and desync
  later steps. And don't enforce `require_live` in the upfront pass — a
  gated app may be brought up by an earlier `up` step of the same sequence.
  A CLI-path `require_live` gate failure can leave behind a grace-bounded
  pid-less reservation from the upfront validation pass; this is the
  reserve-before-bind row the error's suggested `devrun up <app>` reuses,
  not a leak.

## Conventions

- Commits follow Conventional Commits. Follow the active workflow skill's commit cadence (a design/plan skill
  commits its own artifact; per-task execution commits per task).
- TDD: write the failing test first; `cargo test --workspace` is the merge gate.
- Test scratch comes from `tempfile`: `tempfile::tempdir()` for a directory, a
  path joined onto one for a file. Never build a scratch path by hand from
  `std::env::temp_dir()` — a hand-built path outlives the test and fills `/tmp`.
  `TempDir` deletes its tree on drop, so bind it for as long as the path is used:
  a helper that returns a path derived from a guard must hand back the guard too,
  or the directory is gone before the caller reads it.
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
- The issue tracker is the `Tracker` trait in `devkit-common::tracker`.
  `tracker::resolve` picks the implementation: a resolvable `LINEAR_API_KEY`
  means Linear, else a github.com `origin` remote means GitHub, else
  `NoneTracker`, whose empty answers are how `issue` degrades. The GitHub arm is
  built from `Repos::issues` — resolve is the *only* place a `GithubTracker` is
  constructed, which is what lets its `ready` report on the token alone — and
  without an issues repository it falls back to `NoneTracker`, undeclared, with
  the failure in `reason`. `resolve` takes an explicit kind that wins over
  detection; `[tracker] kind` is where it comes from. It returns a `Resolved`, whose `declared` flag says whether the project
  named this tracker or devkit fell back to it — the finished verdict skips the
  issue-state gate only for a *declared* `TrackerKind::None`, because devkit
  finding no tracker is silence, not an answer. `reason` is prose except for one
  load-bearing part: a reason produced by detection carries the `DETECTED`
  prefix, and `unbuilt_reason` (free function, plus the `Resolved` method) reads
  its absence on an undeclared `None` as "the project named a tracker devkit
  could not build". That is what keeps such a project from being told to name
  one — so keep the prefix on every detection arm. Config loading belongs to the callers that have it — the `issue`
  subcommand's `crate::issue::tracker::select` (which returns the `Repos` alongside, since
  the two come from one config load) and the MCP `issue.status` action —
  and a config that does not load degrades to detection rather than failing the
  command. `devkit-issue` reads no config: `status::gather_local` detects with
  repositories defaulted from `origin` alone, and every other caller injects its
  tracker via `status::gather_with`.
- GitHub repositories come from `[github] issues_repo` / `pr_repo` via
  `devkit_common::github::Repos`, each key resolving independently, defaulting
  to a github.com `origin` remote, and required only where it is used. `Repos`
  is threaded to every GitHub operation rather than re-derived; the `[github]`
  table is the one config table with `deny_unknown_fields`, because a typo'd key
  silently ignored would resolve a different repository than the project
  declared. Repository-scoped `gh` calls go through `cmd::gh_json_in` /
  `cmd::gh_capture`, which append `--repo github.com/<slug>` to every argument
  vector so an ambient `GH_REPO` or `GH_HOST` cannot redirect one.
- `StateKind` (Triage/Backlog/Unstarted/Started/Completed/Canceled) is the state
  vocabulary every tracker maps onto — match it exhaustively, no `_ =>` arms.
  Only `Completed` and `Canceled` are closed.
- CI runs the `test` job (and `clippy`) on ubuntu, macos, and windows. Tests that spawn or
  reap processes must poll for the expected state, not sleep a fixed interval — a loaded
  Windows runner exits a child later than a short fixed sleep allows.
- **Every user-facing verb is a `devkit` subcommand** — credential setup and
  diagnosis (`auth`, `doctor`) alongside the operational subcommands (`ports`,
  `run`, `issue`, `locks`, `docs`, `mcp`). Each operational subcommand is also
  reachable under a short name of its own (`portm`, `devrun`, `issue`, `lockm`,
  `docm`, `devkit-mcp`) through a hardlink `devkit install-links` creates
  beside the binary. `config` stays on
  `run`. `devkitd` stays a separate binary because `devkitd_bin()` finds it as
  a sibling file and `install-service` writes its path into a systemd unit.
  Token reads resolve through `devkit-common::secrets` (env → `secrets.toml`),
  never from `config.toml`.
- **Timing:** `issue`/`devrun` accept `--timing[=trace]` / `--timing-log <FILE>`
  (or `DEVKIT_TIMING`). Timing wraps the shared IO primitives (`cmd::capture`,
  `github`, `tracker::linear::send`, `slack`) via `devkit-common::timing`; a global tracing
  layer aggregates flat spans by op and prints a stderr summary on exit. `devkitd`
  carries the same spans but has no activation flag yet.

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

Go through `registry::{alloc, record_pid, release, release_ports, snapshot, prune, listening_view,
status_table, status_table_with}` — they
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

The ports holder is the worktree **root path**, not a minted session token:
`registry::holder_alive(holder)` is `Path::new(holder).exists()`, so a holder is judged
live by whether its directory still exists. This is what makes a worktree's ports
auto-reclaim on `git worktree remove` — the holder path vanishes and `prune` frees the
rows. (Locks instead use a session-token holder with TTL/pid liveness; the two registries
intentionally differ.) Cross-worktree, an agent addresses each worktree's allocations by
that worktree's root path.
