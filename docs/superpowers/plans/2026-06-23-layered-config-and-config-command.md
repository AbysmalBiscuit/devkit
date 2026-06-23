# Layered config resolution + `devrun config` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve `devkit.toml` by deep-merging every file from cwd up to root plus the home config (deepest wins per value, `[config] root = true` stops the walk), and add a `devrun config` subcommand that shows the effective config (with optional per-value origin) and lists configured apps.

**Architecture:** A new `config::resolve` parses each layer to a `toml::Table`, deep-merges them (tables merge key-by-key; scalars/arrays replace wholesale) while recording per-leaf provenance, then deserializes the merged table into the existing `Config`. `load::load` routes through it, so every binary inherits layering. `devrun config` formats `Config` + `Provenance`.

**Tech Stack:** Rust 2024, `toml` 0.8, `serde`, `serde_json`, `clap`, `anyhow`.

**Spec:** `docs/superpowers/specs/2026-06-23-layered-config-and-config-command-design.md`

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/devkit-ports/src/config.rs` | config types, layer discovery, deep-merge, provenance, flatten | Modify |
| `crates/devkit-ports/src/load.rs` | funnel: resolve config → build catalog | Modify (route through `resolve`, carry provenance) |
| `src/bin/devrun/main.rs` | devrun CLI; add `config` subcommand + dispatch | Modify |
| `src/bin/devrun/config.rs` | `config show` / `config apps` formatting | Create |
| `README.md` | document `devrun config` and layered resolution | Modify |

All work happens in the worktree `../devkit-worktrees/config-layering` (branch `lev/config-layering`). Run commands from that directory.

---

## Task 1: Make config types serializable (and serialization-safe)

`config show` serializes `Config` to TOML/JSON. The `toml` serializer requires every struct to emit scalar/array fields *before* table (map) fields, so `AppConfig`'s map fields must move last.

**Files:**
- Modify: `crates/devkit-ports/src/config.rs:1-2` (imports), `:6-124` (derives + `AppConfig` field order)

- [ ] **Step 1: Write the failing test** — append to the `tests` module in `crates/devkit-ports/src/config.rs` (before the final `}`):

```rust
    #[test]
    fn config_roundtrips_through_toml_serialization() {
        let c = Config::parse(SAMPLE).unwrap();
        let s = toml::to_string_pretty(&c).expect("serialize config to toml");
        let c2 = Config::parse(&s).expect("reparse serialized config");
        assert_eq!(c2.apps["api"].base_port, 9100);
        assert_eq!(c2.defaults.branch_prefix, "lev/");
    }

    #[test]
    fn config_serializes_app_with_static_env_and_trailing_scalars() {
        // An app with a map field (static_env) AND scalar/array fields must
        // serialize without the toml "values before tables" error.
        let src = format!("{SAMPLE}setup = [[\"bun\", \"install\"]]\npath = \"apps/api\"\n");
        let c = Config::parse(&src).unwrap();
        let s = toml::to_string_pretty(&c).expect("serialize app with trailing scalars");
        assert!(s.contains("setup"));
        assert!(s.contains("path"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-ports config_roundtrips config_serializes`
Expected: FAIL — `Config` does not implement `Serialize` (compile error: `to_string_pretty` bound not satisfied).

- [ ] **Step 3: Add `Serialize` derives and reorder `AppConfig` fields**

In `crates/devkit-ports/src/config.rs`, change the import line:

```rust
use serde::{Deserialize, Serialize};
```

Add `Serialize` to each config struct derive. Change every `#[derive(Debug, Deserialize)]` on `Config`, `DaemonConfig`, `Defaults`, `Person`, and `AppConfig` to `#[derive(Debug, Deserialize, Serialize)]`.

Then reorder `AppConfig` so the two `HashMap` fields (`static_env`, `prep_env`) come **last**, after all scalar/array fields. Replace the struct body (`crates/devkit-ports/src/config.rs:101-124`) with:

```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct AppConfig {
    pub base_port: u16,
    pub launch: Vec<String>,
    #[serde(default)]
    pub url_env: Option<String>,
    /// This app serves the URL that consumer apps wire to via their `url_env`.
    /// Exactly one app (the API) is normally marked; consumers reference it by role,
    /// not by a hardcoded name.
    #[serde(default)]
    pub provides_url: bool,
    /// Commands run in the app's directory during `issue setup`, in order. Each
    /// inner array is one argv (program + args), e.g.
    /// `[["doppler","run","-c","local","--","bun","install"]]`.
    #[serde(default)]
    pub setup: Vec<Vec<String>>,
    #[serde(default)]
    pub path: Option<String>,
    // Map fields last: the toml serializer emits scalar/array fields before tables.
    #[serde(default)]
    pub static_env: HashMap<String, String>,
    /// Env written to `<app>/.env.local` during `issue setup` (e.g. dummy workflow ids).
    #[serde(default)]
    pub prep_env: HashMap<String, String>,
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports config_roundtrips config_serializes`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs
git commit -m "feat(ports): make config types serializable for config show"
```

---

## Task 2: Deep-merge core with provenance

A pure merge over parsed layers — the heart of layered resolution, testable without the filesystem.

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (add `Provenance`, `merge_layers`, `deep_merge`, `record_origin`)

- [ ] **Step 1: Write the failing tests** — append to the `tests` module in `crates/devkit-ports/src/config.rs`:

```rust
    fn tbl(s: &str) -> toml::Table {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn deeper_layer_overrides_scalar_keeps_others() {
        let base = tbl("[defaults]\nworktree_root='/a'\nbranch_prefix='x/'\n");
        let top = tbl("[defaults]\nbranch_prefix='y/'\n");
        let (m, origin) = merge_layers(&[
            (PathBuf::from("/base"), base),
            (PathBuf::from("/top"), top),
        ]);
        assert_eq!(m["defaults"]["branch_prefix"].as_str(), Some("y/"));
        assert_eq!(m["defaults"]["worktree_root"].as_str(), Some("/a"));
        assert_eq!(origin["defaults.branch_prefix"], PathBuf::from("/top"));
        assert_eq!(origin["defaults.worktree_root"], PathBuf::from("/base"));
    }

    #[test]
    fn arrays_replace_wholesale() {
        let base = tbl("[apps.api]\nlaunch=['a','b']\n");
        let top = tbl("[apps.api]\nlaunch=['c']\n");
        let (m, origin) = merge_layers(&[
            (PathBuf::from("/b"), base),
            (PathBuf::from("/t"), top),
        ]);
        let launch = m["apps"]["api"]["launch"].as_array().unwrap();
        assert_eq!(launch.len(), 1);
        assert_eq!(launch[0].as_str(), Some("c"));
        assert_eq!(origin["apps.api.launch"], PathBuf::from("/t"));
    }

    #[test]
    fn nested_maps_merge_per_key() {
        let base = tbl("[apps.api.static_env]\nA='1'\nB='2'\n");
        let top = tbl("[apps.api.static_env]\nB='9'\nC='3'\n");
        let (m, origin) = merge_layers(&[
            (PathBuf::from("/b"), base),
            (PathBuf::from("/t"), top),
        ]);
        let se = &m["apps"]["api"]["static_env"];
        assert_eq!(se["A"].as_str(), Some("1"));
        assert_eq!(se["B"].as_str(), Some("9"));
        assert_eq!(se["C"].as_str(), Some("3"));
        assert_eq!(origin["apps.api.static_env.B"], PathBuf::from("/t"));
        assert_eq!(origin["apps.api.static_env.A"], PathBuf::from("/b"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-ports deeper_layer arrays_replace nested_maps`
Expected: FAIL — `merge_layers` not found.

- [ ] **Step 3: Implement the merge** — add to `crates/devkit-ports/src/config.rs` (after the `impl Config` block, around `:131`). Note the `use` additions at the top: `toml::{Table, Value}` are referenced fully-qualified below, so no new `use` is required.

```rust
/// Per-leaf record of which config layer supplied each value.
#[derive(Debug, Default)]
pub struct Provenance {
    /// Resolved layer files, lowest→highest precedence.
    pub layers: Vec<PathBuf>,
    /// Dotted config path (e.g. `apps.api.base_port`) → file that supplied it.
    pub origin: HashMap<String, PathBuf>,
}

/// Deep-merge parsed layers given lowest→highest precedence. Tables merge key by
/// key; every non-table value (scalar or array) is replaced wholesale by a higher
/// layer. Records, per leaf dotted-path, the highest layer that set it.
pub(crate) fn merge_layers(
    layers: &[(PathBuf, toml::Table)],
) -> (toml::Table, HashMap<String, PathBuf>) {
    let mut merged = toml::Table::new();
    let mut origin = HashMap::new();
    for (path, table) in layers {
        deep_merge(&mut merged, table, path, "", &mut origin);
    }
    (merged, origin)
}

fn deep_merge(
    acc: &mut toml::Table,
    overlay: &toml::Table,
    src: &Path,
    prefix: &str,
    origin: &mut HashMap<String, PathBuf>,
) {
    for (k, v) in overlay {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        if let (Some(toml::Value::Table(at)), toml::Value::Table(ot)) = (acc.get_mut(k), v) {
            deep_merge(at, ot, src, &path, origin);
        } else {
            record_origin(&path, v, src, origin);
            acc.insert(k.clone(), v.clone());
        }
    }
}

/// Record the source file for every scalar/array leaf reachable from `v`. A table
/// recurses into its keys; everything else is a single leaf.
fn record_origin(path: &str, v: &toml::Value, src: &Path, origin: &mut HashMap<String, PathBuf>) {
    match v {
        toml::Value::Table(t) => {
            for (k, sub) in t {
                record_origin(&format!("{path}.{k}"), sub, src, origin);
            }
        }
        _ => {
            origin.insert(path.to_string(), src.to_path_buf());
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports deeper_layer arrays_replace nested_maps`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs
git commit -m "feat(ports): add deep-merge with per-leaf provenance"
```

---

## Task 3: Layer discovery + `resolve`

Walk cwd→root collecting `devkit.toml` files, honor `[config] root = true`, prepend the home config, merge, and deserialize.

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (add `resolve`, `resolve_with_home`, `discover`, `read_layer`, `home_config_path`)

- [ ] **Step 1: Write the failing tests** — append to the `tests` module. These write temp `devkit.toml` trees and pass an explicit (or absent) home so the dev machine's real `~/.config/devkit/config.toml` never leaks in:

```rust
    fn unique_tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("devkit-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const FULL_DEFAULTS: &str =
        "worktree_root='/w'\nbranch_prefix='x/'\nbaseline_ref='r'\nbaseline_path='/b'\n";

    #[test]
    fn resolve_merges_parent_and_child() {
        let root = unique_tmp("merge");
        let child = root.join("repo");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(
            root.join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=1\nlaunch=['a']\n"),
        )
        .unwrap();
        std::fs::write(
            child.join("devkit.toml"),
            "[defaults]\nbranch_prefix='y/'\n[apps.api]\nbase_port=2\n",
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, &child, None).unwrap();
        assert_eq!(cfg.defaults.branch_prefix, "y/"); // child overrides
        assert_eq!(cfg.defaults.worktree_root, "/w"); // inherited from parent
        assert_eq!(cfg.apps["api"].base_port, 2); // child overrides
        assert_eq!(cfg.apps["api"].launch, vec!["a".to_string()]); // inherited
        assert_eq!(prov.layers.len(), 2);
        assert_eq!(prov.origin["defaults.branch_prefix"], child.join("devkit.toml"));
    }

    #[test]
    fn root_marker_stops_walk() {
        let root = unique_tmp("rooted");
        let child = root.join("repo");
        std::fs::create_dir_all(&child).unwrap();
        let home = root.join("home.toml");
        std::fs::write(&home, "[defaults]\nbranch_prefix='HOME/'\n").unwrap();
        std::fs::write(
            root.join("devkit.toml"),
            "[defaults]\nworktree_root='/PARENT'\n",
        )
        .unwrap();
        std::fs::write(
            child.join("devkit.toml"),
            format!("[config]\nroot=true\n[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=2\nlaunch=['a']\n"),
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, &child, Some(&home)).unwrap();
        assert_eq!(cfg.defaults.worktree_root, "/w"); // parent's /PARENT dropped
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // home's HOME/ dropped
        assert_eq!(prov.layers, vec![child.join("devkit.toml")]);
    }

    #[test]
    fn home_layer_is_lowest_precedence() {
        let root = unique_tmp("home");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let home = root.join("home.toml");
        std::fs::write(&home, "[defaults]\nbranch_prefix='HOME/'\nworktree_root='/hw'\n").unwrap();
        std::fs::write(
            repo.join("devkit.toml"),
            format!("[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=2\nlaunch=['a']\n"),
        )
        .unwrap();
        let (cfg, prov) = resolve_with_home(None, &repo, Some(&home)).unwrap();
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // repo wins over home
        assert_eq!(prov.origin["defaults.branch_prefix"], repo.join("devkit.toml"));
        // a field only the home layer sets still resolves, attributed to home
        assert_eq!(prov.layers.first(), Some(&home));
    }

    #[test]
    fn explicit_config_bypasses_layering() {
        let root = unique_tmp("explicit");
        let child = root.join("repo");
        std::fs::create_dir_all(&child).unwrap();
        let explicit = root.join("custom.toml");
        std::fs::write(
            &explicit,
            format!("[defaults]\n{FULL_DEFAULTS}[apps.api]\nbase_port=7\nlaunch=['a']\n"),
        )
        .unwrap();
        std::fs::write(child.join("devkit.toml"), "[defaults]\nbranch_prefix='IGNORED/'\n").unwrap();
        let (cfg, prov) = resolve_with_home(Some(&explicit), &child, None).unwrap();
        assert_eq!(cfg.apps["api"].base_port, 7);
        assert_eq!(cfg.defaults.branch_prefix, "x/"); // child file not consulted
        assert_eq!(prov.layers, vec![explicit]);
    }

    #[test]
    fn resolve_errors_when_no_config_found() {
        let root = unique_tmp("empty");
        let err = resolve_with_home(None, &root, None).unwrap_err();
        assert!(err.to_string().contains("no devkit.toml"));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-ports resolve_ root_marker home_layer explicit_config`
Expected: FAIL — `resolve_with_home` not found.

- [ ] **Step 3: Implement discovery and resolve** — add to `crates/devkit-ports/src/config.rs` (after `merge_layers`). Add `use anyhow::bail;` is not needed — use `anyhow::bail!` via the already-imported macro path; the file already imports `anyhow::{Context, Result}`, so reference `anyhow::bail!` fully-qualified.

```rust
fn read_layer(p: &Path) -> Result<(PathBuf, toml::Table)> {
    let body = std::fs::read_to_string(p)
        .with_context(|| format!("reading config layer {}", p.display()))?;
    let table: toml::Table =
        toml::from_str(&body).with_context(|| format!("parsing config layer {}", p.display()))?;
    Ok((p.to_path_buf(), table))
}

fn home_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/devkit/config.toml"))
}

/// Whether a parsed layer declares `[config] root = true` (stop walking upward).
fn is_root_layer(t: &toml::Table) -> bool {
    t.get("config")
        .and_then(|c| c.as_table())
        .and_then(|c| c.get("root"))
        .and_then(|r| r.as_bool())
        .unwrap_or(false)
}

/// Build the ordered layer list (lowest→highest precedence): the home config (unless
/// a `root = true` marker cuts it off), then each `devkit.toml` from the filesystem
/// root down to `start`. An explicit path or `$DEVKIT_CONFIG` is the sole layer.
fn discover(
    explicit: Option<&Path>,
    start: &Path,
    home: Option<&Path>,
) -> Result<Vec<(PathBuf, toml::Table)>> {
    if let Some(p) = explicit {
        return Ok(vec![read_layer(p)?]);
    }
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") {
        return Ok(vec![read_layer(&PathBuf::from(p))?]);
    }

    // Walk upward collecting devkit.toml files (deepest first); stop at a root marker.
    let mut stack: Vec<(PathBuf, toml::Table)> = Vec::new();
    let mut rooted = false;
    let mut dir = Some(start);
    while let Some(d) = dir {
        let c = d.join("devkit.toml");
        if c.is_file() {
            let layer = read_layer(&c)?;
            let is_root = is_root_layer(&layer.1);
            stack.push(layer);
            if is_root {
                rooted = true;
                break;
            }
        }
        dir = d.parent();
    }

    let mut layers: Vec<(PathBuf, toml::Table)> = Vec::new();
    if !rooted
        && let Some(h) = home
        && h.is_file()
    {
        layers.push(read_layer(h)?);
    }
    stack.reverse(); // deepest-first → shallowest-first (lowest precedence first)
    layers.extend(stack);

    if layers.is_empty() {
        anyhow::bail!(
            "no devkit.toml found (--config / $DEVKIT_CONFIG / ./devkit.toml walking up / ~/.config/devkit/config.toml)"
        );
    }
    Ok(layers)
}

/// Resolve the effective config by layering and deep-merging all applicable files.
pub fn resolve(explicit: Option<&Path>, start: &Path) -> Result<(Config, Provenance)> {
    resolve_with_home(explicit, start, home_config_path().as_deref())
}

/// `resolve` with an injectable home-config path (tests pass a controlled path or
/// `None` so the real `~/.config/devkit/config.toml` never participates).
pub(crate) fn resolve_with_home(
    explicit: Option<&Path>,
    start: &Path,
    home: Option<&Path>,
) -> Result<(Config, Provenance)> {
    let layers = discover(explicit, start, home)?;
    let order: Vec<PathBuf> = layers.iter().map(|(p, _)| p.clone()).collect();
    let (merged, origin) = merge_layers(&layers);
    let cfg: Config = toml::Value::Table(merged)
        .try_into()
        .context("deserializing merged devkit config")?;
    Ok((
        cfg,
        Provenance {
            layers: order,
            origin,
        },
    ))
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports resolve_ root_marker home_layer explicit_config`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs
git commit -m "feat(ports): layer devkit.toml from cwd to root plus home config"
```

---

## Task 4: Route `load` through `resolve` and add `flatten`

Make every binary inherit layering, carry provenance on `Loaded`, and add the dotted-leaf flattener `config show --origin` needs.

**Files:**
- Modify: `crates/devkit-ports/src/load.rs:10-29`
- Modify: `crates/devkit-ports/src/config.rs` (add `flatten` + test)

- [ ] **Step 1: Write the failing test for `flatten`** — append to the `tests` module in `crates/devkit-ports/src/config.rs`:

```rust
    #[test]
    fn flatten_yields_sorted_dotted_leaves() {
        let v: toml::Value =
            toml::from_str("[a]\nx=1\n[a.b]\ny='z'\nlist=['p','q']\n").unwrap();
        let mut out = Vec::new();
        flatten(&v, "", &mut out);
        let paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();
        assert!(paths.contains(&"a.x"));
        assert!(paths.contains(&"a.b.y"));
        // arrays are single leaves, not flattened element-by-element
        assert!(paths.contains(&"a.b.list"));
        assert!(!paths.iter().any(|p| p.starts_with("a.b.list.")));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p devkit-ports flatten_yields`
Expected: FAIL — `flatten` not found.

- [ ] **Step 3: Implement `flatten`** — add to `crates/devkit-ports/src/config.rs` (after `record_origin`):

```rust
/// Flatten a serialized config `Value` into `(dotted-path, leaf-value)` pairs. Tables
/// recurse; scalars and arrays are leaves. Mirrors `record_origin`'s leaf model so
/// every emitted path can be looked up in `Provenance::origin`.
pub fn flatten(v: &toml::Value, prefix: &str, out: &mut Vec<(String, toml::Value)>) {
    match v {
        toml::Value::Table(t) => {
            for (k, sub) in t {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(sub, &path, out);
            }
        }
        _ => out.push((prefix.to_string(), v.clone())),
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p devkit-ports flatten_yields`
Expected: PASS.

- [ ] **Step 5: Route `load::load` through `resolve` and carry provenance** — replace the whole body of `crates/devkit-ports/src/load.rs` with:

```rust
use crate::{
    apps::{self, App},
    config::{self, Config, Provenance},
    doppler,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub struct Loaded {
    pub config: Config,
    pub catalog: HashMap<String, App>,
    pub provenance: Provenance,
}

pub fn load(explicit: Option<&Path>, start: &Path) -> Result<Loaded> {
    let (cfg, provenance) = config::resolve(explicit, start)?;
    let yaml_path = config::expand_tilde(&cfg.defaults.doppler_yaml);
    let p2p = match std::fs::read_to_string(&yaml_path) {
        Ok(y) => doppler::path_to_project(&y)?,
        Err(_) => HashMap::new(), // doppler.yaml optional; apps then need explicit path/project
    };
    let catalog = apps::catalog(&cfg, &p2p)?;
    Ok(Loaded {
        config: cfg,
        catalog,
        provenance,
    })
}
```

- [ ] **Step 6: Run the full workspace suite to confirm nothing regressed**

Run: `cargo test --workspace`
Expected: PASS — all existing tests plus the new ones. (`Loaded` gained a field; every caller only reads `.config`/`.catalog`, so no caller breaks.)

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-ports/src/config.rs crates/devkit-ports/src/load.rs
git commit -m "feat(ports): resolve layered config through load and expose provenance"
```

---

## Task 5: `devrun config show`

**Files:**
- Create: `src/bin/devrun/config.rs`
- Modify: `src/bin/devrun/main.rs:1` (module), `:26-63` (`Cmd` + `ConfigCmd`), `:184-218` (dispatch)

- [ ] **Step 1: Write the failing tests for the formatting helpers** — create `src/bin/devrun/config.rs` with the implementation stubs and tests together (tests reference functions defined in the same step's Step 3 code; write the test module now, the bodies in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use devkit_ports::config::{Config, Provenance};
    use std::path::PathBuf;

    // Build the sample inline: `config::tests_sample()` is `#[cfg(test)]` in
    // devkit-ports, so it is NOT compiled into the crate when the devrun binary
    // builds its tests (a dependency builds without its own test cfg).
    fn sample_cfg() -> Config {
        Config::parse(
            "[defaults]\nworktree_root='/w'\nbranch_prefix='x/'\nbaseline_ref='r'\nbaseline_path='/b'\n[apps.api]\nbase_port=1\nlaunch=['a']\n",
        )
        .unwrap()
    }

    #[test]
    fn origin_lines_annotate_source_and_default() {
        let cfg = sample_cfg();
        let mut prov = Provenance::default();
        prov.origin.insert(
            "defaults.worktree_root".into(),
            PathBuf::from("/home/u/.config/devkit/config.toml"),
        );
        let lines = origin_lines(&cfg, &prov).unwrap();
        // a value present in the origin map is attributed to its file
        assert!(lines.iter().any(|l| l.starts_with("defaults.worktree_root =")
            && l.contains("# from /home/u/.config/devkit/config.toml")));
        // a serde-defaulted value (pr_base) has no origin → marked (default)
        assert!(lines
            .iter()
            .any(|l| l.starts_with("defaults.pr_base =") && l.contains("# (default)")));
        // output is sorted by path
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted);
    }

    #[test]
    fn origin_json_has_config_and_origins() {
        let cfg = sample_cfg();
        let mut prov = Provenance::default();
        prov.origin
            .insert("defaults.worktree_root".into(), PathBuf::from("/x/devkit.toml"));
        let v = origin_json(&cfg, &prov).unwrap();
        assert!(v.get("config").is_some());
        assert_eq!(
            v["origins"]["defaults.worktree_root"].as_str(),
            Some("/x/devkit.toml")
        );
    }
}
```

(`sample_cfg()` builds the `Config` from an inline literal — see the comment on it above — because `config::tests_sample()` is `#[cfg(test)]`-gated in `devkit-ports` and therefore not present in the dependency when this binary compiles its tests.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p devkit --bin devrun origin_lines origin_json`
Expected: FAIL — `src/bin/devrun/config.rs` functions not defined (and module not yet declared).

- [ ] **Step 3: Implement the `config` module** — put this at the **top** of `src/bin/devrun/config.rs` (above the `#[cfg(test)] mod tests` from Step 1):

```rust
use crate::Cli;
use anyhow::{Context, Result};
use devkit_ports::config::{self, Config, Provenance};
use devkit_ports::load;
use std::collections::BTreeMap;
use std::path::Path;

/// `devrun config show [--origin] [--json]`
pub fn show(cli: &Cli, cwd: &str, origin: bool, json: bool) -> Result<()> {
    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
    let cfg = &loaded.config;
    let prov = &loaded.provenance;
    match (origin, json) {
        (true, false) => {
            for line in origin_lines(cfg, prov)? {
                println!("{line}");
            }
        }
        (true, true) => {
            println!("{}", serde_json::to_string_pretty(&origin_json(cfg, prov)?)?);
        }
        (false, true) => println!("{}", serde_json::to_string_pretty(cfg)?),
        (false, false) => println!("{}", toml::to_string_pretty(cfg)?),
    }
    Ok(())
}

/// Flattened `path = value  # from <file>` (or `# (default)`) lines, sorted by path.
fn origin_lines(cfg: &Config, prov: &Provenance) -> Result<Vec<String>> {
    let val = toml::Value::try_from(cfg).context("serializing config to toml")?;
    let mut leaves = Vec::new();
    config::flatten(&val, "", &mut leaves);
    leaves.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(leaves
        .iter()
        .map(|(path, value)| match prov.origin.get(path) {
            Some(f) => format!("{path} = {value}  # from {}", f.display()),
            None => format!("{path} = {value}  # (default)"),
        })
        .collect())
}

/// `{ "config": <cfg>, "origins": { dotted-path: file } }` for `--origin --json`.
fn origin_json(cfg: &Config, prov: &Provenance) -> Result<serde_json::Value> {
    let origins: BTreeMap<String, String> = prov
        .origin
        .iter()
        .map(|(k, v)| (k.clone(), v.display().to_string()))
        .collect();
    Ok(serde_json::json!({ "config": cfg, "origins": origins }))
}
```

- [ ] **Step 4: Wire the subcommand into `main.rs`** — make these four edits in `src/bin/devrun/main.rs`:

1. Add the module declaration at the top (after `mod baseline;`, `:1`):

```rust
mod baseline;
mod config;
```

2. Add a `Config` variant to the `Cmd` enum (inside `enum Cmd { … }`, after the `Logs { … }` variant, before `Completions`):

```rust
    /// Show the effective merged config, or list configured apps.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
```

3. Add the `ConfigCmd` enum immediately after the `Cmd` enum closes (after `:63`):

```rust
#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the effective merged config (TOML by default).
    Show {
        /// Annotate each value with the file it was resolved from.
        #[arg(long)]
        origin: bool,
        /// Emit JSON instead of TOML.
        #[arg(long)]
        json: bool,
    },
    /// List the configured apps from the merged config.
    Apps {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}
```

4. Add the dispatch arm in `main`'s `match &cli.cmd` (after the `Cmd::Logs { … }` arm, before `Cmd::Completions`):

```rust
        Cmd::Config { cmd } => match cmd {
            ConfigCmd::Show { origin, json } => config::show(&cli, &cwd, *origin, *json),
            ConfigCmd::Apps { json } => config::apps(&cli, &cwd, *json),
        },
```

(`config::apps` lands in Task 6; until then the binary will not compile. Implement Task 6 before running `devrun`. To keep Task 5 independently testable, run the unit tests with `--lib`-style filtering as below, which compiles the test module but you may temporarily stub `apps`. Simplest: do Tasks 5 and 6 back-to-back before the next full build.)

- [ ] **Step 5: Add a temporary `apps` stub so the binary compiles for Task 5 tests** — add to `src/bin/devrun/config.rs` (replaced for real in Task 6):

```rust
/// `devrun config apps [--json]` — implemented in Task 6.
pub fn apps(_cli: &Cli, _cwd: &str, _json: bool) -> Result<()> {
    anyhow::bail!("not yet implemented")
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin devrun origin_lines origin_json`
Expected: PASS (2 tests).

- [ ] **Step 7: Commit**

```bash
git add src/bin/devrun/config.rs src/bin/devrun/main.rs
git commit -m "feat(devrun): add config show with optional value provenance"
```

---

## Task 6: `devrun config apps`

**Files:**
- Modify: `src/bin/devrun/config.rs` (replace the `apps` stub; add tests)

- [ ] **Step 1: Write the failing tests** — add to the `tests` module in `src/bin/devrun/config.rs`:

```rust
    use devkit_ports::apps::App;
    use std::collections::HashMap;

    fn sample_catalog() -> HashMap<String, App> {
        let mut m = HashMap::new();
        m.insert(
            "api".to_string(),
            App {
                name: "api".into(),
                base_port: 9100,
                path: "apps/api".into(),
                launch: vec!["nitro".into(), "dev".into()],
                url_env: Some("FOUNDRY_API_BASE_URL".into()),
                provides_url: true,
                static_env: HashMap::new(),
                prep_env: HashMap::new(),
                setup: Vec::new(),
            },
        );
        m
    }

    #[test]
    fn apps_json_lists_resolved_fields() {
        let v = apps_json(&sample_catalog());
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("api"));
        assert_eq!(arr[0]["base_port"].as_u64(), Some(9100));
        assert_eq!(arr[0]["path"].as_str(), Some("apps/api"));
        assert_eq!(arr[0]["provides_url"].as_bool(), Some(true));
        assert_eq!(arr[0]["url_env"].as_str(), Some("FOUNDRY_API_BASE_URL"));
    }

    #[test]
    fn apps_table_renders_sorted_names() {
        let mut cat = sample_catalog();
        cat.insert(
            "lab-os".to_string(),
            App {
                name: "lab-os".into(),
                base_port: 9200,
                path: "apps/lab-os".into(),
                launch: vec!["next".into()],
                url_env: None,
                provides_url: false,
                static_env: HashMap::new(),
                prep_env: HashMap::new(),
                setup: Vec::new(),
            },
        );
        let t = apps_table(&cat);
        let api_at = t.find("api").unwrap();
        let lab_at = t.find("lab-os").unwrap();
        assert!(api_at < lab_at); // sorted by name
    }
```

For `apps_table` the test calls `.find(substr)` — to keep the table testable as plain text, `apps_table` returns a `String` (the rendered table), and `find` is `str::find`. Adjust the test's last two lines to:

```rust
        let t = apps_table(&cat);
        let api_at = t.find("api").unwrap();
        let lab_at = t.find("lab-os").unwrap();
        assert!(api_at < lab_at);
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p devkit --bin devrun apps_json apps_table`
Expected: FAIL — `apps_json` / `apps_table` not defined.

- [ ] **Step 3: Replace the `apps` stub with the real implementation** — in `src/bin/devrun/config.rs`, replace the stub `pub fn apps` from Task 5 with:

```rust
use devkit_common::ui;
use devkit_ports::apps::App;
use std::collections::HashMap;

/// `devrun config apps [--json]` — a pure readout of the merged app catalog.
pub fn apps(cli: &Cli, cwd: &str, json: bool) -> Result<()> {
    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(cwd))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&apps_json(&loaded.catalog))?
        );
    } else {
        println!("{}", apps_table(&loaded.catalog));
    }
    Ok(())
}

/// Catalog apps sorted by name, as a JSON array of their resolved fields.
fn apps_json(catalog: &HashMap<String, App>) -> serde_json::Value {
    let mut names: Vec<&String> = catalog.keys().collect();
    names.sort();
    let rows: Vec<serde_json::Value> = names
        .iter()
        .map(|n| {
            let a = &catalog[*n];
            serde_json::json!({
                "name": a.name,
                "base_port": a.base_port,
                "path": a.path,
                "provides_url": a.provides_url,
                "url_env": a.url_env,
                "launch": a.launch,
            })
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// Catalog apps sorted by name, rendered as a text table.
fn apps_table(catalog: &HashMap<String, App>) -> String {
    let mut names: Vec<&String> = catalog.keys().collect();
    names.sort();
    let mut t = ui::table(&["NAME", "PORT", "PATH", "PROVIDES_URL", "URL_ENV", "LAUNCH"]);
    for n in names {
        let a = &catalog[n];
        t.add_row(vec![
            a.name.clone(),
            a.base_port.to_string(),
            a.path.clone(),
            a.provides_url.to_string(),
            a.url_env.clone().unwrap_or_else(|| "-".into()),
            a.launch.join(" "),
        ]);
    }
    t.to_string()
}
```

If `ui::table(...).to_string()` is not directly available (the type's `Display`), check how `print_summary` in `main.rs:158-177` renders (`println!("{t}")` — so `t` implements `Display`, and `.to_string()` works). No change needed.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin devrun apps_json apps_table`
Expected: PASS (2 tests).

- [ ] **Step 5: Manual smoke check against the repo's own absence of config** — build and exercise:

Run: `cargo build -p devkit --bin devrun && ./target/debug/devrun --help | rg config`
Expected: the `config` subcommand is listed.

- [ ] **Step 6: Commit**

```bash
git add src/bin/devrun/config.rs
git commit -m "feat(devrun): add config apps catalog listing"
```

---

## Task 7: Docs + full gate

**Files:**
- Modify: `README.md` (document `devrun config` and layered resolution)

- [ ] **Step 1: Find the devrun section in the README**

Run: `rg -n "devrun|config.toml|## " README.md | head -40`
Expected: locate the `devrun` usage section and any existing config-resolution description.

- [ ] **Step 2: Document the new behavior** — add a subsection near the `devrun` docs describing:
  - `devrun config show [--origin] [--json]` — print the effective merged config; `--origin` annotates each value with its source file; `--json` emits JSON.
  - `devrun config apps [--json]` — list configured apps (name, port, path, provides_url, url_env, launch).
  - Layered resolution: `devkit.toml` is now merged from the filesystem root down to the cwd, with `~/.config/devkit/config.toml` as the base layer; deeper files override per value; `[config] root = true` stops the upward walk; `--config`/`$DEVKIT_CONFIG` selects a single file verbatim.

Keep wording tight and match the README's existing voice (see the `CLAUDE.md` comment rules — timeless, present-tense, no change-narration).

- [ ] **Step 3: Run the full merge gate**

Run: `cargo fmt --all`
Then: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero warnings.
Then: `cargo test --workspace`
Expected: all tests pass (the original 128 plus the ones added here).

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: document devrun config and layered config resolution"
```

---

## Self-Review notes

- **Spec coverage:** §1a discovery → Task 3; §1b merge rule → Task 2; §1c provenance → Tasks 2–4; §1d errors → Task 3 (`read_layer` context, `resolve_errors_when_no_config_found`); §2a `config show` → Task 5; §2b `config apps` → Task 6; harness out-of-scope → no task (correct). Behavior-change note (§4) → README in Task 7.
- **Type consistency:** `Provenance { layers: Vec<PathBuf>, origin: HashMap<String, PathBuf> }`, `merge_layers`, `resolve`/`resolve_with_home`, `flatten`, `Loaded.provenance`, `config::show`/`config::apps`, `origin_lines`/`origin_json`/`apps_json`/`apps_table` are referenced consistently across tasks.
- **Ordering caveat captured:** Task 5 introduces a `Cmd::Config` arm that calls `config::apps`, implemented in Task 6 — flagged in Task 5 Step 4/5 with a stub so each task builds.
- **No HOME-env races:** layering tests use `resolve_with_home(.., None|Some(tmp))`, never the process `HOME`.
