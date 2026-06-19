# devkit — local dev orchestration package

**Status:** design approved, pending spec review
**Date:** 2026-06-19

## Problem

Three pain points in the example local-dev loop, plus consolidation of existing tooling:

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
   worktree discovery, id parsing, rich tables) the new tools need.

## Solution overview

One installable Python package, `devkit`, with a **generic, config-driven engine** and an
**example config** shipped as the working default. It exposes five console commands and
replaces the per-script port logic with a single flock'd registry.

| Command | Purpose |
|---|---|
| `portman` | port registry — `status` / `alloc` / `release` / `prune` |
| `devrun` | runner — `up` / `down` / `status` / `logs` |
| `issue-prep` | mechanical slice of `/issue-setup` (worktree + env + reserve ports) |
| `issue-end` | migrated from `~/.local/bin/issue-end.py` |
| `pr-status` | migrated from `~/.local/bin/pr-status.py` |

### Key decisions (resolved during brainstorming)

1. **Port coordination: flock'd JSON registry, no daemon.** A single registry file guarded
   by an `flock`; allocate/release/prune are atomic under the lock. The recorded
   reservation row — not a long-held lock — is what prevents collisions, so the lock is
   held only for fast registry mutations, never across spawn or readiness polling.
   Liveness is lazy-pruned (dead holder / dead pid / expired reservation). A daemon was
   rejected as more machinery than the race requires.
2. **Baseline = a local `origin/staging` worktree, run side-by-side.** A dedicated
   throwaway worktree (default `~/Git/example/_baseline`) brings up the same in-scope apps
   on a second port set for faithful local-vs-local A/B.
3. **Supervised processes.** The runner spawns servers detached, records pid + logfile +
   port in the registry, and supports `up`/`down`/`status`/`logs`. `down` is the
   deterministic deallocation path (kill tracked pids → release ports). This is a
   per-machine process manager, **not** a resident daemon.
4. **One uv package, generic + config-driven.** The engine is project-agnostic; all
   example specifics live in `devkit.toml`, with app project+path auto-pulled from the
   monorepo's `doppler.yaml`. `devrun` imports the registry (rather than shelling out to
   `portman`) so it shares the readiness/liveness helpers and keeps registry mutations in
   one place.

## Repository & install

- **Home:** `~/Git/devkit`, its own git repo, private GitHub remote (`gh repo create`).
- **Install:** `uv tool install --editable .` from the clone → puts the five commands on
  `PATH` and tracks edits live.
- This design doc is authored in `~/.claude/docs/...` (the devkit repo does not exist
  until implementation); it moves into the repo as part of the build.

## Package layout

```
~/Git/devkit/
├── pyproject.toml              # [project.scripts]: portman, devrun, issue-prep, issue-end, pr-status
├── devkit/
│   ├── common.py               # Console/err, gh(), git(), scan_progress(), worktree discover(),
│   │                           #   issue_id_of(), Linear GraphQL client, XDG state/cache paths
│   ├── config.py               # devkit.toml loader + doppler.yaml merge (~60 lines)
│   ├── apps.py                  # App catalog assembled from config
│   ├── registry.py             # flock'd port registry — the shared core (~100 lines)
│   ├── portman.py              # CLI over the registry
│   ├── devrun.py               # runner CLI
│   ├── issue_prep.py           # mechanical issue-setup
│   ├── issue_end.py            # migrated (repoint cleanup-script path to importlib.resources)
│   ├── pr_status.py            # migrated
│   └── data/issue-end-cleanup.sh   # battle-tested; shipped as package data
├── configs/example.toml        # working default config
├── tests/test_registry.py      # race-correctness tests
└── README.md
```

## Components

### `config.py` — config loader

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

`doppler_project` and `path` per app come from `doppler.yaml` (matched by app name → path
→ project). The config only carries what doppler.yaml lacks.

### `apps.py` — catalog

Merges config + doppler.yaml into frozen `App` records: `name, base_port, doppler_project,
path, launch, url_env, static_env, preserve_env`. Single source of truth, replacing the
tables currently duplicated across `validate-webapp.md`, `issue-setup.md`, and CLAUDE.md.

### `registry.py` — the shared core

State: `~/.claude/state/devkit/ports.json` + `ports.lock`. Entry:
`{ "<port>": {app, holder, role, pid, logfile, ts} }` where `holder` = canonical worktree
path (or scratch label), `role` ∈ `issue|baseline`.

- **`alloc(holder, apps, role)`** — under lock: prune; per app pick `base`, increment past
  any port that is registry-claimed *or* OS-listening; bind-test; write entry with
  `pid=null`. Idempotent: existing holder+app+role returns its current port (re-running
  `up` reuses).
- **`record_pid(holder, app, role, pid, logfile)`** — fill in pid/logfile after spawn.
- **`release(holder, role=?)`** — drop matching entries, return freed ports. Does not kill
  processes.
- **`prune()`** — drop an entry when its holder path is gone, its pid is dead, or it has
  `pid=null` + nothing listening + age > 120 s (reservation grace).
- helpers: `listening(port)`, `pid_alive(pid)`, `holder_alive(path)`.

### `portman` CLI

`portman [status]` → rich table (PORT, APP, ROLE, HOLDER/issue-id, PID, LISTENING, AGE)
across all holders · `portman alloc --holder P --role issue api lab-os` ·
`portman release --holder P [--role]` · `portman prune` · `-C/--dir` derives holder from cwd.

### `devrun` CLI — the runner

Resolves worktree (holder + issue id) from cwd via `common.discover`.

- **`devrun up [apps…] [--role issue|baseline|both] [--env K=V]… [--env-file F] [-- extra args] [--dry-run]`**
  - resolve apps: args → `git diff origin/staging...HEAD --stat` app paths → ask; auto-add
    api if a webapp needs it.
  - `registry.alloc` ports → assemble env per app, precedence low→high:
    **doppler `dev_local`** → app `static_env` → **computed url-wiring** (set each
    consumer's `url_env` to this role's local api port — the false-negative trap from
    validate-webapp, handled automatically) → user `--env`/`--env-file`.
  - spawn detached (`setsid`; output → `~/.claude/state/devkit/logs/<holder>/<role>-<app>.log`),
    `record_pid`, poll readiness, report per-app PASS/FAIL (FAIL dumps last log lines;
    process left running for inspection).
  - `--role both`: bring up issue + baseline on two port sets; baseline first ensures the
    baseline worktree exists and is `fetch`ed + `reset --hard origin/staging` (loud about
    what it did).
  - `--dry-run`: print the resolved plan (argv + env + ports), spawn nothing.
- **`devrun down [--role|--all]`** — kill tracked pids, then `registry.release`.
- **`devrun status`** — registry view, optionally filtered to this holder.
- **`devrun logs <app> [--role] [-f]`** — tail the logfile.

### `issue-prep` CLI

`issue-prep --issue ENG-1234 --slug eng-1234-… [--apps api,lab-os]`: `git fetch` +
`worktree add -b lev/<slug> origin/staging`, symlink `.env.local` per app, write lab-os
dummy env, one `bun install`, **reserve ports via the registry**, print JSON
(worktree, branch, ports) for the `/issue-setup` command to fold into its summary.
Supports `--dry-run`.

This **replaces the `Port slot:` file-scanning** in `issue-setup.md`: the registry is now
the port source of truth. Old `ISSUE_SUMMARY_*.md` files remain readable; new ones just
list reserved ports. The `/issue-setup` command keeps the MCP/judgment work (Linear,
Sentry, summary prose) and calls `issue-prep` for the deterministic steps.

### Migrated tools

`issue_end.py` and `pr_status.py` move in largely as-is, refactored to import shared
helpers from `common.py`. `issue_end.py`'s hardcoded `~/.claude/scripts/issue-end-cleanup.sh`
path repoints to `importlib.resources` (the script ships in `devkit/data/`). The
shell-completion env-var pin (`_ISSUE_END_COMPLETE`, `_PR_STATUS_COMPLETE`) carries over per
command.

## Error handling

- doppler not authed / project missing → surface its stderr, fail that app, don't
  half-start the stack.
- corrupt registry JSON → back up + reinit with a warning.
- lock contention → block with ~10 s timeout, then error.
- readiness timeout → mark FAIL, dump last N log lines, leave the process running.
- **Never** run `doppler … -c prd`. doppler config comes from `defaults.doppler_config`,
  validated against a `prd` denylist.

## Testing

- `tests/test_registry.py` is the real one: spawn concurrent allocs (multiprocessing)
  against a temp registry and assert no port is ever double-assigned; assert prune drops
  dead-holder / dead-pid / expired rows; assert release frees; assert alloc idempotency.
  Simulate "listening" by binding real sockets on ephemeral ports.
- `--dry-run` gives `devrun`/`issue-prep` a no-spawn path that tests assert against
  (resolved argv + env).

## Build order

1. `common.py` + `config.py` + `apps.py` (with `tests` for config/doppler merge).
2. `registry.py` + `portman` + `tests/test_registry.py`.
3. `devrun` (verify api-URL env var + launch argv against the live repo here).
4. `issue-prep`.
5. Migrate `issue-end` + `pr-status` onto `common.py`; retire the `.sh` duplicates and the
   old `~/.local/bin` copies.
6. Rewrite `/validate-webapp` and `/issue-setup` commands to call these tools instead of
   inlining port logic.

## Open questions (for spec review)

1. **api-URL env var name** — assumed `NEXT_PUBLIC_API_URL`; to be confirmed by grepping
   lab-os/foundry-portal at build time (step 3). Confirm if you already know it.
2. **plate-api / website** — not fully specced in the docs (`plate-api` launch likely
   `PORT={port} bun dev`; `website` base 4300). Defer their catalog entries to build-time
   verification, or out of scope?
3. **Migration of old scripts** — OK to delete `~/.local/bin/{issue-end,pr-status}.py`,
   `~/.claude/scripts/{issue-end-scan,pr-status}.sh`, and `~/.local/bin/pr-status.sh` once
   the uv tool provides replacements? (`issue-end-cleanup.sh` is kept as package data.)
4. **Baseline reset** — confirmed default is auto `reset --hard origin/staging` on the
   throwaway `_baseline` tree each `--role both` run, printing what it did (no prompt).
