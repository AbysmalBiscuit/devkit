# Task require_live + Lazy Sequence Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Command tasks can require that a referenced app's server is live in this worktree before running, `--env` overrides waive both the gate and the allocation, and sequences re-resolve each command step at execution time so long builds can't bake expired ports.

**Architecture:** All logic lives in `devkit-ports` (`task.rs` for resolution/gating, `registry.rs` for a pure `live_port` helper); `src/bin/devrun/main.rs` only re-wires execution to call the new `task::resolve_step` per step. The upfront `task::resolve` pass keeps full static validation and rendering (used for `--dry-run` and fail-before-spawn), but executed plans always come from a fresh `resolve_step` immediately before spawn.

**Tech Stack:** Rust edition 2024, anyhow, serde/toml, minijinja (via `devkit-common::template`), flock'd JSON registry.

**Spec:** `docs/superpowers/specs/2026-07-19-task-require-live-design.md`

## Global Constraints

- `cargo test --workspace` must stay green; `cargo clippy --workspace --all-targets -- -D warnings` zero-warning policy; `cargo fmt --all` before each commit.
- Conventional Commits, subject ≤50 chars, imperative, lowercase after colon.
- Work happens in a worktree under `../devkit-worktrees/` (never a branch checkout in the primary clone) — create via superpowers:using-git-worktrees at execution start, branch from `main`.
- Edition 2024: `std::env::set_var` is `unsafe`; wrap in `unsafe {}` (test-only, single-threaded start of test).
- Tests that touch the registry must isolate state by setting BOTH `HOME` and `XDG_STATE_HOME` to a tempdir (pattern in `crates/devkit-ports/tests/registry.rs`), and must not assume real ports are free below ~47000 — use bases ≥47300.
- Liveness syscalls (`pid_alive`, `listening`) stay OUTSIDE `with_lock` (registry facade invariant).
- No comments narrating the change; comments state present-tense behavior only.

## File Structure

- `crates/devkit-ports/src/config.rs` — `TaskConfig` gains `require_live: Vec<String>`.
- `crates/devkit-ports/src/registry.rs` — new pure `live_port(&Data, holder, app)` helper + holder-scoping regression test.
- `crates/devkit-ports/src/task.rs` — `effective_env` helper (override waiver), static `require_live` validation, liveness gate, new public `resolve_step`.
- `crates/devkit-ports/tests/task_gate.rs` — NEW env-isolated integration test (gate, waiver, holder scoping, lazy re-resolution) against a real flock store.
- `src/bin/devrun/main.rs` — `cmd_task` executes fresh `resolve_step` plans; `--dry-run` prints the upfront resolution unchanged.
- `docs/configuration.md`, `AGENTS.md` — document semantics + invariant.
- `~/.config/devkit/config.toml` — post-install rollout (Task 8, outside the repo).

---

### Task 1: `require_live` config field

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (TaskConfig, ~line 165)
- Modify: `crates/devkit-ports/src/task.rs` (sequence-shape validation, ~line 100)

**Interfaces:**
- Produces: `TaskConfig.require_live: Vec<String>` (serde default empty). Sequence tasks reject a non-empty `require_live`.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-ports/src/config.rs` tests module:

```rust
#[test]
fn task_require_live_roundtrips() {
    let s = "[tasks.build]\nrun=['git']\nrequire_live=['api']\n";
    let c = Config::parse(s).unwrap();
    assert_eq!(c.tasks["build"].require_live, vec!["api"]);
    let out = toml::to_string(&c).unwrap();
    let c2 = Config::parse(&out).unwrap();
    assert_eq!(c2.tasks["build"].require_live, vec!["api"]);
}
```

In `crates/devkit-ports/src/task.rs` tests module (extend `resolve_rejects_bad_shapes`):

```rust
let seq_with_require_live = TaskConfig {
    require_live: vec!["api-prod".into()],
    steps: vec![Step::Up("api-prod".into())],
    ..TaskConfig::default()
};
```

Add `("seq-require-live", seq_with_require_live)` to the `cfg_with` list and `"seq-require-live"` to the `for bad in [...]` loop.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports task_require_live_roundtrips resolve_rejects_bad_shapes`
Expected: compile error — `require_live` field does not exist.

- [ ] **Step 3: Implement**

In `config.rs`, add to `TaskConfig` (after `env`):

```rust
    /// Apps whose server must be live in this worktree when the task executes.
    /// Each name must be referenced by the task's templates via `ports[...]`;
    /// a user `--env` override of every referencing value waives the check.
    #[serde(default)]
    pub require_live: Vec<String>,
```

In `task.rs` `resolve`, extend the sequence guard:

```rust
            ensure!(
                t.app.is_none() && t.env.is_empty() && t.require_live.is_empty(),
                "sequence task `{name}` may only set `description` and `steps`"
            );
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports task_require_live_roundtrips resolve_rejects_bad_shapes`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs crates/devkit-ports/src/task.rs
git commit -m "feat(ports): add require_live field to task config"
```

---

### Task 2: Override waiver — effective env templates

**Files:**
- Modify: `crates/devkit-ports/src/task.rs` (`resolve_command` ~line 143, `resolve_command_with_ports` ~line 218, existing tests)

**Interfaces:**
- Consumes: `TaskConfig.require_live` from Task 1 (field exists; not yet validated).
- Produces: `fn effective_env<'a>(static_env: &'a HashMap<String,String>, t: &'a TaskConfig, user_env: &BTreeMap<String,String>) -> BTreeMap<&'a str, &'a str>` — `static_env` overlaid by task `env`, minus user-overridden keys. `resolve_command_with_ports` signature changes: the `static_env: &HashMap<String,String>` parameter becomes `env_templates: &BTreeMap<&str, &str>`, and the function no longer reads `t.env` (the map already merged it).

- [ ] **Step 1: Write the failing tests**

In `task.rs` tests module:

```rust
    #[test]
    fn effective_env_merges_and_drops_overridden_keys() {
        let static_env: HashMap<String, String> = [
            ("A".to_string(), "from-static".to_string()),
            ("B".to_string(), "from-static".to_string()),
        ]
        .into();
        let t = command_task(None, &["git"], &[("B", "from-task"), ("C", "from-task")]);
        let user: BTreeMap<String, String> = [("C".to_string(), "x".to_string())].into();
        let m = effective_env(&static_env, &t, &user);
        assert_eq!(m["A"], "from-static");
        assert_eq!(m["B"], "from-task");
        assert!(!m.contains_key("C"));
    }

    #[test]
    fn overridden_env_key_is_not_rendered() {
        // BASE references a port that is NOT in the ports map; rendering it
        // would error. The user override must make that value irrelevant.
        let t = command_task(None, &["git"], &[("BASE", "http://localhost:{{ ports['api-prod'] }}")]);
        let user: BTreeMap<String, String> =
            [("BASE".to_string(), "https://preview".to_string())].into();
        let env_templates = effective_env(&HashMap::new(), &t, &user);
        let plan = resolve_command_with_ports(
            "t",
            &t,
            &env_templates,
            Path::new("/wt"),
            None,
            &BTreeMap::new(),
            None,
            &BTreeMap::new(),
            &user,
        )
        .unwrap();
        assert_eq!(plan.env["BASE"], "https://preview");
    }
```

- [ ] **Step 2: Update existing tests to the new signature**

Every existing `resolve_command_with_ports` call in the tests passes `static_env`-shaped maps; convert them through `effective_env`. Example for `command_env_layering_static_then_task_then_user`:

```rust
        let cat = api_catalog();
        let env_templates = effective_env(&cat["api-prod"].static_env, &t, &user);
        let plan = resolve_command_with_ports(
            "t",
            &t,
            &env_templates,
            Path::new("/wt"),
            Some("apps/api"),
            &BTreeMap::new(),
            None,
            &BTreeMap::new(),
            &user,
        )
        .unwrap();
        assert_eq!(plan.env["FROM_APP"], "user");
```

Note: `effective_env` borrows from `cat`/`t`/`user`, so bind those to locals living past the call. The second half of that test (no user env) expects `plan2.env["FROM_APP"] == "task"` — build `env_templates` with an empty user map. `command_renders_ports_in_env_and_argv` and `command_prd_doppler_is_rejected` pass `effective_env(&HashMap::new(), &t, &BTreeMap::new())`.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p devkit-ports --lib task`
Expected: compile error — `effective_env` not defined.

- [ ] **Step 4: Implement**

In `task.rs`, above `resolve_command`:

```rust
/// Env templates a command task will render: `static_env` overlaid by the
/// task's `env`, minus any key the user's `--env` supplies. An overridden
/// value is neither scanned for port references nor rendered, so a port it
/// references neither allocates a reservation nor arms the liveness gate.
fn effective_env<'a>(
    static_env: &'a HashMap<String, String>,
    t: &'a TaskConfig,
    user_env: &BTreeMap<String, String>,
) -> BTreeMap<&'a str, &'a str> {
    let mut m: BTreeMap<&'a str, &'a str> = BTreeMap::new();
    for (k, v) in static_env {
        m.insert(k.as_str(), v.as_str());
    }
    for (k, v) in &t.env {
        m.insert(k.as_str(), v.as_str());
    }
    m.retain(|k, _| !user_env.contains_key(*k));
    m
}
```

In `resolve_command`, replace the template collection:

```rust
    let static_env = app.map(|a| a.static_env.clone()).unwrap_or_default();
    let env_templates = effective_env(&static_env, t, user_env);
    let vars = &cfg.templates.variables;

    let mut templates: Vec<&str> = t.run.iter().map(String::as_str).collect();
    templates.extend(env_templates.values().copied());
    let refs = template::referenced_ports(&templates, vars)
        .with_context(|| format!("scanning templates of task `{name}`"))?;
```

and pass `&env_templates` to `resolve_command_with_ports` instead of `&static_env`.

In `resolve_command_with_ports`: change the parameter to `env_templates: &BTreeMap<&str, &str>`, delete the two render loops over `static_env` and `t.env`, and render the merged map once:

```rust
    let mut env = BTreeMap::new();
    for (k, v) in env_templates {
        env.insert(
            (*k).to_string(),
            template::render_launch(v, own_port, ports, variables)
                .with_context(|| format!("rendering env `{k}` of task `{name}`"))?,
        );
    }
    for (k, v) in user_env {
        env.insert(k.clone(), v.clone());
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p devkit-ports --lib task`
Expected: PASS (all task tests, including the two new ones).

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-ports/src/task.rs
git commit -m "feat(ports): user env overrides waive task port refs"
```

---

### Task 3: Static `require_live` validation

**Files:**
- Modify: `crates/devkit-ports/src/task.rs` (`resolve_command`)

**Interfaces:**
- Consumes: `TaskConfig.require_live` (Task 1), `effective_env` (Task 2).
- Produces: `resolve_command` errors when `require_live` names an app absent from the catalog or unreferenced by the task's own templates (scanned WITHOUT the user-override filter, so an override can never turn a valid config into a static error).

- [ ] **Step 1: Write the failing tests**

In `task.rs` tests module:

```rust
    #[test]
    fn require_live_unknown_app_errors() {
        let mut t = command_task(None, &["git", "version"], &[]);
        t.require_live = vec!["nope".into()];
        let cfg = cfg_with(&[("t", t)]);
        let err = resolve(
            &cfg,
            &api_catalog(),
            Path::new("/wt"),
            "/wt",
            "t",
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("unknown app `nope`"));
    }

    #[test]
    fn require_live_unreferenced_app_errors() {
        let mut t = command_task(None, &["git", "version"], &[]);
        t.require_live = vec!["api-prod".into()];
        let cfg = cfg_with(&[("t", t)]);
        let err = resolve(
            &cfg,
            &api_catalog(),
            Path::new("/wt"),
            "/wt",
            "t",
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("never references"));
    }
```

(Both tasks reference no ports, so `resolve` never touches the registry — safe as unit tests.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports --lib require_live`
Expected: FAIL — `resolve` currently succeeds (no validation exists).

- [ ] **Step 3: Implement**

In `resolve_command`, after `let vars = ...` and before the effective-scan block, add the un-filtered scan + checks:

```rust
    let mut all_templates: Vec<&str> = t.run.iter().map(String::as_str).collect();
    all_templates.extend(static_env.values().map(String::as_str));
    all_templates.extend(t.env.values().map(String::as_str));
    let all_refs = template::referenced_ports(&all_templates, vars)
        .with_context(|| format!("scanning templates of task `{name}`"))?;
    for r in &t.require_live {
        ensure!(
            catalog.contains_key(r),
            "task `{name}` lists unknown app `{r}` in require_live"
        );
        ensure!(
            all_refs.apps.contains(r),
            "task `{name}` lists `{r}` in require_live but never references `ports['{r}']`"
        );
    }
```

(`static_env` is still in scope from Task 2's changes; order the block so `static_env` and `env_templates` are both computed before it or reorder as needed — `all_templates` uses the raw maps, `templates` uses `env_templates`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports --lib task`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/task.rs
git commit -m "feat(ports): validate require_live names and references"
```

---

### Task 4: `live_port` registry helper + holder-scoping regression test

**Files:**
- Modify: `crates/devkit-ports/src/registry.rs`

**Interfaces:**
- Produces: `pub fn live_port(data: &Data, holder: &str, app: &str) -> Option<u16>` — the port of `app`'s issue-role reservation under `holder` whose pid is set and alive.

- [ ] **Step 1: Write the failing tests**

In `registry.rs` (inside the existing in-module `#[cfg(test)]` tests, alongside the `Data`-level tests):

```rust
    #[test]
    fn live_port_requires_matching_live_row() {
        let mut d = Data::default();
        let p = d.alloc_one("/wt", "api", 47340, Role::Issue);
        assert_eq!(live_port(&d, "/wt", "api"), None); // pid-less reservation
        d.record_pid(p, "api", "/wt", Role::Issue, std::process::id(), "l".into());
        assert_eq!(live_port(&d, "/wt", "api"), Some(p));
        assert_eq!(live_port(&d, "/other", "api"), None); // wrong holder
        assert_eq!(live_port(&d, "/wt", "web"), None); // wrong app
        d.record_pid(p, "api", "/wt", Role::Issue, u32::MAX, "l".into());
        assert_eq!(live_port(&d, "/wt", "api"), None); // dead pid
    }

    #[test]
    fn alloc_one_skips_foreign_holder_row() {
        let mut d = Data::default();
        assert_eq!(d.alloc_one("/foreign", "api", 47350, Role::Issue), 47350);
        // Same app under a different holder never captures the foreign row.
        assert_eq!(d.alloc_one("/mine", "api", 47350, Role::Issue), 47351);
        // Idempotent per holder.
        assert_eq!(d.alloc_one("/mine", "api", 47350, Role::Issue), 47351);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports --lib live_port alloc_one_skips`
Expected: compile error — `live_port` undefined. (`alloc_one_skips_foreign_holder_row` locks in existing behavior; after Step 3 both must pass — if the alloc test fails, holder scoping is broken and must be investigated, not adjusted.)

- [ ] **Step 3: Implement**

In `registry.rs`, near `pid_alive`:

```rust
/// Port of `app`'s live reservation under `holder` (issue role): a row whose
/// pid is set and alive. The pid probe keeps the answer correct even on an
/// unpruned view; keep calls outside `with_lock`.
pub fn live_port(data: &Data, holder: &str, app: &str) -> Option<u16> {
    data.entries.iter().find_map(|(port, e)| {
        (e.holder == holder && e.app == app && e.role == Role::Issue && e.pid.is_some_and(pid_alive))
            .then_some(*port)
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports --lib live_port alloc_one_skips`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/registry.rs
git commit -m "feat(ports): add live_port lookup and holder-scope test"
```

---

### Task 5: Liveness gate + `resolve_step`

**Files:**
- Modify: `crates/devkit-ports/src/task.rs`

**Interfaces:**
- Consumes: `registry::live_port` (Task 4), static validation (Task 3).
- Produces:
  - `resolve_command` gains a trailing `enforce_live: bool` parameter. `resolve()` passes `false` at both call sites (upfront pass never gates).
  - `pub fn resolve_step(cfg: &Config, catalog: &HashMap<String, App>, worktree_root: &Path, holder: &str, name: &str, user_env: &BTreeMap<String, String>) -> Result<CommandPlan>` — resolves command task `name` with fresh allocation, fresh render, and the gate enforced. Errors if `name` is not a command task.
  - Gate semantics: for each `require_live` app still present in the effective (post-waiver) refs, `registry::snapshot()` must contain a live row for (holder, app, Issue) — checked BEFORE allocation so a failed gate mints no reservation. Error text: ``require_live: `{app}` has no live server in this worktree (devrun up {app})``.

- [ ] **Step 1: Write the failing test (compile-level)**

The gate needs a real registry, covered by Task 6's integration test. Here, drive the API shape with a unit test that exercises `resolve_step`'s task-shape validation (no ports involved, registry untouched):

```rust
    #[test]
    fn resolve_step_rejects_non_command_tasks() {
        let seq = TaskConfig {
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let cfg = cfg_with(&[("seq", seq)]);
        let err = resolve_step(
            &cfg,
            &api_catalog(),
            Path::new("/wt"),
            "/wt",
            "seq",
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("not a command task"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-ports --lib resolve_step`
Expected: compile error — `resolve_step` undefined.

- [ ] **Step 3: Implement**

Add `enforce_live: bool` as the last parameter of `resolve_command`; update the two `resolve()` call sites to pass `false`. In `resolve_command`, insert the gate between the require_live static validation and the allocation:

```rust
    if enforce_live {
        let gated: Vec<&String> = t
            .require_live
            .iter()
            .filter(|r| refs.apps.contains(*r))
            .collect();
        if !gated.is_empty() {
            let data = registry::snapshot()?;
            for r in gated {
                ensure!(
                    registry::live_port(&data, holder, r).is_some(),
                    "require_live: `{r}` has no live server in this worktree (devrun up {r})"
                );
            }
        }
    }
```

(This requires `refs` computed before the gate; keep the order: static validation → effective scan (`refs`) → gate → allocation.)

Add the public entry point:

```rust
/// Resolve command task `name` for immediate execution: fresh allocation,
/// fresh render, `require_live` enforced. Sequences call this per step at
/// execution time so a long-running earlier step cannot expire the ports an
/// upfront render used; standalone commands call it right before exec.
pub fn resolve_step(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    user_env: &BTreeMap<String, String>,
) -> Result<CommandPlan> {
    let t = cfg
        .tasks
        .get(name)
        .ok_or_else(|| anyhow!("unknown task `{name}` (run `devrun task` to list)"))?;
    ensure!(
        !t.run.is_empty() && t.steps.is_empty(),
        "task `{name}` is not a command task"
    );
    resolve_command(cfg, catalog, worktree_root, holder, name, t, user_env, true)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports --lib task`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/task.rs
git commit -m "feat(ports): gate task execution on live servers"
```

---

### Task 6: Integration test — gate, waiver, scoping, laziness end-to-end

**Files:**
- Create: `crates/devkit-ports/tests/task_gate.rs`

**Interfaces:**
- Consumes: `task::resolve_step` (Task 5), `registry::{with_lock, record_pid, release_ports, snapshot}` facades.

- [ ] **Step 1: Write the test**

```rust
//! Env-isolated end-to-end checks of `task::resolve_step` against a real
//! flock-backed registry: the require_live gate, the user-override waiver,
//! holder-scoped allocation, and fresh per-call resolution.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use devkit_ports::apps::App;
use devkit_ports::config::{Config, TaskConfig};
use devkit_ports::registry::{self, Role};
use devkit_ports::task;

const BASE: u16 = 47360;

fn catalog() -> HashMap<String, App> {
    let mut m = HashMap::new();
    m.insert(
        "api".to_string(),
        App {
            name: "api".into(),
            base_port: BASE,
            path: "apps/api".into(),
            launch: vec![],
            url_env: None,
            provides_url: false,
            static_env: HashMap::new(),
            prep_files: vec![],
            setup: vec![],
        },
    );
    m
}

fn build_task() -> TaskConfig {
    TaskConfig {
        run: vec!["git".into(), "version".into()],
        env: [(
            "BASE".to_string(),
            "http://localhost:{{ ports['api'] }}".to_string(),
        )]
        .into(),
        require_live: vec!["api".into()],
        ..TaskConfig::default()
    }
}

#[test]
fn gate_waiver_scoping_and_lazy_resolution() {
    // The registry path comes from process-global env, so every scenario runs
    // sequentially inside this single test.
    let tmp = std::env::temp_dir().join(format!("devkit-task-gate-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe {
        std::env::set_var("HOME", &tmp);
        std::env::set_var("XDG_STATE_HOME", &tmp);
    }
    let mine = tmp.join("wt-mine");
    let foreign = tmp.join("wt-foreign");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::create_dir_all(&foreign).unwrap();
    let mine_s = mine.to_str().unwrap();
    let foreign_s = foreign.to_str().unwrap();

    let mut cfg = Config::default();
    cfg.tasks.insert("build".into(), build_task());
    let cat = catalog();
    let none = BTreeMap::new();

    // Gate: no reservation at all → loud error, and no reservation minted.
    let err = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &none).unwrap_err();
    assert!(format!("{err:#}").contains("no live server"), "{err:#}");
    assert!(registry::snapshot().unwrap().entries.is_empty());

    // Waiver: overriding BASE removes the only reference to `api`, so the
    // gate is skipped and nothing is allocated.
    let user: BTreeMap<String, String> =
        [("BASE".to_string(), "https://preview".to_string())].into();
    let plan = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &user).unwrap();
    assert_eq!(plan.env["BASE"], "https://preview");
    assert!(registry::snapshot().unwrap().entries.is_empty());

    // Holder scoping: a foreign holder on BASE never satisfies my gate and
    // never leaks into my allocation.
    registry::with_lock(|d| Ok(d.alloc_one(foreign_s, "api", BASE, Role::Issue))).unwrap();
    let err = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &none).unwrap_err();
    assert!(format!("{err:#}").contains("no live server"), "{err:#}");

    let my_port = registry::with_lock(|d| Ok(d.alloc_one(mine_s, "api", BASE, Role::Issue))).unwrap();
    assert_eq!(my_port, BASE + 1);
    registry::record_pid(
        my_port,
        "api",
        mine_s,
        Role::Issue,
        std::process::id(),
        tmp.join("api.log"),
    )
    .unwrap();
    let plan = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &none).unwrap();
    assert_eq!(plan.env["BASE"], format!("http://localhost:{}", BASE + 1));

    // Laziness: after the server moves, a fresh resolve_step renders the new
    // port — nothing is cached from the earlier call.
    registry::release_ports(&[my_port]).unwrap();
    registry::release_ports(&[BASE]).unwrap(); // free the foreign row too
    let moved = registry::with_lock(|d| Ok(d.alloc_one(mine_s, "api", BASE, Role::Issue))).unwrap();
    assert_eq!(moved, BASE);
    registry::record_pid(
        moved,
        "api",
        mine_s,
        Role::Issue,
        std::process::id(),
        tmp.join("api.log"),
    )
    .unwrap();
    let plan = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &none).unwrap();
    assert_eq!(plan.env["BASE"], format!("http://localhost:{BASE}"));
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p devkit-ports --test task_gate`
Expected: PASS (Tasks 1–5 landed). If any scenario fails, the corresponding earlier task is buggy — fix there, not by weakening the test. Two harness caveats: check `App`'s current field list (`crates/devkit-ports/src/apps.rs`) if the struct literal fails to compile, and adjust the literal, not the struct; and if the first `registry::snapshot()` errors because no registry file exists yet, seed the store first with `registry::with_lock(|_| Ok(()))` at the top of the test rather than dropping the empty-registry asserts.

- [ ] **Step 3: Verify pre-existing suite still green**

Run: `cargo test -p devkit-ports`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/devkit-ports/tests/task_gate.rs
git commit -m "test(ports): cover gate, waiver, scoping, laziness"
```

---

### Task 7: devrun executes fresh per-step plans

**Files:**
- Modify: `src/bin/devrun/main.rs` (`cmd_task`, ~line 497)

**Interfaces:**
- Consumes: `task::resolve_step` (Task 5). `CommandPlan.name` is the task's config name — used to re-resolve.
- Produces: non-dry-run execution always runs a plan freshly resolved by `resolve_step`; `--dry-run` prints the upfront `resolve()` output unchanged and never gates.

- [ ] **Step 1: Implement**

Replace the `match resolved` block in `cmd_task`:

```rust
    let root_path = Path::new(&root);
    match resolved {
        Resolved::Command(plan) => {
            if dry_run {
                return run_task_step(&plan, true);
            }
            let fresh =
                task::resolve_step(&loaded.config, &loaded.catalog, root_path, &root, name, &user)?;
            run_task_step(&fresh, false)
        }
        Resolved::Sequence(items) => {
            for item in &items {
                match item {
                    SeqItem::Run(plan) => {
                        if dry_run {
                            run_task_step(plan, true)?;
                        } else {
                            let fresh = task::resolve_step(
                                &loaded.config,
                                &loaded.catalog,
                                root_path,
                                &root,
                                &plan.name,
                                &user,
                            )?;
                            run_task_step(&fresh, false)?;
                        }
                    }
                    SeqItem::Up(app) => {
                        cmd_up(
                            cli,
                            cwd,
                            std::slice::from_ref(app),
                            RoleSelector::Issue,
                            env_pairs,
                            env_file,
                            UpFlags {
                                dry_run,
                                supervise: false,
                            },
                        )?;
                    }
                }
            }
            Ok(())
        }
    }
```

- [ ] **Step 2: Build and run the full check**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: zero warnings, all tests pass.

- [ ] **Step 3: Manual smoke (dry-run only, safe anywhere)**

Run from any configured worktree (or skip if none available in the execution environment): `devrun task --help` builds; `cargo run --bin devrun -- task` lists tasks.
Expected: no behavior change in listing/dry-run.

- [ ] **Step 4: Commit**

```bash
git add src/bin/devrun/main.rs
git commit -m "feat(devrun): re-resolve task steps at execution time"
```

---

### Task 8: Documentation

**Files:**
- Modify: `docs/configuration.md` (Tasks section, ~line 107)
- Modify: `AGENTS.md` (Invariants section)

- [ ] **Step 1: Document `require_live` in configuration.md**

Add to the Tasks section, after the existing `ports[...]` explanation (fit the surrounding style):

```markdown
- `require_live = ["app", …]` (command tasks only): each listed app must have a
  live devrun-managed server in this worktree when the task *executes*, or the
  task fails before spawning:

      require_live: `api-serve` has no live server in this worktree (devrun up api-serve)

  Each name must be referenced by the task's templates via `ports[...]`. A
  `--env`/`--env-file` override that replaces every value referencing an app
  waives that app's check *and* its allocation — a user-supplied URL makes the
  local port irrelevant (this is how one build task serves both a local flow
  and a hosted-preview flow). References in `run` argv cannot be overridden.
- Sequence steps are re-resolved immediately before each step runs: ports are
  re-allocated and templates re-rendered then, and `require_live` is checked
  then — so a step gated on an app that an earlier `up` step starts works, and
  a build step longer than the reservation grace period cannot bake a port
  that a later `up` step no longer gets. `--dry-run` prints the upfront
  resolution and never gates.
```

- [ ] **Step 2: Add the AGENTS.md invariant**

Append to the Invariants list:

```markdown
- **Sequence steps re-resolve at execution time; the upfront pass never
  gates.** `task::resolve` validates every step before anything spawns, but
  its rendered plans are for validation and `--dry-run` display only —
  execution calls `task::resolve_step` per command step (fresh allocation +
  render, `require_live` enforced) immediately before spawning it. Don't
  execute the upfront plans: a build step longer than
  `RESERVATION_GRACE_SECS` would let a t=0 reservation expire and desync
  later steps. And don't enforce `require_live` in the upfront pass — a
  gated app may be brought up by an earlier `up` step of the same sequence.
```

- [ ] **Step 3: Commit**

```bash
git add docs/configuration.md AGENTS.md
git commit -m "docs: cover require_live and lazy step resolution"
```

---

### Task 9: Land the branch + install

- [ ] **Step 1: Final gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: clean.

- [ ] **Step 2: Fast-forward main from outside its worktree** (per AGENTS.md)

```bash
git -C /home/lev/Git/lev/devkit switch main
git -C /home/lev/Git/lev/devkit merge --ff-only <branch>
git worktree remove ../devkit-worktrees/<name>
```

(If `main` moved, rebase the branch first. Do not push unless the user asks.)

- [ ] **Step 3: Install the new binaries**

Run: `cargo install --path /home/lev/Git/lev/devkit`
Expected: `devrun` in `~/.cargo/bin` reports the new behavior (`devrun task` still lists tasks).

---

### Task 10: Personal config rollout (outside the repo)

**Files:**
- Modify: `~/.config/devkit/config.toml`

All four edits below; then smoke-test. These target the AdaptyvBio monorepo config, not the devkit repo.

- [ ] **Step 1: Template the prd build task and gate both build tasks**

In `[tasks.lab-os-profile-build]`: add `require_live = ["api-serve"]` (its env already uses `{{ ports['api-serve'] }}`).

In `[tasks.lab-os-profile-build-prd]`: replace the hardcoded env value and add the gate:

```toml
[tasks.lab-os-profile-build-prd]
description = "profiled next build against PROD data (prd_local_api); local prod-api URL baked in"
app = "lab-os-serve"
require_live = ["api-serve"]
run = [
    "doppler",
    "run",
    "--preserve-env=FOUNDRY_API_BASE_URL",
    "-p",
    "lab-os",
    "-c",
    "prd_local_api",
    "--",
    "bun",
    "next",
    "build",
    "--profile",
]
env = { WORKCELL_BLI_RUN_WORKFLOW_ID = "asy-bli-run/v0.0.8", FOUNDRY_API_BASE_URL = "http://localhost:{{ ports['api-serve'] }}", VERCEL_ENV = "production" }
```

Also update the stale comment above it (9292 is not a pinned slot; the port now resolves from this worktree's live api-serve, and `profile-lab-os-preview`'s `--env` override waives the gate).

- [ ] **Step 2: Template lab-os-serve-prd's static_env**

```toml
static_env = { WORKCELL_BLI_RUN_WORKFLOW_ID = "dummy", FOUNDRY_API_BASE_URL = "http://localhost:{{ ports['api-serve'] }}", VERCEL_ENV = "production" }
```

(and drop the "boot-safe placeholder" comment — the value now tracks the worktree's api-serve; `--env` still overrides for the preview flow).

- [ ] **Step 3: Add the bake assertion task and wire it into the local sequence**

```toml
# The API host is compiled into .next/routes-manifest.json; this asserts the
# baked port is this worktree's live api-serve and that it answers. Catches a
# server that moved after the build (registry is advisory; the curl checks
# what's actually listening).
[tasks.lab-os-assert-bake]
description = "assert built lab-os routes-manifest targets this worktree's live api-serve"
app = "lab-os-serve"
require_live = ["api-serve"]
run = [
    "bash",
    "-c",
    """
set -euo pipefail
want={{ ports['api-serve'] }}
baked=$(jq -r '[.rewrites[].destination // empty | scan("localhost:[0-9]+")] | unique | .[]' .next/routes-manifest.json | cut -d: -f2)
[ -n "$baked" ] || { echo "no localhost destination baked in routes-manifest.json"; exit 1; }
[ "$baked" = "$want" ] || { echo "baked port $baked != live api-serve $want — rebuild (devrun task lab-os-profile-build)"; exit 1; }
curl -s -o /dev/null "http://localhost:$want/" || { echo "api-serve on $want is not answering"; exit 1; }
echo "bake OK: routes-manifest → localhost:$want (live)"
""",
]
```

Insert into the local profiling sequence after the build:

```toml
[tasks.profile-lab-os]
description = "profiling stack: built api + profiled lab-os, wired together"
steps = [
    { task = "api-profile-build" },
    { up = "api-serve" },
    { task = "lab-os-profile-build" },
    { task = "lab-os-assert-bake" },
    { up = "lab-os-serve" },
]
```

(Per decision: NOT added to `profile-lab-os-preview` — its bake is a Vercel URL, not a local port.)

- [ ] **Step 4: Smoke-test in the monorepo worktree**

```bash
devrun task lab-os-profile-build-prd --dry-run   # renders ports['api-serve'], no gate
devrun task lab-os-profile-build-prd             # with api-serve DOWN: expect loud require_live error
devrun up api-serve && devrun task lab-os-assert-bake   # after a real build: expect "bake OK"
```

Expected: the gate error names `api-serve` and suggests `devrun up api-serve`; the assertion passes against a fresh build and fails against a stale one.

---

## Unresolved questions

None — spec decisions (tasks-only gating, non-prd twin gated, assert task local-only) were settled with the user before this plan.
