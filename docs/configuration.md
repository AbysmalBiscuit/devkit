# devkit configuration

devkit's engine is project-agnostic; every project- and machine-specific detail
lives in a single TOML config. That config is **personal** (it holds your
worktree paths, app catalog, local secrets, and teammate handles), so it lives
outside the repo. This document is the canonical reference for the config
*shape*; copy the example below to a local file and adjust it.

## Location

The config is resolved from the first of:

1. `--config <path>` (global flag on every binary)
2. `$DEVKIT_CONFIG`
3. `./devkit.toml` and `./devkit.local.toml` (searched upward from the working directory)
4. `~/.config/devkit/config.toml`

The recommended setup is to keep your real config at
`~/.config/devkit/config.toml`, where every binary discovers it automatically —
no flag or env var needed. (`.gitignore` also ignores `/configs/*.toml`, should
you prefer to keep a copy inside a checkout.)

`devkit.local.toml` is the untracked twin of `devkit.toml`: same shape, same
schema, and it overrides the `devkit.toml` beside it. Settings one machine or
one checkout needs go there, so the repository's `devkit.toml` carries only
what the project shares. It stands alone too — a directory holding only a
`devkit.local.toml` is a devkit project. Ignoring it is the repository's job;
devkit does not write a `.gitignore` entry for you.

## Secrets

Credentials are **not** stored in `config.toml`. They resolve env-first, then from
a separate `~/.config/devkit/secrets.toml` written `0600`:

```toml
# ~/.config/devkit/secrets.toml  (chmod 600)
linear_api_key   = "lin_api_…"
linear_workspace = "adaptyv"
slack_token      = "xoxb-…"
```

Resolution order for each credential is `$ENV` → `secrets.toml` → unset, so a
shell export or a Doppler-injected variable always overrides the file. Populate
the file with `devkit auth <linear|slack>` (it validates the token against the
live API before saving) and inspect it with `devkit doctor`.

## Editor support

`devkit.toml` has a published JSON Schema, so an editor running the TOML
language server ([taplo](https://taplo.tamasfe.dev)) gives completion, hover
docs and inline validation.

```sh
devkit schema init                # ./devkit.toml
devkit schema init path/to/devkit.toml
devkit schema init devkit.local.toml
```

That adds the `#:schema` directive to an existing config, or writes a starter
one when there is none. It is idempotent — a file already naming a schema is
left untouched — so it is safe to run against a config under review.

The starter has every setting commented out, so nothing is active until you
have read it, and `devkit brief` reports the config as not resolving until you
uncomment `[defaults]`. The values are filled in from the checkout rather than
left as placeholders, so what you uncomment is already right.

To do it by hand, the directive is a taplo **header**: first line, preceded
only by other directives and comments.

```toml
#:schema https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-config.json

[defaults]
worktree_root = "~/Git/example-worktrees"
```

A filesystem path works too, which is how to validate against an unreleased
schema — including devkit's own checkout, where the release URL does not yet
resolve:

```toml
#:schema /home/you/Git/devkit/schema/devkit-config.json
```

To cover every `devkit.toml` without editing each one, use a taplo rule
instead:

```toml
# .taplo.toml
[[rule]]
include = ["**/devkit.toml", "**/devkit.local.toml"]
[rule.schema]
url = "https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-config.json"
```

Every release attaches the schema as an asset, so `latest/download` always
resolves to the newest released version. To validate against the devkit you
actually have installed, name its tag instead:

```
https://github.com/AbysmalBiscuit/devkit/releases/download/v1.2.3/devkit-config.json
```

Either beats pointing at `main`, which validates your config against keys no
released binary accepts yet.

The schema is generated from the same Rust types serde reads — the doc comments
in `crates/devkit-config/src/lib.rs` become the hover text — so it cannot
describe a shape the binaries do not accept. It catches what config resolution
would otherwise only report at run time, and only through `devrun config show`:
an `[apps.x]` without `base_port` or `launch`, a value of the wrong type, a
task step that is neither `task` nor `up`, an unknown `ecosystem`.

Three things it deliberately does not do:

- **Nothing is required at the top level.** `[defaults]` is required of the
  *merged* config, not of any one file — a layer carries only what it
  overrides, and devkit's own `devkit.toml` is `[harness]` and nothing else.
  An editor validates the file in front of it, so requiring `[defaults]` would
  mark correct overlays as errors.
- **Unknown keys pass.** devkit ignores keys it does not recognise, so a
  schema that rejected them would be stricter than the parser. A misspelled
  `lock = false` still silently does nothing.
- **Post-parse rules are invisible to it**, such as an app needing a `path`
  when there is no `doppler.yaml` to infer one, or a task setting exactly one
  of `run`/`steps`.

One caveat from layering: `base_port` and `launch` are marked required on each
app, which is right for an app defined in one file and wrong for one whose keys
are split across the home config and a project overlay. That split is rare
enough to be worth the check.

Regenerate it after changing any config type:

```sh
DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema
```

`cargo test` fails if the committed file is stale, printing a unified diff of
what moved; the env var makes that same run rewrite the file instead.
`cargo run --bin devkit -- schema` prints it to stdout without touching
anything.

## Sections

### `[defaults]`

| Key | Required | Meaning |
|---|---|---|
| `worktree_root` | yes | Directory under which issue worktrees are created. `~` is expanded. |
| `branch_prefix` | yes | Prefix for branches created by `issue setup` (e.g. `you/`). |
| `baseline_ref` | yes | Git ref the baseline server tracks (e.g. `origin/staging`). |
| `baseline_path` | yes | Checkout path for the baseline server. `~` is expanded. |
| `doppler_yaml` | no | Path to the repo's `doppler.yaml`; its `setup` paths seed app **path inference**. `~` is expanded. Absent → apps need an explicit `path`. |
| `pr_base` | no (default `"staging"`) | Default base branch for PRs opened by `issue review`. |
| `require_pr_reviewer` | no (default `false`) | Refuse `issue review request` when it would open a new PR without a `--to` reviewer. Left unset, the PR opens with no reviewer and nobody is Slacked. |
| `apps_dir` | no | Directory (relative to a worktree) that holds per-app subdirectories. |
| `issue_summary` | no (default `false`) | Write the issue summary file on every `issue setup`, as though `--summary` were passed. `--summary` / `--no-summary` still decide a single run. The file's path and body come from `templates.issue_summary_path` and `templates.issue_summary`. |
| `worktree_include` | no | Glob patterns (relative to the monorepo root) for untracked local files copied into a newly created worktree by `issue setup` / `issue checkout-pr`, at the same relative path. `issue sync-includes` re-runs the same copy against worktrees that already exist, from this one pattern list. A pattern ending in `/`, or one matching a directory, copies recursively. Existing destinations are never overwritten by default; copy failures warn and are skipped (fail-open). `issue sync-includes --overwrite` is the opt-in way to replace files a worktree already has, and it needs a scope — one or more selectors, or `--all`. Anchor patterns (`apps/*/.env.local`) rather than scanning the whole tree — `**` descends into `node_modules`. |

### Path values

`worktree_root`, `baseline_path`, and `doppler_yaml` are resolved once when the
config loads, in this order:

1. `${VAR}` is replaced with that environment variable. An unset variable is an
   error naming both the config key and the variable. `$$` is a literal `$`; a
   `$` followed by anything else is left alone.
2. A leading `~/` expands to `$HOME`.
3. A path that is still relative is anchored by what it names, never by the
   working directory, and `.` / `..` are folded out either way:
   - `worktree_root` and `baseline_path` name a location **on this machine**,
     so they resolve against the directory of the config file that declared
     them — including when that file is a linked worktree's main checkout.
   - `doppler_yaml` names a file **inside the repository being worked on**, so
     it resolves against the checkout reading the config instead: each
     worktree resolves its own copy, and a branch that edits it takes effect
     without merging first.

`branch_prefix` gets step 1 only.

Step 3 is what lets a project commit its `devkit.toml`:

```toml
[defaults]
worktree_root = "../myproject-worktrees"
baseline_path = "../myproject-worktrees/_baseline"
baseline_ref  = "origin/main"
```

That is correct on every machine and for every developer. Only `branch_prefix`
is personal — put it in `devkit.local.toml`, or write `"${USER}/"`.

### `[apps.<name>]`

One table per runnable app. `<name>` is the app id passed to `issue setup --apps`.

| Key | Required | Meaning |
|---|---|---|
| `base_port` | yes | Base port; per-worktree ports are allocated from here via the registry. |
| `launch` | yes | The complete launch command, run verbatim. `{{ port }}` is substituted with the allocated port. Write the whole invocation here, including any `doppler run -c <config> --` wrapper and `--preserve-env=…` flags the app needs. |
| `path` | no | App subdirectory (relative to the repo) when it differs from `<name>`. |
| `url` | no | Address the app serves on, defaulting to `http://localhost:{{ port }}`. Rendered as a minijinja template over the same variables as `launch` — `{{ port }}`, `ports['<app>']`, and `[templates.variables]` — so it can carry any scheme, host, or path (`https://app.localhost:{{ port }}/admin`). It is what `devrun up` prints in its URL column and, for the `provides_url` app, what is wired into consumers' `url_env`. Devkit never terminates TLS on an app's behalf, and readiness stays a TCP probe against the allocated port. |
| `url_env` | no | Env var that receives the app's URL. |
| `provides_url` | no | `true` marks the one app whose URL other apps consume. Exactly one app should set this. |
| `static_env` | no | Inline env vars always set for this app. |
| `prep_files` | no | Files written into the app's directory during `issue setup`, before `setup` commands run. Each entry is `{ path, content, overwrite }` — `path` is relative to the app dir (parent dirs created), `content` is rendered as a minijinja template with the issue context (`prefix`, `issue`, `slug`, `apps`, `app`, `branch`, `worktree`) plus `[templates.variables]`, and `overwrite` (default `false`) keeps an existing file unless set to `true`. Emit a literal `{{` with `{% raw %}…{% endraw %}`. As an array, a deeper `devkit.toml` replaces the whole list rather than appending. |
| `setup` | no | Commands run in the app's directory during `issue setup`, in order. Each entry is one argv array (program + args), e.g. `[["doppler", "run", "-c", "local_config", "--", "bun", "install"]]`. Use this for installs and any doppler wiring; nothing project-specific is hardcoded in the tool. |

devkit runs `launch` exactly as written — it builds no command prefix. To use
Doppler, wrap the command yourself, e.g.
`launch = ["doppler","run","-c","dev_local","--","nitro","dev","--port","{{ port }}"]`.

Launch argv and `static_env` values are minijinja templates rendered per
launch with strict undefined handling:

- `{{ port }}` — the app's own allocated port.
- `{{ ports['other-app'] }}` — another app's port in this worktree, resolved
  from the port registry. Referencing an app that isn't running writes a
  normal pid-less reservation which a later `devrun up other-app` claims, so
  a consumer can bake the port before the server exists. A typo'd app name
  is a hard error.
- `[templates.variables]` constants are available by name.

The old `{port}` placeholder is retired; a leftover `{port}` in a rendered
value fails the launch with a migration hint.

Before starting such a server, devkit refuses a launch that resolves to the
`prd` config: it reads `-c`/`--config` from `launch`, then `DOPPLER_CONFIG` from
the app's env, then `doppler configure get config --scope <app dir>`; a Doppler
launch whose config is `prd` or cannot be resolved is rejected.

**Migration:** earlier configs set `[defaults].doppler_config` and let devkit
prepend `doppler run`. Move that wrapper into each app's `launch`, and delete the
`doppler_config`, `doppler_project`, and `preserve_env` keys (fold any
`--preserve-env=…` into `launch`).

To enforce a hard per-app memory cap *without* the daemon restarting the server,
set a runtime or OS limit through the app's `static_env` — e.g.
`static_env = { NODE_OPTIONS = "--max-old-space-size=2048" }`, or wrap `launch`
in a `ulimit -v` shell. The runtime/OS aborts the process on breach and the
daemon's crash-restart recovers it; this keeps enforcement in the runtime rather
than the daemon's `memory_action`. On Linux with cgroup-v2 delegation the daemon
also supports a first-class `memory_max_mb` kernel cap — see the `[daemon]`
section below.

## Tasks

`[tasks.<name>]` defines canned oneshots run by `devrun task <name>`
(`devrun task` lists them). A task is either a **command** (`run`) or a
**sequence** (`steps`), never both.

```toml
[tasks.api-prod-build]
description = "prod nitro build (node-server preset)"
app = "api-prod"        # run in this app's dir, inherit its static_env
run = ["doppler", "run", "-p", "api-foundry", "-c", "dev_local",
       "--preserve-env=NITRO_PRESET", "--", "bun", "nitro", "build"]
env = { NITRO_PRESET = "node-server" }

[tasks.profile-lab-os]
description = "prod api + profiled lab-os, wired together"
steps = [
  { task = "api-prod-build" },
  { up = "api-prod" },
  { task = "lab-os-build" },
  { up = "lab-os-prod" },
]
```

- Command tasks run in the foreground with inherited stdio; the exit code is
  propagated. `run` and `env` values are minijinja templates with the same
  `port`/`ports` context as launches; `{{ ports['x'] }}` resolves from the
  port registry (issue role), writing a pid-less reservation when `x` isn't
  running. Env layering, low to high: app `static_env` → task `env` → CLI
  `--env-file` → `--env`. Tasks do not get `url_env` provider wiring —
  reference the app you need explicitly via `ports[...]`. Doppler invocations
  go through the same `prd` guard as launches.
- `require_live = ["app", …]` (command tasks only): each listed app must have a
  live devrun-managed server in this worktree when the task *executes*, or the
  task fails before spawning:

      require_live: `api-serve` has no live server in this worktree (devrun up api-serve)

  Each name must be referenced by the task's templates via `ports[...]`. A
  `--env`/`--env-file` override that replaces every value referencing an app
  waives that app's check *and* its allocation — a user-supplied URL makes the
  local port irrelevant (this is how one build task serves both a local flow
  and a hosted-preview flow). References in `run` argv cannot be overridden.
  The reference must go through `ports['<app>']` specifically — a task
  referencing its own app only via `{{ port }}` does not satisfy
  `require_live` for that app.
- Sequence steps are re-resolved immediately before each step runs: ports are
  re-allocated and templates re-rendered then, and `require_live` is checked
  then — so a step gated on an app that an earlier `up` step starts works, and
  a build step longer than the reservation grace period cannot bake a port
  that a later `up` step no longer gets. `--dry-run` prints the upfront
  resolution and never gates.
- Sequence steps run in order and stop at the first failure. `{ up = "app" }`
  is `devrun up app` (a no-op for a live server). A sequence may only set
  `description` and `steps`, and cannot reference another sequence. The CLI
  `--env`/`--env-file` overlay applies to every step: command steps layer it
  above the task `env`, and `{ up = "app" }` steps layer it above the app's
  `static_env`, exactly as `devrun up --env` would.
- `devrun task <name> --dry-run` prints each rendered plan (still resolving
  ports, so the printed values are real) without executing.

### `[daemon]`

Optional daemon-level tuning. Env overrides are listed alongside each key.

#### Memory management

Two layers of memory control are available; they compose without conflict:

| Key | Env override | Default | Meaning |
|---|---|---|---|
| `memory_max_mb` | `DEVKIT_DAEMON_MEM_MAX_MB` | `0` (off) | Hard kernel ceiling per supervised server tree, in MB. Linux-only (cgroup-v2). See subsection below. |
| `memory_limit_mb` | `DEVKIT_DAEMON_MEM_LIMIT_MB` | `0` (off) | Soft RSS threshold, in MB. When a server's tree-RSS stays over this for `memory_limit_ticks` consecutive supervision ticks, the daemon SIGTERMs it and respawns within the crash-loop budget. Requires `memory_action = "restart"`. |
| `memory_action` | `DEVKIT_DAEMON_MEMORY_ACTION` | `""` (off) | Set to `"restart"` to enable the soft poll-based restart on `memory_limit_mb` breach. |
| `memory_limit_ticks` | `DEVKIT_DAEMON_MEM_LIMIT_TICKS` | `2` | Consecutive over-`memory_limit_mb` supervision ticks before the soft restart fires. |

#### `memory_max_mb` — hard cgroup-v2 kernel cap (Linux only)

`memory_max_mb` sets a hard per-server memory ceiling enforced by the kernel via
a cgroup-v2 `memory.max` leaf. A server whose resident set exceeds the cap is
OOM-killed by the kernel; the daemon observes that as a crash and respawns it
through the existing crash-restart path. It is **not** a new restart path —
the same crash-loop budget applies.

`memory_max_mb` sits **above** `memory_limit_mb`: the soft poll-based action
(`memory_action = "restart"`) is the graceful first responder, acting at
`memory_limit_mb`; the kernel cap at `memory_max_mb` is the backstop for spikes
too fast for the 500 ms poll loop. Set `memory_max_mb` higher than
`memory_limit_mb` (or omit `memory_limit_mb` entirely) to preserve this ordering.

**Requires cgroup-v2 delegation.** The daemon must run inside a delegated
cgroup-v2 subtree with the memory controller enabled. The recommended setup is
`devkitd install-service`, which writes a `systemd --user` unit with
`Delegate=yes` — no `sudo` required. Without delegation the daemon logs a
one-time warning and falls back to the soft `memory_action` path; no server spawn
ever fails because cgroup setup is unavailable (fail-open).

Cap setup is **fail-open**: any cgroup error logs once and proceeds uncapped
rather than blocking or killing a server. A broken cgroup configuration degrades
to today's soft behavior.

**macOS / Windows**: `memory_max_mb` is documented but has no effect. The daemon
stays silent (no warning) — the soft `memory_action` path remains available on
all platforms.

### `[harness]`

Opt-in for the agent write-access harness.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `enforce_writes` | bool | `false` | When `true`, the devkit plugin's `PreToolUse` hook enforces write locks automatically. When absent or `false`, the hook exits immediately with no effect. |

**Three opt-in sources, with precedence.** The hook resolves whether to enforce
for a given checkout from, in order:

1. **`DEVKIT_ENFORCE_WRITES`** (env) — an explicit master switch. `1`/`true`/`yes`/`on`
   forces enforcement on; `0`/`false`/`no`/`off` forces it off, overriding both
   config files. Unset/blank/unrecognized → no opinion, fall through.
2. **The project layers applying where the write happens** `[harness] enforce_writes`
   — every layer the config walk resolves for that location, including
   `devkit.local.toml` and a linked worktree's inherited main-checkout layer,
   opts in if any of them sets the flag. The checkout root the walk anchors to
   comes from asking git, not from scanning ancestors for a `.git` entry.
3. **The global config** `[harness] enforce_writes` — read from `$DEVKIT_CONFIG`
   (else `~/.config/devkit/config.toml`). Set it here to enforce across **every**
   checkout without a per-checkout file.

With the env var unset, enforcement is on when **either** a project layer **or**
the global config opts in. Set `enforce_writes = true` in the global config for a
machine-wide default; drop a per-checkout `devkit.toml` only when you want to opt a
single checkout in (or, with the global default on, set the env var to `off` to opt
a session out). The global-config and env routes need **no** per-worktree file — so
they avoid shadowing the global config in `devrun`/`portm` discovery.

**What enforcement gates.** The hook intercepts `Edit`, `MultiEdit`, `Write`,
and `NotebookEdit` — the structured write tools. Shell-level writes made via
`Bash` are outside the harness's scope (a documented gap; coordinate those
manually with `lockm acquire`).

**Activation requires `lockm` on `PATH`.** The hooks invoke bare `lockm hook
<event>`. Install via `cargo install --path .`; the binary must be resolvable
from the shell that runs hook commands.

**Fail-open / fail-closed behaviour.**

- *Harness off* (no opt-in from any source — env unset, no checkout `devkit.toml`
  flag, and no global-config flag): the hook exits 0 immediately. No locks are
  taken; zero overhead.
- *`lockm` absent from `PATH`*: the hook invocation fails silently and the
  write proceeds. This is fail-open to avoid blocking agents on machines that
  do not have the binary installed.
- *Registry error when the harness is on*: the hook denies the write rather
  than allowing it through silently (fail-closed). The deny message includes
  the error so the agent can report it.

**Example** — enforce everywhere via the global config (`~/.config/devkit/config.toml`):

```toml
[harness]
enforce_writes = true
```

Or per-checkout, add the same table to that checkout's own `devkit.toml`; only the
`[harness]` table is read, so it may be an otherwise-empty file or a full project
config. Or skip both files and set `DEVKIT_ENFORCE_WRITES=1` in the environment.

### `[brief]`

What `devkit brief` emits. The plugin's hooks call it unconditionally; these
switches decide whether it produces anything.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | The whole brief. `false` suppresses every section, and is read before any work is done. |
| `pins` | `true` | The library-versions section only. |
| `locks` | `true` | The `lockm status` line. Turn it off where only one session ever works in a checkout at a time. |
| `apps` | `true` | The `Apps` line and the `devrun up` / `portm status` bullets. |
| `tasks` | `true` | The task table and the `devrun task` bullet. |

A section is omitted when the checkout has nothing to report, whatever its
switch says; the switch suppresses a section the checkout *does* have. Both
read the same downstream, so the bullets introducing a section go with it:
`apps = false` drops the `Apps` line, the `devrun up` bullet and the `portm
status` line together, and `tasks = false` drops both the task table and its
bullet. The intro names only the facilities that survive, and a project left
with none of them produces no devrun section at all.

Live servers have no switch. A port this worktree holds is a fact about the
machine rather than a listing the brief chose to carry, so the server table
appears whenever the registry has rows for the worktree — and it keeps the
`devrun down` and `portm status` lines relevant even under `apps = false`.

Set it in `~/.config/devkit/config.toml` as a personal default and override it
per project in that project's `devkit.toml`. A malformed `[brief]` table falls
back to these defaults rather than withholding the brief.

### `[tracker]`

Which issue tracker backs the `issue` commands.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `kind` | string | _(detect)_ | `linear`, `github`, or `none`. |

devkit picks the tracker by detection: a resolvable `LINEAR_API_KEY`
(environment or `~/.config/devkit/secrets.toml`) means Linear, otherwise a
github.com `origin` remote means GitHub, otherwise no tracker.

`github` talks to GitHub Issues in the repository the `[github]` table below
resolves as `issues_repo`. It authenticates with the token `gh auth login`,
`GH_TOKEN` or `GITHUB_TOKEN` supplies — devkit stores no GitHub credential of
its own; run `gh auth login` once and `devkit auth github` to see which token
devkit would send and who it belongs to. On a GitHub project a bare number is a
PR, not an issue, so `issue checkout-pr 3340` needs no disambiguation. If no
issues repository resolves there is nothing to ask, and the project falls back
to running with no tracker; `devkit doctor`'s `tracker` row carries the reason.

Detection is a floor, not a convenience: a `LINEAR_API_KEY` exported globally
resolves to Linear for *every* project on the machine. `kind` names the tracker
outright and wins over detection: every `issue` command that talks to a tracker
reads it, as does the `issue.status` MCP action. Detection is what decides only
when no config resolves — a directory outside any devkit project, or a config
that fails to load, still gets a tracker rather than an error.

With `kind = "none"`, `issue` still creates worktrees, tracks PRs, and reaches
FINISHED on a merged PR and a clean tree, as long as the worktree carries an
issue id; the STATE column reads `no tracker`. Under any other tracker the
verdict also waits on the issue reaching a completed state, and a tracker that
answered with nothing for that issue holds the verdict open rather than
promoting the worktree.

`devkit doctor`'s `tracker` row prints which tracker resolved and why, which is
the only place detection's choice is visible: it names the `kind` and whether
config or detection produced it.

Detection landing on no tracker holds the verdict open too. Declaring
`kind = "none"` says the project has no issue states to wait for; detection
finding neither a key nor a GitHub remote says devkit found nothing to ask,
which is the same silence a tracker with no key gives. `issue end` removes
worktrees and deletes their branches, so it never acts on an unanswered
question.

Everything that asks a tracker a question goes through the resolved one:
`issue setup`'s title-derived slug and summary file, `issue checkout-pr`'s
disambiguation of a bare number, `issue dashboard`'s issue timeline, and the
ISSUE column in `issue prs`. So each answers from the tracker this project
declared rather than from whatever `LINEAR_API_KEY` happens to be exported in
the shell. What stays Linear-specific is `LINEAR_WORKSPACE`, which supplies the
workspace slug for clickable Linear issue links without a lookup, and
`[linear] resolve_pr_links` below.

### `[github]`

Which GitHub repositories this project's issues and pull requests live in.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `issues_repo` | string | _(the `origin` remote)_ | `owner/repo` holding the issues, e.g. `org/planning`. |
| `pr_repo` | string | _(the `origin` remote)_ | `owner/repo` pull requests are opened against, e.g. `upstream/app`. |

The table is deliberately not under `[tracker]`: a Linear project with a fork
workflow needs `pr_repo` just as much as a GitHub one does, and a project may
track issues in a repository separate from its code.

**Each key resolves on its own, and is required only where it is used.** A
project that only reads PRs never has to supply an `issues_repo`, and a project
that supplies neither pays nothing until a GitHub operation asks for one. When
the key an operation needs cannot be resolved, the error names that key.

**Defaulting from `origin` needs a github.com `origin`.** There is no
`gh repo view` fallback and no second remote is consulted: if `origin` is
absent, or is not a github.com URL, the key that would have defaulted from it
fails and the error says to set `[github] issues_repo` / `pr_repo`. The error is
the instruction. An SSH-alias remote is the case that surprises people — a
remote spelled `gh:owner/repo.git`, pointing at a `~/.ssh/config` `Host gh`
entry, contains no `github.com`, so nothing can be read from it. devkit's own
repository is set up that way and names both keys outright as a result.

**Unknown keys in this table are rejected, unlike every other table in this
file.** A misspelled `issue_repo` silently ignored would leave the project
resolving a *different* repository than it declared — devkit would default from
`origin` and query someone else's issues while the config appeared to say
otherwise. Failing the config load is the smaller harm, so this one table
refuses what it does not recognise.

```toml
[github]
issues_repo = "org/planning"
pr_repo = "upstream/app"
```

`issue prs --repo owner/name` overrides `pr_repo` for a single invocation (as
does the `repo` argument of the `issue.prs` MCP action); it does not touch
`issues_repo`. Every repository-scoped `gh` invocation devkit makes carries the
resolved repository explicitly, so an ambient `GH_REPO` cannot redirect it.

### `[linear]`

Opt-in Linear enrichment for `issue prs`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `resolve_pr_links` | bool | `false` | When `true`, `issue prs` asks Linear which issues each open PR is linked to and shows the union of the text-derived id and every linked id in the ISSUE column, deduplicated (text id first). |

The lookup authenticates with `LINEAR_API_KEY` (environment or
`~/.config/devkit/secrets.toml`); **no token lives in this table**. It costs
one extra batched round trip per 25 PRs, after the GitHub fetch. Fail-soft:
with no key, or on any Linear error, the column falls back to the
text-derived id — `issue prs` never fails because of Linear. The MCP
`issue.prs` action honors the same flag.

The flag gates Linear alone: it is a `[linear]` key, and a Linear key does not
become a global switch. Under the GitHub tracker the ISSUE column carries each
PR's closing issues whether or not this is set, at the cost of a batched round
trip of its own.

```toml
[linear]
resolve_pr_links = true
```

### `[hooks]`

Commands devkit runs when a lifecycle event fires. Each key is named
`{before,after}_<event>` and holds a list of argv arrays — the program plus its
arguments, run directly with no shell, so pipes, `&&`, and globs are not
available.

| Key | Fires on | Cwd |
|---|---|---|
| `after_worktree_create` | `issue setup` and `issue checkout-pr`, once the worktree exists and its apps are prepared, after the command has reported the worktree | the new worktree's root |

The event names a state change, not the caller. `after_worktree_create` fires
from both `issue setup` and `issue checkout-pr`; naming it after either command
would have been wrong for the other. A new hook key fires from every command
that reaches its state.

Each argv element is a minijinja template over `worktree`, `branch`, `issue`,
`slug`, `apps`, `prefix`, and `[templates.variables]`. Hooks run in the order
listed.

Failures are **fail-open**: a hook that cannot be rendered, cannot be spawned,
or exits non-zero prints a `warning:` line to stderr, and the remaining hooks
still run. The worktree already exists by the time hooks fire, so a missing
program must not fail the command that created it. Hook output is captured and
discarded, which keeps the JSON line the command prints last on stdout.

As an array, a deeper `devkit.toml` replaces the whole list rather than
appending — set machine-wide hooks in `~/.config/devkit/config.toml` and expect
a project that defines the same key to take over entirely.

```toml
[hooks]
after_worktree_create = [["zoxide", "add", "{{ worktree }}"]]
```

### `[people.<alias>]`

Teammate handle aliases used by `issue review` (`--to <alias>`). The alias maps
to delivery handles; **no tokens live here** — `SLACK_TOKEN` and
`LINEAR_API_KEY` come from the environment / Doppler.

| Key | Required | Meaning |
|---|---|---|
| `slack` | yes | Slack user (or channel) id, e.g. `U0XXXXXXXXX`. |
| `github` | no | GitHub login used as the default PR reviewer for this person. |

## Example

```toml
[defaults]
worktree_root  = "~/Git/acme"
branch_prefix  = "you/"
baseline_ref   = "origin/staging"
baseline_path  = "~/Git/acme/_baseline"
doppler_yaml   = "~/Git/acme/monorepo/doppler.yaml"
pr_base        = "staging"

[apps.api]
base_port    = 9100
launch       = ["doppler", "run", "-c", "dev_local", "--preserve-env=SOME_JWT_SECRET", "--", "nitro", "dev", "--port", "{{ port }}"]
url_env      = "API_BASE_URL"
provides_url = true
static_env   = { SOME_JWT_SECRET = "local-dev-placeholder-value" }

[apps.web]
base_port  = 4100
launch     = ["next", "dev", "-p", "{{ port }}"]
url_env    = "API_BASE_URL"
setup      = [["doppler", "run", "-c", "local_config", "--", "bun", "install"]]

[[apps.web.prep_files]]
path    = ".env.local"
content = """
SOME_FEATURE_FLAG=dummy
"""

[apps.worker]
base_port = 8080
path      = "services/worker"
launch    = ["uv", "run", "uvicorn", "server.main:create_app", "--factory", "--reload", "--port", "{{ port }}"]

[hooks]
after_worktree_create = [["zoxide", "add", "{{ worktree }}"]]

[people.alice]
slack  = "U0XXXXXXXXX"
github = "alice-gh"
```

## Environment

Env-only tuning knobs with no `config.toml` equivalent:

| Variable | Default | Meaning |
|---|---|---|
| `DEVKIT_FETCH_TTL_SECS` | `60` | Freshness window for `git fetch`. `issue setup`, `issue checkout-pr`, and `devrun up`'s baseline refresh skip a fetch of the same repo+remote made within this many seconds, reusing the remote-tracking refs already on disk (so the ref a worktree is cut from is at most this stale). `0` disables the gate — always fetch. |
| `DEVKIT_HYPERLINKS` | _(detect)_ | Override OSC 8 hyperlink emission in the `issue`/`portm`/etc. tables. `always`/`1`/`on`/`true`/`yes` forces clickable links; `never`/`0`/`off`/`false`/`no` disables them. Unset auto-detects via [`supports-hyperlinks`](https://crates.io/crates/supports-hyperlinks). Set `always` for a hyperlink-capable terminal that detection misses — e.g. an alacritty fork exporting a bare `TERM=xterm-256color`. |
