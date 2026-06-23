# devkit Implementation Plan

> **Historical snapshot (2026-06-19).** Superseded; binary/layout names here are out of date. See README.md, AGENTS.md, and docs/superpowers/specs/ for the current design.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Rust workspace of five binaries (`portman`, `devrun`, `issue-prep`, `issue-end`, `pr-status`) that coordinate local dev: a flock'd port registry, a supervised dev-app runner with baseline A/B, mechanical issue-setup, and the migrated worktree/PR triage tools.

**Architecture:** Two library crates — `devkit-common` (gh/git/doppler wrappers, worktree discovery, id parsing, Linear client, XDG paths, table/style helpers) and `devkit-ports` (config, app catalog, flock'd registry) — with five thin binary crates linking them. The registry is a single JSON file guarded by an advisory file lock; a reservation row (not a long-held lock) prevents the alloc race. All app/launch/env data lives in `devkit.toml`, so the engine is project-agnostic and the app catalog is config, not code.

**Tech Stack:** Rust (edition 2024, rustc 1.96), `clap`, `serde`/`serde_json`, `toml`, `serde_yaml`, `fd-lock`, `nix`, `comfy-table`, `anstyle`, `supports-hyperlinks`, `ureq`, `anyhow`, `thiserror`.

**Resolved facts baked in:**
- API-URL env var is **`FOUNDRY_API_BASE_URL`**, value `http://localhost:<api_port>` (lab-os/foundry-portal rewrite `/api/v1/*` to it).
- `launch` is defined in `devkit.toml` and run under **our** `dev_local` doppler — deliberately bypassing app `dev` scripts (e.g. `website`'s script uses `doppler -c prd`). A `prd` denylist guards the doppler config.
- State dir: `~/.claude/state/devkit/` (`ports.json`, `ports.lock`, `logs/`). Cache dir: `$XDG_CACHE_HOME/devkit/` (or `~/.cache/devkit/`).
- Old `~/.local/bin/*.py` / `~/.claude/scripts/*.sh` are **not** deleted by this work (user cleans up).

---

## File structure

```
~/Git/lev/devkit/
├── Cargo.toml                                  # [workspace] + [workspace.dependencies]
├── crates/
│   ├── devkit-common/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # re-exports
│   │       ├── paths.rs          # state/cache/log dir resolution
│   │       ├── cmd.rs            # git()/gh()/doppler()/run() wrappers + errors
│   │       ├── worktree.rs       # discover(), issue_id_of()
│   │       ├── linear.rs         # batched GraphQL "Done" gate
│   │       └── ui.rs             # comfy-table builder, color, OSC8 links, stderr spinner
│   ├── devkit-ports/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs         # Config/Defaults/AppConfig structs + discovery + load
│   │       ├── doppler.rs        # parse doppler.yaml → {app path → project}
│   │       ├── apps.rs           # App struct, catalog build (config + doppler merge)
│   │       └── registry.rs       # Registry, Entry, Role; lock/load/save; alloc/release/prune; liveness
│   │   └── tests/registry.rs     # multiprocess race test
│   ├── portman/{Cargo.toml,src/main.rs}
│   ├── devrun/{Cargo.toml,src/main.rs,src/env.rs,src/supervise.rs,src/baseline.rs}
│   ├── issue-prep/{Cargo.toml,src/main.rs}
│   ├── issue-end/{Cargo.toml,src/main.rs}
│   └── pr-status/{Cargo.toml,src/main.rs}
├── configs/example.toml
└── README.md
```

---

## Phase 0 — Workspace skeleton

### Task 0: Convert `cargo init` output into a workspace

**Files:**
- Modify: `Cargo.toml` (root)
- Delete: `src/main.rs`, `src/` (the default bin)
- Create: `crates/devkit-common/Cargo.toml`, `crates/devkit-common/src/lib.rs`, `crates/devkit-ports/Cargo.toml`, `crates/devkit-ports/src/lib.rs`, and `crates/{portman,devrun,issue-prep,issue-end,pr-status}/Cargo.toml` + `src/main.rs`

- [ ] **Step 1: Replace root `Cargo.toml` with a workspace manifest**

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
version = "0.1.0"

[workspace.dependencies]
anyhow = "1"
thiserror = "2"
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
serde_yaml = "0.9"
fd-lock = "4"
nix = { version = "0.29", features = ["signal", "process"] }
comfy-table = "7"
anstyle = "1"
supports-hyperlinks = "3"
ureq = { version = "2", features = ["json"] }
devkit-common = { path = "crates/devkit-common" }
devkit-ports = { path = "crates/devkit-ports" }
```

- [ ] **Step 2: Remove the default binary**

Run: `git rm src/main.rs && rmdir src 2>/dev/null; true`

- [ ] **Step 3: Create the two lib crates**

`crates/devkit-common/Cargo.toml`:
```toml
[package]
name = "devkit-common"
edition.workspace = true
version.workspace = true

[dependencies]
anyhow.workspace = true
thiserror.workspace = true
serde = { workspace = true }
serde_json.workspace = true
comfy-table.workspace = true
anstyle.workspace = true
supports-hyperlinks.workspace = true
ureq.workspace = true
nix.workspace = true
```
`crates/devkit-common/src/lib.rs`:
```rust
pub mod cmd;
pub mod linear;
pub mod paths;
pub mod ui;
pub mod worktree;
```
`crates/devkit-ports/Cargo.toml`:
```toml
[package]
name = "devkit-ports"
edition.workspace = true
version.workspace = true

[dependencies]
anyhow.workspace = true
thiserror.workspace = true
serde = { workspace = true }
serde_json.workspace = true
toml.workspace = true
serde_yaml.workspace = true
fd-lock.workspace = true
nix.workspace = true
devkit-common.workspace = true
```
`crates/devkit-ports/src/lib.rs`:
```rust
pub mod apps;
pub mod config;
pub mod doppler;
pub mod registry;
```

- [ ] **Step 4: Create five bin-crate skeletons**

For each of `portman, devrun, issue-prep, issue-end, pr-status`, create `crates/<bin>/Cargo.toml`:
```toml
[package]
name = "<bin>"
edition.workspace = true
version.workspace = true

[dependencies]
anyhow.workspace = true
clap.workspace = true
devkit-common.workspace = true
devkit-ports.workspace = true   # omit for pr-status (only needs devkit-common)
```
and `crates/<bin>/src/main.rs`:
```rust
fn main() -> anyhow::Result<()> {
    Ok(())
}
```
(For `pr-status`, drop the `devkit-ports` line — it only needs `devkit-common`.)

- [ ] **Step 5: Stub the lib modules so the workspace compiles**

Create empty `pub` module files for every module named in the two `lib.rs` files (e.g. `crates/devkit-common/src/cmd.rs` containing only `// implemented in a later task`). An empty `.rs` file is a valid empty module.

- [ ] **Step 6: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: PASS (compiles, no warnings that fail; empty crates).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "chore: restructure into cargo workspace (common + ports libs, five bins)"
```

---

## Phase 1 — `devkit-common`

### Task 1: `paths.rs` — state/cache/log dirs

**Files:**
- Modify: `crates/devkit-common/src/paths.rs`
- Test: inline `#[cfg(test)]` module in the same file

- [ ] **Step 1: Write the failing test**

```rust
// crates/devkit-common/src/paths.rs
use std::path::PathBuf;

/// `~/.claude/state/devkit/` — registry + lock + logs live here.
pub fn state_dir() -> PathBuf {
    home().join(".claude/state/devkit")
}
pub fn registry_file() -> PathBuf { state_dir().join("ports.json") }
pub fn lock_file() -> PathBuf { state_dir().join("ports.lock") }
pub fn logs_dir() -> PathBuf { state_dir().join("logs") }

/// `$XDG_CACHE_HOME/devkit` or `~/.cache/devkit`.
pub fn cache_dir() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("devkit"),
        _ => home().join(".cache/devkit"),
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").expect("HOME must be set"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_under_state() {
        assert!(registry_file().ends_with(".claude/state/devkit/ports.json"));
        assert!(logs_dir().ends_with(".claude/state/devkit/logs"));
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p devkit-common paths`
Expected: PASS (this module is self-contained; test ships with impl).

- [ ] **Step 3: Commit**

```bash
git add crates/devkit-common/src/paths.rs
git commit -m "feat(common): state/cache/log path helpers"
```

### Task 2: `cmd.rs` — subprocess wrappers

**Files:**
- Modify: `crates/devkit-common/src/cmd.rs`

- [ ] **Step 1: Implement and test the wrappers**

```rust
// crates/devkit-common/src/cmd.rs
use anyhow::{Context, Result, bail};
use std::process::Command;

/// Run a command, capture stdout; error includes stderr on non-zero exit.
pub fn capture(program: &str, args: &[&str], cwd: Option<&str>) -> Result<String> {
    let mut c = Command::new(program);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c
        .output()
        .with_context(|| format!("failed to spawn `{program}`"))?;
    if !out.status.success() {
        bail!(
            "`{program} {}` failed ({}):\n{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `git -C <cwd> <args...>`.
pub fn git(args: &[&str], cwd: &str) -> Result<String> {
    let mut full = vec!["-C", cwd];
    full.extend_from_slice(args);
    capture("git", &full, None)
}

/// `gh <args...>` parsed as JSON.
pub fn gh_json<T: serde::de::DeserializeOwned>(args: &[&str], cwd: &str) -> Result<T> {
    let out = capture("gh", args, Some(cwd))?;
    let trimmed = out.trim();
    let raw = if trimmed.is_empty() { "[]" } else { trimmed };
    serde_json::from_str(raw).with_context(|| "parsing gh JSON output")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_reports_stderr_on_failure() {
        let err = capture("sh", &["-c", "echo boom >&2; exit 3"], None).unwrap_err();
        assert!(err.to_string().contains("boom"));
    }
    #[test]
    fn capture_returns_stdout() {
        assert_eq!(capture("echo", &["hi"], None).unwrap().trim(), "hi");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p devkit-common cmd`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/devkit-common/src/cmd.rs
git commit -m "feat(common): git/gh subprocess wrappers with stderr-aware errors"
```

### Task 3: `worktree.rs` — discovery + issue-id

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs`

- [ ] **Step 1: Write the failing test (pure parser first)**

```rust
// crates/devkit-common/src/worktree.rs
use crate::cmd::git;
use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String, // "DETACHED" if none
}

/// Parse `git worktree list --porcelain` output. First entry is the main repo.
pub fn parse_porcelain(out: &str) -> Vec<Worktree> {
    let mut all = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut flush = |p: &mut Option<String>, b: &mut Option<String>, v: &mut Vec<Worktree>| {
        if let Some(pp) = p.take() {
            v.push(Worktree { path: PathBuf::from(pp), branch: b.take().unwrap_or_else(|| "DETACHED".into()) });
        }
    };
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut path, &mut branch, &mut all);
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = Some(b.to_string());
        }
    }
    flush(&mut path, &mut branch, &mut all);
    all
}

/// Derive an `ENG-1234`-style id from a branch or directory name, uppercased.
pub fn issue_id_of(branch: &str, path: &std::path::Path) -> String {
    let dir = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    for src in [branch, dir] {
        if let Some(m) = find_id(src) {
            return m.to_uppercase();
        }
    }
    "UNKNOWN".into()
}

fn find_id(s: &str) -> Option<String> {
    // first run of letters-dash-digits, e.g. eng-1234
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() { i += 1; }
            if i < bytes.len() && bytes[i] == b'-' {
                let dash = i;
                i += 1;
                let ds = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
                if i > ds {
                    return Some(format!("{}-{}", &s[start..dash], &s[ds..i]));
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// (main_repo_path, other_worktrees) from a path inside any worktree.
pub fn discover(start: &str) -> Result<(PathBuf, Vec<Worktree>)> {
    let out = git(&["worktree", "list", "--porcelain"], start)?;
    let mut all = parse_porcelain(&out);
    anyhow::ensure!(!all.is_empty(), "not inside a git repo: {start}");
    let main = all.remove(0);
    Ok((main.path, all))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn parses_two_worktrees() {
        let out = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /repo/eng-1\nHEAD def\nbranch refs/heads/lev/eng-1234-x\n";
        let wts = parse_porcelain(out);
        assert_eq!(wts.len(), 2);
        assert_eq!(wts[1].branch, "lev/eng-1234-x");
    }
    #[test]
    fn id_from_branch_then_dir() {
        assert_eq!(issue_id_of("lev/eng-1234-fix", Path::new("/x")), "ENG-1234");
        assert_eq!(issue_id_of("DETACHED", Path::new("/x/abc-9")), "ABC-9");
        assert_eq!(issue_id_of("main", Path::new("/x/scratch")), "UNKNOWN");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p devkit-common worktree`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(common): worktree discovery + issue-id parsing"
```

### Task 4: `ui.rs` + `linear.rs` stubs with real signatures

**Files:**
- Modify: `crates/devkit-common/src/ui.rs`, `crates/devkit-common/src/linear.rs`

- [ ] **Step 1: `ui.rs` — table + OSC8 link helpers**

```rust
// crates/devkit-common/src/ui.rs
use comfy_table::{Table, presets::NOTHING};

/// A borderless table with the given header row.
pub fn table(headers: &[&str]) -> Table {
    let mut t = Table::new();
    t.load_preset(NOTHING);
    t.set_header(headers.iter().copied());
    t
}

/// OSC8 hyperlink when the terminal supports it; otherwise just the label.
pub fn link(label: &str, url: &str) -> String {
    if supports_hyperlinks::on(supports_hyperlinks::Stream::Stdout) {
        format!("\x1b]8;;{url}\x1b\\{label}\x1b]8;;\x1b\\")
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn link_plain_when_unsupported() {
        // In test env stdout is not a tty; link == label.
        assert_eq!(link("PR #1", "https://x"), "PR #1");
    }
}
```

- [ ] **Step 2: `linear.rs` — batched Done-gate client**

```rust
// crates/devkit-common/src/linear.rs
use anyhow::Result;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearState {
    pub kind: String, // completed | started | unstarted | backlog | triage | canceled
    pub name: String, // "Done"
}

/// Build the batched GraphQL query for the given `ENG-1234` ids. Pure → testable.
pub fn build_query(ids: &[String]) -> Option<(String, HashMap<String, String>)> {
    let mut aliases = HashMap::new();
    let mut parts = Vec::new();
    for (idx, id) in ids.iter().enumerate() {
        let (team, num) = id.split_once('-')?;
        let alias = format!("i{idx}");
        aliases.insert(alias.clone(), id.clone());
        parts.push(format!(
            "{alias}: issues(filter: {{ team: {{ key: {{ eq: \"{}\" }} }}, number: {{ eq: {} }} }}) {{ nodes {{ identifier state {{ type name }} }} }}",
            team.to_uppercase(), num
        ));
    }
    if parts.is_empty() { return None; }
    Some((format!("query {{ {} }}", parts.join(" ")), aliases))
}

/// Query Linear; returns id → state. Empty map if no key/ids or on network error.
pub fn states(ids: &[String], key: Option<&str>) -> HashMap<String, LinearState> {
    let (Some(key), Some((query, aliases))) = (key, build_query(ids)) else {
        return HashMap::new();
    };
    match fetch(&query, &aliases, key) {
        Ok(m) => m,
        Err(e) => { eprintln!("Linear lookup failed: {e}"); HashMap::new() }
    }
}

fn fetch(query: &str, aliases: &HashMap<String, String>, key: &str) -> Result<HashMap<String, LinearState>> {
    let resp: serde_json::Value = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", key)
        .send_json(ureq::json!({ "query": query }))?
        .into_json()?;
    let mut out = HashMap::new();
    if let Some(data) = resp.get("data").and_then(|d| d.as_object()) {
        for (alias, block) in data {
            if let (Some(id), Some(node)) = (
                aliases.get(alias),
                block.get("nodes").and_then(|n| n.get(0)),
            ) {
                let st = &node["state"];
                out.insert(id.clone(), LinearState {
                    kind: st["type"].as_str().unwrap_or("").to_string(),
                    name: st["name"].as_str().unwrap_or("").to_string(),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_aliases_each_id() {
        let (q, a) = build_query(&["ENG-1".into(), "ABC-22".into()]).unwrap();
        assert!(q.contains("number: { eq: 1 }"));
        assert!(q.contains("number: { eq: 22 }"));
        assert_eq!(a.len(), 2);
    }
    #[test]
    fn empty_ids_no_query() {
        assert!(build_query(&[]).is_none());
    }
}
```

- [ ] **Step 3: Run tests + commit**

Run: `cargo test -p devkit-common`
Expected: PASS
```bash
git add crates/devkit-common/src/ui.rs crates/devkit-common/src/linear.rs
git commit -m "feat(common): table/link helpers + batched Linear Done-gate client"
```

---

## Phase 2 — `devkit-ports`: config & catalog

### Task 5: `config.rs` + `doppler.rs`

**Files:**
- Modify: `crates/devkit-ports/src/config.rs`, `crates/devkit-ports/src/doppler.rs`

- [ ] **Step 1: `doppler.rs` — parse `doppler.yaml` to path→project**

```rust
// crates/devkit-ports/src/doppler.rs
use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct DopplerFile { setup: Vec<Entry> }
#[derive(Deserialize)]
struct Entry { project: String, path: String }

/// Map repo-relative app path (e.g. "apps/api") → doppler project.
pub fn path_to_project(yaml: &str) -> Result<HashMap<String, String>> {
    let f: DopplerFile = serde_yaml::from_str(yaml)?;
    Ok(f.setup.into_iter().map(|e| (e.path, e.project)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_path_to_project() {
        let y = "setup:\n  - project: api-foundry\n    config: dev_local\n    path: apps/api\n";
        let m = path_to_project(y).unwrap();
        assert_eq!(m.get("apps/api").unwrap(), "api-foundry");
    }
}
```

- [ ] **Step 2: `config.rs` — typed config + discovery + load**

```rust
// crates/devkit-ports/src/config.rs
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub defaults: Defaults,
    pub apps: HashMap<String, AppConfig>,
}

#[derive(Debug, Deserialize)]
pub struct Defaults {
    pub worktree_root: String,
    pub branch_prefix: String,
    pub baseline_ref: String,
    pub baseline_path: String,
    pub doppler_config: String,
    pub doppler_yaml: String,
}

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub base_port: u16,
    pub launch: Vec<String>,
    #[serde(default)]
    pub url_env: Option<String>,
    #[serde(default)]
    pub preserve_env: Vec<String>,
    #[serde(default)]
    pub static_env: HashMap<String, String>,
    /// Optional overrides; normally derived from doppler.yaml.
    #[serde(default)]
    pub doppler_project: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

impl Config {
    pub fn parse(s: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(s).context("parsing devkit.toml")?;
        anyhow::ensure!(
            cfg.defaults.doppler_config != "prd",
            "refusing config with doppler_config = prd"
        );
        Ok(cfg)
    }
}

/// `--config` → `$DEVKIT_CONFIG` → `./devkit.toml` walking up → `~/.config/devkit/config.toml`.
pub fn locate(explicit: Option<&Path>, start: &Path) -> Option<PathBuf> {
    if let Some(p) = explicit { return Some(p.to_path_buf()); }
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") { return Some(PathBuf::from(p)); }
    let mut dir = Some(start);
    while let Some(d) = dir {
        let c = d.join("devkit.toml");
        if c.is_file() { return Some(c); }
        dir = d.parent();
    }
    let home = std::env::var_os("HOME")?;
    let fallback = PathBuf::from(home).join(".config/devkit/config.toml");
    fallback.is_file().then_some(fallback)
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    const SAMPLE: &str = r#"
[defaults]
worktree_root = "~/Git/example"
branch_prefix = "lev/"
baseline_ref = "origin/staging"
baseline_path = "~/Git/example/_baseline"
doppler_config = "dev_local"
doppler_yaml = "~/Git/example/monorepo/doppler.yaml"
[apps.api]
base_port = 9100
launch = ["nitro", "dev", "--port", "{port}"]
url_env = "FOUNDRY_API_BASE_URL"
preserve_env = ["SUPABASE_JWT_SECRET"]
static_env = { SUPABASE_JWT_SECRET = "s" }
"#;
    #[test]
    fn parses_sample() {
        let c = Config::parse(SAMPLE).unwrap();
        assert_eq!(c.apps["api"].base_port, 9100);
        assert_eq!(c.apps["api"].url_env.as_deref(), Some("FOUNDRY_API_BASE_URL"));
    }
    #[test]
    fn rejects_prd() {
        let bad = SAMPLE.replace("dev_local", "prd");
        assert!(Config::parse(&bad).is_err());
    }
}
```

- [ ] **Step 3: Run tests + commit**

Run: `cargo test -p devkit-ports config doppler`
Expected: PASS
```bash
git add crates/devkit-ports/src/config.rs crates/devkit-ports/src/doppler.rs
git commit -m "feat(ports): devkit.toml config + doppler.yaml parsing (prd denylist)"
```

### Task 6: `apps.rs` — merged catalog

**Files:**
- Modify: `crates/devkit-ports/src/apps.rs`

- [ ] **Step 1: Implement + test the merge**

```rust
// crates/devkit-ports/src/apps.rs
use crate::config::{AppConfig, Config};
use anyhow::Result;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct App {
    pub name: String,
    pub base_port: u16,
    pub doppler_project: Option<String>,
    pub path: String,
    pub launch: Vec<String>,
    pub url_env: Option<String>,
    pub preserve_env: Vec<String>,
    pub static_env: HashMap<String, String>,
}

/// Build the catalog: project+path come from doppler.yaml unless the app overrides them.
pub fn catalog(cfg: &Config, path_to_project: &HashMap<String, String>) -> Result<HashMap<String, App>> {
    let mut out = HashMap::new();
    for (name, a) in &cfg.apps {
        let path = a.path.clone().or_else(|| guess_path(name, path_to_project))
            .ok_or_else(|| anyhow::anyhow!("app `{name}`: no path in config and none inferrable from doppler.yaml"))?;
        let project = a.doppler_project.clone().or_else(|| path_to_project.get(&path).cloned());
        out.insert(name.clone(), App {
            name: name.clone(),
            base_port: a.base_port,
            doppler_project: project,
            path,
            launch: a.launch.clone(),
            url_env: a.url_env.clone(),
            preserve_env: a.preserve_env.clone(),
            static_env: a.static_env.clone(),
        });
    }
    Ok(out)
}

fn guess_path(name: &str, p2p: &HashMap<String, String>) -> Option<String> {
    let cand = format!("apps/{name}");
    p2p.contains_key(&cand).then_some(cand)
}

#[allow(dead_code)]
fn _use(_: &AppConfig) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    #[test]
    fn pulls_project_from_doppler() {
        let cfg = Config::parse(crate::config::tests_sample()).unwrap();
        let mut p2p = HashMap::new();
        p2p.insert("apps/api".to_string(), "api-foundry".to_string());
        let cat = catalog(&cfg, &p2p).unwrap();
        assert_eq!(cat["api"].doppler_project.as_deref(), Some("api-foundry"));
        assert_eq!(cat["api"].path, "apps/api");
    }
}
```

- [ ] **Step 2: Expose the sample for cross-module tests**

In `config.rs`, add (inside the file, not the test module):
```rust
#[cfg(test)]
pub fn tests_sample() -> &'static str { tests::SAMPLE }
```
and make `SAMPLE` `pub(crate)` in the test module.

- [ ] **Step 3: Run + commit**

Run: `cargo test -p devkit-ports apps`
Expected: PASS
```bash
git add crates/devkit-ports/src/apps.rs crates/devkit-ports/src/config.rs
git commit -m "feat(ports): app catalog merging config with doppler.yaml"
```

---

## Phase 3 — `devkit-ports`: the registry

### Task 7: registry types + lock + load/save

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs`

- [ ] **Step 1: Types, RAII lock, load/save**

```rust
// crates/devkit-ports/src/registry.rs
use anyhow::{Context, Result};
use devkit_common::paths;
use fd_lock::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Issue, Baseline }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub app: String,
    pub holder: String,
    pub role: Role,
    pub pid: Option<u32>,
    pub logfile: Option<PathBuf>,
    pub ts: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub entries: BTreeMap<u16, Entry>,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn read(path: &std::path::Path) -> Data {
    match fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).unwrap_or_else(|_| {
            let _ = fs::rename(path, path.with_extension("json.bak"));
            eprintln!("warning: corrupt registry; backed up and reinitialised");
            Data::default()
        }),
        _ => Data::default(),
    }
}

fn write(path: &std::path::Path, data: &Data) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(data)?)?;
    fs::rename(&tmp, path).context("atomically replacing registry")?;
    Ok(())
}

/// Run `f` while holding the exclusive registry lock; persists the mutated `Data`.
pub fn with_lock<T>(f: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
    fs::create_dir_all(paths::state_dir())?;
    let lock_path = paths::lock_file();
    let _ = OpenOptions::new().create(true).write(true).open(&lock_path)?;
    let mut lock = RwLock::new(File::open(&lock_path)?);
    let _guard = lock.write()?; // blocks until exclusive
    let reg = paths::registry_file();
    let mut data = read(&reg);
    let out = f(&mut data)?;
    write(&reg, &data)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_serde() {
        let mut d = Data::default();
        d.entries.insert(9100, Entry { app: "api".into(), holder: "/w".into(), role: Role::Issue, pid: None, logfile: None, ts: 1 });
        let s = serde_json::to_string(&d).unwrap();
        let back: Data = serde_json::from_str(&s).unwrap();
        assert_eq!(back.entries[&9100].app, "api");
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test -p devkit-ports registry::tests::roundtrip_serde`
Expected: PASS
```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "feat(ports): registry types, RAII flock, atomic load/save"
```

### Task 8: liveness helpers

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs`

- [ ] **Step 1: Add + test liveness**

```rust
// append to registry.rs
use std::net::TcpListener;

/// True if something is bound to localhost:port (we could NOT bind it).
pub fn listening(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_err()
}
pub fn pid_alive(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid as i32), None).is_ok()
}
pub fn holder_alive(holder: &str) -> bool {
    std::path::Path::new(holder).exists()
}

#[cfg(test)]
mod liveness_tests {
    use super::*;
    #[test]
    fn detects_bound_port() {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(listening(port));   // l holds it
        drop(l);
        assert!(!listening(port));  // freed
    }
    #[test]
    fn current_pid_alive() {
        assert!(pid_alive(std::process::id()));
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test -p devkit-ports liveness`
Expected: PASS
```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "feat(ports): registry liveness helpers (listening/pid/holder)"
```

### Task 9: alloc / release / prune

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs`

- [ ] **Step 1: Implement the core operations**

```rust
// append to registry.rs
pub const RESERVATION_GRACE_SECS: u64 = 120;

impl Data {
    /// Drop entries whose holder is gone, pid is dead, or are stale unbacked reservations.
    pub fn prune(&mut self) {
        let now = now();
        self.entries.retain(|port, e| {
            if !holder_alive(&e.holder) { return false; }
            match e.pid {
                Some(pid) => pid_alive(pid),
                None => listening(*port) || now.saturating_sub(e.ts) < RESERVATION_GRACE_SECS,
            }
        });
    }

    fn holds(&self, holder: &str, app: &str, role: Role) -> Option<u16> {
        self.entries.iter()
            .find(|(_, e)| e.holder == holder && e.app == app && e.role == role)
            .map(|(p, _)| *p)
    }

    /// Reserve a port for one app (idempotent per holder+app+role). pid stays None.
    pub fn alloc_one(&mut self, holder: &str, app: &str, base: u16, role: Role) -> u16 {
        if let Some(p) = self.holds(holder, app, role) { return p; }
        let mut port = base;
        while self.entries.contains_key(&port) || listening(port) {
            port += 1;
        }
        self.entries.insert(port, Entry {
            app: app.into(), holder: holder.into(), role, pid: None, logfile: None, ts: now(),
        });
        port
    }

    pub fn record_pid(&mut self, port: u16, pid: u32, logfile: PathBuf) {
        if let Some(e) = self.entries.get_mut(&port) {
            e.pid = Some(pid);
            e.logfile = Some(logfile);
        }
    }

    /// Release all entries for a holder (optionally one role). Returns freed ports.
    pub fn release(&mut self, holder: &str, role: Option<Role>) -> Vec<u16> {
        let freed: Vec<u16> = self.entries.iter()
            .filter(|(_, e)| e.holder == holder && role.map_or(true, |r| e.role == r))
            .map(|(p, _)| *p).collect();
        for p in &freed { self.entries.remove(p); }
        freed
    }
}

#[cfg(test)]
mod ops_tests {
    use super::*;
    #[test]
    fn alloc_is_idempotent_per_holder() {
        let mut d = Data::default();
        let a = d.alloc_one("/w", "api", 9100, Role::Issue);
        let b = d.alloc_one("/w", "api", 9100, Role::Issue);
        assert_eq!(a, b);
        assert_eq!(d.entries.len(), 1);
    }
    #[test]
    fn alloc_skips_claimed_ports() {
        let mut d = Data::default();
        let a = d.alloc_one("/w1", "api", 9100, Role::Issue);
        let b = d.alloc_one("/w2", "api", 9100, Role::Issue);
        assert_ne!(a, b);
    }
    #[test]
    fn prune_drops_dead_holder() {
        let mut d = Data::default();
        d.entries.insert(9100, Entry { app:"api".into(), holder:"/definitely/not/here".into(), role:Role::Issue, pid:None, logfile:None, ts: now() });
        d.prune();
        assert!(d.entries.is_empty());
    }
    #[test]
    fn release_frees_by_holder() {
        let mut d = Data::default();
        // holder must exist for nothing else to drop it; use cwd.
        let cwd = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        d.alloc_one(&cwd, "api", 9100, Role::Issue);
        let freed = d.release(&cwd, None);
        assert_eq!(freed.len(), 1);
        assert!(d.entries.is_empty());
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test -p devkit-ports ops_tests`
Expected: PASS
```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "feat(ports): registry alloc/release/prune (idempotent reservations)"
```

### Task 10: multiprocess race test

**Files:**
- Create: `crates/devkit-ports/tests/registry.rs`

- [ ] **Step 1: Write the concurrency test**

This forks N child processes that each `with_lock(|d| d.alloc_one(...))` against a shared
registry under a temp `HOME`, then asserts every returned port is distinct.

```rust
// crates/devkit-ports/tests/registry.rs
use devkit_ports::registry::{self, Role};
use std::process::Command;

// Child mode: set DEVKIT_TEST_ALLOC=<holder> and print the allocated port.
fn main_like() {}

#[test]
fn concurrent_alloc_never_collides() {
    // Use this test binary itself as the worker via an env switch.
    if let Ok(holder) = std::env::var("DEVKIT_TEST_ALLOC") {
        let port = registry::with_lock(|d| Ok(d.alloc_one(&holder, "api", 9100, Role::Issue))).unwrap();
        print!("{port}");
        std::process::exit(0);
    }

    let tmp = std::env::temp_dir().join(format!("devkit-race-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let exe = std::env::current_exe().unwrap();

    let mut kids = Vec::new();
    for i in 0..16 {
        let holder = tmp.join(format!("w{i}")); // distinct, existing holder dirs
        std::fs::create_dir_all(&holder).unwrap();
        kids.push(Command::new(&exe)
            .env("HOME", &tmp)               // registry under tmp/.claude/state/devkit
            .env("DEVKIT_TEST_ALLOC", &holder)
            .args(["--exact", "concurrent_alloc_never_collides", "--nocapture"])
            .output().unwrap());
    }
    let mut ports: Vec<String> = kids.into_iter()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    ports.sort();
    let n = ports.len();
    ports.dedup();
    assert_eq!(ports.len(), n, "ports collided: {ports:?}");
    assert_eq!(n, 16, "expected 16 allocations");
    let _ = main_like;
}
```

> Note: the test re-invokes its own binary with `--exact <name>` so the same test fn acts
> as worker (env-gated) and orchestrator. If the harness filtering proves awkward, split
> the worker into a tiny `examples/alloc_worker.rs` and invoke that instead — same logic.

- [ ] **Step 2: Run**

Run: `cargo test -p devkit-ports --test registry`
Expected: PASS (16 distinct ports)

- [ ] **Step 3: Commit**

```bash
git add crates/devkit-ports/tests/registry.rs
git commit -m "test(ports): multiprocess alloc race never double-assigns a port"
```

---

## Phase 4 — `portman` binary

### Task 11: portman CLI

**Files:**
- Modify: `crates/portman/src/main.rs`

- [ ] **Step 1: Implement the CLI**

```rust
// crates/portman/src/main.rs
use anyhow::Result;
use clap::{Parser, Subcommand};
use devkit_common::ui;
use devkit_ports::registry::{self, Data, Role};

#[derive(Parser)]
#[command(about = "Port registry for local dev servers")]
struct Cli {
    #[arg(short = 'C', long = "dir")]
    dir: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Status,
    Alloc { #[arg(long)] holder: String, #[arg(long, value_enum, default_value = "issue")] role: RoleArg, apps: Vec<String> },
    Release { #[arg(long)] holder: String, #[arg(long, value_enum)] role: Option<RoleArg> },
    Prune,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RoleArg { Issue, Baseline }
impl From<RoleArg> for Role {
    fn from(r: RoleArg) -> Self { match r { RoleArg::Issue => Role::Issue, RoleArg::Baseline => Role::Baseline } }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Status) {
        Cmd::Status => status()?,
        Cmd::Prune => { registry::with_lock(|d| { d.prune(); Ok(()) })?; println!("pruned"); }
        Cmd::Release { holder, role } => {
            let freed = registry::with_lock(|d| Ok(d.release(&holder, role.map(Into::into))))?;
            println!("released: {freed:?}");
        }
        Cmd::Alloc { holder, role, apps } => {
            // NOTE: portman alloc needs base ports; it loads the catalog (see Task 14 for shared loader).
            anyhow::bail!("`portman alloc` requires the catalog loader from Task 14 wiring: {holder} {:?} {apps:?}", Role::from(role));
        }
    }
    Ok(())
}

fn status() -> Result<()> {
    let data: Data = registry::with_lock(|d| { d.prune(); Ok(std::mem::take(d)) })?;
    // prune mutated+persisted; we took a copy to render then must put it back:
    registry::with_lock(|d| { *d = serde_json::from_str(&serde_json::to_string(&data)?)?; Ok(()) })?;
    let mut t = ui::table(&["PORT", "APP", "ROLE", "HOLDER", "PID", "LISTENING", "AGE"]);
    for (port, e) in &data.entries {
        let id = holder_label(&e.holder);
        t.add_row(vec![
            port.to_string(), e.app.clone(),
            format!("{:?}", e.role).to_lowercase(), id,
            e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            if registry::listening(*port) { "yes".into() } else { "no".into() },
            format!("{}s", registry::now().saturating_sub(e.ts)),
        ]);
    }
    println!("{t}");
    Ok(())
}

fn holder_label(h: &str) -> String {
    std::path::Path::new(h).file_name().and_then(|s| s.to_str()).unwrap_or(h).to_string()
}
```

> The `status()` take/put dance is ugly; in implementation prefer adding a
> `registry::snapshot()` helper that reads under the lock, prunes, persists, and returns a
> clone — replace the two `with_lock` calls with one. (Add `pub fn snapshot() -> Result<Data>`
> to `registry.rs` and a `Clone` derive on `Data`/`Entry`.)

- [ ] **Step 2: Add `snapshot()` to registry.rs and derive Clone**

```rust
// registry.rs — derive Clone on Data and Entry, then:
pub fn snapshot() -> Result<Data> {
    with_lock(|d| { d.prune(); Ok(d.clone()) })
}
```
Replace the `status()` body's first two lines with `let data = registry::snapshot()?;`.

- [ ] **Step 3: Build + smoke test**

Run: `cargo run -p portman -- status`
Expected: prints an empty table (no panics).

- [ ] **Step 4: Commit**

```bash
git add crates/portman/src/main.rs crates/devkit-ports/src/registry.rs
git commit -m "feat(portman): status/release/prune CLI over the registry"
```

> `portman alloc` is completed in Task 14 once the shared catalog loader exists (it needs
> base ports). Until then it returns the explicit error above.

---

## Phase 5 — shared loader + `devrun`

### Task 12: shared catalog/config loader (used by devrun, portman alloc, issue-prep)

**Files:**
- Create: `crates/devkit-ports/src/load.rs`; Modify: `crates/devkit-ports/src/lib.rs`

- [ ] **Step 1: One entry point that resolves config → catalog**

```rust
// crates/devkit-ports/src/load.rs
use crate::{apps::{self, App}, config::{self, Config}, doppler};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

pub struct Loaded {
    pub config: Config,
    pub catalog: HashMap<String, App>,
}

pub fn load(explicit: Option<&Path>, start: &Path) -> Result<Loaded> {
    let cfg_path = config::locate(explicit, start)
        .context("no devkit.toml found (--config / $DEVKIT_CONFIG / ./devkit.toml / ~/.config/devkit/config.toml)")?;
    let cfg = Config::parse(&std::fs::read_to_string(&cfg_path)?)?;
    let yaml_path = config::expand_tilde(&cfg.defaults.doppler_yaml);
    let p2p = match std::fs::read_to_string(&yaml_path) {
        Ok(y) => doppler::path_to_project(&y)?,
        Err(_) => HashMap::new(), // doppler.yaml optional; apps then need explicit path/project
    };
    let catalog = apps::catalog(&cfg, &p2p)?;
    Ok(Loaded { config: cfg, catalog })
}
```
Add `pub mod load;` to `lib.rs`.

- [ ] **Step 2: Wire `portman alloc`**

Replace the `Cmd::Alloc` arm in `portman/src/main.rs`:
```rust
Cmd::Alloc { holder, role, apps } => {
    let loaded = devkit_ports::load::load(None, std::path::Path::new("."))?;
    let role: Role = role.into();
    let mut out = Vec::new();
    registry::with_lock(|d| {
        d.prune();
        for app in &apps {
            let base = loaded.catalog.get(app)
                .ok_or_else(|| anyhow::anyhow!("unknown app `{app}`"))?.base_port;
            out.push((app.clone(), d.alloc_one(&holder, app, base, role)));
        }
        Ok(())
    })?;
    for (app, port) in out { println!("{app}={port}"); }
}
```
Add `devkit-ports` as a dep of `portman` (already present per Task 0).

- [ ] **Step 3: Build + commit**

Run: `cargo build -p portman && cargo test -p devkit-ports`
Expected: PASS
```bash
git add crates/devkit-ports/src/load.rs crates/devkit-ports/src/lib.rs crates/portman/src/main.rs
git commit -m "feat(ports): shared config→catalog loader; wire portman alloc"
```

### Task 13: `devrun` env assembly (`env.rs`) + dry-run

**Files:**
- Create: `crates/devrun/src/env.rs`

- [ ] **Step 1: Pure env-layering with a test**

```rust
// crates/devrun/src/env.rs
use devkit_ports::apps::App;
use std::collections::BTreeMap;

/// Build the doppler argv prefix: `doppler run -p <project> -c <config> [--preserve-env=K]... --`
pub fn doppler_prefix(app: &App, config: &str) -> Vec<String> {
    let mut v = vec!["doppler".into(), "run".into()];
    if let Some(p) = &app.doppler_project { v.push("-p".into()); v.push(p.clone()); }
    v.push("-c".into()); v.push(config.into());
    for k in &app.preserve_env { v.push(format!("--preserve-env={k}")); }
    v.push("--".into());
    v
}

/// Resolve `{port}` in the launch argv.
pub fn launch_argv(app: &App, port: u16) -> Vec<String> {
    app.launch.iter().map(|a| a.replace("{port}", &port.to_string())).collect()
}

/// Env layering (low→high): static_env → url-wiring → user overrides.
/// `api_port` is this role's api port, if api is in the same run.
pub fn env_for(
    app: &App, api_port: Option<u16>, user: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (k, v) in &app.static_env { env.insert(k.clone(), v.clone()); }
    if let (Some(var), Some(p)) = (api_url_consumer(app), api_port) {
        env.insert(var, format!("http://localhost:{p}"));
    }
    for (k, v) in user { env.insert(k.clone(), v.clone()); }
    env
}

/// The env var THIS app reads to reach the api (its own url_env is for OTHERS;
/// a consumer's wiring var is configured the same — we set it when api shares the run).
fn api_url_consumer(app: &App) -> Option<String> {
    // Convention: a webapp's url_env names the var it *also consumes* to reach the api.
    // For api itself url_env is set but it doesn't consume itself, so skip name == api.
    if app.name == "api" { None } else { app.url_env.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    fn app(name: &str, url_env: Option<&str>) -> App {
        App { name: name.into(), base_port: 1, doppler_project: Some("proj".into()),
            path: "apps/x".into(), launch: vec!["next".into(),"dev".into(),"-p".into(),"{port}".into()],
            url_env: url_env.map(Into::into), preserve_env: vec![], static_env: HashMap::new() }
    }
    #[test]
    fn wires_api_url_for_consumer() {
        let e = env_for(&app("lab-os", Some("FOUNDRY_API_BASE_URL")), Some(9103), &BTreeMap::new());
        assert_eq!(e["FOUNDRY_API_BASE_URL"], "http://localhost:9103");
    }
    #[test]
    fn user_override_wins() {
        let mut u = BTreeMap::new();
        u.insert("FOUNDRY_API_BASE_URL".into(), "http://x".into());
        let e = env_for(&app("lab-os", Some("FOUNDRY_API_BASE_URL")), Some(9103), &u);
        assert_eq!(e["FOUNDRY_API_BASE_URL"], "http://x");
    }
    #[test]
    fn launch_substitutes_port() {
        assert_eq!(launch_argv(&app("lab-os", None), 4103), vec!["next","dev","-p","4103"]);
    }
}
```

- [ ] **Step 2: Run + commit**

Run: `cargo test -p devrun env`
(Add `mod env;` to `devrun/src/main.rs` and a placeholder `fn main(){}` if needed to compile.)
Expected: PASS
```bash
git add crates/devrun/src/env.rs crates/devrun/src/main.rs
git commit -m "feat(devrun): doppler prefix + env layering + api-url wiring"
```

### Task 14: `devrun` supervise (`supervise.rs`) — spawn/readiness/down

**Files:**
- Create: `crates/devrun/src/supervise.rs`

- [ ] **Step 1: Detached spawn + readiness + signal**

```rust
// crates/devrun/src/supervise.rs
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Spawn `argv` detached (own session), env-augmented, stdout+stderr → logfile.
/// Returns the child pid.
pub fn spawn_detached(
    argv: &[String], cwd: &str, env: &BTreeMap<String, String>, logfile: &PathBuf,
) -> Result<u32> {
    use std::os::unix::process::CommandExt;
    fs::create_dir_all(logfile.parent().unwrap())?;
    let out = File::create(logfile)?;
    let err = out.try_clone()?;
    let (prog, rest) = argv.split_first().context("empty launch argv")?;
    let mut c = Command::new(prog);
    c.args(rest).current_dir(cwd).envs(env)
        .stdin(Stdio::null()).stdout(out).stderr(err);
    unsafe { c.pre_exec(|| { nix::unistd::setsid().map(|_| ()).map_err(|e| e.into()) }); }
    let child = c.spawn().with_context(|| format!("spawning {prog}"))?;
    Ok(child.id())
}

/// Poll localhost:port until it accepts a TCP connection or times out.
pub fn wait_ready(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if TcpStream::connect_timeout(&(("127.0.0.1", port).into()), Duration::from_millis(500)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// SIGTERM a pid (ignore if already gone).
pub fn stop(pid: u32) {
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

pub fn tail(logfile: &PathBuf, lines: usize) -> String {
    let body = fs::read_to_string(logfile).unwrap_or_default();
    body.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
}
```

- [ ] **Step 2: Test the testable parts**

```rust
// append to supervise.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spawn_and_ready_on_python_http() {
        let tmp = std::env::temp_dir().join(format!("devrun-{}.log", std::process::id()));
        // pick a free port
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let argv: Vec<String> = ["python3","-m","http.server",&port.to_string()].iter().map(|s| s.to_string()).collect();
        let env = BTreeMap::new();
        let pid = spawn_detached(&argv, ".", &env, &tmp).unwrap();
        assert!(wait_ready(port, Duration::from_secs(10)), "server never came up");
        stop(pid);
        let _ = fs::remove_file(&tmp);
    }
}
```

- [ ] **Step 3: Run + commit**

Run: `cargo test -p devrun supervise`
Expected: PASS (spawns a throwaway `python3 -m http.server`, confirms readiness, kills it)
```bash
git add crates/devrun/src/supervise.rs
git commit -m "feat(devrun): detached spawn, readiness poll, SIGTERM, log tail"
```

### Task 15: `devrun` CLI — `up`/`down`/`status`/`logs` (+ `--dry-run`)

**Files:**
- Modify: `crates/devrun/src/main.rs`

- [ ] **Step 1: Wire the subcommands**

Implement `main.rs` with clap subcommands. `up`:
1. resolve holder via `devkit_common::worktree::discover(cwd)` → issue worktree path.
2. resolve apps: positional args, else parse `git diff origin/staging...HEAD --stat` for
   `apps/<name>/` prefixes via the catalog; if a webapp with `url_env` is selected, ensure
   `api` is added.
3. parse `--env K=V` (repeatable) + `--env-file` into the user override map.
4. `registry::with_lock`: prune, `alloc_one` each app → ports map. Determine `api_port`.
5. for each app build argv = `doppler_prefix(app,cfg) ++ launch_argv(app,port)`, env via
   `env_for`, logfile = `paths::logs_dir()/<holder-slug>/<role>-<app>.log`.
6. if `--dry-run`: print app, port, cwd, full argv, env keys; spawn nothing; return.
7. else `spawn_detached`, `record_pid` (second `with_lock`), `wait_ready` (120s); collect
   PASS/FAIL; on FAIL print `tail(log, 30)`.
8. print a summary table (app, role, port, `http://localhost:port`, pid, log path).

`down`: `discover` holder → for each tracked entry of this holder(/role) `stop(pid)`, then
`registry::with_lock(|d| d.release(holder, role))`. `status`: delegate to `portman status`
logic (call `registry::snapshot()` and render, filtered to this holder unless `--all`).
`logs <app>`: find entry by holder+app(+role), `tail` or exec `tail -f` when `-f`.

```rust
// signature sketch for the resolver, with a test below
pub fn apps_from_diff(diff_stat: &str, known: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for line in diff_stat.lines() {
        if let Some(rest) = line.trim().strip_prefix("apps/") {
            if let Some(name) = rest.split('/').next() {
                if known.iter().any(|k| k == name) && !found.contains(&name.to_string()) {
                    found.push(name.to_string());
                }
            }
        }
    }
    found
}
```

- [ ] **Step 2: Test the diff resolver**

```rust
#[cfg(test)]
mod tests {
    use super::apps_from_diff;
    #[test]
    fn picks_known_apps_from_diff() {
        let diff = " apps/api/server/x.ts | 2 +-\n apps/lab-os/page.tsx | 1 +\n packages/z/y.ts | 1 +\n";
        let known = vec!["api".to_string(), "lab-os".to_string(), "foundry-portal".to_string()];
        assert_eq!(apps_from_diff(diff, &known), vec!["api", "lab-os"]);
    }
}
```

- [ ] **Step 3: Dry-run smoke test**

Run (from an example worktree, once `configs/example.toml` is installed — see Task 18):
`cargo run -p devrun -- up api lab-os --dry-run`
Expected: prints resolved ports + `doppler run -p api-foundry -c dev_local …` argv + the
`FOUNDRY_API_BASE_URL=http://localhost:<api_port>` wiring; spawns nothing.

- [ ] **Step 4: Commit**

```bash
git add crates/devrun/src/main.rs
git commit -m "feat(devrun): up/down/status/logs with dry-run and app auto-resolution"
```

### Task 16: `devrun` baseline (`baseline.rs`) + `--role both`

**Files:**
- Create: `crates/devrun/src/baseline.rs`

- [ ] **Step 1: Ensure-fresh baseline worktree (guarded reset)**

```rust
// crates/devrun/src/baseline.rs
use anyhow::{Result, bail};
use devkit_common::cmd::git;
use std::path::Path;

/// Ensure `path` is a worktree at a fresh `ref` (e.g. origin/staging).
/// Creates it if missing; otherwise fetches and hard-resets — but refuses if the
/// tree is dirty (anything other than a clean checkout), so no real work is lost.
pub fn ensure_fresh(main_repo: &str, path: &str, git_ref: &str) -> Result<()> {
    let (remote, _) = git_ref.split_once('/').unwrap_or(("origin", git_ref));
    if !Path::new(path).exists() {
        git(&["fetch", remote], main_repo)?;
        git(&["worktree", "add", "--detach", path, git_ref], main_repo)?;
        return Ok(());
    }
    let dirty = !git(&["status", "--porcelain"], path)?.trim().is_empty();
    if dirty {
        bail!("baseline worktree {path} is dirty — refusing to reset --hard. Clean it or remove it.");
    }
    git(&["fetch", remote], path)?;
    git(&["reset", "--hard", git_ref], path)?;
    Ok(())
}
```

- [ ] **Step 2: Wire `--role both` in `up`**

When role is `both`: first `ensure_fresh(main, cfg.baseline_path, cfg.baseline_ref)`, then
run the same app set twice — once with holder = issue worktree + `Role::Issue`, once with
holder = baseline path + `Role::Baseline` (cwd = `<baseline>/apps/<app>`). Two port sets;
summary table groups by role.

- [ ] **Step 3: Test the guard (no network)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_dirty_baseline() {
        // init a repo, dirty it, assert ensure_fresh bails on the dirty branch.
        let tmp = std::env::temp_dir().join(format!("bl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.to_str().unwrap();
        git(&["init", "-q"], p).unwrap();
        std::fs::write(tmp.join("f"), "x").unwrap();
        // dirty (untracked) tree → guard trips
        let err = ensure_fresh(p, p, "origin/staging").unwrap_err();
        assert!(err.to_string().contains("dirty"));
    }
}
```

- [ ] **Step 4: Run + commit**

Run: `cargo test -p devrun baseline`
Expected: PASS
```bash
git add crates/devrun/src/baseline.rs crates/devrun/src/main.rs
git commit -m "feat(devrun): baseline worktree A/B with guarded hard-reset"
```

---

## Phase 6 — `issue-prep`, `issue-end`, `pr-status`

### Task 17: `issue-prep`

**Files:**
- Modify: `crates/issue-prep/src/main.rs`

- [ ] **Step 1: Implement**

clap args `--issue ENG-1234 --slug eng-1234-… [--apps a,b] [--dry-run]`. Steps:
1. derive `branch = format!("{}{}", cfg.defaults.branch_prefix, slug)`,
   `worktree = expand_tilde(&cfg.defaults.worktree_root).join(slug)`,
   `monorepo = expand_tilde(&cfg.defaults.worktree_root).join("monorepo")`.
2. `git fetch origin` (monorepo); `git worktree add -b <branch> <worktree> <baseline_ref>`.
   If branch exists → bail with a clear message (let `/issue-setup` decide).
3. for each app: `ln -s <worktree_root>/.env.local <worktree>/apps/<app>/.env` (skip if
   exists); if app == lab-os, write `apps/lab-os/.env.local` with
   `WORKCELL_BLI_RUN_WORKFLOW_ID=dummy`.
4. `bun install` once in `<worktree>/apps/<first app>`.
5. `registry::with_lock`: `alloc_one` each app at `Role::Issue` → ports.
6. print JSON: `{ "worktree":…, "branch":…, "ports": { "api":9103, … } }`.
   `--dry-run` prints the plan and the would-be ports without creating anything.

```rust
// the printable result type
#[derive(serde::Serialize)]
struct Prepared<'a> {
    worktree: String,
    branch: String,
    ports: std::collections::BTreeMap<&'a str, u16>,
}
```

- [ ] **Step 2: Smoke test (dry-run)**

Run: `cargo run -p issue-prep -- --issue ENG-9999 --slug eng-9999-demo --apps api --dry-run`
Expected: prints the JSON plan, creates no worktree.

- [ ] **Step 3: Commit**

```bash
git add crates/issue-prep/src/main.rs
git commit -m "feat(issue-prep): mechanical worktree+env+port reservation, JSON output"
```

### Task 18: `configs/example.toml` + install + README

**Files:**
- Create: `configs/example.toml`, `README.md`

- [ ] **Step 1: Write the example config**

```toml
[defaults]
worktree_root  = "~/Git/example"
branch_prefix  = "lev/"
baseline_ref   = "origin/staging"
baseline_path  = "~/Git/example/_baseline"
doppler_config = "dev_local"
doppler_yaml   = "~/Git/example/monorepo/doppler.yaml"

[apps.api]
base_port    = 9100
launch       = ["nitro", "dev", "--port", "{port}"]
url_env      = "FOUNDRY_API_BASE_URL"
preserve_env = ["SUPABASE_JWT_SECRET"]
static_env   = { SUPABASE_JWT_SECRET = "super-secret-jwt-token-with-at-least-32-characters-long" }

[apps.lab-os]
base_port  = 4100
launch     = ["next", "dev", "-p", "{port}"]
url_env    = "FOUNDRY_API_BASE_URL"
static_env = { WORKCELL_BLI_RUN_WORKFLOW_ID = "dummy" }

[apps.foundry-portal]
base_port = 4200
launch    = ["next", "dev", "-p", "{port}"]
url_env   = "FOUNDRY_API_BASE_URL"

[apps.website]
base_port = 4300
launch    = ["next", "dev", "--port", "{port}"]
url_env   = "FOUNDRY_API_BASE_URL"

[apps.plate-api]
base_port = 8080
path      = "apps/plate_tools/plate_api"
launch    = ["uv", "run", "uvicorn", "server.main:create_app", "--factory", "--reload", "--port", "{port}"]
```

> `website`/`plate-api` `launch` deliberately run the framework binary under our `dev_local`
> doppler — **not** their package.json `dev` scripts (website's uses `doppler -c prd`).

- [ ] **Step 2: Install config + verify a real dry-run**

Run:
```bash
mkdir -p ~/.config/devkit && cp configs/example.toml ~/.config/devkit/config.toml
cargo run -p devrun -- -C ~/Git/example/monorepo up api lab-os --dry-run
```
Expected: ports allocated, doppler argv shown, `FOUNDRY_API_BASE_URL` wired to the api port.

- [ ] **Step 3: README + install instructions, commit**

README documents `cargo install --path crates/<bin>` for each tool (or `cargo install --path crates/portman ...`), config discovery, and the five commands.
```bash
git add configs/example.toml README.md ~/.config/devkit/config.toml 2>/dev/null || git add configs/example.toml README.md
git commit -m "feat: example config, README, install instructions"
```

### Task 19: `issue-end` (rewrite)

**Files:**
- Modify: `crates/issue-end/src/main.rs`

- [ ] **Step 1: Port the Python behavior onto `devkit-common`**

Subcommands `status` (default) / `clean [ids…]` / `clean --clean-worktree sel…`.
1. `discover(cwd)` → worktrees; `issue_id_of` each.
2. one `gh pr list --state all --limit 500 --json number,state,url,headRefName` (cwd =
   main repo) via `cmd::gh_json`; pick best PR per head (MERGED>OPEN>CLOSED).
3. `linear::states(ids, env LINEAR_API_KEY)` for the Done gate.
4. `dirty = !git status --porcelain`.
5. verdict: finished = PR MERGED + Linear completed + clean. Render with `ui::table` and
   OSC8 links (`ui::link`).
6. `clean`: for each finished worktree, confirm (unless `-y`), then **reimplement cleanup
   in Rust**: refuse if cwd is inside the target; `git worktree remove [--force]`,
   `git worktree prune`, `git branch -D <branch>`, `rm <parent>/ISSUE_*<id>*.md`.
   `--clean-worktree sel…` bypasses the finished gate, matching selectors against issue id /
   branch / dir / path (port `select_explicit`).

Port the pure pieces with tests:
```rust
pub fn best_pr<'a>(prs: &'a [Pr], head: &str) -> Option<&'a Pr> { /* rank MERGED>OPEN>CLOSED */ }
#[cfg(test)]
mod tests { /* assert MERGED chosen over OPEN for same head */ }
```

- [ ] **Step 2: Build, run read-only `status`, commit**

Run: `cargo run -p issue-end -- status -C ~/Git/example/monorepo`
Expected: table matching the Python tool's columns (ISSUE/BRANCH/TREE/PR/LINEAR/VERDICT).
```bash
git add crates/issue-end/src/main.rs
git commit -m "feat(issue-end): Rust rewrite (gh + Linear gate + Rust cleanup)"
```

### Task 20: `pr-status` (rewrite)

**Files:**
- Modify: `crates/pr-status/src/main.rs`

- [ ] **Step 1: Port the two-table triage + diff cache**

flags `-m/--mine`, `-r/--reviews`, `-R owner/repo`, `--no-cache`. Use `cmd::gh_json` for
authored + review-requested PRs, compute REVIEW/CHECK/ACTION per PR, render with `ui::table`
+ `ui::link`. Diff cache: read prior snapshot from `paths::cache_dir()/pr-status/<repo>.json`,
render changed cells as `before → after`, write the new snapshot (unless `--no-cache`).
Port the pure ACTION-derivation logic with unit tests (e.g. "approved + checks green →
ready to merge").

- [ ] **Step 2: Build, run, commit**

Run: `cargo run -p pr-status`
Expected: the two tables render against the current repo.
```bash
git add crates/pr-status/src/main.rs
git commit -m "feat(pr-status): Rust rewrite with before→after diff cache"
```

---

## Phase 7 — command integration

### Task 21: Rewrite `/validate-webapp` and `/issue-setup` to call the binaries

**Files:**
- Modify: `~/.claude/commands/validate-webapp.md`, `~/.claude/commands/issue-setup.md`

- [ ] **Step 1: `validate-webapp.md`** — replace the manual port table + `ss` free-port loop
  (steps 2–4) with: run `devrun up <apps> --role issue` (or `--role both` when an A/B is
  wanted), read back the printed ports/URLs, then proceed to the chrome-devtools validation
  unchanged. Keep the "leave servers running" note; mention `devrun down` to tear down.

- [ ] **Step 2: `issue-setup.md`** — replace step 7 (port-slot scanning) and the mechanical
  parts of steps 4–6 with a single `issue-prep --issue <ID> --slug <slug> --apps <…>` call,
  consuming its JSON for the summary's Ports section. Keep steps 1/2/8 (Linear/Sentry/summary
  prose) as Claude-driven MCP work. Note that the registry — not the `Port slot:` marker — is
  now the source of truth.

- [ ] **Step 3: Commit (in the ~/.claude repo)**

```bash
cd ~/.claude && git add commands/validate-webapp.md commands/issue-setup.md
git commit -m "docs(commands): drive validate-webapp/issue-setup via devkit binaries"
```

---

## Self-review notes

- **Spec coverage:** registry (T7–10), portman (T11–12,14), baseline (T16), supervised
  up/down/status/logs (T14–15), issue-prep (T17), issue-end/pr-status rewrite (T19–20),
  config-driven catalog + doppler merge (T5–6,12), prd denylist (T5, T18 note), command
  rewrites (T21). All spec sections map to a task.
- **No-script-deletion:** honored — Task 21 rewrites commands but no task deletes the old
  `~/.local/bin`/`~/.claude/scripts` files (user does cleanup).
- **api-URL wiring** uses the verified `FOUNDRY_API_BASE_URL` (T13, T18).
- **Type consistency:** `Role`, `Data`, `Entry`, `App`, `alloc_one`, `with_lock`,
  `snapshot`, `env_for`, `ensure_fresh` are used with consistent signatures across tasks.

## Open follow-ups (non-blocking)

- `plate-api`/`website` launch argvs are best-effort from package.json; verify on first real
  `devrun up` of each (their `--dry-run` plan will show the exact command to sanity-check).
- `cargo install` of all five bins at once: document the exact invocation in the README once
  confirmed (`cargo install --path crates/portman` ... per bin, or a small install script).
