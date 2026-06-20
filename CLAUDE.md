# devkit

A Rust workspace (edition 2024): a root `devkit` binary package whose four CLIs live
in `src/bin/`, plus two library crates, coordinating local development for a monorepo.
The engine is project-agnostic; every project-specific detail lives in `devkit.toml`.
See `README.md` for user-facing CLI docs.

## Commands

```sh
cargo build --release                       # all four binaries → target/release
cargo install --path .                       # install all four into ~/.cargo/bin
cargo test --workspace                       # full gate — 92 tests, must stay green
cargo clippy --workspace --all-targets -- -D warnings   # zero-warning policy
cargo test -p devkit-ports --test registry   # multiprocess flock race test
```

## Layout

The workspace root is the `devkit` binary package; its CLIs live in `src/bin/` and
install together via `cargo install --path .`. Two library crates are members.

| Unit | Role |
|---|---|
| `crates/devkit-common` | shared lib: `paths`, `cmd` (git/gh wrappers), `worktree`, `ui` (tables/links), `linear`, `slack`, `supervise` |
| `crates/devkit-ports` | lib: `config` (toml), `doppler` (yaml), `apps` (catalog), `registry` (flock'd port store), `load`, `daemon` |
| `src/bin/portman` | CLI over the port registry |
| `src/bin/devrun` | supervised dev-server runner (`env`, `supervise`, `baseline`) |
| `src/bin/issue` | issue lifecycle: `setup`, `status`, `end`, `prs`, `dashboard`, `review` |
| `src/bin/devkit-portd` | port-registry supervisor daemon; bin gated by the `daemon` feature (on by default) |

The three user-facing CLIs (`portman`, `devrun`, `issue`) each expose a
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
- **`prd` is rejected** as a `doppler_config` to avoid running against production secrets.

## Conventions

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

## Registry facade

Go through `registry::{alloc, record_pid, release, snapshot, prune, status_table}` — they
keep liveness syscalls (bind/stat/kill) out of the exclusive lock. Don't reintroduce
probing inside `with_lock`. This facade is also the seam a future port daemon plugs into.
