# `devrun task`: canned oneshot commands with registry-aware templating

## Problem

Recurring multi-step flows — the motivating one is profiling/prod builds in
the monorepo — live only as comments in the personal config. The current
flow is four commands with three easy-to-forget nuances:

1. Build the prod API with a preset override the repo config doesn't allow
   (`NITRO_PRESET=node-server … bun nitro build`).
2. `devrun up api-prod`.
3. Build the frontend for profiling (`bun next build --profile`) with the
   API URL **baked in at build time** — Next.js embeds
   `FOUNDRY_API_BASE_URL` into client bundles, and it must point at
   api-prod's registry-allocated port.
4. `devrun up lab-os-prod --env FOUNDRY_API_BASE_URL=http://localhost:<same
   port>` — explicit, because api-prod is deliberately not the
   `provides_url` provider.

Nothing in devkit can express step 3/4's cross-app port reference:
`env_for` only wires the single global provider. So the knowledge rots in
comments and every session re-derives it. A `justfile` in the monorepo was
considered and rejected: the monorepo is a company repo and these flows are
personal tooling; the personal config is already their home — this feature
makes those comments executable.

## Design

Three phases, each landing separately. Phase 1 fixes a latent bug the
feature depends on; phase 2 is a breaking template migration; phase 3 is
the feature.

### Phase 1 — `up` idempotency (prerequisite, standalone bugfix)

Today re-running `up` for an already-running app unconditionally spawns a
second process (`run::launch`, and the daemon's `supervise_app` in
`src/bin/devkitd/server.rs`). The duplicate fails to bind, and
`insert_owned` repoints the supervision table at the doomed pid — the
crash-loop restarter then fights the original server.

Fix, in both paths: after `alloc` and before spawn, if the (holder, app,
role) row already has a **live pid**, skip the spawn and report the
existing server's state (Ready if listening, else Starting). A dead pid
(crashed) spawns as today. This makes `up` — and therefore task `up` steps
— re-runnable.

### Phase 2 — one template syntax everywhere (breaking migration)

All config-driven strings move to minijinja via the existing
`devkit_common::template::render` seam (strict undefined,
`[templates.variables]` constants underneath). Launch argv and `static_env`
values are rendered per launch with this context:

- `port` — the app's own allocated port (replaces the ad-hoc `{port}`
  substitution in `launch_argv`).
- `ports` — map of app name → this worktree's allocated port for that app,
  e.g. `{{ ports['api-prod'] }}`. Strict-undefined makes a typo'd app name
  a hard error.

`{port}` is retired. Because minijinja passes `{port}` through as *literal
text* (a missed migration launches a server with a garbage arg instead of
erroring), rendering fails if any rendered argv or env value still contains
`{port}`. Rollout updates the personal config, `docs/configuration.md`,
and README examples in the same change.

`ports` in `static_env` is what deletes nuance 4: `[apps.lab-os-prod.static_env]`
gains `FOUNDRY_API_BASE_URL = "http://localhost:{{ ports['api-prod'] }}"`
and `devrun up lab-os-prod` self-wires.

Port resolution for `ports` is **two-phase** so `template::render` stays
pure and no registry write happens mid-render:

1. Discovery render: evaluate every template for the launch/task with a
   recording `ports` object that collects referenced app names (and whether
   `port` itself was used) without touching the registry.
2. Allocate all referenced apps in one `registry::alloc` call (existing
   reserve-before-bind path, holder = current worktree root, role =
   issue), then render for real against the resulting plain map.

Referencing an app that isn't running writes the normal pid-less
reservation, which the later `up` claims — so a build can bake a port
before its server exists. Caveat (documented, accepted): the 300 s
reservation grace means an ad-hoc build followed much later by `up` could
get a different port; composite tasks that `up` before the dependent build
sidestep this entirely.

### Phase 3 — `[tasks]` + `devrun task`

One new top-level config table and one new subcommand. A task is either a
**command** (`run`) or a **sequence** (`steps`), never both.

```toml
[tasks.api-prod-build]
description = "prod nitro build (node-server preset)"
app = "api-prod"
run = ["doppler", "run", "-p", "api-foundry", "-c", "dev_local",
       "--preserve-env=NITRO_PRESET", "--", "bun", "nitro", "build"]
env = { NITRO_PRESET = "node-server" }

[tasks.lab-os-build]
description = "profiled next build, api URL baked in"
app = "lab-os-prod"
run = ["doppler", "run", "-p", "lab-os", "-c", "dev_local", "--",
       "bun", "next", "build", "--profile"]
env = { WORKCELL_BLI_RUN_WORKFLOW_ID = "asy-bli-run/v0.0.8",
        FOUNDRY_API_BASE_URL = "http://localhost:{{ ports['api-prod'] }}" }

[tasks.profile-lab-os]
description = "prod api + profiled lab-os, wired together"
steps = [
  { task = "api-prod-build" },
  { up = "api-prod" },
  { task = "lab-os-build" },
  { up = "lab-os-prod" },
]
```

Fields:

- `description` (optional) — shown by the listing; this is how an agent
  rediscovers the canned flows.
- `app` (optional) — command tasks only. Runs in that app's directory and
  inherits its `static_env`. Absent → runs at the worktree root with no
  inherited env.
- `run` — argv array, minijinja-rendered (argv and `env` values) with the
  phase-2 context.
- `env` — extra environment, layered *over* the app's `static_env`.
- `steps` — sequence entries, each `{ task = "<command-task>" }` or
  `{ up = "<app>" }`. A step may not reference another sequence task
  (no nesting, no cycles, v1). A sequence task carries only `description`
  and `steps` — `app`, `run`, and `env` on it are errors.

Command tasks do **not** get `env_for`'s provider url-wiring; a task that
needs another app's URL says so explicitly with `{{ ports['…'] }}`.

`[tasks]` merges across config layers exactly like `[apps]` (field-wise
deep merge with provenance).

**CLI.** `devrun task` lists all tasks (name, kind, app, description).
`devrun task <name>` runs one. `devrun task <name> --dry-run` prints the
rendered plan (cwd, argv, env, resolved ports) without executing, mirroring
`up --dry-run` — this is also the template-debugging tool. Like
`up --dry-run`, it still resolves ports (writing pid-less reservations if
absent) so the printed plan shows the real values. `--env
KEY=VAL` overlays on top of the task's env, mirroring `up`.

**Execution semantics.**

- Command task: render (two-phase port resolution) → `assert_not_prd` on
  the rendered argv, resolved from the task's cwd — the same guard that
  covers every other launch path → spawn in the foreground with inherited
  stdio → propagate the exit code. Env layering, low to high: app
  `static_env` → task `env` → CLI `--env`.
- Sequence task: steps run in order; the first failure stops the sequence
  and `devrun task` exits with that step's code. `up` steps are the same
  code path as `devrun up <app>` for the current worktree, issue role,
  `wait = true` (a later build step may call the server during
  prerendering), and are no-ops for a live server per phase 1.
- Roles: tasks always resolve issue-role allocations for the current
  worktree. No `--role` flag until a baseline flow materialises.

**Errors** (all hard, before any spawn where possible): unknown task name;
`run` and `steps` both present or both absent; unknown `app`; step
referencing an unknown task, a sequence task, or an unknown app; template
errors (strict undefined, leftover `{port}`).

**Placement.** Task model + parsing in `devkit-ports::config`; execution
planning next to `launch`/`env_for` in `devkit-ports::run` (the seam CLI,
MCP, and daemon share); the subcommand in `src/bin/devrun`. `completions`
picks the new subcommand up via clap as usual.

## Out of scope (v1)

- MCP `devrun.task` handler — builds run for minutes and would block a
  stdio tool call with no streaming; agents drive `devrun task` through
  their shell. Revisit only for a shell-less consumer.
- Nested/parallel/conditional steps, `down` steps.
- Baseline-role tasks (`--role`).
- Extending `[apps].setup` (issue-setup-time, wrong lifecycle) or an
  arbitrary `devrun exec <app> -- …` passthrough (doesn't fix the
  forgetting problem; unbounded surface).

## Testing

TDD throughout; `cargo test --workspace` stays the gate.

- Phase 1: registry + fake-pid tests proving `up` on a live pid skips the
  spawn in both the direct and daemon paths, and still spawns on a dead
  pid. Poll for state, never fixed sleeps (Windows CI).
- Phase 2: render-context tests (`port`, `ports`, variables precedence);
  discovery pass collects referenced apps without registry writes;
  leftover-`{port}` guard; end-to-end `up --dry-run` shows substituted
  argv/env.
- Phase 3: config parse/roundtrip/layer-merge for `[tasks]`; validation
  errors above; env layering order; sequence stop-on-first-failure;
  `assert_not_prd` rejects a prd doppler task; `--dry-run` renders and
  reserves but never spawns.
