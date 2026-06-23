# Optional doppler via full launch templating

**Date:** 2026-06-23
**Status:** approved (design)

## Summary

devkit hardcodes Doppler into every server launch: `run::plan_group` prepends a
`doppler run -c <config> [-p <project>] [--preserve-env=K]… --` prefix it builds
from `[defaults].doppler_config`, the per-app `doppler_project`, and
`preserve_env`. A project that does not use Doppler cannot run a server, so the
README lists `doppler` as a hard requirement.

This change removes the built prefix. An app's `launch` becomes the **complete,
templated command** that devkit runs verbatim — including the Doppler invocation
when the project wants one. Doppler stays a first-class integration through two
read-only behaviors devkit keeps: the `prd` safety guard (now resolved by
reading the launch command, the app's env, and — as a last resort — asking the
Doppler CLI) and `doppler.yaml` path inference. After this, the only hard
requirements are `git` and `gh`.

## Motivation

- The engine claims to be project-agnostic ("every project-specific detail lives
  in `devkit.toml`"), yet the Doppler wrapper is compiled in.
- Users must repeat nothing today, but they also cannot opt out: a non-Doppler
  app is impossible.
- The `launch` field already templates `{port}`; extending it to hold the whole
  command is the smallest conceptual change that removes the coupling.

## Goals

- `launch` is the full argv, run verbatim with `{port}` substituted. devkit
  builds no command prefix.
- Apps that name neither `doppler` nor any specific runtime in `launch`/`setup`
  need none installed. Only `git` and `gh` remain hard requirements.
- The `prd` guard survives: a launch that invokes Doppler against the `prd`
  config is rejected.
- Doppler stays first-class: `doppler.yaml` path inference and config resolution
  remain.

## Non-goals

- No backward-compatibility shim. `[defaults].doppler_config` is removed, not
  deprecated; existing personal configs must update their `launch` arrays. (The
  config is personal and external to the repo, so this is a one-time manual
  edit, not a fleet migration.)
- No change to env layering (`static_env`, `url_env`/`provides_url` wiring) — it
  is independent of Doppler and stays as-is.
- The guard never runs `doppler run` (no secret fetch, no auth dependency); it
  reads at most the local scope via `doppler configure get`.

## Design

### `launch` is the whole command

`run::plan_group` stops combining a prefix with the launch argv. The plan's
argv is simply `launch_argv(app, port)` — every `{port}` replaced, nothing
prepended. A Doppler user writes the wrapper themselves:

```toml
[apps.api]
base_port = 9100
launch = ["doppler", "run", "-c", "dev_local", "--", "nitro", "dev", "--port", "{port}"]
```

A non-Doppler user writes a bare command:

```toml
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{port}"]
```

The `doppler_prefix` builder, its `--preserve-env` handling, and the `-p
<project>` injection are deleted. Their information now lives in `launch`.

### The `prd` guard reads the command, then asks Doppler

Today `Config::parse` rejects `doppler_config = "prd"` — a single global string
it can read directly. With the config inlined into `launch`, the guard resolves
each Doppler app's *effective* config in Doppler's own precedence order and
rejects `prd`:

1. If the launch program is not Doppler, the app is unguarded — accept it. ("Is
   Doppler" = the basename of `launch[0]` equals `doppler`.)
2. Explicit config flag in the launch argv — `-c <v>`, `-c=<v>`, `--config <v>`,
   or `--config=<v>`. The new `launch` almost always spells this out, so this is
   the dominant path.
3. Else `DOPPLER_CONFIG` in the app's resolved env (`static_env`) — a value
   devkit already holds; no subprocess.
4. Else ask Doppler what it would use: run
   `doppler configure get config --plain --scope <app cwd>`, which reads the
   persisted local scope (`~/.doppler/.doppler.yaml`) **without** fetching
   secrets. `doppler configure get` does not accept `-c`, which is why steps 2–3
   parse the flag/env themselves rather than delegating everything to Doppler.
5. If the resolved config is `prd`, reject. If steps 2–4 all come up empty (no
   flag, no env, no local scope, or `doppler` is not installed), reject
   fail-safe — devkit cannot prove the launch is not `prd`.

The fully authoritative probe would be `doppler run … -- printenv
DOPPLER_CONFIG`, which honors flag, env, and scope identically — but `doppler
run` fetches secrets over the network and needs auth, too heavy for a guard that
fires before every launch. `configure get config --scope` reads the same local
scope Doppler consults when no flag/env is given, with no network call.

The repo's `doppler.yaml` is **not** a guard input: it is only the template for
`doppler setup`, and `doppler run` does not read it at runtime. Trusting it would
guess at a config Doppler might not actually use. (`doppler.yaml` is still read
for app *path* inference — see below — just not for the guard.)

The guard runs in the **launch path** (`devrun up` and the MCP `devrun.up`), not
in `load()`: it gates *starting a server against secrets*, so read-only commands
(`portm ls`, `issue status`) neither launch servers nor should pay a subprocess
cost or require `doppler` to be installed. It operates over the built
`LaunchPlan` (argv + resolved env + cwd), which already carries every input it
needs.

### What stays

- **`doppler_yaml`** in `[defaults]` — still the path to `doppler.yaml`, used for
  path inference and the guard's config fallback. Reading the file stays
  best-effort: absent file → empty map → guard step 4 (warn) for any Doppler
  launch.
- **`static_env`, `url_env`, `provides_url`** — env layering in `env_for` is
  untouched. devkit still sets these vars on the spawned process.
- **`setup`** — already literal argv arrays; unchanged. A Doppler install line
  stays `[["doppler","run","-c","local","--","bun","install"]]`.

### What is removed

| Removed | Reason |
|---|---|
| `[defaults].doppler_config` | Config now lives in each app's `launch`. |
| `[apps.*].doppler_project` | `-p <project>` is written in `launch` if wanted. |
| `[apps.*].preserve_env` | `--preserve-env=K` is written in `launch` if wanted. |
| `run::doppler_prefix` | No prefix is built. |
| `plan_group`'s `doppler_config` param | No prefix to parameterize. |
| `App.doppler_project`, `AppConfig.doppler_project`, `AppConfig.preserve_env` | Backing fields for the above. |

## Affected code

- **`crates/devkit-ports/src/run.rs`** — delete `doppler_prefix`; `plan_group`
  drops its `doppler_config` parameter and sets `argv = launch_argv(app, port)`.
  Add the guard: a pure `config_from_argv_env(argv, env) -> Option<String>`
  (steps 2–3, unit-testable) plus a thin `assert_not_prd(plan) -> Result<()>`
  that adds the step-4 `doppler configure get` fallback and the fail-safe reject.
- **`crates/devkit-ports/src/doppler.rs`** — unchanged parser (`path_to_project`
  stays; it feeds path inference only). The guard does not read `doppler.yaml`.
- **`crates/devkit-ports/src/apps.rs`** — `App`/catalog drop `doppler_project`;
  `guess_path` keeps using the `path_to_project` keys.
- **`crates/devkit-ports/src/config.rs`** — `Defaults` drops `doppler_config`;
  `AppConfig` drops `doppler_project` and `preserve_env`; the `prd` ensure in
  `Config::parse` is removed (the guard moves to the launch path).
- **`src/bin/devrun/main.rs`** and **`crates/devkit-mcp/src/devrun.rs`** — drop
  the `&cfg.defaults.doppler_config` argument to `plan_group`; call
  `run::assert_not_prd` on each plan before spawning.
- **`README.md`** — move `doppler` out of **Required** (bun is already removed).
- **`docs/configuration.md`** — drop `doppler_config` from the `[defaults]`
  table and `doppler_project`/`preserve_env` from the `[apps.*]` table; update
  the `launch` row to state it is the complete command, with a Doppler example;
  add a short migration note.
- **`AGENTS.md`** — update the "`prd` is rejected" invariant to describe the
  parse-the-launch guard instead of the `doppler_config` key.

## Testing (TDD)

- **run.rs — `config_from_argv_env` (pure, steps 2–3)**
  - `["doppler","run","-c","prd","--",…]` → `Some("prd")`; same for `-c=prd`,
    `--config prd`, `--config=prd`.
  - `-c dev_local` → `Some("dev_local")`.
  - no flag, `env` has `DOPPLER_CONFIG=prd` → `Some("prd")`.
  - non-Doppler launch (`basename(argv[0]) != "doppler"`), even with a literal
    `"prd"` arg → `None` (treated as unguarded by the caller).
  - Doppler launch, no flag, no env var → `None` (caller falls through to step 4).
- **run.rs — `assert_not_prd`**
  - a plan whose resolved config is `prd` → `Err`.
  - a non-Doppler plan → `Ok` regardless of args.
  - (step-4 `doppler configure get` is exercised by hand / left to integration,
    since it shells out; the pure resolver carries the unit coverage.)
- **run.rs — `plan_group`** — rewrite `plan_group_builds_doppler_wrapped_argv` to
  assert argv equals the verbatim, port-substituted launch (no `doppler`
  prepended unless the launch itself put it there).
- **config.rs** — update `SAMPLE` (launch carries the full command including
  `doppler run -c …`; remove `doppler_config`/`preserve_env`); replace the
  `rejects_prd` parse test with an `assert_not_prd` guard test; keep
  `parses_app_setup_commands`.
- **apps.rs** — drop `pulls_project_from_doppler`; keep the path-inference test.
- Full gate: `cargo test --workspace`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo fmt --all`.

## Migration note (for the personal config)

Existing `devkit.toml`/`config.toml` files must:

1. Move the Doppler wrapper into each app's `launch`, e.g.
   `launch = ["doppler","run","-c","dev_local","--","nitro","dev","--port","{port}"]`.
2. Delete `[defaults].doppler_config` and any `doppler_project` / `preserve_env`
   app keys (folding `--preserve-env=…` into `launch` where it was used).

Unknown keys are ignored (no `deny_unknown_fields`), so a stale
`doppler_config` left behind is silently dropped rather than erroring — but it
no longer has any effect, so removing it avoids confusion.

## Resolved decisions

1. **Unverifiable Doppler launch → reject (fail-safe).** When steps 2–4 cannot
   resolve a config (no flag, no `DOPPLER_CONFIG`, no local scope, or `doppler`
   absent), the guard rejects rather than warns. Under the new `launch` the
   config is almost always explicit, so this residual is rare; refusing what it
   cannot prove safe is the right default for a prod-secrets guard.
2. **Env-supplied config is covered via the standard `DOPPLER_CONFIG`.** The
   guard reads `DOPPLER_CONFIG` from the app's resolved env (step 3) — the same
   variable Doppler itself consumes. No `DEVKIT_`-prefixed knob: that would
   re-introduce a devkit-owned config field we are removing, and Doppler would
   not read it, so it would guard a value that does not drive the launch.
