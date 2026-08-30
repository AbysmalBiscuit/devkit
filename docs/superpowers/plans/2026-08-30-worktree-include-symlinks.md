# Worktree include symlinks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An include pattern that matches a symlink reproduces the link at the destination pointing at the same target, instead of walking through it and duplicating the files behind it.

**Architecture:** A new `sys::symlink` platform primitive creates links. `IncludePlan` grows a third vector, `links`, holding `(relative path, target)` pairs. `plan_includes` and `plan_dir` test `symlink_metadata` before any `is_dir` test, so a link is classified as a link and never recursed into. `apply_includes` creates the planned links after copying files, and returns the link count alongside the file count.

**Tech Stack:** Rust edition 2024, `std::os::unix::fs::symlink` / `std::os::windows::fs::{symlink_dir, symlink_file}`, `tempfile` for test fixtures, `anyhow` for errors.

**Spec:** `docs/superpowers/specs/2026-08-30-worktree-include-symlinks-design.md`

## Global Constraints

- No new workspace dependencies. Everything here is `std` plus the existing `tempfile` dev-dependency.
- Fail-open: every failure in the include path becomes a warning string, never a propagated error. A failed include has never aborted worktree creation and must not start.
- Platform-specific code lives only in `crates/devkit-common/src/sys/`. No `#[cfg(windows)]` or `#[cfg(unix)]` in `worktree.rs`.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all` must all pass before each commit.
- Tests take scratch directories from `tempfile::tempdir()`, never from a hand-built path under `std::env::temp_dir()`. Bind the `TempDir` guard for as long as the path is used.
- `copy_out` is out of scope and must not change behaviour.
- CI runs ubuntu, macos and windows. Any test needing a symlink fixture must skip cleanly where the platform refuses to create one.

## Coordination

The branch `worktree-include-progress` is unmerged and restructures `IncludePlan` from three vectors into a per-pattern list with `missing()` / `existing()` flattening iterators. This plan is written against `main`. If that branch lands first, rebase onto it and express `links` as a third entry kind in whatever shape it introduced; the behaviour in every task below is unchanged by that restructuring.

---

### Task 1: `sys::symlink` platform primitive

Creates a symlink, choosing the Windows call from whether the target is a directory. Nothing else in the workspace can create a link today.

**Files:**
- Modify: `crates/devkit-common/src/sys/mod.rs`
- Modify: `crates/devkit-common/src/sys/unix.rs`
- Modify: `crates/devkit-common/src/sys/windows.rs`
- Test: `crates/devkit-common/src/sys/mod.rs` (a `#[cfg(test)] mod tests` at the end of the file)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn symlink(target: &Path, link: &Path, target_is_dir: bool) -> std::io::Result<()>` in `devkit_common::sys`.

- [ ] **Step 1: Write the failing test**

Append to `crates/devkit-common/src/sys/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Windows refuses symlink creation without Developer Mode or admin, and
    /// CI runners may not have either. A refusal is not a failure of the code
    /// under test, so the test reports the skip and stops.
    fn can_symlink(dir: &Path) -> bool {
        let probe = dir.join("probe_link");
        match symlink(Path::new("probe_target"), &probe, false) {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(e) => {
                eprintln!("skipping: this platform refuses symlink creation ({e})");
                false
            }
        }
    }

    #[test]
    fn symlink_to_a_file_is_a_link_holding_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        if !can_symlink(tmp.path()) {
            return;
        }
        std::fs::write(tmp.path().join("real.txt"), "content").unwrap();
        let link = tmp.path().join("link.txt");

        symlink(Path::new("real.txt"), &link, false).unwrap();

        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "destination is a link");
        assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("real.txt"));
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "content");
    }

    #[test]
    fn symlink_to_a_directory_resolves_through_to_its_contents() {
        let tmp = tempfile::tempdir().unwrap();
        if !can_symlink(tmp.path()) {
            return;
        }
        std::fs::create_dir(tmp.path().join("real_dir")).unwrap();
        std::fs::write(tmp.path().join("real_dir/inner.txt"), "inner").unwrap();
        let link = tmp.path().join("link_dir");

        symlink(Path::new("real_dir"), &link, true).unwrap();

        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "destination is a link");
        assert_eq!(
            std::fs::read_to_string(link.join("inner.txt")).unwrap(),
            "inner"
        );
    }

    /// A source link whose target is gone is reproduced as a link whose target
    /// is gone, not reported as an error.
    #[test]
    fn symlink_to_a_missing_target_still_creates_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        if !can_symlink(tmp.path()) {
            return;
        }
        let link = tmp.path().join("dangling");

        symlink(Path::new("nothing_here"), &link, false).unwrap();

        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink());
        assert!(!link.exists(), "target does not resolve");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common sys::tests`

Expected: FAIL to compile, `cannot find function 'symlink' in this scope`.

- [ ] **Step 3: Add the Unix backend**

Append to `crates/devkit-common/src/sys/unix.rs`:

```rust
pub(super) fn symlink(
    target: &std::path::Path,
    link: &std::path::Path,
    _target_is_dir: bool,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
```

- [ ] **Step 4: Add the Windows backend**

Append to `crates/devkit-common/src/sys/windows.rs`:

```rust
pub(super) fn symlink(
    target: &std::path::Path,
    link: &std::path::Path,
    target_is_dir: bool,
) -> std::io::Result<()> {
    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}
```

- [ ] **Step 5: Declare it on the boundary**

Add to `crates/devkit-common/src/sys/mod.rs`, beside the other forwarding wrappers:

```rust
/// Create a symlink at `link` pointing at `target`, where `target` is written
/// verbatim rather than resolved. Windows needs to know at creation time
/// whether the target is a directory and has no way to infer it from the path,
/// so `target_is_dir` is supplied by the caller; Unix ignores it.
///
/// Windows refuses this without Developer Mode or administrator rights. The
/// error is returned rather than handled, so a caller can degrade.
pub fn symlink(target: &Path, link: &Path, target_is_dir: bool) -> std::io::Result<()> {
    imp::symlink(target, link, target_is_dir)
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit-common sys::tests`

Expected: PASS, 3 tests. On a Windows machine without Developer Mode they pass by printing the skip line; check the output says which happened.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-common/src/sys/
git commit -m "feat(sys): add a symlink primitive to the platform boundary"
```

---

### Task 2: Plan links instead of walking through them

`plan_includes` and `plan_dir` classify a match by `Path::is_dir`, which follows links, so a symlinked directory takes the recursion branch and a symlinked file reaches `classify_file`. This task makes a link its own plan entry and stops the walk descending through one. Nothing creates links yet, so the observable change is that a link's contents stop being planned.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs:158-162` (`IncludePlan`)
- Modify: `crates/devkit-common/src/worktree.rs:173-235` (`plan_includes`)
- Modify: `crates/devkit-common/src/worktree.rs:250-285` (`plan_dir`)
- Test: `crates/devkit-common/src/worktree.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `IncludePlan.links: Vec<(PathBuf, PathBuf)>`, each pair being `(path relative to dest, target exactly as read_link returned it)`, sorted and deduplicated by the relative path. `fn classify_link(rel: &Path, src: &Path, links: &mut Vec<(PathBuf, PathBuf)>, warnings: &mut Vec<String>)`.

**Every matched link goes in `links`, whatever the destination holds.** `existing` stays a list of files only. This matters: `apply_includes` runs `copy_file` over `existing` under `--overwrite`, so a link routed there would be replaced by a *copy of its target's contents* — reintroducing the behaviour this change exists to remove, on the overwrite path. Whether to skip, or to replace, an occupied destination is decided per link in Task 3 against the live filesystem.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `crates/devkit-common/src/worktree.rs`:

```rust
/// Creating a symlink is refused on Windows without Developer Mode. Where it
/// is refused the test cannot build its fixture, so it reports and stops.
fn link_or_skip(target: &Path, link: &Path, target_is_dir: bool) -> bool {
    match crate::sys::symlink(target, link, target_is_dir) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("skipping: this platform refuses symlink creation ({e})");
            false
        }
    }
}

#[test]
fn a_symlinked_file_is_planned_as_a_link_not_a_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    if !link_or_skip(Path::new("../real.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["inc/".to_string()]);

    assert!(plan.missing.is_empty(), "no files planned: {:?}", plan.missing);
    assert_eq!(plan.links.len(), 1, "one link planned: {:?}", plan.links);
    assert_eq!(plan.links[0].0, Path::new("inc").join("link.txt"));
    assert_eq!(plan.links[0].1, Path::new("..").join("real.txt"));
}

/// The whole point: the walk must not descend through a link, so the files
/// behind it are never planned.
#[test]
fn a_symlinked_directory_is_planned_as_one_link_not_its_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(source.join("real_dir")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real_dir/inner.txt"), "inner").unwrap();
    if !link_or_skip(Path::new("../real_dir"), &source.join("inc/link_dir"), true) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["inc/".to_string()]);

    assert!(
        plan.missing.is_empty(),
        "contents not planned: {:?}",
        plan.missing
    );
    assert_eq!(plan.links.len(), 1, "one link planned: {:?}", plan.links);
    assert_eq!(plan.links[0].0, Path::new("inc").join("link_dir"));
}

/// A pattern naming the link directly, rather than its parent directory,
/// reaches a different branch of plan_includes and must classify the same.
#[test]
fn a_pattern_naming_a_link_directly_plans_it_as_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(source.join("real_dir")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    if !link_or_skip(Path::new("real_dir"), &source.join("link_dir"), true) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["link_dir".to_string()]);

    assert!(plan.missing.is_empty(), "{:?}", plan.missing);
    assert_eq!(plan.links.len(), 1, "{:?}", plan.links);
    assert_eq!(plan.links[0].0, Path::new("link_dir"));
}

/// An occupied destination does not demote a link into `existing`. Routing it
/// there would hand it to `copy_file` under --overwrite, which copies the
/// target's contents. Whether to skip or replace is decided at apply time.
#[test]
fn a_link_stays_a_link_even_when_its_destination_is_occupied() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    std::fs::write(dest.join("link.txt"), "already here").unwrap();
    if !link_or_skip(Path::new("real.txt"), &source.join("link.txt"), false) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["link.txt".to_string()]);

    assert_eq!(plan.links.len(), 1, "still a link: {:?}", plan.links);
    assert!(
        plan.existing.is_empty(),
        "never routed to the file path: {:?}",
        plan.existing
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common worktree::tests::a_symlinked -- --nocapture`

Expected: FAIL to compile, `no field 'links' on type 'IncludePlan'`.

- [ ] **Step 3: Add the field**

Replace the `IncludePlan` definition at `crates/devkit-common/src/worktree.rs:155-162`:

```rust
/// The result of walking `patterns` without copying anything: every matched
/// file sorted by whether `dest` already has it, plus every matched symlink
/// paired with the target it holds. Paths are relative to `dest`.
pub struct IncludePlan {
    pub missing: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
    /// (path relative to `dest`, target exactly as the source link holds it).
    /// A link's own contents are never planned, so a symlinked directory
    /// contributes one entry here and nothing to `missing`.
    pub links: Vec<(PathBuf, PathBuf)>,
    pub warnings: Vec<String>,
}
```

- [ ] **Step 4: Add the link classifier**

Add beside `classify_file` in `crates/devkit-common/src/worktree.rs`:

```rust
/// Record a source symlink and the target it holds. Every matched link lands
/// here whatever the destination holds: routing an occupied one into
/// `existing` would hand it to `copy_file` under `overwrite`, which writes the
/// target's contents instead of reproducing the link. `make_link` decides
/// per link whether to skip or replace.
fn classify_link(
    rel: &Path,
    src: &Path,
    links: &mut Vec<(PathBuf, PathBuf)>,
    warnings: &mut Vec<String>,
) {
    match std::fs::read_link(src) {
        Ok(target) => links.push((rel.to_path_buf(), target)),
        Err(e) => warnings.push(format!("reading link {}: {e}", src.display())),
    }
}
```

- [ ] **Step 5: Test the link before the directory in both walk branches**

In `plan_includes`, replace the classification at `crates/devkit-common/src/worktree.rs:215-227`:

```rust
            let target = dest.join(rel);
            // A link is classified before any is_dir test, which would follow
            // it and send a symlinked directory into the recursion.
            if is_symlink(&matched) {
                classify_link(rel, &matched, &mut links, &mut warnings);
            } else if matched.is_dir() {
                plan_dir(
                    &matched,
                    rel,
                    dest,
                    &mut missing,
                    &mut existing,
                    &mut links,
                    &mut warnings,
                );
            } else {
                classify_file(&target, rel, &mut missing, &mut existing);
            }
```

In `plan_dir`, replace the classification at `crates/devkit-common/src/worktree.rs:277-282`:

```rust
        if is_symlink(&child) {
            classify_link(&child_rel, &child, links, warnings);
        } else if child.is_dir() {
            plan_dir(&child, &child_rel, dest, missing, existing, links, warnings);
        } else {
            classify_file(&target, &child_rel, missing, existing);
        }
```

Add `links: &mut Vec<(PathBuf, PathBuf)>,` to `plan_dir`'s parameter list, after `existing`. Add the helper beside `classify_link`:

```rust
/// Whether `path` is a symlink, judged without following it. A Windows
/// junction reports true here and is reproduced as a symlink.
fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}
```

- [ ] **Step 6: Declare, sort and return the new vector**

In `plan_includes`, add `let mut links = Vec::new();` beside the other three declarations, and replace the sort-and-return block at `crates/devkit-common/src/worktree.rs:231-240`:

```rust
    missing.sort();
    missing.dedup();
    existing.sort();
    existing.dedup();
    links.sort();
    links.dedup_by(|a, b| a.0 == b.0);
    IncludePlan {
        missing,
        existing,
        links,
        warnings,
    }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p devkit-common worktree:: -- --nocapture`

Expected: PASS. The four new tests pass or print their skip line. Every pre-existing `worktree::tests` test still passes — none of them use symlinks, so none should change.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-common/src/worktree.rs
git commit -m "fix(worktree): plan a matched symlink as a link, not its contents"
```

---

### Task 3: Create the planned links

`apply_includes` copies `missing` and, under `overwrite`, `existing`. This task makes it create `plan.links` too, returns the count separately, and handles replacing an existing destination without deleting through a link.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs:73-79` (`copy_includes`)
- Modify: `crates/devkit-common/src/worktree.rs:120-153` (`apply_includes`)
- Test: `crates/devkit-common/src/worktree.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `devkit_common::sys::symlink` from Task 1; `IncludePlan.links` from Task 2.
- Produces: `apply_includes` returns `(usize, usize, Vec<String>)` — `(files_copied, links_created, warnings)`. `copy_includes` returns the same triple. Both are breaking signature changes; Task 4 updates the callers.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/devkit-common/src/worktree.rs`:

```rust
#[test]
fn a_symlinked_file_is_reproduced_as_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    if !link_or_skip(Path::new("../real.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let (copied, linked, warnings) = copy_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(copied, 0, "no bytes copied");
    assert_eq!(linked, 1);
    assert!(warnings.is_empty(), "{warnings:?}");
    let landed = dest.join("inc/link.txt");
    let meta = std::fs::symlink_metadata(&landed).unwrap();
    assert!(meta.file_type().is_symlink(), "destination is a link");
    assert_eq!(
        std::fs::read_link(&landed).unwrap(),
        Path::new("..").join("real.txt")
    );
}

/// A relative target reproduced verbatim resolves inside the destination,
/// because the destination has the same shape as the source.
#[test]
fn a_relative_target_resolves_inside_the_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(dest.join("inc")).unwrap();
    std::fs::write(source.join("real.txt"), "source copy").unwrap();
    std::fs::write(dest.join("real.txt"), "destination copy").unwrap();
    if !link_or_skip(Path::new("../real.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let (_, linked, _) = copy_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(linked, 1);
    assert_eq!(
        std::fs::read_to_string(dest.join("inc/link.txt")).unwrap(),
        "destination copy",
        "the link resolves within the destination, not back to the source"
    );
}

#[test]
fn a_symlinked_directorys_contents_are_not_duplicated() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(source.join("real_dir")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real_dir/inner.txt"), "inner").unwrap();
    if !link_or_skip(Path::new("../real_dir"), &source.join("inc/link_dir"), true) {
        return;
    }

    let (copied, linked, _) = copy_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(copied, 0, "nothing copied");
    assert_eq!(linked, 1);
    let landed = dest.join("inc/link_dir");
    assert!(
        std::fs::symlink_metadata(&landed).unwrap().file_type().is_symlink(),
        "reproduced as a link"
    );
    assert!(
        !dest.join("inc/link_dir_real").exists(),
        "no duplicate directory materialised"
    );
}

/// A broken source link is a fact to mirror, not an error to report.
#[test]
fn a_broken_link_is_reproduced_broken_without_a_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    if !link_or_skip(Path::new("nothing_here"), &source.join("inc/dangling"), false) {
        return;
    }

    let (_, linked, warnings) = copy_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(linked, 1);
    assert!(warnings.is_empty(), "{warnings:?}");
    let landed = dest.join("inc/dangling");
    assert!(std::fs::symlink_metadata(&landed).unwrap().file_type().is_symlink());
    assert!(!landed.exists(), "target does not resolve");
}

/// Without --overwrite an occupied destination is left exactly as it was, and
/// is not counted as a link created.
#[test]
fn an_occupied_destination_is_left_alone_without_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    std::fs::write(dest.join("link.txt"), "already here").unwrap();
    if !link_or_skip(Path::new("real.txt"), &source.join("link.txt"), false) {
        return;
    }

    let (copied, linked, warnings) = copy_includes(&source, &dest, &["link.txt".to_string()]);

    assert_eq!(copied, 0);
    assert_eq!(linked, 0, "nothing created: {warnings:?}");
    assert_eq!(
        std::fs::read_to_string(dest.join("link.txt")).unwrap(),
        "already here"
    );
    assert!(
        !std::fs::symlink_metadata(dest.join("link.txt"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "the existing plain file was not replaced"
    );
}

/// Replacing a link to a directory must unlink it, never recurse into the
/// target and delete the files it points at.
#[test]
fn overwriting_a_linked_directory_leaves_the_target_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(source.join("real_dir")).unwrap();
    std::fs::create_dir_all(dest.join("inc")).unwrap();
    std::fs::create_dir_all(tmp.path().join("victim")).unwrap();
    std::fs::write(tmp.path().join("victim/precious.txt"), "do not delete").unwrap();
    if !link_or_skip(Path::new("../real_dir"), &source.join("inc/link_dir"), true) {
        return;
    }
    if !link_or_skip(&tmp.path().join("victim"), &dest.join("inc/link_dir"), true) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["inc/".to_string()]);
    let (_, linked, warnings) = apply_includes(&source, &dest, &plan, true);

    assert_eq!(linked, 1, "{warnings:?}");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("victim/precious.txt")).unwrap(),
        "do not delete",
        "the old link's target was not deleted through it"
    );
    assert_eq!(
        std::fs::read_link(dest.join("inc/link_dir")).unwrap(),
        Path::new("..").join("real_dir")
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common worktree:: -- --nocapture`

Expected: FAIL to compile, `expected a tuple with 2 elements, found one with 3`.

- [ ] **Step 3: Create links in `apply_includes`**

Replace the body of `apply_includes` between the declarations and the return, at `crates/devkit-common/src/worktree.rs:128-152`:

```rust
    let mut copied = 0usize;
    let mut linked = 0usize;
    let mut warnings = Vec::new();

    for rel in &plan.missing {
        copy_file(
            &source.join(rel),
            &dest.join(rel),
            overwrite,
            &mut copied,
            &mut warnings,
        );
    }
    if overwrite {
        for rel in &plan.existing {
            copy_file(
                &source.join(rel),
                &dest.join(rel),
                true,
                &mut copied,
                &mut warnings,
            );
        }
    }
    for (rel, target) in &plan.links {
        make_link(
            &source.join(rel),
            &dest.join(rel),
            target,
            overwrite,
            &mut linked,
            &mut warnings,
        );
    }

    (copied, linked, warnings)
```

The `missing` and `existing` loops are untouched — Task 2 keeps links out of both vectors, so `copy_file` never sees one. Every link goes through `make_link`, which reads the live destination and honours `overwrite` itself.

- [ ] **Step 4: Add `make_link`**

Add beside `copy_file` in `crates/devkit-common/src/worktree.rs`:

```rust
/// Reproduce a source symlink at `dst`, writing `target` verbatim. Creates the
/// destination's parent directories the way `copy_file` does. Whether the
/// target is a directory is decided by resolving the *source* link, since the
/// target string alone cannot say and Windows needs to know; a source link that
/// does not resolve takes the file form and reproduces a broken link.
///
/// Windows refuses symlink creation without Developer Mode, so a failure here
/// is a warning and the run continues without the link, per the fail-open rule
/// the rest of this module follows.
fn make_link(
    src: &Path,
    dst: &Path,
    target: &Path,
    overwrite: bool,
    linked: &mut usize,
    warnings: &mut Vec<String>,
) {
    match std::fs::symlink_metadata(dst) {
        Ok(meta) => {
            if !overwrite {
                return;
            }
            // remove_dir_all through a link would delete the target's contents.
            let removed = if meta.file_type().is_symlink() {
                std::fs::remove_file(dst).or_else(|_| std::fs::remove_dir(dst))
            } else if meta.is_dir() {
                std::fs::remove_dir_all(dst)
            } else {
                std::fs::remove_file(dst)
            };
            if let Err(e) = removed {
                warnings.push(format!("replacing {}: {e}", dst.display()));
                return;
            }
        }
        Err(_) => {
            if let Some(parent) = dst.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                warnings.push(format!("creating {}: {e}", parent.display()));
                return;
            }
        }
    }
    match crate::sys::symlink(target, dst, src.is_dir()) {
        Ok(()) => *linked += 1,
        Err(e) => warnings.push(format!(
            "linking {} -> {}: {e}",
            dst.display(),
            target.display()
        )),
    }
}
```

- [ ] **Step 5: Widen `copy_includes`**

Replace `copy_includes` at `crates/devkit-common/src/worktree.rs:73-79`:

```rust
pub fn copy_includes(
    source: &Path,
    dest: &Path,
    patterns: &[String],
) -> (usize, usize, Vec<String>) {
    let plan = plan_includes(source, dest, patterns);
    let (copied, linked, apply_warnings) = apply_includes(source, dest, &plan, false);
    let mut warnings = plan.warnings;
    warnings.extend(apply_warnings);
    (copied, linked, warnings)
}
```

Update its doc comment's last line from `Returns (files_copied, warnings).` to `Returns (files_copied, links_created, warnings).`, and add: `A match that is a symlink is reproduced as a symlink pointing at the same target; its contents are not copied.`

Update `apply_includes`'s doc comment the same way.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit-common worktree:: -- --nocapture`

Expected: PASS, or skip lines on a platform that refuses symlink creation. `cargo test --workspace` will still fail to build the `devkit` binary — Task 4 fixes the callers.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(worktree): reproduce a matched symlink instead of its contents"
```

---

### Task 4: Report links at the call sites

Two callers destructure the old 2-tuple and one of them prints a summary. This task updates both and gives links their own output line.

**Files:**
- Modify: `src/bin/devkit/issue/setup.rs:216-220`
- Modify: `src/bin/devkit/issue/sync.rs:204-225`
- Test: `src/bin/devkit/issue/sync.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: the `(usize, usize, Vec<String>)` return from Task 3.
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Update the setup call site**

In `src/bin/devkit/issue/setup.rs`, replace lines 216-217:

```rust
    let (_copied, _linked, warnings) =
        devkit_common::worktree::copy_includes(std::path::Path::new(primary), worktree, patterns);
```

- [ ] **Step 2: Write the failing test for the sync summary**

Add to `mod tests` in `src/bin/devkit/issue/sync.rs`:

```rust
#[test]
fn counts_report_files_and_links_on_separate_lines() {
    assert_eq!(counts(0, 0), vec!["  copied nothing"]);
    assert_eq!(counts(3, 0), vec!["  copied 3 file(s):"]);
    assert_eq!(counts(0, 2), vec!["  linked 2"]);
    assert_eq!(
        counts(3, 2),
        vec!["  copied 3 file(s):", "  linked 2"],
        "a run that did both reports both"
    );
}

/// A run that only reproduced links did something, and must not say it copied
/// nothing.
#[test]
fn links_alone_are_not_reported_as_nothing() {
    assert!(!counts(0, 1).iter().any(|l| l.contains("nothing")));
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --bin devkit sync::tests::counts -- --nocapture`

Expected: FAIL to compile, `cannot find function 'counts' in this scope`.

- [ ] **Step 4: Add the helper**

Add to `src/bin/devkit/issue/sync.rs`, beside `list`:

```rust
/// The count lines for one worktree's result, in print order. Links are
/// counted apart from files and never folded into the file count: a link is
/// not a copied file, and a run that only reproduced links has not copied
/// nothing. When files were copied the first line is always the copy line, so
/// the caller can append the file list to it.
fn counts(copied: usize, linked: usize) -> Vec<String> {
    let mut lines = Vec::new();
    if copied > 0 {
        lines.push(format!("  copied {copied} file(s):"));
    }
    if linked > 0 {
        lines.push(format!("  linked {linked}"));
    }
    if lines.is_empty() {
        lines.push("  copied nothing".to_string());
    }
    lines
}
```

- [ ] **Step 5: Wire it into the run loop**

In `src/bin/devkit/issue/sync.rs`, replace line 204:

```rust
        let (copied, linked, warnings) = worktree::apply_includes(&source, &wt.path, &plan, clobber);
```

Replace the reporting block at lines 214-225:

```rust
        let mut lines = counts(copied, linked);
        if copied > 0 {
            let names = if clobber {
                let mut all = plan.missing.clone();
                all.extend(plan.existing.iter().cloned());
                list(&all, verbose)
            } else {
                list(&plan.missing, verbose)
            };
            lines[0].push('\n');
            lines[0].push_str(&names);
        }
        for line in lines {
            println!("{line}");
        }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --bin devkit sync::tests -- --nocapture`

Expected: PASS, including the seven pre-existing `list` tests.

- [ ] **Step 7: Run the full gate**

Run:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three clean. This is the first point at which the whole workspace builds again.

- [ ] **Step 8: Commit**

```bash
git add src/bin/devkit/issue/setup.rs src/bin/devkit/issue/sync.rs
git commit -m "feat(issue): report reproduced links on their own line"
```

---

### Task 5: Dry run names links as links

`report_dry` prints `would copy` / `would overwrite` / `would leave alone` from `plan.missing` and `plan.existing`. It says nothing about `plan.links`, so a dry run of a tree with links reports nothing where the real run creates them.

**Files:**
- Modify: `src/bin/devkit/issue/sync.rs` (`report_dry` and its call site in `run`)
- Test: `src/bin/devkit/issue/sync.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `IncludePlan.links` from Task 2; `list` and `LIST_MAX` from the existing module.
- Produces: `fn link_list(links: &[(PathBuf, PathBuf)], verbose: bool) -> String`. `report_dry` gains a `dest: &Path` parameter, becoming `fn report_dry(plan: &IncludePlan, dest: &Path, overwrite: bool, verbose: bool)`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/bin/devkit/issue/sync.rs`:

```rust
/// A dry run has to say a link is a link. Reporting it as a file to copy is
/// how the old behaviour hid itself in the preview.
#[test]
fn link_lines_name_the_target() {
    let links = vec![
        (PathBuf::from("inc/a"), PathBuf::from("../real_a")),
        (PathBuf::from("inc/b"), PathBuf::from("/abs/real_b")),
    ];
    let out = link_list(&links, false);
    assert!(out.contains("inc/a -> ../real_a"), "{out}");
    assert!(out.contains("inc/b -> /abs/real_b"), "{out}");
}

#[test]
fn an_empty_link_list_is_empty() {
    assert_eq!(link_list(&[], false), "");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --bin devkit sync::tests::link_ -- --nocapture`

Expected: FAIL to compile, `cannot find function 'link_list' in this scope`.

- [ ] **Step 3: Add `link_list`**

Add to `src/bin/devkit/issue/sync.rs`, beside `list`:

```rust
/// Render planned links one per line as `path -> target`, capped the way
/// `list` caps files. The target is shown because it is the part a reader
/// cannot infer and the part that decides whether the link will resolve.
fn link_list(links: &[(PathBuf, PathBuf)], verbose: bool) -> String {
    let shown = if verbose {
        links.len()
    } else {
        links.len().min(LIST_MAX)
    };
    let mut lines: Vec<String> = links[..shown]
        .iter()
        .map(|(rel, target)| format!("    {} -> {}", rel.display(), target.display()))
        .collect();
    let rest = links.len() - shown;
    if rest > 0 {
        lines.push(format!("    ...and {rest} more"));
        lines.push("    (rerun with --verbose to name every link)".to_string());
    }
    lines.join("\n")
}
```

- [ ] **Step 4: Report links in `report_dry`**

`report_dry` cannot tell a link it would create from one whose destination is
occupied without looking at the destination, and it does not currently receive
one. Change its signature to `fn report_dry(plan: &IncludePlan, dest: &Path, overwrite: bool, verbose: bool)` and update its single call site in `run` to pass `&wt.path`.

Add to `report_dry` after the `existing` block and before the "nothing to copy" check:

```rust
    // Occupancy is read here rather than stored in the plan: the plan keeps
    // every matched link in one vector precisely so nothing routes a link
    // through the file path, and the destination is what decides the wording.
    let (fresh, occupied): (Vec<_>, Vec<_>) = plan
        .links
        .iter()
        .cloned()
        .partition(|(rel, _)| std::fs::symlink_metadata(dest.join(rel)).is_err());
    if !fresh.is_empty() {
        println!("  would link:\n{}", link_list(&fresh, verbose));
    }
    if !occupied.is_empty() {
        if overwrite {
            println!("  would replace with a link:\n{}", link_list(&occupied, verbose));
        } else {
            println!(
                "  would leave these links alone (rerun with --overwrite to replace):\n{}",
                link_list(&occupied, verbose)
            );
        }
    }
```

Change the final condition so a plan holding only links does not claim there is nothing to do:

```rust
    if plan.missing.is_empty() && plan.existing.is_empty() && plan.links.is_empty() {
        println!("  nothing to copy");
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --bin devkit sync::tests -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/devkit/issue/sync.rs
git commit -m "feat(issue): name planned links in the sync-includes dry run"
```

---

### Task 6: Documentation

Three places describe the include copy. One of them states the behaviour this change removes and pairs it with `preserve`'s outbound direction, which is deliberately unchanged.

**Files:**
- Modify: `docs/configuration.md:122` (the `worktree_include` table row)
- Modify: `docs/configuration.md:444-446` (the `preserve` limits paragraph)
- Modify: `docs/commands.md` (the `sync-includes` bullet in the `status`/`info`/`end`/`sync-includes` section)

**Interfaces:**
- Consumes: the behaviour from Tasks 2 through 5.
- Produces: nothing.

- [ ] **Step 1: Correct the `preserve` paragraph**

In `docs/configuration.md`, replace the sentence beginning "Two limits worth knowing. Symlinks are followed…":

```markdown
Two limits worth knowing. Symlinks are followed on the way out, so a link inside
the worktree is archived as its target's content. This is deliberately the
opposite of `defaults.worktree_include`, which reproduces a link rather than its
contents: an include lands in a live worktree that still sits beside the primary
checkout, where a relative link resolves, while preservation archives out of a
worktree about to be deleted into a location that may outlive the link's target
entirely. And a copy is not atomic: `std::fs::copy` truncates
```

Leave the remainder of that paragraph unchanged.

- [ ] **Step 2: Add the rule to the `worktree_include` row**

In `docs/configuration.md:122`, insert before the final "Anchor patterns" sentence:

```markdown
A match that is a symlink is reproduced as a symlink holding the same target, and its contents are not copied — a symlinked directory becomes one link, not a duplicated tree. Creating a symlink on Windows needs Developer Mode or administrator rights; where it is refused the link is skipped with a warning and the rest of the run continues.
```

- [ ] **Step 3: Add the rule to `docs/commands.md`**

In the `sync-includes` bullet, insert after the sentence ending "any file the worktree already has.":

```markdown
A matched symlink is reproduced as a symlink pointing at the same target rather than being followed, so its contents are never duplicated into the worktree; links are counted and reported separately from copied files. On Windows this needs Developer Mode or administrator rights, and a refused link warns and is skipped.
```

- [ ] **Step 4: Check the docs against the code**

Run: `rg -n "symlink" docs/commands.md docs/configuration.md`

Expected: the `preserve` paragraph, the `worktree_include` row, and the `sync-includes` bullet, and no remaining claim that the inbound direction follows links.

- [ ] **Step 5: Run the full gate**

Run:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all three clean.

- [ ] **Step 6: Commit**

```bash
git add docs/commands.md docs/configuration.md
git commit -m "docs: describe symlink reproduction in worktree_include"
```

---

## Verification

After Task 6, confirm the change end to end against a real tree rather than a fixture.

- [ ] Build: `cargo build --release -p devkit`
- [ ] Create a symlinked directory inside a project's include set, then run `devkit issue sync-includes --dry-run` and confirm it prints a `would link:` line naming the target rather than the files behind it.
- [ ] Run `devkit issue sync-includes` against a worktree and confirm with `ls -la` that the destination holds a link, that the link resolves, and that no duplicate tree was written.
- [ ] Confirm the run prints a `linked N` line.
