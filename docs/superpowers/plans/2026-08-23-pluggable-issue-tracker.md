# Pluggable issue tracker, phases 1 and 2: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put a `Tracker` seam under every Linear call site in the `issue` CLI so
a second provider can be added without touching callers, and make `[defaults]`
path values portable enough to commit to a shared repository.

**Architecture:** Two independent deliveries. Phase 1 resolves `[defaults]`
paths at config-load time: `${VAR}` expansion, then `~`, then relative paths
resolved against the config layer that declared them. Phase 2 extracts
`devkit-config` into its own leaf crate, replaces the stringly-typed Linear
state vocabulary with a `StateKind` enum, and moves every `linear::` call behind
a `Tracker` trait with `Linear` and `None` implementations. Phase 2 changes no
behavior; a green `cargo test --workspace` plus unchanged CLI output is its
proof.

**Tech Stack:** Rust 2024, `anyhow` for errors, `serde` + `toml` for config,
`schemars` for the published JSON Schema, `minijinja` for templates.

**Spec:** `docs/superpowers/specs/2026-08-23-pluggable-issue-tracker-design.md`

**Not in this plan:** Phase 3, the GitHub tracker. The spec flags its `issue_pr`
cross-reference mapping as unproven pending a prototype, so its task steps would
be invented rather than specified. It gets its own plan after that prototype.

## Global Constraints

- Rust edition 2024. `std::env::set_var` is `unsafe` in this edition; every test
  that sets an environment variable must wrap the call in `unsafe { }`.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  and `cargo fmt --all` all pass before every commit. Zero-warning policy.
- Conventional Commits for every commit: `type(scope): description`, imperative
  mood, lowercase after the colon, no trailing period, subject ≤50 chars.
- `anyhow` everywhere; errors carry `.context()`.
- No `_ =>` catch-all arms when matching an enum this plan introduces. Map every
  variant. (`AGENTS.md`, Conventions.)
- Comments explain a non-obvious *why*, never restate the *what*. No PR or task
  references in comments.
- Tests that spawn or reap processes poll for state rather than sleeping a fixed
  interval. None of this plan's tests spawn processes, but the rule stands.
- Never run `git stash`. Other sessions share this repository.
- Work happens in the worktree `../devkit-worktrees/pluggable-tracker` on branch
  `lev/pluggable-issue-tracker`. Never check out a feature branch in the primary
  clone.

## File structure

Phase 1 touches one file plus its tests.

| Path | Responsibility |
|---|---|
| `crates/devkit-ports/src/config.rs` | gains `expand_vars`, `normalize_lexically`, `resolve_path_key`, `resolve_defaults`; `resolve_with_home` calls the last one |

Phase 2 moves and adds files.

| Path | Responsibility |
|---|---|
| `crates/devkit-config/Cargo.toml` | new leaf crate: `anyhow`, `schemars`, `serde`, `toml` |
| `crates/devkit-config/src/lib.rs` | the former `devkit-ports/src/config.rs`, verbatim |
| `crates/devkit-common/src/tracker/mod.rs` | `StateKind`, `State`, `IssueRef`, `IssueDetails`, `AssignedIssue`, `PrRef`, the `Tracker` trait, `resolve` |
| `crates/devkit-common/src/tracker/none.rs` | the no-tracker implementation |
| `crates/devkit-common/src/tracker/linear.rs` | the former `devkit-common/src/linear.rs` plus a `LinearTracker` impl |
| `crates/devkit-common/src/tracker/fake.rs` | `#[cfg(any(test, feature = "test-support"))]` fake for injection |
| `crates/devkit-common/src/worktree.rs` | gains `IssueRecord`, and `issue_id_of` becomes record-first |
| `crates/devkit-issue/src/status.rs` | `IssueWorktree.state`, `StatusReport.tracker`, `gather_with` |

## Phase 1: config path expansion

### Task 1: `${VAR}` expansion in config values

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` (add near `expand_tilde`, line 769)
- Test: `crates/devkit-ports/src/config.rs` (the existing `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `fn expand_vars(raw: &str, key: &str) -> Result<String>` — private to
  the module, consumed by Task 2.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/devkit-ports/src/config.rs`:

```rust
#[test]
fn expand_vars_substitutes_a_set_variable() {
    unsafe { std::env::set_var("DEVKIT_TEST_ROOT", "/srv/work") };
    let got = expand_vars("${DEVKIT_TEST_ROOT}/trees", "defaults.worktree_root").unwrap();
    assert_eq!(got, "/srv/work/trees");
}

#[test]
fn expand_vars_errors_naming_the_key_and_the_variable() {
    let err = expand_vars("${DEVKIT_TEST_ABSENT}/x", "defaults.worktree_root")
        .expect_err("an unset variable must be an error");
    let msg = err.to_string();
    assert!(msg.contains("defaults.worktree_root"), "message names the key: {msg}");
    assert!(msg.contains("DEVKIT_TEST_ABSENT"), "message names the variable: {msg}");
}

#[test]
fn expand_vars_treats_double_dollar_as_a_literal() {
    let got = expand_vars("/opt/$${NOT_A_VAR}/x", "defaults.baseline_path").unwrap();
    assert_eq!(got, "/opt/${NOT_A_VAR}/x");
}

#[test]
fn expand_vars_passes_a_bare_dollar_through() {
    // A `$` not followed by `{` or `$` is a legal path character, so it stays.
    let got = expand_vars("/opt/a$b/c", "defaults.baseline_path").unwrap();
    assert_eq!(got, "/opt/a$b/c");
}

#[test]
fn expand_vars_errors_on_an_unterminated_brace() {
    let err = expand_vars("${OPEN", "defaults.worktree_root").expect_err("unterminated");
    assert!(err.to_string().contains("unterminated"), "{err}");
}

#[test]
fn expand_vars_leaves_a_plain_value_alone() {
    let got = expand_vars("~/Git/example", "defaults.worktree_root").unwrap();
    assert_eq!(got, "~/Git/example");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-ports config::tests::expand_vars`
Expected: FAIL to compile with `cannot find function 'expand_vars' in this scope`.

- [ ] **Step 3: Write the implementation**

Add above `expand_tilde` in `crates/devkit-ports/src/config.rs`:

```rust
/// Expand `${VAR}` references in a config value. `$$` is a literal `$`; a `$`
/// followed by anything else is left alone, since it is a legal path character.
/// An unset variable is an error naming both the config key and the variable —
/// silently substituting an empty string would produce a plausible wrong path.
fn expand_vars(raw: &str, key: &str) -> Result<String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
        } else if let Some(tail) = after.strip_prefix('{') {
            let end = tail
                .find('}')
                .with_context(|| format!("`{key}`: unterminated `${{` in {raw:?}"))?;
            let name = &tail[..end];
            let val = std::env::var(name).map_err(|_| {
                anyhow::anyhow!("`{key}`: `${{{name}}}` is not set in the environment")
            })?;
            out.push_str(&val);
            rest = &tail[end + 1..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports config::tests::expand_vars`
Expected: PASS, 6 tests.

Then `cargo clippy -p devkit-ports --all-targets -- -D warnings`. `expand_vars`
is not yet called by anything, so expect a `dead_code` warning; add
`#[allow(dead_code)]` above it with the comment `// wired up in the next commit`
**only if** clippy fails, and remove it in Task 2.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-ports/src/config.rs
git commit -m "feat(config): expand \${VAR} in config values"
```

### Task 2: resolve `[defaults]` paths against their declaring layer

**Files:**
- Modify: `crates/devkit-ports/src/config.rs` — add `normalize_lexically`,
  `resolve_path_key`, `resolve_defaults`; call the last from
  `resolve_with_home` (line 739)
- Modify: `docs/configuration.md`
- Test: `crates/devkit-ports/src/config.rs` (the existing `mod tests`)

**Interfaces:**
- Consumes: `expand_vars(raw, key) -> Result<String>` from Task 1;
  `Provenance.origin: HashMap<String, PathBuf>` (config.rs:502);
  `expand_tilde(&str) -> PathBuf` (config.rs:769).
- Produces: `resolve_with_home` returns a `Config` whose
  `defaults.worktree_root`, `defaults.baseline_path`, and
  `defaults.doppler_yaml` are absolute normalized strings, and whose
  `defaults.branch_prefix` has had `${VAR}` expanded.

Why lexical normalization rather than `std::fs::canonicalize`: `worktree_root`
routinely does not exist yet when config loads, and `canonicalize` fails on a
missing path. It also matters that the result has no `..` left in it — the ports
registry uses the worktree root path *as a string* for holder identity and
prefix matching (`strays/mod.rs:229-240`), so `/a/b/../c` and `/a/c` must not
both be producible.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/devkit-ports/src/config.rs`:

```rust
use std::io::Write as _;

/// Write `body` to `<dir>/devkit.toml` and return the file's path.
fn write_cfg(dir: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join("devkit.toml");
    let mut f = std::fs::File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    p
}

#[test]
fn normalize_lexically_drops_dot_and_pops_dotdot() {
    assert_eq!(
        normalize_lexically(Path::new("/a/b/../c/./d")),
        PathBuf::from("/a/c/d")
    );
    assert_eq!(normalize_lexically(Path::new("/a/../..")), PathBuf::from("/"));
}

#[test]
fn a_relative_path_resolves_against_its_declaring_layer() {
    let tmp = std::env::temp_dir().join(format!("devkit-relcfg-{}", std::process::id()));
    let proj = tmp.join("proj");
    write_cfg(
        &proj,
        "[defaults]\n\
         worktree_root = \"../proj-worktrees\"\n\
         branch_prefix = \"lev/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \"../proj-worktrees/_baseline\"\n",
    );
    let (cfg, _) = resolve_with_home(None, &proj, None).unwrap();
    assert_eq!(
        cfg.defaults.worktree_root,
        tmp.join("proj-worktrees").to_string_lossy()
    );
    assert_eq!(
        cfg.defaults.baseline_path,
        tmp.join("proj-worktrees/_baseline").to_string_lossy()
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn the_same_relative_path_resolves_alike_from_two_start_dirs() {
    let tmp = std::env::temp_dir().join(format!("devkit-relcfg2-{}", std::process::id()));
    let proj = tmp.join("proj");
    let nested = proj.join("a").join("b");
    std::fs::create_dir_all(&nested).unwrap();
    write_cfg(
        &proj,
        "[defaults]\n\
         worktree_root = \"../proj-worktrees\"\n\
         branch_prefix = \"lev/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \"\"\n",
    );
    let (from_root, _) = resolve_with_home(None, &proj, None).unwrap();
    let (from_nested, _) = resolve_with_home(None, &nested, None).unwrap();
    assert_eq!(from_root.defaults.worktree_root, from_nested.defaults.worktree_root);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn an_absolute_path_and_a_tilde_path_are_left_absolute() {
    let tmp = std::env::temp_dir().join(format!("devkit-abscfg-{}", std::process::id()));
    write_cfg(
        &tmp,
        "[defaults]\n\
         worktree_root = \"/srv/trees\"\n\
         branch_prefix = \"lev/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \"\"\n",
    );
    let (cfg, _) = resolve_with_home(None, &tmp, None).unwrap();
    assert_eq!(cfg.defaults.worktree_root, "/srv/trees");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn an_empty_path_key_stays_empty() {
    let tmp = std::env::temp_dir().join(format!("devkit-emptycfg-{}", std::process::id()));
    write_cfg(
        &tmp,
        "[defaults]\n\
         worktree_root = \"/srv/trees\"\n\
         branch_prefix = \"lev/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \"\"\n",
    );
    let (cfg, _) = resolve_with_home(None, &tmp, None).unwrap();
    assert_eq!(cfg.defaults.baseline_path, "", "an unset path must not become the layer dir");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn branch_prefix_expands_vars_but_is_not_a_path() {
    unsafe { std::env::set_var("DEVKIT_TEST_DEV", "lev") };
    let tmp = std::env::temp_dir().join(format!("devkit-prefixcfg-{}", std::process::id()));
    write_cfg(
        &tmp,
        "[defaults]\n\
         worktree_root = \"/srv/trees\"\n\
         branch_prefix = \"${DEVKIT_TEST_DEV}/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \"\"\n",
    );
    let (cfg, _) = resolve_with_home(None, &tmp, None).unwrap();
    assert_eq!(cfg.defaults.branch_prefix, "lev/");
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn an_unset_var_fails_the_whole_config_load() {
    let tmp = std::env::temp_dir().join(format!("devkit-badvarcfg-{}", std::process::id()));
    write_cfg(
        &tmp,
        "[defaults]\n\
         worktree_root = \"${DEVKIT_TEST_MISSING_ROOT}/trees\"\n\
         branch_prefix = \"lev/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \"\"\n",
    );
    let err = resolve_with_home(None, &tmp, None).expect_err("unset var must fail the load");
    assert!(err.to_string().contains("DEVKIT_TEST_MISSING_ROOT"), "{err}");
    std::fs::remove_dir_all(&tmp).ok();
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-ports config::tests::`
Expected: FAIL to compile with `cannot find function 'normalize_lexically'`.

- [ ] **Step 3: Write the implementation**

Add above `expand_tilde` in `crates/devkit-ports/src/config.rs`:

```rust
/// Resolve `.` and `..` without touching the filesystem. `worktree_root` is
/// routinely a directory that does not exist yet, so `fs::canonicalize` is not
/// available; and the ports registry compares holder paths as strings, so a
/// surviving `..` would let one directory have two spellings.
fn normalize_lexically(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The directory of the config layer that supplied `key`, from the per-leaf
/// provenance map.
fn layer_dir<'a>(origin: &'a HashMap<String, PathBuf>, key: &str) -> Option<&'a Path> {
    origin.get(key).and_then(|p| p.parent())
}

/// Expand `${VAR}`, then `~`, then anchor a still-relative path to the config
/// layer that declared it. Empty stays empty — an unset optional path must not
/// silently become the layer's own directory.
fn resolve_path_key(
    raw: &str,
    key: &str,
    origin: &HashMap<String, PathBuf>,
) -> Result<String> {
    if raw.is_empty() {
        return Ok(String::new());
    }
    let expanded = expand_vars(raw, key)?;
    let p = expand_tilde(&expanded);
    let joined = match (p.is_absolute(), layer_dir(origin, key)) {
        (true, _) | (false, None) => p,
        (false, Some(dir)) => dir.join(p),
    };
    Ok(normalize_lexically(&joined).to_string_lossy().into_owned())
}

/// Resolve every `[defaults]` value that carries a path or an environment
/// reference, in place, once, at load time.
fn resolve_defaults(cfg: &mut Config, origin: &HashMap<String, PathBuf>) -> Result<()> {
    for (key, field) in [
        ("defaults.worktree_root", &mut cfg.defaults.worktree_root),
        ("defaults.baseline_path", &mut cfg.defaults.baseline_path),
        ("defaults.doppler_yaml", &mut cfg.defaults.doppler_yaml),
    ] {
        *field = resolve_path_key(field, key, origin)?;
    }
    cfg.defaults.branch_prefix =
        expand_vars(&cfg.defaults.branch_prefix, "defaults.branch_prefix")?;
    Ok(())
}
```

The `for` loop over an array of `(&str, &mut String)` borrows three disjoint
fields of `cfg.defaults` at once, which the borrow checker accepts because the
array literal takes each field's reference exactly once. If it does not compile
on your toolchain, write the three calls out longhand rather than reaching for
`unsafe` or a macro.

- [ ] **Step 4: Wire it into the loader**

In `resolve_with_home` (`crates/devkit-ports/src/config.rs:739`), change the
deserialization block from:

```rust
    let cfg: Config = toml::Value::Table(merged)
        .try_into()
        .context("deserializing merged devkit config")?;
```

to:

```rust
    let mut cfg: Config = toml::Value::Table(merged)
        .try_into()
        .context("deserializing merged devkit config")?;
    resolve_defaults(&mut cfg, &origin)?;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-ports`
Expected: PASS. If a pre-existing test asserts a `[defaults]` path round-trips
verbatim, it is now asserting the old behavior — update it to expect the
resolved absolute path and say so in the commit body.

Then: `cargo test --workspace`. `strays/mod.rs:486-615` builds `Config` values
by hand rather than through `resolve`, so those tests are unaffected.

- [ ] **Step 6: Document it**

In `docs/configuration.md`, under the `[defaults]` table, add:

```markdown
### Path values

`worktree_root`, `baseline_path`, and `doppler_yaml` are resolved once when the
config loads, in this order:

1. `${VAR}` is replaced with that environment variable. An unset variable is an
   error naming both the config key and the variable. `$$` is a literal `$`; a
   `$` followed by anything else is left alone.
2. A leading `~/` expands to `$HOME`.
3. A path that is still relative is resolved against **the directory of the
   config file that declared it**, not the working directory, and `.` / `..` are
   folded out.

`branch_prefix` gets step 1 only.

Step 3 is what lets a project commit its `devkit.toml`:

```toml
[defaults]
worktree_root = "../myproject-worktrees"
baseline_path = "../myproject-worktrees/_baseline"
baseline_ref  = "origin/main"
```

That is correct on every machine and for every developer. Only `branch_prefix`
is personal — put it in `devkit.local.toml`, or write `"${USER}/"`.
```

- [ ] **Step 7: Regenerate the schema and commit**

The doc comments on `Defaults` are unchanged, so the schema should not drift.
Confirm rather than assume:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/devkit-ports/src/config.rs docs/configuration.md
git commit -m "feat(config): resolve default paths against their layer"
```

If the schema-drift test fails, run `DEVKIT_UPDATE_SCHEMA=1 cargo test --workspace`
and include `schema/devkit-config.json` in the commit.

## Phase 2: the tracker seam

### Task 3: extract `devkit-config` into its own crate

**Files:**
- Create: `crates/devkit-config/Cargo.toml`, `crates/devkit-config/src/lib.rs`
- Delete: `crates/devkit-ports/src/config.rs`
- Modify: `Cargo.toml` (workspace members and dependencies),
  `crates/devkit-ports/Cargo.toml`, `crates/devkit-ports/src/lib.rs`,
  and every file naming `devkit_ports::config` or `crate::config`

**Interfaces:**
- Consumes: nothing.
- Produces: the crate `devkit_config`, exposing at its root exactly what
  `devkit_ports::config` exposed: `Config`, `Defaults`, `Templates`,
  `AppConfig`, `TaskConfig`, `Person`, `LinearConfig`, `BriefConfig`,
  `LayerMarker`, `PrepFile`, `Provenance`, `Health`, `NoConfig`, `resolve`,
  `health`, `home_config_path`, `flatten`, `expand_tilde`, and the `DEFAULT_*`
  constants.

This is a move, not a rewrite. No logic changes. Do it as one commit so the
diff reads as a rename.

- [ ] **Step 1: Create the crate**

```bash
mkdir -p crates/devkit-config/src
git mv crates/devkit-ports/src/config.rs crates/devkit-config/src/lib.rs
```

Write `crates/devkit-config/Cargo.toml`:

```toml
[package]
name = "devkit-config"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
anyhow.workspace = true
schemars.workspace = true
serde = { workspace = true }
toml.workspace = true
```

Copy the `version`/`edition`/`license`/`repository` lines verbatim from
`crates/devkit-ports/Cargo.toml`; if that crate spells them out literally rather
than using `.workspace = true`, match whatever it does.

- [ ] **Step 2: Register it in the workspace**

In the root `Cargo.toml`, add `"crates/devkit-config"` to `members` and
`devkit-config = { path = "crates/devkit-config" }` to the workspace
dependencies, both in the existing alphabetical position. Then add
`devkit-config.workspace = true` to the `[dependencies]` of
`crates/devkit-ports/Cargo.toml`.

- [ ] **Step 3: Fix the module declaration and the internal callers**

Remove `pub mod config;` from `crates/devkit-ports/src/lib.rs`.

The four `devkit-ports` modules that use it are `apps.rs`, `task.rs`, `load.rs`,
and `strays/mod.rs`:

```bash
rg -l 'crate::config' crates/devkit-ports/src \
  | xargs sed -i 's/crate::config/devkit_config/g'
```

`lib.rs` in the new crate begins with `use anyhow::{Context, Result};` and needs
no change; it had zero `crate::` references, which is what makes this move
mechanical.

- [ ] **Step 4: Fix the external callers**

```bash
rg -l 'devkit_ports::config' --glob '*.rs' \
  | xargs sed -i 's/devkit_ports::config/devkit_config/g'
```

Then add `devkit-config.workspace = true` to the `[dependencies]` of the root
package's `Cargo.toml` (the `devkit` binary package) and of any other crate the
sed touched. Find them with:

```bash
rg -l 'devkit_config' --glob '*.rs' | sed 's|/src/.*||;s|^src.*|.|' | sort -u
```

- [ ] **Step 5: Verify the move changed nothing**

Run:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Expected: PASS, with the same test count as before the move. A changed count
means a test file was left behind or duplicated.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(config): extract devkit-config into its own crate

config.rs had zero crate:: imports and sat in devkit-ports for
historical reasons. Three crates worked around the placement:
devkit-locks declares its own HarnessProbe, devkit-docs reimplements
the layer walk, devkit-issue went without.

A leaf crate keeps schemars off devkit-common's dependency path,
which the alternative of moving config into devkit-common would not."
```

### Task 4: replace the state strings with a `StateKind` enum

**Files:**
- Create: `crates/devkit-common/src/tracker/mod.rs`
- Modify: `crates/devkit-common/src/lib.rs` (add `pub mod tracker;`)
- Test: `crates/devkit-common/src/tracker/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub enum StateKind { Triage, Backlog, Unstarted, Started, Completed, Canceled }`
    with `Serialize`/`Deserialize` to the lowercase names, `FromStr`, `Display`,
    `Copy`, `PartialEq`, `Eq`, `Debug`, `Clone`.
  - `pub struct State { pub kind: StateKind, pub name: String, pub color: Option<String> }`,
    `Serialize`/`Deserialize`/`Debug`/`Clone`/`PartialEq`/`Eq`.
  - `StateKind::is_open(self) -> bool` — true for everything except `Completed`
    and `Canceled`.

`color` is `Option<String>` because Linear's batch state query does not fetch a
colour today while its history query does, and a tracker may have no colours at
all. Task 6 adds `color` to the batch query so Linear always supplies one.

- [ ] **Step 1: Write the failing tests**

Create `crates/devkit-common/src/tracker/mod.rs` containing only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_kind_round_trips_through_its_wire_string() {
        for (k, s) in [
            (StateKind::Triage, "triage"),
            (StateKind::Backlog, "backlog"),
            (StateKind::Unstarted, "unstarted"),
            (StateKind::Started, "started"),
            (StateKind::Completed, "completed"),
            (StateKind::Canceled, "canceled"),
        ] {
            assert_eq!(k.to_string(), s);
            assert_eq!(s.parse::<StateKind>().unwrap(), k);
            assert_eq!(serde_json::to_value(k).unwrap(), serde_json::json!(s));
        }
    }

    #[test]
    fn an_unknown_state_string_parses_as_unstarted() {
        // A tracker may add a state devkit does not know. Treat it as open
        // rather than failing the whole status run.
        assert_eq!("something-new".parse::<StateKind>().unwrap(), StateKind::Unstarted);
    }

    #[test]
    fn only_completed_and_canceled_are_closed() {
        assert!(StateKind::Triage.is_open());
        assert!(StateKind::Backlog.is_open());
        assert!(StateKind::Unstarted.is_open());
        assert!(StateKind::Started.is_open());
        assert!(!StateKind::Completed.is_open());
        assert!(!StateKind::Canceled.is_open());
    }
}
```

Add `pub mod tracker;` to `crates/devkit-common/src/lib.rs`, in the existing
alphabetical position among the other `pub mod` lines.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common tracker::`
Expected: FAIL to compile with `cannot find type 'StateKind' in this scope`.

- [ ] **Step 3: Write the implementation**

Above the `mod tests` block in `crates/devkit-common/src/tracker/mod.rs`:

```rust
//! The tracker seam: one contract over Linear, GitHub Issues, or no tracker.

use serde::{Deserialize, Serialize};

/// Where an issue sits in its tracker's workflow. Linear's `state.type`
/// vocabulary, adopted as devkit's own because the status verdict, the triage
/// colours, and the dashboard bands were already written against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateKind {
    Triage,
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
}

impl StateKind {
    /// Every kind but the two terminal ones. The finished verdict and the
    /// dashboard's "open now" count both key off this.
    pub fn is_open(self) -> bool {
        !matches!(self, StateKind::Completed | StateKind::Canceled)
    }
}

impl std::fmt::Display for StateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StateKind::Triage => "triage",
            StateKind::Backlog => "backlog",
            StateKind::Unstarted => "unstarted",
            StateKind::Started => "started",
            StateKind::Completed => "completed",
            StateKind::Canceled => "canceled",
        })
    }
}

impl std::str::FromStr for StateKind {
    type Err = std::convert::Infallible;

    /// Never fails: a tracker may add a workflow state devkit has no name for,
    /// and one unknown state must not fail an entire status run.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "triage" => StateKind::Triage,
            "backlog" => StateKind::Backlog,
            "started" => StateKind::Started,
            "completed" => StateKind::Completed,
            "canceled" => StateKind::Canceled,
            _ => StateKind::Unstarted,
        })
    }
}

/// One issue's workflow state, as devkit renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub kind: StateKind,
    /// The tracker's own label, e.g. "In Progress" or "Not planned".
    pub name: String,
    /// Hex colour, when the tracker supplies one.
    pub color: Option<String>,
}
```

Add `serde_json` to `crates/devkit-common/Cargo.toml` `[dev-dependencies]` if
the round-trip test cannot see it — it is already a normal dependency, so it
should be visible.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-common tracker::`
Expected: PASS, 3 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/tracker/mod.rs crates/devkit-common/src/lib.rs
git commit -m "feat(tracker): add a typed issue state vocabulary"
```

### Task 5: the `Tracker` trait, the `None` implementation, and `[tracker]` config

**Files:**
- Modify: `crates/devkit-common/src/tracker/mod.rs`
- Create: `crates/devkit-common/src/tracker/none.rs`
- Modify: `crates/devkit-config/src/lib.rs` (add `TrackerKind`, `TrackerConfig`,
  `Config.tracker`)
- Modify: `crates/devkit-common/Cargo.toml` (depend on `devkit-config`)
- Test: `crates/devkit-common/src/tracker/mod.rs`, `crates/devkit-config/src/lib.rs`

**Interfaces:**
- Consumes: `StateKind`, `State` from Task 4.
- Produces:

```rust
// devkit_config
pub enum TrackerKind { Linear, Github, None }   // serde: "linear" | "github" | "none"
pub struct TrackerConfig { pub kind: Option<TrackerKind> }
// Config gains: #[serde(default)] pub tracker: TrackerConfig

// devkit_common::tracker
pub struct IssueRef { pub id: String, pub slug: Option<String> }
pub struct PrRef { pub url: String, pub number: u64 }
pub struct IssueDetails { /* fields copied verbatim from linear::IssueDetails */ }
pub struct AssignedIssue { pub identifier: String, pub created_at: String,
                           pub state: State,
                           pub history: Vec<(String, Option<State>, Option<State>)> }

pub trait Tracker: Send + Sync {
    fn kind(&self) -> TrackerKind;
    fn ready(&self) -> bool;
    fn issue_ref(&self, input: &str) -> IssueRef;
    fn title(&self, id: &str) -> Result<Option<String>>;
    fn details(&self, id: &str) -> Result<Option<IssueDetails>>;
    fn states(&self, ids: &[String]) -> HashMap<String, State>;
    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>>;
    fn candidates(&self, n: u64) -> Result<Vec<IssueRef>>;
    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>>;
    fn assigned_history(&self, on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>>;
    fn timeline_origin(&self) -> Result<Option<String>>;
    fn issue_url(&self, id: &str) -> Option<String>;
    fn check(&self) -> Result<String>;
}

pub fn resolve(kind: Option<TrackerKind>, repo: Option<&str>, cwd: &Path) -> Box<dyn Tracker>;
```

`Tracker: Send + Sync` because `status::gather` fetches inside
`std::thread::scope` (`status.rs:360`) and the tracker crosses that boundary.

`resolve` returns `None`'s implementation for `TrackerKind::Github` in this
phase, since the GitHub adapter arrives in phase 3. It must not silently
pretend: an explicit `kind = "github"` logs one line to stderr saying the GitHub
tracker is not implemented yet.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-config/src/lib.rs`, add to `mod tests`:

```rust
#[test]
fn tracker_kind_parses_from_the_table() {
    let c: Config = toml::from_str(
        "[defaults]\nworktree_root = \"/x\"\nbranch_prefix = \"l/\"\n\
         baseline_ref = \"origin/main\"\nbaseline_path = \"\"\n\
         [tracker]\nkind = \"github\"\n",
    )
    .unwrap();
    assert_eq!(c.tracker.kind, Some(TrackerKind::Github));
}

#[test]
fn an_absent_tracker_table_leaves_the_kind_unset() {
    let c: Config = toml::from_str(
        "[defaults]\nworktree_root = \"/x\"\nbranch_prefix = \"l/\"\n\
         baseline_ref = \"origin/main\"\nbaseline_path = \"\"\n",
    )
    .unwrap();
    assert_eq!(c.tracker.kind, None, "absent means detect, not linear");
}
```

In `crates/devkit-common/src/tracker/mod.rs`, add to `mod tests`:

```rust
#[test]
fn the_none_tracker_answers_empty_and_is_never_ready() {
    let t = resolve(Some(TrackerKind::None), None, Path::new("/nowhere"));
    assert_eq!(t.kind(), TrackerKind::None);
    assert!(!t.ready());
    assert!(t.states(&["ENG-1".into()]).is_empty());
    assert!(t.title("ENG-1").unwrap().is_none());
    assert!(t.details("ENG-1").unwrap().is_none());
    assert!(t.issue_url("ENG-1").is_none());
    assert!(t.candidates(7).unwrap().is_empty());
    assert!(t.assigned_history(&mut |_| {}).unwrap().is_empty());
}

#[test]
fn the_none_tracker_passes_an_id_through_unchanged() {
    let t = resolve(Some(TrackerKind::None), None, Path::new("/nowhere"));
    let r = t.issue_ref("  eng-1  ");
    assert_eq!(r.id, "eng-1");
    assert_eq!(r.slug, None);
}

#[test]
fn an_explicit_kind_is_never_overridden_by_detection() {
    let t = resolve(Some(TrackerKind::None), None, Path::new("/nowhere"));
    assert_eq!(t.kind(), TrackerKind::None);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-config tracker && cargo test -p devkit-common tracker::`
Expected: FAIL to compile, `cannot find type 'TrackerKind'`.

- [ ] **Step 3: Add the config types**

In `crates/devkit-config/src/lib.rs`, beside the other section structs:

```rust
/// Which issue tracker a project uses. Absent means detect: a resolvable
/// `LINEAR_API_KEY`, then a GitHub `origin` remote, then no tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    Linear,
    Github,
    None,
}

/// The `[tracker]` table.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct TrackerConfig {
    /// Force a tracker instead of detecting one.
    pub kind: Option<TrackerKind>,
}
```

Add to `Config`, beside `pub linear: LinearConfig` (line 28):

```rust
    /// Which issue tracker backs `issue`. Detected when the table is absent.
    #[serde(default)]
    pub tracker: TrackerConfig,
```

Add `"tracker"` to `STANDALONE_SECTIONS` only if a `[tracker]`-only config layer
should be legal without `[defaults]`. It should not — a tracker without a
project is meaningless — so leave that array alone.

- [ ] **Step 4: Add the trait and the `None` implementation**

Add `devkit-config.workspace = true` to `crates/devkit-common/Cargo.toml`
`[dependencies]`.

Append to `crates/devkit-common/src/tracker/mod.rs`, above `mod tests`:

```rust
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub use devkit_config::TrackerKind;

pub mod none;

/// An issue id parsed from CLI input, plus the title slug when the input
/// carried one (a pasted issue URL usually does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub id: String,
    pub slug: Option<String>,
}

/// A pull request linked to an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub url: String,
    pub number: u64,
}

/// One issue assigned to the current user, with its state transitions. Drives
/// the dashboard's issues-over-time chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignedIssue {
    pub identifier: String,
    pub created_at: String,
    pub state: State,
    /// `(when, from, to)` per recorded transition, unsorted.
    pub history: Vec<(String, Option<State>, Option<State>)>,
}

/// Everything the issue summary file renders. Every field is empty rather than
/// absent when the tracker has nothing there.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueDetails {
    pub id: String,
    pub title: String,
    pub url: String,
    pub description: String,
    pub state: String,
    pub assignee: String,
    pub priority: String,
    pub estimate: String,
    pub labels: Vec<String>,
    pub parent: String,
    pub project: String,
}

/// One issue tracker. Every method is read-only: `devkit-issue` is a triage
/// facade and never mutates a tracker.
pub trait Tracker: Send + Sync {
    fn kind(&self) -> TrackerKind;
    /// Configured and able to authenticate. False means callers should degrade
    /// rather than error.
    fn ready(&self) -> bool;
    /// Parse CLI input — a bare id, a `#123`, or an issue URL — into an id and,
    /// when the input spelled one out, a title slug.
    fn issue_ref(&self, input: &str) -> IssueRef;
    fn title(&self, id: &str) -> Result<Option<String>>;
    fn details(&self, id: &str) -> Result<Option<IssueDetails>>;
    /// Batched: one round trip for every id.
    fn states(&self, ids: &[String]) -> HashMap<String, State>;
    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>>;
    /// Issues that a bare number might refer to, for disambiguation.
    fn candidates(&self, n: u64) -> Result<Vec<IssueRef>>;
    /// PR URL to the issue ids it references.
    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>>;
    fn assigned_history(&self, on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>>;
    /// Earliest timestamp the dashboard's timeline should start from.
    fn timeline_origin(&self) -> Result<Option<String>>;
    fn issue_url(&self, id: &str) -> Option<String>;
    /// A one-line identity for `devkit doctor`.
    fn check(&self) -> Result<String>;
}

/// The tracker for this project. An explicit `kind` always wins; otherwise a
/// resolvable Linear key, then a GitHub `origin` remote, then no tracker.
///
/// Detection is a floor, not a convenience: a globally exported
/// `LINEAR_API_KEY` resolves to Linear for every project, so a GitHub project on
/// such a machine must set `kind` explicitly. What detection buys is that every
/// config predating `[tracker]` keeps behaving exactly as it did.
///
/// Both non-`None` arms return `NoneTracker` for now: Task 6 fills in Linear,
/// and phase 3 fills in GitHub.
```rust
pub fn resolve(kind: Option<TrackerKind>, repo: Option<&str>, cwd: &Path) -> Box<dyn Tracker> {
    let _ = repo; // consumed by the GitHub tracker in phase 3
    match kind.unwrap_or_else(|| detect(cwd)) {
        TrackerKind::Linear => Box::new(none::NoneTracker),
        TrackerKind::Github => {
            eprintln!("devkit: the GitHub tracker is not implemented yet — running without one");
            Box::new(none::NoneTracker)
        }
        TrackerKind::None => Box::new(none::NoneTracker),
    }
}

/// Detection order, used only when `[tracker] kind` is absent.
fn detect(cwd: &Path) -> TrackerKind {
    if crate::secrets::resolve("LINEAR_API_KEY").is_some() {
        return TrackerKind::Linear;
    }
    if crate::github::repo_slug(&cwd.to_string_lossy()).is_ok() {
        return TrackerKind::Github;
    }
    TrackerKind::None
}
```

Create `crates/devkit-common/src/tracker/none.rs`:

```rust
//! The tracker for a project that has none. Every answer is empty, which is
//! exactly how `issue` behaved before the seam existed when no Linear key was
//! set — expressed once here instead of as a `has_key` branch at each caller.

use super::{AssignedIssue, IssueDetails, IssueRef, PrRef, State, Tracker, TrackerKind};
use anyhow::Result;
use std::collections::HashMap;

pub struct NoneTracker;

impl Tracker for NoneTracker {
    fn kind(&self) -> TrackerKind {
        TrackerKind::None
    }
    fn ready(&self) -> bool {
        false
    }
    fn issue_ref(&self, input: &str) -> IssueRef {
        IssueRef {
            id: input.trim().to_string(),
            slug: None,
        }
    }
    fn title(&self, _id: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn details(&self, _id: &str) -> Result<Option<IssueDetails>> {
        Ok(None)
    }
    fn states(&self, _ids: &[String]) -> HashMap<String, State> {
        HashMap::new()
    }
    fn issue_pr(&self, _id: &str) -> Result<Option<PrRef>> {
        Ok(None)
    }
    fn candidates(&self, _n: u64) -> Result<Vec<IssueRef>> {
        Ok(Vec::new())
    }
    fn issues_for_prs(&self, _urls: &[String]) -> HashMap<String, Vec<String>> {
        HashMap::new()
    }
    fn assigned_history(&self, _on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>> {
        Ok(Vec::new())
    }
    fn timeline_origin(&self) -> Result<Option<String>> {
        Ok(None)
    }
    fn issue_url(&self, _id: &str) -> Option<String> {
        None
    }
    fn check(&self) -> Result<String> {
        Ok("no tracker configured".to_string())
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-config tracker && cargo test -p devkit-common tracker::`
Expected: PASS.

Then `cargo test --workspace`. The schema-drift test will now FAIL, because
`Config` gained a `[tracker]` table. That is expected; fix it in the next step
rather than reverting.

- [ ] **Step 6: Regenerate the schema and commit**

```bash
DEVKIT_UPDATE_SCHEMA=1 cargo test --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add -A
git commit -m "feat(tracker): add the Tracker trait and a no-tracker impl"
```

### Task 6: move Linear behind the trait

**Files:**
- Move: `crates/devkit-common/src/linear.rs` → `crates/devkit-common/src/tracker/linear.rs`
- Modify: `crates/devkit-common/src/lib.rs` (drop `pub mod linear;`)
- Modify: `crates/devkit-common/src/tracker/mod.rs` (`resolve` returns
  `LinearTracker`)
- Modify: `crates/devkit-common/src/github.rs` (receives `pr_number_from_url`)
- Modify: every file importing `devkit_common::linear`
- Test: `crates/devkit-common/src/tracker/linear.rs`

**Interfaces:**
- Consumes: `Tracker`, `State`, `StateKind`, `IssueRef`, `IssueDetails`,
  `AssignedIssue`, `PrRef`, `TrackerKind` from Tasks 4 and 5.
- Produces: `tracker::linear::LinearTracker` (a unit struct holding the resolved
  API key), and `devkit_common::github::pr_number_from_url`.

The existing free functions stay as they are. `LinearTracker` is a thin adapter
over them, so this task changes no Linear query except one: `build_query`
(`linear.rs:305`) gains `color` inside its `state { … }` selection so
`states()` can populate `State.color`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in what will be `crates/devkit-common/src/tracker/linear.rs`:

```rust
#[test]
fn linear_maps_its_state_types_onto_state_kinds() {
    let s = parse_state(&serde_json::json!({
        "type": "started", "name": "In Progress", "color": "#f2c94c"
    }));
    assert_eq!(s.kind, StateKind::Started);
    assert_eq!(s.name, "In Progress");
    assert_eq!(s.color.as_deref(), Some("#f2c94c"));
}

#[test]
fn linear_uppercases_a_bare_id() {
    let t = LinearTracker::new(Some("k".into()));
    assert_eq!(t.issue_ref("eng-42").id, "ENG-42");
}

#[test]
fn linear_reads_an_id_and_slug_from_a_url_by_path_position() {
    let t = LinearTracker::new(Some("k".into()));
    let r = t.issue_ref("https://linear.app/acme-2/issue/ENG-42/fix-the-login");
    assert_eq!(r.id, "ENG-42", "a workspace named acme-2 is not the issue id");
    assert_eq!(r.slug.as_deref(), Some("fix-the-login"));
}

#[test]
fn a_keyless_linear_tracker_is_not_ready_and_answers_empty() {
    let t = LinearTracker::new(None);
    assert!(!t.ready());
    assert!(t.states(&["ENG-1".into()]).is_empty());
    assert!(t.title("ENG-1").unwrap().is_none());
}

#[test]
fn the_batch_state_query_asks_for_a_colour() {
    let (q, _) = build_query(&["ENG-1".into()]).expect("a non-empty id list builds a query");
    assert!(q.contains("color"), "State.color needs the field selected: {q}");
}
```

`parse_state` is a new private helper this task adds; `build_query` already
exists at `linear.rs:305`.

- [ ] **Step 2: Move the file and run the tests to verify they fail**

```bash
git mv crates/devkit-common/src/linear.rs crates/devkit-common/src/tracker/linear.rs
```

Remove `pub mod linear;` from `crates/devkit-common/src/lib.rs` and add
`pub mod linear;` to `crates/devkit-common/src/tracker/mod.rs`.

Run: `cargo test -p devkit-common tracker::linear`
Expected: FAIL to compile, `cannot find function 'parse_state'`.

- [ ] **Step 3: Add the adapter**

Append to `crates/devkit-common/src/tracker/linear.rs`:

```rust
use super::{
    AssignedIssue, IssueDetails, IssueRef, PrRef, State, StateKind, Tracker, TrackerKind,
};

/// Linear's `state { type name color }` block as devkit's `State`.
fn parse_state(v: &serde_json::Value) -> State {
    State {
        kind: v["type"].as_str().unwrap_or("").parse().expect("infallible"),
        name: v["name"].as_str().unwrap_or("").to_string(),
        color: v["color"].as_str().map(str::to_string),
    }
}

/// Linear behind the tracker seam. Holds the API key resolved once at
/// construction; `None` means every call degrades to empty rather than erroring,
/// which is what keeps `issue status` useful on a machine with no key.
pub struct LinearTracker {
    key: Option<String>,
}

impl LinearTracker {
    pub fn new(key: Option<String>) -> Self {
        Self { key }
    }
}

impl Tracker for LinearTracker {
    fn kind(&self) -> TrackerKind {
        TrackerKind::Linear
    }

    fn ready(&self) -> bool {
        self.key.is_some()
    }

    fn issue_ref(&self, input: &str) -> IssueRef {
        let trimmed = input.trim();
        if trimmed.contains("linear.app")
            && let Some(parsed) = url_ref(trimmed)
        {
            return parsed;
        }
        IssueRef {
            id: trimmed.to_uppercase(),
            slug: None,
        }
    }

    fn title(&self, id: &str) -> Result<Option<String>> {
        match &self.key {
            Some(k) => issue_title(id, k),
            None => Ok(None),
        }
    }

    fn details(&self, id: &str) -> Result<Option<IssueDetails>> {
        match &self.key {
            Some(k) => Ok(issue_details(id, k)?.map(Into::into)),
            None => Ok(None),
        }
    }

    fn states(&self, ids: &[String]) -> HashMap<String, State> {
        states(ids, self.key.as_deref())
    }

    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>> {
        match &self.key {
            Some(k) => Ok(issue_pr(id, k)?.0.map(|p| PrRef {
                url: p.url,
                number: p.number,
            })),
            None => Ok(None),
        }
    }

    fn candidates(&self, n: u64) -> Result<Vec<IssueRef>> {
        match &self.key {
            Some(k) => Ok(issues_by_number(n, k)?
                .into_iter()
                .map(|c| IssueRef {
                    id: c.id,
                    slug: None,
                })
                .collect()),
            None => Ok(Vec::new()),
        }
    }

    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>> {
        issues_for_prs(urls, self.key.as_deref())
    }

    fn assigned_history(&self, on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>> {
        match &self.key {
            Some(k) => assigned_issue_history_with_progress(k, on_page),
            None => Ok(Vec::new()),
        }
    }

    fn timeline_origin(&self) -> Result<Option<String>> {
        match &self.key {
            Some(k) => viewer_created_at(k).map(Some),
            None => Ok(None),
        }
    }

    fn issue_url(&self, id: &str) -> Option<String> {
        let ws = workspace_url_key()?;
        Some(format!("https://linear.app/{ws}/issue/{id}"))
    }

    fn check(&self) -> Result<String> {
        let key = self
            .key
            .as_deref()
            .context("no Linear API key — run `devkit auth linear`")?;
        let id = validate(key)?;
        Ok(format!("linear: {} ({})", id.org_name, id.viewer_email))
    }
}
```

Three supporting changes go with it:

1. `states` (`linear.rs:325`) and `assigned_issue_history_with_progress`
   (`linear.rs:511`) return `State` instead of `LinearState`/`StateRef`. Delete
   both old structs and route every construction through `parse_state`.
2. `build_query` (`linear.rs:305`) selects `color` inside `state { … }`.
3. `IssueDetails` in this file is renamed away; implement
   `From<self::IssueDetails> for super::IssueDetails` only if the field sets
   differ. If they match, delete the local one and use the shared type
   throughout.

Move `pr_number_from_url` (`linear.rs:33`) and its tests into
`crates/devkit-common/src/github.rs`, and update the one caller,
`src/bin/issue/checkout.rs:34`.

- [ ] **Step 4: Point `resolve` at it**

In `crates/devkit-common/src/tracker/mod.rs`, replace the `TrackerKind::Linear`
arm:

```rust
        TrackerKind::Linear => Box::new(linear::LinearTracker::new(
            crate::secrets::resolve("LINEAR_API_KEY"),
        )),
```

- [ ] **Step 5: Update every `linear::` importer**

```bash
rg -l 'devkit_common::linear|common::linear' --glob '*.rs' \
  | xargs sed -i 's/devkit_common::linear/devkit_common::tracker::linear/g'
```

Then fix the residue by hand: `LinearState` and `StateRef` no longer exist, so
`crates/devkit-issue/src/status.rs`, `src/bin/issue/dashboard/bucket.rs`, and
`src/bin/issue/dashboard/data.rs` need their imports changed to `State`. Task 7
reshapes those call sites properly; here, do the minimum that compiles.

- [ ] **Step 6: Run the tests to verify they pass**

Run:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Expected: PASS. Every pre-existing Linear test moved with the file and must
still pass unchanged — if one needs editing, the adapter changed behavior it
should not have.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(tracker): put Linear behind the Tracker trait

The free functions are unchanged; LinearTracker adapts them. The batch
state query gains a colour so State.color is populated on both the
status and history paths, and pr_number_from_url moves to the github
module, which is what it always parsed."
```

### Task 7: reshape the status report and inject the tracker

**Files:**
- Modify: `crates/devkit-issue/src/status.rs` (`IssueWorktree`, `StatusReport`,
  `assemble`, `gather`, `gather_local`, `reason_not_finished`)
- Modify: `crates/devkit-issue/Cargo.toml` (no change expected; it already
  depends on `devkit-common`)
- Modify: `crates/devkit-mcp/src/issue.rs`, `src/bin/issue/triage.rs`,
  `src/bin/issue/status.rs`, `src/bin/issue/end.rs`,
  `src/bin/issue/dashboard/mod.rs`, `src/bin/issue/dashboard/data.rs`,
  `src/bin/issue/dashboard/bucket.rs`
- Create: `crates/devkit-common/src/tracker/fake.rs`
- Test: `crates/devkit-issue/src/status.rs`

**Interfaces:**
- Consumes: `Tracker`, `State`, `StateKind`, `TrackerKind` from Tasks 4–6.
- Produces:

```rust
// devkit_issue::status
pub struct IssueWorktree { /* … */ pub state: Option<State> }   // replaces linear_kind/linear_name
pub struct TrackerInfo { pub kind: TrackerKind, pub ready: bool, pub link_base: Option<String> }
pub struct StatusReport { pub worktrees: Vec<IssueWorktree>, pub finished_count: usize,
                          pub tracker: TrackerInfo }
pub fn gather_with(start: &str, ids: &[String], t: &dyn Tracker) -> Result<StatusReport>;
pub fn gather(start: &str, ids: &[String]) -> Result<StatusReport>;   // resolves, then delegates
pub fn assemble(d: Discovered, dirty: Vec<bool>, prs: Prs,
                states: HashMap<String, State>, tracker: TrackerInfo) -> StatusReport;

// devkit_common::tracker::fake
pub struct FakeTracker { pub states: HashMap<String, State>, pub ready: bool }
```

This is the task the whole seam was for. `gather` (`status.rs:344`) currently
spawns threads that make real network calls, so nothing above `assemble` has
ever been tested. `gather_with` takes the tracker as an argument and `gather`
becomes a two-line wrapper that resolves one.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/devkit-issue/src/status.rs`:

```rust
use devkit_common::tracker::fake::FakeTracker;

fn done(name: &str) -> State {
    State { kind: StateKind::Completed, name: name.into(), color: None }
}

#[test]
fn a_merged_clean_worktree_with_a_completed_issue_is_finished() {
    let t = FakeTracker::ready([("ENG-1", done("Done"))]);
    let report = assemble(
        discovered_for_test(&[("ENG-1", "lev/eng-1-fix")]),
        vec![false],
        prs_for_test(&[("lev/eng-1-fix", "MERGED", 10)]),
        t.states(&["ENG-1".into()]),
        TrackerInfo { kind: TrackerKind::Linear, ready: true, link_base: None },
    );
    assert!(report.worktrees[0].finished);
    assert_eq!(report.finished_count, 1);
}

#[test]
fn an_open_issue_is_not_finished_and_says_why() {
    let mut st = done("In Progress");
    st.kind = StateKind::Started;
    let report = assemble(
        discovered_for_test(&[("ENG-2", "lev/eng-2-wip")]),
        vec![false],
        prs_for_test(&[("lev/eng-2-wip", "MERGED", 11)]),
        HashMap::from([("ENG-2".to_string(), st)]),
        TrackerInfo { kind: TrackerKind::Linear, ready: true, link_base: None },
    );
    assert!(!report.worktrees[0].finished);
    let why = report.worktrees[0].reason_not_finished.as_deref().unwrap();
    assert!(why.contains("In Progress"), "the reason names the state: {why}");
}

#[test]
fn with_no_tracker_a_merged_clean_worktree_is_finished_without_a_state() {
    let report = assemble(
        discovered_for_test(&[("UNKNOWN", "lev/some-branch")]),
        vec![false],
        prs_for_test(&[("lev/some-branch", "MERGED", 12)]),
        HashMap::new(),
        TrackerInfo { kind: TrackerKind::None, ready: false, link_base: None },
    );
    assert!(
        report.worktrees[0].finished,
        "a project with no tracker still finishes on PR merged + clean"
    );
}

#[test]
fn gather_with_drives_the_whole_path_against_a_fake_tracker() {
    // The point of the seam: no network, and the state actually lands on a row.
    let t = FakeTracker::ready([("ENG-9", done("Done"))]);
    let tmp = fixture_repo_with_worktree("ENG-9", "lev/eng-9-thing");
    let report = gather_with(&tmp.to_string_lossy(), &[], &t).unwrap();
    let row = report
        .worktrees
        .iter()
        .find(|w| w.issue_id == "ENG-9")
        .expect("the fixture worktree is discovered");
    assert_eq!(row.state.as_ref().unwrap().kind, StateKind::Completed);
    assert_eq!(report.tracker.kind, TrackerKind::Linear);
}
```

`discovered_for_test`, `prs_for_test`, and `fixture_repo_with_worktree` are test
helpers. `Discovered::for_test` already exists at `status.rs:467`; write the
other two beside it. `fixture_repo_with_worktree` runs `git init`, commits an
empty tree, and adds one worktree on the named branch — reuse whatever the
existing `crates/devkit-issue/tests/gather_local.rs` already does to build a
repo rather than writing a second version.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-issue status::tests`
Expected: FAIL to compile, `cannot find function 'gather_with'` and
`no field 'state' on type 'IssueWorktree'`.

- [ ] **Step 3: Write the fake tracker**

Create `crates/devkit-common/src/tracker/fake.rs`:

```rust
//! A tracker that answers from a fixed map. Lets `devkit-issue` exercise the
//! whole gather path — discovery, state attachment, finished verdict — with no
//! network and no credentials.

use super::{AssignedIssue, IssueDetails, IssueRef, PrRef, State, Tracker, TrackerKind};
use anyhow::Result;
use std::collections::HashMap;

pub struct FakeTracker {
    pub states: HashMap<String, State>,
    pub ready: bool,
}

impl FakeTracker {
    /// A ready tracker knowing exactly these issue states.
    pub fn ready<const N: usize>(rows: [(&str, State); N]) -> Self {
        Self {
            states: rows.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            ready: true,
        }
    }
}

impl Tracker for FakeTracker {
    fn kind(&self) -> TrackerKind {
        TrackerKind::Linear
    }
    fn ready(&self) -> bool {
        self.ready
    }
    fn issue_ref(&self, input: &str) -> IssueRef {
        IssueRef { id: input.trim().to_uppercase(), slug: None }
    }
    fn title(&self, id: &str) -> Result<Option<String>> {
        Ok(self.states.get(id).map(|s| s.name.clone()))
    }
    fn details(&self, _id: &str) -> Result<Option<IssueDetails>> {
        Ok(None)
    }
    fn states(&self, ids: &[String]) -> HashMap<String, State> {
        ids.iter()
            .filter_map(|i| self.states.get(i).map(|s| (i.clone(), s.clone())))
            .collect()
    }
    fn issue_pr(&self, _id: &str) -> Result<Option<PrRef>> {
        Ok(None)
    }
    fn candidates(&self, _n: u64) -> Result<Vec<IssueRef>> {
        Ok(Vec::new())
    }
    fn issues_for_prs(&self, _urls: &[String]) -> HashMap<String, Vec<String>> {
        HashMap::new()
    }
    fn assigned_history(&self, _on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>> {
        Ok(Vec::new())
    }
    fn timeline_origin(&self) -> Result<Option<String>> {
        Ok(None)
    }
    fn issue_url(&self, id: &str) -> Option<String> {
        Some(format!("https://example.test/issue/{id}"))
    }
    fn check(&self) -> Result<String> {
        Ok("fake tracker".to_string())
    }
}
```

Gate it in `crates/devkit-common/src/tracker/mod.rs` so it is available to other
crates' tests without shipping in release builds:

```rust
#[cfg(any(test, feature = "test-support"))]
pub mod fake;
```

Add to `crates/devkit-common/Cargo.toml`:

```toml
[features]
test-support = []
```

and to `crates/devkit-issue/Cargo.toml`:

```toml
[dev-dependencies]
devkit-common = { workspace = true, features = ["test-support"] }
```

- [ ] **Step 4: Reshape the report**

In `crates/devkit-issue/src/status.rs`:

Replace the two Linear fields on `IssueWorktree` (line 44-45):

```rust
    /// The tracker's state for this issue, absent when the tracker has no row
    /// for it or there is no tracker.
    pub state: Option<State>,
```

Replace the two on `StatusReport` (lines 55-56):

```rust
    pub tracker: TrackerInfo,
```

and add:

```rust
/// Which tracker produced this report and whether it could answer.
#[derive(Debug, Clone, Serialize)]
pub struct TrackerInfo {
    pub kind: TrackerKind,
    /// Configured and able to authenticate. False means the state column is
    /// blank because there is nothing to ask, not because the issue is unknown.
    pub ready: bool,
    /// Prefix for building issue links, when the tracker has one.
    pub link_base: Option<String>,
}
```

Change `assemble`'s signature and its state-attachment block:

```rust
pub fn assemble(
    d: Discovered,
    dirty: Vec<bool>,
    prs: Prs,
    states: HashMap<String, State>,
    tracker: TrackerInfo,
) -> StatusReport {
```

```rust
        if let Some(st) = states.get(&wt.issue_id) {
            wt.state = Some(st.clone());
        }
        let reason = reason_not_finished(wt, tracker.ready, false);
```

Change `reason_not_finished` (`status.rs:318-326`) to read the enum:

```rust
    match wt.state.as_ref() {
        None if has_tracker => bits.push("tracker state unknown".into()),
        None => bits.push("no tracker".into()),
        Some(s) if s.kind != StateKind::Completed => {
            bits.push(format!("{} {}", tracker_label, s.name))
        }
        Some(_) => {}
    }
```

Keep whatever `tracker_label` the surrounding code already builds; if it
hardcodes the word "Linear", replace it with the tracker's name so the message
does not lie under a different provider.

Split `gather`:

```rust
/// Discover worktrees, fetch PRs and tracker state concurrently, and compute
/// the finished verdict.
pub fn gather(start: &str, ids: &[String]) -> Result<StatusReport> {
    let t = devkit_common::tracker::resolve(None, None, Path::new(start));
    gather_with(start, ids, t.as_ref())
}

/// `gather` against a caller-supplied tracker. Tests inject a fake; callers
/// that already resolved one avoid resolving it twice.
pub fn gather_with(start: &str, ids: &[String], t: &dyn Tracker) -> Result<StatusReport> {
```

and inside it, replace the Linear thread (`status.rs:360-373`) with one calling
`t.states(&ids_v)` and `t.issue_url("")` for the link base. The thread scope
stays; `Tracker: Send + Sync` is what allows `t` to cross it.

- [ ] **Step 5: Fix the consumers**

Five files read the removed fields. Each change is mechanical:

- `src/bin/issue/triage.rs:61` — rename `linear_cell` to `state_cell`, take
  `row.state.as_ref()`, match on `StateKind` exhaustively:

```rust
pub(crate) fn state_cell(row: &IssueWorktree, ready: bool) -> String {
    match row.state.as_ref() {
        None => ui::dim(if ready { "unknown" } else { "no tracker" }),
        Some(s) => match s.kind {
            StateKind::Completed => ui::green(&s.name),
            StateKind::Started => ui::yellow(&s.name),
            StateKind::Canceled => ui::red(&s.name),
            StateKind::Triage | StateKind::Backlog | StateKind::Unstarted => ui::dim(&s.name),
        },
    }
}
```

- `src/bin/issue/dashboard/mod.rs:83-85` — the sort rank matches `StateKind`
  exhaustively instead of three strings.
- `src/bin/issue/dashboard/mod.rs:142` — `.filter(|i| i.state.kind.is_open())`.
- `src/bin/issue/end.rs:205` — `report.has_linear_key` becomes
  `report.tracker.ready`.
- `crates/devkit-mcp/src/issue.rs` — no field names are spelled there beyond
  serialization, so it should compile untouched; if it does not, the JSON shape
  change is intended and approved.

Rename the user-facing strings while you are in these files: "no Linear key" to
"no tracker", "Linear unknown" to "tracker state unknown".

- [ ] **Step 6: Run the tests to verify they pass**

Run:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Expected: PASS, including the four new tests. Then run the CLI against this
repository and eyeball it:

```bash
cargo run --bin issue -- status
```

Expected: the same table as before the refactor, with the state column reading
"no tracker" rather than "no key" where it is blank.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(issue): report tracker state instead of Linear state

IssueWorktree.state replaces linear_kind/linear_name and
StatusReport.tracker replaces has_linear_key/linear_workspace, which
changes the MCP issue handler's JSON shape.

gather_with takes the tracker as an argument, so the gather path is
testable without a network for the first time."
```

### Task 8: record-first issue-id recovery

**Files:**
- Move: `src/bin/issue/record.rs` → `crates/devkit-common/src/record.rs`
- Modify: `crates/devkit-common/src/worktree.rs` (`issue_id_of` becomes
  record-first), `crates/devkit-common/src/lib.rs` (add `pub mod record;`)
- Modify: `src/bin/issue/main.rs` (drop `mod record;`),
  `src/bin/issue/setup.rs`, `src/bin/issue/checkout.rs`, `src/bin/issue/end.rs`,
  `src/bin/issue/review/mod.rs` (import the moved type)
- Test: `crates/devkit-common/src/worktree.rs`

**Interfaces:**
- Consumes: `Tracker::issue_ref` from Task 5.
- Produces: `devkit_common::record::{IssueRecord, read, write}` and
  `devkit_common::worktree::issue_id_of(worktree: &Path, branch: &str) -> String`.

Note the argument order change: `issue_id_of` currently takes `(branch, path)`
(`worktree.rs:37`). The record is now the primary source, so the path leads.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/devkit-common/src/worktree.rs`:

```rust
#[test]
fn the_record_wins_over_the_branch_name() {
    let dir = std::env::temp_dir().join(format!("devkit-idrec-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".devkit")).unwrap();
    std::fs::write(
        dir.join(".devkit").join("issue.toml"),
        "issue = \"87\"\nslug = \"fix\"\napps = []\n",
    )
    .unwrap();
    // The branch carries a Linear-shaped id that is NOT this worktree's issue.
    assert_eq!(issue_id_of(&dir, "lev/eng-1-something"), "87");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn without_a_record_the_branch_scan_still_works() {
    let dir = std::env::temp_dir().join(format!("devkit-idbranch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(issue_id_of(&dir, "lev/eng-1-something"), "ENG-1");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_worktree_with_neither_is_unknown() {
    let dir = std::env::temp_dir().join(format!("devkit-idnone-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(issue_id_of(&dir, "lev/no-id-here"), "UNKNOWN");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_numeric_record_id_is_not_uppercased_into_nonsense() {
    let dir = std::env::temp_dir().join(format!("devkit-idnum-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".devkit")).unwrap();
    std::fs::write(
        dir.join(".devkit").join("issue.toml"),
        "issue = \"87\"\nslug = \"x\"\napps = []\n",
    )
    .unwrap();
    assert_eq!(issue_id_of(&dir, "DETACHED"), "87");
    std::fs::remove_dir_all(&dir).ok();
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common worktree::`
Expected: FAIL — `issue_id_of` takes its arguments the other way round, and the
record tests fail because nothing reads `.devkit/issue.toml`.

- [ ] **Step 3: Move the record and rewrite the lookup**

```bash
git mv src/bin/issue/record.rs crates/devkit-common/src/record.rs
```

Add `pub mod record;` to `crates/devkit-common/src/lib.rs`, change
`pub(crate) fn` to `pub fn` on `write` and `read`, and drop `mod record;` from
`src/bin/issue/main.rs`.

Replace `issue_id_of` in `crates/devkit-common/src/worktree.rs:37`:

```rust
/// This worktree's issue id. The setup record is authoritative because it holds
/// whatever the tracker actually calls the issue; the branch and directory scan
/// is the fallback that keeps worktrees made before the record — or by a plain
/// `git worktree add` — working.
pub fn issue_id_of(worktree: &std::path::Path, branch: &str) -> String {
    if let Some(rec) = crate::record::read(worktree)
        && !rec.issue.is_empty()
    {
        return rec.issue;
    }
    let dir = worktree
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    for src in [branch, dir] {
        if let Some(m) = find_id(src) {
            return m.to_uppercase();
        }
    }
    "UNKNOWN".into()
}
```

The record value is returned verbatim. Normalization is the tracker's job now
(`Tracker::issue_ref`), which is what stops a numeric GitHub id being uppercased
into itself while a Linear id keeps its case handling.

Update the one caller, `crates/devkit-issue/src/status.rs:141`:

```rust
        let iid = worktree::issue_id_of(Path::new(&wt.path), &wt.branch);
```

- [ ] **Step 4: Delegate `classify` to the tracker**

In `src/bin/issue/checkout.rs:31`, `classify` hardcodes both Linear and GitHub
URL shapes. Keep only the two rules that are true for any tracker, and hand the
rest over:

```rust
/// Classify the identifier by shape. The two PR rules are tracker-independent;
/// anything else is the tracker's to recognize.
fn classify(input: &str, t: &dyn Tracker) -> Result<Ident> {
    let s = input.trim();
    if s.contains("github.com") && s.contains("/pull/") {
        let n = devkit_common::github::pr_number_from_url(s)
            .context("no PR number in GitHub URL")?;
        return Ok(Ident::Pr(n));
    }
    if let Some(rest) = s.strip_prefix('#')
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(Ident::Pr(rest.parse().context("bad PR number")?));
    }
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Ident::Fuzzy(s.parse().context("bad number")?));
    }
    let r = t.issue_ref(s);
    anyhow::ensure!(!r.id.is_empty(), "unrecognized PR/issue identifier: {s}");
    Ok(Ident::Issue(r))
}
```

Rename the `Ident::Linear(String)` variant to `Ident::Issue(IssueRef)` and
update its two match arms in `resolve` (`checkout.rs:164-190`). `resolve_linear`
becomes `resolve_issue` and calls `t.issue_pr(&r.id)` instead of
`linear::issue_pr(id, key)`.

- [ ] **Step 5: Run the tests to verify they pass**

Run:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Expected: PASS. `classify`'s existing tests need a tracker argument — pass
`&LinearTracker::new(Some("k".into()))` so their expectations hold unchanged.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(issue): read the issue id from the setup record first

issue_id_of scanned the branch for a letters-dash-digits run, which is a
Linear id shape. The per-worktree record already holds whatever the
tracker calls the issue; the scan stays as the fallback for worktrees
that predate it."
```

### Task 9: documentation

**Files:**
- Modify: `AGENTS.md`, `docs/configuration.md`, `README.md`
- Modify: `schema/devkit-config.json` (regenerated, not hand-edited)

**Interfaces:**
- Consumes: everything above.
- Produces: nothing code depends on.

- [ ] **Step 1: Update the crate table in `AGENTS.md`**

Add a row, in the table's existing order:

```markdown
| `crates/devkit-config` | lib: the `devkit.toml` shape, layer discovery and merge, per-leaf provenance, and the derived JSON Schema. A leaf crate — no internal dependencies — so `devkit-common`, `devkit-ports`, `devkit-locks`, `devkit-issue`, and `devkit-docs` can all read config |
```

Change the `devkit-common` row: drop `linear`, add `tracker`. Change the
`devkit-ports` row: drop `config` from its module list.

Under Conventions, replace the `Role` bullet's neighbours with a new one:

```markdown
- The issue tracker is a `Tracker` trait in `devkit-common::tracker`, selected by
  `[tracker] kind` or detected. `StateKind` (Triage/Backlog/Unstarted/Started/
  Completed/Canceled) is the state vocabulary every provider maps onto — match it
  exhaustively, no `_ =>` arms. A project with no tracker gets `NoneTracker`,
  whose empty answers are how `issue` degrades.
```

- [ ] **Step 2: Document `[tracker]` in `docs/configuration.md`**

```markdown
## `[tracker]`

Which issue tracker backs the `issue` commands.

| Key | Default | Meaning |
|---|---|---|
| `kind` | _(detect)_ | `linear`, `github`, or `none`. |

When `kind` is absent, devkit detects: a resolvable `LINEAR_API_KEY` means
Linear, otherwise a GitHub `origin` remote means GitHub, otherwise no tracker.

Detection is a floor, not a convenience. A `LINEAR_API_KEY` exported globally
resolves to Linear for *every* project, so a GitHub project on such a machine
must set `kind = "github"` explicitly. What detection buys is that a config
written before `[tracker]` existed keeps behaving exactly as it did.
`devkit doctor` prints which tracker resolved and why.

With `kind = "none"`, `issue` still creates worktrees, tracks PRs, and computes
the finished verdict from PR-merged-plus-clean. It cannot derive a slug from an
issue title, write a summary file, or draw the dashboard's issue timeline.
```

- [ ] **Step 3: Regenerate the schema**

```bash
DEVKIT_UPDATE_SCHEMA=1 cargo test --workspace
git diff --stat schema/devkit-config.json
```

Expected: the diff adds `TrackerConfig` and `TrackerKind` definitions and a
`tracker` property. If it shows unrelated churn, something else changed the
config types and needs explaining before you commit.

- [ ] **Step 4: Full gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Expected: all three PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: document the tracker seam and devkit-config"
```

## Self-review notes

Checked against the spec; two gaps are deliberate and named here so a reader
does not mistake them for oversights.

1. **`Tracker::check` has no consumer in this plan.** `devkit doctor`'s tracker
   row and the `gh auth login` guidance are phase-3 work, since neither means
   anything while GitHub is unimplemented. The method exists now because Task 6
   implements it for Linear from the existing `validate`, and leaving it out
   would mean reopening the trait in phase 3.
2. **Conventional-title parsing is phase 3.** The spec groups it with the GitHub
   work because that is where the titles it targets live. It touches
   `slug.rs` and the template context, neither of which this plan modifies.

Type consistency was checked across tasks: `StateKind` and `TrackerKind` are
never confused, `State.color` is `Option<String>` at every use, `assemble` takes
`HashMap<String, State>` in both its definition (Task 7, Step 4) and its callers
(Task 7, Step 1), and `issue_id_of` takes `(&Path, &str)` in both its definition
and its one caller.
