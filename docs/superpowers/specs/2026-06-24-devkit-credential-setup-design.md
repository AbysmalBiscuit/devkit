# devkit credential setup & diagnostics design

**Status:** Approved 2026-06-24
**Scope:** The "Setup help/oauth for linear and slack" item in `docs/next-steps.md`
— guided, validated, persistent credential setup for Linear and Slack from the
CLI, plus a read-only health check.

## Goal

Let a user configure the Linear and Slack credentials devkit's tooling needs
without hand-editing shell rc files or wiring Doppler around every `issue` call.
A new `devkit` binary captures a token, validates it against the live API,
stores it, and can diagnose what is missing.

Today both tokens come only from the process environment:

| Credential | Env var | Used by |
|---|---|---|
| Linear API key | `LINEAR_API_KEY` | `issue status`/`end` Done gate, `issue dashboard` timeline |
| Linear workspace slug | `LINEAR_WORKSPACE` | clickable issue links in `issue status` |
| Slack bot token | `SLACK_TOKEN` | `issue review` reviewer message |

Doppler feeds the *app servers* their env via the `[apps]` launch commands; it
does not cover devkit's own tooling credentials unless every `issue` invocation
is wrapped. This feature fills that gap.

## The `devkit` binary and its boundary

A new sixth binary, `src/bin/devkit/`, joins the four user-facing CLIs. Its
boundary, to be recorded in `AGENTS.md`:

> `devkit` configures and diagnoses the toolkit itself — credentials and
> `doctor`. The operational verbs (`portm`, `devrun`, `issue`, `lockm`) stay in
> their own binaries.

`config` deliberately stays on `devrun` — moving a command that shipped on
2026-06-23 would bundle a breaking rename into a feature, and `devrun config` is
discoverable where it is used. `devkit` is the home for cross-cutting
credential/diagnostic surface only; it does not become an umbrella over the
operational verbs.

Subcommands: `auth`, `doctor`, `completions`. Like the other four CLIs it
installs via `cargo install --path .`, installs `report::install_panic_hook`,
and exposes `completions <shell>` via `clap_complete`.

## Components

| Unit | Role |
|---|---|
| `devkit-common::secrets` (new) | the secret store at `~/.config/devkit/secrets.toml` (`0600`); the single seam every read goes through |
| `devkit-common::linear` (extend) | `validate(key) -> Result<LinearIdentity>` + pure `parse_identity(&Value)` |
| `devkit-common::slack` (extend) | `validate(token) -> Result<SlackIdentity>` + pure `parse_identity(&Value)` |
| `src/bin/devkit/` (new binary) | `auth`, `doctor`, `completions` — a thin shell over the above |

## Secret store (`devkit-common::secrets`)

A plaintext TOML file beside `config.toml`:

```toml
# ~/.config/devkit/secrets.toml  (chmod 600)
linear_api_key   = "lin_api_…"
linear_workspace = "adaptyv"
slack_token      = "xoxb-…"
```

Path resolution mirrors `config::home_config_path` exactly so the two files are
always co-located: `$HOME/.config/devkit/secrets.toml`. (`config.toml` is
HOME-based today, not `XDG_CONFIG_HOME`-based; the secrets file matches it rather
than diverging.)

```rust
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Secrets {
    pub linear_api_key: Option<String>,
    pub linear_workspace: Option<String>,
    pub slack_token: Option<String>,
}

/// Parse `~/.config/devkit/secrets.toml`. Missing file → `Secrets::default()`.
pub fn load() -> Result<Secrets>;

/// env var → secrets.toml → None. `env_key` is the uppercase env name
/// (`LINEAR_API_KEY`); the toml key is its lowercase form.
pub fn resolve(env_key: &str) -> Option<String>;

/// Read-modify-write one key, preserving the others. Creates the parent dir and
/// the file, and chmod 600 on every write so permissions cannot drift.
pub fn store(toml_key: &str, value: &str) -> Result<()>;
```

### Resolution order

Every read site applies **`$ENV` → `secrets.toml` → unset**. The environment
always wins, so a shell export or a Doppler-injected var is never overridden, and
behavior is identical to today when nothing is stored. An empty env value
(`LINEAR_API_KEY=""`) is treated as unset, preserving the existing
`.filter(|s| !s.is_empty())` semantics.

### Permissions

`store` writes the file then `chmod 600` (Unix). On Windows the chmod is a no-op;
the file inherits the user-profile ACL, which is the platform norm for
per-user secret files (`%USERPROFILE%\.config\devkit\`). The store is plaintext
by design: env still takes precedence, the file is `0600` and lives outside any
repo, and on the primary WSL2 target an OS keyring offers no at-rest encryption
anyway. A keyring backend is a deferred follow-up (see Out of scope).

## Validators

Both extend their existing module and split into a network function plus a pure
parser, matching the `linear.rs`/`slack.rs` test style (parsing is unit-tested
without the network).

### Linear

```rust
pub struct LinearIdentity {
    pub workspace_url_key: String, // organization.urlKey → also stored as linear_workspace
    pub org_name: String,
    pub viewer_email: String,
}
pub fn validate(key: &str) -> Result<LinearIdentity>;
fn parse_identity(resp: &serde_json::Value) -> Result<LinearIdentity>;
```

Query: `query { viewer { name email } organization { urlKey name } }`. A `200`
carrying an `errors` array, or a `401`, is an invalid key — surfaced as
`"invalid Linear API key"`.

### Slack

```rust
pub struct SlackIdentity { pub team: String, pub user: String, pub url: String }
pub fn validate(token: &str) -> Result<SlackIdentity>;
fn parse_identity(resp: &serde_json::Value) -> Result<SlackIdentity>;
```

`POST https://slack.com/api/auth.test` with `Authorization: Bearer <token>`.
`{ok:true}` yields the identity; `{ok:false,error}` surfaces the Slack error
(`invalid_auth`, `token_revoked`, …) via the existing `check_response` style.

## `devkit auth <provider>`

`provider` is a `clap` `ValueEnum`: `linear` | `slack`.

1. **Acquire the token**, in precedence order:
   - `--token <value>` flag, else
   - piped stdin (when stdin is not a TTY), else
   - interactive hidden prompt via `rpassword` (no echo).
2. **Validate** against the live API.
3. On failure: print the error, **store nothing**, exit non-zero.
4. On success: `store()` the token. For Linear, also store `linear_workspace`
   from the returned `urlKey` in the same step. Print a confirmation:

```
✓ linear: workspace "adaptyv" (you@adaptyv.bio)
  saved to ~/.config/devkit/secrets.toml
```

The interactive prompt names where to get the token
(`https://linear.app/settings/api` for Linear; the Slack app's OAuth & Permissions
page for Slack).

## `devkit doctor`

Read-only; stores nothing. For each credential it reports the **source**
(`env` / `file` / `unset`) and, when present, the result of a live validation:

```
$ devkit doctor
✓ linear_api_key    env    workspace "adaptyv" (you@adaptyv.bio)
✓ linear_workspace  file   adaptyv
✗ slack_token       file   invalid_auth — token rejected by auth.test
  unset linear_workspace would fall back to the Linear API at link time

linear_api_key unset → run: devkit auth linear   (https://linear.app/settings/api)
```

**Exit code:** non-zero only when a credential that *is* set fails validation (a
real, actionable problem). An `unset` credential is a warning, never a failure —
a user may legitimately not use Slack. A network error during validation is
reported as `unreachable`, not a hard failure (doctor is diagnostic).

`--json` emits the same rows as a JSON array for scripts, mirroring
`devrun config`'s `--json` flag.

## CLI surface

```
devkit auth <linear|slack> [--token <value>]
devkit doctor [--json]
devkit completions <shell>
```

`--token` and stdin exist so `auth` is scriptable and unit-testable; the
interactive `rpassword` prompt is the only uncovered shell. The validate-then-
store core takes a token string, so tests drive it directly without a TTY.

## Read-site refactor

Reroute every direct token read through `secrets::resolve` so the file fallback
is universal (the MCP issue actions inherit it for free via `devkit-issue`):

| File | Current | Becomes |
|---|---|---|
| `crates/devkit-common/src/linear.rs` | `env::var("LINEAR_WORKSPACE")`, `env::var("LINEAR_API_KEY")` | `secrets::resolve("LINEAR_WORKSPACE")`, `secrets::resolve("LINEAR_API_KEY")` |
| `crates/devkit-issue/src/status.rs` | `env::var("LINEAR_API_KEY")` | `secrets::resolve("LINEAR_API_KEY")` |
| `src/bin/issue/status.rs` | `env::var("LINEAR_API_KEY")` | `secrets::resolve("LINEAR_API_KEY")` |
| `src/bin/issue/dashboard/data.rs` | `env::var("LINEAR_API_KEY")` (×2) | `secrets::resolve("LINEAR_API_KEY")` |
| `src/bin/issue/review.rs` | `env::var("SLACK_TOKEN")` | `secrets::resolve("SLACK_TOKEN")` |

`resolve` returns `Option<String>` with empty-string-as-unset folded in, so each
call site keeps its existing shape.

## Error handling

- `anyhow` with `.context()` naming the provider (`"validating Linear API key"`,
  `"validating Slack token"`).
- `auth` store I/O errors are **fatal** — the user explicitly asked to save the
  token; a silent failure is worse than an abort. (This differs from the
  fail-open gitignore/cgroup paths, where the side effect is a convenience.)
- `doctor` never aborts on a single credential; a network failure becomes an
  `unreachable` row.

## Dependency

Add `rpassword` (`workspace.dependencies`, used by the `devkit` binary only) for
the no-echo interactive prompt. It is small and cross-platform (termios / Win
console). The dep-light ethos is honored elsewhere: `--token`/stdin already give
a non-interactive path, and `rpassword` earns its place by keeping a pasted token
out of terminal scrollback.

## Testing

TDD; `cargo test --workspace` is the merge gate.

| Unit | Tests |
|---|---|
| `secrets::load`/`resolve`/`store` | missing file → all `None`; round-trip store→load; env wins over file; empty env treated as unset; `store` preserves sibling keys; written file is `0600` (Unix) |
| `linear::parse_identity` | success body → identity with `urlKey`; `errors` body and `401`-shaped body → error |
| `slack::parse_identity` | `ok:true` → identity; `ok:false` → surfaced Slack error |
| `doctor` row computation | pure fn `(source, validation) -> Row`; covers env/file/unset × valid/invalid/unreachable; exit code derived from rows (set-but-invalid → non-zero) |
| `auth` core | validate-then-store given a token string: Linear stores key + workspace; invalid token stores nothing |

Network calls themselves are not unit-tested (the existing modules already only
test query building and parsing). The interactive prompt is the thin uncovered
shell.

## Docs

- `README.md`: add `devkit` to the binary list; the env-var section gains the
  resolution order (env → `secrets.toml`) and the `devkit auth`/`doctor` commands.
- `docs/configuration.md`: a "Secrets" section documenting `secrets.toml`, its
  `0600` permissions, the keys, and the resolution order.
- `AGENTS.md`: a layout-table row for `src/bin/devkit`, and the boundary
  statement above.
- `docs/next-steps.md`: mark the "Setup help/oauth for linear and slack" item
  resolved, referencing this spec and the plan.

## Out of scope (deferred follow-ups)

- **OAuth browser flows.** Token paste only. Real OAuth needs a hosted devkit
  OAuth app with a client secret for each provider and a local callback server —
  heavy infrastructure for a marginal UX gain on a personal CLI. Linear's
  standard path is a personal API key; Slack needs a created app regardless.
- **OS keyring backend.** The store reads through a single `secrets` seam, so a
  keyring backend can be added later behind it. Skipped now because the primary
  target is WSL2, where no keyring daemon runs and at-rest encryption does not
  materialize.
- **Broader onboarding wizard.** Capturing `worktree_root`, `branch_prefix`, or
  the app catalog is a separate concern from credentials and is not part of this
  feature.
