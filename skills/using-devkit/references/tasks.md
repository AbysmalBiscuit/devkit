# Canned tasks — `devrun task`

Projects define oneshot tasks in `[tasks]`: builds, profiling flows, assertions, anything needing the project's apps, ports, and env wired in. A task renders **real registry ports** into its templates, so a hand-typed port is exactly the drift devkit exists to prevent. Check for a task before assembling the command yourself.

```sh
devrun task                                   # list configured tasks (name, kind, app, description)
devrun task <name> --dry-run                  # print the rendered plan(s) — argv, cwd, env — without running
devrun task <name>                            # run it
devrun task <name> --env KEY=value            # overlay env on every step
devrun task <name> --env-file F
```

A **command** task runs in the foreground and propagates its exit code. A **sequence** task runs `{ task = … }` / `{ up = … }` steps in order, stopping at the first failure. The `--env`/`--env-file` overlay applies to every step: command steps layer it above the task's `env`, `up` steps above the app's `static_env`, the same as `devrun up --env`.

## The `require_live` gate

A task may declare `require_live = ["<app>"]`, meaning that app must have a live server **in this worktree** — a registry row with an alive pid — before the task runs. A build that bakes another app's URL into its output is the usual case.

Failing the gate errors with:

```
require_live: `<app>` has no live server in this worktree (devrun up <app>)
```

Do what the message says: `devrun up <app>`, then rerun the task. Exporting the env var by hand produces a build with a stale or wrong URL baked in, which is the failure the gate exists to catch.

Only `ports['<app>']` references arm the gate. `run` argv references cannot be overridden and always arm theirs.

A CLI-path gate failure can leave a pid-less reservation row behind, bounded by the 300s reservation grace. That is the reserve-before-bind row the suggested `devrun up <app>` reuses, not a leak.

## `--env` waives the gate

Overriding a key with `--env KEY=…` replaces that value entirely: its port references are neither allocated nor gated, and the value is taken verbatim. That is how the same build task targets a remote or preview URL with no local server running.

## Sequences resolve lazily

`task::resolve` validates every step upfront, but those rendered plans feed validation and `--dry-run` only. Execution re-resolves each command step immediately before it runs — fresh port allocation, gate enforced then — so a step sees ports as they are *after* earlier steps. An `up` step earlier in the sequence can satisfy a later step's `require_live` gate.

This is why `--dry-run` output can differ from what actually runs: dry-run shows the t=0 rendering of every step.
