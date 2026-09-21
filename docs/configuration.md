# devkit configuration

devkit's engine is project-agnostic; every project- and machine-specific detail lives in TOML config. This page covers where that config lives and how its files combine. The reference for each key is generated:

- `devkit schema` prints the JSON Schema. Its descriptions are the doc comments on the config types, so every key's type, default and meaning comes from the same source the binaries parse. `cargo doc -p devkit-config --open` shows the same text with a worked example per table.
- `devkit schema init` points a `devkit.toml` at that schema, so an editor shows the same descriptions on hover.
- The `using-devkit` skill's [`references/config.md`](../skills/using-devkit/references/config.md) carries what spans several keys: path resolution, write enforcement, tracker detection, and the preserve and hook rules.

devkit parses the config with the TOML 1.1 grammar, so a `devkit.toml` may use what 1.1 added: newlines and a trailing comma inside an inline table, and the `\e` escape in a basic string. Nothing in devkit requires any of it. Editors, linters and other tools reading the same file may still be on 1.0, so a config that has to stay portable is safer written as 1.0.

## Location

The config is resolved from the first of:

1. `--config <path>` (global flag on every binary)
2. `$DEVKIT_CONFIG`
3. `./devkit.toml` and `./devkit.local.toml` (searched upward from the working directory)
4. `~/.config/devkit/config.toml`

Personal settings (worktree paths, local secrets, teammate handles) belong at `~/.config/devkit/config.toml`, where every binary finds them with no flag or env var. `.gitignore` also ignores `/configs/*.toml`, should you prefer to keep a copy inside a checkout.

`devkit.local.toml` is the untracked twin of `devkit.toml`: same shape, same schema, and it overrides the `devkit.toml` beside it. Settings one machine or one checkout needs go there, so the repository's `devkit.toml` carries only what the project shares. It stands alone too: a directory holding only a `devkit.local.toml` is a devkit project. Ignoring it is the repository's job; devkit does not write a `.gitignore` entry for you.

## Layering

Every `devkit.toml` from the filesystem root down to the cwd is merged, with `~/.config/devkit/config.toml` as the lowest-precedence base layer beneath them all. Deeper files override shallower ones per value: tables merge key-by-key, while scalars and arrays replace wholesale. `devkit config` prints the merged result, headed by the layer files in precedence order; `--origin` traces each value to the file it came from and names the layers it overrode.

Two escapes bypass the walk:

- `[config] root = true` in a `devkit.toml` or `devkit.local.toml` stops the upward walk at that directory and drops every shallower layer, the home config included. Full isolation.
- `--config <path>` or `$DEVKIT_CONFIG` selects a single file verbatim, with no layering and no home base.

## Secrets

Credentials are **not** stored in `config.toml`. They resolve env-first, then from a separate `~/.config/devkit/secrets.toml` written `0600`:

```toml
# ~/.config/devkit/secrets.toml  (chmod 600)
linear_api_key   = "lin_api_..."
linear_workspace = "adaptyv"
slack_token      = "xoxb-..."
```

Resolution order for each credential is `$ENV` -> `secrets.toml` -> unset, so a shell export or a Doppler-injected variable always overrides the file. Populate the file with `devkit auth <linear|slack>` (it validates the token against the live API before saving) and inspect it with `devkit doctor`. GitHub has no entry: devkit uses the token `gh auth login`, `GH_TOKEN` or `GITHUB_TOKEN` supplies, and `devkit auth github` reports which.

## Editor support

`devkit.toml` has a published JSON Schema, so an editor running the TOML language server ([taplo](https://taplo.tamasfe.dev)) gives completion, hover docs and inline validation.

```sh
devkit schema init                # ./devkit.toml
devkit schema init path/to/devkit.toml
devkit schema init devkit.local.toml
```

That adds the `#:schema` directive to an existing config, or writes a starter one when there is none. It is idempotent: a file already naming a schema is left untouched, so it is safe to run against a config under review.

The starter has every setting commented out, so nothing is active until you have read it, and `devkit brief` reports the config as not resolving until you uncomment `[defaults]`. The values are filled in from the checkout rather than left as placeholders, so what you uncomment is already right.

To do it by hand, the directive is a taplo **header**: first line, preceded only by other directives and comments.

```toml
#:schema https://github.com/AbysmalBiscuit/devkit/releases/latest/download/devkit-config.json

[defaults]
worktree_root = "~/git/example_worktrees"
```

A filesystem path works too, which is how to validate against an unreleased schema, including devkit's own checkout, where the release URL does not yet resolve:

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

The schema catches what config resolution would otherwise only report at run time, and only through `devkit config`: an `[apps.x]` without `base_port` or `launch`, a value of the wrong type, a task step that is neither `task` nor `up`, an unknown `ecosystem`. What it leaves out, on purpose:

- **Nothing is required at the top level, `[defaults]` included.** A layer carries only what it overrides, and devkit's own `devkit.toml` is `[harness]` and nothing else. An editor validates the file in front of it, so requiring `[defaults]` would mark correct overlays as errors.
- **Unknown keys pass.** devkit ignores keys it does not recognise, so a schema that rejected them would be stricter than the parser. A misspelled `lock = false` still silently does nothing.
- **Post-parse rules are invisible to it**, such as an app needing a `path` when there is no `doppler.yaml` to infer one, or a task setting exactly one of `run`/`steps`.

One caveat from layering: `base_port` and `launch` are marked required on each app, which is right for an app defined in one file and wrong for one whose keys are split across the home config and a project overlay. That split is rare enough to be worth the check.

Regenerate it after changing any config type:

```sh
DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema
```

`cargo test` fails if the committed file is stale, printing a unified diff of what moved; the env var makes that same run rewrite the file instead. `cargo run --bin devkit -- schema` prints it to stdout without touching anything.

## Example

Every table in one config. A test parses this block out of this page on every run, so it stays valid.

```toml
[defaults]
worktree_root  = "~/git/acme_worktrees"
branch_prefix  = "you/"
baseline_ref   = "origin/staging"
baseline_dir   = "~/git/acme_worktrees/_baselines"
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

Env-only tuning knobs with no `config.toml` equivalent. The `[daemon]` keys, `[parallelism] threads` and the `[harness]` switches also have environment overrides, named in each key's description.

| Variable | Default | Meaning |
|---|---|---|
| `DEVKIT_CALLER` | _(detect)_ | Override caller-identity detection behind `required = "agents"`/`"humans"` and the `devrun task` args listing. `agent` or `human` (case- and whitespace-insensitive) forces that classification; anything else is ignored. Unset, a harness session variable (`CLAUDE_CODE_SESSION_ID`, `CODEX_SESSION_ID`) counts as an agent, else a non-terminal stdin counts as an agent, else the caller is human. Set `human` to see a task's human-facing args from a non-terminal shell, e.g. `DEVKIT_CALLER=human devrun task`. |
| `DEVKIT_FETCH_TTL_SECS` | `60` | Freshness window for `git fetch`. `issue setup`, `issue pr checkout`, and `devrun up`'s baseline refresh skip a fetch of the same repo+remote made within this many seconds, reusing the remote-tracking refs already on disk (so the ref a worktree is cut from is at most this stale). `0` disables the gate: always fetch. |
| `DEVKIT_HYPERLINKS` | _(detect)_ | Override OSC 8 hyperlink emission in the `issue`/`portm`/etc. tables. `always`/`1`/`on`/`true`/`yes` forces clickable links; `never`/`0`/`off`/`false`/`no` disables them. Unset auto-detects via [`supports-hyperlinks`](https://crates.io/crates/supports-hyperlinks). Set `always` for a hyperlink-capable terminal that detection misses, e.g. an alacritty fork exporting a bare `TERM=xterm-256color`. |
| `DEVKIT_HELP` | _(detect)_ | `terse` or `full` pins which help view `--help` prints, wherever output goes. |
| `DEVKIT_TIMING` | _(off)_ | `summary` or `trace` turns on `issue`/`devrun` IO timing without `--timing`. |

### TLS trust

Every devkit HTTPS call (GitHub, Linear, Slack, and the package registries `docm add` queries) trusts the Mozilla roots bundled into the binary plus the machine's certificate store. A CA installed there by an intercepting proxy, a corporate network, or a GitHub Enterprise instance is trusted with no devkit setting. `SSL_CERT_FILE` (a PEM bundle) and `SSL_CERT_DIR` (a colon-separated list of directories), when set, replace the platform store as the second source. Certificate verification cannot be turned off. A rejected certificate reports `TLS certificate not trusted` and names the store devkit checked.
