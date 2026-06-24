# Config Templating Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the five issue-lifecycle strings (branch, worktree dir, PR title, PR body, Slack message) config-driven minijinja templates with built-in defaults that reproduce today's output.

**Architecture:** A `template::render` wrapper over minijinja (strict undefined) in `devkit-common`; a `Templates` config struct in `devkit-ports` with per-key defaults; `issue setup` renders branch/worktree-dir and persists a `.devkit/issue.toml` record (and ensures `.devkit/` is globally gitignored); `issue review` reads that record and renders PR/Slack templates, threading each render's output into the next context.

**Tech Stack:** Rust 2024, minijinja 2 (`builtins` only, `serde` already in-tree), serde_json, toml 0.8, anyhow.

**Spec:** `docs/superpowers/specs/2026-06-24-config-templating-design.md`

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/devkit-common/src/template.rs` (Create) | `render(template, ctx, variables)` — minijinja strict-undefined render with `variables` merged underneath the context |
| `crates/devkit-common/src/lib.rs` (Modify) | register `pub mod template;` |
| `crates/devkit-common/Cargo.toml` (Modify) | add `minijinja` dep |
| `Cargo.toml` (Modify) | add `minijinja` to `[workspace.dependencies]` |
| `crates/devkit-ports/src/config.rs` (Modify) | `Templates` struct, `DEFAULT_*` consts + accessors, `templates` field on `Config` |
| `src/bin/issue/record.rs` (Create) | `IssueRecord` + `write`/`read` of `<worktree>/.devkit/issue.toml` |
| `src/bin/issue/gitignore.rs` (Create) | resolve global excludes path + `ensure_devkit_ignored` (fail-open at caller) |
| `src/bin/issue/setup.rs` (Modify) | render branch/worktree-dir, write record, ensure gitignore |
| `src/bin/issue/review.rs` (Modify) | render PR title/body/Slack, thread context, read record |
| `src/bin/issue/main.rs` (Modify) | register new mods; `--no-gitignore` on Setup; `body` optional on Review; wire args |
| `README.md` (Modify) | document `[templates]`, the record, the global-gitignore behavior |
| `docs/next-steps.md` (Modify) | mark items 2 & 3 RESOLVED |

---

## Task 1: `template::render` in devkit-common

**Files:**
- Modify: `Cargo.toml` (workspace deps), `crates/devkit-common/Cargo.toml`
- Create: `crates/devkit-common/src/template.rs`
- Modify: `crates/devkit-common/src/lib.rs`

- [ ] **Step 1: Add the minijinja dependency**

In `Cargo.toml`, add to `[workspace.dependencies]` (after the `textplots` line, line 58):

```toml
minijinja = { version = "2", default-features = false, features = ["builtins"] }
```

In `crates/devkit-common/Cargo.toml`, add under `[dependencies]` (after `ureq.workspace = true`):

```toml
minijinja.workspace = true
```

`default-features = false` drops minijinja's debug/loader extras; `builtins` keeps the standard filter library (`|default`, `|upper`, …). Control structures (`{% if %}`, `{% for %}`) are core syntax and remain available. `serde`/`serde_json` are already in `devkit-common`.

- [ ] **Step 2: Write the failing test file**

Create `crates/devkit-common/src/template.rs` with only the tests first:

```rust
use anyhow::{Context, Result};
use minijinja::{Environment, UndefinedBehavior};
use serde::Serialize;
use std::collections::BTreeMap;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn substitutes_named_fields() {
        let out = render("{{ a }}-{{ b }}", &json!({"a": "x", "b": "y"}), &novars()).unwrap();
        assert_eq!(out, "x-y");
    }

    #[test]
    fn supports_if_and_for() {
        let tmpl = "{% for a in apps %}{{ a }},{% endfor %}{% if flag %}!{% endif %}";
        let out = render(tmpl, &json!({"apps": ["w", "i"], "flag": true}), &novars()).unwrap();
        assert_eq!(out, "w,i,!");
    }

    #[test]
    fn strict_undefined_is_an_error() {
        assert!(render("{{ missing }}", &json!({}), &novars()).is_err());
    }

    #[test]
    fn variables_fill_unset_fields() {
        let mut vars = BTreeMap::new();
        vars.insert("team".to_string(), "platform".to_string());
        let out = render("{{ team }}", &json!({}), &vars).unwrap();
        assert_eq!(out, "platform");
    }

    #[test]
    fn context_wins_over_variable() {
        let mut vars = BTreeMap::new();
        vars.insert("slug".to_string(), "from-const".to_string());
        let out = render("{{ slug }}", &json!({"slug": "from-ctx"}), &vars).unwrap();
        assert_eq!(out, "from-ctx");
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p devkit-common template`
Expected: FAIL — `render` not found / module not declared.

- [ ] **Step 4: Implement `render`**

Prepend to `crates/devkit-common/src/template.rs` (above the `#[cfg(test)]` block):

```rust
/// Render a minijinja template with strict undefined handling. `variables`
/// supply constants merged underneath `ctx` — a context field of the same name
/// wins. `ctx` must serialize to a JSON object.
pub fn render(
    template: &str,
    ctx: &impl Serialize,
    variables: &BTreeMap<String, String>,
) -> Result<String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.add_template("t", template)
        .context("compiling template")?;
    let tmpl = env.get_template("t").expect("template just added");
    let value = merged_context(ctx, variables)?;
    tmpl.render(value).context("rendering template")
}

fn merged_context(
    ctx: &impl Serialize,
    variables: &BTreeMap<String, String>,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(ctx).context("serializing template context")?;
    if let Some(obj) = value.as_object_mut() {
        for (k, v) in variables {
            obj.entry(k.clone())
                .or_insert_with(|| serde_json::Value::String(v.clone()));
        }
    }
    Ok(value)
}
```

- [ ] **Step 5: Register the module**

In `crates/devkit-common/src/lib.rs`, add in alphabetical position (after `pub mod supervise;` / before `pub mod sys;` — keep the existing ordering, insert `pub mod template;` after `pub mod supervise;`):

```rust
pub mod template;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit-common template`
Expected: PASS — 5 tests.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/devkit-common/Cargo.toml crates/devkit-common/src/template.rs crates/devkit-common/src/lib.rs
git commit -m "feat(common): add minijinja template render helper"
```

---

## Task 2: `Templates` config struct + defaults

**Files:**
- Modify: `crates/devkit-ports/src/config.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/devkit-ports/src/config.rs` (inside `mod tests { ... }`, after the existing tests):

```rust
#[test]
fn templates_default_when_absent() {
    let t: Templates = toml::from_str("").unwrap();
    assert!(t.branch.is_none());
    assert!(t.variables.is_empty());
    assert_eq!(t.branch(), DEFAULT_BRANCH);
    assert_eq!(t.worktree_dir(), DEFAULT_WORKTREE_DIR);
    assert_eq!(t.pr_title(), DEFAULT_PR_TITLE);
    assert_eq!(t.pr_body(), DEFAULT_PR_BODY);
    assert_eq!(t.slack(), DEFAULT_SLACK);
}

#[test]
fn templates_partial_override() {
    let t: Templates = toml::from_str("branch = \"{{ slug }}\"\n").unwrap();
    assert_eq!(t.branch(), "{{ slug }}");
    assert_eq!(t.worktree_dir(), DEFAULT_WORKTREE_DIR);
}

#[test]
fn templates_variables_parse() {
    let t: Templates = toml::from_str("[variables]\nteam = \"platform\"\n").unwrap();
    assert_eq!(t.variables.get("team").map(String::as_str), Some("platform"));
}

#[test]
fn config_has_default_templates() {
    let cfg = Config::parse(tests_sample()).unwrap();
    assert_eq!(cfg.templates.branch(), DEFAULT_BRANCH);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-ports templates`
Expected: FAIL — `Templates`, `DEFAULT_BRANCH`, `cfg.templates` not found.

- [ ] **Step 3: Add the `Templates` struct, consts, accessors**

In `crates/devkit-ports/src/config.rs`, add after the `Person` struct (after line 99, before `AppConfig`):

```rust
pub const DEFAULT_BRANCH: &str = "{{ prefix }}{{ slug }}";
pub const DEFAULT_WORKTREE_DIR: &str = "{{ slug }}";
pub const DEFAULT_PR_TITLE: &str = "{{ input }}";
pub const DEFAULT_PR_BODY: &str = "{{ input }}";
pub const DEFAULT_SLACK: &str = "{{ input }} {{ pr_url }}";

/// Config-driven minijinja templates for the issue-lifecycle strings. Each
/// `None` field falls back to its `DEFAULT_*` constant, which reproduces the
/// historical hardcoded output. `variables` are user constants merged under
/// every render context.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct Templates {
    pub branch: Option<String>,
    pub worktree_dir: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub slack: Option<String>,
    #[serde(default)]
    pub variables: std::collections::BTreeMap<String, String>,
}

impl Templates {
    pub fn branch(&self) -> &str {
        self.branch.as_deref().unwrap_or(DEFAULT_BRANCH)
    }
    pub fn worktree_dir(&self) -> &str {
        self.worktree_dir.as_deref().unwrap_or(DEFAULT_WORKTREE_DIR)
    }
    pub fn pr_title(&self) -> &str {
        self.pr_title.as_deref().unwrap_or(DEFAULT_PR_TITLE)
    }
    pub fn pr_body(&self) -> &str {
        self.pr_body.as_deref().unwrap_or(DEFAULT_PR_BODY)
    }
    pub fn slack(&self) -> &str {
        self.slack.as_deref().unwrap_or(DEFAULT_SLACK)
    }
}
```

- [ ] **Step 4: Add the `templates` field to `Config`**

In the `Config` struct (line 6-14), add the field after `daemon`:

```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub defaults: Defaults,
    pub apps: HashMap<String, AppConfig>,
    #[serde(default)]
    pub people: HashMap<String, Person>,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub templates: Templates,
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports templates`
Expected: PASS — 4 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-ports/src/config.rs
git commit -m "feat(ports): add Templates config struct with defaults"
```

---

## Task 3: Issue setup record

**Files:**
- Create: `src/bin/issue/record.rs`
- Modify: `src/bin/issue/main.rs` (register `mod record;`)

- [ ] **Step 1: Write the failing test file**

Create `src/bin/issue/record.rs`:

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Per-worktree record written by `issue setup` and read by `issue review`,
/// carrying the setup-time context that is otherwise unavailable at review.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueRecord {
    pub issue: String,
    pub slug: String,
    pub apps: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let dir = std::env::temp_dir().join(format!("devkit-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rec = IssueRecord {
            issue: "ABC-123".into(),
            slug: "fix-login".into(),
            apps: vec!["web".into(), "api".into()],
        };
        write(&dir, &rec).unwrap();
        assert_eq!(read(&dir), Some(rec));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_missing_is_none() {
        let dir = std::env::temp_dir().join("devkit-rec-does-not-exist-xyz");
        assert_eq!(read(&dir), None);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin issue record`
Expected: FAIL — `write`/`read` not found.

- [ ] **Step 3: Implement `write` and `read`**

Add to `src/bin/issue/record.rs` (above the `#[cfg(test)]` block):

```rust
/// `<worktree>/.devkit/issue.toml`.
fn path(worktree: &Path) -> std::path::PathBuf {
    worktree.join(".devkit").join("issue.toml")
}

/// Write the record under `<worktree>/.devkit/`, creating the directory.
pub fn write(worktree: &Path, rec: &IssueRecord) -> Result<()> {
    let p = path(worktree);
    std::fs::create_dir_all(p.parent().expect("path has a parent"))
        .with_context(|| format!("creating {}", p.parent().unwrap().display()))?;
    let body = toml::to_string(rec).context("serializing issue record")?;
    std::fs::write(&p, body).with_context(|| format!("writing {}", p.display()))
}

/// Read the record from `<worktree>/.devkit/issue.toml`, or `None` if absent or
/// unparseable.
pub fn read(worktree: &Path) -> Option<IssueRecord> {
    let body = std::fs::read_to_string(path(worktree)).ok()?;
    toml::from_str(&body).ok()
}
```

- [ ] **Step 4: Register the module**

In `src/bin/issue/main.rs`, add with the other `mod` declarations (the file has `mod review;` at line 8; insert `mod record;` near the alphabetical position among the existing `mod` lines):

```rust
mod record;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --bin issue record`
Expected: PASS — 2 tests.

(`toml` is already a dependency of the root `devkit` package, so no Cargo change is needed.)

- [ ] **Step 6: Commit**

```bash
git add src/bin/issue/record.rs src/bin/issue/main.rs
git commit -m "feat(issue): persist setup record for review-time context"
```

---

## Task 4: Global gitignore helper

**Files:**
- Create: `src/bin/issue/gitignore.rs`
- Modify: `src/bin/issue/main.rs` (register `mod gitignore;`)

- [ ] **Step 1: Write the failing test file**

Create `src/bin/issue/gitignore.rs`:

```rust
use anyhow::{Context, Result};
use devkit_ports::config::expand_tilde;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_configured_path() {
        let p = resolve_excludes_path(Some("~/custom/ignore"), "/home/u", None);
        assert_eq!(p, PathBuf::from("/home/u/custom/ignore"));
    }

    #[test]
    fn resolve_uses_xdg_when_unset() {
        let p = resolve_excludes_path(None, "/home/u", Some("/home/u/.xdg"));
        assert_eq!(p, PathBuf::from("/home/u/.xdg/git/ignore"));
    }

    #[test]
    fn resolve_falls_back_to_home() {
        let p = resolve_excludes_path(None, "/home/u", None);
        assert_eq!(p, PathBuf::from("/home/u/.config/git/ignore"));
    }

    #[test]
    fn needs_devkit_detects_presence() {
        assert!(needs_devkit(""));
        assert!(needs_devkit("node_modules/\n.other\n"));
        assert!(!needs_devkit("node_modules/\n.devkit/\n"));
        assert!(!needs_devkit(".devkit\n"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin issue gitignore`
Expected: FAIL — `resolve_excludes_path` / `needs_devkit` not found.

- [ ] **Step 3: Implement the pure helpers + the IO entry point**

Add to `src/bin/issue/gitignore.rs` (above the `#[cfg(test)]` block):

```rust
/// Resolve git's global excludes file. A configured `core.excludesfile` wins
/// (tilde-expanded); otherwise `$XDG_CONFIG_HOME/git/ignore`, else
/// `<home>/.config/git/ignore` — the path git reads by default.
fn resolve_excludes_path(configured: Option<&str>, home: &str, xdg: Option<&str>) -> PathBuf {
    if let Some(c) = configured.map(str::trim).filter(|c| !c.is_empty()) {
        return expand_tilde(c);
    }
    let base = match xdg.map(str::trim).filter(|x| !x.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => Path::new(home).join(".config"),
    };
    base.join("git").join("ignore")
}

/// True when `.devkit/` (or `.devkit`) is not already an ignore line.
fn needs_devkit(contents: &str) -> bool {
    !contents
        .lines()
        .map(str::trim)
        .any(|l| l == ".devkit/" || l == ".devkit")
}

/// Ensure `.devkit/` is in the global excludes file. Idempotent; append-only.
/// Returns an error on IO failure — the caller decides whether to ignore it.
pub fn ensure_devkit_ignored() -> Result<()> {
    let configured = devkit_common::cmd::capture("git", &["config", "--global", "core.excludesfile"], None)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let home = std::env::var("HOME").context("HOME not set")?;
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let path = resolve_excludes_path(configured.as_deref(), &home, xdg.as_deref());

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if !needs_devkit(&existing) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(".devkit/\n");
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    println!("added .devkit/ to {}", path.display());
    Ok(())
}
```

- [ ] **Step 4: Register the module**

In `src/bin/issue/main.rs`, add with the other `mod` declarations:

```rust
mod gitignore;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --bin issue gitignore`
Expected: PASS — 4 tests.

- [ ] **Step 6: Commit**

```bash
git add src/bin/issue/gitignore.rs src/bin/issue/main.rs
git commit -m "feat(issue): ensure .devkit is in the global gitignore"
```

---

## Task 5: Wire setup — render, persist, ignore

**Files:**
- Modify: `src/bin/issue/setup.rs`
- Modify: `src/bin/issue/main.rs`

- [ ] **Step 1: Replace the branch-name test with template-render tests**

In `src/bin/issue/setup.rs`, replace the entire `#[cfg(test)] mod tests { ... }` block (the `branch_uses_prefix_and_slug` test) with:

```rust
#[cfg(test)]
mod tests {
    use devkit_ports::config::Templates;
    use serde_json::json;

    #[test]
    fn default_branch_renders_prefix_and_slug() {
        let t = Templates::default();
        let ctx = json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix"});
        let out = devkit_common::template::render(t.branch(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "lev/fix");
    }

    #[test]
    fn default_worktree_dir_renders_slug() {
        let t = Templates::default();
        let ctx = json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix"});
        let out = devkit_common::template::render(t.worktree_dir(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "fix");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin issue setup`
Expected: FAIL — references to `Templates`/template render compile against code not yet wired (and the old `branch_name` test is gone). Build error is acceptable as the failing state.

- [ ] **Step 3: Add `no_gitignore` to `SetupArgs` and render via templates**

In `src/bin/issue/setup.rs`:

Add `no_gitignore` to the args struct:

```rust
pub struct SetupArgs {
    pub issue: String,
    pub slug: String,
    pub apps: Vec<String>,
    pub dry_run: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
    pub no_gitignore: bool,
}
```

Remove the `branch_name` free function (lines 25-27) and its use. Replace the `wt_root`/`worktree`/`branch` block (currently lines 40-43) with template rendering. After `let catalog = &loaded.catalog;` and the app-existence check, build a shared setup context and render:

```rust
    let wt_root = expand_tilde(&cfg.defaults.worktree_root);
    let monorepo = wt_root.join("monorepo");
    let ctx = serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": args.issue,
        "slug": args.slug,
        "apps": args.apps,
    });
    let vars = &cfg.templates.variables;
    let branch = devkit_common::template::render(cfg.templates.branch(), &ctx, vars)
        .context("rendering `branch` template")?
        .trim()
        .to_string();
    let wt_name = devkit_common::template::render(cfg.templates.worktree_dir(), &ctx, vars)
        .context("rendering `worktree_dir` template")?
        .trim()
        .to_string();
    let worktree = wt_root.join(&wt_name);
    let holder = worktree.to_string_lossy().into_owned();
```

(Delete the original `let branch = branch_name(...)` and `let holder = ...` lines so they are not duplicated.)

- [ ] **Step 4: Write the record and ensure gitignore after worktree creation**

In `src/bin/issue/setup.rs`, immediately after the `git(&["worktree", "add", ...])?;` call succeeds (after the worktree-add block, before the `// Per-app bootstrap` comment), insert:

```rust
    crate::record::write(
        &worktree,
        &crate::record::IssueRecord {
            issue: args.issue.clone(),
            slug: args.slug.clone(),
            apps: args.apps.clone(),
        },
    )?;
    if !args.no_gitignore {
        if let Err(e) = crate::gitignore::ensure_devkit_ignored() {
            eprintln!("warning: could not update global gitignore: {e:#}");
        }
    }
```

- [ ] **Step 5: Thread `no_gitignore` through `main.rs`**

In `src/bin/issue/main.rs`, add the flag to the `Setup` subcommand variant (after `dry_run`):

```rust
    Setup {
        #[arg(long)]
        issue: String,
        #[arg(long)]
        slug: String,
        #[arg(long, value_delimiter = ',')]
        apps: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long = "no-gitignore")]
        no_gitignore: bool,
    },
```

And in the dispatch arm:

```rust
        Some(Cmd::Setup {
            issue,
            slug,
            apps,
            dry_run,
            no_gitignore,
        }) => setup::run(setup::SetupArgs {
            issue,
            slug,
            apps,
            dry_run,
            dir: cli.dir,
            config: cli.config,
            no_gitignore,
        }),
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --bin issue setup && cargo build --bin issue`
Expected: PASS — 2 setup tests; binary builds.

- [ ] **Step 7: Commit**

```bash
git add src/bin/issue/setup.rs src/bin/issue/main.rs
git commit -m "feat(issue): render branch and worktree dir from templates"
```

---

## Task 6: Wire review — render PR title/body/Slack

**Files:**
- Modify: `src/bin/issue/review.rs`
- Modify: `src/bin/issue/main.rs`

- [ ] **Step 1: Replace the `compose_text` test, add guard + default-slack tests**

In `src/bin/issue/review.rs`, in the `tests` module, remove the `compose_appends_url` test and add:

```rust
    #[test]
    fn default_slack_appends_url() {
        let t = devkit_ports::config::Templates::default();
        let ctx = serde_json::json!({"input": "please review", "pr_url": "https://gh/pr/1"});
        let out = devkit_common::template::render(t.slack(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "please review https://gh/pr/1");
    }

    #[test]
    fn require_pr_title_rejects_empty() {
        assert!(require_pr_title("  ").is_err());
        assert!(require_pr_title("Fix login").is_ok());
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin issue review`
Expected: FAIL — `require_pr_title` not found; `compose_text` test removed.

- [ ] **Step 3: Replace `compose_text` with the title guard and a render helper**

In `src/bin/issue/review.rs`, replace the `compose_text` function (lines 46-49) with:

```rust
/// Reject an empty rendered PR title on the create path.
pub(crate) fn require_pr_title(title: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("--pr-title is required to create a PR");
    }
    Ok(())
}

/// Render a review template, attaching the template key and, when the setup
/// record is absent, a hint that an `{{ issue }}`/`{{ slug }}` reference needs it.
fn render_review(
    tmpl: &str,
    key: &str,
    ctx: &serde_json::Value,
    vars: &std::collections::BTreeMap<String, String>,
    missing_record_at: Option<&str>,
) -> Result<String> {
    let mut r = devkit_common::template::render(tmpl, ctx, vars)
        .with_context(|| format!("rendering `{key}` template"));
    if let Some(worktree) = missing_record_at {
        r = r.with_context(|| {
            format!("no .devkit/issue.toml found in {worktree} — was it created by `issue setup`?")
        });
    }
    r
}

/// Base context shared by every review template.
fn base_ctx(
    record: Option<&crate::record::IssueRecord>,
    branch: &str,
    reviewer: &str,
    to: &str,
) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("branch".into(), serde_json::json!(branch));
    m.insert("reviewer".into(), serde_json::json!(reviewer));
    m.insert("to".into(), serde_json::json!(to));
    if let Some(r) = record {
        m.insert("issue".into(), serde_json::json!(r.issue));
        m.insert("slug".into(), serde_json::json!(r.slug));
        m.insert("apps".into(), serde_json::json!(r.apps));
    }
    serde_json::Value::Object(m)
}

/// Clone `base` and add extra fields for a single template render.
fn with_fields(base: &serde_json::Value, extra: &[(&str, serde_json::Value)]) -> serde_json::Value {
    let mut m = base.as_object().cloned().unwrap_or_default();
    for (k, v) in extra {
        m.insert((*k).into(), v.clone());
    }
    serde_json::Value::Object(m)
}
```

Add `use anyhow::Context;` is already present via the `use anyhow::{Context, Result, bail};` line — confirm `Context` is in that import list (it is).

- [ ] **Step 4: Rebuild `run` to read the record and render in order**

In `src/bin/issue/review.rs` `run`, after `guard_branch(&branch)?;` and the push block, replace the PR-action/`compose_text` section. Read the record and compute the action up front:

```rust
    let toplevel = git(&["rev-parse", "--show-toplevel"], &start)?
        .trim()
        .to_string();
    let record = crate::record::read(std::path::Path::new(&toplevel));
    let missing_at = if record.is_none() {
        Some(toplevel.as_str())
    } else {
        None
    };
    let tmpls = &loaded.config.templates;
    let vars = &tmpls.variables;
    let base = base_ctx(record.as_ref(), &branch, &reviewer, &args.to);

    let pr_title = render_review(
        tmpls.pr_title(),
        "pr_title",
        &with_fields(&base, &[("input", serde_json::json!(args.pr_title.clone().unwrap_or_default()))]),
        vars,
        missing_at,
    )?;
    let pr_body = render_review(
        tmpls.pr_body(),
        "pr_body",
        &with_fields(
            &base,
            &[
                ("input", serde_json::json!(args.pr_body.clone().unwrap_or_default())),
                ("pr_title", serde_json::json!(pr_title)),
            ],
        ),
        vars,
        missing_at,
    )?;
```

- [ ] **Step 5: Use rendered title/body in the create path; render Slack after `pr_url`**

In the `PrAction::Create` arm, replace the `args.pr_title` / `args.pr_body` usage:

```rust
        PrAction::Create => {
            require_pr_title(&pr_title)?;
            let base_branch = args
                .base
                .clone()
                .unwrap_or_else(|| loaded.config.defaults.pr_base.clone());
            let out = capture(
                "gh",
                &[
                    "pr", "create",
                    "--base", &base_branch,
                    "--reviewer", &reviewer,
                    "--title", &pr_title,
                    "--body", &pr_body,
                ],
                Some(&start),
            )
            .context("gh pr create failed")?;
            out.lines()
                .rev()
                .find(|l| l.contains("://"))
                .context("could not parse a PR URL from `gh pr create` output")?
                .trim()
                .to_string()
        }
```

After the `let pr_url = match ... ;` block, replace `let text = compose_text(&args.body, &pr_url);` with:

```rust
    let text = render_review(
        tmpls.slack(),
        "slack",
        &with_fields(
            &base,
            &[
                ("input", serde_json::json!(args.body.clone().unwrap_or_default())),
                ("pr_title", serde_json::json!(pr_title)),
                ("pr_url", serde_json::json!(pr_url)),
            ],
        ),
        vars,
        missing_at,
    )?;
```

- [ ] **Step 6: Make `body` optional in `ReviewArgs` and `main.rs`**

In `src/bin/issue/review.rs`, change the field:

```rust
pub struct ReviewArgs {
    pub body: Option<String>,
    pub to: String,
    pub reviewer: Option<String>,
    pub base: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub no_push: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}
```

In `src/bin/issue/main.rs`, change the `Review` variant's positional `body`:

```rust
    Review {
        /// Slack message body. With a configured `slack` template this fills its
        /// `{{ input }}`; the PR URL is appended by the default template.
        body: Option<String>,
        #[arg(long)]
        to: String,
        #[arg(long)]
        reviewer: Option<String>,
        #[arg(long)]
        base: Option<String>,
        #[arg(long = "pr-title")]
        pr_title: Option<String>,
        #[arg(long = "pr-body")]
        pr_body: Option<String>,
        #[arg(long = "no-push")]
        no_push: bool,
    },
```

The dispatch arm already moves `body` into `ReviewArgs { body, ... }` — no change needed there since the field type now matches.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test --bin issue review && cargo build --bin issue`
Expected: PASS — review tests; binary builds.

- [ ] **Step 8: Run the full gate**

Run: `cargo test --workspace`
Expected: PASS except the pre-existing environment-sensitive `devkit-common` failures (`ui::tests::link_plain_when_unsupported`, `supervise::tests::probe_port_*`) — confirm any failure is one of those and is untouched by this branch (`git diff main...HEAD --stat -- crates/devkit-common/` is empty except where this plan added `template.rs`/`lib.rs`).

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add src/bin/issue/review.rs src/bin/issue/main.rs
git commit -m "feat(issue): render PR and Slack text from templates"
```

---

## Task 7: Documentation

**Files:**
- Modify: `README.md`
- Modify: `docs/next-steps.md`

- [ ] **Step 1: Document the `[templates]` table in the README**

In `README.md`, under the `## Configuration` section (near where app/launch templating is documented), add a `### Templates` subsection. Include:

- The `[templates]` keys (`branch`, `worktree_dir`, `pr_title`, `pr_body`, `slack`) and that each is optional, falling back to a default that reproduces prior behavior.
- The defaults table (branch `{{ prefix }}{{ slug }}`, worktree_dir `{{ slug }}`, pr_title/pr_body `{{ input }}`, slack `{{ input }} {{ pr_url }}`).
- The variable contexts: setup-site `prefix`/`issue`/`slug`/`apps`; review-site `branch`/`reviewer`/`to`/`issue`/`slug`/`apps` plus the per-template `input` (the matching CLI flag) and threaded `pr_title`/`pr_url`.
- `[templates.variables]` for user constants (context wins on name clash).
- minijinja Jinja syntax (`{{ }}`, `{% if %}`, `{% for %}`), strict-undefined behavior.
- The `.devkit/issue.toml` record and that `issue setup` adds `.devkit/` to the global gitignore unless `--no-gitignore` is passed.

Use this concrete example block:

````markdown
### Templates

`issue setup` and `issue review` render five strings from optional minijinja
templates under `[templates]`. Each unset key falls back to a default that
matches the historical hardcoded output.

```toml
[templates]
branch       = "{{ prefix }}{{ issue }}-{{ slug }}"
worktree_dir = "{{ slug }}"
pr_title     = "{{ issue }}: {{ input }}"
pr_body      = "Closes {{ issue }}.\n\n{{ input }}"
slack        = "{{ pr_title }}\n{{ input }}\n{{ pr_url }}"

[templates.variables]            # constants; a context field of the same name wins
team = "platform"
```

| Key | Default | Context |
|---|---|---|
| `branch`, `worktree_dir` | `{{ prefix }}{{ slug }}`, `{{ slug }}` | `prefix`, `issue`, `slug`, `apps` |
| `pr_title` | `{{ input }}` | review base + `input` = `--pr-title` |
| `pr_body` | `{{ input }}` | review base + `input` = `--pr-body`, `pr_title` |
| `slack` | `{{ input }} {{ pr_url }}` | review base + `input` = `body` arg, `pr_title`, `pr_url` |

Review base context: `branch`, `reviewer`, `to`, and `issue`/`slug`/`apps` from
the `.devkit/issue.toml` record `issue setup` writes in the worktree. `issue
setup` also adds `.devkit/` to your global gitignore (`--no-gitignore` skips it).
An undefined variable is an error (strict mode), so typos surface immediately.
````

- [ ] **Step 2: Mark items 2 & 3 resolved in next-steps.md**

In `docs/next-steps.md`, replace the two empty sections at the bottom:

```markdown
## Configurable templates for messages

## Configurable templates for issue start
```

with:

```markdown
## Configurable templates for messages

**Status:** RESOLVED 2026-06-24 — Slack review text and PR title/body are
minijinja templates under `[templates]` (`slack`, `pr_title`, `pr_body`), with
defaults reproducing prior behavior. See
`docs/superpowers/specs/2026-06-24-config-templating-design.md` and
`docs/superpowers/plans/2026-06-24-config-templating.md`.

## Configurable templates for issue start

**Status:** RESOLVED 2026-06-24 — `issue setup` renders the branch name and
worktree directory from `[templates]` (`branch`, `worktree_dir`), and persists a
`.devkit/issue.toml` record so review-time templates can reference `issue`/`slug`/`apps`.
See the spec/plan referenced above.
```

- [ ] **Step 3: Commit**

```bash
git add README.md docs/next-steps.md
git commit -m "docs: document config templating"
```

---

## Notes for the implementer

- **Worktree discipline:** all code work happens in a feature worktree under
  `../devkit-worktrees/`, never on the primary clone's `main` (see `AGENTS.md`).
- **Pre-existing test failures:** `devkit-common`'s `ui::tests::link_plain_when_unsupported`
  and `supervise::tests::probe_port_*` are environment-sensitive and fail on
  pristine `main` under WSL2. This branch touches none of those files (except
  adding `template.rs` + one `lib.rs` line); treat only *new* failures as real.
- **`capture` signature:** `devkit_common::cmd::capture(prog, &[args], cwd: Option<&str>)`
  returns `Result<String>` (stdout). Used in `gitignore.rs` for `git config`.
- **No Cargo change for `toml` in the issue bin:** the root `devkit` package
  already declares `toml.workspace = true`.
```
