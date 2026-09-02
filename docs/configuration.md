# devkit configuration

devkit's engine is project-agnostic; every project- and machine-specific detail lives in a single TOML config. That config is **personal** (it holds your worktree paths, app catalog, local secrets, and teammate handles), so it lives outside the repo. This document is the canonical reference for the config *shape*; copy the example below to a local file and adjust it.

## Location

The config is resolved from the first of:

1. `--config <path>` (global flag on every binary)
2. `$DEVKIT_CONFIG`
3. `./devkit.toml` and `./devkit.local.toml` (searched upward from the working directory)
4. `~/.config/devkit/config.toml`

The recommended setup is to keep your real config at `~/.config/devkit/config.toml`, where every binary discovers it automatically — no flag or env var needed. (`.gitignore` also ignores `/configs/*.toml`, should you prefer to keep a copy inside a checkout.)

`devkit.local.toml` is the untracked twin of `devkit.toml`: same shape, same schema, and it overrides the `devkit.toml` beside it. Settings one machine or one checkout needs go there, so the repository's `devkit.toml` carries only what the project shares. It stands alone too — a directory holding only a `devkit.local.toml` is a devkit project. Ignoring it is the repository's job; devkit does not write a `.gitignore` entry for you.

## Layering

Every `devkit.toml` from the filesystem root down to the cwd is merged, with `~/.config/devkit/config.toml` as the lowest-precedence base layer beneath them all. Deeper files override shallower ones per value: tables merge key-by-key, while scalars and arrays replace wholesale. `devkit config` prints the merged result, headed by the layer files in precedence order; `--origin` traces each value to the file it came from and names the layers it overrode.

Two escapes bypass the walk:

- `[config] root = true` in a `devkit.toml` or `devkit.local.toml` stops the upward walk at that directory and drops every shallower layer, the home config included. Full isolation.
- `--config <path>` or `$DEVKIT_CONFIG` selects a single file verbatim, with no layering and no home base.

App `path` is normally inferred from the repo's `doppler.yaml`; individual `[apps.<name>]` sections may override it with an explicit `path`. `launch` is run verbatim, so a Doppler wrapper lives in each app's `launch`. devkit refuses to start a Doppler launch whose config resolves to `prd`, so it cannot run against production secrets.

## Secrets

Credentials are **not** stored in `config.toml`. They resolve env-first, then from a separate `~/.config/devkit/secrets.toml` written `0600`:

```toml
# ~/.config/devkit/secrets.toml  (chmod 600)
linear_api_key   = "lin_api_…"
linear_workspace = "adaptyv"
slack_token      = "xoxb-…"
```

Resolution order for each credential is `$ENV` → `secrets.toml` → unset, so a shell export or a Doppler-injected variable always overrides the file. Populate the file with `devkit auth <linear|slack>` (it validates the token against the live API before saving) and inspect it with `devkit doctor`.

## Editor support

`devkit.toml` has a published JSON Schema, so an editor running the TOML language server ([taplo](https://taplo.tamasfe.dev)) gives completion, hover docs and inline validation.

```sh
devkit schema init                # ./devkit.toml
devkit schema init path/to/devkit.toml
devkit schema init devkit.local.toml
```

That adds the `#:schema` directive to an existing config, or writes a starter one when there is none. It is idempotent — a file already naming a schema is left untouched — so it is safe to run against a config under review.

The starter has every setting commented out, so nothing is active until you have read it, and `devkit brief` reports the config as not resolving until you uncomment `[defaults]`. The values are filled in from the checkout rather than left as placeholders, so what you uncomment is already right.

To do it by hand, the directive is a taplo **header**: first line, preceded only by other directives and comments.

```toml
#:schema https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-config.json

[defaults]
worktree_root = "~/git/example_worktrees"
```

A filesystem path works too, which is how to validate against an unreleased schema — including devkit's own checkout, where the release URL does not yet resolve:

```toml
#:schema /home/you/Git/devkit/schema/devkit-config.json
```

To cover every `devkit.toml` without editing each one, use a taplo rule instead:

```toml
# .taplo.toml
[[rule]]
include = ["**/devkit.toml", "**/devkit.local.toml"]
[rule.schema]
url = "https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-config.json"
```

Every release attaches the schema as an asset, so `latest/download` always resolves to the newest released version. To validate against the devkit you actually have installed, name its tag instead:

```
https://github.com/AbysmalBiscuit/devkit/releases/download/v1.2.3/devkit-config.json
```

Either beats pointing at `main`, which validates your config against keys no released binary accepts yet.

The schema is generated from the same Rust types serde reads — the doc comments in `crates/devkit-config/src/lib.rs` become the hover text — so it cannot describe a shape the binaries do not accept. It catches what config resolution would otherwise only report at run time, and only through `devkit config`: an `[apps.x]` without `base_port` or `launch`, a value of the wrong type, a task step that is neither `task` nor `up`, an unknown `ecosystem`.

Three things it deliberately does not do:

- **Nothing is required at the top level, `[defaults]` included.** A layer carries only what it overrides, and devkit's own `devkit.toml` is `[harness]` and nothing else. An editor validates the file in front of it, so requiring `[defaults]` would mark correct overlays as errors.
- **Unknown keys pass.** devkit ignores keys it does not recognise, so a schema that rejected them would be stricter than the parser. A misspelled `lock = false` still silently does nothing.
- **Post-parse rules are invisible to it**, such as an app needing a `path` when there is no `doppler.yaml` to infer one, or a task setting exactly one of `run`/`steps`.

One caveat from layering: `base_port` and `launch` are marked required on each app, which is right for an app defined in one file and wrong for one whose keys are split across the home config and a project overlay. That split is rare enough to be worth the check.

Regenerate it after changing any config type:

```sh
DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema
```

`cargo test` fails if the committed file is stale, printing a unified diff of what moved; the env var makes that same run rewrite the file instead. `cargo run --bin devkit -- schema` prints it to stdout without touching anything.

## Sections

### `[defaults]`

| Key | Required | Meaning |
|---|---|---|
| `worktree_root` | no | Directory under which issue worktrees are created. `~` is expanded. Defaults to the primary checkout's `_worktrees` sibling, e.g. `~/git/example_worktrees` beside `~/git/example`. |
| `branch_prefix` | no | Prefix for branches created by `issue setup` (e.g. `you/`). |
| `baseline_ref` | no | Git ref the baseline server tracks (e.g. `origin/staging`). Defaults to the remote's default branch, read from `origin/HEAD`. When neither resolves, the error names both fixes: set `defaults.baseline_ref`, or run `git remote set-head origin -a`. |
| `baseline_path` | no | Checkout path for the baseline server. `~` is expanded. |
| `doppler_yaml` | no | Path to the repo's `doppler.yaml`; its `setup` paths seed app **path inference**. `~` is expanded. Absent → apps need an explicit `path`. |
| `pr_base` | no (default `"main"`) | Default base branch for PRs opened by `issue pr create`. |
| `pr_create_state` | no (default `"draft"`) | State `issue pr create` opens a PR in when neither `--draft` nor `--ready` is given: `"draft"` or `"ready"`. |
| `require_pr_reviewer` | no (default `false`) | Refuse any run that would leave a PR ready for review with no human GitHub reviewer other than the PR's own author: `issue pr create --ready`, `issue pr ready`, and the draft-to-ready flip in `issue review request`. |
| `apps_dir` | no | Directory (relative to a worktree) that holds per-app subdirectories. |
| `ignored_checks` | no | Glob patterns for status-check names to discount from a PR's CHECK verdict, e.g. a deploy left red by an unfinished PR. Matched case-insensitively against each check's name. A PR reads green when only ignored checks fail, and those failures still appear in the triage output rather than being hidden. |
| `stray_scan_width` | no (default `64`) | Width of each app's port-band scan window for stray detection: ports `[base_port, base_port + stray_scan_width)`. |
| `issue_summary` | no (default `false`) | Write the issue summary file on every `issue setup`, as though `--summary` were passed. `--summary` / `--no-summary` still decide a single run. The file's path and body come from `templates.issue_summary_path` and `templates.issue_summary`. |
| `worktree_include` | no | Glob patterns (relative to the primary checkout's root) for untracked local files copied into a newly created worktree by `issue setup` / `issue pr checkout`, at the same relative path. `issue sync-includes` re-runs the same copy against worktrees that already exist, from this one pattern list. A pattern ending in `/`, or one matching a directory, copies recursively. Existing destinations are never overwritten by default; copy failures warn and are skipped (fail-open). `issue sync-includes --overwrite` is the opt-in way to replace files a worktree already has, and it needs a scope — one or more selectors, or `--all`. A match that is a symlink is reproduced as a symlink holding the same target, and its contents are not copied, so a symlinked directory becomes one link rather than a duplicated tree. Creating a symlink on Windows needs Developer Mode or administrator rights; where it is refused, the link is skipped with a warning and the rest of the run continues. A directory match reads its whole subtree into memory before copying any of it, so peak memory during a sync scales with the largest single include's subtree rather than staying flat. A trailing `**` matches every path below its anchor, its direct children included: `a/**` covers `a/f.txt` as well as `a/b/f.txt`, and a bare `**` covers a file sitting at the checkout root. A `.` component or a repeated separator names the same path it would in any other glob — `a/./b/*` and `a//b/*` both mean `a/b/*`. A symlinked directory named as a pattern's own literal anchor (`linked/**`) is walked through rather than reproduced, because the anchor is where the walk starts rather than something it matched; write `linked/` to get the link. Anchor patterns (`apps/*/.env.local`) rather than scanning the whole tree — `**` descends into `node_modules`. |

### Path values

`worktree_root`, `baseline_path`, and `doppler_yaml` are resolved once when the config loads, in this order:

1. `${VAR}` is replaced with that environment variable. An unset variable is an error naming both the config key and the variable. `$$` is a literal `$`; a `$` followed by anything else is left alone.
2. A leading `~/` expands to `$HOME`.
3. A path that is still relative is anchored by what it names, never by the working directory, and `.` / `..` are folded out either way:
   - `worktree_root` and `baseline_path` name a location **on this machine**, so they resolve against the directory of the config file that declared them — including when that file is a linked worktree's main checkout.
   - `doppler_yaml` names a file **inside the repository being worked on**, so it resolves against the checkout reading the config instead: each worktree resolves its own copy, and a branch that edits it takes effect without merging first.

`branch_prefix` gets step 1 only.

Step 3 is what lets a project commit its `devkit.toml`:

```toml
[defaults]
worktree_root = "../myproject_worktrees"
baseline_path = "../myproject_worktrees/_baseline"
baseline_ref  = "origin/main"
```

That is correct on every machine and for every developer. Only `branch_prefix` is personal — put it in `devkit.local.toml`, or write `"${USER}/"`.

### `[apps.<name>]`

One table per runnable app. `<name>` is the app id passed to `issue setup --apps`.

| Key | Required | Meaning |
|---|---|---|
| `base_port` | yes | Base port; per-worktree ports are allocated from here via the registry. |
| `launch` | yes | The complete launch command, run verbatim. `{{ port }}` is substituted with the allocated port. Write the whole invocation here, including any `doppler run -c <config> --` wrapper and `--preserve-env=…` flags the app needs. |
| `path` | no | App subdirectory (relative to the repo) when it differs from `<name>`. |
| `url` | no | Address the app serves on, defaulting to `http://localhost:{{ port }}`. Rendered as a minijinja template over the same variables as `launch` — `{{ port }}`, `ports['<app>']`, and `[templates.variables]` — so it can carry any scheme, host, or path (`https://app.localhost:{{ port }}/admin`). It is what `devrun up` prints in its URL column and, for the `provides_url` app, what is wired into consumers' `url_env`. Devkit never terminates TLS on an app's behalf, and readiness stays a TCP probe against the allocated port. |
| `url_env` | no | Env var that receives the app's URL. |
| `provides_url` | no | `true` marks the one app whose URL other apps consume. Exactly one app should set this. `devrun` wires the provider's local port into each consumer's `url_env`, and auto-includes the provider when a consumer is run. |
| `static_env` | no | Inline env vars always set for this app. |
| `prep_files` | no | Files written into the app's directory during `issue setup`, before `setup` commands run. Each entry is `{ path, content, overwrite }` — `path` is relative to the app dir (parent dirs created), `content` is rendered as a minijinja template with the issue context (`prefix`, `issue`, `slug`, `apps`, `app`, `branch`, `worktree`) plus `[templates.variables]`, and `overwrite` (default `false`) keeps an existing file unless set to `true`. Emit a literal `{{` with `{% raw %}…{% endraw %}`. As an array, a deeper `devkit.toml` replaces the whole list rather than appending. |
| `setup` | no | Commands run in the app's directory during `issue setup`, in order. Each entry is one argv array (program + args), e.g. `[["doppler", "run", "-c", "local_config", "--", "bun", "install"]]`. Use this for installs and any doppler wiring; nothing project-specific is hardcoded in the tool. |

devkit runs `launch` exactly as written — it builds no command prefix. To use Doppler, wrap the command yourself, e.g. `launch = ["doppler","run","-c","dev_local","--","nitro","dev","--port","{{ port }}"]`.

Launch argv and `static_env` values are minijinja templates rendered per launch with strict undefined handling:

- `{{ port }}` — the app's own allocated port.
- `{{ ports['other-app'] }}` — another app's port in this worktree, resolved from the port registry. Referencing an app that isn't running writes a normal pid-less reservation which a later `devrun up other-app` claims, so a consumer can bake the port before the server exists. A typo'd app name is a hard error.
- `[templates.variables]` constants are available by name.

The old `{port}` placeholder is retired; a leftover `{port}` in a rendered value fails the launch with a migration hint.

Before starting such a server, devkit refuses a launch that resolves to the `prd` config: it reads `-c`/`--config` from `launch`, then `DOPPLER_CONFIG` from the app's env, then `doppler configure get config --scope <app dir>`; a Doppler launch whose config is `prd` or cannot be resolved is rejected.

**Migration:** earlier configs set `[defaults].doppler_config` and let devkit prepend `doppler run`. Move that wrapper into each app's `launch`, and delete the `doppler_config`, `doppler_project`, and `preserve_env` keys (fold any `--preserve-env=…` into `launch`).

To enforce a hard per-app memory cap *without* the daemon restarting the server, set a runtime or OS limit through the app's `static_env` — e.g. `static_env = { NODE_OPTIONS = "--max-old-space-size=2048" }`, or wrap `launch` in a `ulimit -v` shell. The runtime/OS aborts the process on breach and the daemon's crash-restart recovers it; this keeps enforcement in the runtime rather than the daemon's `memory_action`. On Linux with cgroup-v2 delegation the daemon also supports a first-class `memory_max_mb` kernel cap — see the `[daemon]` section below.

## Tasks

`[tasks.<name>]` defines canned oneshots run by `devrun task <name>` (`devrun task` lists them). A task is either a **command** (`run`) or a **sequence** (`steps`), never both.

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

- Command tasks run in the foreground with inherited stdio; the exit code is propagated. `run` and `env` values are minijinja templates with the same `port`/`ports` context as launches; `{{ ports['x'] }}` resolves from the port registry (issue role), writing a pid-less reservation when `x` isn't running. Env layering, low to high: app `static_env` → task `env` → CLI `--env-file` → `--env`. Tasks do not get `url_env` provider wiring — reference the app you need explicitly via `ports[...]`. Doppler invocations go through the same `prd` guard as launches.
- `require_live = ["app", …]` (command tasks only): each listed app must have a live devrun-managed server in this worktree when the task *executes*, or the task fails before spawning:

      require_live: `api-serve` has no live server in this worktree (devrun up api-serve)

  Each name must be referenced by the task's templates via `ports[...]`. A `--env`/`--env-file` override that replaces every value referencing an app waives that app's check *and* its allocation — a user-supplied URL makes the local port irrelevant (this is how one build task serves both a local flow and a hosted-preview flow). References in `run` argv cannot be overridden. The reference must go through `ports['<app>']` specifically — a task referencing its own app only via `{{ port }}` does not satisfy `require_live` for that app.
- Sequence steps are re-resolved immediately before each step runs: ports are re-allocated and templates re-rendered then, and `require_live` is checked then — so a step gated on an app that an earlier `up` step starts works, and a build step longer than the reservation grace period cannot bake a port that a later `up` step no longer gets. `--dry-run` prints the upfront resolution and never gates.
- Sequence steps run in order and stop at the first failure. `{ up = "app" }` is `devrun up app` (a no-op for a live server). A sequence may only set `description` and `steps`, and cannot reference another sequence. The CLI `--env`/`--env-file` overlay applies to every step: command steps layer it above the task `env`, and `{ up = "app" }` steps layer it above the app's `static_env`, exactly as `devrun up --env` would.
- `devrun task <name> --dry-run` prints each rendered plan (still resolving ports, so the printed values are real) without executing.

### `[daemon]`

Optional daemon-level tuning. Env overrides are listed alongside each key,
because they are the one thing `devkit schema` does not carry: run it for any
key's type and default, which it derives from the config types themselves.

#### Supervision and lifetime

| Key | Env override | Default | Meaning |
|---|---|---|---|
| `enabled` | `DEVKIT_DAEMON=1` | `false` | Run gate. The daemon autostarts only when this is true, `DEVKIT_DAEMON=1` is set, or `--supervise` is passed. |
| `max_restarts` | `DEVKIT_DAEMON_MAX_RESTARTS` | `5` | Restarts allowed inside `restart_window_secs`. This and the key below are the crash-loop budget every restart path spends. |
| `restart_window_secs` | `DEVKIT_DAEMON_RESTART_WINDOW` | `60` | Length of the sliding window `max_restarts` is counted over. |
| `health_probe_secs` | `DEVKIT_DAEMON_HEALTH_PROBE_SECS` | `0` (off) | TCP health-probe interval. `0` starts no probe thread at all. A server judged hung is SIGTERMed and respawned through the crash path, inside the budget above. |
| `health_fail_threshold` | `DEVKIT_DAEMON_HEALTH_FAIL_THRESHOLD` | `3` | Consecutive probe failures, after the server first answers, before it is judged hung. |
| `idle_timeout_secs` | `DEVKIT_DAEMON_IDLE_SECS` | `1800` | Exit after this long with zero clients and zero supervised children. |

#### Memory management

Two layers of memory control are available; they compose without conflict:

| Key | Env override | Default | Meaning |
|---|---|---|---|
| `memory_max_mb` | `DEVKIT_DAEMON_MEM_MAX_MB` | `0` (off) | Hard kernel ceiling per supervised server tree, in MB. Linux-only (cgroup-v2). See subsection below. |
| `memory_limit_mb` | `DEVKIT_DAEMON_MEM_LIMIT_MB` | `0` (off) | Soft RSS threshold, in MB. When a server's tree-RSS stays over this for `memory_limit_ticks` consecutive supervision ticks, the daemon SIGTERMs it and respawns within the crash-loop budget. Requires `memory_action = "restart"`. |
| `memory_action` | `DEVKIT_DAEMON_MEMORY_ACTION` | `"warn"` | What a `memory_limit_mb` breach does. `"warn"` logs and leaves the server alone; `"restart"` enables the soft poll-based restart. |
| `memory_limit_ticks` | `DEVKIT_DAEMON_MEM_LIMIT_TICKS` | `3` | Consecutive over-`memory_limit_mb` supervision ticks before the soft restart fires. |
| `memory_warn_mb` | `DEVKIT_DAEMON_MEM_WARN_MB` | `0` (off) | Log a loud line past this supervised tree-RSS, in MB. Warns only; it restarts nothing. |

#### `memory_max_mb` — hard cgroup-v2 kernel cap (Linux only)

`memory_max_mb` sets a hard per-server memory ceiling enforced by the kernel via a cgroup-v2 `memory.max` leaf. A server whose resident set exceeds the cap is OOM-killed by the kernel; the daemon observes that as a crash and respawns it through the existing crash-restart path. It is **not** a new restart path — the same crash-loop budget applies.

`memory_max_mb` sits **above** `memory_limit_mb`: the soft poll-based action (`memory_action = "restart"`) is the graceful first responder, acting at `memory_limit_mb`; the kernel cap at `memory_max_mb` is the backstop for spikes too fast for the 500 ms poll loop. Set `memory_max_mb` higher than `memory_limit_mb` (or omit `memory_limit_mb` entirely) to preserve this ordering.

**Requires cgroup-v2 delegation.** The daemon must run inside a delegated cgroup-v2 subtree with the memory controller enabled. The recommended setup is `devkitd install-service`, which writes a `systemd --user` unit with `Delegate=yes` — no `sudo` required. Without delegation the daemon logs a one-time warning and falls back to the soft `memory_action` path; no server spawn ever fails because cgroup setup is unavailable (fail-open).

Cap setup is **fail-open**: any cgroup error logs once and proceeds uncapped rather than blocking or killing a server. A broken cgroup configuration degrades to today's soft behavior.

**macOS / Windows**: `memory_max_mb` is documented but has no effect. The daemon stays silent (no warning) — the soft `memory_action` path remains available on all platforms.

### `[parallelism]`

Width of the worker pool devkit shares across its parallel work: the
`worktree_include` walk and copy today, and whatever else adopts
`devkit_common::pool` later.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `threads` | integer ≥ 1 | 4 | Threads in the shared pool. |

`DEVKIT_THREADS` overrides it, and an unparseable or zero value there is
ignored rather than obeyed.

A thread count describes the machine rather than the project, so it belongs in
the personal layer at `~/.config/devkit/config.toml`. A config may carry
`[parallelism]` alone, with no `[defaults]` table around it.

The default is sized for file copying, past which added threads return little.
Raising it helps most on a filesystem where a stat is slow and concurrency hides
the latency, such as a Windows drive mounted under WSL.

### `[docs]`

Per-project overlay on the global docs manifest at `~/.config/devkit/docs.toml`.
One `[[docs.libs]]` entry per library; every field except `name` is optional, so
an entry here overrides a single field of the global entry and leaves the rest.
`docs/commands.md` covers how a version is resolved and how checkouts are
referenced and pruned.

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | Id the library is addressed by on the `docm` command line, and the key an overlay entry merges onto. |
| `ecosystem` | no | Which importer graph resolves the version (`rust`, `js`, `python`, `git`). Omit to detect it from the project's lockfiles. |
| `package` | no | Registry package name when it differs from `name`, e.g. `@types/node`. |
| `repo` | no | Git URL to clone, skipping the registry lookup that would find it. |
| `ref` | no | Manual pin (tag, branch or sha). Wins over lockfile resolution. |
| `src_dir` | no | Source directory inside the checkout, overriding layout detection. |
| `docs_dir` | no | Docs directory inside the checkout, overriding layout detection. |
| `notes` | no | Freeform note surfaced by `docm info` and `docm list`: what this library is here for. |

### `[harness]`

Opt-in for the agent write-access harness.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `enforce_writes` | bool | `false` | When `true`, the devkit plugin's `PreToolUse` hook enforces write locks automatically. When absent or `false`, the hook exits immediately with no effect. |

**Three opt-in sources, with precedence.** The hook resolves whether to enforce for a given checkout from, in order:

1. **`DEVKIT_ENFORCE_WRITES`** (env) — an explicit master switch. `1`/`true`/`yes`/`on` forces enforcement on; `0`/`false`/`no`/`off` forces it off, overriding both config files. Unset/blank/unrecognized → no opinion, fall through.
2. **The project layers applying where the write happens** `[harness] enforce_writes` — every layer the config walk resolves for that location, including `devkit.local.toml` and a linked worktree's inherited main-checkout layer, opts in if any of them sets the flag. The checkout root the walk anchors to comes from asking git, not from scanning ancestors for a `.git` entry.
3. **The global config** `[harness] enforce_writes` — read from `$DEVKIT_CONFIG` (else `~/.config/devkit/config.toml`). Set it here to enforce across **every** checkout without a per-checkout file.

With the env var unset, enforcement is on when **either** a project layer **or** the global config opts in. Set `enforce_writes = true` in the global config for a machine-wide default; drop a per-checkout `devkit.toml` only when you want to opt a single checkout in (or, with the global default on, set the env var to `off` to opt a session out). The global-config and env routes need **no** per-worktree file — so they avoid shadowing the global config in `devrun`/`portm` discovery.

**What enforcement gates.** The hook intercepts `Edit`, `MultiEdit`, `Write`, and `NotebookEdit` — the structured write tools. Shell-level writes made via `Bash` are outside the harness's scope (a documented gap; coordinate those manually with `lockm acquire`).

**Activation requires `lockm` on `PATH`.** The hooks invoke bare `lockm hook <event>` — the same command as `devkit locks hook <event>`. Install `devkit` via `cargo install --path .`; its first run creates the `lockm` link automatically (or run `devkit install-links` directly). The resolved name must be reachable from the shell that runs hook commands.

**Fail-open / fail-closed behaviour.**

- *Harness off* (no opt-in from any source — env unset, no checkout `devkit.toml` flag, and no global-config flag): the hook exits 0 immediately. No locks are taken; zero overhead.
- *`lockm` absent from `PATH`*: the hook invocation fails silently and the write proceeds. This is fail-open to avoid blocking agents on machines that do not have the binary installed.
- *Registry error when the harness is on*: the hook denies the write rather than allowing it through silently (fail-closed). The deny message includes the error so the agent can report it.

**Example** — enforce everywhere via the global config (`~/.config/devkit/config.toml`):

```toml
[harness]
enforce_writes = true
```

Or per-checkout, add the same table to that checkout's own `devkit.toml`; only the `[harness]` table is read, so it may be an otherwise-empty file or a full project config. Or skip both files and set `DEVKIT_ENFORCE_WRITES=1` in the environment.

### `[brief]`

What `devkit brief` emits. The plugin's hooks call it unconditionally; these switches decide whether it produces anything.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | The whole brief. `false` suppresses every section, and is read before any work is done. |
| `pins` | `true` | The library-versions section only. |
| `locks` | `true` | The `lockm status` line. Turn it off where only one session ever works in a checkout at a time. |
| `apps` | `true` | The `Apps` line and the `devrun up` / `portm status` bullets. |
| `tasks` | `true` | The task table and the `devrun task` bullet. |

A section is omitted when the checkout has nothing to report, whatever its switch says; the switch suppresses a section the checkout *does* have. Both read the same downstream, so the bullets introducing a section go with it: `apps = false` drops the `Apps` line, the `devrun up` bullet and the `portm status` line together, and `tasks = false` drops both the task table and its bullet. The intro names only the facilities that survive, and a project left with none of them produces no devrun section at all.

Live servers have no switch. A port this worktree holds is a fact about the machine rather than a listing the brief chose to carry, so the server table appears whenever the registry has rows for the worktree — and it keeps the `devrun down` and `portm status` lines relevant even under `apps = false`.

Set it in `~/.config/devkit/config.toml` as a personal default and override it per project in that project's `devkit.toml`. A malformed `[brief]` table falls back to these defaults rather than withholding the brief.

### `[tracker]`

Which issue tracker backs the `issue` commands.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `kind` | string | _(detect)_ | `linear`, `github`, or `none`. |

devkit picks the tracker by detection: a resolvable `LINEAR_API_KEY` (environment or `~/.config/devkit/secrets.toml`) means Linear, otherwise a github.com `origin` remote means GitHub, otherwise no tracker.

`github` talks to GitHub Issues in the repository the `[github]` table below resolves as `issues_repo`. It authenticates with the token `gh auth login`, `GH_TOKEN` or `GITHUB_TOKEN` supplies — devkit stores no GitHub credential of its own; run `gh auth login` once and `devkit auth github` to see which token devkit would send and who it belongs to. On a GitHub project a bare number is a PR, not an issue, so `issue pr checkout 3340` needs no disambiguation. If no issues repository resolves there is nothing to ask, and the project falls back to running with no tracker; `devkit doctor`'s `tracker` row carries the reason.

Detection is a floor, not a convenience: a `LINEAR_API_KEY` exported globally resolves to Linear for *every* project on the machine. `kind` names the tracker outright and wins over detection: every `issue` command that talks to a tracker reads it, as does the `issue.status` MCP action. Detection is what decides only when no config resolves — a directory outside any devkit project, or a config that fails to load, still gets a tracker rather than an error.

With `kind = "none"`, `issue` still creates worktrees, tracks PRs, and reaches FINISHED on a merged PR and a clean tree, as long as the worktree carries an issue id; the STATE column reads `no tracker`. Under any other tracker the verdict also waits on the issue reaching a completed state, and a tracker that answered with nothing for that issue holds the verdict open rather than promoting the worktree.

`devkit doctor`'s `tracker` row prints which tracker resolved and why, which is the only place detection's choice is visible: it names the `kind` and whether config or detection produced it.

Detection landing on no tracker holds the verdict open too. Declaring `kind = "none"` says the project has no issue states to wait for; detection finding neither a key nor a GitHub remote says devkit found nothing to ask, which is the same silence a tracker with no key gives. `issue end` removes worktrees and deletes their branches, so it never acts on an unanswered question.

Everything that asks a tracker a question goes through the resolved one: `issue setup`'s title-derived slug and summary file, `issue pr checkout`'s disambiguation of a bare number, `issue dashboard`'s issue timeline, and the ISSUE column in `issue prs`. So each answers from the tracker this project declared rather than from whatever `LINEAR_API_KEY` happens to be exported in the shell. What stays Linear-specific is `LINEAR_WORKSPACE`, which supplies the workspace slug for clickable Linear issue links without a lookup, and `[linear] resolve_pr_links` below.

### `[github]`

Which GitHub repositories this project's issues and pull requests live in.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `issues_repo` | string | _(the `origin` remote)_ | `owner/repo` holding the issues, e.g. `org/planning`. |
| `pr_repo` | string | _(the `origin` remote)_ | `owner/repo` pull requests are opened against, e.g. `upstream/app`. |

The table is deliberately not under `[tracker]`: a Linear project with a fork workflow needs `pr_repo` just as much as a GitHub one does, and a project may track issues in a repository separate from its code.

**Each key resolves on its own, and is required only where it is used.** A project that only reads PRs never has to supply an `issues_repo`, and a project that supplies neither pays nothing until a GitHub operation asks for one. When the key an operation needs cannot be resolved, the error names that key.

**Defaulting from `origin` needs an `origin` that reaches github.com.** There is no `gh repo view` fallback and no second remote is consulted: if `origin` is absent, or reaches a host other than github.com, the key that would have defaulted from it fails and the error says to set `[github] issues_repo` / `pr_repo`. The error is the instruction.

An SSH-alias remote counts. A remote spelled `gh:owner/repo.git` names a `~/.ssh/config` `Host gh` entry rather than a hostname, so devkit asks ssh what that alias resolves to (`ssh -G gh`) and defaults from `origin` when the answer is github.com. Git itself never performs this substitution — it hands the alias to ssh — so no git command reports the real host. Resolution needs OpenSSH's `ssh -G`; where that is unavailable or the alias resolves elsewhere, the keys have to be named outright, and the error names the alias and what it resolved to.

**Unknown keys in this table are rejected — `[github]` and `[preserve.<name>]` are the only two tables in this file that do.** A misspelled `issue_repo` silently ignored would leave the project resolving a *different* repository than it declared — devkit would default from `origin` and query someone else's issues while the config appeared to say otherwise. Failing the config load is the smaller harm, so this table refuses what it does not recognise.

```toml
[github]
issues_repo = "org/planning"
pr_repo = "upstream/app"
```

`issue prs --repo owner/name` overrides `pr_repo` for a single invocation (as does the `repo` argument of the `issue.prs` MCP action); it does not touch `issues_repo`. Every repository-scoped `gh` invocation devkit makes carries the resolved repository explicitly, so an ambient `GH_REPO` cannot redirect it.

### `[linear]`

Opt-in Linear enrichment for `issue prs`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `resolve_pr_links` | bool | `false` | When `true`, `issue prs` asks Linear which issues each open PR is linked to and shows the union of the text-derived id and every linked id in the ISSUE column, deduplicated (text id first). |

The lookup authenticates with `LINEAR_API_KEY` (environment or `~/.config/devkit/secrets.toml`); **no token lives in this table**. It costs one extra batched round trip per 25 PRs, after the GitHub fetch. Fail-soft: with no key, or on any Linear error, the column falls back to the text-derived id — `issue prs` never fails because of Linear. The MCP `issue.prs` action honors the same flag.

The flag gates Linear alone: it is a `[linear]` key, and a Linear key does not become a global switch. Under the GitHub tracker the ISSUE column carries each PR's closing issues whether or not this is set, at the cost of a batched round trip of its own.

```toml
[linear]
resolve_pr_links = true
```

### `[hooks]`

Commands devkit runs when a lifecycle event fires. Each key is named `{before,after}_<event>` and holds a list of argv arrays — the program plus its arguments, run directly with no shell, so pipes, `&&`, and globs are not available.

| Key | Fires on | Cwd |
|---|---|---|
| `after_worktree_create` | `issue setup` and `issue pr checkout`, once the worktree exists and its apps are prepared, after the command has reported the worktree | the new worktree's root |
| `after_worktree_remove` | `issue end`, once per worktree it removed, after every removal in the run has finished and the stale worktree entries are pruned | the main repository root |
| `after_end` | `issue end`, once per run that removed at least one worktree, after every `after_worktree_remove` hook | the main repository root |

Most keys name a state change rather than the caller. `after_worktree_create` fires from both `issue setup` and `issue pr checkout`; naming it after either command would have been wrong for the other, and a new state key fires from every command that reaches its state. `after_end` is the deliberate exception: a run-level event has exactly one caller by construction and no worktree state it could be named for.

A worktree kept back by a required `[preserve]` failure, refused as dirty, or skipped at the confirmation prompt never fires `after_worktree_remove`. A run that removed nothing fires neither key, including the early exits that report nothing to clean up.

Each argv element is a minijinja template over `[templates.variables]` plus the keys its own event carries. `after_worktree_create` renders over `worktree`, `branch`, `issue`, `slug`, `apps`, and `prefix`. `after_worktree_remove` adds `worktree_root` and `primary`. Its worktree is already gone, so `issue`, `slug` and `apps` come from the `.devkit/issue.toml` record read before the removal. `after_end` carries `removed` (the removed worktree paths, in the order they were confirmed), `count`, `prefix`, `worktree_root`, and `primary`, and none of the single-worktree keys. `removed` renders as a single sequence: a hook argument of `{{ removed }}` lands in one argv slot holding the whole list, so a command that wants one argument per worktree cannot get it from this key. Hooks run in the order listed, and every `after_worktree_remove` finishes before `after_end` starts.

Failures are **fail-open**: a hook that cannot be rendered, cannot be spawned, or exits non-zero prints a `warning:` line to stderr, and the remaining hooks still run. The worktree already exists by the time hooks fire, so a missing program must not fail the command that created it. Hook output is captured and discarded, which keeps the JSON line the command prints last on stdout. The `issue end` keys fire after the removals have joined and the run has printed its summary, so a failing hook cannot keep a removed worktree from being reported as removed. When the main repository root does not resolve, both are skipped with a warning rather than run in a directory the removal just deleted. A `devkit.toml` that fails to load leaves `issue end` unable to read either hook key, so both silently run nothing — the same degradation `[preserve]` gets, which the command already refuses without `--no-preserve`.

As an array, a deeper `devkit.toml` replaces the whole list rather than appending — set machine-wide hooks in `~/.config/devkit/config.toml` and expect a project that defines the same key to take over entirely.

```toml
[hooks]
after_worktree_create = [["zoxide", "add", "{{ worktree }}"]]
after_worktree_remove = [["zoxide", "remove", "{{ worktree }}"]]
after_end = [["alacritree", "project", "refresh"]]
```

### `[preserve.<name>]`

Files copied out of an issue worktree before `issue end` removes it, so an
agent's scratch output outlives the worktree. Each entry names its own
destination, so one run can archive different files to different places.

| Key | Required | Meaning |
|---|---|---|
| `from` | yes | Glob patterns for the files to copy, relative to the worktree root. Each is rendered as a minijinja template. |
| `to` | yes | Destination directory, rendered as a minijinja template. Must render to a non-empty absolute path. It is created when the first file lands in it, so an entry that matches nothing leaves no directory behind. |
| `required` | no (default `false`) | Keep the worktree instead of removing it when this entry warns. |

```toml
[preserve.scratch]
from     = [".scratch/"]
to       = "{{ worktree_root }}/archive/{{ issue }}/scratch"
required = true

[preserve.notes]
from = ["docs/notes/*.md"]
to   = "{{ primary }}/.devkit/archive/{{ issue }}"
```

Render context: `worktree`, `branch`, `issue`, `slug`, `apps`, `prefix`,
`worktree_root` (the resolved `defaults.worktree_root`), `primary` (the primary
checkout's root), and `[templates.variables]`. The issue fields come from the
worktree's `.devkit/issue.toml`, so changing how worktrees or branches are named
after setup cannot misname the destination; a worktree without a readable record
renders them empty. `primary` is absent when the primary checkout cannot be
resolved, and an entry that uses it then fails naming it rather than falling
back to another path.

To archive a whole directory and everything beneath it, name the directory
itself, with or without a trailing slash: `from = [".scratch/"]`. A trailing
`**` reaches the same set — it matches every path below the named directory,
the files sitting directly inside it included. `dir/*` matches only those
direct children, one level deep.

Patterns are worktree-relative. One that could reach outside the worktree —
absolute, rooted, holding a `..` component, or drive-relative on Windows
(`C:scratch`) — is skipped with a warning: the archive cannot reach outside the
tree it is saving. The recorded summary file therefore cannot be preserved at its
default location beside the worktrees directory; set
`issue_summary_path = "{{ worktree }}/.devkit/issue.md"` to keep it inside the
worktree, where a pattern can name it. A pattern that renders empty is skipped;
one that matches nothing is not a failure, which is the normal case for a
worktree that produced no scratch.

A destination that resolves inside any worktree the same run is removing is
skipped: the copy would be deleted seconds after it was written. The check asks
the filesystem rather than comparing spellings, so a destination that reaches a
worktree through a symlink, through `..`, or — on a case-insensitive filesystem
— through different casing is caught as well. An existing destination file is
replaced, since the worktree's copy is the one about to be lost.

Failures are **fail-open**, like `[hooks]`: an entry that cannot render, is
rejected, or fails to copy prints a `warning:` line naming the entry, and the
worktree is removed anyway. `required = true` flips one entry to fail-closed —
its warnings keep that worktree, its branch, and its summary intact, and
`issue end` exits non-zero. `required` governs errors only, never emptiness.

Preservation runs before any worktree is removed, serially and in sorted entry
name order, with one progress step per worktree. Two worktrees archiving the
same filename into the same `to` collide, and worktree order decides; template
`{{ issue }}` into `to` to keep them apart.

Two limits worth knowing. Symlinks are followed on the way out, so a link inside
the worktree is archived as its target's content. This is deliberately the
opposite of `defaults.worktree_include`, which reproduces a link rather than its
contents: an include lands in a live worktree that still sits beside the primary
checkout, where a relative link resolves, while preservation archives out of a
worktree about to be deleted into a location that may outlive the link's target
entirely. And a copy is not atomic: `std::fs::copy` truncates
before writing, so a copy interrupted over an existing archive leaves a short
file. Preservation finishing before any removal is what keeps that from costing
data — nothing is deleted until every entry has run.

### `[people.<alias>]`

Teammate handle aliases used by `issue review` (`--to <alias>`). The alias maps to delivery handles; **no tokens live here** — `SLACK_TOKEN` and `LINEAR_API_KEY` come from the environment / Doppler.

| Key | Required | Meaning |
|---|---|---|
| `slack` | yes | Slack user (or channel) id, e.g. `U0XXXXXXXXX`. |
| `github` | no | GitHub login used as the default PR reviewer for this person. |

### `[templates]`

`issue setup` and `issue review` render seven strings from optional minijinja templates. Each unset key falls back to a default that matches the historical hardcoded output.

Three further keys cap how long the rendered result may be: `branch_max` (default 46), `worktree_dir_max` (default 24), and `checkout_worktree_dir_max` (default 46). A branch that cannot fit falls back to the shortest slug still worth reading; a worktree directory name that cannot fit is an error, because a limit on a filesystem path that silently does not hold is the reason these keys exist.

```toml
[templates]
branch          = "{{ prefix }}{{ issue }}-{{ slug }}"
worktree_dir    = "{{ slug }}"
pr_title        = "{{ issue }}: {{ input }}"
pr_body         = "Closes {{ issue }}.\n\n{{ input }}"
review_request  = "{{ input }} {{ pr_url }}"
review_finish   = "{{ input }} {{ pr_url }}"
issue_summary_path = "{{ worktree }}/.devkit/issue.md"   # or "notes/{{ issue }}.md", from worktree_root

[templates.variables]            # constants; a context field of the same name wins
team = "platform"
```

| Key | Default | Context |
|---|---|---|
| `branch`, `worktree_dir` | `{{ prefix }}{{ slug }}`, `{{ slug }}` | `prefix`, `issue`, `slug`, `short_slug`, `apps` |
| `checkout_worktree_dir` | `{{ pr_number }}-{{ pr_title }}` (or `{{ pr_number }}-{{ pr_title }}_[{{ linear_id }}]` when reached through an issue) | `pr_number`, `pr_title`, `linear_id`, `linear_title` (the last two carry whichever tracker answered) |
| `branch_max` | `46` | characters; the derived slug is shortened on a word boundary to fit |
| `worktree_dir_max` | `24` | characters; caps `{{ short_slug }}`, and does nothing to a template without it |
| `checkout_worktree_dir_max` | `46` | characters; caps `pr_title` and `linear_title`, splitting the budget when both are rendered |
| `pr_title` | `{{ input }}` | review base + `input` = `--pr-title` |
| `pr_body` | `{{ input }}` | review base + `input` = `--pr-body`, `pr_title` |
| `review_request` | `{{ input }} {{ pr_url }}` | review base + `input` = body arg, `pr_title`, `pr_url`, `name`, `slack_id` |
| `review_finish` | `{{ input }} {{ pr_url }}` | `pr_url`, `pr_title`, `author`, `input`, `name`, `slack_id` |
| `issue_summary_path` | `ISSUE_SUMMARY_{{ issue }}.md`, taken from `worktree_root` when relative | summary base |
| `issue_summary` | a facts header, `## Description`, then empty `## Summary` / `## Pointers` | summary base |

`short_slug` is derived from the `branch` template's own slug, so a `branch` template that renders it is unmeasured by `branch_max` and may exceed that limit by however long `short_slug` itself renders.

Summary base context for `issue_summary_path` and `issue_summary`: `issue` (the tracker's own spelling), `title`, `url`, `description`, `state`, `assignee`, `priority`, `estimate`, `labels`, `parent`, `project`, `worktree`, `branch`, `slug`, `prefix`, `apps`. Anything the tracker left empty renders as the empty string, so `{% if parent %}` drops the line rather than printing a blank one. Render `{{ worktree }}` into `issue_summary_path` to keep the file inside the worktree instead.

Review base context for `review_request`: `branch`, `issue`/`slug`/`apps` from the `.devkit/issue.toml` record `issue setup` writes in the worktree, plus `pr_url`, `pr_title`, and per-recipient `name`/`slack_id`. `issue setup` also adds `.devkit/` to your global gitignore (`--no-gitignore` skips it). An undefined variable is an error (strict mode), so typos surface immediately.

## Example

```toml
[defaults]
worktree_root  = "~/git/acme_worktrees"
branch_prefix  = "you/"
baseline_ref   = "origin/staging"
baseline_path  = "~/git/acme_worktrees/_baseline"
doppler_yaml   = "~/git/acme/app/doppler.yaml"
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
| `DEVKIT_FETCH_TTL_SECS` | `60` | Freshness window for `git fetch`. `issue setup`, `issue pr checkout`, and `devrun up`'s baseline refresh skip a fetch of the same repo+remote made within this many seconds, reusing the remote-tracking refs already on disk (so the ref a worktree is cut from is at most this stale). `0` disables the gate — always fetch. |
| `DEVKIT_HYPERLINKS` | _(detect)_ | Override OSC 8 hyperlink emission in the `issue`/`portm`/etc. tables. `always`/`1`/`on`/`true`/`yes` forces clickable links; `never`/`0`/`off`/`false`/`no` disables them. Unset auto-detects via [`supports-hyperlinks`](https://crates.io/crates/supports-hyperlinks). Set `always` for a hyperlink-capable terminal that detection misses — e.g. an alacritty fork exporting a bare `TERM=xterm-256color`. |
