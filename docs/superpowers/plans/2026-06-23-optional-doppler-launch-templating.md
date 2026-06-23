# Optional doppler via full launch templating — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `launch` the complete, verbatim command devkit runs, so Doppler is one possible wrapper a config writes rather than a compiled-in prefix; keep Doppler first-class via a launch-time `prd` guard.

**Architecture:** Delete the `doppler run …` prefix `run::plan_group` builds today; the plan's argv becomes the port-substituted `launch` argv unchanged. A new launch-time guard (`run::assert_not_prd`) resolves each Doppler launch's effective config — explicit `-c`/`--config`, then `DOPPLER_CONFIG` in the resolved env, then `doppler configure get config --scope <cwd>` — and refuses `prd` (and refuses an unresolvable Doppler launch). The `doppler_config`, `doppler_project`, and `preserve_env` config keys are removed.

**Tech Stack:** Rust 2024 workspace; `anyhow` for errors; `cargo test`/`clippy`/`fmt` gate. Spec: `docs/superpowers/specs/2026-06-23-optional-doppler-launch-templating-design.md`.

---

## File structure

| File | Change |
|---|---|
| `crates/devkit-ports/src/run.rs` | Add `config_from_argv_env`, `doppler_scoped_config`, `assert_not_prd`; delete `doppler_prefix`; `plan_group` drops its `doppler_config` param and emits verbatim argv; `launch` calls the guard. |
| `crates/devkit-ports/src/config.rs` | `Defaults` drops `doppler_config`; `AppConfig` drops `doppler_project` and `preserve_env`; `Config::parse` drops the `prd` ensure; test samples updated. |
| `crates/devkit-ports/src/apps.rs` | `App` + `catalog` drop `doppler_project`/`preserve_env`; `path_to_project` keys still drive path inference. |
| `src/bin/devrun/main.rs` | Drop the `doppler_config` argument to `plan_group`. |
| `crates/devkit-mcp/src/devrun.rs` | Drop the `doppler_config` argument to `plan_group`. |
| `docs/configuration.md` | Drop `doppler_config`/`preserve_env` rows; rewrite the `launch` row; add the guard + migration notes. |
| `AGENTS.md` | Rewrite the "`prd` is rejected" invariant to describe the launch-parse guard. |
| `README.md` | Move `doppler` out of **Required**. (Bun already removed.) |

Order is chosen so every task ends compiling and green: Task 1 adds the guard (purely additive), Task 2 removes the prefix and rewires call sites together, Task 3 deletes the now-dead keys, Task 4 finishes docs.

---

## Task 1: Add the launch-time `prd` guard (additive)

**Files:**
- Modify: `crates/devkit-ports/src/run.rs` (add functions after `env_for`, ends line 59; add tests in the `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/devkit-ports/src/run.rs` (e.g. after `wires_api_url_for_consumer`, ~line 437):

```rust
#[test]
fn config_from_explicit_flag() {
    let env = BTreeMap::new();
    let v = |a: &[&str]| {
        config_from_argv_env(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &env)
    };
    assert_eq!(v(&["doppler", "run", "-c", "prd", "--", "x"]).as_deref(), Some("prd"));
    assert_eq!(v(&["doppler", "run", "-c=stg", "--", "x"]).as_deref(), Some("stg"));
    assert_eq!(v(&["doppler", "run", "--config", "dev", "--", "x"]).as_deref(), Some("dev"));
    assert_eq!(v(&["doppler", "run", "--config=dev", "--", "x"]).as_deref(), Some("dev"));
}

#[test]
fn config_flag_after_separator_is_ignored() {
    // `-c prod` belongs to the wrapped command, not doppler.
    let argv: Vec<String> = ["doppler", "run", "--", "tool", "-c", "prod"]
        .iter().map(|s| s.to_string()).collect();
    assert_eq!(config_from_argv_env(&argv, &BTreeMap::new()), None);
}

#[test]
fn config_from_env_when_no_flag() {
    let mut env = BTreeMap::new();
    env.insert("DOPPLER_CONFIG".to_string(), "prd".to_string());
    let argv: Vec<String> = ["doppler", "run", "--", "x"].iter().map(|s| s.to_string()).collect();
    assert_eq!(config_from_argv_env(&argv, &env).as_deref(), Some("prd"));
}

#[test]
fn non_doppler_launch_resolves_to_none() {
    let argv: Vec<String> = ["next", "dev", "-c", "prd"].iter().map(|s| s.to_string()).collect();
    assert_eq!(config_from_argv_env(&argv, &BTreeMap::new()), None);
}

#[test]
fn guard_rejects_prd_and_unresolvable_doppler() {
    let plan = |argv: &[&str], env: BTreeMap<String, String>| LaunchPlan {
        app: "web".into(),
        port: 1,
        argv: argv.iter().map(|s| s.to_string()).collect(),
        cwd: std::path::PathBuf::from("/nonexistent-app-dir"),
        env,
        log: std::path::PathBuf::from("/dev/null"),
    };
    // explicit prd → reject
    assert!(assert_not_prd(&plan(&["doppler", "run", "-c", "prd", "--", "x"], BTreeMap::new())).is_err());
    // explicit safe config → ok
    assert!(assert_not_prd(&plan(&["doppler", "run", "-c", "dev", "--", "x"], BTreeMap::new())).is_ok());
    // non-doppler launch → ok (unguarded)
    assert!(assert_not_prd(&plan(&["next", "dev"], BTreeMap::new())).is_ok());
    // doppler launch with no flag/env, cwd has no scope → unresolvable → reject
    assert!(assert_not_prd(&plan(&["doppler", "run", "--", "x"], BTreeMap::new())).is_err());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-ports config_from_ guard_rejects_ non_doppler_launch -- --nocapture`
Expected: compile error — `config_from_argv_env` / `assert_not_prd` not found.

- [ ] **Step 3: Implement the guard**

Insert after `env_for` (after line 59) in `crates/devkit-ports/src/run.rs`:

```rust
/// Resolve the Doppler config a launch would use *from inputs devkit already
/// holds*: an explicit `-c`/`--config` flag in the launch argv (highest
/// precedence, scanned only up to the `--` separator), else `DOPPLER_CONFIG` in
/// the resolved env. Returns `None` when the launch is not a Doppler invocation
/// or specifies no inline config.
pub fn config_from_argv_env(argv: &[String], env: &BTreeMap<String, String>) -> Option<String> {
    let prog = argv.first()?;
    if Path::new(prog).file_name().and_then(|s| s.to_str()) != Some("doppler") {
        return None;
    }
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        if a == "-c" || a == "--config" {
            if let Some(v) = it.next() {
                return Some(v.clone());
            }
        } else if let Some(v) = a.strip_prefix("-c=").or_else(|| a.strip_prefix("--config=")) {
            return Some(v.to_string());
        }
    }
    env.get("DOPPLER_CONFIG").cloned()
}

/// Best-effort read of the locally-scoped Doppler config for `cwd` via
/// `doppler configure get config --plain --scope <cwd>`. This reads the persisted
/// scope (`~/.doppler/.doppler.yaml`) and does *not* fetch secrets. Returns `None`
/// if `doppler` is absent, exits non-zero, or prints nothing.
fn doppler_scoped_config(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("doppler")
        .args(["configure", "get", "config", "--plain", "--scope"])
        .arg(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Refuse a launch that would run Doppler against the `prd` config. A launch
/// whose program is not `doppler` is unguarded. For a Doppler launch the config
/// is resolved in Doppler's own precedence order — explicit flag, then
/// `DOPPLER_CONFIG`, then the local scope — and a launch whose config resolves to
/// `prd`, or cannot be resolved at all, is rejected (fail-safe).
pub fn assert_not_prd(plan: &LaunchPlan) -> Result<()> {
    let prog = plan.argv.first().map(String::as_str).unwrap_or_default();
    if Path::new(prog).file_name().and_then(|s| s.to_str()) != Some("doppler") {
        return Ok(());
    }
    let config = config_from_argv_env(&plan.argv, &plan.env)
        .or_else(|| doppler_scoped_config(&plan.cwd));
    match config.as_deref() {
        Some("prd") => anyhow::bail!(
            "refusing to launch `{}`: doppler config resolves to `prd` (production secrets)",
            plan.app
        ),
        Some(_) => Ok(()),
        None => anyhow::bail!(
            "refusing to launch `{}`: cannot determine its doppler config (no -c/--config, \
             no DOPPLER_CONFIG, no local scope). Add an explicit `-c <config>` to its launch.",
            plan.app
        ),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports config_from_ guard_rejects_ non_doppler_launch config_flag_after`
Expected: PASS (5 tests). Note: `guard_rejects_…` runs `doppler configure get` for the unresolvable case; with no flag/env and a nonexistent cwd it returns `None` → reject, so the test passes whether or not `doppler` is installed.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/run.rs
git commit -m "feat(devrun): add launch-time prd guard for doppler launches"
```

---

## Task 2: Replace the built doppler prefix with the verbatim launch + wire the guard

**Files:**
- Modify: `crates/devkit-ports/src/run.rs` (delete `doppler_prefix` lines 17-31; `plan_group` lines 169-201; `launch` line 281+; rewrite test at line 508)
- Modify: `src/bin/devrun/main.rs:307`
- Modify: `crates/devkit-mcp/src/devrun.rs:92`

- [ ] **Step 1: Rewrite the failing test**

Replace `plan_group_builds_doppler_wrapped_argv` (lines 507-531) in `crates/devkit-ports/src/run.rs` with:

```rust
#[test]
fn plan_group_runs_launch_verbatim() {
    let mut catalog = HashMap::new();
    catalog.insert("web".to_string(), app("web", None));
    let mut ports = BTreeMap::new();
    ports.insert("web".to_string(), 4321u16);
    let plans = plan_group(
        &catalog,
        &["web".to_string()],
        &ports,
        None,
        std::path::Path::new("/root"),
        Role::Issue,
        &BTreeMap::new(),
    );
    assert_eq!(plans.len(), 1);
    let p = &plans[0];
    assert_eq!(p.app, "web");
    assert_eq!(p.port, 4321);
    // No prefix is built: argv is the port-substituted launch, verbatim.
    assert_eq!(p.argv, vec!["next", "dev", "-p", "4321"]);
    assert!(p.cwd.ends_with("apps/x"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit-ports plan_group_runs_launch_verbatim`
Expected: compile error — `plan_group` still takes the `doppler_config` arg, and `argv[0]` is still `doppler`.

- [ ] **Step 3: Delete `doppler_prefix`**

Remove lines 17-31 of `crates/devkit-ports/src/run.rs` (the `doppler_prefix` doc comment and function in full):

```rust
/// Build the doppler argv prefix: `doppler run -p <project> -c <config> [--preserve-env=K]... --`
pub fn doppler_prefix(app: &App, config: &str) -> Vec<String> {
    let mut v = vec!["doppler".into(), "run".into()];
    if let Some(p) = &app.doppler_project {
        v.push("-p".into());
        v.push(p.clone());
    }
    v.push("-c".into());
    v.push(config.into());
    for k in &app.preserve_env {
        v.push(format!("--preserve-env={k}"));
    }
    v.push("--".into());
    v
}
```

- [ ] **Step 4: Drop the `doppler_config` param from `plan_group` and emit verbatim argv**

In `plan_group` (`crates/devkit-ports/src/run.rs`), remove the `doppler_config: &str,` parameter (line 171) so the signature is:

```rust
pub fn plan_group(
    catalog: &HashMap<String, App>,
    apps: &[String],
    ports: &BTreeMap<String, u16>,
    provider: Option<&str>,
    base_dir: &Path,
    role: Role,
    user_env: &BTreeMap<String, String>,
) -> Vec<LaunchPlan> {
```

And replace the two argv lines (184-185):

```rust
        let mut argv = doppler_prefix(app, doppler_config);
        argv.extend(launch_argv(app, port));
```

with:

```rust
        let argv = launch_argv(app, port);
```

- [ ] **Step 5: Call the guard at the top of `launch`**

In `launch` (`crates/devkit-ports/src/run.rs`, line 281), insert the guard as the first statement of the body, before the `#[cfg(feature = "daemon")]` block:

```rust
) -> Result<Vec<ServerStatus>> {
    for p in plans {
        assert_not_prd(p)?;
    }
    #[cfg(feature = "daemon")]
    if supervise_daemon {
```

- [ ] **Step 6: Update the two `plan_group` call sites**

In `src/bin/devrun/main.rs`, delete line 307 (`&cfg.defaults.doppler_config,`) so the call reads:

```rust
        let plans = run::plan_group(
            catalog,
            &apps,
            &ports,
            provider.as_deref(),
            base_dir,
            *grp_role,
            &user,
        );
```

In `crates/devkit-mcp/src/devrun.rs`, delete line 92 (`&loaded.config.defaults.doppler_config,`) so the call reads:

```rust
    let plans = run::plan_group(
        catalog,
        &apps,
        &ports,
        provider.as_deref(),
        Path::new(&a.root),
        Role::Issue,
        &user,
    );
```

- [ ] **Step 7: Run the workspace build + tests to verify green**

Run: `cargo test -p devkit-ports plan_group_runs_launch_verbatim && cargo build --workspace`
Expected: the rewritten test PASSES and the whole workspace compiles. (`App.doppler_project`/`preserve_env` are now set-but-unused — that is fine for `pub` struct fields and is cleaned up in Task 3.)

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-ports/src/run.rs src/bin/devrun/main.rs crates/devkit-mcp/src/devrun.rs
git commit -m "feat(devrun): run launch argv verbatim instead of wrapping in doppler"
```

---

## Task 3: Remove the dead doppler config keys

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (`Defaults` line 74; `AppConfig` lines 113, 126; `Config::parse` lines 134-137; samples lines 186, 192, 241; `rejects_prd` test lines 204-208)
- Modify: `crates/devkit-ports/src/apps.rs` (`App` lines 9, 14; `catalog` lines 42-59; test lines 75-83)
- Modify: `crates/devkit-ports/src/run.rs` (test helper `app` lines 409, 414)

- [ ] **Step 1: Drop the fields and the parse-time guard in `config.rs`**

Remove `pub doppler_config: String,` (line 74) from `Defaults`. Leave `doppler_yaml`.

Remove from `AppConfig` the `preserve_env` field (line 112-113):

```rust
    #[serde(default)]
    pub preserve_env: Vec<String>,
```

and the `doppler_project` field (line 125-126):

```rust
    #[serde(default)]
    pub doppler_project: Option<String>,
```

Simplify `Config::parse` (lines 131-140) to drop the `prd` ensure:

```rust
impl Config {
    pub fn parse(s: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(s).context("parsing devkit.toml")?;
        Ok(cfg)
    }
}
```

- [ ] **Step 2: Update the test samples in `config.rs`**

In the `SAMPLE` const (lines 180-194): delete the `doppler_config = "dev_local"` line (186) and the `preserve_env = ["SUPABASE_JWT_SECRET"]` line (192), and change the `api` launch (line 190) to the full templated command:

```toml
launch = ["doppler", "run", "-c", "dev_local", "--", "nitro", "dev", "--port", "{port}"]
```

In the inline config inside `parses_people_and_pr_base` (line 241): delete the `doppler_config = "dev_local"` line.

Delete the now-obsolete `rejects_prd` test (lines 204-208) — the guard moved to `run::assert_not_prd` (covered in Task 1):

```rust
#[test]
fn rejects_prd() {
    let bad = SAMPLE.replace("dev_local", "prd");
    assert!(Config::parse(&bad).is_err());
}
```

- [ ] **Step 3: Drop the fields from `App` and `catalog` in `apps.rs`**

Remove from the `App` struct: `pub doppler_project: Option<String>,` (line 9) and `pub preserve_env: Vec<String>,` (line 14).

In `catalog` (lines 42-61), delete the `project` computation (lines 42-45) and the two field assignments (`doppler_project: project,` and `preserve_env: a.preserve_env.clone(),`), so the inserted `App` is:

```rust
        out.insert(
            name.clone(),
            App {
                name: name.clone(),
                base_port: a.base_port,
                path,
                launch: a.launch.clone(),
                url_env: a.url_env.clone(),
                provides_url: a.provides_url,
                static_env: a.static_env.clone(),
                prep_env: a.prep_env.clone(),
                setup: a.setup.clone(),
            },
        );
```

The `path_to_project: &HashMap<String, String>` parameter stays — `guess_path` still uses its keys for path inference.

- [ ] **Step 4: Replace the `apps.rs` doppler-project test with a path-inference test**

Replace `pulls_project_from_doppler` (lines 75-83) with:

```rust
#[test]
fn infers_path_from_doppler_yaml() {
    let cfg = Config::parse(crate::config::tests_sample()).unwrap();
    let mut p2p = HashMap::new();
    p2p.insert("apps/api".to_string(), "api-foundry".to_string());
    let cat = catalog(&cfg, &p2p).unwrap();
    // `api` has no explicit `path`; it is inferred from the doppler.yaml key.
    assert_eq!(cat["api"].path, "apps/api");
}
```

- [ ] **Step 5: Update the `run.rs` test helper**

In the `app` helper (`crates/devkit-ports/src/run.rs`, lines 405-419), remove the `doppler_project: Some("proj".into()),` line (409) and the `preserve_env: vec![],` line (414).

- [ ] **Step 6: Run the full workspace gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: all tests PASS; zero clippy warnings (no unused fields remain).

- [ ] **Step 7: Format and commit**

```bash
cargo fmt --all
git add crates/devkit-ports/src/config.rs crates/devkit-ports/src/apps.rs crates/devkit-ports/src/run.rs
git commit -m "refactor(ports): drop doppler_config, doppler_project, preserve_env keys"
```

---

## Task 4: Documentation

**Files:**
- Modify: `README.md` (Requirements block, lines 192-199)
- Modify: `docs/configuration.md` (`[defaults]` table; `[apps.*]` table; add notes)
- Modify: `AGENTS.md` (the "`prd` is rejected" invariant bullet)

- [ ] **Step 1: Move `doppler` out of README Required**

In `README.md`, change the Requirements block so `doppler` is no longer required:

```markdown
**Required:**

- `git`
- `gh` (GitHub CLI, authenticated)

**Optional:**

- `$LINEAR_API_KEY` — enables the Linear issue-Done gate in `issue status`/`issue end` and the issue timeline in `issue dashboard`
```

(Also update the `devrun` description near line 20: it currently says servers "are launched under `doppler run -c dev_local`, bypassing each app's own `dev` script." Change to: "Each app's `launch` command is run verbatim with `{port}` substituted; wrap it in `doppler run` in the config if the app needs Doppler-injected secrets.")

- [ ] **Step 2: Update the `[defaults]` table in `docs/configuration.md`**

Delete the `doppler_config` row. Reword the `doppler_yaml` row to drop the "required" claim and describe its remaining role:

```markdown
| `doppler_yaml` | no | Path to the repo's `doppler.yaml`; its `setup` paths seed app **path inference**. `~` is expanded. Absent → apps need an explicit `path`. |
```

- [ ] **Step 3: Update the `[apps.*]` table and add the guard note in `docs/configuration.md`**

Delete the `preserve_env` row. Rewrite the `launch` row:

```markdown
| `launch` | yes | The complete launch command, run verbatim. `{port}` is substituted with the allocated port. Write the whole invocation here, including any `doppler run -c <config> --` wrapper and `--preserve-env=…` flags the app needs. |
```

Add a paragraph after the table:

```markdown
devkit runs `launch` exactly as written — it builds no command prefix. To use
Doppler, wrap the command yourself, e.g.
`launch = ["doppler","run","-c","dev_local","--","nitro","dev","--port","{port}"]`.
Before starting such a server, devkit refuses a launch that resolves to the
`prd` config: it reads `-c`/`--config` from `launch`, then `DOPPLER_CONFIG` from
the app's env, then `doppler configure get config --scope <app dir>`; a Doppler
launch whose config is `prd` or cannot be resolved is rejected.

**Migration:** earlier configs set `[defaults].doppler_config` and let devkit
prepend `doppler run`. Move that wrapper into each app's `launch`, and delete the
`doppler_config`, `doppler_project`, and `preserve_env` keys (fold any
`--preserve-env=…` into `launch`).
```

- [ ] **Step 4: Update the `prd` invariant in `AGENTS.md`**

Replace the bullet:

```markdown
- **`prd` is rejected** as a `doppler_config` to avoid running against production secrets.
```

with:

```markdown
- **A `prd` doppler launch is rejected.** `launch` is run verbatim, so devkit
  guards at launch time: for a launch whose program is `doppler`, it resolves the
  config from `-c`/`--config`, else `DOPPLER_CONFIG`, else `doppler configure get
  config --scope <app dir>`, and refuses to start a server when that resolves to
  `prd` or cannot be resolved. The guard lives in `run::assert_not_prd`, called
  from `run::launch`, so it covers `devrun`, the MCP `devrun.up`, and both the
  daemon and direct spawn paths.
```

- [ ] **Step 5: Commit**

```bash
git add README.md docs/configuration.md AGENTS.md
git commit -m "docs: document launch templating and the launch-time prd guard"
```

---

## Final verification

- [ ] **Run the full merge gate**

Run:
```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: formatting clean, all tests green, zero warnings.

- [ ] **Manual smoke (optional, needs `doppler`)**

With a `devkit.toml` whose app `launch` is `["doppler","run","-c","prd","--","echo","hi"]`,
run `devrun up --dry-run` then `devrun up` and confirm the guard refuses with the
`prd` message. Repeat with `-c dev_local` and confirm it launches.
