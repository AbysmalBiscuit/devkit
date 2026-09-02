# Baseline worktrees implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single shared baseline checkout with per-fork-point, content-addressed baseline worktrees that devkit creates on demand and reclaims when nothing references them, and make every `[defaults]` key optional.

**Architecture:** Stage A relaxes config: every `[defaults]` key gets a serde default, `worktree_root` derives from the primary checkout, and the merge-base target resolves from `origin/HEAD` when `baseline_ref` is unset. Stage B replaces `ensure_fresh` with a pin-and-create path: the baseline is `git merge-base HEAD <target>`, it lives at `<baseline_dir>/<12-hex>`, a marker file inside it records completeness and identity, each worktree's record names the baseline it uses, and the reference count is derived by scanning worktrees rather than stored.

**Tech Stack:** Rust edition 2024, `anyhow`, `serde`/`toml`, `clap`, `rayon` via `devkit_common::pool`, `fd-lock`, `tempfile` for test scratch. Test runner is `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-09-02-baseline-worktrees-design.md`

## Global constraints

- Merge gate is `cargo nextest run --workspace --no-fail-fast`. Also required green before any commit: `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all`.
- **Every task's commit leaves the workspace compiling and the gate green.** Before each `git commit`, run the gate, then `git status --short` and confirm the staged set matches the working tree's changed files. A task that renames or removes a field stages every file the compiler named, not only the file that declared it.
- Every error uses `anyhow` with `.context()`. No `unwrap()` outside tests.
- Test scratch comes from `tempfile::tempdir()`. Never build a path from `std::env::temp_dir()`. Bind the `TempDir` guard for as long as the path is used.
- Git invocations go through `devkit_common::git::Git`. Tests use `Git::fixture` so no user git config leaks in.
- Tests never depend on the developer's live port registry, daemon, or `$HOME`. Anything that would read `registry::snapshot()` takes a `&registry::Data` parameter instead, so tests pass `registry::Data::default()`.
- Path assertions in tests join onto `tempdir()`. A literal like `/w` is not absolute on Windows, where `resolve_path_key` then anchors it to the layer directory and the assertion compares two different trees.
- Parallel work goes through `devkit_common::pool`. `pool::jwalk_parallelism()` is evaluated on the thread that builds the walk, never inside `pool::install`.
- `Role` is matched exhaustively. No `_ => Issue` arms.
- Tests that wait on a process poll for the expected state rather than sleeping a fixed interval. CI runs ubuntu, macos, and windows.
- Conventional Commits. Stage A's final commit, the `baseline_path` removal, and stage B's final commit are each `feat!` with a `BREAKING CHANGE` footer.
- Comments state a non-obvious why, never narrate the change. No `this PR`, no `used to`, no issue references.

## Lock ordering

Two locks exist in stage B, both under `<baseline_dir>/.locks/`. **Directory lock, then slot lock, never the reverse.**

| Caller | Takes |
|---|---|
| `baseline::ensure` | slot only |
| `baseline::drop_reference` | directory, then slot |
| `baseline::prune_all` | directory, then slot per baseline |

Both waits are unbounded. A function that takes a lock never calls another function that takes the same lock: `flock` on two open file descriptions of one file blocks even within a single process, so a locked wrapper always delegates to an unlocked body that the sweep can call directly.

---

## File structure

**Stage A**

| File | Responsibility |
|---|---|
| `crates/devkit-config/src/lib.rs` | `Defaults` fields all `#[serde(default)]`; `resolve` gains a `default_worktree_root` parameter; the missing-`[defaults]` bail is deleted; `pr_base` default becomes `main` |
| `crates/devkit-common/src/git.rs` | `derived_worktree_root`, `non_bare_main`, `default_remote_branch` |
| `crates/devkit-common/src/config.rs` | computes the derived default and passes it into `devkit_config::resolve` |
| `src/bin/devkit/baseline.rs` (new) | `target()`, the one place `baseline_ref`-or-detection is resolved |
| `src/bin/devkit/issue/setup.rs`, `issue/checkout.rs`, `run/mod.rs` | call `baseline::target` instead of reading `baseline_ref` directly |
| `schema/devkit-config.json` | regenerated |
| `docs/configuration.md`, `AGENTS.md` | key table, `_worktrees` convention |

**Stage B**

| File | Responsibility |
|---|---|
| `src/bin/devkit/baseline/mod.rs` | `pin`, `Marker`, `slot`, `ensure`, `fingerprint`, `referencers`, `drop_reference`, `prune_all` |
| `src/bin/devkit/baseline/locks.rs` | `with_dir`, `with_slot`, `reentry_conflict` |
| `crates/devkit-config/src/lib.rs` | `baseline_dir` added, then `baseline_path` removed with its migration; reserved template-variable names rejected |
| `crates/devkit-common/src/worktree.rs` | `BASELINE_MARKER`, `is_baseline`, and `discover` filtering on it |
| `crates/devkit-common/src/record.rs` | `baseline` field, atomic write, `read_state` distinguishing absent from corrupt |
| `crates/devkit-common/src/cmd.rs` | `capture_env`, so a hook child can carry the re-entry marker without the parent mutating its own environment |
| `src/bin/devkit/issue/hooks.rs` | `run_all` passes an env slice through to each hook |
| `src/bin/devkit/run/baseline.rs` | deleted |
| `src/bin/devkit/run/mod.rs` | `cmd_up` calls `baseline::ensure`; repin brings down abandoned rows; `Cmd::Baseline` group; `build_selector`/`touches_foreign` sole-referencer rule |
| `src/bin/devkit/issue/end.rs` | `cleanup` calls `drop_reference` after removing its own worktree |
| `crates/devkit-ports/src/strays/mod.rs` | `baseline_dir` managed root, longest-root-first attribution |
| `src/bin/devkit/doctor.rs` | `baseline_orphans` row |

**Task order and the `baseline_path` removal.** `baseline_dir` is *added* in B2 while `baseline_path` stays on `Defaults`, unused by anything new. Its live readers go away in B10 (`cmd_up`), B13 (`strays::managed_roots`), and B14 (the schema starter). B14 then deletes the field and adds the migration. Deleting it any earlier leaves the workspace red across a run of commits.

---

# Stage A: config relaxation

### Task A1: Every `[defaults]` key optional

**Files:**
- Modify: `crates/devkit-config/src/lib.rs:291-305` (the four required fields), `crates/devkit-config/src/lib.rs:959-969` (the bail)
- Test: `crates/devkit-config/src/lib.rs` (the existing `mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces: `Defaults` deserializes from an empty or absent table. `Config` still exposes `defaults.worktree_root: String`, `branch_prefix: String`, `baseline_ref: String`, `baseline_path: String`, each empty when unset.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/devkit-config/src/lib.rs`:

```rust
#[test]
fn a_defaults_table_with_one_key_deserializes() {
    let cfg: Config = toml::from_str("[defaults]\nbranch_prefix = 'lev/'\n").unwrap();
    assert_eq!(cfg.defaults.branch_prefix, "lev/");
    assert_eq!(cfg.defaults.worktree_root, "");
    assert_eq!(cfg.defaults.baseline_ref, "");
}

#[test]
fn a_config_with_a_section_but_no_defaults_resolves() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[config]\nroot = true\n[github]\nissues_repo = 'a/b'\n",
    )
    .unwrap();
    let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None).unwrap();
    assert_eq!(cfg.defaults.worktree_root, "");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-config a_defaults_table_with_one_key a_config_with_a_section`
Expected: FAIL. The first with `missing field 'worktree_root'`, the second with the `missing field 'defaults'` bail.

- [ ] **Step 3: Add the defaults and delete the bail**

In `Defaults`, put `#[serde(default)]` on `worktree_root`, `branch_prefix`, `baseline_ref`, and `baseline_path`. `impl Default for Defaults` already produces empty strings for all four, so it needs no change.

Delete this block from `resolve_with_home`:

```rust
    if !merged.contains_key("defaults")
        && let Some(section) = merged
            .keys()
            .find(|k| !STANDALONE_SECTIONS.contains(&k.as_str()))
    {
        anyhow::bail!(
            "deserializing merged devkit config: missing field `defaults`, \
             which `[{section}]` needs"
        );
    }
```

`STANDALONE_SECTIONS` loses its only consumer. Delete the constant and its doc comment.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS. Any test asserting the old bail's message fails here; delete those tests, since the behavior they pin is the behavior being removed.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-config/src/lib.rs
git commit -m "feat(config): make every [defaults] key optional"
```

---

### Task A2: Derived `worktree_root`

**Files:**
- Modify: `crates/devkit-common/src/git.rs`, `crates/devkit-common/src/config.rs:16-30`, `crates/devkit-config/src/lib.rs` (`resolve`, `resolve_with_home`, `health_with_home`, `resolve_defaults`), plus every call site the compiler names
- Test: `crates/devkit-common/src/git.rs` (`mod tests`), `crates/devkit-config/src/lib.rs` (`mod tests`), `crates/devkit-common/src/config.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task A1's optional `worktree_root`.
- Produces:

```rust
// devkit_common::git
pub fn derived_worktree_root(primary: &Path) -> Option<PathBuf>;
/// The main worktree's path, or `None` when it is bare.
pub fn non_bare_main(start: &Path) -> Result<Option<PathBuf>>;

// devkit_config
pub fn resolve(
    explicit: Option<&Path>,
    start: &Path,
    main_checkout: Option<&Path>,
    checkout_root: Option<&Path>,
    default_worktree_root: Option<&Path>,
) -> Result<(Config, HashMap<String, PathBuf>)>;
```

The new parameter is last. Every caller outside `devkit_common::config::resolve` passes `None`.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-common/src/git.rs` `mod tests`:

```rust
#[test]
fn the_derived_worktree_root_is_the_underscore_sibling() {
    let got = derived_worktree_root(Path::new("/home/lev/Git/lev/devkit"));
    assert_eq!(got, Some(PathBuf::from("/home/lev/Git/lev/devkit_worktrees")));
}

#[test]
fn a_path_with_no_parent_derives_nothing() {
    assert_eq!(derived_worktree_root(Path::new("/")), None);
}

#[test]
fn a_bare_main_worktree_has_no_non_bare_main() {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("origin.git");
    let seed = tmp.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        Git::fixture(cwd).args(args.iter().copied()).output().unwrap()
    };
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("f"), "x").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-qm", "init"]);
    git(tmp.path(), &["clone", "-q", "--bare", seed.to_str().unwrap(), bare.to_str().unwrap()]);

    // A linked worktree of a bare repository: `checkout_root` succeeds and
    // names this worktree, so deriving from it would give every worktree its
    // own root. `non_bare_main` is the value that must stay empty.
    let wt = tmp.path().join("wt");
    git(&bare, &["worktree", "add", "--detach", wt.to_str().unwrap()]);
    assert_eq!(non_bare_main(&wt).unwrap(), None);
}
```

In `crates/devkit-config/src/lib.rs` `mod tests`:

```rust
#[test]
fn an_unset_worktree_root_takes_the_derived_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[config]\nroot = true\n[defaults]\nbranch_prefix = 'lev/'\n",
    )
    .unwrap();
    let derived = dir.path().join("proj_worktrees");
    let (cfg, _) =
        resolve_with_home(None, dir.path(), None, None, None, Some(derived.as_path())).unwrap();
    assert_eq!(
        Path::new(&cfg.defaults.worktree_root),
        normalize_lexically(&derived)
    );
}

#[test]
fn a_set_worktree_root_wins_over_the_derived_default() {
    let dir = tempfile::tempdir().unwrap();
    let explicit = dir.path().join("explicit");
    std::fs::write(
        dir.path().join("devkit.toml"),
        format!(
            "[config]\nroot = true\n[defaults]\nworktree_root = '{}'\n",
            explicit.display().to_string().replace('\\', "\\\\")
        ),
    )
    .unwrap();
    let derived = dir.path().join("proj_worktrees");
    let (cfg, _) =
        resolve_with_home(None, dir.path(), None, None, None, Some(derived.as_path())).unwrap();
    assert_eq!(
        Path::new(&cfg.defaults.worktree_root),
        normalize_lexically(&explicit)
    );
}
```

In `crates/devkit-common/src/config.rs` `mod tests`. Without this, a `None` passed into `devkit_config::resolve` leaves every test above green while the binary derives nothing:

```rust
#[test]
fn the_common_door_passes_the_derived_root_through() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("proj");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        crate::git::Git::fixture(&repo).args(args.iter().copied()).output().unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("devkit.toml"), "[config]\nroot = true\n[defaults]\n").unwrap();

    let cfg = resolve(None, &repo).unwrap().config;
    assert!(
        cfg.defaults.worktree_root.ends_with("proj_worktrees"),
        "derived root not threaded through: {}",
        cfg.defaults.worktree_root
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-common derived_worktree_root non_bare_main the_common_door; cargo nextest run -p devkit-config worktree_root`
Expected: FAIL. `derived_worktree_root` and `non_bare_main` are undefined and `resolve_with_home` takes five parameters.

- [ ] **Step 3: Implement**

In `crates/devkit-common/src/git.rs`:

```rust
/// The conventional worktree directory for a checkout: its own name plus
/// `_worktrees`, beside it. The underscore separates the suffix from a project
/// name, which commonly contains hyphens.
pub fn derived_worktree_root(primary: &Path) -> Option<PathBuf> {
    let name = primary.file_name()?.to_str()?;
    Some(primary.parent()?.join(format!("{name}_worktrees")))
}

/// The repository's main worktree, or `None` when it is bare. Distinct from
/// [`primary_checkout`], which falls back to the caller's own checkout: from a
/// linked worktree of a bare repository that fallback names the linked worktree
/// itself, so anything derived per-repository must not use it.
pub fn non_bare_main(start: &Path) -> Result<Option<PathBuf>> {
    Ok(worktrees(start)?
        .first()
        .filter(|w| !w.bare)
        .map(|w| w.path.clone()))
}
```

In `crates/devkit-config/src/lib.rs`, thread `default_worktree_root: Option<&Path>` through `resolve`, `resolve_with_home`, and `health_with_home` as the last parameter, and apply it in `resolve_defaults` before the path resolution runs:

```rust
    if cfg.defaults.worktree_root.is_empty()
        && let Some(d) = default_worktree_root
    {
        cfg.defaults.worktree_root = d.to_string_lossy().into_owned();
    }
```

The assignment happens before `resolve_path_key` so a derived absolute path passes through `normalize_lexically` like any other.

In `crates/devkit-common/src/config.rs`:

```rust
    // Keyed off the main worktree alone: a bare repository has no directory to
    // put a `_worktrees` sibling beside, and falling back to the caller's own
    // checkout would give every linked worktree a different root.
    let derived = crate::git::non_bare_main(start)
        .ok()
        .flatten()
        .as_deref()
        .and_then(crate::git::derived_worktree_root);
    let resolved = devkit_config::resolve(
        explicit,
        start,
        main_checkout.as_deref(),
        checkout_root.as_deref(),
        derived.as_deref(),
    );
```

A bare main worktree therefore leaves `worktree_root` empty: resolution succeeds and the error arrives at the point of use.

- [ ] **Step 4: Fix every call site and run the gate**

Run: `cargo build --workspace --all-targets`
The compiler names each `resolve` / `resolve_with_home` / `health_with_home` call. There are roughly forty, across `crates/devkit-config/src/lib.rs`, `crates/devkit-config/tests/relative_config_paths.rs`, `crates/devkit-config/tests/repo_relative_anchor.rs`, and `crates/devkit-common/src/config.rs`. Each gets a trailing `None` except the one in `devkit_common::config::resolve`.

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/devkit-common crates/devkit-config
git status --short
git commit -m "feat(config): derive worktree_root from the primary checkout"
```

`git status --short` must show nothing outside those two crates. Anything else belongs to a different change and gets unstaged first.

---

### Task A3: Resolve the baseline target

**Files:**
- Create: `src/bin/devkit/baseline.rs`
- Modify: `src/bin/devkit/main.rs` (add `mod baseline;`), `crates/devkit-common/src/git.rs`, `src/bin/devkit/issue/setup.rs:513`, `src/bin/devkit/issue/checkout.rs:421`, `src/bin/devkit/run/mod.rs:615-632`, `src/bin/devkit/run/mod.rs:642`
- Test: `src/bin/devkit/baseline.rs` (`mod tests`), `crates/devkit-common/src/git.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task A1's optional `baseline_ref`.
- Produces: `devkit_common::git::default_remote_branch(repo: &Path) -> Result<String>` returning e.g. `origin/main`; `crate::baseline::target(cfg: &devkit_config::Config, repo: &Path) -> Result<String>`.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-common/src/git.rs` `mod tests`:

```rust
#[test]
fn the_default_remote_branch_comes_from_origin_head() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    Git::fixture(p).args(["init", "-q", "-b", "main"]).output().unwrap();
    std::fs::write(p.join("f"), "x").unwrap();
    Git::fixture(p).args(["add", "."]).output().unwrap();
    Git::fixture(p).args(["commit", "-qm", "init"]).output().unwrap();
    Git::fixture(p)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"])
        .output()
        .unwrap();
    assert_eq!(default_remote_branch(p).unwrap(), "origin/main");
}

#[test]
fn a_repo_without_origin_head_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    Git::fixture(p).args(["init", "-q"]).output().unwrap();
    assert!(default_remote_branch(p).is_err());
}
```

In `src/bin/devkit/baseline.rs` `mod tests`:

```rust
#[test]
fn a_configured_ref_wins_over_detection() {
    let mut cfg = devkit_config::Config::default();
    cfg.defaults.baseline_ref = "origin/release".into();
    // Detection would fail in a non-repo path; the configured ref means it is
    // never consulted.
    let got = target(&cfg, std::path::Path::new("/nonexistent")).unwrap();
    assert_eq!(got, "origin/release");
}

#[test]
fn an_undetectable_target_names_both_fixes() {
    let tmp = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(tmp.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    let err = target(&devkit_config::Config::default(), tmp.path()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("baseline_ref"), "{msg}");
    assert!(msg.contains("git remote set-head"), "{msg}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-common default_remote_branch; cargo nextest run --bin devkit baseline::`
Expected: FAIL. Both functions are undefined.

- [ ] **Step 3: Implement**

In `crates/devkit-common/src/git.rs`:

```rust
/// The remote's default branch, e.g. `origin/main`, from the `origin/HEAD`
/// symbolic ref. `git clone` sets it; `git init` plus a manually added remote
/// does not, which is why the caller has a fallback to offer.
pub fn default_remote_branch(repo: &Path) -> Result<String> {
    let out = Git::at(repo)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()?;
    let s = out.trim();
    if s.is_empty() {
        anyhow::bail!("origin/HEAD names no branch");
    }
    Ok(s.to_string())
}
```

Create `src/bin/devkit/baseline.rs`:

```rust
//! Where the baseline comparison starts. Every consumer of `baseline_ref`
//! resolves through here, so a project that declares nothing still gets the
//! remote's default branch.

use anyhow::{Context, Result};
use devkit_config::Config;
use std::path::Path;

/// The ref a worktree's baseline is measured against: the configured
/// `baseline_ref`, else the remote's default branch.
pub fn target(cfg: &Config, repo: &Path) -> Result<String> {
    if !cfg.defaults.baseline_ref.is_empty() {
        return Ok(cfg.defaults.baseline_ref.clone());
    }
    devkit_common::git::default_remote_branch(repo).context(
        "no baseline target: set `defaults.baseline_ref`, \
         or run `git remote set-head origin -a` so origin/HEAD names one",
    )
}
```

Add `mod baseline;` to `src/bin/devkit/main.rs`.

Replace each direct read of `cfg.defaults.baseline_ref` with a `baseline::target(cfg, repo)?` call. `rg -n "defaults\.baseline_ref" src/` lists them: `src/bin/devkit/issue/setup.rs` (the `worktree add` start point and the step count), `src/bin/devkit/issue/checkout.rs`, and both sites in `src/bin/devkit/run/mod.rs` (the diff format string and the "no apps to run" message). Resolve the target once per command and reuse it rather than calling `target` twice in one function.

Guard the empty `baseline_path` at its use site in `cmd_up`, in the `Role::Baseline` arm before `ensure_fresh`:

```rust
    anyhow::ensure!(
        !cfg.defaults.baseline_path.is_empty(),
        "`--role baseline` needs `defaults.baseline_path`"
    );
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/baseline.rs src/bin/devkit/main.rs crates/devkit-common/src/git.rs src/bin/devkit/issue/setup.rs src/bin/devkit/issue/checkout.rs src/bin/devkit/run/mod.rs
git commit -m "feat(issue): resolve the baseline target from origin/HEAD"
```

---

### Task A4: `pr_base` default, schema, docs

**Files:**
- Modify: `crates/devkit-config/src/lib.rs:377-379`, `schema/devkit-config.json`, `docs/configuration.md:113-147`, `docs/configuration.md:572`, `AGENTS.md`
- Test: `crates/devkit-config/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: Tasks A1 through A3.
- Produces: nothing new. This task closes stage A.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn pr_base_defaults_to_main() {
    let cfg: Config = toml::from_str("[defaults]\n").unwrap();
    assert_eq!(cfg.defaults.pr_base, "main");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p devkit-config pr_base_defaults_to_main`
Expected: FAIL, `assertion failed: left == "staging"`.

- [ ] **Step 3: Implement**

```rust
fn default_pr_base() -> String {
    "main".to_string()
}
```

Update `docs/configuration.md`: the `pr_base` row's default, the `worktree_root` row to say it defaults to the primary checkout's `_worktrees` sibling, the four keys' Required column to `no`, and the `../myproject-worktrees` example at line 142 plus the sample at 570 to the `_worktrees` form. Update the worktrees convention in `AGENTS.md` to match.

Regenerate the schema:

```bash
DEVKIT_UPDATE_SCHEMA=1 cargo test schema
```

- [ ] **Step 4: Run the full gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: PASS, and `git diff --stat schema/devkit-config.json` shows the regenerated file.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-config/src/lib.rs schema/devkit-config.json docs/configuration.md AGENTS.md
git commit -m "feat(config)!: default pr_base to main

Every [defaults] key is now optional, so a personal config layer that
sets one key no longer breaks resolution in directories that are not
devkit projects.

BREAKING CHANGE: defaults.pr_base defaults to main instead of staging.
Projects that target staging must set it explicitly."
```

---

# Stage B: the baseline redesign

### Task B1: Pin resolution

**Files:**
- Modify: `src/bin/devkit/baseline.rs`
- Test: `src/bin/devkit/baseline.rs` (`mod tests`)

**Interfaces:**
- Consumes: `baseline::target` from Task A3.
- Produces: `crate::baseline::pin(worktree: &Path, target: &str) -> Result<String>` returning a full 40-character sha.

- [ ] **Step 1: Write the failing tests**

```rust
/// Two commits on main, a branch cut from the first, then main advances:
/// the merge base stays at the fork point.
#[test]
fn the_pin_is_the_fork_point_not_the_tip() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let git = |args: &[&str]| {
        devkit_common::git::Git::fixture(p).args(args.iter().copied()).output().unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(p.join("a"), "1").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "one"]);
    let fork = git(&["rev-parse", "HEAD"]).trim().to_string();
    git(&["checkout", "-qb", "feat"]);
    std::fs::write(p.join("b"), "2").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "two"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(p.join("c"), "3").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "three"]);
    git(&["checkout", "-q", "feat"]);

    assert_eq!(pin(p, "main").unwrap(), fork);
}

#[test]
fn unrelated_histories_error_naming_both_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path();
    let git = |args: &[&str]| {
        devkit_common::git::Git::fixture(p).args(args.iter().copied()).output().unwrap()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(p.join("a"), "1").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "one"]);
    git(&["checkout", "-q", "--orphan", "lonely"]);
    git(&["commit", "-qm", "orphan", "--allow-empty"]);

    let err = pin(p, "main").unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("main"), "{msg}");
    assert!(msg.contains("share no history"), "{msg}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit baseline::tests`
Expected: FAIL, `pin` is undefined.

- [ ] **Step 3: Implement**

```rust
/// The commit this worktree forked from: the merge base of its HEAD and
/// `target`. Local refs only, so no fetch is needed — extending a branch does
/// not move its merge base with another branch, and the value changes only when
/// the worktree is rebased.
pub fn pin(worktree: &Path, target: &str) -> Result<String> {
    let out = devkit_common::git::Git::at(worktree)
        .args(["merge-base", "HEAD", target])
        .output()
        .with_context(|| {
            format!("HEAD and {target} share no history, so no baseline exists between them; \
                     set `defaults.baseline_ref` to a ref this branch descends from")
        })?;
    let sha = out.trim();
    anyhow::ensure!(!sha.is_empty(), "`git merge-base HEAD {target}` named no commit");
    Ok(sha.to_string())
}
```

`git merge-base` exits 1 with empty stdout and empty stderr for unrelated histories, so `Git::output`'s error carries no explanation of its own. The `context` is what makes it readable.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --bin devkit baseline::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/baseline.rs
git commit -m "feat(run): pin the baseline to the worktree's fork point"
```

---

### Task B2: Add `baseline_dir`

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`Defaults`, `resolve_defaults`)
- Test: `crates/devkit-config/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task A2's `default_worktree_root` plumbing.
- Produces: `cfg.defaults.baseline_dir: String`, resolved as a host path and defaulting to `<worktree_root>/_baselines`.

`baseline_path` stays on `Defaults` and keeps working. Task B14 removes it, once B10 and B13 have retired its live readers.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn baseline_dir_defaults_under_worktree_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    std::fs::write(
        dir.path().join("devkit.toml"),
        format!(
            "[config]\nroot = true\n[defaults]\nworktree_root = '{}'\n",
            root.display().to_string().replace('\\', "\\\\")
        ),
    )
    .unwrap();
    let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
    assert_eq!(
        Path::new(&cfg.defaults.baseline_dir),
        normalize_lexically(&root.join("_baselines"))
    );
}

#[test]
fn an_explicit_baseline_dir_wins_over_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("w");
    let explicit = dir.path().join("elsewhere");
    std::fs::write(
        dir.path().join("devkit.toml"),
        format!(
            "[config]\nroot = true\n[defaults]\nworktree_root = '{}'\nbaseline_dir = '{}'\n",
            root.display().to_string().replace('\\', "\\\\"),
            explicit.display().to_string().replace('\\', "\\\\")
        ),
    )
    .unwrap();
    let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
    assert_eq!(
        Path::new(&cfg.defaults.baseline_dir),
        normalize_lexically(&explicit)
    );
}

#[test]
fn no_worktree_root_leaves_baseline_dir_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("devkit.toml"), "[config]\nroot = true\n[defaults]\n").unwrap();
    let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
    assert_eq!(cfg.defaults.baseline_dir, "");
}
```

Every path here joins onto `tempdir()`. A literal `/w` is not absolute on Windows, where `resolve_path_key` anchors it to the layer directory and the assertion then compares two different trees.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-config baseline_dir`
Expected: FAIL, `baseline_dir` does not exist.

- [ ] **Step 3: Implement**

In `Defaults`, beside `baseline_path`:

```rust
    /// Directory baseline worktrees are created under, one per fork-point
    /// commit. `~` is expanded. Names a location on this machine, so a relative
    /// value anchors to the directory of the layer that declared it. Defaults to
    /// `_baselines` under `worktree_root`.
    #[serde(default)]
    pub baseline_dir: String,
```

Add it to `impl Default for Defaults` as `String::new()`.

In `resolve_defaults`, default it before resolving, and resolve it as `PathKind::Host` alongside `worktree_root`:

```rust
    // After `worktree_root` is resolved, so a derived value is already absolute
    // and normalized.
    if cfg.defaults.baseline_dir.is_empty() && !cfg.defaults.worktree_root.is_empty() {
        cfg.defaults.baseline_dir = Path::new(&cfg.defaults.worktree_root)
            .join("_baselines")
            .to_string_lossy()
            .into_owned();
    }
```

Order matters: `worktree_root`'s own `resolve_path_key` runs first, then this default, then `baseline_dir`'s `resolve_path_key`, which is a no-op for an already-absolute value and anchors an explicitly configured relative one to its declaring layer.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS. Nothing else changes: `baseline_dir` has no readers yet.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-config/src/lib.rs
git commit -m "feat(config): add defaults.baseline_dir"
```

---

### Task B3: The marker file

**Files:**
- Modify: `src/bin/devkit/baseline.rs`, `crates/devkit-common/src/worktree.rs`
- Test: `src/bin/devkit/baseline.rs` (`mod tests`), `crates/devkit-common/src/worktree.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces:

```rust
// devkit_common::worktree — the marker's path and its existence test live here
// because `discover` (Task B8) needs both and `devkit-common` cannot depend on
// the binary. One definition, two consumers.
pub const BASELINE_MARKER: &str = ".devkit/baseline.toml";
pub fn is_baseline(worktree: &Path) -> bool;

// crate::baseline — the marker's contents
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppMark { pub fingerprint: String }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub sha: String,
    #[serde(default)]
    pub apps: BTreeMap<String, AppMark>,
}

pub enum MarkerState { Ok(Marker), Unusable, Absent }
pub fn read_marker(dir: &Path) -> MarkerState;
pub fn write_marker(dir: &Path, m: &Marker) -> Result<()>;
```

- [ ] **Step 1: Write the failing tests**

In `src/bin/devkit/baseline.rs` `mod tests`:

```rust
#[test]
fn a_marker_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let mut apps = std::collections::BTreeMap::new();
    apps.insert("api".to_string(), AppMark { fingerprint: "9f2c".into() });
    let m = Marker { sha: "d13d90b724bf".into(), apps };
    write_marker(dir.path(), &m).unwrap();
    assert!(matches!(read_marker(dir.path()), MarkerState::Ok(got) if got == m));
}

#[test]
fn an_absent_marker_is_absent_and_a_corrupt_one_is_unusable() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(read_marker(dir.path()), MarkerState::Absent));
    std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
    std::fs::write(dir.path().join(devkit_common::worktree::BASELINE_MARKER), "sha = ").unwrap();
    assert!(matches!(read_marker(dir.path()), MarkerState::Unusable));
}
```

In `crates/devkit-common/src/worktree.rs` `mod tests`:

```rust
#[test]
fn a_directory_with_the_marker_is_a_baseline() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!is_baseline(dir.path()));
    std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
    std::fs::write(dir.path().join(BASELINE_MARKER), "sha = 'abc'\n").unwrap();
    assert!(is_baseline(dir.path()));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit baseline::tests::a_marker; cargo nextest run -p devkit-common is_a_baseline`
Expected: FAIL, the types and `is_baseline` are undefined.

- [ ] **Step 3: Implement**

In `crates/devkit-common/src/worktree.rs`:

```rust
/// Marker identifying a baseline worktree. Baselines are linked worktrees, so
/// without a way to tell one apart each becomes an `UNKNOWN` row in
/// `issue status`, `sync-includes --all` copies into it, and
/// `--clean-worktree` can remove one while worktrees still reference it.
pub const BASELINE_MARKER: &str = ".devkit/baseline.toml";

/// Whether a worktree is a baseline. Uses `metadata` rather than `exists`,
/// which folds every error into `false`: a permission failure would otherwise
/// classify a baseline as an issue worktree, which is the unsafe direction.
pub fn is_baseline(worktree: &Path) -> bool {
    match std::fs::metadata(worktree.join(BASELINE_MARKER)) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}
```

In `src/bin/devkit/baseline.rs`:

```rust
use devkit_common::worktree::BASELINE_MARKER;

/// Written last, after every bootstrap step, so its presence is what makes a
/// baseline complete: a directory without one is an interrupted bootstrap
/// whatever its HEAD says. It also carries identity, which lets a stray
/// directory be told from a real baseline, and each app's prep fingerprint.
pub fn write_marker(dir: &Path, m: &Marker) -> Result<()> {
    let p = dir.join(BASELINE_MARKER);
    let parent = p.parent().expect("marker path has a parent");
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let body = toml::to_string(m).context("serializing baseline marker")?;
    // Rename rather than write in place: a crash partway through a write would
    // otherwise leave a file that parses as neither a marker nor its absence.
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("renaming into {}", p.display()))
}

pub fn read_marker(dir: &Path) -> MarkerState {
    match std::fs::read_to_string(dir.join(BASELINE_MARKER)) {
        Err(_) => MarkerState::Absent,
        Ok(body) => match toml::from_str(&body) {
            Ok(m) => MarkerState::Ok(m),
            Err(_) => MarkerState::Unusable,
        },
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/baseline.rs crates/devkit-common/src/worktree.rs
git commit -m "feat(run): add the baseline marker file"
```

---

### Task B4: Slot resolution

**Files:**
- Modify: `src/bin/devkit/baseline.rs`
- Test: `src/bin/devkit/baseline.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task B3's `read_marker`.
- Produces:

```rust
pub enum Slot { Reuse(PathBuf, Marker), Rebuild(PathBuf), Create(PathBuf) }
pub fn slot(baseline_dir: &Path, sha: &str) -> Slot;
pub fn short(sha: &str) -> &str;   // first 12 chars
```

- [ ] **Step 1: Write the failing tests**

```rust
const SHA: &str = "d13d90b724bf8a3c0000000000000000000000ab";
const OTHER: &str = "0123456789ab0000000000000000000000000000";

fn place(root: &std::path::Path, name: &str, sha: &str) {
    let d = root.join(name);
    std::fs::create_dir_all(&d).unwrap();
    write_marker(&d, &Marker { sha: sha.into(), apps: Default::default() }).unwrap();
}

#[test]
fn an_empty_dir_creates_at_the_short_sha() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(slot(root.path(), SHA), Slot::Create(p) if p == root.path().join("d13d90b724bf")));
}

#[test]
fn a_matching_marker_is_reused() {
    let root = tempfile::tempdir().unwrap();
    place(root.path(), "d13d90b724bf", SHA);
    assert!(matches!(slot(root.path(), SHA), Slot::Reuse(..)));
}

#[test]
fn a_colliding_marker_moves_to_the_next_candidate() {
    let root = tempfile::tempdir().unwrap();
    place(root.path(), "d13d90b724bf", OTHER);
    assert!(matches!(slot(root.path(), SHA), Slot::Create(p) if p.ends_with("d13d90b724bf_2")));
}

#[test]
fn a_markerless_directory_is_rebuilt_in_place() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("d13d90b724bf")).unwrap();
    assert!(matches!(slot(root.path(), SHA), Slot::Rebuild(p) if p.ends_with("d13d90b724bf")));
}

#[test]
fn a_corrupt_marker_is_rebuilt_in_place() {
    let root = tempfile::tempdir().unwrap();
    let d = root.path().join("d13d90b724bf");
    std::fs::create_dir_all(d.join(".devkit")).unwrap();
    std::fs::write(d.join(devkit_common::worktree::BASELINE_MARKER), "sha = ").unwrap();
    assert!(matches!(slot(root.path(), SHA), Slot::Rebuild(_)));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit baseline::tests`
Expected: FAIL, `slot` is undefined.

- [ ] **Step 3: Implement**

```rust
/// Directory-name form of a sha. Twelve hex characters is 48 bits against a few
/// dozen directories, and it leaves Windows path headroom a 40-character name
/// would spend.
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Which directory serves `sha`, and in what state. An interrupted bootstrap
/// leaves a registered worktree with no marker; classifying that as occupied
/// would strand it, since the baseline would move to `_2`, prune reports rather
/// than removes it, and the worktree filter would stop recognizing it.
pub fn slot(baseline_dir: &Path, sha: &str) -> Slot {
    let base = short(sha);
    for n in 1u32.. {
        let name = if n == 1 { base.to_string() } else { format!("{base}_{n}") };
        let path = baseline_dir.join(&name);
        match read_marker(&path) {
            MarkerState::Ok(m) if m.sha == sha => return Slot::Reuse(path, m),
            MarkerState::Ok(_) => continue,
            MarkerState::Unusable => return Slot::Rebuild(path),
            MarkerState::Absent => {
                return match std::fs::metadata(&path) {
                    Ok(_) => Slot::Rebuild(path),
                    Err(_) => Slot::Create(path),
                };
            }
        }
    }
    unreachable!("the loop returns on the first free or rebuildable slot")
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --bin devkit baseline::tests`
Expected: PASS, five new tests.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/baseline.rs
git commit -m "feat(run): resolve a baseline's slot from its marker"
```

---

### Task B5: The record's baseline field

**Files:**
- Modify: `crates/devkit-common/src/record.rs`, plus every `IssueRecord` literal the compiler names (`src/bin/devkit/issue/setup.rs`, `src/bin/devkit/issue/checkout.rs`, and their test modules)
- Test: `crates/devkit-common/src/record.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaselinePin { pub sha: String, pub path: String }
// IssueRecord gains: pub baseline: Option<BaselinePin>
pub enum RecordState { Ok(IssueRecord), Unusable, Absent }
pub fn read_state(worktree: &Path) -> RecordState;
```

`record::read` keeps its current signature and behavior for existing callers.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_record_without_a_baseline_still_reads() {
    let rec: IssueRecord = toml::from_str("issue = 'ENG-1'\nslug = 'x'\napps = []\n").unwrap();
    assert_eq!(rec.baseline, None);
}

#[test]
fn a_baseline_pin_round_trips_through_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let rec = IssueRecord {
        issue: "ENG-1".into(),
        slug: "x".into(),
        apps: vec![],
        summary: None,
        pr: None,
        baseline: Some(BaselinePin { sha: "d13d90b724bf".into(), path: "/b/d13d".into() }),
    };
    write(dir.path(), &rec).unwrap();
    assert_eq!(read(dir.path()), Some(rec));
}

#[test]
fn a_corrupt_record_is_unusable_not_absent() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(read_state(dir.path()), RecordState::Absent));
    std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
    std::fs::write(dir.path().join(".devkit").join("issue.toml"), "issue = ").unwrap();
    assert!(matches!(read_state(dir.path()), RecordState::Unusable));
}

/// The rename is what makes a write atomic: a reader sees either the whole
/// previous record or the whole new one, never a truncated file. Asserting the
/// temp file is gone is what pins the rename — a plain `fs::write` would also
/// pass a round-trip assertion.
#[test]
fn a_write_leaves_no_temp_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let rec = IssueRecord {
        issue: "ENG-1".into(),
        slug: "x".into(),
        apps: vec![],
        summary: None,
        pr: None,
        baseline: None,
    };
    write(dir.path(), &rec).unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join(".devkit"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
    assert_eq!(read(dir.path()), Some(rec));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-common record::`
Expected: FAIL, `baseline`, `BaselinePin`, and `read_state` are undefined.

- [ ] **Step 3: Implement**

Add to `IssueRecord`:

```rust
    /// The baseline this worktree compares against, written whenever
    /// `devrun up` resolves one. Absent on records written before baselines
    /// were per-worktree, and on a worktree that has never run one. The path is
    /// stored alongside the sha because it is what `issue end` deletes: a sha
    /// alone would resolve somewhere new the moment `baseline_dir` changed,
    /// orphaning every existing baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselinePin>,
```

Make `write` atomic with the same temp-plus-rename shape as `write_marker`, and add:

```rust
/// `read`, but distinguishing a record that is absent from one that exists and
/// does not parse. The baseline reference count needs the distinction: counting
/// a corrupt record as "no reference" would delete a baseline out from under a
/// worktree serving from it.
pub fn read_state(worktree: &Path) -> RecordState {
    match std::fs::read_to_string(path(worktree)) {
        Err(_) => RecordState::Absent,
        Ok(body) => match toml::from_str(&body) {
            Ok(r) => RecordState::Ok(r),
            Err(_) => RecordState::Unusable,
        },
    }
}
```

- [ ] **Step 4: Fix every literal and run the gate**

Run: `cargo build --workspace --all-targets`
Every `IssueRecord { .. }` literal now needs `baseline: None`. The compiler names each: `src/bin/devkit/issue/setup.rs`, `src/bin/devkit/issue/checkout.rs`, and their test modules.

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/record.rs src/bin/devkit/issue/setup.rs src/bin/devkit/issue/checkout.rs
git status --short
git commit -m "feat(issue): record the baseline a worktree compares against"
```

---

### Task B6: Locks

**Files:**
- Create: `src/bin/devkit/baseline/locks.rs` (converting `baseline.rs` into `baseline/mod.rs`)
- Test: `src/bin/devkit/baseline/locks.rs` (`mod tests`)

**Interfaces:**
- Consumes: nothing.
- Produces:

```rust
/// Set on a hook child's environment during bootstrap, carrying the slot name
/// being built.
pub const REENTRY_VAR: &str = "DEVKIT_BASELINE_BOOTSTRAP";

/// Serializes work on one baseline slot. The key is the slot **directory
/// name** (`d13d90b724bf`, or `d13d90b724bf_2` after a collision), not the sha:
/// two shas can share a short prefix and land in different directories, and a
/// caller holding only a path can always name the slot.
pub fn with_slot<T>(baseline_dir: &Path, slot_name: &str, f: impl FnOnce() -> Result<T>) -> Result<T>;

/// Serializes a sweep over the whole directory against concurrent deletions.
pub fn with_dir<T>(baseline_dir: &Path, f: impl FnOnce() -> Result<T>) -> Result<T>;

/// Whether the ambient re-entry marker names the slot about to be locked.
pub fn reentry_conflict(marker: Option<&str>, slot_name: &str) -> bool;
```

Closure-based, matching `crates/devkit-docs/src/locks.rs`: `fd_lock::RwLockWriteGuard` borrows its `RwLock`, so a returned guard would need a self-referential struct. Both waits are unbounded. Both create `<baseline_dir>/.locks/` as needed.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_marker_conflicts_only_with_its_own_slot() {
    assert!(reentry_conflict(Some("d13d90b724bf"), "d13d90b724bf"));
    assert!(!reentry_conflict(Some("d13d90b724bf"), "0123456789ab"));
    assert!(!reentry_conflict(None, "d13d90b724bf"));
    // A collision slot is a different tree and so a different lock.
    assert!(!reentry_conflict(Some("d13d90b724bf"), "d13d90b724bf_2"));
}

#[test]
fn a_slot_lock_runs_its_closure_and_releases() {
    let dir = tempfile::tempdir().unwrap();
    let got = with_slot(dir.path(), "d13d90b724bf", || Ok(7)).unwrap();
    assert_eq!(got, 7);
    // Released: a second acquisition in the same thread would otherwise block
    // forever, since two opens of one lock file are two open-file descriptions.
    assert_eq!(with_slot(dir.path(), "d13d90b724bf", || Ok(8)).unwrap(), 8);
}

#[test]
fn different_slots_do_not_block_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let got = with_slot(dir.path(), "aaaaaaaaaaaa", || {
        with_slot(dir.path(), "bbbbbbbbbbbb", || Ok(1))
    })
    .unwrap();
    assert_eq!(got, 1);
}

#[test]
fn the_dir_lock_and_a_slot_lock_nest_in_that_order() {
    let dir = tempfile::tempdir().unwrap();
    let got = with_dir(dir.path(), || with_slot(dir.path(), "aaaaaaaaaaaa", || Ok(2))).unwrap();
    assert_eq!(got, 2);
}

/// Mutual exclusion, which is the whole point of the lock. Two opens of one
/// lock file are two open-file descriptions, so `flock` serializes them even
/// inside a single process — the same guarantee that holds across processes.
/// The order of the two appends is what proves the contender waited.
#[test]
fn a_contender_waits_for_the_holder() {
    use std::sync::mpsc;

    let dir = tempfile::tempdir().unwrap();
    let log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let (started_tx, started_rx) = mpsc::channel();

    std::thread::scope(|s| {
        with_slot(dir.path(), "aaaaaaaaaaaa", || {
            let log_b = std::sync::Arc::clone(&log);
            let path = dir.path().to_path_buf();
            s.spawn(move || {
                started_tx.send(()).unwrap();
                with_slot(&path, "aaaaaaaaaaaa", || {
                    log_b.lock().unwrap().push('B');
                    Ok(())
                })
                .unwrap();
            });
            // The contender has entered `with_slot` and can only be blocking on
            // the lock this closure holds. Waiting for the signal rather than
            // sleeping keeps the test honest on a loaded runner.
            started_rx.recv().unwrap();
            log.lock().unwrap().push('A');
            Ok(())
        })
        .unwrap();
    });

    assert_eq!(*log.lock().unwrap(), "AB", "the contender did not wait");
}
```

The signal fires just before the contender calls `with_slot`, so a scheduler that
runs `A` first is the expected ordering and a lock that failed to block would
produce `BA`. There is a window where the contender has signalled but not yet
reached the `flock` call; it cannot produce `BA`, because reaching the append
requires acquiring the lock this thread still holds.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit baseline::locks`
Expected: FAIL, the module does not exist.

- [ ] **Step 3: Implement**

Convert `src/bin/devkit/baseline.rs` to `src/bin/devkit/baseline/mod.rs` and add `mod locks;` at its top.

```rust
//! Advisory locks for baseline creation and deletion. Lock files live in
//! `<baseline_dir>/.locks/` rather than inside a baseline, so a lock survives
//! the removal of the tree it guards.

use anyhow::{Context, Result};
use fd_lock::RwLock;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

pub const REENTRY_VAR: &str = "DEVKIT_BASELINE_BOOTSTRAP";

const DIR: &str = ".locks";
const DIR_LOCK: &str = "_dir";

fn lock_path(baseline_dir: &Path, stem: &str) -> PathBuf {
    baseline_dir.join(DIR).join(format!("{stem}.lock"))
}

/// The wait is unbounded on purpose. A worktree racing a long bootstrap must
/// wait and then find a finished tree, and a timed-out acquisition inside a
/// prune sweep would abandon the sweep while still holding the directory lock.
fn hold<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let parent = path.parent().context("lock path has no parent")?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let mut lock = RwLock::new(file);
    let _held = lock
        .write()
        .with_context(|| format!("locking {}", path.display()))?;
    f()
}

pub fn reentry_conflict(marker: Option<&str>, slot_name: &str) -> bool {
    marker == Some(slot_name)
}

pub fn with_slot<T>(
    baseline_dir: &Path,
    slot_name: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    // A hook that runs `devrun up --role baseline` for the baseline currently
    // bootstrapping would block on a lock its own parent holds, forever and
    // silently. The marker turns that into a message naming the hook.
    let marker = std::env::var(REENTRY_VAR).ok();
    if reentry_conflict(marker.as_deref(), slot_name) {
        anyhow::bail!(
            "an `after_worktree_create` hook ran `devrun up --role baseline` for the \
             baseline being bootstrapped ({slot_name}); remove that call from the hook"
        );
    }
    hold(&lock_path(baseline_dir, slot_name), f)
}

pub fn with_dir<T>(baseline_dir: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    hold(&lock_path(baseline_dir, DIR_LOCK), f)
}
```

`DIR_LOCK` is `_dir`, which no 12-hex slot name can equal, so the directory lock and a slot lock are always different files.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --bin devkit baseline::locks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/baseline/
git commit -m "feat(run): add baseline creation and directory locks"
```

---

### Task B7: Bootstrap and reserved context keys

**Files:**
- Modify: `crates/devkit-common/src/cmd.rs`, `src/bin/devkit/issue/hooks.rs`, `src/bin/devkit/issue/setup.rs` (`run_after_worktree_create` gains an env slice; issue-role contexts gain `role`), `src/bin/devkit/issue/end.rs` (its two `run_all` call sites), `src/bin/devkit/issue/checkout.rs` (`with_cleanup` becomes `pub(crate)`; issue-role context gains `role`), `src/bin/devkit/baseline/mod.rs`, `crates/devkit-config/src/lib.rs`
- Test: `crates/devkit-common/src/cmd.rs`, `src/bin/devkit/baseline/mod.rs`, `crates/devkit-config/src/lib.rs`

**Interfaces:**
- Consumes: Tasks B3, B4, B6.
- Produces:

```rust
// devkit_common::cmd
pub fn capture_env(program: &str, args: &[&str], cwd: Option<&str>, env: &[(&str, &str)]) -> Result<String>;

// crate::issue::hooks
pub(crate) fn run_all(cwd: &Path, key: &str, hooks: &[Vec<String>],
                      ctx: &serde_json::Value, vars: &BTreeMap<String, String>,
                      env: &[(&str, &str)], steps: &Steps);

// crate::issue::setup
pub(crate) fn run_after_worktree_create(worktree: &Path, hooks: &[Vec<String>],
                                        ctx: &serde_json::Value,
                                        vars: &BTreeMap<String, String>,
                                        env: &[(&str, &str)], steps: &Steps);

// crate::issue::checkout
pub(crate) fn with_cleanup<T>(worktree: &Path, primary: &str, f: impl FnOnce() -> Result<T>) -> Result<T>;

// crate::baseline
pub fn fingerprint(app: &devkit_ports::apps::App, includes: &[String]) -> String;
pub fn ensure(cfg: &Config, catalog: &HashMap<String, App>, primary: &Path,
              sha: &str, apps: &[String], steps: &Steps) -> Result<PathBuf>;
```

`prep_apps` and `run_after_worktree_create` are already `pub(crate)`; only the latter's signature changes.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-common/src/cmd.rs` `mod tests`:

```rust
#[test]
fn capture_env_reaches_the_child_without_touching_this_process() {
    let out = if cfg!(windows) {
        capture_env("cmd", &["/C", "echo %DEVKIT_TEST_ENV%"], None, &[("DEVKIT_TEST_ENV", "on")])
    } else {
        capture_env("sh", &["-c", "printf %s \"$DEVKIT_TEST_ENV\""], None, &[("DEVKIT_TEST_ENV", "on")])
    }
    .unwrap();
    assert_eq!(out.trim(), "on");
    // Setting a variable for a child must not mutate this process: `set_var` is
    // `unsafe` in edition 2024 precisely because other threads read the
    // environment concurrently, and devkit runs progress threads.
    assert!(std::env::var("DEVKIT_TEST_ENV").is_err());
}
```

In `src/bin/devkit/baseline/mod.rs` `mod tests`:

```rust
fn app_with(prep: Vec<devkit_config::PrepFile>, setup: Vec<Vec<String>>) -> devkit_ports::apps::App {
    devkit_ports::apps::App {
        name: "api".into(),
        base_port: 3000,
        path: "apps/api".into(),
        launch: vec!["run".into()],
        url: None,
        url_env: None,
        provides_url: false,
        static_env: Default::default(),
        prep_files: prep,
        setup,
    }
}

fn prep(path: &str, content: &str) -> devkit_config::PrepFile {
    devkit_config::PrepFile { path: path.into(), content: content.into(), overwrite: false }
}

/// The FNV-1a offset basis, which an app with nothing to hash must produce.
/// This pins the algorithm: a fingerprint is stored in the marker and compared
/// on a later run, possibly under a different toolchain, so a hash that shifts
/// between Rust releases would either re-prep every baseline forever or stop
/// noticing a real change.
#[test]
fn an_app_with_nothing_to_hash_is_the_fnv_offset_basis() {
    assert_eq!(fingerprint(&app_with(vec![], vec![]), &[]), "cbf29ce484222325");
}

#[test]
fn a_fingerprint_moves_when_prep_content_changes() {
    let before = fingerprint(&app_with(vec![prep(".env", "A=1")], vec![]), &[]);
    let after = fingerprint(&app_with(vec![prep(".env", "A=2")], vec![]), &[]);
    assert_ne!(before, after);
}

#[test]
fn a_fingerprint_moves_when_includes_change() {
    let app = app_with(vec![], vec![]);
    assert_ne!(fingerprint(&app, &[".env".into()]), fingerprint(&app, &[".env.local".into()]));
}

/// Without a separator between fields, `["ab", "c"]` and `["a", "bc"]` hash
/// identically and a changed setup command goes unnoticed.
#[test]
fn field_boundaries_are_part_of_the_hash() {
    let a = app_with(vec![], vec![vec!["ab".into(), "c".into()]]);
    let b = app_with(vec![], vec![vec!["a".into(), "bc".into()]]);
    assert_ne!(fingerprint(&a, &[]), fingerprint(&b, &[]));
}

#[test]
fn the_synthetic_identity_is_stable_per_sha() {
    let ctx = bootstrap_context("d13d90b724bf8a3c", &["api".to_string()]);
    assert_eq!(ctx["issue"], "baseline-d13d90b724bf");
    assert_eq!(ctx["slug"], "baseline-d13d90b724bf");
    assert_eq!(ctx["branch"], "baseline-d13d90b724bf");
    assert_eq!(ctx["role"], "baseline");
    assert_eq!(ctx["sha"], "d13d90b724bf8a3c");
}

/// A `prep_files` template naming `{{ issue }}` must render inside a baseline
/// rather than hard-failing: `template::render` is strict and `prep_apps`
/// propagates a render error with `?`.
#[test]
fn a_prep_template_naming_the_issue_renders_in_a_baseline() {
    let ctx = bootstrap_context("d13d90b724bf8a3c", &["api".to_string()]);
    let got = devkit_common::template::render(
        "ISSUE={{ issue }} ROLE={{ role }}",
        &ctx,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(got, "ISSUE=baseline-d13d90b724bf ROLE=baseline");
}
```

In `crates/devkit-config/src/lib.rs` `mod tests`:

```rust
#[test]
fn a_template_variable_colliding_with_a_context_key_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[config]\nroot = true\n[templates.variables]\nrole = 'x'\n",
    )
    .unwrap();
    let err = resolve_with_home(None, dir.path(), None, None, None, None).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("role"), "{msg}");
}

#[test]
fn an_ordinary_template_variable_is_still_accepted() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[config]\nroot = true\n[templates.variables]\nregion = 'eu'\n",
    )
    .unwrap();
    let (cfg, _) = resolve_with_home(None, dir.path(), None, None, None, None).unwrap();
    assert_eq!(cfg.templates.variables["region"], "eu");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-common capture_env; cargo nextest run --bin devkit baseline::tests; cargo nextest run -p devkit-config template_variable`
Expected: FAIL, none of `capture_env`, `fingerprint`, `bootstrap_context`, or the collision check exist.

- [ ] **Step 3: Pass an environment to hook children**

In `crates/devkit-common/src/cmd.rs`, generalize `capture`:

```rust
/// Run a command with extra environment variables, capture stdout; the error
/// includes stderr on non-zero exit. The variables reach the child only:
/// `std::env::set_var` mutates a process other threads are reading, which is
/// why edition 2024 made it `unsafe`.
pub fn capture_env(
    program: &str,
    args: &[&str],
    cwd: Option<&str>,
    env: &[(&str, &str)],
) -> Result<String> {
    let _span = crate::timing::subprocess_span(program, args).entered();
    let mut c = Command::new(program);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
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

pub fn capture(program: &str, args: &[&str], cwd: Option<&str>) -> Result<String> {
    capture_env(program, args, cwd, &[])
}
```

In `src/bin/devkit/issue/hooks.rs`, thread `env: &[(&str, &str)]` through `run_all` into `run_rendered`, which calls `capture_env` instead of `capture`. Update the three call sites: `issue/setup.rs:150` and `issue/end.rs:496,506` pass `&[]`.

- [ ] **Step 4: Reject colliding template variables**

In `crates/devkit-config/src/lib.rs`:

```rust
/// Names the render context supplies for a baseline. A `[templates.variables]`
/// entry of the same name would be shadowed silently, because
/// `template::render` merges variables underneath the context.
const RESERVED_VARIABLES: [&str; 2] = ["role", "sha"];
```

In `resolve_with_home`, after deserializing, bail on any `templates.variables` key in that list, naming the key and the layer `origin` recorded it against. Only these two are reserved: `issue`, `slug`, `branch`, `apps`, and `prefix` have always been context keys, and rejecting them now would break projects that shadow one deliberately.

- [ ] **Step 5: Implement the fingerprint and the context**

In `src/bin/devkit/baseline/mod.rs`:

```rust
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn eat(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// What an app was prepped from. A bare "this app is prepped" flag would go
/// stale: a project that adds a key to an app's env file would give issue
/// worktrees the new key and baselines the old one forever, and
/// `issue sync-includes` no longer reaches baselines.
///
/// FNV-1a rather than `DefaultHasher`, whose output is explicitly not stable
/// between Rust releases. This value is stored in the marker and compared on a
/// later run, possibly under a different toolchain.
pub fn fingerprint(app: &App, includes: &[String]) -> String {
    let mut h = FNV_OFFSET;
    for f in &app.prep_files {
        h = eat(h, f.path.as_bytes());
        h = eat(h, b"\0");
        h = eat(h, f.content.as_bytes());
        h = eat(h, b"\0");
        h = eat(h, &[u8::from(f.overwrite)]);
    }
    for cmd in &app.setup {
        for part in cmd {
            h = eat(h, part.as_bytes());
            h = eat(h, b"\0");
        }
        h = eat(h, b"\x01");
    }
    for i in includes {
        h = eat(h, i.as_bytes());
        h = eat(h, b"\0");
    }
    format!("{h:016x}")
}

/// A baseline is one shared tree, so it renders with one stable identity rather
/// than borrowing the identity of whichever worktree happened to create it.
/// Keying on the sha keeps two baselines from sharing per-issue resources, and
/// supplying `issue`/`slug`/`branch` at all is what lets a `prep_files`
/// template that names one render here: `template::render` is strict.
fn bootstrap_context(sha: &str, apps: &[String]) -> serde_json::Value {
    let id = format!("baseline-{}", short(sha));
    serde_json::json!({
        "issue": id,
        "slug": id,
        "branch": id,
        "role": "baseline",
        "sha": sha,
        "apps": apps,
    })
}
```

Add `"role": "issue"` to the context built in `issue/setup.rs` and `issue/checkout.rs`, so a hook or prep template can branch on it in either role.

- [ ] **Step 6: Implement `ensure`**

```rust
/// The baseline directory for `sha`, created if needed. Reuse preps only what
/// has drifted; a new or interrupted tree is built from scratch.
///
/// Runs before the caller resolves ports, so a long bootstrap cannot outlive a
/// reservation taken alongside it.
pub fn ensure(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    primary: &Path,
    sha: &str,
    apps: &[String],
    steps: &Steps,
) -> Result<PathBuf> {
    anyhow::ensure!(
        !cfg.defaults.baseline_dir.is_empty(),
        "`--role baseline` needs `defaults.worktree_root` or `defaults.baseline_dir`"
    );
    let root = expand_tilde(&cfg.defaults.baseline_dir);
    std::fs::create_dir_all(&root)
        .with_context(|| format!("creating {}", root.display()))?;

    // The slot is resolved twice: once to name the lock, once inside it, since
    // another process may have finished a bootstrap while this one waited.
    let name = slot_name(&slot(&root, sha));
    locks::with_slot(&root, &name, || {
        let ctx = bootstrap_context(sha, apps);
        let vars = &cfg.templates.variables;
        let includes = &cfg.defaults.worktree_include;
        let primary_s = primary.to_str().context("primary checkout path not UTF-8")?;
        let env = [(locks::REENTRY_VAR, name.as_str())];

        let (path, mut marker) = match slot(&root, sha) {
            Slot::Reuse(path, marker) => (path, marker),
            Slot::Rebuild(path) => {
                // Always `--force`: the tree may hold rendered prep files and
                // include copies that a plain remove would refuse over.
                let _ = Git::at(primary)
                    .args(["worktree", "remove", "--force", path.to_str().unwrap_or_default()])
                    .timeout(devkit_common::git::SLOW_TIMEOUT)
                    .output();
                let _ = std::fs::remove_dir_all(&path);
                create(primary_s, &path, sha, steps)?;
                (path, Marker { sha: sha.to_string(), apps: BTreeMap::new() })
            }
            Slot::Create(path) => {
                create(primary_s, &path, sha, steps)?;
                (path, Marker { sha: sha.to_string(), apps: BTreeMap::new() })
            }
        };

        with_cleanup(&path, primary_s, || {
            backfill_includes(primary_s, &path, includes, steps);
            let stale: Vec<String> = apps
                .iter()
                .filter(|a| {
                    catalog.get(*a).is_some_and(|app| {
                        marker.apps.get(*a).map(|m| m.fingerprint.as_str())
                            != Some(fingerprint(app, includes).as_str())
                    })
                })
                .cloned()
                .collect();
            if !stale.is_empty() {
                let branch = format!("baseline-{}", short(sha));
                steps.during_result("Preparing apps…", || {
                    prep_apps(&path, &branch, &stale, catalog, &ctx, vars)
                })?;
            }
            run_after_worktree_create(&path, &cfg.hooks.after_worktree_create, &ctx, vars, &env, steps);
            for a in apps {
                if let Some(app) = catalog.get(a) {
                    marker.apps.insert(
                        a.clone(),
                        AppMark { fingerprint: fingerprint(app, includes) },
                    );
                }
            }
            // Last, so an interrupted bootstrap leaves no marker and the probe
            // table classifies the tree as `Rebuild` rather than complete.
            write_marker(&path, &marker)
        })?;
        Ok(path)
    })
}

/// Detached so the baseline never occupies a branch name and never shows up in
/// a session manager's branch list.
fn create(primary_s: &str, path: &Path, sha: &str, steps: &Steps) -> Result<()> {
    // A directory removed by hand leaves a registration behind; `worktree add`
    // refuses over it until the registration is pruned.
    let _ = Git::at(Path::new(primary_s)).args(["worktree", "prune"]).output();
    steps.during_result("Creating baseline worktree…", || {
        Git::at(Path::new(primary_s))
            .args(["worktree", "add", "--detach", path.to_str().unwrap_or_default(), sha])
            .timeout(devkit_common::git::SLOW_TIMEOUT)
            .output()
    })?;
    Ok(())
}

fn slot_name(s: &Slot) -> String {
    let p = match s {
        Slot::Reuse(p, _) | Slot::Rebuild(p) | Slot::Create(p) => p,
    };
    p.file_name().unwrap_or_default().to_string_lossy().into_owned()
}
```

Make `with_cleanup` `pub(crate)` in `src/bin/devkit/issue/checkout.rs`. It fires only on `Err`, which is what `slot`'s `Rebuild` arm covers: a SIGINT leaves a markerless tree, and the next run rebuilds it in place.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-common/src/cmd.rs crates/devkit-config/src/lib.rs src/bin/devkit/
git status --short
git commit -m "feat(run): bootstrap a baseline worktree lazily"
```

---

### Task B8: Filter baselines out of worktree discovery

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs:64-69`
- Test: `crates/devkit-common/src/worktree.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task B3's `BASELINE_MARKER` and `is_baseline`, both already in this file.
- Produces: `discover` returns only non-baseline worktrees.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn discover_skips_a_worktree_carrying_a_baseline_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    let git = |cwd: &std::path::Path, args: &[&str]| {
        crate::git::Git::fixture(cwd).args(args.iter().copied()).output().unwrap()
    };
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("f"), "x").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-qm", "init"]);

    let issue = tmp.path().join("issue");
    git(&main, &["worktree", "add", "-b", "feat", issue.to_str().unwrap()]);
    let bl = tmp.path().join("bl");
    git(&main, &["worktree", "add", "--detach", bl.to_str().unwrap()]);
    std::fs::create_dir_all(bl.join(".devkit")).unwrap();
    std::fs::write(bl.join(BASELINE_MARKER), "sha = 'abc'\n").unwrap();

    let (_, others) = discover(main.to_str().unwrap()).unwrap();
    let names: Vec<_> = others.iter().map(|w| w.path.clone()).collect();
    assert_eq!(names, vec![issue], "baseline still listed: {names:?}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p devkit-common discover_skips`
Expected: FAIL, two worktrees are listed.

- [ ] **Step 3: Implement**

```rust
/// (main_repo_path, other_worktrees) from a path inside any worktree.
/// Baselines are filtered out: they are linked worktrees, so every consumer of
/// this list — `issue status`, `sync-includes --all`, `--clean-worktree` —
/// would otherwise treat one as an issue worktree.
pub fn discover(start: &str) -> Result<(PathBuf, Vec<Worktree>)> {
    let mut all = git::worktrees(Path::new(start))?.into_iter();
    let main = all.next().expect("git never lists zero worktrees");
    Ok((main.path, all.filter(|w| !is_baseline(&w.path)).collect()))
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(issue): keep baselines out of worktree discovery"
```

---

### Task B9: Reference counting and deletion

**Files:**
- Modify: `src/bin/devkit/baseline/mod.rs`, `src/bin/devkit/issue/end.rs:92-168`
- Test: `src/bin/devkit/baseline/mod.rs` (`mod tests`)

**Interfaces:**
- Consumes: Tasks B5, B6, B8.
- Produces:

```rust
pub struct References {
    /// Baseline path → the worktrees naming it.
    pub by_baseline: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Worktrees whose record exists and does not parse. Which baseline each
    /// names is unknown, so no baseline is provably unreferenced while any of
    /// these exist.
    pub unreadable: Vec<PathBuf>,
}

pub fn referencers(repo: &str) -> Result<References>;

/// Whether any live port row is held by `baseline`.
pub fn live_rows_hold(baseline: &Path, ports: &registry::Data) -> bool;

/// The ports one holder owns, for `run::bring_down_ports`.
pub fn rows_for_holder(holder: &str, ports: &registry::Data) -> Vec<u16>;

/// Write `[baseline]` into a worktree's record, preserving its other fields.
pub fn write_pin(worktree: &Path, sha: &str, path: &Path) -> Result<()>;

/// Remove `baseline` when nothing references it. Returns whether it removed it.
pub fn drop_reference(repo: &str, baseline: &Path, ports: &registry::Data, force: bool) -> Result<bool>;
```

`ports` is a parameter rather than a `registry::snapshot()` call inside, so a unit test passes `registry::Data::default()` instead of reading the developer's live registry through a daemon socket and `$XDG_STATE_HOME`.

`drop_reference` is called *after* the caller's own worktree is gone.

- [ ] **Step 1: Write the failing tests**

```rust
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: String,
    baseline: std::path::PathBuf,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
}

fn git_in(cwd: &Path, args: &[&str]) -> String {
    devkit_common::git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap()
}

/// One commit, one baseline at it, two issue worktrees whose records both name
/// that baseline.
fn two_worktrees_sharing_one_baseline() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("main");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-qm", "init"]);
    let sha = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    let baseline = tmp.path().join("_baselines").join(short(&sha));
    std::fs::create_dir_all(baseline.parent().unwrap()).unwrap();
    git_in(&repo, &["worktree", "add", "--detach", baseline.to_str().unwrap()]);
    write_marker(&baseline, &Marker { sha: sha.clone(), apps: Default::default() }).unwrap();

    let mut make = |name: &str| {
        let wt = tmp.path().join(name);
        git_in(&repo, &["worktree", "add", "-b", name, wt.to_str().unwrap()]);
        devkit_common::record::write(
            &wt,
            &devkit_common::record::IssueRecord {
                issue: name.to_string(),
                slug: name.to_string(),
                apps: vec![],
                summary: None,
                pr: None,
                baseline: Some(devkit_common::record::BaselinePin {
                    sha: sha.clone(),
                    path: baseline.to_string_lossy().into_owned(),
                }),
            },
        )
        .unwrap();
        wt
    };
    let a = make("a");
    let b = make("b");
    Fixture { _tmp: tmp, repo: repo.to_string_lossy().into_owned(), baseline, a, b }
}

fn remove_worktree(repo: &str, wt: &Path) {
    git_in(Path::new(repo), &["worktree", "remove", "--force", wt.to_str().unwrap()]);
}

fn corrupt_record(wt: &Path) {
    std::fs::write(wt.join(".devkit").join("issue.toml"), "issue = ").unwrap();
}

#[test]
fn a_corrupt_record_counts_as_a_referencer() {
    let f = two_worktrees_sharing_one_baseline();
    corrupt_record(&f.b);
    remove_worktree(&f.repo, &f.a);
    let ports = devkit_ports::registry::Data::default();
    assert!(!drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
    assert!(f.baseline.exists(), "cannot-tell must not delete");
}

#[test]
fn the_last_referencer_removes_the_baseline() {
    let f = two_worktrees_sharing_one_baseline();
    let ports = devkit_ports::registry::Data::default();
    remove_worktree(&f.repo, &f.a);
    assert!(!drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
    assert!(f.baseline.exists());
    remove_worktree(&f.repo, &f.b);
    assert!(drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
    assert!(!f.baseline.exists());
}

/// The regression test for the leak this design's ordering exists to prevent.
/// Counting references while the caller's own worktree still exists makes each
/// of two concurrent `issue end` runs see the other and decline, which leaks
/// the baseline in the common case rather than a rare one.
#[test]
fn two_concurrent_ends_leave_no_baseline_behind() {
    let f = two_worktrees_sharing_one_baseline();
    let removed: Vec<bool> = std::thread::scope(|s| {
        let handles: Vec<_> = [f.a.clone(), f.b.clone()]
            .into_iter()
            .map(|wt| {
                let repo = f.repo.clone();
                let baseline = f.baseline.clone();
                s.spawn(move || {
                    remove_worktree(&repo, &wt);
                    let ports = devkit_ports::registry::Data::default();
                    drop_reference(&repo, &baseline, &ports, false).unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(removed.iter().filter(|r| **r).count(), 1, "exactly one remover");
    assert!(!f.baseline.exists(), "baseline leaked");
}

#[test]
fn a_live_row_refuses_without_force() {
    let f = two_worktrees_sharing_one_baseline();
    remove_worktree(&f.repo, &f.a);
    remove_worktree(&f.repo, &f.b);
    let mut ports = devkit_ports::registry::Data::default();
    ports.entries.insert(
        3000,
        devkit_ports::registry::Entry {
            app: "api".into(),
            holder: f.baseline.to_string_lossy().into_owned(),
            role: devkit_ports::registry::Role::Baseline,
            pid: Some(std::process::id()),
            logfile: None,
            ts: devkit_ports::registry::now(),
        },
    );
    let err = drop_reference(&f.repo, &f.baseline, &ports, false).unwrap_err();
    assert!(format!("{err:#}").contains("running servers"), "{err:#}");
    assert!(drop_reference(&f.repo, &f.baseline, &ports, true).unwrap(), "force removes");
}

/// A baseline holds rendered prep files and include copies that git does not
/// track, so a plain `worktree remove` would refuse over them.
/// A worktree made by hand has no record. It still references the baseline it
/// runs against, so the pin creates the record rather than declining.
#[test]
fn pinning_a_worktree_with_no_record_creates_one() {
    let f = two_worktrees_sharing_one_baseline();
    let bare = f.baseline.parent().unwrap().parent().unwrap().join("c");
    git_in(Path::new(&f.repo), &["worktree", "add", "-b", "c", bare.to_str().unwrap()]);
    assert!(devkit_common::record::read(&bare).is_none(), "fixture must start recordless");

    write_pin(&bare, "d13d90b724bf8a3c", &f.baseline).unwrap();
    let rec = devkit_common::record::read(&bare).unwrap();
    assert_eq!(rec.baseline.unwrap().path, f.baseline.to_string_lossy());
    assert_eq!(rec.issue, "c", "identity falls back to the branch");

    let refs = referencers(&f.repo).unwrap();
    assert!(refs.by_baseline[&f.baseline].contains(&bare), "the new record counts");
}

#[test]
fn untracked_prep_output_does_not_block_removal() {
    let f = two_worktrees_sharing_one_baseline();
    std::fs::write(f.baseline.join(".env.local"), "A=1").unwrap();
    remove_worktree(&f.repo, &f.a);
    remove_worktree(&f.repo, &f.b);
    let ports = devkit_ports::registry::Data::default();
    assert!(drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit baseline::tests`
Expected: FAIL, `drop_reference` and `References` are undefined.

- [ ] **Step 3: Implement**

```rust
/// Which worktrees name each baseline. Derived rather than stored: a registry
/// would keep a phantom reference alive after a plain `git worktree remove`,
/// and the fix for that is this scan with a file to maintain beside it.
pub fn referencers(repo: &str) -> Result<References> {
    let (_, others) = devkit_common::worktree::discover(repo)?;
    let mut by_baseline: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut unreadable = Vec::new();
    for w in others {
        match devkit_common::record::read_state(&w.path) {
            RecordState::Ok(r) => {
                if let Some(b) = r.baseline {
                    by_baseline.entry(PathBuf::from(b.path)).or_default().push(w.path);
                }
            }
            RecordState::Unusable => unreadable.push(w.path),
            RecordState::Absent => {}
        }
    }
    Ok(References { by_baseline, unreadable })
}

pub fn live_rows_hold(baseline: &Path, ports: &registry::Data) -> bool {
    let holder = baseline.to_string_lossy();
    ports
        .entries
        .values()
        .any(|e| e.holder == holder && e.pid.is_some_and(registry::pid_alive))
}

pub fn rows_for_holder(holder: &str, ports: &registry::Data) -> Vec<u16> {
    ports
        .entries
        .iter()
        .filter(|(_, e)| e.holder == holder)
        .map(|(p, _)| *p)
        .collect()
}

/// Remove `baseline` when nothing references it any more. The caller's own
/// worktree must already be gone: counting while it still exists makes two
/// concurrent `issue end` runs each see the other and each decline.
pub fn drop_reference(
    repo: &str,
    baseline: &Path,
    ports: &registry::Data,
    force: bool,
) -> Result<bool> {
    let root = baseline.parent().context("baseline path has no parent")?;
    locks::with_dir(root, || {
        let refs = referencers(repo)?;
        remove_if_unreferenced(repo, baseline, &refs, ports, force)
    })
}

/// The body of [`drop_reference`], without the directory lock. A sweep already
/// holds that lock and calls this directly: `flock` blocks on a second open of
/// the same file even within one process, so a locked function must never call
/// another locked function.
fn remove_if_unreferenced(
    repo: &str,
    baseline: &Path,
    refs: &References,
    ports: &registry::Data,
    force: bool,
) -> Result<bool> {
    if !refs.unreadable.is_empty() {
        return Ok(false);
    }
    if refs.by_baseline.get(baseline).is_some_and(|v| !v.is_empty()) {
        return Ok(false);
    }
    let root = baseline.parent().context("baseline path has no parent")?;
    let name = baseline
        .file_name()
        .context("baseline path has no final component")?
        .to_string_lossy()
        .into_owned();
    locks::with_slot(root, &name, || {
        // A live server in the tree is the one thing worth refusing for.
        if !force && live_rows_hold(baseline, ports) {
            anyhow::bail!(
                "{} still has running servers; stop them or pass --force",
                baseline.display()
            );
        }
        // Always `--force`: a baseline holds include copies and rendered prep
        // files, and any untracked file would otherwise refuse the removal.
        Git::at(Path::new(repo))
            .args(["worktree", "remove", "--force", baseline.to_str().unwrap_or_default()])
            .timeout(devkit_common::git::SLOW_TIMEOUT)
            .output()?;
        Ok(true)
    })
}

/// Point a worktree's record at a baseline, leaving its other fields alone.
///
/// Writes a record when there is none. A worktree made by hand rather than by
/// `issue setup` still holds a reference, and skipping the write there would
/// let prune reclaim a baseline that worktree is serving from.
pub fn write_pin(worktree: &Path, sha: &str, path: &Path) -> Result<()> {
    let mut rec = match devkit_common::record::read(worktree) {
        Some(rec) => rec,
        None => {
            let branch = devkit_common::git::branch(worktree)?;
            devkit_common::record::IssueRecord {
                issue: branch.clone(),
                slug: branch,
                apps: vec![],
                summary: None,
                pr: None,
                baseline: None,
            }
        }
    };
    rec.baseline = Some(devkit_common::record::BaselinePin {
        sha: sha.to_string(),
        path: path.to_string_lossy().into_owned(),
    });
    devkit_common::record::write(worktree, &rec)
}
```

In `end.rs::cleanup`, read the record's baseline pin alongside the summary path, and call `drop_reference` after the `git worktree remove` of the issue worktree succeeds. A failure there warns rather than aborting: `issue end` deletion is best-effort and `devrun baseline prune` is the guarantee.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/baseline/ src/bin/devkit/issue/end.rs
git commit -m "feat(issue): reclaim a baseline when its last referencer ends"
```

---

### Task B10: Wire `devrun up`, retire `ensure_fresh`, repin

**Files:**
- Delete: `src/bin/devkit/run/baseline.rs`
- Modify: `src/bin/devkit/run/mod.rs:640-665` and its `mod baseline;` line
- Test: `tests/baseline_up.rs` (new)

**Interfaces:**
- Consumes: Tasks B1, B7, B9.
- Produces: `cmd_up`'s `Role::Baseline` arm yields `(Role::Baseline, <baseline path>, <baseline path>)`.

- [ ] **Step 1: Write the failing tests**

`tests/baseline_up.rs`. These drive the real binary, so they point `HOME` and `XDG_STATE_HOME` at a tempdir the way `tests/brief_main_checkout.rs` does, keeping the developer's registry and config out of it.

```rust
use std::path::Path;
use std::process::Command;

fn git(cwd: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .status()
        .unwrap();
    assert!(st.success(), "git {args:?}");
}

fn devkit(cwd: &Path, state: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(args)
        .current_dir(cwd)
        .env("XDG_STATE_HOME", state)
        .env("HOME", state)
        .output()
        .unwrap()
}

/// Two worktrees cut from the same commit resolve to one baseline directory.
#[test]
fn two_worktrees_at_one_fork_point_share_a_baseline() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("apps").join("api")).unwrap();
    // An app is required: `cmd_up` bails with "no apps to run" before it builds
    // any group, so a project with none never reaches the baseline path.
    std::fs::write(
        repo.join("devkit.toml"),
        "[config]\nroot = true\n\
         [defaults]\nbaseline_ref = 'main'\n\
         [apps.api]\nbase_port = 4000\npath = 'apps/api'\nlaunch = ['echo', 'x']\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);

    for name in ["a", "b"] {
        let wt = tmp.path().join("proj_worktrees").join(name);
        git(&repo, &["worktree", "add", "-b", name, wt.to_str().unwrap()]);
        devkit(&wt, &state, &["run", "up", "--role", "baseline", "--dry-run", "api"]);
    }

    let baselines = tmp.path().join("proj_worktrees").join("_baselines");
    let entries: Vec<_> = std::fs::read_dir(&baselines)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|n| n != ".locks")
        .collect();
    assert_eq!(entries.len(), 1, "one fork point, one baseline: {entries:?}");
}

/// The `path` under `[baseline]` in a worktree's record.
fn baseline_of(wt: &Path) -> String {
    let body = std::fs::read_to_string(wt.join(".devkit").join("issue.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&body).unwrap();
    doc["baseline"]["path"].as_str().unwrap().to_string()
}

/// Every holder the port registry currently has a row for.
fn holders(state: &Path) -> Vec<String> {
    let Ok(body) = std::fs::read_to_string(state.join("devkit").join("ports.json")) else {
        return Vec::new();
    };
    let doc: serde_json::Value = serde_json::from_str(&body).unwrap();
    doc["entries"]
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|e| e["holder"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A rebase moves the merge base, so `up` repins. The old baseline's rows have
/// a holder no worktree names any more, so they come down first.
#[test]
fn repinning_stops_the_abandoned_baselines_servers() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("apps").join("api")).unwrap();
    std::fs::write(
        repo.join("devkit.toml"),
        "[config]\nroot = true\n\
         [defaults]\nbaseline_ref = 'main'\n\
         [apps.api]\nbase_port = 4000\npath = 'apps/api'\nlaunch = ['echo', 'x']\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    // `--dry-run` skips the spawn, not the group build: the baseline is still
    // created and pinned, which is all this asserts. Nothing is launched, so
    // `launch` never has to be a real program on any platform.
    devkit(&wt, &state, &["run", "up", "--role", "baseline", "--dry-run", "api"]);
    let first = baseline_of(&wt);

    // A pid-less reservation is what `ports alloc` writes before anything
    // binds, so the repin's teardown releases the row instead of signalling a
    // process — no child to spawn, and nothing to poll for.
    devkit(&wt, &state, &["ports", "alloc", "--holder", &first, "--role", "baseline", "api"]);
    assert!(holders(&state).contains(&first), "reservation not seeded");

    git(&repo, &["commit", "-qm", "second", "--allow-empty"]);
    git(&wt, &["rebase", "-q", "main"]);
    devkit(&wt, &state, &["run", "up", "--role", "baseline", "--dry-run", "api"]);

    let second = baseline_of(&wt);
    assert_ne!(first, second, "the record still names the old baseline");
    assert!(
        !holders(&state).contains(&first),
        "rows under the abandoned baseline survived the repin"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --test baseline_up`
Expected: FAIL. The first because `_baselines` is never created; the second because there is no repin.

- [ ] **Step 3: Implement**

`cmd_up` already calls `run::ensure_provider(catalog, &mut apps)` before the group-building block, so `&apps` here is the final list.

```rust
    let primary = devkit_common::git::primary_checkout(Path::new(cwd))?;
    let groups: Vec<(Role, String, PathBuf)> = {
        let mut g = Vec::new();
        for r in role.roles() {
            match r {
                Role::Issue => {
                    g.push((Role::Issue, issue_holder.clone(), PathBuf::from(&issue_holder)));
                }
                Role::Baseline => {
                    let wt = Path::new(&issue_holder);
                    let sha = crate::baseline::pin(wt, &baseline_target)?;
                    let previous = devkit_common::record::read(wt).and_then(|r| r.baseline);
                    let path =
                        crate::baseline::ensure(cfg, catalog, &primary, &sha, &apps, &steps)?;
                    // A rebase repoints this worktree at a different baseline.
                    // Its old baseline's servers stay alive under a holder no
                    // worktree names any more: unreachable without a terminal,
                    // and enough to block prune forever.
                    if let Some(prev) = previous.filter(|p| Path::new(&p.path) != path) {
                        let ports = registry::snapshot()?;
                        let abandoned = crate::baseline::rows_for_holder(&prev.path, &ports);
                        if !abandoned.is_empty() {
                            run::bring_down_ports(&abandoned)?;
                        }
                    }
                    crate::baseline::write_pin(wt, &sha, &path)?;
                    g.push((Role::Baseline, path.to_string_lossy().into_owned(), path));
                }
            }
        }
        g
    };
```

`baseline_target` is the `baseline::target(cfg, Path::new(cwd))?` value Task A3 already resolves once at the top of `cmd_up` for the diff and the "no apps" message.

Delete `src/bin/devkit/run/baseline.rs` and its `mod baseline;` line in `run/mod.rs`. `crate::baseline` is a different module and stays.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git rm src/bin/devkit/run/baseline.rs
git add src/bin/devkit/run/mod.rs tests/baseline_up.rs
git commit -m "feat(run): create baselines lazily instead of resetting one"
```

---

### Task B11: `down` narrowing

**Files:**
- Modify: `src/bin/devkit/run/mod.rs` (`build_selector:324`, `touches_foreign:745`, `cmd_down:792`, and the existing test at `:1093`)
- Test: `src/bin/devkit/run/mod.rs` (`mod tests`)

**Interfaces:**
- Consumes: Task B9's `referencers`.
- Produces:

```rust
/// The baseline this worktree is the only referencer of, if any. `None` when
/// the worktree names no baseline, when another worktree names the same one, or
/// when any record is unreadable.
fn sole_referenced_baseline(repo: &str, current: &str) -> Option<PathBuf>;

fn build_selector(a: &DownArgs, current: &str, own_baseline: Option<&Path>) -> registry::DownSelector;
fn touches_foreign(matched: &[(u16, &registry::Entry)], current: &str, own_baseline: Option<&Path>) -> bool;
```

`cmd_down` computes `sole_referenced_baseline` once and passes it to both. The existing test `build_selector_maps_scope_and_filter` gains a `None` third argument; its `Scope::Current` assertions are unchanged, which is the point — a worktree with no baseline behaves exactly as before.

- [ ] **Step 1: Write the failing tests**

```rust
fn entry(holder: &str, role: registry::Role) -> registry::Entry {
    registry::Entry {
        app: "api".into(),
        holder: holder.into(),
        role,
        pid: Some(1),
        logfile: None,
        ts: 0,
    }
}

#[test]
fn a_sole_referenced_baseline_is_not_foreign() {
    let bl = std::path::PathBuf::from("/b/d13d90b724bf");
    let e = entry("/b/d13d90b724bf", registry::Role::Baseline);
    let matched = vec![(3000u16, &e)];
    assert!(touches_foreign(&matched, "/wt/cur", None), "no baseline: foreign");
    assert!(
        !touches_foreign(&matched, "/wt/cur", Some(&bl)),
        "sole referencer stops its own baseline without a terminal"
    );
}

#[test]
fn another_worktrees_baseline_stays_foreign() {
    let mine = std::path::PathBuf::from("/b/d13d90b724bf");
    let e = entry("/b/0123456789ab", registry::Role::Baseline);
    let matched = vec![(3000u16, &e)];
    assert!(touches_foreign(&matched, "/wt/cur", Some(&mine)));
}

#[test]
fn the_default_scope_covers_the_worktree_and_its_own_baseline() {
    let bl = std::path::PathBuf::from("/b/d13d90b724bf");
    let a = DownArgs::default();
    let s = build_selector(&a, "/wt/cur", Some(&bl));
    match s.scope {
        registry::Scope::Holders(hs) => {
            assert!(hs.iter().any(|h| h == "/wt/cur"));
            assert!(hs.iter().any(|h| h == "/b/d13d90b724bf"));
        }
        other => panic!("expected Holders, got {other:?}"),
    }
}

#[test]
fn no_baseline_leaves_the_default_scope_current() {
    let a = DownArgs::default();
    let s = build_selector(&a, "/wt/cur", None);
    assert!(matches!(s.scope, registry::Scope::Current(ref h) if h == "/wt/cur"));
}
```

A shared baseline is covered by `sole_referenced_baseline` returning `None`, which Task B9's `referencers` tests already pin.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --bin devkit run::tests`
Expected: FAIL, both functions take two arguments.

- [ ] **Step 3: Implement**

```rust
fn sole_referenced_baseline(repo: &str, current: &str) -> Option<PathBuf> {
    let refs = crate::baseline::referencers(repo).ok()?;
    if !refs.unreadable.is_empty() {
        return None;
    }
    let mine = devkit_common::record::read(Path::new(current))?.baseline?;
    let path = PathBuf::from(mine.path);
    let holders = refs.by_baseline.get(&path)?;
    (holders.len() == 1).then_some(path)
}
```

In `build_selector`, when no explicit scope flag is given and `own_baseline` is `Some`, produce `Scope::Holders(vec![current, baseline])` instead of `Scope::Current(current)`. `Scope::Holders` already exists, so the registry needs no change. In `touches_foreign`, treat an entry whose holder equals `own_baseline` as not foreign.

In `cmd_down`, resolve it once before building the selector:

```rust
    let repo = devkit_common::git::primary_checkout(Path::new(cwd))?;
    let own_baseline = sole_referenced_baseline(&repo.to_string_lossy(), &current);
    let selector = build_selector(args, &current, own_baseline.as_deref());
```

The check and the kill both happen inside `locks::with_slot` for that baseline, since `up` writes its record under the same lock: without it, another worktree's `up` can claim these servers between the check and the kill and be told its baseline is ready just before it dies.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/run/mod.rs
git commit -m "feat(run): let a sole referencer stop its own baseline"
```

---

### Task B12: `devrun baseline list` and `prune`

**Files:**
- Modify: `Cargo.toml` (the root package's `[dependencies]`), `src/bin/devkit/run/mod.rs` (`Cmd`, dispatch), `src/bin/devkit/baseline/mod.rs`
- Test: `tests/baseline_cmd.rs` (new)

`jwalk` is declared in `[workspace.dependencies]` but is not a dependency of the
root `devkit` package, so `dir_size` needs `jwalk.workspace = true` added to the
root `Cargo.toml`'s `[dependencies]` before it will compile.

**Interfaces:**
- Consumes: Tasks B3, B9.
- Produces:

```rust
// clap
Cmd::Baseline { cmd: BaselineCmd }
enum BaselineCmd { List, Prune { dry_run: bool, force: bool } }

// crate::baseline
pub struct Listed { pub path: PathBuf, pub sha: Option<String>, pub referencers: Vec<PathBuf>, pub bytes: u64 }
pub fn list(baseline_dir: &Path, repo: &str) -> Result<Vec<Listed>>;
pub struct Pruned { pub removed: Vec<PathBuf>, pub reported: Vec<PathBuf> }
pub fn prune_all(baseline_dir: &Path, repo: &str, ports: &registry::Data,
                 dry_run: bool, force: bool) -> Result<Pruned>;
```

- [ ] **Step 1: Write the failing tests**

`tests/baseline_cmd.rs`, reusing the `git` and `devkit` helpers from `tests/baseline_up.rs`.

```rust
struct Fx {
    _tmp: tempfile::TempDir,
    state: std::path::PathBuf,
    repo: std::path::PathBuf,
    wt: std::path::PathBuf,
    baselines: std::path::PathBuf,
    baseline: std::path::PathBuf,
    stray: std::path::PathBuf,
}

/// One worktree that has pinned a baseline, plus a hand-made directory under
/// `baseline_dir` that devkit did not create and cannot claim.
fn fixture() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    let repo = tmp.path().join("proj");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("apps").join("api")).unwrap();
    std::fs::write(
        repo.join("devkit.toml"),
        "[config]\nroot = true\n\
         [defaults]\nbaseline_ref = 'main'\n\
         [apps.api]\nbase_port = 4000\npath = 'apps/api'\nlaunch = ['echo', 'x']\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);

    let wt = tmp.path().join("proj_worktrees").join("a");
    git(&repo, &["worktree", "add", "-b", "a", wt.to_str().unwrap()]);
    devkit(&wt, &state, &["run", "up", "--role", "baseline", "--dry-run", "api"]);

    let baselines = tmp.path().join("proj_worktrees").join("_baselines");
    let baseline = std::fs::read_dir(&baselines)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.join(".devkit").join("baseline.toml").exists())
        .expect("up created a baseline");
    let stray = baselines.join("notabaseline");
    std::fs::create_dir_all(&stray).unwrap();

    Fx { _tmp: tmp, state, repo, wt, baselines, baseline, stray }
}

/// An unreferenced baseline goes; a directory with no marker is reported, not
/// deleted, because devkit cannot prove it created it.
#[test]
fn prune_removes_an_unreferenced_baseline_and_reports_a_markerless_directory() {
    let f = fixture();
    git(&f.repo, &["worktree", "remove", "--force", f.wt.to_str().unwrap()]);

    let out = devkit(&f.repo, &f.state, &["run", "baseline", "prune"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!f.baseline.exists(), "unreferenced baseline survived:\n{stdout}");
    assert!(f.stray.exists(), "a markerless directory must never be deleted");
    assert!(stdout.contains("notabaseline"), "prune must name what it left alone:\n{stdout}");
}

#[test]
fn dry_run_removes_nothing_and_still_reports() {
    let f = fixture();
    git(&f.repo, &["worktree", "remove", "--force", f.wt.to_str().unwrap()]);

    let out = devkit(&f.repo, &f.state, &["run", "baseline", "prune", "--dry-run"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(f.baseline.exists(), "dry run deleted a baseline:\n{stdout}");
    assert!(f.stray.exists());
    let name = f.baseline.file_name().unwrap().to_string_lossy();
    assert!(stdout.contains(&*name), "dry run must name what it would remove:\n{stdout}");
}

/// `list` enumerates with `read_dir`, so a tree git has no registration for is
/// visible. `git worktree list` would not show it at all, which is exactly the
/// state an operator needs to see.
#[test]
fn list_shows_a_baseline_git_no_longer_knows_about() {
    let f = fixture();
    let orphan = f.baselines.join("000000000000");
    std::fs::create_dir_all(orphan.join(".devkit")).unwrap();
    std::fs::write(orphan.join(".devkit").join("baseline.toml"), "sha = 'abc'\n").unwrap();

    let out = devkit(&f.repo, &f.state, &["run", "baseline", "list"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("000000000000"), "unregistered baseline not listed:\n{stdout}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run --test baseline_cmd`
Expected: FAIL, the subcommand does not exist.

- [ ] **Step 3: Implement**

```rust
/// Enumerate via `read_dir` rather than `git worktree list`: a directory git no
/// longer knows about is exactly what an operator needs to see, and the git
/// list would render it invisible.
pub fn list(baseline_dir: &Path, repo: &str) -> Result<Vec<Listed>> {
    let refs = referencers(repo)?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(baseline_dir)? {
        let path = entry?.path();
        if !path.is_dir() || path.file_name().is_some_and(|n| n == ".locks") {
            continue;
        }
        let sha = match read_marker(&path) {
            MarkerState::Ok(m) => Some(m.sha),
            MarkerState::Unusable | MarkerState::Absent => None,
        };
        out.push(Listed {
            referencers: refs.by_baseline.get(&path).cloned().unwrap_or_default(),
            bytes: dir_size(&path),
            path,
            sha,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Remove every unreferenced baseline in one pass under a single directory
/// lock. Calls `remove_if_unreferenced` directly rather than `drop_reference`,
/// which takes that same lock: two open file descriptions on one lock file
/// block each other even inside one process.
pub fn prune_all(
    baseline_dir: &Path,
    repo: &str,
    ports: &registry::Data,
    dry_run: bool,
    force: bool,
) -> Result<Pruned> {
    locks::with_dir(baseline_dir, || {
        let refs = referencers(repo)?;
        let mut removed = Vec::new();
        let mut reported = Vec::new();
        for entry in std::fs::read_dir(baseline_dir)? {
            let path = entry?.path();
            if !path.is_dir() || path.file_name().is_some_and(|n| n == ".locks") {
                continue;
            }
            // No marker means devkit cannot prove it created this tree, so it
            // is named and left alone rather than deleted.
            if matches!(read_marker(&path), MarkerState::Absent) {
                reported.push(path);
                continue;
            }
            if dry_run {
                if !refs.by_baseline.contains_key(&path) && refs.unreadable.is_empty() {
                    removed.push(path);
                }
                continue;
            }
            match remove_if_unreferenced(repo, &path, &refs, ports, force) {
                Ok(true) => removed.push(path),
                Ok(false) => {}
                // One stuck baseline must not abandon the sweep.
                Err(e) => eprintln!("warning: {}: {e:#}", path.display()),
            }
        }
        Ok(Pruned { removed, reported })
    })
}

/// Total bytes under `path`. Walked in parallel because a baseline holds a full
/// dependency tree; `jwalk_parallelism` is evaluated here, on the thread that
/// builds the walk, since inside `pool::install` it would see itself as nested
/// and silently go serial.
fn dir_size(path: &Path) -> u64 {
    let parallelism = devkit_common::pool::jwalk_parallelism();
    jwalk::WalkDir::new(path)
        .parallelism(parallelism)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}
```

Add `Cmd::Baseline` to `run/mod.rs`'s clap enum and dispatch it. `list` renders through `devkit_common::ui` like the other tables. `prune` prints what it removed and, separately, what it left alone.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/bin/devkit/run/mod.rs src/bin/devkit/baseline/ tests/baseline_cmd.rs
git commit -m "feat(run): add devrun baseline list and prune"
```

---

### Task B13: Doctor row and stray attribution

**Files:**
- Modify: `src/bin/devkit/doctor.rs:334-344`, `crates/devkit-ports/src/strays/mod.rs:227-270`
- Test: `src/bin/devkit/doctor.rs` (`mod tests`), `crates/devkit-ports/src/strays/mod.rs` (`mod tests`)

**Interfaces:**
- Consumes: Tasks B2, B9.
- Produces: a `baseline_orphans` doctor row; longest-root-first matching in `attribute_holder`.

This task swaps `managed_roots` from `baseline_path` to `baseline_dir`. `baseline_path` stays on `Defaults` until Task B14.

- [ ] **Step 1: Write the failing tests**

In `crates/devkit-ports/src/strays/mod.rs` `mod tests`. `attribute_holder` is `#[cfg(unix)]`, so these carry the same attribute:

```rust
/// `baseline_dir` defaults to `<worktree_root>/_baselines`, so `worktree_root`
/// is a prefix of it. First-match over the root list would attribute a stray
/// inside a baseline to `<worktree_root>/_baselines` — the container, not the
/// baseline — and `devrun down` would then address a holder no row uses.
#[test]
fn a_stray_inside_a_baseline_is_attributed_to_that_baseline() {
    let roots = vec!["/w".to_string(), "/w/_baselines".to_string()];
    let got = attribute_holder("/w/_baselines/d13d90b724bf/apps/api", &[], &roots);
    assert_eq!(got.as_deref(), Some("/w/_baselines/d13d90b724bf"));
}

#[test]
fn a_stray_in_a_plain_worktree_still_attributes_to_it() {
    let roots = vec!["/w".to_string(), "/w/_baselines".to_string()];
    let got = attribute_holder("/w/feat-x/apps/api", &[], &roots);
    assert_eq!(got.as_deref(), Some("/w/feat-x"));
}

#[test]
fn managed_roots_names_the_baseline_directory() {
    let mut cfg = Config::default();
    cfg.defaults.worktree_root = "/w".into();
    cfg.defaults.baseline_dir = "/w/_baselines".into();
    assert!(managed_roots(&cfg).iter().any(|r| r == "/w/_baselines"));
}
```

In `src/bin/devkit/doctor.rs` `mod tests`:

```rust
#[test]
fn no_orphans_is_an_ok_row() {
    assert!(matches!(baseline_orphan_check(0, 0), Check::Ok(_)));
}

#[test]
fn orphans_warn_and_name_the_prune_command() {
    let Check::Warn(msg) = baseline_orphan_check(2, 1_500_000_000) else {
        panic!("expected a warning");
    };
    assert!(msg.contains("devrun baseline prune"), "{msg}");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-ports strays::; cargo nextest run --bin devkit doctor::tests::orphans`
Expected: FAIL, `baseline_dir` is not a managed root and `baseline_orphan_check` is undefined.

- [ ] **Step 3: Implement**

In `managed_roots`, replace the `baseline_path` block with the same shape over `cfg.defaults.baseline_dir`.

In `attribute_holder`, match the longest root first. The `known` holders branch above already does this with `max_by_key(|h| h.len())`; the `roots` loop is first-match and needs the same treatment:

```rust
    let mut by_depth: Vec<&String> = roots.iter().collect();
    by_depth.sort_by_key(|r| std::cmp::Reverse(r.len()));
    for r in by_depth {
        // …the existing body, unchanged…
    }
```

In `doctor.rs`, add a row shaped like `devrun_strays`: count the baselines `baseline::list` reports with no referencers, sum their bytes, and warn naming `devrun baseline prune`. Read-only — the row never removes anything. Factor the verdict into `baseline_orphan_check(count: usize, bytes: u64) -> Check` so it is testable without a filesystem.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bin/devkit/doctor.rs crates/devkit-ports/src/strays/mod.rs
git commit -m "feat(doctor): report unreferenced baselines"
```

---

### Task B14: Remove `baseline_path`

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (`Defaults`, `resolve_defaults`, `resolve_with_home`, and its `mod tests`), `src/bin/devkit/schema.rs:105`, `tests/schema_init.rs:43`, `crates/devkit-ports/src/apps.rs:86`, `src/bin/devkit/run/config.rs:164`, `src/bin/devkit/issue/tracker.rs`, `crates/devkit-config/tests/repo_relative_anchor.rs`, `crates/devkit-mcp/tests/issue_status_tracker.rs`, and every fixture string under `tests/`
- Test: `crates/devkit-config/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: Tasks B10 and B13, which retired the last two readers of the field.
- Produces: `baseline_path` no longer exists on `Defaults`. A project layer that sets it is a hard error; the home layer warns and ignores it.

The asymmetry is deliberate. A home config is read from every directory on the machine, including repositories that are not devkit projects, so erroring there would reproduce the exact failure stage A removed.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn baseline_path_in_a_project_layer_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[config]\nroot = true\n[defaults]\nbaseline_path = '/old'\n",
    )
    .unwrap();
    let err = resolve_with_home(None, dir.path(), None, None, None, None).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("baseline_dir"), "names the replacement: {msg}");
    assert!(msg.contains("devkit.toml"), "names the layer: {msg}");
}

#[test]
fn baseline_path_in_the_home_layer_warns_and_is_ignored() {
    let home = tempfile::tempdir().unwrap();
    let home_cfg = home.path().join("config.toml");
    std::fs::write(&home_cfg, "[defaults]\nbaseline_path = '/old'\n").unwrap();
    let proj = tempfile::tempdir().unwrap();
    let root = proj.path().join("w");
    std::fs::write(
        proj.path().join("devkit.toml"),
        format!(
            "[config]\nroot = true\n[defaults]\nworktree_root = '{}'\n",
            root.display().to_string().replace('\\', "\\\\")
        ),
    )
    .unwrap();
    let (cfg, _) =
        resolve_with_home(None, proj.path(), None, None, Some(&home_cfg), None).unwrap();
    assert_eq!(
        Path::new(&cfg.defaults.worktree_root),
        normalize_lexically(&root)
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit-config baseline_path`
Expected: FAIL. `baseline_path` is still accepted everywhere, so the first test resolves cleanly.

- [ ] **Step 3: Implement**

Delete `baseline_path` from `Defaults`, from `impl Default for Defaults`, and from `resolve_defaults`'s path-key list.

In `resolve_with_home`, before deserializing the merged table, act on the leaf's recorded origin:

```rust
    if let Some(from) = origin.get("defaults.baseline_path") {
        if Some(from.as_path()) == home_config {
            eprintln!(
                "warning: `defaults.baseline_path` in {} is ignored; \
                 baselines now live under `defaults.baseline_dir`",
                from.display()
            );
        } else {
            anyhow::bail!(
                "`defaults.baseline_path` in {} is no longer a checkout path. \
                 Set `defaults.baseline_dir` to the directory baselines are \
                 created under, and remove the old checkout with \
                 `git worktree remove --force <path>`",
                from.display()
            );
        }
    }
```

`home_config` is the `Option<&Path>` parameter `resolve_with_home` already takes. Then remove the key from the merged table so the warn path still deserializes.

- [ ] **Step 4: Delete every remaining mention and run the gate**

Run `rg -l "baseline_path"` and clear each hit outside `docs/superpowers/`, whose archived plans and specs describe past states and stay as they are:

- `src/bin/devkit/schema.rs:105` — the commented starter line becomes `baseline_dir`.
- `tests/schema_init.rs:43` — the asserted key becomes `baseline_dir`.
- `crates/devkit-ports/src/apps.rs:86` — the `SAMPLE` fixture.
- `crates/devkit-config/src/lib.rs` — fixture strings *and* the assertions at 2164, 2211, 2229, 2287, which read the field directly.
- `src/bin/devkit/run/config.rs:164`, `src/bin/devkit/issue/tracker.rs`, `crates/devkit-config/tests/repo_relative_anchor.rs`, `crates/devkit-mcp/tests/issue_status_tracker.rs`, and the fixture strings under `tests/`.
- `docs/configuration.md` and `schema/devkit-config.json` — Task B15 handles both.

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates src tests
git status --short
git commit -m "feat(config)!: replace baseline_path with baseline_dir

BREAKING CHANGE: defaults.baseline_path is removed. Set
defaults.baseline_dir to the directory baselines are created under, and
remove the old baseline checkout with git worktree remove --force. A
baseline_path in a project config is now an error; one in the personal
config warns and is ignored."
```

---

### Task B15: Docs, schema, and the release commit

**Files:**
- Modify: `docs/configuration.md`, `docs/commands.md:30-59`, `AGENTS.md`, `schema/devkit-config.json`

**Interfaces:**
- Consumes: every prior stage B task.
- Produces: nothing.

- [ ] **Step 1: Update the docs**

`docs/configuration.md`: replace the `baseline_path` row with `baseline_dir`, note that it defaults to `_baselines` under `worktree_root`, document the reserved template-variable names (`role` and `sha`), and describe the marker file and what its absence means.

`docs/commands.md`: correct the `devrun down --role baseline` line, which describes the old single-checkout behavior, and document `devrun baseline list` and `devrun baseline prune`.

`AGENTS.md`: add the baseline invariants — deletion counts references *after* the caller's worktree is gone; both baseline locks wait without a bound, and the directory lock always precedes a slot lock; a markerless directory is rebuilt in place rather than skipped.

- [ ] **Step 2: Regenerate the schema**

```bash
DEVKIT_UPDATE_SCHEMA=1 cargo test schema
```

- [ ] **Step 3: Run the full gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check && cargo test --workspace --doc`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add docs AGENTS.md schema/devkit-config.json
git commit -m "feat(run)!: give each fork point its own baseline worktree

Baselines are content-addressed by merge base, shared between worktrees
at the same fork point, created on demand with the full worktree
bootstrap, and reclaimed when no worktree references them.

BREAKING CHANGE: defaults.baseline_path is removed. Set
defaults.baseline_dir to the directory baselines are created under, and
remove the old baseline checkout with git worktree remove --force."
```
