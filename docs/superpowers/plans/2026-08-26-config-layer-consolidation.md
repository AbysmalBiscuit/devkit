# Config Layer Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a linked worktree resolve its repository's config without carrying a copy, by routing every git question through one module and teaching config discovery about the main checkout.

**Architecture:** One `devkit_common::git` module becomes the only place in the workspace that spawns git, with a sanitized environment and a timeout. `devkit-config` gains `project_layers`, the single answer to which project config files apply, and receives the main checkout as a parameter so it stays a leaf crate with no git knowledge. The lock harness and the docs manifest both call `project_layers` instead of walking on their own.

**Tech Stack:** Rust edition 2024, `anyhow` for errors, `toml` for config, `tempfile` for test scratch, `schemars` for the published JSON Schema. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-26-config-layer-consolidation-design.md`

## Global Constraints

- Every task ends green on `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all`.
- No new crate dependencies. Git is already a required dependency; it is shelled out to, never replaced with a library.
- `devkit-config` has no internal dependencies and gains none. It never calls git.
- Test scratch comes from `tempfile::tempdir()`. Never build a scratch path from `std::env::temp_dir()`. Bind the `TempDir` for as long as its path is used.
- Comments are timeless: no `this PR`, no `previously`, no issue or task references. See `AGENTS.md`.
- Tests that spawn processes poll for the expected state rather than sleeping a fixed interval — CI runs on ubuntu, macos, and windows.
- Commits follow Conventional Commits and end with `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `crates/devkit-common/src/git.rs` | **New.** The only place in the workspace that spawns git. Owns the sanitized environment, the timeout, and the named questions (`checkout_root`, `main_checkout`, `worktrees`, `branch`). |
| `crates/devkit-common/src/cmd.rs` | Loses `git`. Keeps `capture` and the `gh` helpers, whose environments must stay intact. |
| `crates/devkit-common/src/worktree.rs` | Loses `discover`'s git call and its `parse_porcelain`, both of which move to `git.rs`. Keeps the include-copying and issue-id logic. |
| `crates/devkit-config/src/layers.rs` | **New.** `Layer`, `LayerKind`, `project_layers`. The single definition of which project config files apply, in what order, with what cutoff and dedupe. |
| `crates/devkit-config/src/lib.rs` | `discover` expressed in terms of `project_layers`; `resolve` gains the main-checkout parameter; per-key path anchoring. |
| `crates/devkit-locks/src/hook.rs` | `harness_enabled` over the layer list; `enforcement_enabled` takes the payload CWD; global opt-in stays outside the stack. |
| `crates/devkit-locks/src/lib.rs` | `find_root_from` delegates to `git::checkout_root` with a declared fallback. |
| `crates/devkit-docs/src/manifest.rs` | `discover` over `project_layers`; `project_devkit_toml` restricted away from inherited layers. |

---

## Task 1: The git module — spawning

**Files:**
- Create: `crates/devkit-common/src/git.rs`
- Modify: `crates/devkit-common/src/lib.rs:1-22` (add `pub mod git;`)
- Test: `crates/devkit-common/src/git.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::timing::subprocess_span` (existing, `timing.rs`).
- Produces: `pub fn run(args: &[&str], cwd: &Path) -> Result<String>` — `git -C <cwd> <args…>` with `GIT_DIR`, `GIT_COMMON_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` removed from the environment and a 10s timeout. Every later task builds on this.

- [ ] **Step 1: Write the failing test**

Add to `crates/devkit-common/src/git.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// An ambient `GIT_DIR` redirects git to another repository. Every call in
    /// the workspace goes through `run`, so stripping it here is what stops a
    /// stranger's config being read as this repository's.
    #[test]
    fn run_strips_ambient_git_dir() {
        let repo = tempfile::tempdir().unwrap();
        run(&["init", "-q", "-b", "main"], repo.path()).unwrap();

        let decoy = tempfile::tempdir().unwrap();
        run(&["init", "-q", "-b", "main"], decoy.path()).unwrap();

        // SAFETY: single-threaded test; the var is removed before returning.
        unsafe { std::env::set_var("GIT_DIR", decoy.path().join(".git")) };
        let out = run(&["rev-parse", "--show-toplevel"], repo.path());
        unsafe { std::env::remove_var("GIT_DIR") };

        let toplevel = std::fs::canonicalize(out.unwrap().trim()).unwrap();
        assert_eq!(toplevel, std::fs::canonicalize(repo.path()).unwrap());
    }

    #[test]
    fn run_reports_stderr_on_failure() {
        let repo = tempfile::tempdir().unwrap();
        let err = run(&["rev-parse", "--show-toplevel"], repo.path()).unwrap_err();
        assert!(err.to_string().contains("not a git repository"), "{err}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-common git::tests -- --test-threads=1`
Expected: FAIL — the module does not exist (`unresolved import`).

- [ ] **Step 3: Write minimal implementation**

Create `crates/devkit-common/src/git.rs`:

```rust
//! The single door to git. Every git invocation in the workspace goes through
//! here so that two properties hold everywhere rather than nowhere: the
//! environment cannot redirect the call to another repository, and a git that
//! stops responding cannot block its caller forever.

use anyhow::{Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Backstop for a git that never returns. Long enough that no healthy call
/// reaches it, short enough that a wedged one fails instead of hanging a write
/// through the PreToolUse hook.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Variables that repoint git at a different repository. Left in place, any of
/// them silently changes which `devkit.toml` devkit reads — and that file
/// carries `[apps] launch` and `[tasks] run`, which devkit executes.
const REDIRECTING_VARS: [&str; 4] = [
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
];

/// `git -C <cwd> <args…>`, capturing stdout. The escape hatch for operations
/// this module has no named function for.
pub fn run(args: &[&str], cwd: &Path) -> Result<String> {
    let _span = crate::timing::subprocess_span("git", args).entered();

    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    for var in REDIRECTING_VARS {
        command.env_remove(var);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn `git`: {e}"))?;

    // Poll rather than block: `wait_with_output` has no timeout, and this runs
    // on the write path. The 1ms step keeps a healthy call's overhead under the
    // spawn cost it already pays. Output is drained only after exit, which is
    // safe for the volumes git produces here.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("`git {}` did not finish within {TIMEOUT:?}", args.join(" "));
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "`git {}` failed ({}):\n{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
```

Add to `crates/devkit-common/src/lib.rs`, keeping the list alphabetical:

```rust
pub mod git;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-common git::tests -- --test-threads=1`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/git.rs crates/devkit-common/src/lib.rs
git commit -m "feat(common): add the single git module

Every git call in the workspace routes through one function so the
environment cannot repoint it at another repository and a wedged git
cannot block its caller. GIT_DIR, GIT_COMMON_DIR, GIT_WORK_TREE, and
GIT_INDEX_FILE are stripped; cmd::capture cannot strip them because gh
and doppler need their environments intact.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 2: The git module — the named questions

**Files:**
- Modify: `crates/devkit-common/src/git.rs` (append)
- Modify: `crates/devkit-common/src/worktree.rs:1-33` (move `Worktree` and `parse_porcelain` out)
- Test: `crates/devkit-common/src/git.rs` (inline tests)

**Interfaces:**
- Consumes: `run` (Task 1).
- Produces:
  - `pub struct Worktree { pub path: PathBuf, pub branch: String, pub bare: bool }`
  - `pub fn parse_porcelain(out: &str) -> Vec<Worktree>`
  - `pub fn checkout_root(start: &Path) -> Result<PathBuf>`
  - `pub fn main_checkout(start: &Path) -> Result<Option<PathBuf>>`
  - `pub fn worktrees(start: &Path) -> Result<Vec<Worktree>>`
  - `pub fn branch(start: &Path) -> Result<String>`

  `Worktree` gains `bare` over the `worktree.rs` version; every construction site must set it.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `crates/devkit-common/src/git.rs`:

```rust
    /// Builds a repo with one commit; returns the guard so the caller keeps the
    /// directory alive for as long as it uses the path.
    fn repo_with_commit() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(&["init", "-q", "-b", "main"], dir.path()).unwrap();
        run(&["config", "user.email", "t@example.com"], dir.path()).unwrap();
        run(&["config", "user.name", "Test"], dir.path()).unwrap();
        std::fs::write(dir.path().join("f.txt"), "x").unwrap();
        run(&["add", "."], dir.path()).unwrap();
        run(&["commit", "-qm", "init"], dir.path()).unwrap();
        dir
    }

    #[test]
    fn main_checkout_is_none_in_the_main_checkout() {
        let repo = repo_with_commit();
        assert_eq!(main_checkout(repo.path()).unwrap(), None);
    }

    #[test]
    fn linked_worktree_resolves_its_main_checkout() {
        let repo = repo_with_commit();
        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("wt");
        run(
            &["worktree", "add", "-q", linked.to_str().unwrap(), "-b", "side"],
            repo.path(),
        )
        .unwrap();

        let found = main_checkout(&linked).unwrap().expect("a main checkout");
        assert_eq!(
            std::fs::canonicalize(found).unwrap(),
            std::fs::canonicalize(repo.path()).unwrap()
        );
    }

    /// A bare repository has no main working tree, so there is no checkout to
    /// inherit config from.
    #[test]
    fn bare_main_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("b.git");
        run(&["init", "-q", "--bare", bare.to_str().unwrap()], dir.path()).unwrap();
        assert_eq!(main_checkout(&bare).unwrap(), None);
    }

    #[test]
    fn checkout_root_errors_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert!(checkout_root(dir.path()).is_err());
    }

    #[test]
    fn parse_porcelain_marks_a_bare_first_entry() {
        let parsed = parse_porcelain("worktree /x/b.git\nbare\n");
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].bare);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-common git::tests -- --test-threads=1`
Expected: FAIL — `cannot find function main_checkout`, `checkout_root`, `parse_porcelain` in this scope.

- [ ] **Step 3: Write minimal implementation**

Append to `crates/devkit-common/src/git.rs`:

```rust
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// `DETACHED` when the worktree has no branch checked out.
    pub branch: String,
    /// A bare repository has no working tree, so it holds no config.
    pub bare: bool,
}

/// Parse `git worktree list --porcelain`. Git lists the main worktree first,
/// which is what `main_checkout` relies on.
pub fn parse_porcelain(out: &str) -> Vec<Worktree> {
    let mut all = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut bare = false;

    fn flush(
        p: &mut Option<String>,
        b: &mut Option<String>,
        bare: &mut bool,
        v: &mut Vec<Worktree>,
    ) {
        if let Some(pp) = p.take() {
            v.push(Worktree {
                path: PathBuf::from(pp),
                branch: b.take().unwrap_or_else(|| "DETACHED".into()),
                bare: std::mem::take(bare),
            });
        }
    }

    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut path, &mut branch, &mut bare, &mut all);
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = Some(b.to_string());
        } else if line.trim() == "bare" {
            bare = true;
        }
    }
    flush(&mut path, &mut branch, &mut bare, &mut all);
    all
}

/// The checkout containing `start`. Errors when `start` is not in a
/// repository; a caller wanting a fallback declares one.
pub fn checkout_root(start: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(
        run(&["rev-parse", "--show-toplevel"], start)?.trim(),
    ))
}

/// Every worktree of `start`'s repository, main first.
pub fn worktrees(start: &Path) -> Result<Vec<Worktree>> {
    Ok(parse_porcelain(&run(
        &["worktree", "list", "--porcelain"],
        start,
    )?))
}

/// `start`'s repository's main checkout, or `None` when `start` is already in
/// it and when the main worktree is bare. Git names the main worktree itself,
/// so no path is derived from the git directory's location: the parent of the
/// common directory cannot tell a real main worktree from a bare repository at
/// `/x/.git` or a `--separate-git-dir=/x/.git` clone.
pub fn main_checkout(start: &Path) -> Result<Option<PathBuf>> {
    let all = worktrees(start)?;
    let Some(main) = all.first() else {
        return Ok(None);
    };
    if main.bare {
        return Ok(None);
    }
    let here = checkout_root(start)?;
    Ok((!same_path(&main.path, &here)).then(|| main.path.clone()))
}

/// The branch checked out at `start`.
pub fn branch(start: &Path) -> Result<String> {
    Ok(run(&["rev-parse", "--abbrev-ref", "HEAD"], start)?
        .trim()
        .to_string())
}

/// Compare two paths by identity where the filesystem can answer, falling back
/// to a lexical comparison when either does not exist.
fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}
```

In `crates/devkit-common/src/worktree.rs`, delete the `Worktree` struct and `parse_porcelain` (lines 5-33) and re-export from the new home so existing importers keep working. Replace the file's first line with:

```rust
use crate::git;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub use crate::git::{Worktree, parse_porcelain};
```

and rewrite `discover`:

```rust
/// (main_repo_path, other_worktrees) from a path inside any worktree.
pub fn discover(start: &str) -> Result<(PathBuf, Vec<Worktree>)> {
    let mut all = git::worktrees(Path::new(start))?;
    anyhow::ensure!(!all.is_empty(), "not inside a git repo: {start}");
    let main = all.remove(0);
    Ok((main.path, all))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p devkit-common -- --test-threads=1`
Expected: PASS. The `worktree.rs` tests that construct `Worktree` need the new `bare` field; add `bare: false` to each literal the compiler names.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src/git.rs crates/devkit-common/src/worktree.rs
git commit -m "feat(common): answer checkout questions through the git module

main_checkout takes the first entry of \`worktree list --porcelain\`,
which git documents as the main worktree. Deriving it as the parent of
the common directory cannot distinguish a real main worktree from a bare
repository at /x/.git or a --separate-git-dir clone whose working tree is
elsewhere; both would contribute executable config from a directory that
is not a checkout of the repository.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 3: Migrate `devkit-common`'s own callers

**Files:**
- Modify: `crates/devkit-common/src/gitfetch.rs:14,98`
- Modify: `crates/devkit-common/src/github.rs:258`
- Modify: `crates/devkit-common/src/gitignore.rs:44`
- Modify: `crates/devkit-common/src/tracker/mod.rs:288-289,390-391`
- Test: existing tests in those modules

**Interfaces:**
- Consumes: `git::run` (Task 1).
- Produces: nothing new. `cmd::git` loses its `devkit-common`-internal callers.

- [ ] **Step 1: Find every internal caller**

Run: `rg -n 'cmd::git|crate::cmd::git|use crate::cmd::git' crates/devkit-common/src/`
Expected: matches in `gitfetch.rs`, `github.rs`, `tracker/mod.rs`.

- [ ] **Step 2: Rewrite each call**

`cmd::git(args, cwd_str)` becomes `git::run(args, Path::new(cwd_str))`. The
argument order is unchanged; only the `cwd` type differs, from `&str` to
`&Path`. In `gitfetch.rs` replace `use crate::cmd::git;` with `use crate::git;`
and each `git(&[...], cwd)` with `git::run(&[...], Path::new(cwd))`.

`gitignore.rs:44` calls `capture("git", &["config", "--global", …], None)` with
no working directory. It becomes:

```rust
let configured = crate::git::run(&["config", "--global", "core.excludesfile"], Path::new("."))
```

- [ ] **Step 3: Run the crate's tests**

Run: `cargo test -p devkit-common -- --test-threads=1`
Expected: PASS.

- [ ] **Step 4: Confirm no internal callers remain**

Run: `rg -n 'cmd::git' crates/devkit-common/src/`
Expected: no matches outside `cmd.rs`'s own definition.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-common/src
git commit -m "refactor(common): route internal git calls through the module

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 4: `find_root_from` and the fake-repo fixtures

**Files:**
- Modify: `crates/devkit-locks/src/lib.rs:38-50` (`find_root_from`), `:366`, `:403`, `:451`
- Modify: `crates/devkit-mcp/src/locks.rs:236`
- Modify: `tests/locks.rs:10`, `tests/mcp.rs:9`, `tests/lock_harness_race.rs:25`
- Test: `crates/devkit-locks/src/lib.rs:400-430` (existing root-resolution tests)

**Interfaces:**
- Consumes: `devkit_common::git::checkout_root` (Task 2).
- Produces: `find_root_from` keeps its `(start: &Path) -> PathBuf` signature and its no-repository fallback, now declared rather than incidental. Every existing caller (`locks/lib.rs:54`, `:101`, `:284`, `lockm.rs:165`) is source-compatible.

- [ ] **Step 1: Write the failing test**

Replace the fixture line in `crates/devkit-locks/src/lib.rs:403` (the test asserting resolution from a subdirectory) so the fixture is a real repository, and add a test pinning the fallback:

```rust
    /// A directory named `.git` is not a repository. The fixture is a real one
    /// so this asserts root resolution rather than the presence of a filename.
    fn init_repo(at: &Path) {
        devkit_common::git::run(&["init", "-q", "-b", "main"], at).unwrap();
    }

    #[test]
    fn root_resolves_from_a_subdirectory() {
        let root = tempfile::tempdir().unwrap();
        init_repo(root.path());
        let deep = root.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            std::fs::canonicalize(find_root_from(&deep)).unwrap(),
            std::fs::canonicalize(root.path()).unwrap()
        );
    }

    /// Outside a repository, lock scoping falls back to the start directory.
    /// Declared, because `lockm` is usable outside a repository and its locks
    /// must still be scoped to somewhere.
    #[test]
    fn root_falls_back_to_start_outside_a_repository() {
        let start = tempfile::tempdir().unwrap();
        assert_eq!(find_root_from(start.path()), start.path());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-locks root_resolves_from_a_subdirectory -- --test-threads=1`
Expected: FAIL — the current implementation finds no `.git` and returns `deep`.

- [ ] **Step 3: Write minimal implementation**

Replace `find_root_from` in `crates/devkit-locks/src/lib.rs`:

```rust
/// The checkout containing `start`, or `start` itself when it is not in a
/// repository. The fallback is deliberate: `lockm` is usable outside a
/// repository, and its locks still need a scope. Asking git rather than
/// looking for a directory named `.git` is what keeps that answer honest — the
/// filename is not the repository.
pub fn find_root_from(start: &Path) -> PathBuf {
    devkit_common::git::checkout_root(start).unwrap_or_else(|_| start.to_path_buf())
}
```

Then convert every fake-repo fixture. In each of `crates/devkit-locks/src/lib.rs:366`, `:451`, `crates/devkit-mcp/src/locks.rs:236`, `tests/locks.rs:10`, `tests/mcp.rs:9`, `tests/lock_harness_race.rs:25`, replace:

```rust
std::fs::create_dir_all(p.path().join(".git")).unwrap();
```

with:

```rust
devkit_common::git::run(&["init", "-q", "-b", "main"], p.path()).unwrap();
```

matching each site's local variable name for the directory.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks -p devkit-mcp -- --test-threads=1 && cargo test --test locks --test mcp --test lock_harness_race`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks crates/devkit-mcp tests
git commit -m "refactor(locks): resolve the checkout root through git

A directory named .git is not a repository, and when none was found the
walk returned its own argument, so a caller outside a repository got a
checkout root that was not one. The fallback stays for lockm's sake but
is now declared. Seven fixtures that faked a repository with an empty
.git directory become real repositories.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 5: Migrate the remaining callers and close the door

**Files:**
- Modify: `src/bin/devkit/brief.rs:371,502`; `src/bin/devrun/main.rs:238`; `src/bin/devrun/baseline.rs:35`; `src/bin/issue/info.rs:46,254`; `src/bin/issue/end.rs:80,121,131`; `src/bin/issue/setup.rs:380`; `src/bin/issue/checkout.rs:395`; `src/bin/issue/review/request.rs:260,280,305`; `src/bin/issue/review/finish.rs:186,189`; `src/bin/portm.rs:59`; `crates/devkit-issue/src/status.rs:2,261`; `crates/devkit-docs/src/cache.rs:328,380`; `crates/devkit-docs/src/lib.rs:279`; `crates/devkit-docs/src/upgrade.rs:480,485,515`
- Modify: `crates/devkit-common/src/cmd.rs` (delete `git`)
- Test: `tests/no_stray_git.rs` (create)

**Interfaces:**
- Consumes: everything from Task 2.
- Produces: `cmd::git` no longer exists. `git::checkout_root` replaces every `rev-parse --show-toplevel`; `git::main_checkout` replaces `end.rs`'s two `--git-common-dir` calls; `git::branch` replaces every `rev-parse --abbrev-ref HEAD`.

- [ ] **Step 1: Write the failing guard test**

Create `tests/no_stray_git.rs`:

```rust
//! One door to git. A second one reintroduces the environment redirect and the
//! unbounded wait that the module exists to prevent, so this is enforced rather
//! than documented.

use std::path::Path;

#[test]
fn git_is_only_spawned_by_the_git_module() {
    let offenders = scan(&[
        "Command::new(\"git\")",
        "cmd::git(",
        "capture(\"git\"",
    ]);
    assert!(
        offenders.is_empty(),
        "git must be spawned only by devkit_common::git; found:\n{}",
        offenders.join("\n")
    );
}

/// An empty directory is not a repository. A fixture that fakes one asserts a
/// filename rather than a behavior, and no longer resolves once the root comes
/// from git.
#[test]
fn no_fixture_fakes_a_repository() {
    let offenders = scan(&["create_dir_all(&repo.join(\".git\"))", ".join(\".git\")"]);
    let offenders: Vec<_> = offenders
        .into_iter()
        .filter(|line| line.contains("create_dir_all"))
        .collect();
    assert!(
        offenders.is_empty(),
        "build fixtures with `git init`, not an empty .git directory:\n{}",
        offenders.join("\n")
    );
}

fn scan(needles: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    walk(Path::new(env!("CARGO_MANIFEST_DIR")), &mut |path, body| {
        if path.ends_with("crates/devkit-common/src/git.rs") || path.ends_with("tests/no_stray_git.rs") {
            return;
        }
        for (n, line) in body.lines().enumerate() {
            if needles.iter().any(|needle| line.contains(needle)) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    });
    found
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "target" || name == ".git" || name == "docs" {
            continue;
        }
        if path.is_dir() {
            walk(&path, f);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(body) = std::fs::read_to_string(&path)
        {
            f(&path, &body);
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test no_stray_git`
Expected: FAIL, listing every unmigrated call site.

- [ ] **Step 3: Migrate each site the test names**

Mechanical, three shapes:

```rust
// was: let root = devkit_common::cmd::git(&["rev-parse", "--show-toplevel"], cwd_str)?.trim().to_string();
let root = devkit_common::git::checkout_root(Path::new(cwd_str))?;

// was: let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], top)?.trim().to_string();
let branch = devkit_common::git::branch(Path::new(top))?;

// was (end.rs:80 and :121):
//   let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"], wt)?;
//   let main = Path::new(common.trim()).parent()…
let main = devkit_common::git::main_checkout(Path::new(wt))?;
```

`end.rs` wants the main checkout even when called from it, where
`main_checkout` returns `None`. Resolve that case explicitly:

```rust
let main = devkit_common::git::main_checkout(Path::new(wt))?
    .map(Ok)
    .unwrap_or_else(|| devkit_common::git::checkout_root(Path::new(wt)))?;
```

`docs/upgrade.rs:480` (`path.join(".git").is_file()`) asks whether a checkout is
a linked worktree. Replace with a `main_checkout(path)?.is_some()` call.

Everything else becomes `git::run(args, Path::new(cwd))`.

Finally delete `pub fn git` from `crates/devkit-common/src/cmd.rs:26-31`.

- [ ] **Step 4: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, including `no_stray_git`.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: route every git call through the git module

Closes the GIT_DIR redirect in issue end, which resolved the main
checkout from an environment-sensitive rev-parse. A guard test keeps the
door single.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 6: `project_layers`

**Files:**
- Create: `crates/devkit-config/src/layers.rs`
- Modify: `crates/devkit-config/src/lib.rs:706-757` (`discover`)
- Test: `crates/devkit-config/src/layers.rs` (inline tests)

**Interfaces:**
- Consumes: nothing outside `devkit-config`.
- Produces:
  - `pub enum LayerKind { Ancestor, Checkout, MainCheckout }`
  - `pub struct Layer { pub path: PathBuf, pub kind: LayerKind }`
  - `pub fn project_layers(start: &Path, main_checkout: Option<&Path>) -> Result<Vec<Layer>>` — lowest precedence first, excluding the home config and any explicit override.

  Task 7 and Task 8 both call this. Task 9 supplies a non-`None` `main_checkout`.

- [ ] **Step 1: Write the failing test**

Create `crates/devkit-config/src/layers.rs` with tests only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn write(at: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(at).unwrap();
        let p = at.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn local_outranks_tracked_in_one_directory() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "devkit.toml", "");
        write(dir.path(), "devkit.local.toml", "");
        let layers = project_layers(dir.path(), None).unwrap();
        let names: Vec<_> = layers
            .iter()
            .map(|l| l.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["devkit.toml", "devkit.local.toml"]);
    }

    #[test]
    fn deeper_directories_outrank_shallower() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b");
        write(dir.path(), "devkit.toml", "");
        write(&deep, "devkit.toml", "");
        let layers = project_layers(&deep, None).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].path.parent().unwrap(), deep);
    }

    /// The marker is a positional barrier: it drops everything lower in
    /// precedence and leaves everything nearer `start` alone.
    #[test]
    fn root_marker_drops_only_lower_precedence_layers() {
        let dir = tempfile::tempdir().unwrap();
        let mid = dir.path().join("mid");
        let deep = mid.join("deep");
        write(dir.path(), "devkit.toml", "");
        write(&mid, "devkit.toml", "[config]\nroot = true\n");
        write(&deep, "devkit.toml", "");
        let layers = project_layers(&deep, None).unwrap();
        assert_eq!(layers.len(), 2, "the outermost layer is cut off");
        assert_eq!(layers[0].path.parent().unwrap(), mid);
        assert_eq!(layers[1].path.parent().unwrap(), deep);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-config layers::tests`
Expected: FAIL — `project_layers` is not defined.

- [ ] **Step 3: Write minimal implementation**

Prepend to `crates/devkit-config/src/layers.rs`:

```rust
//! Which project config files apply where. The one definition of the file set,
//! its order, its cutoff, and its dedupe — shared by the full config resolver,
//! the lock harness, and the docs manifest, each of which composes its own
//! global inputs on top.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Tracked, committed config: the project's own settings.
pub const CONFIG_FILE: &str = "devkit.toml";
/// Untracked overrides beside it, for what one machine or checkout needs and
/// the repository should not carry.
pub const LOCAL_CONFIG_FILE: &str = "devkit.local.toml";

/// Where a layer came from. Callers that mutate config need this: an inherited
/// layer is read-only, and writing to one edits another checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    /// Found walking up from `start`, above the checkout root.
    Ancestor,
    /// The current checkout's own file, or one nested below its root.
    Checkout,
    /// Inherited from this repository's main checkout. Never a mutation
    /// target, never a project identity.
    MainCheckout,
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub path: PathBuf,
    pub kind: LayerKind,
}

/// Project config layers applying at `start`, lowest precedence first. Excludes
/// the home config and any `--config` / `$DEVKIT_CONFIG` override: those differ
/// per reader, so each composes its own.
pub fn project_layers(start: &Path, main_checkout: Option<&Path>) -> Result<Vec<Layer>> {
    let mut collected: Vec<Layer> = Vec::new();

    if let Some(main) = main_checkout {
        collected.extend(files_in(main, LayerKind::MainCheckout));
    }

    // Walk outward from `start`, then reverse: the walk is natural deepest-first
    // and the stack is lowest-precedence-first.
    let mut upward: Vec<Layer> = Vec::new();
    for dir in start.ancestors() {
        upward.extend(files_in(dir, LayerKind::Checkout).into_iter().rev());
    }
    upward.reverse();
    collected.splice(0..0, upward.into_iter().filter(|_| false)); // placeholder, replaced below

    Ok(collected)
}

/// The config files present in one directory, tracked first so the untracked
/// one outranks it.
fn files_in(dir: &Path, kind: LayerKind) -> Vec<Layer> {
    [CONFIG_FILE, LOCAL_CONFIG_FILE]
        .into_iter()
        .map(|name| dir.join(name))
        .filter(|p| p.is_file())
        .map(|path| Layer { path, kind })
        .collect()
}

/// Whether a layer file declares `[config] root = true`.
fn declares_root(path: &Path) -> Result<bool> {
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("reading config layer {}", path.display()))?;
    let table: toml::Table = toml::from_str(&body)
        .with_context(|| format!("parsing config layer {}", path.display()))?;
    Ok(table
        .get("config")
        .and_then(|c| c.as_table())
        .and_then(|c| c.get("root"))
        .and_then(|r| r.as_bool())
        .unwrap_or(false))
}
```

Then replace the body of `project_layers` with the real assembly — ordering,
then dedupe, then cutoff, in that sequence, because a barrier must be evaluated
at one position and duplicates would otherwise give it two:

```rust
pub fn project_layers(start: &Path, main_checkout: Option<&Path>) -> Result<Vec<Layer>> {
    let root = start
        .ancestors()
        .find(|d| {
            d.join(CONFIG_FILE).is_file() || d.join(LOCAL_CONFIG_FILE).is_file()
        })
        .unwrap_or(start);

    let mut ordered: Vec<Layer> = Vec::new();

    // Ancestors, outermost first, above the checkout root.
    let mut ancestors: Vec<&Path> = start
        .ancestors()
        .skip_while(|d| *d != root)
        .skip(1)
        .collect();
    ancestors.reverse();
    for dir in ancestors {
        ordered.extend(files_in(dir, LayerKind::Ancestor));
    }

    // The main checkout sits above every ancestor and below this checkout.
    if let Some(main) = main_checkout {
        ordered.extend(files_in(main, LayerKind::MainCheckout));
    }

    // The checkout root, then anything nested between it and `start`.
    let mut inner: Vec<&Path> = start.ancestors().take_while(|d| *d != root).collect();
    inner.push(root);
    inner.reverse();
    for dir in inner {
        ordered.extend(files_in(dir, LayerKind::Checkout));
    }

    dedupe(&mut ordered);
    apply_cutoff(&mut ordered)?;
    Ok(ordered)
}

/// Keep the highest-precedence occurrence of each file. The canonical path is
/// the key only — `Layer.path` keeps its original spelling and its kind,
/// because a `Checkout` layer symlinked to the main checkout's file must not
/// become a writable handle on the main checkout.
fn dedupe(layers: &mut Vec<Layer>) {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut keep = vec![true; layers.len()];
    for i in (0..layers.len()).rev() {
        let key = std::fs::canonicalize(&layers[i].path)
            .unwrap_or_else(|_| layers[i].path.clone());
        if seen.contains(&key) {
            keep[i] = false;
        } else {
            seen.push(key);
        }
    }
    let mut iter = keep.into_iter();
    layers.retain(|_| iter.next().unwrap_or(true));
}

/// `[config] root = true` drops every layer lower in precedence than the one
/// declaring it. Matching the outward walk it replaces: stop there, keep
/// everything nearer `start`. Applied uniformly, so a marker means the same
/// thing read from the main checkout as from a worktree of it.
fn apply_cutoff(layers: &mut Vec<Layer>) -> Result<()> {
    let mut barrier = None;
    for (i, layer) in layers.iter().enumerate() {
        if declares_root(&layer.path)? {
            barrier = Some(i);
        }
    }
    if let Some(i) = barrier {
        layers.drain(..i);
    }
    Ok(())
}
```

Register the module in `crates/devkit-config/src/lib.rs`, near the top:

```rust
mod layers;
pub use layers::{Layer, LayerKind, project_layers};
```

Then express `discover` in terms of it, replacing the walk at `lib.rs:718-740`:

```rust
    let layers = project_layers(start, main_checkout)?;
    let rooted = layers
        .first()
        .is_some_and(|l| declares_root_public(&l.path).unwrap_or(false));

    let mut out: Vec<(PathBuf, toml::Table)> = Vec::new();
    if !rooted
        && let Some(h) = home
        && h.is_file()
    {
        out.push(read_layer(h)?);
    }
    for layer in &layers {
        out.push(read_layer(&layer.path)?);
    }
    if out.is_empty() {
        return Err(anyhow::Error::new(NoConfig));
    }
    Ok(out)
```

Expose `declares_root` from `layers.rs` as `pub(crate) fn declares_root_public`
so `discover` can ask whether the surviving lowest layer is a barrier, which is
what drops the home config.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-config`
Expected: PASS, including the pre-existing `resolve_with_home` tests, which
must not change behavior — `main_checkout` is `None` everywhere until Task 9.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-config/src
git commit -m "feat(config): add project_layers as the one layer discovery

The file set, its order, its cutoff, and its dedupe get one definition.
Globals and explicit overrides stay out: the three readers deliberately
differ there, and a single flat list could not preserve all three.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 7: The harness over the layer list

**Files:**
- Modify: `crates/devkit-locks/Cargo.toml` (add `devkit-config.workspace = true`)
- Modify: `crates/devkit-locks/src/hook.rs:143-200`
- Modify: `src/bin/lockm.rs:159-168`
- Test: `crates/devkit-locks/src/hook.rs` (inline), `tests/lock_harness_race.rs`

**Interfaces:**
- Consumes: `devkit_config::project_layers` (Task 6), `devkit_common::git` (Task 2).
- Produces: `pub fn enforcement_enabled(cwd: &Path) -> bool` — takes the payload CWD, not a pre-resolved root. `harness_enabled` becomes private, replaced by a layer-list walk.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-locks/src/hook.rs`:

```rust
    /// A harness declaration below the checkout root must be seen. The caller
    /// used to collapse the CWD to a root first, which hid it.
    #[test]
    fn harness_declared_in_a_nested_directory_is_honored() {
        let repo = tempfile::tempdir().unwrap();
        devkit_common::git::run(&["init", "-q", "-b", "main"], repo.path()).unwrap();
        std::fs::write(repo.path().join("devkit.toml"), "").unwrap();
        let nested = repo.path().join("packages/thing");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("devkit.local.toml"),
            "[harness]\nenforce_writes = true\n",
        )
        .unwrap();

        assert!(enforcement_enabled(&nested));
        assert!(!enforcement_enabled(repo.path()));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-locks harness_declared_in_a_nested -- --test-threads=1`
Expected: FAIL — `enforcement_enabled` takes a root and reads only `<root>/devkit.toml`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/devkit-locks/Cargo.toml` under `[dependencies]`:

```toml
devkit-config.workspace = true
```

Replace `harness_enabled` and `enforcement_enabled` in `crates/devkit-locks/src/hook.rs`:

```rust
/// True iff any project layer applying at `cwd` sets `[harness] enforce_writes`.
fn harness_enabled(cwd: &Path) -> bool {
    let Ok(layers) = devkit_config::project_layers(cwd, None) else {
        return false;
    };
    layers.iter().any(|layer| {
        std::fs::read_to_string(&layer.path)
            .map(|b| harness_flag_in(&b))
            .unwrap_or(false)
    })
}

/// Whether write enforcement is active for a write originating at `cwd`.
///
/// Takes the working directory rather than a checkout root: a declaration in a
/// directory between the root and the write is part of the answer, and
/// collapsing to the root first discards it. The env override is read before
/// any filesystem work, so an explicit on/off costs nothing.
pub fn enforcement_enabled(cwd: &Path) -> bool {
    if let Some(v) = parse_env_override(std::env::var("DEVKIT_ENFORCE_WRITES").ok().as_deref()) {
        return v;
    }
    // The global opt-in sits outside the layer stack and outside the `[config]
    // root` cutoff on purpose: a repository must not be able to switch off
    // machine-wide write enforcement by declaring itself a root.
    harness_enabled(cwd) || global_harness_enabled()
}
```

`resolve_enforcement` keeps its existing unit tests; leave it in place as the
tested statement of precedence and call it from `enforcement_enabled` if you
prefer to keep the three inputs explicit.

In `src/bin/lockm.rs`, stop pre-resolving the root:

```rust
    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    if !hook::enforcement_enabled(&cwd) {
        return; // no opt-in (env, project layers, or global config) → no enforcement
    }
```

Everything after this point that needs a root keeps calling
`devkit_locks::find_root_from(&cwd)`, which Task 4 already routed through git.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-locks -- --test-threads=1 && cargo test --test lock_harness_race`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks src/bin/lockm.rs
git commit -m "fix(locks): resolve the harness over the full layer stack

The harness read <root>/devkit.toml and nothing else, so it ignored
devkit.local.toml and every directory between the root and the write.
Taking the working directory rather than a pre-resolved root is what
makes a nested declaration visible; the env override now short-circuits
before any filesystem work. The global opt-in stays outside the stack so
a repository cannot switch off machine-wide enforcement.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 8: The docs manifest over the layer list

**Files:**
- Modify: `crates/devkit-docs/Cargo.toml` (add `devkit-config.workspace = true`)
- Modify: `crates/devkit-docs/src/manifest.rs:152-185`
- Test: `crates/devkit-docs/tests/manifest.rs` or the existing manifest tests

**Interfaces:**
- Consumes: `devkit_config::{project_layers, LayerKind}` (Task 6).
- Produces: `Discovered::project_devkit_toml` is now `Option<PathBuf>` restricted to a `Checkout` or `Ancestor` layer. `discover(start, global)` keeps its signature.

- [ ] **Step 1: Write the failing test**

```rust
    /// `[docs]` in an untracked local file is part of the manifest. The docs
    /// walk read only devkit.toml, which surprised anyone who had learned how
    /// [apps] resolves.
    #[test]
    fn local_config_contributes_docs_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("devkit.toml"), "").unwrap();
        std::fs::write(
            dir.path().join("devkit.local.toml"),
            "[docs.libs.zod]\nrepo = \"https://example.invalid/zod\"\n",
        )
        .unwrap();
        let found = discover(dir.path(), Some(Path::new("/nonexistent"))).unwrap();
        assert!(found.manifest.libs.contains_key("zod"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-docs local_config_contributes_docs_entries`
Expected: FAIL — the manifest walk reads only `devkit.toml`.

- [ ] **Step 3: Write minimal implementation**

Add to `crates/devkit-docs/Cargo.toml`:

```toml
devkit-config.workspace = true
```

Replace the hand-rolled walk in `crates/devkit-docs/src/manifest.rs` (the
`let mut dir = Some(start); while let Some(d) = dir { … }` block) with:

```rust
    let layers = devkit_config::project_layers(start, None)?;
    let mut nearest: Option<PathBuf> = None;
    for layer in &layers {
        // An inherited layer belongs to another checkout. `--project` writes
        // here, so pointing it at one would edit the main checkout from a
        // worktree.
        if layer.kind != devkit_config::LayerKind::MainCheckout {
            nearest = Some(layer.path.clone());
        }
        if let Some(mut docs) = docs_layer(&layer.path)? {
            stamp(&mut docs, &layer.path);
            manifest = merge(manifest, docs);
        }
    }
```

`project_layers` returns lowest precedence first, so the loop applies layers in
the order the merge already expects and `nearest` ends on the highest-precedence
writable layer. Delete the now-unused `layers` vector and its reversed
application.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p devkit-docs`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-docs
git commit -m "fix(docs): resolve the manifest over the shared layer stack

[docs] now honors devkit.local.toml and the [config] root cutoff, like
every other table. The --project write target is restricted away from
inherited layers, which is what stops docm add --project in a worktree
editing the main checkout.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 9: The main-checkout layer

**Files:**
- Modify: `crates/devkit-config/src/layers.rs` (tests)
- Modify: `crates/devkit-config/src/lib.rs` (`resolve`, `resolve_with_home`, `health`)
- Modify: `crates/devkit-ports/src/load.rs:17`
- Modify: `src/bin/devkit/brief.rs:465`
- Test: `crates/devkit-config/tests/main_checkout_layer.rs` (create)

**Interfaces:**
- Consumes: `project_layers` (Task 6), `devkit_common::git::main_checkout` (Task 2).
- Produces: `pub fn resolve(explicit: Option<&Path>, start: &Path, main_checkout: Option<&Path>) -> Result<(Config, Provenance)>`. Both production callers pass `devkit_common::git::main_checkout(start).ok().flatten().as_deref()`.

- [ ] **Step 1: Write the failing test**

Create `crates/devkit-config/tests/main_checkout_layer.rs`:

```rust
//! The main checkout's config reaches a linked worktree, and the worktree can
//! still override it.

use std::path::Path;

#[test]
fn main_checkout_layer_sits_below_the_checkout() {
    let main = tempfile::tempdir().unwrap();
    std::fs::write(
        main.path().join("devkit.toml"),
        "[apps.web]\nlaunch = [\"main\"]\n",
    )
    .unwrap();

    let worktree = tempfile::tempdir().unwrap();

    let layers = devkit_config::project_layers(worktree.path(), Some(main.path())).unwrap();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].kind, devkit_config::LayerKind::MainCheckout);

    std::fs::write(
        worktree.path().join("devkit.toml"),
        "[apps.web]\nlaunch = [\"mine\"]\n",
    )
    .unwrap();
    let layers = devkit_config::project_layers(worktree.path(), Some(main.path())).unwrap();
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].kind, devkit_config::LayerKind::MainCheckout);
    assert_eq!(layers[1].kind, devkit_config::LayerKind::Checkout);
}

/// A worktree living beneath its own main checkout finds those files twice.
/// They contribute once, at the position their highest-precedence occurrence
/// gives them.
#[test]
fn a_nested_worktree_does_not_duplicate_the_main_layer() {
    let main = tempfile::tempdir().unwrap();
    std::fs::write(main.path().join("devkit.toml"), "").unwrap();
    let nested = main.path().join("worktrees/side");
    std::fs::create_dir_all(&nested).unwrap();

    let layers = devkit_config::project_layers(&nested, Some(main.path())).unwrap();
    assert_eq!(layers.len(), 1, "contributed once, not twice");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-config --test main_checkout_layer`
Expected: FAIL — dedupe or ordering is wrong, or `LayerKind` is not public.

- [ ] **Step 3: Write minimal implementation**

`project_layers` already accepts `main_checkout` from Task 6; make the two tests
pass by confirming ordering and dedupe. Then thread the parameter outward.

In `crates/devkit-config/src/lib.rs`:

```rust
/// Resolve the effective config by layering and deep-merging all applicable
/// files. `main_checkout` is this repository's main checkout when `start` is a
/// linked worktree of one — resolved by the caller, because `devkit-config`
/// asks git nothing.
pub fn resolve(
    explicit: Option<&Path>,
    start: &Path,
    main_checkout: Option<&Path>,
) -> Result<(Config, Provenance)> {
    resolve_with_home(explicit, start, main_checkout, home_config_path().as_deref())
}
```

Thread `main_checkout` through `resolve_with_home`, `discover`, `health`, and
`health_with_home` the same way.

In `crates/devkit-ports/src/load.rs:17`:

```rust
    let main = devkit_common::git::main_checkout(Path::new(start))
        .ok()
        .flatten();
    let (cfg, provenance) = config::resolve(explicit, start, main.as_deref())?;
```

In `src/bin/devkit/brief.rs:465`, the same shape. A git failure yields `None`,
never an error: `devkit brief` must stay silent outside a devkit project and the
hook must never block a write.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --workspace`
Expected: PASS. Existing `resolve_with_home` tests pass `None` and are unchanged.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(config): layer the main checkout's config into a worktree

A linked worktree is a sibling of its main checkout, not a descendant, so
the upward walk never reached the repository's own config and every
worktree needed a copy — one that is written at creation and never
refreshed. The main checkout is injected rather than resolved here, so
devkit-config stays a leaf crate that asks git nothing.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 10: Wire the harness and docs to the main checkout

**Files:**
- Modify: `crates/devkit-locks/src/hook.rs` (`harness_enabled`)
- Modify: `crates/devkit-docs/src/manifest.rs` (`discover`)
- Test: `crates/devkit-locks/src/hook.rs`, `crates/devkit-docs` tests

**Interfaces:**
- Consumes: `devkit_common::git::main_checkout` (Task 2), `project_layers` (Task 6).
- Produces: no signature changes. Both readers stop passing `None`.

- [ ] **Step 1: Write the failing test**

```rust
    /// A worktree inherits its repository's harness declaration, which is the
    /// point: enforcement is declared once, not once per worktree.
    #[test]
    fn harness_is_inherited_from_the_main_checkout() {
        let main = tempfile::tempdir().unwrap();
        devkit_common::git::run(&["init", "-q", "-b", "main"], main.path()).unwrap();
        devkit_common::git::run(&["config", "user.email", "t@example.com"], main.path()).unwrap();
        devkit_common::git::run(&["config", "user.name", "Test"], main.path()).unwrap();
        std::fs::write(
            main.path().join("devkit.toml"),
            "[harness]\nenforce_writes = true\n",
        )
        .unwrap();
        devkit_common::git::run(&["add", "."], main.path()).unwrap();
        devkit_common::git::run(&["commit", "-qm", "init"], main.path()).unwrap();

        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("wt");
        devkit_common::git::run(
            &["worktree", "add", "-q", linked.to_str().unwrap(), "-b", "side"],
            main.path(),
        )
        .unwrap();
        std::fs::remove_file(linked.join("devkit.toml")).ok();

        assert!(enforcement_enabled(&linked));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-locks harness_is_inherited -- --test-threads=1`
Expected: FAIL — `harness_enabled` passes `None`.

- [ ] **Step 3: Write minimal implementation**

In `crates/devkit-locks/src/hook.rs`:

```rust
fn harness_enabled(cwd: &Path) -> bool {
    let main = devkit_common::git::main_checkout(cwd).ok().flatten();
    let Ok(layers) = devkit_config::project_layers(cwd, main.as_deref()) else {
        return false;
    };
    layers.iter().any(|layer| {
        std::fs::read_to_string(&layer.path)
            .map(|b| harness_flag_in(&b))
            .unwrap_or(false)
    })
}
```

Apply the same two lines in `crates/devkit-docs/src/manifest.rs::discover`.

- [ ] **Step 4: Run the full gate**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks crates/devkit-docs
git commit -m "feat(locks,docs): inherit the main checkout's layers

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 11: Path anchoring by key kind

**Files:**
- Modify: `crates/devkit-config/src/lib.rs:940-960` (`resolve_path_key`, `resolve_defaults`), `:222-234` (`Defaults` docstrings)
- Modify: `schema/devkit-config.json` (regenerated)
- Modify: `docs/configuration.md:109`, `:169`
- Test: `crates/devkit-config/tests/relative_config_paths.rs`

**Interfaces:**
- Consumes: `resolve` with `main_checkout` (Task 9).
- Produces: `resolve_path_key(raw, key, origin, checkout_root)` — `doppler_yaml` anchors to `checkout_root`, `worktree_root` and `baseline_path` to the declaring layer.

- [ ] **Step 1: Write the failing test**

Add to `crates/devkit-config/tests/relative_config_paths.rs`:

```rust
/// `doppler_yaml` names a file inside the repository being worked on, so it
/// anchors to the checkout reading it — a branch that adds an app to the map
/// takes effect without merging first. `worktree_root` and `baseline_path` name
/// locations on this machine and keep anchoring to the layer that declared them.
#[test]
fn repository_relative_paths_anchor_to_the_consuming_checkout() {
    let main = tempfile::tempdir().unwrap();
    std::fs::write(
        main.path().join("devkit.toml"),
        "[defaults]\n\
         worktree_root = \"trees\"\n\
         branch_prefix = \"x/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \".\"\n\
         doppler_yaml = \"doppler.yaml\"\n",
    )
    .unwrap();

    let worktree = tempfile::tempdir().unwrap();
    let (cfg, _) =
        devkit_config::resolve(None, worktree.path(), Some(main.path())).unwrap();

    assert_eq!(
        std::path::Path::new(&cfg.defaults.doppler_yaml),
        worktree.path().join("doppler.yaml"),
        "repository-relative: anchors to the checkout reading it"
    );
    assert_eq!(
        std::path::Path::new(&cfg.defaults.worktree_root),
        main.path().join("trees"),
        "host path: anchors to the declaring layer"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p devkit-config --test relative_config_paths`
Expected: FAIL — `doppler_yaml` resolves to `main.path().join("doppler.yaml")`.

- [ ] **Step 3: Write minimal implementation**

In `crates/devkit-config/src/lib.rs`:

```rust
/// What a `[defaults]` path names, which decides what it anchors to.
enum PathKind {
    /// A location on this machine. Only the layer that declared it gives it
    /// meaning, so it anchors there.
    Host,
    /// A file inside the repository being worked on. It anchors to the checkout
    /// reading it, so a branch that changes it takes effect without merging.
    RepoRelative,
}

fn resolve_path_key(
    raw: &str,
    key: &str,
    kind: PathKind,
    origin: &HashMap<String, PathBuf>,
    checkout_root: &Path,
) -> Result<String> {
    let expanded = expand_vars(raw, key)?;
    if expanded.is_empty() {
        return Ok(String::new());
    }
    let p = expand_tilde(&expanded);
    let anchor = match kind {
        PathKind::Host => layer_dir(origin, key),
        PathKind::RepoRelative => Some(checkout_root),
    };
    let joined = match (p.is_absolute(), anchor) {
        (true, _) | (false, None) => p,
        (false, Some(dir)) => dir.join(p),
    };
    Ok(normalize_lexically(&joined).to_string_lossy().into_owned())
}

fn resolve_defaults(
    cfg: &mut Config,
    origin: &HashMap<String, PathBuf>,
    checkout_root: &Path,
) -> Result<()> {
    for (key, kind, field) in [
        (
            "defaults.worktree_root",
            PathKind::Host,
            &mut cfg.defaults.worktree_root,
        ),
        (
            "defaults.baseline_path",
            PathKind::Host,
            &mut cfg.defaults.baseline_path,
        ),
        (
            "defaults.doppler_yaml",
            PathKind::RepoRelative,
            &mut cfg.defaults.doppler_yaml,
        ),
    ] {
        *field = resolve_path_key(field, key, kind, origin, checkout_root)?;
    }
    cfg.defaults.branch_prefix =
        expand_vars(&cfg.defaults.branch_prefix, "defaults.branch_prefix")?;
    Ok(())
}
```

`resolve_with_home` passes the absolutized `start` as `checkout_root`.

Update the three `Defaults` docstrings so the published schema's hover text
carries the rule, which it has never mentioned:

```rust
    /// Directory issue worktrees are created under. `~` is expanded. A relative
    /// path anchors to the directory of the config layer that declared it,
    /// including when that layer is the repository's main checkout.
    pub worktree_root: String,
```

```rust
    /// Checkout path for the baseline server. `~` is expanded. A relative path
    /// anchors to the directory of the config layer that declared it, including
    /// when that layer is the repository's main checkout.
    pub baseline_path: String,
```

```rust
    /// Path to the repo's `doppler.yaml`; its `setup` paths seed app path
    /// inference. `~` is expanded. A relative path anchors to the checkout
    /// reading the config, not to the layer that declared it, so a worktree
    /// resolves its own copy. Leave empty and every app needs its own `path`.
    pub doppler_yaml: String,
```

In `docs/configuration.md:109`, correct
`crates/devkit-ports/src/config.rs` to `crates/devkit-config/src/lib.rs`. At
`:169`, replace the blanket anchoring rule with the per-kind one.

- [ ] **Step 4: Regenerate the schema and run the gate**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit --test config_schema`
Then: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: PASS, with `schema/devkit-config.json` updated.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(config): anchor defaults paths by what the key means

A host path names a location on this machine and only the declaring layer
gives it meaning. A repository-relative path names a file inside the
checkout reading it, so doppler_yaml resolves per worktree and a branch
adding an app takes effect without merging. The published schema has
never described anchoring at all; each of the three keys now carries its
rule.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Self-Review

**Spec coverage.**

| Spec section | Task |
| --- | --- |
| Step 0: one git module, sanitized env, timeout | 1 |
| Step 0: named questions, `worktree list` main checkout | 2 |
| Step 0: migrate all call sites, delete `cmd::git` | 3, 5 |
| Step 0: `find_root_from`, declared fallback, seven fixtures | 4 |
| Step 0: greppable guard | 5 |
| Step 1: `project_layers`, `Layer`, `LayerKind` | 6 |
| Step 1: harness over the stack, CWD signature, env short-circuit, global outside | 7 |
| Step 1: docs over the stack, `--project` write restriction | 8 |
| Step 2: main-checkout layer, precedence, cutoff, dedupe | 6 (rules), 9 (wiring) |
| Step 2: degradation to `None` on git failure | 9 |
| Step 3: per-key anchoring, docstrings, schema, `configuration.md` | 11 |

No spec requirement is unclaimed.

**Known gaps to resolve during execution.**

1. Task 6's `project_layers` is written twice — a scaffold, then the real
   assembly. Implement the second form directly; the first is there only to show
   what the tests bind to first.
2. `docs::resolve::project_root` and the four remaining hand-rolled ancestor
   walks (`docs/manifest.rs`, `docs/resolve.rs`, `docs/lockfiles.rs`) are spec'd
   as `Path::ancestors()` rewrites but have no task. They are behavior-preserving
   cleanups; fold each into whichever task touches its file, or drop them.
3. `end.rs` wants the main checkout even when run from it, where `main_checkout`
   returns `None`. Task 5 gives the fallback; check both call sites still mean
   what they meant.

**Type consistency.** `Layer`/`LayerKind` are defined in Task 6 and used with
those names in 7, 8, 9, 10. `Worktree` gains `bare` in Task 2 and every
construction site is updated there. `resolve` grows its third parameter in Task 9
and both production callers change in the same task.
