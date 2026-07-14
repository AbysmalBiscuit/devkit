# devrun Canned Tasks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `devrun up` idempotent, migrate all launch/env config strings to minijinja with a registry-backed `ports` map, and add a `[tasks]` config table + `devrun task` subcommand for canned oneshot commands and sequences.

**Architecture:** Three phases from `docs/superpowers/specs/2026-07-14-devrun-tasks-design.md`, each landing separately: (1) skip spawning when a live server already holds the (holder, app, role) row — both the direct `run::launch` path and the daemon's `supervise_app`; (2) route launch argv and `static_env` values through the existing `devkit_common::template::render` minijinja seam with a `port`/`ports` context, resolved two-phase (discovery render → one `registry::alloc` → final render); (3) a top-level `[tasks]` table (command tasks and sequence tasks) executed by a new `devrun task` subcommand.

**Tech Stack:** Rust (edition 2024), clap 4, serde/toml, minijinja 2 (`default-features = false, features = ["builtins", "serde"]`), anyhow.

## Global Constraints

- Work in a dedicated worktree, never on a branch in the primary clone: `git worktree add ../devkit-worktrees/devrun-tasks -b feat/devrun-tasks main` and run all commands from there.
- `cargo test --workspace` must stay green after every task; `cargo clippy --workspace --all-targets -- -D warnings` zero-warning policy; `cargo fmt --all` before each commit.
- Conventional Commits; one logical change per commit; imperative subject ≤50 chars.
- Tests that spawn or reap processes poll for the expected state, never sleep a fixed interval (Windows CI).
- `anyhow` with `.context()` everywhere; no `unwrap()` outside tests.
- Comments state present-tense behavior only — no PR/task/phase references.
- Tasks always resolve **issue-role** allocations; holder is the worktree root path.
- `RESERVATION_GRACE_SECS` (300) and all registry invariants in `AGENTS.md` are untouched.

---

## Phase 1 — `up` idempotency

### Task 1: Skip spawn for a live server in `run::launch` (direct path)

**Files:**
- Modify: `crates/devkit-ports/src/run.rs` (launch at :382, tests module at bottom)

**Interfaces:**
- Consumes: `registry::snapshot() -> Result<Data>`, `registry::pid_alive(u32) -> bool`, existing `server_state(port, pid)` and `ServerStatus`.
- Produces: private `existing_server(data: &Data, plan: &LaunchPlan, holder: &str, role: Role) -> Option<ServerStatus>` used by `launch`. `launch`'s signature is unchanged; its result now includes already-running servers with their existing pid.

- [ ] **Step 1: Write the failing tests** (in `run.rs`'s `mod tests`)

```rust
fn entry(app: &str, holder: &str, role: Role, pid: Option<u32>) -> registry::Entry {
    registry::Entry {
        app: app.into(),
        holder: holder.into(),
        role,
        pid,
        logfile: None,
        ts: registry::now(),
    }
}

fn plan(app: &str, port: u16) -> LaunchPlan {
    LaunchPlan {
        app: app.into(),
        port,
        argv: vec!["true".into()],
        cwd: PathBuf::from("."),
        env: BTreeMap::new(),
        log: PathBuf::from("x.log"),
    }
}

/// A dead pid: spawn a trivial child and wait for it, so the pid is reaped.
fn dead_pid() -> u32 {
    let mut c = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "exit"]);
        c
    } else {
        std::process::Command::new("true")
    };
    let mut child = c.spawn().expect("spawn trivial child");
    let pid = child.id();
    child.wait().expect("wait trivial child");
    pid
}

#[test]
fn existing_server_reports_live_pid() {
    let mut data = Data::default();
    let me = std::process::id();
    data.entries.insert(49811, entry("api", "/wt", Role::Issue, Some(me)));
    let s = existing_server(&data, &plan("api", 49811), "/wt", Role::Issue)
        .expect("live pid on matching row must be reported");
    assert_eq!(s.pid, Some(me));
    assert_eq!(s.port, 49811);
}

#[test]
fn existing_server_ignores_dead_pid() {
    let mut data = Data::default();
    data.entries.insert(49812, entry("api", "/wt", Role::Issue, Some(dead_pid())));
    assert!(existing_server(&data, &plan("api", 49812), "/wt", Role::Issue).is_none());
}

#[test]
fn existing_server_ignores_pidless_reservation_and_foreign_rows() {
    let mut data = Data::default();
    let me = std::process::id();
    data.entries.insert(49813, entry("api", "/wt", Role::Issue, None));
    data.entries.insert(49814, entry("web", "/wt", Role::Issue, Some(me)));
    data.entries.insert(49815, entry("api", "/other", Role::Issue, Some(me)));
    data.entries.insert(49816, entry("api", "/wt", Role::Baseline, Some(me)));
    assert!(existing_server(&data, &plan("api", 49813), "/wt", Role::Issue).is_none());
    assert!(existing_server(&data, &plan("api", 49814), "/wt", Role::Issue).is_none());
    assert!(existing_server(&data, &plan("api", 49815), "/wt", Role::Issue).is_none());
    assert!(existing_server(&data, &plan("api", 49816), "/wt", Role::Issue).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports --lib existing_server`
Expected: compile error — `existing_server` not found (RED).

- [ ] **Step 3: Implement `existing_server` and wire it into `launch`**

Add above `launch` in `run.rs`:

```rust
/// The plan's port row when it already tracks a live process for the same
/// holder+app+role. `up` reports such a server instead of respawning it:
/// a duplicate would fail to bind, and on the daemon path would repoint the
/// supervision table at the doomed pid.
fn existing_server(
    data: &Data,
    plan: &LaunchPlan,
    holder: &str,
    role: Role,
) -> Option<ServerStatus> {
    let e = data.entries.get(&plan.port)?;
    if e.holder != holder || e.app != plan.app || e.role != role {
        return None;
    }
    let pid = e.pid?;
    if !registry::pid_alive(pid) {
        return None;
    }
    Some(ServerStatus {
        app: plan.app.clone(),
        role,
        port: plan.port,
        pid: Some(pid),
        logfile: e.logfile.clone(),
        state: server_state(plan.port, Some(pid)),
    })
}
```

In `launch` (run.rs:382), after the `assert_not_prd` loop and the daemon hand-off, partition plans before spawning:

```rust
    let data = registry::snapshot()?;
    let mut existing = Vec::new();
    let mut pending: Vec<&LaunchPlan> = Vec::new();
    for p in plans {
        match existing_server(&data, p, holder, role) {
            Some(s) => existing.push(s),
            None => pending.push(p),
        }
    }

    let mut spawned = Vec::with_capacity(pending.len());
    for p in pending {
        // ... existing spawn_detached + record_pid body, unchanged ...
    }
```

At the end of both the `wait` and non-`wait` branches, append the skipped servers: change each `Ok(spawned.into_iter().map(...).collect())` to

```rust
        let mut out: Vec<ServerStatus> = spawned.into_iter().map(/* unchanged */).collect();
        out.extend(existing);
        Ok(out)
```

(in the `wait` branch, `out` is built the same way from the ready-map block; `existing` servers do not join the `wait_ready` scope — their state was just probed).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports --lib existing_server`
Expected: 3 tests PASS.

- [ ] **Step 5: Full check + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/devkit-ports/src/run.rs
git commit -m "fix(devrun): report a live server instead of respawning on up"
```

### Task 2: Same skip in the daemon's `supervise_app` + integration test

**Files:**
- Modify: `src/bin/devkitd/server.rs:106-156` (`supervise_app`)
- Modify: `tests/supervision.rs` (new test)
- Modify: `AGENTS.md` (invariants section)

**Interfaces:**
- Consumes: `daemon.port_store()` (implements `registry::Store`), `registry::{pid_alive, listening}`, test helpers `common::{Harness, free_port, pid_in_ports_json}` — note `pid_in_ports_json(body: &str, app_name: &str) -> Option<u32>` takes the JSON *body* (use `h.ports_json()`).
- Produces: a second `Request::Supervise` for a live (holder, app, role) returns `Response::Supervised(vec![(port, listening)])` without spawning; the supervision table is untouched on the skip path.

- [ ] **Step 1: Write the failing integration test** (append to `tests/supervision.rs`; the file is `#![cfg(unix)]`)

```rust
/// A second Supervise for a key whose server is alive is a no-op: same port,
/// ready, and the pid in ports.json does not change (no duplicate spawn).
#[test]
fn second_supervise_of_live_server_is_noop() {
    let mut h = Harness::start();
    let port = common::free_port();
    let holder = h.home.to_str().unwrap().to_string();
    let req = Request::Supervise {
        holder: holder.clone(),
        app: "api".into(),
        role: Role::Issue,
        argv: vec![
            "python3".into(),
            "-m".into(),
            "http.server".into(),
            port.to_string(),
        ],
        cwd: ".".into(),
        env: BTreeMap::new(),
        logfile: h.home.join("noop.log"),
        base_port: port,
    };

    let first = h.request(&req);
    assert!(
        matches!(&first, Response::Supervised(v) if v.first().map(|(_, r)| *r) == Some(true)),
        "first supervise did not become ready: {first:?}"
    );
    let pid1 = pid_in_ports_json(&h.ports_json(), "api").expect("no pid after first supervise");

    let second = h.request(&req);
    assert!(
        matches!(&second, Response::Supervised(v) if v.first() == Some(&(port, true))),
        "second supervise must report the live server: {second:?}"
    );
    let pid2 = pid_in_ports_json(&h.ports_json(), "api").expect("no pid after second supervise");
    assert_eq!(pid2, pid1, "second supervise must not respawn");

    h.request(&Request::Down { holder, role: None });
    h.shutdown();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test supervision second_supervise_of_live_server_is_noop`
Expected: FAIL — today the second Supervise spawns a duplicate; either `pid2 != pid1` or the second response reports `ready=false` (the duplicate can't bind). Confirm the failure is one of these, not a harness error.

- [ ] **Step 3: Implement the skip in `supervise_app`**

In `src/bin/devkitd/server.rs`, after the `alloc_with` block resolves `port` and before the `Key`/spawn code:

```rust
    // A live server already tracked for this key is reported, not respawned:
    // the duplicate would fail to bind, and insert_owned would repoint the
    // supervision table at the doomed pid while the real server keeps running.
    match daemon.port_store().snapshot() {
        Ok(snap) => {
            if let Some(e) = snap.entries.get(&port)
                && e.holder == holder
                && e.app == app
                && e.role == role
                && let Some(pid) = e.pid
                && registry::pid_alive(pid)
            {
                return Response::Supervised(vec![(port, registry::listening(port))]);
            }
        }
        Err(e) => return Response::Err(format!("{e:#}")),
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test supervision`
Expected: all supervision tests PASS, including the new one.

- [ ] **Step 5: Record the invariant in AGENTS.md**

In the `## Invariants (do not break)` section of `AGENTS.md`, append:

```markdown
- **`up` is idempotent for a live server.** Both `run::launch` (direct path) and
  the daemon's `Supervise` handler skip the spawn when the (holder, app, role)
  row already has a live pid, reporting the existing server instead. A duplicate
  spawn would fail to bind, and on the daemon path would repoint the supervision
  table at the doomed pid. Sequence-task `up` steps rely on this.
```

- [ ] **Step 6: Full check + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add src/bin/devkitd/server.rs tests/supervision.rs AGENTS.md
git commit -m "fix(devkitd): make second supervise of live server a no-op"
```

---

## Phase 2 — minijinja everywhere

### Task 3: `render_launch` + `referenced_ports` in devkit-common

**Files:**
- Modify: `crates/devkit-common/src/template.rs`

**Interfaces:**
- Consumes: existing `render(template, ctx, variables)` and `merged_context`.
- Produces (used by Tasks 4 and 8):
  - `pub struct PortRefs { pub apps: BTreeSet<String>, pub own_port: bool }`
  - `pub fn referenced_ports(templates: &[&str], variables: &BTreeMap<String, String>) -> Result<PortRefs>` — discovery render, no registry access.
  - `pub fn render_launch(template: &str, port: Option<u16>, ports: &BTreeMap<String, u16>, variables: &BTreeMap<String, String>) -> Result<String>` — final render; errors if the output still contains literal `{port}`.

- [ ] **Step 1: Write the failing tests** (append to `template.rs`'s `mod tests`)

```rust
    fn ports(pairs: &[(&str, u16)]) -> BTreeMap<String, u16> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn render_launch_substitutes_port_and_ports() {
        let out = render_launch(
            "http://localhost:{{ ports['api-prod'] }} own={{ port }}",
            Some(9200),
            &ports(&[("api-prod", 9101)]),
            &novars(),
        )
        .unwrap();
        assert_eq!(out, "http://localhost:9101 own=9200");
    }

    #[test]
    fn render_launch_unknown_ports_key_is_an_error() {
        assert!(render_launch("{{ ports['nope'] }}", None, &ports(&[]), &novars()).is_err());
    }

    #[test]
    fn render_launch_port_without_app_is_an_error() {
        assert!(render_launch("{{ port }}", None, &ports(&[]), &novars()).is_err());
    }

    #[test]
    fn render_launch_rejects_leftover_brace_port() {
        let err = render_launch("--port {port}", Some(9200), &ports(&[]), &novars())
            .unwrap_err()
            .to_string();
        assert!(err.contains("{{ port }}"), "error must show the migration hint: {err}");
    }

    #[test]
    fn render_launch_uses_variables() {
        let mut vars = novars();
        vars.insert("cfg".into(), "dev_local".into());
        let out = render_launch("-c {{ cfg }}", None, &ports(&[]), &vars).unwrap();
        assert_eq!(out, "-c dev_local");
    }

    #[test]
    fn referenced_ports_collects_apps_and_own_port() {
        let refs = referenced_ports(
            &[
                "http://localhost:{{ ports['api-prod'] }}",
                "NITRO_PORT={{ port }}",
                "{{ ports[\"web\"] }}",
                "no refs here",
            ],
            &novars(),
        )
        .unwrap();
        assert_eq!(
            refs.apps,
            ["api-prod", "web"].iter().map(|s| s.to_string()).collect()
        );
        assert!(refs.own_port);
    }

    #[test]
    fn referenced_ports_empty_when_no_refs() {
        let refs = referenced_ports(&["plain", "-c dev"], &novars()).unwrap();
        assert!(refs.apps.is_empty());
        assert!(!refs.own_port);
    }

    #[test]
    fn referenced_ports_unknown_variable_is_an_error() {
        assert!(referenced_ports(&["{{ typo }}"], &novars()).is_err());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-common --lib template`
Expected: compile error — `render_launch`/`referenced_ports` not found (RED).

- [ ] **Step 3: Implement**

Add to `template.rs` (top-of-file imports gain `minijinja::value::{Object, Value}`, `std::collections::BTreeSet`, `std::sync::atomic::{AtomicBool, Ordering}`, `std::sync::{Arc, Mutex}`):

```rust
/// Render a compiled template against a prebuilt minijinja root value, with the
/// same strict-undefined and trailing-newline settings as [`render`].
fn render_value(template: &str, root: Value) -> Result<String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_keep_trailing_newline(true);
    env.add_template("t", template)
        .context("compiling template")?;
    let tmpl = env.get_template("t").expect("template just added");
    tmpl.render(root).context("rendering template")
}

/// Port references a set of launch/task templates makes: the app names looked
/// up via `ports[...]` and whether `port` (the app's own port) is used.
#[derive(Debug, Default, PartialEq)]
pub struct PortRefs {
    pub apps: BTreeSet<String>,
    pub own_port: bool,
}

/// Records every key looked up on `ports` during a discovery render, returning
/// a placeholder value so rendering proceeds.
#[derive(Debug, Default)]
struct PortsRecorder {
    apps: Mutex<BTreeSet<String>>,
}

impl Object for PortsRecorder {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let k = key.as_str()?;
        self.apps.lock().unwrap().insert(k.to_string());
        Some(Value::from(0u16))
    }
}

/// Discovery-render root: serves `port`/`ports` from recorders and everything
/// else from the user variables, so an unknown name still errors (strict).
#[derive(Debug)]
struct DiscoveryCtx {
    vars: BTreeMap<String, String>,
    ports: Arc<PortsRecorder>,
    own_port: Arc<AtomicBool>,
}

impl Object for DiscoveryCtx {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        match key.as_str()? {
            "port" => {
                self.own_port.store(true, Ordering::SeqCst);
                Some(Value::from(0u16))
            }
            "ports" => Some(Value::from_dyn_object(self.ports.clone())),
            k => self.vars.get(k).map(|v| Value::from(v.clone())),
        }
    }
}

/// Collect the port references across `templates` by rendering each against a
/// recording context. Never touches the port registry; `ports[...]` lookups
/// return a placeholder. A reference inside a branch not taken under
/// placeholder values goes unrecorded — keep port refs out of conditionals.
pub fn referenced_ports(
    templates: &[&str],
    variables: &BTreeMap<String, String>,
) -> Result<PortRefs> {
    let ports = Arc::new(PortsRecorder::default());
    let own_port = Arc::new(AtomicBool::new(false));
    for t in templates {
        let ctx = Arc::new(DiscoveryCtx {
            vars: variables.clone(),
            ports: ports.clone(),
            own_port: own_port.clone(),
        });
        render_value(t, Value::from_dyn_object(ctx))
            .with_context(|| format!("scanning template `{t}` for port references"))?;
    }
    Ok(PortRefs {
        apps: std::mem::take(&mut ports.apps.lock().unwrap()),
        own_port: own_port.load(Ordering::SeqCst),
    })
}

#[derive(Serialize)]
struct LaunchCtx<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    ports: &'a BTreeMap<String, u16>,
}

/// Render one launch/static_env/task string against the port context. `port`
/// is the app's own allocated port (absent for a task without an `app`);
/// `ports` maps app name → this worktree's allocated port. Errors if the
/// output still contains the retired `{port}` placeholder, which minijinja
/// would otherwise pass through as literal text.
pub fn render_launch(
    template: &str,
    port: Option<u16>,
    ports: &BTreeMap<String, u16>,
    variables: &BTreeMap<String, String>,
) -> Result<String> {
    let out = render(template, &LaunchCtx { port, ports }, variables)?;
    anyhow::ensure!(
        !out.contains("{port}"),
        "`{{port}}` is retired; use `{{{{ port }}}}` (minijinja) in launch/static_env/task templates"
    );
    Ok(out)
}
```

Also refactor the existing `render` to reuse `render_value` (delete its duplicated `Environment` setup):

```rust
pub fn render(
    template: &str,
    ctx: &impl Serialize,
    variables: &BTreeMap<String, String>,
) -> Result<String> {
    let value = merged_context(ctx, variables)?;
    render_value(template, Value::from_serialize(&value))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-common --lib template`
Expected: all template tests PASS (old and new).

- [ ] **Step 5: Full check + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/devkit-common/src/template.rs
git commit -m "feat(common): add port-context template render and discovery"
```

### Task 4: Render launch argv + static_env; `resolve_ports`; retire `{port}`

**Files:**
- Modify: `crates/devkit-ports/src/run.rs` (`launch_argv` :18 deleted, `plan_group` :235, tests)
- Modify: `crates/devkit-ports/src/strays/signature.rs:25` (+ one test)
- Modify: `crates/devkit-ports/src/strays/mod.rs:400` (test fixture)
- Modify: `crates/devkit-ports/src/config.rs` (test-sample TOMLs at :437, :526, :543, :572)
- Modify: `src/bin/devrun/main.rs:527-543` (`cmd_up` alloc/plan block)
- Modify: `crates/devkit-mcp/src/devrun.rs:79-98` (MCP `up`)

**Interfaces:**
- Consumes: `template::{referenced_ports, render_launch}` (Task 3), `registry::alloc`.
- Produces:
  - `pub fn resolve_ports(catalog: &HashMap<String, App>, apps: &[String], holder: &str, role: Role, variables: &BTreeMap<String, String>) -> Result<BTreeMap<String, u16>>` — selected apps ∪ referenced apps, one alloc.
  - `pub fn plan_group(catalog, apps, ports, provider, base_dir, role, user_env, variables: &BTreeMap<String, String>) -> Result<Vec<LaunchPlan>>` — now renders argv and static_env values; `launch_argv` is deleted.

- [ ] **Step 1: Write the failing tests** (in `run.rs`'s `mod tests`; replace any existing `launch_argv` tests)

```rust
    fn test_app(launch: &[&str], static_env: &[(&str, &str)]) -> App {
        App {
            name: "api".into(),
            base_port: 9100,
            path: "apps/api".into(),
            launch: launch.iter().map(|s| s.to_string()).collect(),
            url_env: None,
            provides_url: false,
            static_env: static_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            prep_files: vec![],
            setup: vec![],
        }
    }

    #[test]
    fn plan_group_renders_argv_and_static_env() {
        let mut catalog = HashMap::new();
        catalog.insert(
            "api".to_string(),
            test_app(
                &["nitro", "dev", "--port", "{{ port }}"],
                &[("PEER", "http://localhost:{{ ports['api-prod'] }}")],
            ),
        );
        let ports: BTreeMap<String, u16> =
            [("api".to_string(), 9100), ("api-prod".to_string(), 9101)].into();
        let plans = plan_group(
            &catalog,
            &["api".to_string()],
            &ports,
            None,
            Path::new("/wt"),
            Role::Issue,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(plans[0].argv, vec!["nitro", "dev", "--port", "9100"]);
        assert_eq!(plans[0].env["PEER"], "http://localhost:9101");
    }

    #[test]
    fn plan_group_rejects_leftover_brace_port() {
        let mut catalog = HashMap::new();
        catalog.insert(
            "api".to_string(),
            test_app(&["nitro", "--port", "{port}"], &[]),
        );
        let ports: BTreeMap<String, u16> = [("api".to_string(), 9100)].into();
        let err = plan_group(
            &catalog,
            &["api".to_string()],
            &ports,
            None,
            Path::new("/wt"),
            Role::Issue,
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap_err();
        // {:#} prints the whole context chain; the hint lives in the root cause.
        assert!(format!("{err:#}").contains("retired"), "got: {err:#}");
    }
```

And in `strays/signature.rs` tests:

```rust
    #[test]
    fn drops_minijinja_placeholders() {
        let launch = v(&["nitro", "dev", "--port", "{{ port }}"]);
        assert_eq!(signature(&launch), v(&["nitro", "dev"]));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports --lib`
Expected: compile errors (plan_group arity/Result) and the signature test failing — `{{ port }}` currently survives the filter (RED).

- [ ] **Step 3: Implement in `run.rs`**

Delete `launch_argv` (:18-23). Add:

```rust
/// Ports one (role, holder) group needs: each selected app's own port plus
/// every app referenced via `ports[...]` in the group's launch argv and
/// static_env templates. One `registry::alloc` covers them all, so a
/// reference to an app that isn't running writes the normal pid-less
/// reservation a later `up` claims.
pub fn resolve_ports(
    catalog: &HashMap<String, App>,
    apps: &[String],
    holder: &str,
    role: Role,
    variables: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, u16>> {
    let mut names: Vec<String> = apps.to_vec();
    for a in apps {
        let app = &catalog[a];
        let mut templates: Vec<&str> = app.launch.iter().map(String::as_str).collect();
        templates.extend(app.static_env.values().map(String::as_str));
        let refs = devkit_common::template::referenced_ports(&templates, variables)
            .with_context(|| format!("scanning templates of app `{a}`"))?;
        for r in refs.apps {
            anyhow::ensure!(
                catalog.contains_key(&r),
                "app `{a}` references unknown app `{r}` via ports[...]"
            );
            if !names.contains(&r) {
                names.push(r);
            }
        }
    }
    let reqs: Vec<(String, u16)> = names
        .iter()
        .map(|n| (n.clone(), catalog[n].base_port))
        .collect();
    Ok(registry::alloc(holder, &reqs, role)?.into_iter().collect())
}
```

Rewrite `plan_group` (signature gains `variables`, returns `Result`):

```rust
#[allow(clippy::too_many_arguments)]
pub fn plan_group(
    catalog: &HashMap<String, App>,
    apps: &[String],
    ports: &BTreeMap<String, u16>,
    provider: Option<&str>,
    base_dir: &Path,
    role: Role,
    user_env: &BTreeMap<String, String>,
    variables: &BTreeMap<String, String>,
) -> Result<Vec<LaunchPlan>> {
    use devkit_common::template::render_launch;
    let provider_port = provider.and_then(|p| ports.get(p).copied());
    let mut plans = Vec::with_capacity(apps.len());
    for a in apps {
        let app = &catalog[a];
        let port = ports[a];
        let argv = app
            .launch
            .iter()
            .map(|t| render_launch(t, Some(port), ports, variables))
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("rendering launch argv of `{a}`"))?;
        let mut rendered = app.clone();
        rendered.static_env = app
            .static_env
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    render_launch(v, Some(port), ports, variables)
                        .with_context(|| format!("rendering static_env `{k}` of `{a}`"))?,
                ))
            })
            .collect::<Result<_>>()?;
        let cwd = base_dir.join(&app.path);
        let env = env_for(&rendered, provider_port, user_env);
        let log = paths::logs_dir()
            .join(holder_slug(base_dir.to_str().unwrap_or("wt")))
            .join(format!("{}-{}.log", role.as_str(), a));
        plans.push(LaunchPlan {
            app: a.clone(),
            port,
            argv,
            cwd,
            env,
            log,
        });
    }
    Ok(plans)
}
```

In `strays/signature.rs:25`, drop unrendered template tokens too:

```rust
    let mut words = cmd
        .iter()
        .filter(|t| !t.starts_with('-') && *t != "{port}" && !t.contains("{{"));
```

- [ ] **Step 4: Update the two callers**

`src/bin/devrun/main.rs` `cmd_up` (:528-543) — replace the `reqs`/`registry::alloc` pair with `resolve_ports`, and pass variables:

```rust
        let ports = run::resolve_ports(catalog, &apps, holder, *grp_role, &cfg.templates.variables)?;
        let plans = run::plan_group(
            catalog,
            &apps,
            &ports,
            provider.as_deref(),
            base_dir,
            *grp_role,
            &user,
            &cfg.templates.variables,
        )?;
```

`crates/devkit-mcp/src/devrun.rs` `up` (:79-98) — same substitution:

```rust
    let vars = &loaded.config.templates.variables;
    let ports = run::resolve_ports(catalog, &apps, &a.root, Role::Issue, vars)?;
    let plans = run::plan_group(
        catalog,
        &apps,
        &ports,
        provider.as_deref(),
        Path::new(&a.root),
        Role::Issue,
        &user,
        vars,
    )?;
```

(the now-unused `reqs`/`registry::alloc` lines and, in MCP, the `registry` import if it becomes unused, are removed).

- [ ] **Step 5: Migrate in-repo config fixtures**

Replace `"{port}"` with `"{{ port }}"` in:
- `crates/devkit-ports/src/config.rs` sample TOMLs (lines 437, 526, 543, 572)
- `crates/devkit-ports/src/strays/mod.rs:400` test fixture

(Leave `{port}` occurrences inside Rust `format!`/`panic!` strings — e.g. `tests/registry.rs`, `devkit-common/src/supervise.rs` — untouched; those are format placeholders, not config.)

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS. If any test still constructs `plan_group` with the old arity, fix it here.

- [ ] **Step 7: Full check + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/devkit-ports/src crates/devkit-mcp/src/devrun.rs src/bin/devrun/main.rs
git commit -m "feat(ports)!: render launch argv and static_env with minijinja"
```

(Body: `{port}` is retired for `{{ port }}`/`{{ ports['app'] }}`; rendering fails on leftover `{port}` so a missed migration errors instead of launching with a garbage arg.)

### Task 5: Migrate docs and the personal config

**Files:**
- Modify: `docs/configuration.md` (5 `{port}` occurrences + new template-context section)
- Modify: `README.md` (1 `{port}` occurrence)
- Modify: `~/.config/devkit/config.toml` (outside the repo — rollout step, not committed)

**Interfaces:**
- Consumes: the phase-2 render context (`port`, `ports`, `[templates.variables]`).
- Produces: doc examples and the live personal config use only minijinja placeholders.

- [ ] **Step 1: Update `docs/configuration.md`**

Replace each `{port}` in launch examples with `{{ port }}`. Where launch templating is described, document the context:

```markdown
Launch argv and `static_env` values are minijinja templates rendered per
launch with strict undefined handling:

- `{{ port }}` — the app's own allocated port.
- `{{ ports['other-app'] }}` — another app's port in this worktree, resolved
  from the port registry. Referencing an app that isn't running writes a
  normal pid-less reservation which a later `devrun up other-app` claims, so
  a consumer can bake the port before the server exists. A typo'd app name
  is a hard error.
- `[templates.variables]` constants are available by name.

The old `{port}` placeholder is retired; a leftover `{port}` in a rendered
value fails the launch with a migration hint.
```

- [ ] **Step 2: Update `README.md`**

Replace the single `{port}` launch example with `{{ port }}`.

- [ ] **Step 3: Migrate the personal config (rollout, not a commit)**

Read `~/.config/devkit/config.toml` fully, then:
- Replace every `"{port}"` argv element in `launch` arrays with `"{{ port }}"` (api, api-prod, and any other app using it).
- Add the self-wiring static_env to lab-os-prod (this is what deletes the manual `--env` on `up`):

```toml
[apps.lab-os-prod.static_env]
FOUNDRY_API_BASE_URL = "http://localhost:{{ ports['api-prod'] }}"
```

  (merge into the existing `[apps.lab-os-prod.static_env]` table if one exists — keep `WORKCELL_BLI_RUN_WORKFLOW_ID = "dummy"` and friends).
- **Do not edit, move, or reflow the graphify install comment block at the bottom of the file** (the lines starting `# graphify install command (claude, dont edit/touch these comments, i need them)`).

- [ ] **Step 4: Smoke-test the migrated config**

Run (from the monorepo checkout, e.g. `~/Git/adaptyv/monorepo`): `devrun up api --dry-run`
Expected: printed argv shows a concrete port (e.g. `--port 9100`), no `{port}` or `{{ port }}` literals. Note: this uses the *installed* devrun; run `cargo install --path .` from the worktree first, or invoke the freshly built `target/release/devrun`.

- [ ] **Step 5: Commit the doc changes**

```bash
git add docs/configuration.md README.md
git commit -m "docs: migrate launch examples to minijinja port templating"
```

---

## Phase 3 — `[tasks]` + `devrun task`

### Task 6: `[tasks]` config model

**Files:**
- Modify: `crates/devkit-ports/src/config.rs`

**Interfaces:**
- Produces (used by Tasks 8-9):
  - `pub enum Step { Task(String), Up(String) }` — serde externally-tagged, lowercase, so TOML `{ task = "x" }` / `{ up = "api" }` round-trip.
  - `pub struct TaskConfig { pub description: Option<String>, pub app: Option<String>, pub run: Vec<String>, pub steps: Vec<Step>, pub env: BTreeMap<String, String> }`
  - `Config` gains `#[serde(default)] pub tasks: HashMap<String, TaskConfig>`.

- [ ] **Step 1: Write the failing tests** (in `config.rs`'s `mod tests`)

```rust
    #[test]
    fn tasks_parse_command_and_sequence() {
        let src = r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "~/tmp/baseline"

[tasks.api-prod-build]
description = "prod nitro build"
app = "api-prod"
run = ["doppler", "run", "-c", "dev_local", "--", "bun", "nitro", "build"]
env = { NITRO_PRESET = "node-server" }

[tasks.profile-lab-os]
steps = [
  { task = "api-prod-build" },
  { up = "api-prod" },
]
"#;
        let c = Config::parse(src).unwrap();
        let t = &c.tasks["api-prod-build"];
        assert_eq!(t.app.as_deref(), Some("api-prod"));
        assert_eq!(t.run[0], "doppler");
        assert_eq!(t.env["NITRO_PRESET"], "node-server");
        assert!(t.steps.is_empty());
        let s = &c.tasks["profile-lab-os"];
        assert_eq!(
            s.steps,
            vec![
                Step::Task("api-prod-build".to_string()),
                Step::Up("api-prod".to_string())
            ]
        );
        assert!(s.run.is_empty());
    }

    #[test]
    fn tasks_roundtrip_through_toml() {
        let src = "[defaults]\nworktree_root = \"w\"\nbranch_prefix = \"x/\"\n\
                   baseline_ref = \"m\"\nbaseline_path = \"b\"\n\
                   [tasks.t]\nrun = [\"git\", \"version\"]\n\
                   [tasks.s]\nsteps = [{ task = \"t\" }, { up = \"api\" }]\n";
        let c = Config::parse(src).unwrap();
        let out = toml::to_string(&c).expect("serialize config with tasks");
        let c2 = Config::parse(&out).unwrap();
        assert_eq!(c2.tasks["s"].steps, c.tasks["s"].steps);
        assert_eq!(c2.tasks["t"].run, c.tasks["t"].run);
    }

    #[test]
    fn tasks_absent_is_empty() {
        let c = Config::parse(tests_sample()).unwrap();
        assert!(c.tasks.is_empty());
    }

    #[test]
    fn tasks_merge_across_layers() {
        let base = tbl("[tasks.build]\nrun = ['git', 'version']\n[tasks.build.env]\nA = '1'\n");
        let top = tbl("[tasks.build.env]\nA = '9'\nB = '2'\n");
        let (m, _) = merge_layers(&[(PathBuf::from("/b"), base), (PathBuf::from("/t"), top)]);
        let t = &m["tasks"]["build"];
        assert_eq!(t["run"][0].as_str(), Some("git"));
        assert_eq!(t["env"]["A"].as_str(), Some("9"));
        assert_eq!(t["env"]["B"].as_str(), Some("2"));
    }
```

(`tbl` and `merge_layers` are the existing helpers used by `nested_maps_merge_per_key` in the same tests module.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports --lib config`
Expected: compile error — `tasks`/`Step`/`TaskConfig` not found (RED).

- [ ] **Step 3: Implement**

In `config.rs`, after `PrepFile`:

```rust
/// One step of a sequence task: run a sibling command task, or bring an app up.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Task(String),
    Up(String),
}

/// A canned oneshot invoked by name via `devrun task`: either a command
/// (`run`, optionally scoped to an `app` for cwd + static_env) or a sequence
/// (`steps`). Exactly one of `run`/`steps` must be set; a sequence task
/// carries no `app`/`env`. Validated at resolution, not at parse.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct TaskConfig {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub run: Vec<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}
```

And on `Config` (keep table-like fields last, matching the existing layout comment):

```rust
    #[serde(default)]
    pub tasks: HashMap<String, TaskConfig>,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports --lib config`
Expected: PASS. (Layer merging needs no code: `merge_layers` already deep-merges `[tasks.<name>]` tables key-by-key and replaces non-table values wholesale, same as `[apps]`.)

- [ ] **Step 5: Full check + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/devkit-ports/src/config.rs
git commit -m "feat(ports): parse [tasks] config table"
```

### Task 7: `assert_not_prd` takes argv/env/cwd directly

**Files:**
- Modify: `crates/devkit-ports/src/run.rs` (`assert_not_prd` :97, its call in `launch`, its tests)

**Interfaces:**
- Produces: `pub fn assert_not_prd(label: &str, argv: &[String], env: &BTreeMap<String, String>, cwd: &Path) -> Result<()>` — same guard, no `LaunchPlan` required, so task resolution (Task 8) can call it without fabricating a plan.

- [ ] **Step 1: Change the signature and callers**

```rust
/// Refuse to run a Doppler invocation against the `prd` config. `label` names
/// the app or task in the error. A non-doppler `argv` is unguarded. The config
/// is resolved in Doppler's own precedence order — explicit flag, then
/// `DOPPLER_CONFIG`, then the local scope for `cwd` — and an invocation whose
/// config resolves to `prd`, or cannot be resolved at all, is rejected
/// (fail-safe).
pub fn assert_not_prd(
    label: &str,
    argv: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<()> {
    let prog = argv.first().map(String::as_str).unwrap_or_default();
    if Path::new(prog).file_name().and_then(|s| s.to_str()) != Some("doppler") {
        return Ok(());
    }
    let config = config_from_argv_env(argv, env).or_else(|| doppler_scoped_config(cwd));
    match config.as_deref() {
        Some("prd") => anyhow::bail!(
            "refusing to launch `{label}`: doppler config resolves to `prd` (production secrets)"
        ),
        Some(_) => Ok(()),
        None => anyhow::bail!(
            "refusing to launch `{label}`: cannot determine its doppler config (no -c/--config, \
             no DOPPLER_CONFIG, no local scope). Add an explicit `-c <config>` to its launch."
        ),
    }
}
```

In `launch` (:389-391) the guard loop becomes:

```rust
    for p in plans {
        assert_not_prd(&p.app, &p.argv, &p.env, &p.cwd)?;
    }
```

Update the existing `assert_not_prd` tests in `run.rs` to pass the four fields their `LaunchPlan` fixtures carried (`&plan.app, &plan.argv, &plan.env, &plan.cwd` — or construct the fields directly and drop the plan fixture).

- [ ] **Step 2: Verify + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/devkit-ports/src/run.rs
git commit -m "refactor(ports): pass argv/env/cwd to assert_not_prd directly"
```

### Task 8: `devkit_ports::task` — resolve, list, exec

**Files:**
- Create: `crates/devkit-ports/src/task.rs`
- Modify: `crates/devkit-ports/src/lib.rs` (add `pub mod task;`)

**Interfaces:**
- Consumes: `config::{Config, Step, TaskConfig}`, `apps::App`, `template::{referenced_ports, render_launch}`, `registry::{alloc, Role}`, `run::assert_not_prd` (Task 7 signature).
- Produces (used by Task 9):
  - `pub struct CommandPlan { pub name: String, pub argv: Vec<String>, pub cwd: PathBuf, pub env: BTreeMap<String, String> }`
  - `pub enum SeqItem { Run(CommandPlan), Up(String) }`
  - `pub enum Resolved { Command(CommandPlan), Sequence(Vec<SeqItem>) }`
  - `pub struct TaskRow { pub name: String, pub kind: &'static str, pub app: String, pub description: String }`
  - `pub fn list(cfg: &Config) -> Vec<TaskRow>` (sorted by name)
  - `pub fn resolve(cfg: &Config, catalog: &HashMap<String, App>, worktree_root: &Path, holder: &str, name: &str, user_env: &BTreeMap<String, String>) -> Result<Resolved>`
  - `pub fn exec(plan: &CommandPlan) -> Result<std::process::ExitStatus>` — foreground, inherited stdio, env overlaid on the process environment.

- [ ] **Step 1: Write the failing tests** (in `task.rs`'s `mod tests`; the registry-free seam `resolve_command_with_ports` keeps unit tests off the real registry file)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Step, TaskConfig};

    fn cfg_with(tasks: &[(&str, TaskConfig)]) -> Config {
        let mut c = Config::default();
        for (n, t) in tasks {
            c.tasks.insert(n.to_string(), t.clone());
        }
        c
    }

    fn command_task(app: Option<&str>, run: &[&str], env: &[(&str, &str)]) -> TaskConfig {
        TaskConfig {
            app: app.map(String::from),
            run: run.iter().map(|s| s.to_string()).collect(),
            env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..TaskConfig::default()
        }
    }

    fn api_catalog() -> HashMap<String, App> {
        let mut m = HashMap::new();
        m.insert(
            "api-prod".to_string(),
            App {
                name: "api-prod".into(),
                base_port: 9101,
                path: "apps/api".into(),
                launch: vec![],
                url_env: None,
                provides_url: false,
                static_env: [("FROM_APP".to_string(), "static".to_string())].into(),
                prep_files: vec![],
                setup: vec![],
            },
        );
        m
    }

    #[test]
    fn command_env_layering_static_then_task_then_user() {
        let t = command_task(Some("api-prod"), &["git", "version"], &[("FROM_APP", "task")]);
        let user: BTreeMap<String, String> = [("FROM_APP".to_string(), "user".to_string())].into();
        let plan = resolve_command_with_ports(
            "t", &t, &api_catalog()["api-prod"].static_env, Path::new("/wt"),
            Some("apps/api"), &BTreeMap::new(), None, &BTreeMap::new(), &user,
        )
        .unwrap();
        assert_eq!(plan.env["FROM_APP"], "user");
        assert_eq!(plan.cwd, Path::new("/wt").join("apps/api"));

        let plan2 = resolve_command_with_ports(
            "t", &t, &api_catalog()["api-prod"].static_env, Path::new("/wt"),
            Some("apps/api"), &BTreeMap::new(), None, &BTreeMap::new(), &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(plan2.env["FROM_APP"], "task");
    }

    #[test]
    fn command_renders_ports_in_env_and_argv() {
        let t = command_task(
            None,
            &["git", "--url", "http://localhost:{{ ports['api-prod'] }}"],
            &[("BASE", "http://localhost:{{ ports['api-prod'] }}")],
        );
        let ports: BTreeMap<String, u16> = [("api-prod".to_string(), 9101)].into();
        let plan = resolve_command_with_ports(
            "t", &t, &HashMap::new(), Path::new("/wt"), None, &ports, None,
            &BTreeMap::new(), &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(plan.argv[2], "http://localhost:9101");
        assert_eq!(plan.env["BASE"], "http://localhost:9101");
        assert_eq!(plan.cwd, Path::new("/wt"));
    }

    #[test]
    fn command_prd_doppler_is_rejected() {
        let t = command_task(None, &["doppler", "run", "-c", "prd", "--", "x"], &[]);
        let err = resolve_command_with_ports(
            "t", &t, &HashMap::new(), Path::new("/wt"), None, &BTreeMap::new(), None,
            &BTreeMap::new(), &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("prd"));
    }

    #[test]
    fn resolve_rejects_bad_shapes() {
        let both = TaskConfig {
            run: vec!["git".into()],
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let neither = TaskConfig::default();
        let seq_with_app = TaskConfig {
            app: Some("api-prod".into()),
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let step_to_sequence = TaskConfig {
            steps: vec![Step::Task("seq".into())],
            ..TaskConfig::default()
        };
        let seq = TaskConfig {
            steps: vec![Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let cfg = cfg_with(&[
            ("both", both),
            ("neither", neither),
            ("seq-with-app", seq_with_app),
            ("nested", step_to_sequence),
            ("seq", seq),
        ]);
        let cat = api_catalog();
        let u = BTreeMap::new();
        for bad in ["both", "neither", "seq-with-app", "nested", "missing"] {
            assert!(
                resolve(&cfg, &cat, Path::new("/wt"), "/wt", bad, &u).is_err(),
                "task `{bad}` must fail validation"
            );
        }
    }

    #[test]
    fn resolve_sequence_maps_steps() {
        let build = command_task(None, &["git", "version"], &[]);
        let seq = TaskConfig {
            steps: vec![Step::Task("build".into()), Step::Up("api-prod".into())],
            ..TaskConfig::default()
        };
        let cfg = cfg_with(&[("build", build), ("seq", seq)]);
        let r = resolve(&cfg, &api_catalog(), Path::new("/wt"), "/wt", "seq", &BTreeMap::new())
            .unwrap();
        match r {
            Resolved::Sequence(items) => {
                assert!(matches!(&items[0], SeqItem::Run(p) if p.argv == ["git", "version"]));
                assert!(matches!(&items[1], SeqItem::Up(a) if a == "api-prod"));
            }
            _ => panic!("expected sequence"),
        }
    }

    #[test]
    fn list_reports_kind_and_sorts() {
        let cfg = cfg_with(&[
            ("b-cmd", command_task(None, &["git"], &[])),
            (
                "a-seq",
                TaskConfig {
                    description: Some("d".into()),
                    steps: vec![Step::Up("api-prod".into())],
                    ..TaskConfig::default()
                },
            ),
        ]);
        let rows = list(&cfg);
        assert_eq!(rows[0].name, "a-seq");
        assert_eq!(rows[0].kind, "sequence");
        assert_eq!(rows[0].description, "d");
        assert_eq!(rows[1].kind, "command");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-ports --lib task`
Expected: compile error — module doesn't exist (RED).

- [ ] **Step 3: Implement `task.rs`**

```rust
//! Canned oneshot tasks (`[tasks]`): resolve a named task against the config,
//! app catalog, and port registry into runnable plans, and execute command
//! plans in the foreground. Shared seam for the `devrun task` CLI (and any
//! future MCP surface).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};
use devkit_common::template;

use crate::apps::App;
use crate::config::{Config, Step, TaskConfig};
use crate::registry::{self, Role};
use crate::run;

/// A command task resolved to a runnable process: rendered argv, cwd, env.
#[derive(Debug, Clone)]
pub struct CommandPlan {
    pub name: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// A resolved sequence step.
#[derive(Debug, Clone)]
pub enum SeqItem {
    Run(CommandPlan),
    Up(String),
}

/// A named task resolved for execution.
#[derive(Debug, Clone)]
pub enum Resolved {
    Command(CommandPlan),
    Sequence(Vec<SeqItem>),
}

/// One row for the `devrun task` listing.
pub struct TaskRow {
    pub name: String,
    pub kind: &'static str,
    pub app: String,
    pub description: String,
}

/// Configured tasks sorted by name. `kind` reflects the shape on disk; an
/// invalid shape (both or neither of `run`/`steps`) is listed as `invalid`
/// rather than hidden, so a typo is visible in the listing.
pub fn list(cfg: &Config) -> Vec<TaskRow> {
    let mut rows: Vec<TaskRow> = cfg
        .tasks
        .iter()
        .map(|(name, t)| TaskRow {
            name: name.clone(),
            kind: match (!t.run.is_empty(), !t.steps.is_empty()) {
                (true, false) => "command",
                (false, true) => "sequence",
                _ => "invalid",
            },
            app: t.app.clone().unwrap_or_else(|| "-".into()),
            description: t.description.clone().unwrap_or_default(),
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

/// Resolve task `name` for execution in `worktree_root`. Command tasks get
/// their port references allocated (issue role, pid-less reservations for
/// apps not yet running) and their templates rendered; sequence tasks
/// validate and resolve each step. All validation errors fire here, before
/// anything spawns.
pub fn resolve(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    user_env: &BTreeMap<String, String>,
) -> Result<Resolved> {
    let t = cfg
        .tasks
        .get(name)
        .ok_or_else(|| anyhow!("unknown task `{name}` (run `devrun task` to list)"))?;
    match (!t.run.is_empty(), !t.steps.is_empty()) {
        (true, true) => bail!("task `{name}` sets both `run` and `steps`"),
        (false, false) => bail!("task `{name}` sets neither `run` nor `steps`"),
        (true, false) => Ok(Resolved::Command(resolve_command(
            cfg, catalog, worktree_root, holder, name, t, user_env,
        )?)),
        (false, true) => {
            ensure!(
                t.app.is_none() && t.env.is_empty(),
                "sequence task `{name}` may only set `description` and `steps`"
            );
            let mut items = Vec::with_capacity(t.steps.len());
            for step in &t.steps {
                match step {
                    Step::Task(r) => {
                        let sub = cfg.tasks.get(r).ok_or_else(|| {
                            anyhow!("task `{name}` references unknown task `{r}`")
                        })?;
                        ensure!(
                            !sub.run.is_empty() && sub.steps.is_empty(),
                            "task `{name}` references `{r}`, which is not a command task \
                             (sequences cannot nest)"
                        );
                        items.push(SeqItem::Run(resolve_command(
                            cfg, catalog, worktree_root, holder, r, sub, user_env,
                        )?));
                    }
                    Step::Up(app) => {
                        ensure!(
                            catalog.contains_key(app),
                            "task `{name}` brings up unknown app `{app}`"
                        );
                        items.push(SeqItem::Up(app.clone()));
                    }
                }
            }
            Ok(Resolved::Sequence(items))
        }
    }
}

/// Discovery + allocation for one command task, then delegate to the pure
/// renderer. `ports[...]` references and (if `{{ port }}` is used) the task's
/// own app are allocated in one `registry::alloc` call.
fn resolve_command(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    worktree_root: &Path,
    holder: &str,
    name: &str,
    t: &TaskConfig,
    user_env: &BTreeMap<String, String>,
) -> Result<CommandPlan> {
    let app = t
        .app
        .as_deref()
        .map(|a| {
            catalog
                .get(a)
                .ok_or_else(|| anyhow!("task `{name}` names unknown app `{a}`"))
        })
        .transpose()?;
    let static_env = app.map(|a| a.static_env.clone()).unwrap_or_default();
    let vars = &cfg.templates.variables;

    let mut templates: Vec<&str> = t.run.iter().map(String::as_str).collect();
    templates.extend(t.env.values().map(String::as_str));
    templates.extend(static_env.values().map(String::as_str));
    let refs = template::referenced_ports(&templates, vars)
        .with_context(|| format!("scanning templates of task `{name}`"))?;

    ensure!(
        !refs.own_port || app.is_some(),
        "task `{name}` uses `{{{{ port }}}}` but has no `app`"
    );
    let mut names: Vec<String> = refs.apps.iter().cloned().collect();
    for r in &names {
        ensure!(
            catalog.contains_key(r),
            "task `{name}` references unknown app `{r}` via ports[...]"
        );
    }
    if refs.own_port {
        let own = app.expect("checked above").name.clone();
        if !names.contains(&own) {
            names.push(own);
        }
    }
    let ports: BTreeMap<String, u16> = if names.is_empty() {
        BTreeMap::new()
    } else {
        let reqs: Vec<(String, u16)> = names
            .iter()
            .map(|n| (n.clone(), catalog[n].base_port))
            .collect();
        registry::alloc(holder, &reqs, Role::Issue)?.into_iter().collect()
    };
    let own_port = refs
        .own_port
        .then(|| ports[&app.expect("checked above").name]);

    resolve_command_with_ports(
        name,
        t,
        &static_env,
        worktree_root,
        app.map(|a| a.path.as_str()),
        &ports,
        own_port,
        vars,
        user_env,
    )
}

/// Render one command task against an already-resolved port map. Registry-free
/// so tests exercise rendering, layering, and the prd guard directly.
#[allow(clippy::too_many_arguments)]
fn resolve_command_with_ports(
    name: &str,
    t: &TaskConfig,
    static_env: &HashMap<String, String>,
    worktree_root: &Path,
    app_path: Option<&str>,
    ports: &BTreeMap<String, u16>,
    own_port: Option<u16>,
    variables: &BTreeMap<String, String>,
    user_env: &BTreeMap<String, String>,
) -> Result<CommandPlan> {
    let argv = t
        .run
        .iter()
        .map(|s| template::render_launch(s, own_port, ports, variables))
        .collect::<Result<Vec<_>>>()
        .with_context(|| format!("rendering `run` of task `{name}`"))?;
    ensure!(
        argv.first().is_some_and(|p| !p.is_empty()),
        "task `{name}` has an empty program"
    );

    let mut env = BTreeMap::new();
    for (k, v) in static_env {
        env.insert(
            k.clone(),
            template::render_launch(v, own_port, ports, variables)
                .with_context(|| format!("rendering static_env `{k}` for task `{name}`"))?,
        );
    }
    for (k, v) in &t.env {
        env.insert(
            k.clone(),
            template::render_launch(v, own_port, ports, variables)
                .with_context(|| format!("rendering env `{k}` of task `{name}`"))?,
        );
    }
    for (k, v) in user_env {
        env.insert(k.clone(), v.clone());
    }

    let cwd = match app_path {
        Some(p) => worktree_root.join(p),
        None => worktree_root.to_path_buf(),
    };
    run::assert_not_prd(name, &argv, &env, &cwd)?;
    Ok(CommandPlan {
        name: name.into(),
        argv,
        cwd,
        env,
    })
}

/// Run a command plan in the foreground with inherited stdio, its env overlaid
/// on the process environment. Returns the child's exit status.
pub fn exec(plan: &CommandPlan) -> Result<std::process::ExitStatus> {
    std::process::Command::new(&plan.argv[0])
        .args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .envs(&plan.env)
        .status()
        .with_context(|| format!("running task `{}` ({})", plan.name, plan.argv.join(" ")))
}
```

Add `pub mod task;` to `crates/devkit-ports/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-ports --lib task`
Expected: all PASS. (`command_prd_doppler_is_rejected` exercises `config_from_argv_env` only — the `-c prd` flag resolves without invoking the doppler CLI.)

- [ ] **Step 5: Full check + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/devkit-ports/src/task.rs crates/devkit-ports/src/lib.rs
git commit -m "feat(ports): resolve and exec canned tasks"
```

### Task 9: `devrun task` subcommand

**Files:**
- Modify: `src/bin/devrun/main.rs` (Cmd enum :53-137, main dispatch :366+, new `cmd_task`)
- Create: `tests/task_cmd.rs`

**Interfaces:**
- Consumes: `devkit_ports::task::{list, resolve, exec, Resolved, SeqItem, CommandPlan}`, existing `cmd_up`, `UpFlags`, `parse_user_env`, `toplevel`, `ui::table`.
- Produces: `devrun task` (list), `devrun task <name> [--env K=V]... [--dry-run]`; a failing step exits the process with the child's exit code; `up` steps reuse `cmd_up` (issue role, no supervise) and are no-ops for live servers per phase 1.

- [ ] **Step 1: Write the failing integration test** (`tests/task_cmd.rs`)

```rust
//! `devrun task` end-to-end: listing, dry-run rendering with a registry-
//! allocated port, execution, and exit-code propagation. Uses an isolated
//! HOME/XDG_STATE_HOME so the port registry never touches the real one.

use std::path::Path;
use std::process::Command;

fn devrun() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devrun"))
}

/// A temp dir that is a git repo (cmd_task resolves the worktree root) with a
/// devkit.toml defining one app and three tasks.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    std::fs::write(
        root.join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "b"

[apps.api]
base_port = 39140
path = "."
launch = ["git", "version"]

[tasks.hello]
description = "prints git version"
run = ["git", "version"]

[tasks.show-port]
run = ["git", "--url=http://localhost:{{ ports['api'] }}", "version"]

[tasks.fail]
run = ["git", "definitely-not-a-subcommand"]
"#,
    )
    .expect("write devkit.toml");
    dir
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    let state = dir.join("state");
    devrun()
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("LOCALAPPDATA", &state) // Windows: keep the registry off the real one
        .env("USERPROFILE", dir) // Windows: keep config resolution off the real home
        .output()
        .expect("run devrun")
}

#[test]
fn task_lists_names_and_descriptions() {
    let dir = setup();
    let out = run_in(dir.path(), &["task"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("hello"), "listing missing task name: {stdout}");
    assert!(stdout.contains("prints git version"), "listing missing description: {stdout}");
}

#[test]
fn task_dry_run_renders_allocated_port() {
    let dir = setup();
    let out = run_in(dir.path(), &["task", "show-port", "--dry-run"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("http://localhost:391"),
        "dry-run must show a rendered port at/above base 39140: {stdout}"
    );
    assert!(!stdout.contains("{{"), "no unrendered templates in dry-run: {stdout}");
}

#[test]
fn task_runs_and_propagates_exit_codes() {
    let dir = setup();
    let ok = run_in(dir.path(), &["task", "hello"]);
    assert!(ok.status.success(), "{ok:?}");

    let bad = run_in(dir.path(), &["task", "fail"]);
    assert!(!bad.status.success(), "failing task must exit non-zero");

    let missing = run_in(dir.path(), &["task", "nope"]);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("unknown task"),
        "{missing:?}"
    );
}
```

If `tempfile` is not already a dev-dependency of the root package, add it to `[dev-dependencies]` in the root `Cargo.toml` (it is already in the workspace dependency graph).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test task_cmd`
Expected: FAIL — `devrun task` is an unknown subcommand (clap error, non-zero exit on the list test) (RED).

- [ ] **Step 3: Implement the subcommand**

Add to `enum Cmd` (after `Logs`):

```rust
    /// Run a canned task from `[tasks]` (no name: list the configured tasks).
    Task {
        name: Option<String>,
        #[arg(long = "env", value_name = "K=V")]
        env: Vec<String>,
        /// Print the rendered plan (cwd, argv, env, resolved ports) without executing.
        #[arg(long)]
        dry_run: bool,
    },
```

Dispatch arm in `main` (next to the `Cmd::Up` arm, matching its style):

```rust
        Cmd::Task { name, env, dry_run } => cmd_task(&cli, &cwd, name.as_deref(), env, *dry_run),
```

Implementation (near `cmd_up`):

```rust
fn cmd_task(
    cli: &Cli,
    cwd: &str,
    name: Option<&str>,
    env_pairs: &[String],
    dry_run: bool,
) -> Result<()> {
    use devkit_ports::task::{self, Resolved, SeqItem};

    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
    let Some(name) = name else {
        let rows = task::list(&loaded.config);
        if rows.is_empty() {
            println!("no tasks configured (add [tasks.<name>] to devkit.toml)");
            return Ok(());
        }
        let mut t = ui::table(&["NAME", "KIND", "APP", "DESCRIPTION"]);
        for r in rows {
            t.add_row(vec![r.name, r.kind.to_string(), r.app, r.description]);
        }
        print!("{t}");
        return Ok(());
    };

    let user = parse_user_env(env_pairs, None)?;
    let root = toplevel(cwd)?;
    let resolved = task::resolve(
        &loaded.config,
        &loaded.catalog,
        Path::new(&root),
        &root,
        name,
        &user,
    )?;
    match resolved {
        Resolved::Command(plan) => run_task_step(&plan, dry_run),
        Resolved::Sequence(items) => {
            for item in &items {
                match item {
                    SeqItem::Run(plan) => run_task_step(plan, dry_run)?,
                    SeqItem::Up(app) => {
                        if dry_run {
                            println!("up {app}");
                        } else {
                            cmd_up(
                                cli,
                                cwd,
                                std::slice::from_ref(app),
                                RoleSelector::Issue,
                                &[],
                                None,
                                UpFlags {
                                    dry_run: false,
                                    supervise: false,
                                },
                            )?;
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

/// Print or execute one command step. A non-zero child exits the process with
/// the child's code, so a sequence stops at its first failure.
fn run_task_step(plan: &devkit_ports::task::CommandPlan, dry_run: bool) -> Result<()> {
    if dry_run {
        println!("[{}]", plan.name);
        println!("  cwd:  {}", plan.cwd.display());
        println!("  argv: {}", plan.argv.join(" "));
        let envs: Vec<String> = plan.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("  env:  {}", envs.join(" "));
        return Ok(());
    }
    eprintln!("→ {}: {}", plan.name, plan.argv.join(" "));
    let status = devkit_ports::task::exec(plan)?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test task_cmd && cargo test --test completions`
Expected: PASS (completions picks up the new subcommand automatically via clap).

- [ ] **Step 5: Full check + commit**

```bash
cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add src/bin/devrun/main.rs tests/task_cmd.rs Cargo.toml
git commit -m "feat(devrun): add task subcommand for canned oneshots"
```

### Task 10: Document `[tasks]`; add the profiling tasks to the personal config

**Files:**
- Modify: `docs/configuration.md` (new `[tasks]` section)
- Modify: `README.md` (devrun section: mention `devrun task`)
- Modify: `AGENTS.md` (Layout table: `devrun` row mentions `task`; add `task` module to the devkit-ports row)
- Modify: `~/.config/devkit/config.toml` (rollout, not committed)

- [ ] **Step 1: `docs/configuration.md` — add after the apps section**

````markdown
## Tasks

`[tasks.<name>]` defines canned oneshots run by `devrun task <name>`
(`devrun task` lists them). A task is either a **command** (`run`) or a
**sequence** (`steps`), never both.

```toml
[tasks.api-prod-build]
description = "prod nitro build (node-server preset)"
app = "api-prod"        # run in this app's dir, inherit its static_env
run = ["doppler", "run", "-p", "api-foundry", "-c", "dev_local",
       "--preserve-env=NITRO_PRESET", "--", "bun", "nitro", "build"]
env = { NITRO_PRESET = "node-server" }

[tasks.profile-lab-os]
description = "prod api + profiled lab-os, wired together"
steps = [
  { task = "api-prod-build" },
  { up = "api-prod" },
  { task = "lab-os-build" },
  { up = "lab-os-prod" },
]
```

- Command tasks run in the foreground with inherited stdio; the exit code is
  propagated. `run` and `env` values are minijinja templates with the same
  `port`/`ports` context as launches; `{{ ports['x'] }}` resolves from the
  port registry (issue role), writing a pid-less reservation when `x` isn't
  running. Env layering, low to high: app `static_env` → task `env` → CLI
  `--env`. Tasks do not get `url_env` provider wiring — reference the app
  you need explicitly via `ports[...]`. Doppler invocations go through the
  same `prd` guard as launches.
- Sequence steps run in order and stop at the first failure. `{ up = "app" }`
  is `devrun up app` (a no-op for a live server). A sequence may only set
  `description` and `steps`, and cannot reference another sequence.
- `devrun task <name> --dry-run` prints each rendered plan (still resolving
  ports, so the printed values are real) without executing.
````

- [ ] **Step 2: `README.md`** — in the devrun CLI summary, add one line:

```markdown
- `devrun task [<name>] [--env K=V] [--dry-run]` — run a canned `[tasks]` oneshot or sequence (no name lists them).
```

- [ ] **Step 3: `AGENTS.md`** — in the Layout table, extend the `src/bin/devrun` row's verb list with `task`, and the `crates/devkit-ports` row's module list with `task` (canned oneshot resolution/exec).

- [ ] **Step 4: Personal config rollout (not committed)**

Append to `~/.config/devkit/config.toml` (without touching the graphify comment block):

```toml
[tasks.api-prod-build]
description = "prod nitro build (repo pins the vercel preset; override to node-server)"
app = "api-prod"
run = ["doppler", "run", "-p", "api-foundry", "-c", "dev_local", "--preserve-env=NITRO_PRESET", "--", "bun", "nitro", "build"]
env = { NITRO_PRESET = "node-server" }

[tasks.lab-os-build]
description = "profiled next build; api URL baked in at build time"
app = "lab-os-prod"
run = ["doppler", "run", "-p", "lab-os", "-c", "dev_local", "--", "bun", "next", "build", "--profile"]
env = { WORKCELL_BLI_RUN_WORKFLOW_ID = "asy-bli-run/v0.0.8", FOUNDRY_API_BASE_URL = "http://localhost:{{ ports['api-prod'] }}" }

[tasks.profile-lab-os]
description = "prod api + profiled lab-os, wired together"
steps = [
  { task = "api-prod-build" },
  { up = "api-prod" },
  { task = "lab-os-build" },
  { up = "lab-os-prod" },
]
```

Cross-check each `run`/`env` against the flow comments already in that file (they are the source of truth for the doppler project names and the WORKCELL id) and trim the now-redundant how-to comments if their content is fully captured by the tasks — keep any nuance the tasks don't encode.

- [ ] **Step 5: Smoke-test + commit docs**

Run (from the monorepo checkout, with the new devrun installed): `devrun task` and `devrun task profile-lab-os --dry-run`
Expected: listing shows the three tasks; dry-run shows four steps with rendered ports and no template literals.

```bash
git add docs/configuration.md README.md AGENTS.md
git commit -m "docs: document [tasks] and devrun task"
```

### Task 11: Final gate

- [ ] **Step 1: Full verification**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p devkit-ports --test registry
```

Expected: all green. Fix anything that isn't, amend into the responsible commit if trivial, otherwise a `fix:` commit.

- [ ] **Step 2: Land**

Fast-forward `main` from outside its worktree per AGENTS.md (`git -C <primary> merge --ff-only feat/devrun-tasks` after switching the primary to `main`), then `git worktree remove` the feature worktree. Do not push or open a PR unless asked.
