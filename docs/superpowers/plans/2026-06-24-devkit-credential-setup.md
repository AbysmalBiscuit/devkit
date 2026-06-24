# devkit Credential Setup & Diagnostics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `devkit` binary that captures, validates, and stores Linear/Slack
credentials (`devkit auth`) and diagnoses what is configured (`devkit doctor`),
backed by a `0600` secret store that every token read falls back to.

**Architecture:** A new `devkit-common::secrets` module owns
`~/.config/devkit/secrets.toml` and a resolver (`env → file → unset`). The
existing `linear`/`slack` modules gain live-API validators split into pure
parsers. A new sixth binary, `src/bin/devkit/`, is a thin shell: `auth` validates
then stores; `doctor` reports source + validity per credential. All existing
token reads route through the resolver, so the file fallback is universal.

**Tech Stack:** Rust 2024, `anyhow`, `ureq` (existing), `toml`, `clap` +
`clap_complete`, `rpassword` (new, for no-echo prompts).

**Spec:** `docs/superpowers/specs/2026-06-24-devkit-credential-setup-design.md`

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/devkit-common/src/secrets.rs` (new) | secret store + resolver: `Secrets`, `Source`, `load`/`resolve`/`source`/`store`/`store_at`, pure `pick`/`source_of` |
| `crates/devkit-common/src/lib.rs` (modify) | register `pub mod secrets;` |
| `crates/devkit-common/Cargo.toml` (modify) | add `toml.workspace = true` |
| `crates/devkit-common/src/linear.rs` (modify) | add `LinearIdentity`, `validate`, pure `parse_identity` |
| `crates/devkit-common/src/slack.rs` (modify) | add `SlackIdentity`, `validate`, pure `parse_identity` |
| `crates/devkit-common/src/linear.rs`, `crates/devkit-issue/src/status.rs`, `src/bin/issue/{status.rs,review.rs,dashboard/data.rs}` (modify) | route token reads through `secrets::resolve` |
| `src/bin/devkit/main.rs` (new) | CLI entry: `auth`, `doctor`, `completions` |
| `src/bin/devkit/auth.rs` (new) | `auth` command: acquire → validate → store |
| `src/bin/devkit/doctor.rs` (new) | `doctor` command: rows, exit code, `--json` |
| `Cargo.toml` (modify) | add `rpassword` + `ureq` to the root binary package; add `rpassword` to workspace deps |
| `README.md`, `docs/configuration.md`, `AGENTS.md`, `docs/next-steps.md` (modify) | document the binary, the store, and the resolution order |

---

## Task 1: Secret store (`devkit-common::secrets`)

**Files:**
- Modify: `crates/devkit-common/Cargo.toml`
- Create: `crates/devkit-common/src/secrets.rs`
- Modify: `crates/devkit-common/src/lib.rs`

- [ ] **Step 1: Add the `toml` dependency to devkit-common**

`crates/devkit-common` does not yet depend on `toml`. In
`crates/devkit-common/Cargo.toml`, under `[dependencies]`, add the line after
`serde_json.workspace = true`:

```toml
toml.workspace = true
```

- [ ] **Step 2: Register the module**

In `crates/devkit-common/src/lib.rs`, add (keep the list alphabetical — insert
after `pub mod report;`):

```rust
pub mod secrets;
```

- [ ] **Step 3: Write the secret store with its tests**

Create `crates/devkit-common/src/secrets.rs`:

```rust
//! The credential store: `~/.config/devkit/secrets.toml`.
//!
//! Tokens resolve env-first, then this file, so a shell export or a
//! Doppler-injected var always wins and behavior is unchanged when nothing is
//! stored. The file is written `0600` and lives beside `config.toml`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Secrets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear_workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slack_token: Option<String>,
}

impl Secrets {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            "linear_api_key" => self.linear_api_key.as_deref(),
            "linear_workspace" => self.linear_workspace.as_deref(),
            "slack_token" => self.slack_token.as_deref(),
            _ => None,
        }
    }

    fn set(&mut self, key: &str, value: String) -> Result<()> {
        let slot = match key {
            "linear_api_key" => &mut self.linear_api_key,
            "linear_workspace" => &mut self.linear_workspace,
            "slack_token" => &mut self.slack_token,
            other => anyhow::bail!("unknown secret key: {other}"),
        };
        *slot = Some(value);
        Ok(())
    }
}

/// Where a credential was resolved from.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    Env,
    File,
    Unset,
}

/// `$HOME/.config/devkit/secrets.toml` — beside `config.toml`, which is also
/// HOME-based (not `XDG_CONFIG_HOME`-based) so the two stay co-located.
pub fn secrets_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_default();
    home.join(".config/devkit/secrets.toml")
}

fn nonempty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

/// env wins over file; empty strings count as unset.
fn pick(env_val: Option<String>, file_val: Option<String>) -> Option<String> {
    nonempty(env_val).or_else(|| nonempty(file_val))
}

fn source_of(env_val: Option<String>, file_val: Option<String>) -> Source {
    if nonempty(env_val).is_some() {
        Source::Env
    } else if nonempty(file_val).is_some() {
        Source::File
    } else {
        Source::Unset
    }
}

fn load_from(path: &Path) -> Result<Secrets> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Secrets::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_to(path: &Path, s: &Secrets) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let body = toml::to_string_pretty(s).context("serializing secrets")?;
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    chmod_600(path)
}

#[cfg(unix)]
fn chmod_600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", path.display()))
}

#[cfg(not(unix))]
fn chmod_600(_path: &Path) -> Result<()> {
    Ok(())
}

/// Persist one credential at `path`, preserving the others. Creates the parent
/// dir and the file `0600`. Public for path-injected tests.
pub fn store_at(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut s = load_from(path)?;
    s.set(key, value.to_string())?;
    write_to(path, &s)
}

/// Parse the secrets file; a missing file is an empty `Secrets`.
pub fn load() -> Result<Secrets> {
    load_from(&secrets_path())
}

/// Resolve a credential: `$<env_key>` → `secrets.toml[<lowercased key>]` → `None`.
pub fn resolve(env_key: &str) -> Option<String> {
    let env_val = std::env::var(env_key).ok();
    let file_val = load()
        .ok()
        .and_then(|s| s.get(&env_key.to_ascii_lowercase()).map(str::to_string));
    pick(env_val, file_val)
}

/// Where `env_key` currently resolves from.
pub fn source(env_key: &str) -> Source {
    let env_val = std::env::var(env_key).ok();
    let file_val = load()
        .ok()
        .and_then(|s| s.get(&env_key.to_ascii_lowercase()).map(str::to_string));
    source_of(env_val, file_val)
}

/// Persist one credential to the default path, preserving the others.
pub fn store(key: &str, value: &str) -> Result<()> {
    store_at(&secrets_path(), key, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("devkit-secrets-{}-{name}", std::process::id()))
            .join("secrets.toml")
    }

    #[test]
    fn missing_file_is_empty() {
        assert_eq!(load_from(&tmp("missing")).unwrap(), Secrets::default());
    }

    #[test]
    fn store_then_load_round_trips() {
        let p = tmp("round");
        let _ = std::fs::remove_file(&p);
        store_at(&p, "linear_api_key", "lin_123").unwrap();
        store_at(&p, "linear_workspace", "adaptyv").unwrap();
        let s = load_from(&p).unwrap();
        assert_eq!(s.linear_api_key.as_deref(), Some("lin_123"));
        assert_eq!(s.linear_workspace.as_deref(), Some("adaptyv"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn store_preserves_siblings() {
        let p = tmp("siblings");
        let _ = std::fs::remove_file(&p);
        store_at(&p, "linear_api_key", "k").unwrap();
        store_at(&p, "slack_token", "xoxb").unwrap();
        let s = load_from(&p).unwrap();
        assert_eq!(s.linear_api_key.as_deref(), Some("k"));
        assert_eq!(s.slack_token.as_deref(), Some("xoxb"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn env_wins_over_file() {
        assert_eq!(pick(Some("e".into()), Some("f".into())), Some("e".into()));
        assert_eq!(pick(None, Some("f".into())), Some("f".into()));
        assert_eq!(pick(Some(String::new()), Some("f".into())), Some("f".into()));
        assert_eq!(pick(None, None), None);
    }

    #[test]
    fn source_reflects_precedence() {
        assert_eq!(source_of(Some("e".into()), Some("f".into())), Source::Env);
        assert_eq!(source_of(Some(String::new()), Some("f".into())), Source::File);
        assert_eq!(source_of(None, None), Source::Unset);
    }

    #[test]
    fn unknown_key_rejected() {
        assert!(Secrets::default().set("nope", "x".into()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stored_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let p = tmp("perms");
        let _ = std::fs::remove_file(&p);
        store_at(&p, "slack_token", "xoxb").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(&p);
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p devkit-common secrets`
Expected: 7 tests pass (`stored_file_is_0600` only on unix).

- [ ] **Step 5: Clippy**

Run: `cargo clippy -p devkit-common --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-common/Cargo.toml crates/devkit-common/src/lib.rs crates/devkit-common/src/secrets.rs
git commit -m "feat(common): add credential secret store"
```

---

## Task 2: Credential validators

**Files:**
- Modify: `crates/devkit-common/src/linear.rs`
- Modify: `crates/devkit-common/src/slack.rs`

- [ ] **Step 1: Write the failing Linear parser test**

In `crates/devkit-common/src/linear.rs`, inside the existing `#[cfg(test)] mod
tests`, add:

```rust
#[test]
fn linear_identity_parsed() {
    let v = serde_json::json!({
        "data": { "viewer": { "email": "me@x.io" },
                  "organization": { "urlKey": "adaptyv", "name": "Adaptyv" } }
    });
    let id = parse_identity(&v).unwrap();
    assert_eq!(id.workspace_url_key, "adaptyv");
    assert_eq!(id.org_name, "Adaptyv");
    assert_eq!(id.viewer_email, "me@x.io");
}

#[test]
fn linear_errors_body_is_invalid() {
    let v = serde_json::json!({ "errors": [{ "message": "authentication failed" }] });
    let e = parse_identity(&v).unwrap_err();
    assert!(e.to_string().contains("invalid Linear API key"));
}

#[test]
fn linear_missing_org_is_invalid() {
    let v = serde_json::json!({ "data": { "viewer": { "email": "" }, "organization": {} } });
    assert!(parse_identity(&v).is_err());
}
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `cargo test -p devkit-common linear_identity_parsed`
Expected: FAIL — `cannot find function parse_identity` / `LinearIdentity`.

- [ ] **Step 3: Implement the Linear validator**

In `crates/devkit-common/src/linear.rs`, add near the top-level items (after the
`LinearState` struct is fine):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearIdentity {
    /// `organization.urlKey` — also persisted as `linear_workspace`.
    pub workspace_url_key: String,
    pub org_name: String,
    pub viewer_email: String,
}

/// Validate `key` against Linear, returning the caller's identity. The ureq
/// error is preserved as the top-level error (no `.context`) so a caller can
/// downcast it to distinguish an unreachable host from a rejected key.
pub fn validate(key: &str) -> Result<LinearIdentity> {
    let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({
            "query": "query { viewer { email } organization { urlKey name } }"
        }))?
        .into_json()?;
    parse_identity(&resp)
}

fn parse_identity(resp: &serde_json::Value) -> Result<LinearIdentity> {
    if let Some(errors) = resp.get("errors").and_then(|e| e.as_array()) {
        let msg = errors
            .first()
            .and_then(|e| e["message"].as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("invalid Linear API key: {msg}");
    }
    let org = &resp["data"]["organization"];
    let viewer = &resp["data"]["viewer"];
    let url_key = org["urlKey"]
        .as_str()
        .context("invalid Linear API key: no organization in response")?;
    Ok(LinearIdentity {
        workspace_url_key: url_key.to_string(),
        org_name: org["name"].as_str().unwrap_or("").to_string(),
        viewer_email: viewer["email"].as_str().unwrap_or("").to_string(),
    })
}
```

`Context` is already imported at the top of `linear.rs` (`use anyhow::{Context,
Result};`).

- [ ] **Step 4: Run the Linear tests**

Run: `cargo test -p devkit-common linear`
Expected: the three new tests plus the existing `linear` tests pass.

- [ ] **Step 5: Write the failing Slack parser test**

In `crates/devkit-common/src/slack.rs`, inside `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn slack_identity_parsed() {
    let v = serde_json::json!({
        "ok": true, "team": "Adaptyv", "user": "devkit",
        "url": "https://adaptyv.slack.com/"
    });
    let id = parse_identity(&v).unwrap();
    assert_eq!(id.team, "Adaptyv");
    assert_eq!(id.user, "devkit");
    assert_eq!(id.url, "https://adaptyv.slack.com/");
}

#[test]
fn slack_not_ok_surfaces_error() {
    let v = serde_json::json!({ "ok": false, "error": "invalid_auth" });
    let e = parse_identity(&v).unwrap_err();
    assert!(e.to_string().contains("invalid_auth"));
}
```

- [ ] **Step 6: Run it to confirm it fails**

Run: `cargo test -p devkit-common slack_identity_parsed`
Expected: FAIL — `cannot find function parse_identity` / `SlackIdentity`.

- [ ] **Step 7: Implement the Slack validator**

In `crates/devkit-common/src/slack.rs`, first widen the import at the top from
`use anyhow::{Result, bail};` to:

```rust
use anyhow::{Context, Result, bail};
```

Then add these top-level items (after `post_message`):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackIdentity {
    pub team: String,
    pub user: String,
    pub url: String,
}

/// Validate `token` via `auth.test`, returning the bot/user identity. The ureq
/// error is preserved as the top-level error (no `.context`) so a caller can
/// downcast it to tell an unreachable host from a rejected token.
pub fn validate(token: &str) -> Result<SlackIdentity> {
    let resp: serde_json::Value = ureq::post("https://slack.com/api/auth.test")
        .set("Authorization", &format!("Bearer {token}"))
        .call()?
        .into_json()?;
    parse_identity(&resp)
}

fn parse_identity(resp: &serde_json::Value) -> Result<SlackIdentity> {
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("unknown error");
        bail!("Slack token rejected: {err}");
    }
    Ok(SlackIdentity {
        team: resp["team"].as_str().unwrap_or("").to_string(),
        user: resp["user"].as_str().unwrap_or("").to_string(),
        url: resp["url"].as_str().unwrap_or("").to_string(),
    })
}
```

`Context` is now imported so `validate`'s `?` conversions compile; it is used by
the `.context`-free body via the `?` operator on `ureq`/`io` errors. If clippy
flags `Context` as unused, drop it from the import — the `?` conversions rely on
`From`, not `Context`. (Keep it only if a `.context` call is added.)

- [ ] **Step 8: Run the Slack tests**

Run: `cargo test -p devkit-common slack`
Expected: the two new tests plus the existing `slack` tests pass.

- [ ] **Step 9: Clippy**

Run: `cargo clippy -p devkit-common --all-targets -- -D warnings`
Expected: no warnings. (If `Context` is unused in `slack.rs`, remove it from the
import per Step 7's note.)

- [ ] **Step 10: Commit**

```bash
git add crates/devkit-common/src/linear.rs crates/devkit-common/src/slack.rs
git commit -m "feat(common): add credential validators"
```

---

## Task 3: Route token reads through the store

**Files:**
- Modify: `crates/devkit-common/src/linear.rs:48-59` (`workspace_url_key`)
- Modify: `crates/devkit-issue/src/status.rs` (the `LINEAR_API_KEY` read in `gather`)
- Modify: `src/bin/issue/status.rs` (the `LINEAR_API_KEY` read)
- Modify: `src/bin/issue/dashboard/data.rs` (two `LINEAR_API_KEY` reads)
- Modify: `src/bin/issue/review.rs` (the `SLACK_TOKEN` read)

These are mechanical call-site swaps to `devkit_common::secrets::resolve`, which
already folds empty-string-as-unset. `resolve` returns `Option<String>`, matching
each site's existing `Option` shape.

- [ ] **Step 1: Reroute `linear::workspace_url_key`**

In `crates/devkit-common/src/linear.rs`, replace the body of `workspace_url_key`:

```rust
pub fn workspace_url_key() -> Option<String> {
    if let Some(slug) = std::env::var("LINEAR_WORKSPACE")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return Some(slug);
    }
    let key = std::env::var("LINEAR_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())?;
    fetch_url_key(&key).ok().flatten()
}
```

with:

```rust
pub fn workspace_url_key() -> Option<String> {
    if let Some(slug) = crate::secrets::resolve("LINEAR_WORKSPACE") {
        return Some(slug);
    }
    let key = crate::secrets::resolve("LINEAR_API_KEY")?;
    fetch_url_key(&key).ok().flatten()
}
```

- [ ] **Step 2: Reroute `devkit-issue` status**

In `crates/devkit-issue/src/status.rs`, replace:

```rust
    let key = std::env::var("LINEAR_API_KEY").ok();
```

with:

```rust
    let key = devkit_common::secrets::resolve("LINEAR_API_KEY");
```

(`devkit-issue` already depends on `devkit-common`.)

- [ ] **Step 3: Reroute the `issue` status binary**

In `src/bin/issue/status.rs`, replace:

```rust
    let key = std::env::var("LINEAR_API_KEY").ok();
```

with:

```rust
    let key = devkit_common::secrets::resolve("LINEAR_API_KEY");
```

- [ ] **Step 4: Reroute the dashboard data reads**

In `src/bin/issue/dashboard/data.rs`, replace the guard in `issues`:

```rust
    let Ok(key) = std::env::var("LINEAR_API_KEY") else {
        return Vec::new();
    };
```

with:

```rust
    let Some(key) = devkit_common::secrets::resolve("LINEAR_API_KEY") else {
        return Vec::new();
    };
```

and in `origin`, replace:

```rust
    if let Ok(key) = std::env::var("LINEAR_API_KEY")
```

with:

```rust
    if let Some(key) = devkit_common::secrets::resolve("LINEAR_API_KEY")
```

- [ ] **Step 5: Reroute the review Slack read**

In `src/bin/issue/review.rs`, replace:

```rust
    match std::env::var("SLACK_TOKEN").ok().filter(|t| !t.is_empty()) {
```

with:

```rust
    match devkit_common::secrets::resolve("SLACK_TOKEN") {
```

- [ ] **Step 6: Verify no direct token-env reads remain**

Run:

```bash
rg -n 'env::var\("(LINEAR_API_KEY|LINEAR_WORKSPACE|SLACK_TOKEN)"\)' \
   crates/devkit-common/src crates/devkit-issue/src src/bin/issue
```

Expected: no matches.

- [ ] **Step 7: Build and run the full suite**

Run: `cargo test --workspace`
Expected: all tests pass — behavior is unchanged when nothing is stored (env
still resolves first), and the rerouted modules compile.

- [ ] **Step 8: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/devkit-common/src/linear.rs crates/devkit-issue/src/status.rs \
        src/bin/issue/status.rs src/bin/issue/dashboard/data.rs src/bin/issue/review.rs
git commit -m "refactor: resolve tokens through the secret store"
```

---

## Task 4: `devkit` binary with `auth`

**Files:**
- Modify: `Cargo.toml` (root) — add `rpassword` + `ureq` deps and the workspace dep
- Create: `src/bin/devkit/main.rs`
- Create: `src/bin/devkit/auth.rs`

The binary is auto-discovered from `src/bin/devkit/main.rs` (no `[[bin]]` entry
needed — only `devkitd` needs one, for its feature gate). This task wires only
`auth` and `completions`; `doctor` arrives in Task 5.

- [ ] **Step 1: Add the dependencies**

In the root `Cargo.toml`, under `[workspace.dependencies]`, add (after the
`ureq = …` line):

```toml
rpassword = "7"
```

Under the root `[dependencies]` (the `devkit` package), add (after
`serde_json.workspace = true`):

```toml
ureq.workspace = true
rpassword.workspace = true
```

(`ureq` is needed for the `doctor` downcast in Task 5 and is harmless here.)

- [ ] **Step 2: Write the failing auth store test**

Create `src/bin/devkit/auth.rs` with the store helpers and their test (the
command wiring is added in Step 4):

```rust
use anyhow::{Context, Result};
use devkit_common::{linear, secrets, slack};
use std::io::{IsTerminal, Read};
use std::path::Path;

use crate::Provider;

fn store_linear(path: &Path, token: &str, id: &linear::LinearIdentity) -> Result<()> {
    secrets::store_at(path, "linear_api_key", token)?;
    secrets::store_at(path, "linear_workspace", &id.workspace_url_key)?;
    Ok(())
}

fn store_slack(path: &Path, token: &str) -> Result<()> {
    secrets::store_at(path, "slack_token", token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_store_persists_key_and_workspace() {
        let p = std::env::temp_dir()
            .join(format!("devkit-auth-{}", std::process::id()))
            .join("secrets.toml");
        let _ = std::fs::remove_file(&p);
        let id = linear::LinearIdentity {
            workspace_url_key: "adaptyv".into(),
            org_name: "Adaptyv".into(),
            viewer_email: "me@x.io".into(),
        };
        store_linear(&p, "lin_secret", &id).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("lin_secret"));
        assert!(body.contains("adaptyv"));
        let _ = std::fs::remove_file(&p);
    }
}
```

- [ ] **Step 3: Confirm it fails to build**

Run: `cargo test --bin devkit linear_store_persists`
Expected: FAIL — `src/bin/devkit/main.rs` does not exist yet (no `crate::Provider`,
no binary target). This is expected; Step 4 adds `main.rs`.

- [ ] **Step 4: Write the binary entry and complete `auth.rs`**

Create `src/bin/devkit/main.rs`:

```rust
use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

mod auth;

#[derive(Parser)]
#[command(name = "devkit", about = "Configure and diagnose the devkit toolkit")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate and store a Linear or Slack credential.
    Auth {
        provider: Provider,
        /// Provide the token non-interactively instead of being prompted.
        #[arg(long)]
        token: Option<String>,
    },
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions { shell: Shell },
}

#[derive(Clone, Copy, ValueEnum)]
enum Provider {
    Linear,
    Slack,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Provider::Linear => "Linear",
            Provider::Slack => "Slack",
        }
    }
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkit");
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Auth { provider, token } => auth::run(provider, token),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "devkit", &mut std::io::stdout());
            Ok(())
        }
    }
}
```

Then append the command body to `src/bin/devkit/auth.rs` (above the `#[cfg(test)]`
module):

```rust
pub fn run(provider: Provider, token: Option<String>) -> Result<()> {
    let token = acquire(provider, token)?;
    let path = secrets::secrets_path();
    match provider {
        Provider::Linear => {
            let id = linear::validate(&token).context("validating Linear API key")?;
            store_linear(&path, &token, &id)?;
            println!("✓ linear: workspace \"{}\" ({})", id.workspace_url_key, id.viewer_email);
        }
        Provider::Slack => {
            let id = slack::validate(&token).context("validating Slack token")?;
            store_slack(&path, &token)?;
            println!("✓ slack: team \"{}\" (user {})", id.team, id.user);
        }
    }
    println!("  saved to {}", path.display());
    Ok(())
}

fn acquire(provider: Provider, token: Option<String>) -> Result<String> {
    if let Some(t) = token {
        return Ok(t.trim().to_string());
    }
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading token from stdin")?;
        return Ok(buf.trim().to_string());
    }
    eprintln!("{}", hint(provider));
    let entered = rpassword::prompt_password(format!("Paste your {} token: ", provider.label()))
        .context("reading token")?;
    Ok(entered.trim().to_string())
}

fn hint(provider: Provider) -> &'static str {
    match provider {
        Provider::Linear => "Create a Personal API Key at https://linear.app/settings/api",
        Provider::Slack => "Create a bot token on your Slack app's OAuth & Permissions page",
    }
}
```

- [ ] **Step 5: Run the auth test**

Run: `cargo test --bin devkit`
Expected: `linear_store_persists_key_and_workspace` passes.

- [ ] **Step 6: Smoke-test the CLI surface**

Run:

```bash
cargo run --bin devkit -- --help
cargo run --bin devkit -- auth --help
cargo run --bin devkit -- completions bash | head -1
```

Expected: top-level help lists `auth` and `completions`; `auth --help` shows the
`<PROVIDER>` positional (`linear`, `slack`) and `--token`; the completions script
prints (first line begins the bash completion).

- [ ] **Step 7: Clippy**

Run: `cargo clippy --bin devkit --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/bin/devkit/main.rs src/bin/devkit/auth.rs
git commit -m "feat(devkit): add devkit binary with auth command"
```

---

## Task 5: `devkit doctor`

**Files:**
- Create: `src/bin/devkit/doctor.rs`
- Modify: `src/bin/devkit/main.rs` (register `mod doctor;` + the `Doctor` subcommand)

- [ ] **Step 1: Write the failing exit-code test**

Create `src/bin/devkit/doctor.rs`:

```rust
use anyhow::Result;
use devkit_common::secrets::{self, Source};
use devkit_common::{linear, slack};

#[derive(Debug, PartialEq, Eq)]
enum Check {
    Ok(String),
    Invalid(String),
    Unreachable,
    Unset(&'static str),
}

struct Row {
    key: &'static str,
    source: Source,
    check: Check,
}

const HINT_LINEAR: &str = "run: devkit auth linear   (https://linear.app/settings/api)";
const HINT_SLACK: &str = "run: devkit auth slack    (Slack app → OAuth & Permissions)";
const HINT_WORKSPACE: &str = "optional — falls back to the Linear API for issue links";

/// Exit non-zero only when a credential that is set fails validation. An unset
/// credential is a warning; an unreachable host is not a hard failure.
fn worst_exit(rows: &[Row]) -> i32 {
    if rows.iter().any(|r| matches!(r.check, Check::Invalid(_))) {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(check: Check) -> Row {
        Row { key: "x", source: Source::Unset, check }
    }

    #[test]
    fn invalid_fails_exit() {
        let rows = vec![row(Check::Ok("ok".into())), row(Check::Invalid("bad".into()))];
        assert_eq!(worst_exit(&rows), 1);
    }

    #[test]
    fn unset_and_unreachable_pass_exit() {
        let rows = vec![
            row(Check::Unset("h")),
            row(Check::Unreachable),
            row(Check::Ok("ok".into())),
        ];
        assert_eq!(worst_exit(&rows), 0);
    }
}
```

- [ ] **Step 2: Confirm it fails to build**

Run: `cargo test --bin devkit invalid_fails_exit`
Expected: FAIL — `main.rs` does not declare `mod doctor;` yet, so the module is
not compiled. (Add the declaration in Step 4.)

- [ ] **Step 3: Implement the doctor body**

Append to `src/bin/devkit/doctor.rs`, above the `#[cfg(test)]` module:

```rust
fn is_unreachable(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<ureq::Error>(), Some(ureq::Error::Transport(_)))
}

fn validate_linear(v: &str) -> Check {
    match linear::validate(v) {
        Ok(id) => Check::Ok(format!("workspace \"{}\" ({})", id.workspace_url_key, id.viewer_email)),
        Err(e) if is_unreachable(&e) => Check::Unreachable,
        Err(e) => Check::Invalid(e.to_string()),
    }
}

fn validate_slack(v: &str) -> Check {
    match slack::validate(v) {
        Ok(id) => Check::Ok(format!("team \"{}\" (user {})", id.team, id.user)),
        Err(e) if is_unreachable(&e) => Check::Unreachable,
        Err(e) => Check::Invalid(e.to_string()),
    }
}

fn gather() -> Vec<Row> {
    vec![
        Row {
            key: "linear_api_key",
            source: secrets::source("LINEAR_API_KEY"),
            check: match secrets::resolve("LINEAR_API_KEY") {
                Some(v) => validate_linear(&v),
                None => Check::Unset(HINT_LINEAR),
            },
        },
        Row {
            key: "linear_workspace",
            source: secrets::source("LINEAR_WORKSPACE"),
            check: match secrets::resolve("LINEAR_WORKSPACE") {
                Some(v) => Check::Ok(v),
                None => Check::Unset(HINT_WORKSPACE),
            },
        },
        Row {
            key: "slack_token",
            source: secrets::source("SLACK_TOKEN"),
            check: match secrets::resolve("SLACK_TOKEN") {
                Some(v) => validate_slack(&v),
                None => Check::Unset(HINT_SLACK),
            },
        },
    ]
}

fn source_label(s: &Source) -> &'static str {
    match s {
        Source::Env => "env",
        Source::File => "file",
        Source::Unset => "unset",
    }
}

fn print_human(rows: &[Row]) {
    for r in rows {
        let (mark, detail) = match &r.check {
            Check::Ok(d) => ("✓", d.clone()),
            Check::Invalid(d) => ("✗", d.clone()),
            Check::Unreachable => ("?", "unreachable".to_string()),
            Check::Unset(hint) => ("·", format!("unset — {hint}")),
        };
        println!("{mark} {:16} {:5} {detail}", r.key, source_label(&r.source));
    }
}

fn print_json(rows: &[Row]) {
    let arr: Vec<_> = rows
        .iter()
        .map(|r| {
            let (status, detail): (&str, Option<String>) = match &r.check {
                Check::Ok(d) => ("ok", Some(d.clone())),
                Check::Invalid(d) => ("invalid", Some(d.clone())),
                Check::Unreachable => ("unreachable", None),
                Check::Unset(h) => ("unset", Some((*h).to_string())),
            };
            serde_json::json!({
                "key": r.key,
                "source": source_label(&r.source),
                "status": status,
                "detail": detail,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&arr).unwrap());
}

pub fn run(json: bool) -> Result<()> {
    let rows = gather();
    if json {
        print_json(&rows);
    } else {
        print_human(&rows);
    }
    if worst_exit(&rows) != 0 {
        std::process::exit(1);
    }
    Ok(())
}
```

- [ ] **Step 4: Wire the subcommand into `main.rs`**

In `src/bin/devkit/main.rs`, add `mod doctor;` next to `mod auth;`:

```rust
mod auth;
mod doctor;
```

add the `Doctor` variant to `enum Cmd` (after `Auth { … }`):

```rust
    /// Check configured credentials and report what is missing.
    Doctor {
        /// Emit the report as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
```

and add its arm to the `match cli.cmd` (after the `Auth` arm):

```rust
        Cmd::Doctor { json } => doctor::run(json),
```

- [ ] **Step 5: Run the doctor tests**

Run: `cargo test --bin devkit`
Expected: `invalid_fails_exit`, `unset_and_unreachable_pass_exit`, and the Task-4
auth test all pass.

- [ ] **Step 6: Smoke-test doctor**

Run:

```bash
env -u LINEAR_API_KEY -u LINEAR_WORKSPACE -u SLACK_TOKEN \
  cargo run --bin devkit -- doctor
echo "exit: $?"
env -u LINEAR_API_KEY -u LINEAR_WORKSPACE -u SLACK_TOKEN \
  cargo run --bin devkit -- doctor --json | head -1
```

Expected: with nothing set, every row prints `· <key> unset — …`, exit `0`; the
`--json` form prints a JSON array opening with `[`.

- [ ] **Step 7: Clippy**

Run: `cargo clippy --bin devkit --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add src/bin/devkit/main.rs src/bin/devkit/doctor.rs
git commit -m "feat(devkit): add doctor command"
```

---

## Task 6: Documentation

**Files:**
- Modify: `README.md`
- Modify: `docs/configuration.md`
- Modify: `AGENTS.md`
- Modify: `docs/next-steps.md`

- [ ] **Step 1: README — binary count, new section, env resolution**

In `README.md`, change the intro line:

```
A Rust workspace of five binaries that coordinate local development for a monorepo.
```

to:

```
A Rust workspace of six binaries that coordinate local development for a monorepo.
```

Add a new binary section after the `### lockm: File Locks` section (and before
the next `##`-level heading):

````markdown
### `devkit`: Setup & Diagnostics

Configures and diagnoses the toolkit itself. `auth` validates a Linear or Slack
credential against the live API and stores it in `~/.config/devkit/secrets.toml`
(`0600`); `doctor` reports where each credential resolves from and whether it is
valid. Tokens always resolve env-first, so a shell export or Doppler-injected var
still wins.

```
devkit auth <linear|slack> [--token <value>]   # validate + store; prompts (no echo) by default
devkit doctor [--json]                          # check configured credentials
devkit completions <shell>
```

- **`auth`**: prompts for the token without echo (or reads `--token`/piped stdin),
  validates it, and saves it. For Linear it also stores the workspace slug derived
  from the API, so issue links work without setting `LINEAR_WORKSPACE`.
- **`doctor`**: one row per credential — source (`env`/`file`/`unset`) and live
  validity. Exits non-zero only when a credential that *is* set fails validation.
````

Then in the **Optional** environment list, replace the three credential bullets:

```
- `$LINEAR_API_KEY`: enables the Linear issue-Done gate in `issue status`/`issue end` and the issue timeline in `issue dashboard`
- `$LINEAR_WORKSPACE`: enables clickable Linear issue links in `issue status`
- `$SLACK_TOKEN`: lets `issue review` post the reviewer message directly (otherwise it emits a `SlackIntent` JSON object)
```

with:

```
- `$LINEAR_API_KEY`: enables the Linear issue-Done gate in `issue status`/`issue end` and the issue timeline in `issue dashboard`
- `$LINEAR_WORKSPACE`: enables clickable Linear issue links in `issue status`
- `$SLACK_TOKEN`: lets `issue review` post the reviewer message directly (otherwise it emits a `SlackIntent` JSON object)

Each of these resolves env-first, then from `~/.config/devkit/secrets.toml`. Run
`devkit auth <linear|slack>` to store them, or `devkit doctor` to check them.
```

- [ ] **Step 2: configuration.md — Secrets section**

In `docs/configuration.md`, after the `## Location` section (before the next
`##`-level section), insert:

````markdown
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
````

- [ ] **Step 3: AGENTS.md — layout row and boundary**

In `AGENTS.md`, update the opening sentence count (the workspace description),
changing "five CLIs" to "six CLIs" where it reads:

```
a root `devkit` binary package whose five CLIs live
```
→
```
a root `devkit` binary package whose six CLIs live
```

In the `## Commands` block, bump the two binary-count comments by one (devkit
now ships alongside the rest):

```
cargo build --release                       # all five binaries → target/release
cargo install --path .                       # install all five into ~/.cargo/bin
```
→
```
cargo build --release                       # all six binaries → target/release
cargo install --path .                       # install all six into ~/.cargo/bin
```

Add a row to the layout table after the `src/bin/lockm.rs` row:

```
| `src/bin/devkit` | credential setup + diagnostics: `auth` (validate + store Linear/Slack tokens), `doctor` |
```

In the "Invariants (do not break)" or "Conventions" section, add a boundary note
(append as a new bullet under **Conventions**):

```
- **`devkit` configures and diagnoses the toolkit itself** — credentials
  (`auth`) and `doctor`. The operational verbs (`portm`, `devrun`, `issue`,
  `lockm`) stay in their own binaries; `config` stays on `devrun`. Token reads
  resolve through `devkit-common::secrets` (env → `secrets.toml`), never from
  `config.toml`.
```

Also update the line listing the user-facing CLIs:

```
The four user-facing CLIs (`portm`, `devrun`, `issue`, `lockm`) each expose a
`completions <shell>` subcommand via `clap_complete`.
```
→
```
The five user-facing CLIs (`portm`, `devrun`, `issue`, `lockm`, `devkit`) each
expose a `completions <shell>` subcommand via `clap_complete`.
```

- [ ] **Step 4: next-steps.md — mark resolved**

In `docs/next-steps.md`, replace the section:

```
  ## Setup help/oath for linear and slack

  Better setup/ux would be achieved with step by step instructions and/or just direct
  oauth/token setup from the cli.
```

with:

```
## Setup help/oauth for linear and slack

**Status:** RESOLVED 2026-06-24 — the `devkit` binary provides `devkit auth
<linear|slack>` (validate a token against the live API and store it in
`~/.config/devkit/secrets.toml`, `0600`) and `devkit doctor` (report each
credential's source and validity). Tokens resolve env-first, then from the
secrets file, via `devkit-common::secrets`. OAuth browser flows and an OS-keyring
backend are deferred follow-ups. See
`docs/superpowers/specs/2026-06-24-devkit-credential-setup-design.md` and
`docs/superpowers/plans/2026-06-24-devkit-credential-setup.md`.
```

- [ ] **Step 5: Verify the whole gate is green**

Run:

```bash
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Expected: six binaries build (including `devkit`); all tests pass; no clippy
warnings; formatting clean.

- [ ] **Step 6: Commit**

```bash
git add README.md docs/configuration.md AGENTS.md docs/next-steps.md
git commit -m "docs: document devkit credential setup"
```

---

## Notes for the implementer

- **`cargo install --path .` installs all binaries**, so `devkit` ships
  alongside the others automatically once `src/bin/devkit/main.rs` exists; no
  `[[bin]]` entry is required (only `devkitd` needs one, for its feature gate).
- **The MCP issue actions inherit the file fallback for free** — they call
  `devkit-issue::status::gather`, which Task 3 reroutes through `secrets::resolve`.
- **Why the validators omit `.context` on the network call:** `doctor` downcasts
  the returned error to `ureq::Error` to tell an unreachable host (`Transport`)
  from a rejected token (`Status`). Wrapping with `.context` would bury the
  `ureq::Error` as a source and break the downcast. `auth` adds its own
  `.context("validating … ")` at its call site, which is fine — `auth` never
  downcasts.
- **Tests never touch the real `~/.config/devkit/secrets.toml`** — the pure
  helpers (`pick`, `source_of`, `worst_exit`) take values directly, and the
  file-touching tests use `store_at`/`load_from` with explicit temp paths.
```
