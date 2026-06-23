# devkit — local dev orchestration (Rust)

> **Historical snapshot (2026-06-19).** Superseded; binary/layout names here are out of date. See README.md, AGENTS.md, and docs/superpowers/specs/ for the current design.

**Status:** design approved, pending spec review
**Date:** 2026-06-19
**Repo:** `~/Git/lev/devkit`

## Problem

Four pain points in the example local-dev loop:

1. **Port collisions.** Multiple Claude instances each run "find first free port via `ss`"
   independently. Two see the same port free, both bind it (a TOCTOU race). The current
   `Port slot:` marker scanned out of `ISSUE_SUMMARY_*.md` files also races and wastes
   ports.
2. **No standard way to bring up the apps a PR needs** for testing, especially comparing
   the issue version against a baseline, with doppler-seeded env plus ad-hoc overrides.
3. **`/issue-setup` hand-runs mechanical git/env steps** through Claude, which is slow and
   error-prone.
4. **Loose tooling.** `issue-end.py` and `pr-status.py` live as standalone
   `~/.local/bin/*.py` uv-scripts and re-implement the same helpers (gh/git wrappers,
   worktree discovery, id parsing, tables) the new tools need.

## Solution overview

A **Rust workspace**, `devkit`, with a **generic, config-driven engine** and an **example
config** shipped as the working default. Two library crates hold the shared engine; five
binary crates are the commands. Single-language, fast-starting static binaries.

| Binary | Purpose |
|---|---|
| `portman` | port registry — `status` / `alloc` / `release` / `prune` |
| `devrun` | runner — `up` / `down` / `status` / `logs` |
| `issue-prep` | mechanical slice of `/issue-setup` (worktree + env + reserve ports) |
| `issue-end` | rewritten from `~/.local/bin/issue-end.py` |
| `pr-status` | rewritten from `~/.local/bin/pr-status.py` |

### Key decisions (resolved during brainstorming)

1. **Port coordination: flock'd JSON registry, no daemon.** A single registry file guarded
   by an advisory file lock; allocate/release/prune are atomic under the lock. The recorded
   reservation row — not a long-held lock — is what prevents collisions, so the lock is
   held only for fast registry mutations, never across spawn or readiness polling. Liveness
   is lazy-pruned (dead holder / dead pid / expired reservation). A daemon was rejected as
   more machinery than the race requires.
2. **Baseline = a local `origin/staging` worktree, run side-by-side.** A dedicated
   throwaway worktree (default `~/Git/example/_baseline`) brings up the same in-scope apps
   on a second port set for faithful local-vs-local A/B.
3. **Supervised processes.** The runner spawns servers detached, records pid + logfile +
   port in the registry, and supports `up`/`down`/`status`/`logs`. `down` is the
   deterministic deallocation path (kill tracked pids → release ports). A per-machine
   process manager, **not** a resident daemon.
4. **Language: Rust.** Correctness-sensitive, frequently run, and the config-driven catalog
   keeps the volatile data in `devkit.toml` so recompiles are only for engine changes.
   `issue-end`/`pr-status` are rewritten in Rust too for a coherent single-language toolchain
   (accepting the rewrite of ~800 lines of working Python).
5. **Modest workspace: two lib crates + thin bin crates.** The two real domains — the
   port engine (`fd-lock`+`nix`) and the GitHub/Linear tooling (`ureq`) — get dependency
   isolation over a shared `devkit-common`. Bins link the libs directly, so `devrun` calling
   the registry is an in-process function call, not a subprocess. Avoids both the
   shared-everything dependency set of a single crate and the over-plumbing of a
   crate-per-module split.

## Repository & install

- **Home:** `~/Git/lev/devkit`, its own git repo, private GitHub remote (`gh repo create`).
- **Install:** `cargo install --path crates/<bin>` per tool, or all at once from the
  workspace. Binaries land on `~/.cargo/bin` (on `PATH`). For live iteration, `cargo build`
  + run from `target/`.
- **Shell completion:** generated via `clap_complete` (replaces the Python click
  completion-var pin).

## Workspace layout

```
~/Git/lev/devkit/
├── Cargo.toml                      # [workspace] members + shared [workspace.dependencies]
├── crates/
│   ├── devkit-common/              # lib: gh/git/doppler wrappers, worktree discovery,
│   │                               #      issue-id parse, Linear GraphQL client, XDG paths,
│   │                               #      table/style/OSC8 helpers
│   ├── devkit-ports/               # lib: registry, config, apps   (deps: common, fd-lock, nix)
│   ├── portman/                    # bin → ports
│   ├── devrun/                     # bin → ports (+ nix for setsid/signals)
│   ├── issue-prep/                 # bin → ports
│   ├── issue-end/                  # bin → common (+ ureq for Linear)
│   └── pr-status/                  # bin → common
├── configs/example.toml            # working default config
└── README.md
```

Crate-level tests live with each crate; the registry race test is
`crates/devkit-ports/tests/registry.rs`.

### Dependencies (mature, minimal; pinned in `[workspace.dependencies]`)

`clap` (derive) · `serde` + `serde_json` (registry) · `toml` (config) · `serde_yaml`
(doppler.yaml) · `fd-lock` (advisory flock) · `comfy-table` + `anstyle` + `supports-hyperlinks`
(tables, color, OSC8 clickable links — parity with the current tools) · `ureq` (blocking
HTTP for the Linear GraphQL query) · `nix` (`setsid` to detach, `kill(pid,0)` liveness,
signals for `down`) · `anyhow` (binary error context) + `thiserror` (library error types).
Port-listening checks and readiness polling use `std::net`; timestamps/age use
`std::time::SystemTime`. Each bin pulls only its domain lib's closure, so `portman` never
links `ureq` and `pr-status` never links `fd-lock`/`nix`.

## Components

### `devkit-ports::config` — config loader

Discovery order: `--config` flag → `$DEVKIT_CONFIG` → `./devkit.toml` (walking up) →
`~/.config/devkit/config.toml`. Schema:

```toml
[defaults]
worktree_root  = "~/Git/example"
branch_prefix  = "lev/"
baseline_ref   = "origin/staging"
baseline_path  = "~/Git/example/_baseline"
doppler_config = "dev_local"
doppler_yaml   = "~/Git/example/monorepo/doppler.yaml"   # project+path source for apps

[apps.api]
base_port    = 9100
launch       = ["nitro", "dev", "--port", "{port}"]
url_env      = "NEXT_PUBLIC_API_URL"     # how OTHER apps reach this one (value http://localhost:{port})
preserve_env = ["SUPABASE_JWT_SECRET"]
static_env   = { SUPABASE_JWT_SECRET = "super-secret-jwt-token-with-at-least-32-characters-long" }

[apps.lab-os]
base_port  = 4100
launch     = ["next", "dev", "-p", "{port}"]
static_env = { WORKCELL_BLI_RUN_WORKFLOW_ID = "dummy" }

[apps.foundry-portal]
base_port = 4200
launch    = ["next", "dev", "-p", "{port}"]
```

`doppler_project` and `path` per app come from `doppler.yaml` (matched by app name → path →
project). The config only carries what `doppler.yaml` lacks. Deserialized into typed structs
via serde.

### `devkit-ports::apps` — catalog

Merges config + `doppler.yaml` into an `App` struct: `name, base_port, doppler_project, path,
launch, url_env, static_env, preserve_env`. Single source of truth, replacing the tables
currently duplicated across `validate-webapp.md`, `issue-setup.md`, and CLAUDE.md.

### `devkit-ports::registry` — the shared core

State: `~/.claude/state/devkit/ports.json` + `ports.lock`. Entry struct:
`{ port, app, holder, role, pid: Option<u32>, logfile: Option<PathBuf>, ts }` where `holder`
= canonical worktree path (or scratch label), `role` ∈ `Issue | Baseline`.

- **`alloc(holder, apps, role) -> Vec<(App, port)>`** — under lock: prune; per app pick
  `base`, increment past any port that is registry-claimed *or* OS-listening; bind-test;
  write entry with `pid = None`. Idempotent: existing holder+app+role returns its current
  port (re-running `up` reuses).
- **`record_pid(holder, app, role, pid, logfile)`** — fill in pid/logfile after spawn.
- **`release(holder, role: Option<Role>) -> Vec<port>`** — drop matching entries. Does not
  kill processes.
- **`prune()`** — drop an entry when its holder path is gone, its pid is dead
  (`kill(pid,0)` → ESRCH), or it has `pid = None` + nothing listening + age > 120 s
  (reservation grace).
- helpers: `listening(port)`, `pid_alive(pid)`, `holder_alive(path)`.

The lock is an RAII guard (`fd-lock`) dropped at end of scope; mutation = read JSON → modify
→ write under the held guard.

### `portman` binary

`portman [status]` → `comfy-table` (PORT, APP, ROLE, HOLDER/issue-id, PID, LISTENING, AGE)
across all holders · `portman alloc --holder P --role issue api lab-os` ·
`portman release --holder P [--role]` · `portman prune` · `-C/--dir` derives holder from cwd.

### `devrun` binary — the runner

Resolves worktree (holder + issue id) from cwd via `devkit_common::discover`.

- **`devrun up [apps…] [--role issue|baseline|both] [--env K=V]… [--env-file F] [-- extra args] [--dry-run]`**
  - resolve apps: args → `git diff origin/staging...HEAD --stat` app paths → ask; auto-add
    api if a webapp needs it.
  - `registry::alloc` ports → assemble env per app, precedence low→high: **doppler
    `dev_local`** → app `static_env` → **computed url-wiring** (set each consumer's `url_env`
    to this role's local api port — the false-negative trap from validate-webapp, handled
    automatically) → user `--env`/`--env-file`.
  - spawn detached (`setsid` via `nix`; stdout/stderr → `~/.claude/state/devkit/logs/<holder>/<role>-<app>.log`),
    `record_pid`, poll readiness (`std::net`), report per-app PASS/FAIL (FAIL tails the log;
    process left running for inspection).
  - `--role both`: bring up issue + baseline on two port sets; baseline first ensures the
    baseline worktree exists and is `fetch`ed + `reset --hard origin/staging` (loud about
    what it did).
  - `--dry-run`: print the resolved plan (argv + env + ports), spawn nothing.
- **`devrun down [--role|--all]`** — signal tracked pids (`nix::kill`), then `registry::release`.
- **`devrun status`** — registry view, optionally filtered to this holder.
- **`devrun logs <app> [--role] [-f]`** — tail the logfile.

### `issue-prep` binary

`issue-prep --issue ENG-1234 --slug eng-1234-… [--apps api,lab-os]`: `git fetch` +
`worktree add -b lev/<slug> origin/staging`, symlink `.env.local` per app, write lab-os
dummy env, one `bun install`, **reserve ports via the registry**, print JSON
(worktree, branch, ports) for the `/issue-setup` command to fold into its summary. Supports
`--dry-run`.

Replaces the `Port slot:` file-scanning in `issue-setup.md`: the registry is now the port
source of truth. Old `ISSUE_SUMMARY_*.md` files remain readable; new ones list reserved
ports. The `/issue-setup` command keeps the MCP/judgment work (Linear, Sentry, summary
prose) and calls `issue-prep` for the deterministic steps.

### `issue-end` / `pr-status` binaries (rewritten in Rust)

Port the existing Python behavior onto `devkit-common`:

- **`issue-end`** — worktree discovery, one bulk `gh pr list --state all --json …` matched by
  head branch, batched Linear GraphQL "Done" gate via `ureq`, `comfy-table` status table,
  and `status`/`clean`/`--clean-worktree` subcommands. The `issue-end-cleanup.sh` logic
  (`git worktree remove`, branch delete, `rm ISSUE_*.md`, refuse-from-inside guard) is
  **reimplemented in Rust** — no shell asset to ship.
- **`pr-status`** — authored + review-requested PR tables via `gh`, the before→after diff
  cache under `$XDG_CACHE_HOME/devkit/pr-status/`, OSC8 links, `comfy-table` rendering.

## Error handling

- doppler not authed / project missing → surface its stderr, fail that app, don't
  half-start the stack.
- corrupt registry JSON → back up + reinit with a warning.
- lock contention → block with ~10 s timeout, then error.
- readiness timeout → mark FAIL, tail the log, leave the process running.
- **Never** run `doppler … -c prd`. doppler config comes from `defaults.doppler_config`,
  validated against a `prd` denylist.
- Binaries use `anyhow` for context-rich top-level errors; the libs use `thiserror` enums so
  callers can match.

## Testing

- `crates/devkit-ports/tests/registry.rs` is the real one: spawn concurrent allocs (threads
  and/or forked child processes) against a temp registry and assert no port is ever
  double-assigned; assert prune drops dead-holder / dead-pid / expired rows; assert release
  frees; assert alloc idempotency. Simulate "listening" by binding real `TcpListener`s on
  ephemeral ports.
- `--dry-run` gives `devrun`/`issue-prep` a no-spawn path that integration tests assert
  against (resolved argv + env).
- Pure functions (config merge, env layering, id parsing) get unit tests next to their
  modules.

## Build order

1. Scaffold the workspace (`Cargo.toml`, `devkit-common` + `devkit-ports` skeletons,
   `[workspace.dependencies]`); implement `devkit-common`; `config` + `apps` in
   `devkit-ports` (with unit tests for config/doppler merge).
2. `registry` + `portman` + `crates/devkit-ports/tests/registry.rs`.
3. `devrun` — verify the api-URL env var name + launch argv against the live repo here.
4. `issue-prep`.
5. Rewrite `issue-end` + `pr-status` onto `devkit-common`; reimplement cleanup logic in Rust;
   retire the old `~/.local/bin/*.py` and `~/.claude/scripts/*.sh`.
6. Rewrite `/validate-webapp` and `/issue-setup` commands to call these binaries instead of
   inlining port logic.

## Open questions (for spec review)

1. **api-URL env var name** — assumed `NEXT_PUBLIC_API_URL`; to be confirmed by grepping
   lab-os/foundry-portal at build time (step 3). Confirm if you already know it.
2. **plate-api / website** — not fully specced in the docs (`plate-api` launch likely
   `PORT={port} bun dev`; `website` base 4300). Defer their catalog entries to build-time
   verification, or out of scope?
3. **Old-script retirement** — OK to delete `~/.local/bin/{issue-end,pr-status}.py`,
   `~/.local/bin/pr-status.sh`, and `~/.claude/scripts/{issue-end-cleanup,issue-end-scan}.sh`
   once the Rust binaries replace them?
4. **Baseline reset** — confirmed default is auto `reset --hard origin/staging` on the
   throwaway `_baseline` tree each `--role both` run, printed loudly, no prompt.
