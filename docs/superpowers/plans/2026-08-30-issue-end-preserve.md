# Preserving files on `issue end` — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `devkit.toml` name files inside an issue worktree that must survive `issue end`, with a per-entry destination path, so an agent's scratch output is archived instead of deleted with the worktree.

**Architecture:** A new `[preserve.<name>]` config table pairs glob patterns with a destination template. `issue end`'s `run` splits into three phases — confirm every worktree, preserve serially, then remove in parallel as it does today. The copier is a `copy_out` sibling of the existing `copy_includes` in `devkit-common::worktree`, so preservation reuses the glob walk that `defaults.worktree_include` already uses in the inbound direction.

**Tech Stack:** Rust edition 2024, `anyhow`, `serde` + `schemars`, `minijinja` (via `devkit_common::template`), the `glob` crate, `tempfile` for test scratch.

**Spec:** `docs/superpowers/specs/2026-08-30-issue-end-preserve-design.md`

## Global Constraints

- **Worktree, not the primary clone.** All work happens in `../devkit-worktrees/issue-end-preserve` on branch `issue-end-preserve`. Never `git checkout` a branch in `C:/Users/Lev/Git/lev/devkit`.
- **TDD.** Write the failing test, run it, watch it fail for the right reason, then implement. Every task below is ordered that way.
- **The merge gate is all three:** `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`. CI runs them on ubuntu, macos, and windows.
- **Test scratch comes from `tempfile`.** `tempfile::tempdir()` for a directory, paths joined onto it for files. Never build a path by hand from `std::env::temp_dir()`. Bind the `TempDir` for as long as the path is used.
- **Comments are timeless.** No `this PR`, no `now we`, no `previously`, no task or issue references. A comment states what the code does and the non-obvious why, in the present tense.
- **No `_ =>` catch-all arms** on `Health`, `StateKind`, or `Role`. Match exhaustively.
- **Conventional Commits**, imperative mood, subject ≤50 chars, lowercase after the colon, no trailing period.
- **Windows is a first-class target.** `cleanup` canonicalizes paths, which yields `\\?\`-prefixed paths on Windows. Any path comparison or glob input must account for that.

---

### Task 1: Guard empty patterns in `plan_includes`

An empty pattern is a latent bug on the existing inbound path, not only the new outbound one. `source.join("")` is the source directory itself, the glob matches it, `strip_prefix` yields `""`, and `plan_dir` then plans every file under the root. Today `worktree_include = [""]` would copy an entire primary checkout into a new worktree. Fixing the shared function covers both callers.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs:148-152` (the `for pattern in patterns` head of `plan_includes`)
- Test: `crates/devkit-common/src/worktree.rs` (the existing `mod tests` at the bottom of the same file)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `plan_includes(source: &Path, dest: &Path, patterns: &[String]) -> IncludePlan` now ignores any pattern that is empty after `trim_end_matches('/')`. Task 2 relies on this so `copy_out` does not need its own empty check.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `crates/devkit-common/src/worktree.rs`. The module already has a `write` helper used by the existing include tests; reuse it.

```rust
/// `source.join("")` is the source directory, which globs to itself and then
/// strips to an empty relative path — planning the entire tree. A pattern that
/// is empty, or only separators, has to drop out before the join.
#[test]
fn an_empty_pattern_plans_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    write(&src.join("a.txt"), "a");
    write(&src.join("nested/b.txt"), "b");

    let plan = plan_includes(
        &src,
        &dst,
        &["".to_string(), "/".to_string(), "//".to_string()],
    );

    assert!(plan.missing.is_empty(), "planned {:?}", plan.missing);
    assert!(plan.existing.is_empty(), "planned {:?}", plan.existing);
    assert!(plan.warnings.is_empty(), "warned {:?}", plan.warnings);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-common an_empty_pattern_plans_nothing`

Expected: FAIL. `plan.missing` contains `a.txt` and `nested/b.txt`, so the first assertion trips with `planned ["a.txt", "nested/b.txt"]`. That is the bug, not a setup error.

- [ ] **Step 3: Write minimal implementation**

In `plan_includes`, immediately after the existing `trim_end_matches`:

```rust
    for pattern in patterns {
        let trimmed = pattern.trim_end_matches('/');
        // An empty pattern joins to `source` itself, which globs to the source
        // directory and strips to an empty relative path — planning every file
        // under the root. Drop it before the join rather than after.
        if trimmed.is_empty() {
            continue;
        }
        let joined = source.join(trimmed);
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-common`

Expected: PASS, including the existing include tests (`copy_includes` round trip, `plan_includes` classification, the overwrite tests) — the guard must not change any of them.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/worktree.rs
git commit -m "fix(worktree): drop an empty include pattern before the join"
```

---

### Task 2: Add `copy_out` with containment guards

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` (add `copy_out` and its private `escapes` helper beside `copy_includes` at line 73)
- Test: `crates/devkit-common/src/worktree.rs` (`mod tests`)

**Interfaces:**
- Consumes: `plan_includes` (Task 1's empty-pattern guard), `apply_includes(source, dest, plan, overwrite)`, `IncludePlan`.
- Produces: `pub fn copy_out(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>)`. Task 5 calls this once per resolved entry.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/devkit-common/src/worktree.rs`:

```rust
/// `plan_includes` strips `source` lexically, so `source.join("../x")` still
/// carries the prefix and yields `../x` as the "relative" path — escaping the
/// destination as well as the source. Absolute patterns replace the base
/// outright. Neither may reach the glob.
#[test]
fn copy_out_refuses_a_pattern_that_escapes_the_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("wt");
    let dst = dir.path().join("archive");
    write(&dir.path().join("outside.md"), "secret");
    write(&wt.join("keep.md"), "keep");

    let (copied, warnings) = copy_out(
        &wt,
        &dst,
        &[
            "../outside.md".to_string(),
            "keep.md".to_string(),
        ],
    );

    assert_eq!(copied, 1, "only the in-tree file is copied");
    assert!(dst.join("keep.md").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("outside.md")).unwrap(),
        "secret",
        "the file outside the worktree is untouched"
    );
    assert_eq!(warnings.len(), 1, "one warning names the pattern: {warnings:?}");
    assert!(warnings[0].contains("../outside.md"), "{warnings:?}");
}

/// The policy difference between the two directions, asserted together so a
/// future edit cannot flip one without the other failing: a backfill never
/// clobbers, an archive always does.
#[test]
fn copy_out_overwrites_where_copy_includes_leaves_alone() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dst = dir.path().join("dst");
    write(&src.join("notes.md"), "new");
    write(&dst.join("notes.md"), "old");

    let (copied, warnings) = copy_includes(&src, &dst, &["notes.md".to_string()]);
    assert_eq!(copied, 0, "backfill skips an existing file");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(std::fs::read_to_string(dst.join("notes.md")).unwrap(), "old");

    let (copied, warnings) = copy_out(&src, &dst, &["notes.md".to_string()]);
    assert_eq!(copied, 1, "archive replaces it");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(std::fs::read_to_string(dst.join("notes.md")).unwrap(), "new");
}

/// A directory pattern archives recursively, at the same relative path.
#[test]
fn copy_out_copies_a_directory_match_recursively() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("wt");
    let dst = dir.path().join("archive");
    write(&wt.join("graphify-out/a.md"), "a");
    write(&wt.join("graphify-out/deep/b.md"), "b");

    let (copied, warnings) = copy_out(&wt, &dst, &["graphify-out/".to_string()]);

    assert_eq!(copied, 2, "{warnings:?}");
    assert_eq!(
        std::fs::read_to_string(dst.join("graphify-out/deep/b.md")).unwrap(),
        "b"
    );
}

/// A pattern that matches nothing is the normal case for a worktree that
/// produced no scratch, and must not warn.
#[test]
fn copy_out_is_quiet_when_a_pattern_matches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let wt = dir.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    let dst = dir.path().join("archive");

    let (copied, warnings) = copy_out(&wt, &dst, &["graphify-out/**".to_string()]);

    assert_eq!(copied, 0);
    assert!(warnings.is_empty(), "{warnings:?}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-common copy_out`

Expected: FAIL to compile with `cannot find function 'copy_out' in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/devkit-common/src/worktree.rs` directly after `copy_includes`. `Component` needs importing from `std::path`.

```rust
/// Whether a pattern could read or write outside its roots. `plan_includes`
/// strips `source` lexically, so a `..` component survives into the path joined
/// onto the destination and escapes both; an absolute or root-relative pattern
/// replaces the base in `Path::join` outright. `has_root` catches `/etc/x` on
/// Windows, where `is_absolute` is false but `join` still discards the base.
fn escapes(pattern: &str) -> bool {
    let p = Path::new(pattern);
    p.is_absolute()
        || p.has_root()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Copy files matching `patterns` (globs relative to `source`) out of a worktree
/// into `dest`, at the same relative path, replacing what is already there. The
/// outbound counterpart to `copy_includes`, fail-open the same way: returns
/// (files_copied, warnings). A pattern that would leave either root is skipped
/// with a warning, because the caller deletes `source` immediately afterward.
pub fn copy_out(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>) {
    let mut warnings = Vec::new();
    let mut inside = Vec::new();
    for pattern in patterns {
        if escapes(pattern) {
            warnings.push(format!(
                "pattern reaches outside the worktree, skipped: {pattern}"
            ));
        } else {
            inside.push(pattern.clone());
        }
    }
    let plan = plan_includes(source, dest, &inside);
    let (copied, apply_warnings) = apply_includes(source, dest, &plan, true);
    warnings.extend(plan.warnings);
    warnings.extend(apply_warnings);
    (copied, warnings)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-common && cargo clippy -p devkit-common --all-targets -- -D warnings`

Expected: PASS, zero warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(worktree): add copy_out for archiving a worktree"
```

---

### Task 3: Add the `[preserve.<name>]` config table

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (add `PreserveConfig` near `HooksConfig` at line 150; add the `preserve` field to `Config` at line 11; add `#[derive(Clone)]` to `GithubConfig` at line 177)
- Modify: `schema/devkit-config.json` (regenerated, not hand-edited)
- Test: `crates/devkit-config/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub struct PreserveConfig { pub from: Vec<String>, pub to: String, pub required: bool }`
  - `Config.preserve: HashMap<String, PreserveConfig>`
  - `GithubConfig` becomes `Clone`, which Task 4 needs in order to keep the loaded `Config` while still handing `Repos::resolve` a `GithubConfig`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/devkit-config/src/lib.rs`:

```rust
/// A typo in `required` is the one config mistake that produces no signal
/// without `deny_unknown_fields`: serde consumes the unknown key as
/// `IgnoredAny`, the entry stays fail-open, and files the user believed were
/// protected are removed with the worktree.
#[test]
fn a_misspelled_preserve_key_is_rejected() {
    let err = toml::from_str::<Config>(
        "[preserve.notes]\nfrom = [\"a.md\"]\nto = \"/archive\"\nrequred = true\n",
    )
    .unwrap_err();
    assert!(err.to_string().contains("requred"), "{err}");
}

#[test]
fn a_preserve_entry_parses_with_required_defaulting_off() {
    let cfg: Config = toml::from_str(
        "[preserve.graphify]\nfrom = [\"graphify-out/**\"]\nto = \"/archive/{{ issue }}\"\n",
    )
    .unwrap();
    let entry = &cfg.preserve["graphify"];
    assert_eq!(entry.from, vec!["graphify-out/**".to_string()]);
    assert_eq!(entry.to, "/archive/{{ issue }}");
    assert!(!entry.required);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p devkit-config preserve`

Expected: FAIL to compile — `no field 'preserve' on type 'Config'`.

- [ ] **Step 3: Write minimal implementation**

In `crates/devkit-config/src/lib.rs`, add the field to `Config` after `hooks`:

```rust
    /// Files copied out of an issue worktree before `issue end` removes it,
    /// keyed by the name that labels the entry's progress step and its
    /// warnings.
    #[serde(default)]
    pub preserve: HashMap<String, PreserveConfig>,
```

Add the struct beside `HooksConfig`:

```rust
/// Files copied out of a worktree before `issue end` removes it. Each entry
/// names its own destination, so one run can archive different files to
/// different places. Rendering, path rules, and the fail-open contract are in
/// `docs/configuration.md`.
///
/// `deny_unknown_fields` because a misspelled `required` would otherwise be
/// consumed silently, leaving the entry fail-open while the user believes the
/// files are protected.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreserveConfig {
    /// Glob patterns for the files to copy, relative to the worktree root and
    /// rendered as minijinja. A pattern that renders empty is skipped; one that
    /// matches nothing is not a failure.
    pub from: Vec<String>,
    /// Destination directory, rendered as minijinja. Must render to a non-empty
    /// absolute path, and is created if absent.
    pub to: String,
    /// Keep the worktree instead of removing it when this entry warns.
    #[serde(default)]
    pub required: bool,
}
```

Add `Clone` to the `GithubConfig` derive list at line 177:

```rust
#[derive(Debug, Default, Clone, JsonSchema, Deserialize, Serialize)]
```

- [ ] **Step 4: Run tests, regenerate the schema, run again**

Run: `cargo test -p devkit-config preserve`
Expected: PASS.

Run: `cargo test --workspace`
Expected: FAIL on `tests/config_schema.rs` with a diff — the committed schema no longer matches the derived types. That failure is the drift test working.

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`
Then: `cargo test --workspace`
Expected: PASS. `schema/devkit-config.json` now carries `PreserveConfig`.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-config/src/lib.rs schema/devkit-config.json
git commit -m "feat(config): add the preserve table"
```

---

### Task 4: Return the load outcome from `tracker::select`

`select` turns every config-load failure into `None`, which is deliberate for read-only commands: the tracker choice must never fail a command that would otherwise work. For `issue end` that same fallback is dangerous — a broken `devkit.toml` yields an empty preserve table and the worktree is removed having preserved nothing. `issue end` needs to tell "no config" from "broken config".

**Files:**
- Modify: `src/bin/devkit/issue/tracker.rs:18-29`
- Test: `src/bin/devkit/issue/tracker.rs` (`mod tests`, which already has a `write_config` helper)

**Interfaces:**
- Consumes: `Config.preserve` and `Clone` on `GithubConfig` (Task 3).
- Produces:
  - `pub struct Selected { pub tracker: Resolved, pub repos: Repos, pub config: Option<devkit_config::Config>, pub health: devkit_config::Health }`
  - `pub fn select_full(config: Option<&str>, start: &str, pr_override: Option<&str>) -> Selected`
  - `select` keeps its exact current signature `(Option<&str>, &str, Option<&str>) -> (Resolved, Repos)` and its three other callers (`info.rs:67`, `status.rs:156`, `dashboard/mod.rs:55`) are untouched.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/bin/devkit/issue/tracker.rs`:

```rust
/// A config that does not parse must be distinguishable from no config at all:
/// `issue end` refuses on the first and proceeds on the second.
#[test]
fn a_broken_config_reports_broken_and_yields_no_config() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("devkit.toml"), "[defaults\n").unwrap();

    let sel = select_full(None, dir.path().to_str().unwrap(), None);

    assert!(matches!(sel.health, devkit_config::Health::Broken(_)));
    assert!(sel.config.is_none());
}

/// A project with no devkit.toml is not a fault. `issue end` still removes
/// worktrees there; it just has no preserve entries to run.
#[test]
fn no_config_reports_absent() {
    let dir = tempfile::tempdir().unwrap();

    let sel = select_full(None, dir.path().to_str().unwrap(), None);

    assert_eq!(sel.health, devkit_config::Health::Absent);
    assert!(sel.config.is_none());
}

/// The loaded config comes back so `issue end` can read its preserve table
/// without a second load.
#[test]
fn a_valid_config_comes_back_with_its_preserve_table() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[defaults]\n\
         worktree_root = \"wts\"\n\
         branch_prefix = \"lev/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \"base\"\n\
         doppler_yaml = \"doppler.yaml\"\n\
         \n\
         [preserve.notes]\n\
         from = [\"notes/*.md\"]\n\
         to = \"/archive\"\n",
    )
    .unwrap();

    let sel = select_full(None, dir.path().to_str().unwrap(), None);

    let cfg = sel.config.expect("config loaded");
    assert_eq!(cfg.preserve["notes"].to, "/archive");
}
```

The `[defaults]` keys mirror what the module's existing `write_config` helper
emits. If `resolve` rejects this fixture for a missing key, copy the helper's
exact body rather than guessing — the helper is the source of truth for what a
minimal valid config needs.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin devkit tracker::tests`

Expected: FAIL to compile — `cannot find function 'select_full' in this scope`.

- [ ] **Step 3: Write minimal implementation**

Replace the body of `src/bin/devkit/issue/tracker.rs` from `pub fn select` down, keeping the existing doc comment on `select`:

```rust
/// Everything one config load yields for an `issue` command: the tracker and
/// repositories `select` returns, plus the config itself and how its load went.
/// `issue end` needs the last two — its preserve entries live in the config, and
/// acting on an empty table because the config is broken would remove a worktree
/// having archived nothing.
pub struct Selected {
    pub tracker: Resolved,
    pub repos: Repos,
    pub config: Option<devkit_config::Config>,
    pub health: devkit_config::Health,
}

/// `select` with the config load's result attached. `health` is classified
/// separately from `load`, because `load` also builds the doppler map and app
/// catalog: a broken `doppler.yaml` is not a broken config, and must not make
/// `issue end` refuse.
pub fn select_full(config: Option<&str>, start: &str, pr_override: Option<&str>) -> Selected {
    let dir = Path::new(start);
    let main = devkit_common::git::main_checkout(dir).ok().flatten();
    let health = devkit_config::health(dir, main.as_deref());
    let cfg = load::load(config.map(Path::new), dir)
        .ok()
        .map(|l| l.config);
    let (kind, github) = match &cfg {
        Some(c) => (c.tracker.kind, c.github.clone()),
        None => (None, devkit_config::GithubConfig::default()),
    };
    let repos = Repos::resolve(&github, start, pr_override);
    let tracker = devkit_common::tracker::resolve(kind, dir, &repos);
    Selected {
        tracker,
        repos,
        config: cfg,
        health,
    }
}

pub fn select(config: Option<&str>, start: &str, pr_override: Option<&str>) -> (Resolved, Repos) {
    let sel = select_full(config, start, pr_override);
    (sel.tracker, sel.repos)
}
```

`Health` derives `PartialEq`, so the `assert_eq!` on `Health::Absent` compiles. If `TrackerKind` is not `Copy`, use `c.tracker.kind.clone()`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin devkit && cargo clippy --workspace --all-targets -- -D warnings`

Expected: PASS. The three other `select` callers still compile unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/issue/tracker.rs
git commit -m "feat(issue): return the config load outcome from select"
```

---

### Task 5: Resolve and run preserve entries for one worktree

The whole of phase 2's per-worktree logic, testable without a git fixture. Task 6 wires it into `run`.

**Files:**
- Create: `src/bin/devkit/issue/preserve.rs`
- Modify: `src/bin/devkit/issue/mod.rs` (add `mod preserve;` beside the other issue submodules)
- Test: `src/bin/devkit/issue/preserve.rs` (`mod tests`)

**Interfaces:**
- Consumes: `devkit_common::worktree::copy_out` (Task 2), `devkit_config::PreserveConfig` (Task 3), `devkit_common::record::IssueRecord`, `devkit_common::template::render`.
- Produces:
  - `pub(crate) struct Resolved { pub name: String, pub patterns: Vec<String>, pub dest: PathBuf }`
  - `pub(crate) struct Skipped { pub name: String, pub reason: String }`
  - `pub(crate) fn context(worktree: &Path, branch: &str, record: Option<&IssueRecord>, prefix: &str, worktree_root: &Path, primary: &Path) -> serde_json::Value`
  - `pub(crate) fn resolve_entry(name: &str, cfg: &PreserveConfig, ctx: &serde_json::Value, vars: &BTreeMap<String, String>, removal_roots: &[PathBuf]) -> Result<Resolved, Skipped>`

- [ ] **Step 1: Write the failing tests**

Create `src/bin/devkit/issue/preserve.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(from: &[&str], to: &str, required: bool) -> devkit_config::PreserveConfig {
        devkit_config::PreserveConfig {
            from: from.iter().map(|s| s.to_string()).collect(),
            to: to.to_string(),
            required,
        }
    }

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn ctx_for(issue: &str) -> serde_json::Value {
        serde_json::json!({
            "worktree": "/wt",
            "branch": "lev/fix",
            "issue": issue,
            "slug": "fix",
            "apps": Vec::<String>::new(),
            "prefix": "lev/",
            "worktree_root": "/wts",
            "primary": "/repo",
        })
    }

    #[test]
    fn a_relative_destination_is_refused() {
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], "archive/{{ issue }}", false),
            &ctx_for("ENG-1"),
            &novars(),
            &[],
        )
        .unwrap_err();
        assert!(err.reason.contains("absolute"), "{}", err.reason);
    }

    /// An empty `to` addresses the process cwd, which would write the archive
    /// into whatever directory the command was run from.
    #[test]
    fn an_empty_destination_is_refused() {
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], "{{ issue }}", false),
            &ctx_for(""),
            &novars(),
            &[],
        )
        .unwrap_err();
        assert!(err.reason.contains("empty"), "{}", err.reason);
    }

    /// Archiving into a tree this run deletes loses the copy moments later.
    #[test]
    fn a_destination_inside_a_worktree_being_removed_is_refused() {
        let roots = vec![PathBuf::from("/wts/fix")];
        let err = resolve_entry(
            "notes",
            &cfg(&["a.md"], "/wts/fix/archive", false),
            &ctx_for("ENG-1"),
            &novars(),
            &roots,
        )
        .unwrap_err();
        assert!(err.reason.contains("removes"), "{}", err.reason);
    }

    /// A pattern that renders empty drops out rather than reaching `copy_out`,
    /// where an empty glob would otherwise have to be caught again.
    #[test]
    fn a_pattern_that_renders_empty_drops_out() {
        let resolved = resolve_entry(
            "notes",
            &cfg(&["{{ issue }}", "keep.md"], "/archive", false),
            &ctx_for(""),
            &novars(),
            &[],
        )
        .unwrap();
        assert_eq!(resolved.patterns, vec!["keep.md".to_string()]);
    }

    #[test]
    fn a_resolved_entry_renders_both_fields() {
        let resolved = resolve_entry(
            "graphify",
            &cfg(&["out/{{ slug }}/**"], "{{ worktree_root }}/archive/{{ issue }}", false),
            &ctx_for("ENG-7"),
            &novars(),
            &[],
        )
        .unwrap();
        assert_eq!(resolved.patterns, vec!["out/fix/**".to_string()]);
        assert_eq!(resolved.dest, PathBuf::from("/wts/archive/ENG-7"));
    }

    /// A malformed record reads as `None`, exactly like an absent one, so both
    /// take the same defaults rather than failing the render.
    #[test]
    fn a_missing_record_renders_the_issue_fields_empty() {
        let ctx = context(
            Path::new("/wt"),
            "lev/fix",
            None,
            "lev/",
            Path::new("/wts"),
            Path::new("/repo"),
        );
        assert_eq!(ctx["issue"], "");
        assert_eq!(ctx["slug"], "");
        assert_eq!(ctx["apps"], serde_json::json!([]));
        assert_eq!(ctx["branch"], "lev/fix");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Add `mod preserve;` to `src/bin/devkit/issue/mod.rs` beside the other submodule declarations, then run:

`cargo test --bin devkit preserve::tests`

Expected: FAIL to compile — `resolve_entry` and `context` do not exist.

- [ ] **Step 3: Write minimal implementation**

Put this above the test module in `src/bin/devkit/issue/preserve.rs`:

```rust
//! Copying a worktree's files out before `issue end` removes it. Resolution and
//! validation live here; the copy itself is `devkit_common::worktree::copy_out`.

use devkit_common::record::IssueRecord;
use devkit_config::PreserveConfig;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One entry resolved against one worktree: the patterns to glob and the
/// directory they land in.
pub(crate) struct Resolved {
    pub name: String,
    pub patterns: Vec<String>,
    pub dest: PathBuf,
}

/// An entry that will not run, and the sentence explaining why. Fail-open turns
/// this into a warning; `required` turns it into an error.
pub(crate) struct Skipped {
    pub name: String,
    pub reason: String,
}

/// The minijinja context an entry's `from` and `to` render against. `issue`,
/// `slug`, and `apps` come from the record rather than being re-derived, so a
/// template edited since setup cannot misname the destination; `record::read`
/// returns `None` for a malformed record as well as an absent one, and both take
/// the same empty defaults.
pub(crate) fn context(
    worktree: &Path,
    branch: &str,
    record: Option<&IssueRecord>,
    prefix: &str,
    worktree_root: &Path,
    primary: &Path,
) -> serde_json::Value {
    serde_json::json!({
        "worktree": worktree.display().to_string(),
        "branch": branch,
        "issue": record.map(|r| r.issue.as_str()).unwrap_or_default(),
        "slug": record.map(|r| r.slug.as_str()).unwrap_or_default(),
        "apps": record.map(|r| r.apps.clone()).unwrap_or_default(),
        "prefix": prefix,
        "worktree_root": worktree_root.display().to_string(),
        "primary": primary.display().to_string(),
    })
}

/// Render and validate one entry. `removal_roots` are the worktrees this run
/// will delete, spelled as the status report gives them: a destination under any
/// of them would be archived and then deleted seconds later.
pub(crate) fn resolve_entry(
    name: &str,
    cfg: &PreserveConfig,
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    removal_roots: &[PathBuf],
) -> Result<Resolved, Skipped> {
    let skip = |reason: String| Skipped {
        name: name.to_string(),
        reason,
    };

    let mut patterns = Vec::with_capacity(cfg.from.len());
    for p in &cfg.from {
        match devkit_common::template::render(p, ctx, vars) {
            Ok(r) if r.trim().is_empty() => {}
            Ok(r) => patterns.push(r.trim().to_string()),
            Err(e) => return Err(skip(format!("rendering `from` entry `{p}`: {e:#}"))),
        }
    }

    let rendered = devkit_common::template::render(&cfg.to, ctx, vars)
        .map_err(|e| skip(format!("rendering `to`: {e:#}")))?;
    let to = rendered.trim();
    if to.is_empty() {
        return Err(skip("`to` rendered empty".into()));
    }
    let dest = PathBuf::from(to);
    if !dest.is_absolute() {
        return Err(skip(format!("`to` must be an absolute path, got `{to}`")));
    }
    if let Some(root) = removal_roots.iter().find(|r| dest.starts_with(r)) {
        return Err(skip(format!(
            "`to` is inside {}, which this run removes",
            root.display()
        )));
    }

    Ok(Resolved {
        name: name.to_string(),
        patterns,
        dest,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --bin devkit preserve && cargo clippy --workspace --all-targets -- -D warnings`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/issue/preserve.rs src/bin/devkit/issue/mod.rs
git commit -m "feat(issue): resolve and validate preserve entries"
```

---

### Task 6: Split `issue end` into confirm, preserve, remove

**Files:**
- Modify: `src/bin/devkit/issue/end.rs:168-260` (`run`)
- Modify: `src/bin/devkit/issue/preserve.rs` (add `run_for` and `Outcome`)
- Modify: `src/bin/devkit/issue/mod.rs:133-152` (the `End` CLI variant) and `:337-350` (the dispatch arm)
- Test: `src/bin/devkit/issue/preserve.rs` and `src/bin/devkit/issue/end.rs` (`mod tests`)

**Interfaces:**
- Consumes: `preserve::{context, resolve_entry, Resolved, Skipped}` (Task 5), `worktree::copy_out` (Task 2), `tracker::select_full` (Task 4), `Config.preserve` (Task 3).
- Produces:
  - `pub(crate) struct Outcome { pub files: usize, pub entries: usize, pub warnings: Vec<String>, pub required_failure: Option<String> }`
  - `pub(crate) fn run_for(worktree: &Path, entries: &[(String, &PreserveConfig)], ctx: &serde_json::Value, vars: &BTreeMap<String, String>, removal_roots: &[PathBuf]) -> Outcome`
  - `end::run` gains a `no_preserve: bool` parameter, placed after `clean_worktree`.

`worktree` here is the status report's spelling of the path (`row.worktree`), not
a canonicalized one. That is deliberate: `cleanup` canonicalizes its own target,
and on Windows that yields a verbatim path, which `glob 0.3.3` accepts behind
`\\?\C:\` but silently matches nothing behind `\\?\UNC\`. Passing the report's
spelling keeps a repo on a network share working, and keeps `removal_roots`
comparable to `dest` with a plain `starts_with`.

- [ ] **Step 1: Write the failing tests**

In `src/bin/devkit/issue/preserve.rs`:

```rust
    /// Fail-open is the default: a bad entry warns and the caller still removes
    /// the worktree.
    #[test]
    fn a_failing_entry_warns_without_blocking_removal() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();

        let bad = cfg(&["a.md"], "relative/path", false);
        let out = run_for(&wt, &[("notes".to_string(), &bad)], &ctx_for("ENG-1"), &novars(), &[]);

        assert!(out.required_failure.is_none(), "removal is not blocked");
        assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
        assert!(out.warnings[0].contains("notes"), "{:?}", out.warnings);
    }

    /// `required` turns the same warning into the reason the worktree survives.
    #[test]
    fn a_failing_required_entry_blocks_removal() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();

        let bad = cfg(&["a.md"], "relative/path", true);
        let out = run_for(&wt, &[("notes".to_string(), &bad)], &ctx_for("ENG-1"), &novars(), &[]);

        let err = out.required_failure.expect("required entry blocks");
        assert!(err.contains("notes"), "{err}");
    }

    /// `required` governs errors, never emptiness. A worktree that produced no
    /// scratch still removes cleanly.
    #[test]
    fn a_required_entry_matching_nothing_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(&wt).unwrap();

        let entry = cfg(&["graphify-out/**"], archive.to_str().unwrap(), true);
        let out = run_for(&wt, &[("graphify".to_string(), &entry)], &ctx_for("ENG-1"), &novars(), &[]);

        assert!(out.required_failure.is_none());
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(out.files, 0);
    }

    #[test]
    fn a_matching_entry_reports_what_it_archived() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let archive = dir.path().join("archive");
        std::fs::create_dir_all(wt.join("graphify-out")).unwrap();
        std::fs::write(wt.join("graphify-out/a.md"), "a").unwrap();

        let entry = cfg(&["graphify-out/**"], archive.to_str().unwrap(), false);
        let out = run_for(&wt, &[("graphify".to_string(), &entry)], &ctx_for("ENG-1"), &novars(), &[]);

        assert_eq!(out.files, 1, "{:?}", out.warnings);
        assert_eq!(out.entries, 1);
        assert!(archive.join("graphify-out/a.md").exists());
    }
```

In `src/bin/devkit/issue/end.rs`, assert the phase order holds where it matters — a required failure must leave everything intact:

```rust
    /// Phase 2 runs before any removal, so a required failure leaves the
    /// worktree, its branch, and its summary exactly as they were.
    #[test]
    fn a_blocked_worktree_keeps_its_branch_and_summary() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let g = |args: &[&str], cwd: &std::path::Path| {
            devkit_common::git::Git::fixture(cwd)
                .args(args.iter().copied())
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
        };
        g(&["init", "-q", "-b", "main"], &main);
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        g(&["add", "-A"], &main);
        g(&["commit", "-qm", "init"], &main);
        let wt = dir.path().join("wt-eng-3");
        g(
            &["worktree", "add", "-q", "-b", "eng-3", wt.to_str().unwrap()],
            &main,
        );
        let summary = dir.path().join("ISSUE_SUMMARY_ENG-3.md");
        std::fs::write(&summary, "notes\n").unwrap();

        // A required entry that cannot resolve: `to` is relative.
        let entry = devkit_config::PreserveConfig {
            from: vec!["a.md".into()],
            to: "relative/path".into(),
            required: true,
        };
        let out = crate::issue::preserve::run_for(
            &wt,
            &[("notes".to_string(), &entry)],
            &crate::issue::preserve::context(
                &wt,
                "eng-3",
                None,
                "",
                dir.path(),
                &main,
            ),
            &std::collections::BTreeMap::new(),
            &[],
        );

        assert!(out.required_failure.is_some());
        assert!(wt.exists(), "worktree untouched");
        assert!(summary.exists(), "summary untouched");
        let branches = devkit_common::git::Git::fixture(&main)
            .args(["branch", "--list", "eng-3"])
            .output()
            .unwrap();
        assert!(!branches.trim().is_empty(), "branch untouched");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin devkit preserve`

Expected: FAIL to compile — `run_for` and `Outcome` do not exist.

- [ ] **Step 3: Write `run_for`**

Append to `src/bin/devkit/issue/preserve.rs`, above the test module:

```rust
/// What preserving one worktree produced. `required_failure` is set only when a
/// `required` entry could not run, and is the caller's reason to keep the
/// worktree.
pub(crate) struct Outcome {
    pub files: usize,
    pub entries: usize,
    pub warnings: Vec<String>,
    pub required_failure: Option<String>,
}

/// Preserve one worktree. `entries` are the config's preserve entries in sorted
/// key order. Fail-open per entry: a failure warns and the next entry still
/// runs, unless the entry is `required`, which stops this worktree and leaves it
/// for the caller to keep.
pub(crate) fn run_for(
    worktree: &Path,
    entries: &[(String, &PreserveConfig)],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    removal_roots: &[PathBuf],
) -> Outcome {
    let mut out = Outcome {
        files: 0,
        entries: 0,
        warnings: Vec::new(),
        required_failure: None,
    };

    for (name, cfg) in entries {
        match resolve_entry(name, cfg, ctx, vars, removal_roots) {
            Err(skipped) => {
                let msg = format!("preserve `{}`: {}", skipped.name, skipped.reason);
                if cfg.required {
                    out.required_failure = Some(msg);
                    return out;
                }
                out.warnings.push(msg);
            }
            Ok(resolved) => {
                let (files, warnings) = devkit_common::worktree::copy_out(
                    worktree,
                    &resolved.dest,
                    &resolved.patterns,
                );
                if !warnings.is_empty() && cfg.required {
                    out.required_failure = Some(format!(
                        "preserve `{}`: {}",
                        resolved.name,
                        warnings.join("; ")
                    ));
                    return out;
                }
                out.warnings.extend(
                    warnings
                        .into_iter()
                        .map(|w| format!("preserve `{}`: {w}", resolved.name)),
                );
                out.files += files;
                if files > 0 {
                    out.entries += 1;
                }
            }
        }
    }

    out
}
```

- [ ] **Step 4: Restructure `run` and add the CLI flag**

In `src/bin/devkit/issue/mod.rs`, add to the `End` variant:

```rust
        /// Remove without copying out the `[preserve]` entries first.
        #[arg(long = "no-preserve")]
        no_preserve: bool,
```

and pass `no_preserve` through the dispatch arm to `end::run`.

In `src/bin/devkit/issue/end.rs`, change `run`'s signature to take `no_preserve: bool` after `clean_worktree`, replace `tracker::select` with `select_full`, and restructure the tail. Extract the label first, since three phases now use it:

```rust
/// How a worktree is named in prompts, steps, and errors: its issue id when the
/// record has one, else its branch.
fn row_label(row: &IssueWorktree) -> String {
    if row.issue_id != "UNKNOWN" {
        row.issue_id.clone()
    } else {
        row.branch.clone()
    }
}
```

At the top of `run`, refuse a broken config unless preservation was waived:

```rust
    let sel = crate::issue::tracker::select_full(config, start, None);
    if !no_preserve
        && let devkit_config::Health::Broken(why) = &sel.health
    {
        anyhow::bail!(
            "devkit.toml does not load, so [preserve] entries cannot be read: {why}\n\
             rerun with --no-preserve to remove without preserving anything"
        );
    }
    let (tracker, repos) = (sel.tracker, sel.repos);
```

Replace the single confirm-and-spawn loop with the three phases:

```rust
    // Phase 1: every prompt precedes every action, so nothing is being removed
    // while the next question is on screen.
    let mut approved: Vec<IssueWorktree> = Vec::new();
    for row in &targets {
        let label = row_label(row);
        let go = steps.suspend(|| {
            println!("\n{label}  {}", row.worktree);
            yes || confirm(&label)
        });
        if go {
            approved.push(row.clone());
        } else {
            steps.suspend(|| println!("    skipped"));
        }
    }
    if approved.is_empty() {
        println!("\nNothing to remove.");
        return Ok(());
    }

    // Phase 2: serial, and complete before the first removal. That ordering is
    // what makes a destination collision resolve in worktree order and a
    // `required` failure surface while every file still exists.
    let entries: Vec<(String, &devkit_config::PreserveConfig)> = match (&sel.config, no_preserve) {
        (Some(cfg), false) => {
            let mut names: Vec<&String> = cfg.preserve.keys().collect();
            names.sort();
            names
                .into_iter()
                .map(|n| (n.clone(), &cfg.preserve[n]))
                .collect()
        }
        _ => Vec::new(),
    };
    let removal_roots: Vec<std::path::PathBuf> =
        approved.iter().map(|r| PathBuf::from(&r.worktree)).collect();
    let vars = sel
        .config
        .as_ref()
        .map(|c| c.templates.variables.clone())
        .unwrap_or_default();
    let (wt_root, prefix) = sel
        .config
        .as_ref()
        .map(|c| {
            (
                devkit_config::expand_tilde(&c.defaults.worktree_root),
                c.defaults.branch_prefix.clone(),
            )
        })
        .unwrap_or_default();

    let mut blocked: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut files = 0usize;
    let mut archived = 0usize;
    let mut required_failures = 0usize;
    if !entries.is_empty() {
        let primary = main.as_deref().map(Path::new);
        for row in &approved {
            let label = row_label(row);
            let wt = Path::new(&row.worktree);
            let record = devkit_common::record::read(wt);
            let ctx = crate::issue::preserve::context(
                wt,
                &row.branch,
                record.as_ref(),
                &prefix,
                &wt_root,
                primary.unwrap_or(wt),
            );
            let out = steps.during(&format!("Preserving {label}…"), || {
                crate::issue::preserve::run_for(wt, &entries, &ctx, &vars, &removal_roots)
            });
            files += out.files;
            archived += out.entries;
            for w in &out.warnings {
                steps.suspend(|| eprintln!("warning: {w}"));
            }
            if let Some(err) = out.required_failure {
                steps.suspend(|| eprintln!("    {label} kept: {err}"));
                blocked.insert(row.worktree.clone());
                required_failures += 1;
            }
        }
    }

    // Phase 3: removals, parallel as before.
    let total = approved.len() - blocked.len();
    let removed = AtomicUsize::new(0);
    let branch_lock = Mutex::new(());
    std::thread::scope(|s| {
        for row in approved.iter().filter(|r| !blocked.contains(&r.worktree)) {
            let label = row_label(row);
            let steps = &steps;
            let branch_lock = &branch_lock;
            let removed = &removed;
            s.spawn(move || {
                match steps.during_result(&format!("Removing {label}…"), || {
                    cleanup(&row.worktree, &row.issue_id, force, branch_lock)
                }) {
                    Ok(()) => {
                        removed.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        let msg = if e.downcast_ref::<Dirty>().is_some() {
                            format!("    {label} is dirty — rerun with --force to discard.")
                        } else {
                            format!("    cleanup failed for {label}: {e}")
                        };
                        steps.suspend(|| eprintln!("{msg}"));
                    }
                }
            });
        }
    });

    if let Some(main) = main {
        let _ = devkit_common::git::Git::at(Path::new(&main))
            .args(["worktree", "prune"])
            .output();
    }
    if archived > 0 {
        println!(
            "\nPreserved {files} file(s) across {archived} entr{}.",
            if archived == 1 { "y" } else { "ies" }
        );
    }
    println!("Removed {} of {}.", removed.load(Ordering::Relaxed), total);
    anyhow::ensure!(
        required_failures == 0,
        "{required_failures} worktree(s) kept: a required preserve entry failed"
    );
    Ok(())
```

Note the existing `let main = main_repo(start).ok();` must move above phase 2, since the context needs the primary checkout.

- [ ] **Step 5: Run the full gate**

Run: `cargo test --workspace`
Expected: PASS, including the existing `cleanup_*` tests, which are untouched.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero warnings.

Run: `cargo fmt --all`

- [ ] **Step 6: Commit**

```bash
git add src/bin/devkit/issue/end.rs src/bin/devkit/issue/preserve.rs src/bin/devkit/issue/mod.rs
git commit -m "feat(issue): preserve worktree files before removing them"
```

---

### Task 7: Document the table and correct `AGENTS.md`

**Files:**
- Modify: `docs/configuration.md` (new `### [preserve.<name>]` section, placed after `### [hooks]`)
- Modify: `docs/commands.md` (the `issue end` entry)
- Modify: `AGENTS.md` (the Conventions bullet calling `[github]` the only table with `deny_unknown_fields`)

**Interfaces:**
- Consumes: the finished behavior from Tasks 1-6.
- Produces: no code.

- [ ] **Step 1: Write the configuration reference**

Add to `docs/configuration.md` after the `[hooks]` section:

````markdown
### `[preserve.<name>]`

Files copied out of an issue worktree before `issue end` removes it, so an
agent's scratch output outlives the worktree. Each entry names its own
destination, so one run can archive different files to different places.

| Key | Required | Meaning |
|---|---|---|
| `from` | yes | Glob patterns for the files to copy, relative to the worktree root. Each is rendered as a minijinja template. |
| `to` | yes | Destination directory, rendered as a minijinja template. Must render to a non-empty absolute path; created if absent. |
| `required` | no (default `false`) | Keep the worktree instead of removing it when this entry warns. |

```toml
[preserve.graphify]
from     = ["graphify-out/**"]
to       = "{{ worktree_root }}/archive/{{ issue }}/graphify"
required = true

[preserve.notes]
from = ["docs/notes/*.md"]
to   = "{{ primary }}/.devkit/archive/{{ issue }}"
```

Render context: `worktree`, `branch`, `issue`, `slug`, `apps`, `prefix`,
`worktree_root` (the resolved `defaults.worktree_root`), `primary` (the primary
checkout's root), and `[templates.variables]`. The issue fields come from the
worktree's `.devkit/issue.toml`, so a `worktree_dir` or `branch` template edited
since setup cannot misname the destination; a worktree without a readable record
renders them empty.

Patterns are worktree-relative. One that is absolute, or that contains a `..`
component, is skipped with a warning — the archive cannot reach outside the tree
it is saving. The recorded summary file therefore cannot be preserved at its
default location beside the worktrees directory; set
`issue_summary_path = "{{ worktree }}/.devkit/issue.md"` to keep it inside the
worktree, where a pattern can name it. A pattern that renders empty is skipped;
one that matches nothing is not a failure, which is the normal case for a
worktree that produced no scratch.

A destination that resolves inside any worktree the same run is removing is
skipped: the copy would be deleted seconds after it was written. An existing
destination file is replaced, since the worktree's copy is the one about to be
lost.

Failures are **fail-open**, like `[hooks]`: an entry that cannot render, is
rejected, or fails to copy prints a `warning:` line naming the entry, and the
worktree is removed anyway. `required = true` flips one entry to fail-closed —
its warnings keep that worktree, its branch, and its summary intact, and
`issue end` exits non-zero. `required` governs errors only, never emptiness.

Preservation runs before any worktree is removed, serially and in sorted entry
name order, with one progress step per worktree. Two worktrees archiving the
same filename into the same `to` collide, and worktree order decides; template
`{{ issue }}` into `to` to keep them apart.

Two limits worth knowing. Symlinks are followed, so a link inside the worktree is
archived as its target's content, matching what `defaults.worktree_include` does
in the inbound direction. And a copy is not atomic: `std::fs::copy` truncates
before writing, so a copy interrupted over an existing archive leaves a short
file. Preservation finishing before any removal is what keeps that from costing
data — nothing is deleted until every entry has run.
````

- [ ] **Step 2: Update the command reference**

In `docs/commands.md`, extend the `issue end` entry to say that it preserves
before removing, that a broken `devkit.toml` makes it refuse unless
`--no-preserve` is passed, and that a `required` entry's failure keeps that
worktree and makes the command exit non-zero.

- [ ] **Step 3: Correct `AGENTS.md`**

The Conventions section currently reads:

> the `[github]` table is the one config table with `deny_unknown_fields`, because a typo'd key silently ignored would resolve a different repository than the project declared

Replace that clause with:

> `[github]` and `[preserve.<name>]` are the two config tables with `deny_unknown_fields`, because in both a silently ignored typo changes behavior without any diagnostic: a wrong `[github]` key resolves a different repository than the project declared, and a wrong `[preserve]` key leaves an entry fail-open while the user believes its files are protected

- [ ] **Step 4: Verify the docs match the code**

Run: `cargo test --workspace`
Expected: PASS. `tests/brief_pins.rs` and the schema drift test both read
committed artifacts, so a docs-only change must leave them green.

Read back the `[preserve.<name>]` section against `src/bin/devkit/issue/preserve.rs`
and confirm every stated rule has a test behind it from Tasks 5 and 6.

- [ ] **Step 5: Commit**

```bash
git add docs/configuration.md docs/commands.md AGENTS.md
git commit -m "docs: document the preserve table"
```

---

## Verification

After Task 7, run the full gate from the worktree root:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Then exercise it by hand once, since no test drives the real CLI end to end:

1. Add a `[preserve.scratch]` entry to this repo's `devkit.toml` pointing `from`
   at a file you create in a throwaway worktree and `to` at a temp directory.
2. Run `issue end --clean-worktree <that worktree>` and confirm the progress
   output shows a `Preserving …` step, the summary line reports the file count,
   and the file exists at the destination after the worktree is gone.
3. Break the entry (`to = "relative"`, `required = true`) and confirm the
   worktree survives, the error names the entry, and the exit code is non-zero.
