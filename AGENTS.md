# devkit

A Rust workspace (edition 2024): a root `devkit` binary package whose five CLIs live
in `src/bin/`, plus three library crates, coordinating local development for a monorepo.
The engine is project-agnostic; every project-specific detail lives in `devkit.toml`.
See `README.md` for user-facing CLI docs.

## Commands

```sh
cargo build --release                       # all five binaries → target/release
cargo install --path .                       # install all five into ~/.cargo/bin
cargo test --workspace                       # full gate — 128 tests, must stay green
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
| `src/bin/portm.rs` | CLI over the port registry |
| `src/bin/devrun` | supervised dev-server runner (`env`, `supervise`, `baseline`) |
| `src/bin/issue` | issue lifecycle: `setup`, `status`, `end`, `prs`, `dashboard`, `review` |
| `src/bin/lockm.rs` | advisory file-lock CLI |
| `src/bin/devkitd` | supervisor daemon serving both the port registry (`ports.sock`) and the lock registry (`locks.sock`), authoritative in memory, write-through to the files, gated by `devkitd.lock`; bin gated by the `daemon` feature (on by default) |

The four user-facing CLIs (`portm`, `devrun`, `issue`, `lockm`) each expose a
`completions <shell>` subcommand via `clap_complete`.

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
- **The supervisor table — not the registry row — decides crash vs. stop.** A child the
  `devkitd` supervision thread reaps is a crash and is restarted (within the crash-loop
  budget); an intentional `Down` removes the key from the table *before* signalling the
  child, so a stopped server is never reaped as a crash. Don't make the restart decision
  read `ports.json`/`d.ports` — a concurrent prune would race it.
- **`prd` is rejected** as a `doppler_config` to avoid running against production secrets.

## Conventions

- Commits follow Conventional Commits. Follow the active workflow skill's commit cadence (a design/plan skill
  commits its own artifact; per-task execution commits per task).
- TDD: write the failing test first; `cargo test --workspace` is the merge gate.
- `anyhow` everywhere — its `.context()` chain and backtrace are the error-reporting
  mechanism. Each binary installs `report::install_panic_hook` for crash diagnostics;
  `RUST_BACKTRACE=1` adds a backtrace to both errors and panics.
- App conventions are config-driven, never hardcoded: the URL-providing app is marked
  `provides_url`; per-app prep files come from `prep_env`; the apps directory is
  `defaults.apps_dir`. Example-specific values live in the personal config at
  `~/.config/devkit/config.toml` (outside the repo; see `docs/configuration.md`).
- `Role` (Issue/Baseline) is defined once in `devkit-ports::registry` with `ValueEnum` +
  `Display`; `devrun`'s CLI uses a separate `RoleSelector` (adds `Both`). No `_ => Issue`
  catch-alls — map roles exhaustively.
- CI runs the `test` job (and `clippy`) on ubuntu, macos, and windows. Tests that spawn or
  reap processes must poll for the expected state, not sleep a fixed interval — a loaded
  Windows runner exits a child later than a short fixed sleep allows.

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
