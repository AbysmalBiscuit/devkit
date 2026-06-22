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
3. `./devkit.toml` (searched upward from the working directory)
4. `~/.config/devkit/config.toml`

The recommended setup is to keep your real config at
`~/.config/devkit/config.toml`, where every binary discovers it automatically —
no flag or env var needed. (`.gitignore` also ignores `/configs/*.toml`, should
you prefer to keep a copy inside a checkout.)

## Sections

### `[defaults]`

| Key | Required | Meaning |
|---|---|---|
| `worktree_root` | yes | Directory under which issue worktrees are created. `~` is expanded. |
| `branch_prefix` | yes | Prefix for branches created by `issue setup` (e.g. `you/`). |
| `baseline_ref` | yes | Git ref the baseline server tracks (e.g. `origin/staging`). |
| `baseline_path` | yes | Checkout path for the baseline server. `~` is expanded. |
| `doppler_config` | yes | Doppler config name for secret injection. **`prd` is rejected** to avoid running against production secrets. |
| `doppler_yaml` | yes | Path to the repo's `doppler.yaml` (maps paths → Doppler projects). `~` is expanded. Optional at runtime: if absent, apps need an explicit project/path. |
| `pr_base` | no (default `"staging"`) | Default base branch for PRs opened by `issue review`. |
| `apps_dir` | no | Directory (relative to a worktree) that holds per-app subdirectories. |

### `[apps.<name>]`

One table per runnable app. `<name>` is the app id passed to `issue setup --apps`.

| Key | Required | Meaning |
|---|---|---|
| `base_port` | yes | Base port; per-worktree ports are allocated from here via the registry. |
| `launch` | yes | Launch argv. `{port}` is substituted with the allocated port. |
| `path` | no | App subdirectory (relative to the repo) when it differs from `<name>`. |
| `url_env` | no | Env var that receives the app's URL. |
| `provides_url` | no | `true` marks the one app whose URL other apps consume. Exactly one app should set this. |
| `preserve_env` | no | Env vars copied through from the ambient environment. |
| `static_env` | no | Inline env vars always set for this app. |
| `prep_env` | no | Env vars written into the per-app prep file during `issue setup`. |
| `setup` | no | Commands run in the app's directory during `issue setup`, in order. Each entry is one argv array (program + args), e.g. `[["doppler", "run", "-c", "local_config", "--", "bun", "install"]]`. Use this for installs and any doppler wiring; nothing project-specific is hardcoded in the tool. |

To enforce a hard per-app memory cap *without* the daemon restarting the server,
set a runtime or OS limit through the app's `static_env` — e.g.
`static_env = { NODE_OPTIONS = "--max-old-space-size=2048" }`, or wrap `launch`
in a `ulimit -v` shell. The runtime/OS aborts the process on breach and the
daemon's crash-restart recovers it; this keeps enforcement in the runtime rather
than the daemon's `memory_action`. On Linux with cgroup-v2 delegation the daemon
also supports a first-class `memory_max_mb` kernel cap — see the `[daemon]`
section below.

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

Per-checkout opt-in for the agent write-access harness. This table is read
from the checkout's own `devkit.toml`; it is not part of the personal config
at `~/.config/devkit/config.toml`.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `enforce_writes` | bool | `false` | When `true`, the devkit plugin's `PreToolUse` hook enforces write locks automatically. When absent or `false`, the hook exits immediately with no effect. |

**What enforcement gates.** The hook intercepts `Edit`, `MultiEdit`, `Write`,
and `NotebookEdit` — the structured write tools. Shell-level writes made via
`Bash` are outside the harness's scope (a documented gap; coordinate those
manually with `lockm acquire`).

**Activation requires `lockm` on `PATH`.** The hooks invoke bare `lockm hook
<event>`. Install via `cargo install --path .`; the binary must be resolvable
from the shell that runs hook commands.

**Fail-open / fail-closed behaviour.**

- *Harness off* (`enforce_writes` absent or `false`, or no `devkit.toml`
  found): the hook exits 0 immediately. No locks are taken; zero overhead.
- *`lockm` absent from `PATH`*: the hook invocation fails silently and the
  write proceeds. This is fail-open to avoid blocking agents on machines that
  do not have the binary installed.
- *Registry error when the harness is on*: the hook denies the write rather
  than allowing it through silently (fail-closed). The deny message includes
  the error so the agent can report it.

**Example** — to opt a checkout in, add to its `devkit.toml`:

```toml
[harness]
enforce_writes = true
```

No other configuration is required. The remainder of the `devkit.toml` may be
a full project config or an otherwise empty file — only the `[harness]` table
is read by the hook.

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
doppler_config = "dev_local"
doppler_yaml   = "~/Git/acme/monorepo/doppler.yaml"
pr_base        = "staging"

[apps.api]
base_port    = 9100
launch       = ["nitro", "dev", "--port", "{port}"]
url_env      = "API_BASE_URL"
provides_url = true
preserve_env = ["SOME_JWT_SECRET"]
static_env   = { SOME_JWT_SECRET = "local-dev-placeholder-value" }

[apps.web]
base_port  = 4100
launch     = ["next", "dev", "-p", "{port}"]
url_env    = "API_BASE_URL"
prep_env   = { SOME_FEATURE_FLAG = "dummy" }
setup      = [["doppler", "run", "-c", "local_config", "--", "bun", "install"]]

[apps.worker]
base_port = 8080
path      = "services/worker"
launch    = ["uv", "run", "uvicorn", "server.main:create_app", "--factory", "--reload", "--port", "{port}"]

[people.alice]
slack  = "U0XXXXXXXXX"
github = "alice-gh"
```
