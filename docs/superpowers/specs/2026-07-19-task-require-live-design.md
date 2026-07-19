# Task live-port gating and lazy sequence resolution

**Date:** 2026-07-19
**Status:** approved

## Problem

`lab-os-profile-build-prd` bakes `FOUNDRY_API_BASE_URL` into Next's
`routes-manifest.json` at build time. The port was a hand-typed `--env` literal,
manually coupled to the port `devrun` allocates for `api-serve` at runtime.
Any drift — a restart landing on a different port, a typo — produces a build
that calls a dead or wrong port and surfaces as confusing API errors in the
browser, never as a build error.

Two incidents motivated this design:

- An `api-serve` stopped by pid (outside `devrun down`) restarted on 9290
  instead of 9291; the previously built lab-os silently pointed at a dead port.
- A hand-typed port cross-wired lab-os to a sibling worktree's server, surfacing
  as `auth_validation_error` with no hint the port was wrong.

## What already works (verified, no change needed)

- `ports['app']` task templates resolve through `registry::alloc`, which is
  **holder-scoped**: `alloc_one` reuses only a reservation whose
  (holder, app, role) all match, and the scan skips ports occupied by foreign
  rows or with a live listener. A template can not capture another worktree's
  reservation. The non-prd twin tasks (`lab-os-profile-build`,
  `apps.lab-os-serve.static_env`) already use this.
- The alloc scan probes `listening(port)`, so untracked servers started outside
  devkit can not be allocated over.

## Gaps this design closes

1. **No "must be live" semantics.** With no reservation present, `ports['app']`
   silently mints a pid-less one and the build bakes it. Correct only if the
   server comes up on it within `RESERVATION_GRACE_SECS`; silently wrong
   otherwise.
2. **Sequences resolve everything at t=0.** All steps' ports are allocated and
   rendered before step 1 runs. A build step longer than
   `RESERVATION_GRACE_SECS` (300s) lets a pid-less reservation expire
   mid-sequence, so a later `up` step can land on a different port than the one
   an earlier build step baked.

## Design

### 1. `require_live` on command tasks

`TaskConfig` gains:

```toml
[tasks.lab-os-profile-build-prd]
app = "lab-os-serve"
require_live = ["api-serve"]
run = [ ... ]
env = { FOUNDRY_API_BASE_URL = "http://localhost:{{ ports['api-serve'] }}" }
```

- Serde default empty; command tasks only. Sequence tasks still allow only
  `description` and `steps`.
- Static validation (resolve time, before anything spawns): every
  `require_live` name must exist in the catalog and be referenced by the task's
  own templates (`run`, `static_env`, task `env` — ignoring user overrides, so
  an override can never turn a valid config into a static error). An
  unreferenced entry is a config error.

### 2. Liveness gate at step execution

Immediately before a command task executes, each gated app must have a
reservation for **this holder** (worktree root, `Role::Issue`) whose pid is
alive. Checked via `registry::snapshot()` plus the existing pid-liveness probe
— liveness syscalls stay outside the exclusive lock, per the registry facade
invariant. On failure the task errors before spawning:

```
require_live: `api-serve` has no live server in this worktree (devrun up api-serve)
```

The gate never runs during the upfront validation pass or `--dry-run` — in a
sequence the gated app may be brought up by an earlier `up` step.

### 3. `--env` overrides waive the gate and the allocation

Template collection in `resolve_command` scans the **effective** env template
map: `static_env` overlaid by task `env`, minus any key present in the user's
`--env`/`--env-file` overrides. A port reference inside an overridden value is
never scanned, so it neither allocates a reservation nor arms the gate.

Consequence: `profile-lab-os-preview --env FOUNDRY_API_BASE_URL=<vercel-url>`
reuses the same build task with no live local `api-serve` and no stray
reservation, while a bare `devrun task lab-os-profile-build-prd` fails loudly
when this worktree's `api-serve` is down. `run` argv references can not be
overridden and always arm their gate.

### 4. Lazy per-step resolution in sequences

`task::resolve` keeps its full upfront pass — shape validation, unknown
task/app/port-ref errors, template render, prd guard — so static errors still
fail before anything spawns, and `--dry-run` prints this resolution. But a
sequence's `Run` steps are re-resolved (re-allocated, re-rendered, gate
enforced) immediately before each step executes, and the fresh plan is what
runs. `SeqItem::Run` carries the task name instead of a pre-rendered plan; the
re-resolution lives in `devkit-ports::task` so a future MCP surface shares it.

This closes the grace-expiry race: in `profile-lab-os`, the lab-os build step
resolves after `up api-serve` completed and renders the genuinely live port.
Allocation idempotence makes the double resolve safe. Standalone command tasks
resolve once at exec, as today.

### 5. Tests

- Holder scoping regression: with a foreign holder's reservation on
  `base_port`, resolving a task with `ports['x']` for a second holder renders
  `base_port + 1`.
- Gate: passes with a live pid; fails with no row; fails with a dead-pid row;
  skipped when every reference to the app is overridden by user env.
- Static validation: `require_live` naming an unknown app errors; naming an
  unreferenced app errors.
- Sequence laziness: a port change between steps is picked up by the later
  step's render.

### 6. Documentation

`docs/configuration.md` documents `require_live`, the override waiver, and the
lazy re-resolution semantics. AGENTS.md gains the invariant that sequence steps
re-resolve at execution time and the upfront pass never enforces liveness.

## Config changes (personal config, after release)

In `~/.config/devkit/config.toml` (outside this repo):

- `tasks.lab-os-profile-build-prd.env.FOUNDRY_API_BASE_URL` →
  `"http://localhost:{{ ports['api-serve'] }}"` plus
  `require_live = ["api-serve"]`.
- `apps.lab-os-serve-prd.static_env.FOUNDRY_API_BASE_URL` → same template,
  mirroring `lab-os-serve`.
- Optional `lab-os-assert-bake` command task (`bash -c` + `jq`): compares the
  baked `routes-manifest.json` destination against
  `{{ ports['api-serve'] }}` and curls the port. The Next-specific assertion
  stays in config; the engine stays project-agnostic.

## Out of scope

- **Pinned ports.** A pin conflicts with multi-worktree allocation (two
  worktrees can not both pin an app to one port); the fixes above make it
  unnecessary.
- **Generic post-exec assertion primitive.** Sequences already compose command
  tasks; a check is just another step.
- **MCP task surface.** `devrun task` is CLI-only today; unchanged.
