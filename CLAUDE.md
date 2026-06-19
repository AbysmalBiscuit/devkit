# devkit

A Rust workspace (edition 2024) of two library crates and five binaries that coordinate
local development for a monorepo. The engine is project-agnostic; every project-specific
detail lives in `devkit.toml`. See `README.md` for user-facing CLI docs.

## Commands

```sh
cargo build --release                       # all five binaries → target/release
cargo test --workspace                       # full gate — 46 tests, must stay green
cargo clippy --workspace --all-targets -- -D warnings   # zero-warning policy
cargo test -p devkit-ports --test registry   # multiprocess flock race test
```

## Layout

| Crate | Role |
|---|---|
| `devkit-common` | shared: `paths`, `cmd` (git/gh wrappers), `worktree`, `ui` (tables/links), `linear` |
| `devkit-ports` | `config` (toml), `doppler` (yaml), `apps` (catalog), `registry` (flock'd port store), `load` |
| `portman` | CLI over the port registry |
| `devrun` | supervised dev-server runner (`env`, `supervise`, `baseline`) |
| `issue-prep` / `issue-end` | worktree setup / triage + cleanup |
| `pr-status` | GitHub PR triage with a per-repo diff cache |

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
- `anyhow` for application errors, `thiserror` for library error types.
- Example-specific values (`baseline_ref`, `FOUNDRY_API_BASE_URL`, app names) belong in
  `devkit.toml` / `configs/example.toml`, never hardcoded in the engine.

## Known warts

- `pr-status` hardcodes the `SWE-` issue prefix in `issue_of` — make it config-driven before
  reusing `pr-status` outside example.
