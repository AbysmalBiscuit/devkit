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
than the daemon's `memory_action`.

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
