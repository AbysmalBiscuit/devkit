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
