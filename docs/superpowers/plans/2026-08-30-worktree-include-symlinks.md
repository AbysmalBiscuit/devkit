# Worktree include symlinks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An include pattern that matches a symlink reproduces the link at the destination pointing at the same target, instead of walking through it and duplicating the files behind it.

**Architecture:** A new `sys::symlink` platform primitive creates links. `PatternPlan` grows a third vector, `links`, holding `(relative path, target)` pairs, so a link is claimed and displayed per pattern like everything else. The walk tests `symlink_metadata` before any `is_dir` test, classifying a link as a link and never recursing into it. A private `LinkMode` keeps `copy_out` following links as it does today. `apply_includes_with` creates each pattern's links after copying its files, and returns the link count alongside the file count.

**Tech Stack:** Rust edition 2024, `std::os::unix::fs::symlink` / `std::os::windows::fs::{symlink_dir, symlink_file}`, `tempfile` for test fixtures, `anyhow` for errors.

**Spec:** `docs/superpowers/specs/2026-08-30-worktree-include-symlinks-design.md`

## Global Constraints

- No new workspace dependencies. Everything here is `std` plus the existing `tempfile` dev-dependency.
- Fail-open: every failure in the include path becomes a warning string, never a propagated error. A failed include has never aborted worktree creation and must not start.
- Platform-specific code lives only in `crates/devkit-common/src/sys/`. No `#[cfg(windows)]` or `#[cfg(unix)]` in `worktree.rs`.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all` must all pass before each commit.
- Tests take scratch directories from `tempfile::tempdir()`, never from a hand-built path under `std::env::temp_dir()`. Bind the `TempDir` guard for as long as the path is used.
- `copy_out` must not change behaviour. It shares the walk, so this is enforced by an explicit `LinkMode::Follow`, not by leaving code untouched, and a test pins it.
- CI runs ubuntu, macos and windows. Any test needing a symlink fixture must skip cleanly where the platform refuses to create one.

## Coordination

`worktree-include-progress` has landed, and this plan is written against the
shape it introduced. `IncludePlan` holds `patterns: Vec<PatternPlan>` rather
than two flat vectors, with `missing()` / `existing()` / `missing_len()` /
`existing_len()` as accessors; `plan_includes_with` and `apply_includes_with`
carry an `IncludeEvent` callback; and `claim_once` gives a path matched by
several patterns to the first of them in configuration order. Links follow all
three of those rules.

`parallel-includes` will parallelise the per-pattern copy loop in
`apply_includes_with` with rayon. It is independent of this work: it
parallelises a list of files and does not care that some entries beside it are
links. Whichever lands second rebases onto the other, and the conflict is
confined to that one loop body.

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

`Walk::plan_one` and `Walk::plan_dir` classify a match by `Path::is_dir`, which
follows links, so a symlinked directory takes the recursion branch and a
symlinked file reaches `classify_file`. This task makes a link its own plan
entry and stops the walk descending through one. Nothing creates links yet, so
the observable change is that a link's contents stop being planned.

The same walk serves `copy_out`, whose behaviour the spec keeps unchanged, so
this task also introduces the mode that separates the two directions.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` — `PatternPlan`, `IncludePlan` and its accessors, `Walk`, `Walk::plan_dir`, `Walk::plan_one`, `plan_includes_with`, `claim_once`, `copy_out`
- Test: `crates/devkit-common/src/worktree.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `devkit_common::sys::symlink` from Task 1, in test fixtures only.
- Produces:
  - `PatternPlan.links: Vec<(PathBuf, PathBuf)>`, each pair being `(path relative to dest, target exactly as read_link returned it)`, sorted and deduplicated by the relative path within the pattern.
  - `IncludePlan::links(&self) -> impl Iterator<Item = (&Path, &Path)>` and `IncludePlan::links_len(&self) -> usize`, matching the existing `missing()` / `missing_len()` pair.
  - `enum LinkMode { Preserve, Follow }` — private to the module.
  - `fn is_symlink(path: &Path) -> bool` — private.
  - `Walk::classify_link(&self, rel: &Path, src: &Path, out: &mut PatternPlan, warnings: &mut Vec<String>)`.

**Links are per-pattern, like `missing` and `existing`.** They live in
`PatternPlan`, so `claim_once` gives a link matched by two patterns to the first
of them, the per-pattern progress display counts it, and the flattening
accessors present it in the same order as everything else.

**Every matched link goes in `links`, whatever the destination holds.**
`existing` stays a list of files only. This matters: `apply_includes_with` runs
`copy_file` over `existing` under `overwrite`, so a link routed there would be
replaced by a *copy of its target's contents* — reintroducing the behaviour this
change exists to remove, on the overwrite path. Whether to skip, or to replace,
an occupied destination is decided per link in Task 3 against the live
filesystem.

**`copy_out` needs the old behaviour and shares this walk.** The spec keeps
`issue end --preserve` following links and archiving their contents, because it
archives out of a worktree that is about to be deleted: a reproduced link would
point at a path that stops resolving the moment the worktree goes. Since
`copy_out` builds its plan with `plan_includes`, leaving that function alone is
not an option — the constraint has to be enforced in the walk. `LinkMode` is
that enforcement. It stays private: both public `plan_includes*` functions
delegate with `Preserve`, and `copy_out` is the one caller that asks for
`Follow`, so the public surface does not grow.

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

    assert_eq!(plan.missing_len(), 0, "no files planned");
    assert_eq!(plan.existing_len(), 0);
    let links: Vec<_> = plan.links().collect();
    assert_eq!(links.len(), 1, "one link planned: {links:?}");
    assert_eq!(links[0].0, Path::new("inc").join("link.txt"));
    assert_eq!(links[0].1, Path::new("..").join("real.txt"));
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
}

#[test]
fn a_symlinked_dir_contributes_one_entry_and_is_not_walked() {
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

    assert_eq!(
        plan.missing_len(),
        0,
        "the link's contents are not planned: {:?}",
        plan.missing().collect::<Vec<_>>()
    );
    let links: Vec<_> = plan.links().collect();
    assert_eq!(links.len(), 1, "one link planned: {links:?}");
    assert_eq!(links[0].0, Path::new("inc").join("link_dir"));
}

#[test]
fn a_pattern_naming_a_link_directly_plans_it_as_a_link() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("real_dir")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real_dir/inner.txt"), "inner").unwrap();
    if !link_or_skip(Path::new("real_dir"), &source.join("link_dir"), true) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["link_dir".to_string()]);

    assert_eq!(plan.missing_len(), 0);
    let links: Vec<_> = plan.links().collect();
    assert_eq!(links.len(), 1, "{links:?}");
    assert_eq!(links[0].0, Path::new("link_dir"));
}

/// An occupied destination keeps the entry in `links`, never in `existing`:
/// `existing` is copied with `copy_file` under `--overwrite`, which would write
/// the target's contents. Task 3 decides skip-or-replace per link.
#[test]
fn an_occupied_link_destination_stays_a_link_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(dest.join("inc")).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    std::fs::write(dest.join("inc/link.txt"), "already here").unwrap();
    if !link_or_skip(Path::new("../real.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(plan.links().count(), 1, "still a link entry");
    assert_eq!(
        plan.existing_len(),
        0,
        "never routed to the copy path: {:?}",
        plan.existing().collect::<Vec<_>>()
    );
}

/// `copy_out` archives out of a worktree that is about to be deleted, so it
/// keeps following links and copying what they resolve to.
#[test]
fn copy_out_still_archives_a_links_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("wt");
    let dest = tmp.path().join("archive");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    if !link_or_skip(Path::new("../real.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let (copied, warnings) = copy_out(&source, &dest, &["inc/".to_string()]);

    assert_eq!(copied, 1, "the target's contents were archived: {warnings:?}");
    let landed = dest.join("inc/link.txt");
    assert!(
        !std::fs::symlink_metadata(&landed)
            .unwrap()
            .file_type()
            .is_symlink(),
        "archived as a real file, not a link"
    );
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), "content");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common worktree:: -- --nocapture`

Expected: FAIL to compile, `no method named 'links' found for struct 'IncludePlan'`.

- [ ] **Step 3: Add the field and the accessors**

Add to `PatternPlan` in `crates/devkit-common/src/worktree.rs`, after `existing`:

```rust
    /// (path relative to `dest`, target exactly as the source link holds it).
    /// A link's own contents are never planned, so a symlinked directory
    /// contributes one entry here and nothing to `missing`.
    pub links: Vec<(PathBuf, PathBuf)>,
```

Extend `PatternPlan`'s doc comment: `Every file one worktree_include pattern matched, split by whether dest already has it, plus every symlink it matched paired with the target that link holds.`

Add to `impl IncludePlan`, beside `existing()` and `existing_len()`:

```rust
    /// Every matched symlink and the target it holds, ordered as
    /// [`IncludePlan::missing`] is.
    pub fn links(&self) -> impl Iterator<Item = (&Path, &Path)> {
        self.patterns
            .iter()
            .flat_map(|p| p.links.iter().map(|(rel, t)| (rel.as_path(), t.as_path())))
    }

    /// Total count of matched symlinks, across every pattern.
    pub fn links_len(&self) -> usize {
        self.patterns.iter().map(|p| p.links.len()).sum()
    }
```

Add `links: Vec::new(),` to the `PatternPlan` literal in `plan_includes_with`.

- [ ] **Step 4: Add the mode, the predicate, and the link classifier**

Add near the top of `crates/devkit-common/src/worktree.rs`, above `Walk`:

```rust
/// How a walk treats a matched symlink.
///
/// The inbound and outbound directions genuinely differ. An include lands in a
/// live worktree that still sits beside the primary checkout, so a reproduced
/// link resolves. `copy_out` archives out of a worktree that is about to be
/// deleted, into a location that may outlive the target, so a link there could
/// archive a path that stops resolving the moment the worktree goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkMode {
    /// Plan the link itself and do not descend through it.
    Preserve,
    /// Follow the link and plan whatever it resolves to.
    Follow,
}

/// Whether `path` is a symlink, judged without following it. A Windows
/// junction reports true here and is reproduced as a symlink.
fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}
```

Add `mode: LinkMode,` to the `Walk` struct, and this method to `impl Walk`, beside `classify_file`:

```rust
    /// Record a source symlink and the target it holds, and count it as a
    /// match. Every matched link lands here whatever the destination holds:
    /// routing an occupied one into `existing` would hand it to `copy_file`
    /// under `overwrite`, which writes the target's contents instead of
    /// reproducing the link. `make_link` decides per link whether to skip or
    /// replace.
    fn classify_link(
        &self,
        rel: &Path,
        src: &Path,
        out: &mut PatternPlan,
        warnings: &mut Vec<String>,
    ) {
        match std::fs::read_link(src) {
            Ok(target) => out.links.push((rel.to_path_buf(), target)),
            Err(e) => {
                warnings.push(format!("reading link {}: {e}", src.display()));
                return;
            }
        }
        self.found.set(self.found.get() + 1);
        (self.on)(IncludeEvent::Found {
            files: self.found.get(),
        });
    }
```

- [ ] **Step 5: Test the link before the directory in both walk branches**

In `Walk::plan_dir`, replace the classification of each child:

```rust
            if self.mode == LinkMode::Preserve && is_symlink(&child) {
                self.classify_link(&child_rel, &child, out, warnings);
            } else if child.is_dir() {
                self.plan_dir(&child, &child_rel, out, warnings);
            } else {
                self.classify_file(&child_rel, out);
            }
```

In `Walk::plan_one`, replace the classification of each glob match:

```rust
            // A link is classified before any is_dir test, which follows it and
            // would send a symlinked directory into the recursion.
            if self.mode == LinkMode::Preserve && is_symlink(&matched) {
                self.classify_link(rel, &matched, out, warnings);
            } else if matched.is_dir() {
                self.plan_dir(&matched, rel, out, warnings);
            } else {
                self.classify_file(rel, out);
            }
```

- [ ] **Step 6: Thread the mode through and sort the new vector**

Rename the body of `plan_includes_with` to a private `plan_with_mode`, taking
`mode: LinkMode` as its last parameter, and construct `Walk` with it. Inside,
add the sort beside the other two:

```rust
        out.links.sort();
        out.links.dedup_by(|a, b| a.0 == b.0);
```

Both public entry points delegate:

```rust
pub fn plan_includes_with(
    source: &Path,
    dest: &Path,
    patterns: &[String],
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> IncludePlan {
    plan_with_mode(source, dest, patterns, on, LinkMode::Preserve)
}
```

In `copy_out`, replace `let plan = plan_includes(source, dest, &inside);` with:

```rust
    let plan = plan_with_mode(source, dest, &inside, &|_| {}, LinkMode::Follow);
```

Add to `copy_out`'s doc comment: `Symlinks are followed and archived as their target's content — the outbound direction deliberately differs from the inbound one, which reproduces the link.`

- [ ] **Step 7: Claim a link once**

In `claim_once`, add the third retain so a link matched by two patterns is
planned under the first of them:

```rust
        plan.links.retain(|(rel, _)| claimed.insert(rel.clone()));
```

Extend `claim_once`'s doc comment: `Links are claimed on the same rule, so one is created once.`

- [ ] **Step 8: Run the tests to verify they pass**

Run: `cargo test -p devkit-common worktree:: -- --nocapture`

Expected: PASS. The five new tests pass or print their skip line. Every
pre-existing `worktree::tests` test still passes — none of them use symlinks,
so none should change.

- [ ] **Step 9: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add crates/devkit-common/src/worktree.rs
git commit -m "fix(worktree): plan a matched symlink as a link, not its contents"
```

---

### Task 3: Create the planned links

`apply_includes_with` walks each pattern's worklist and copies it. This task
makes it create that pattern's links too, count them separately, and replace an
existing destination without deleting through a link.

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` — `copy_includes`, `copy_includes_with`, `apply_includes`, `apply_includes_with`, `copy_out`, and a new `make_link` beside `copy_file`
- Test: `crates/devkit-common/src/worktree.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `devkit_common::sys::symlink` from Task 1; `PatternPlan.links` and `IncludePlan::links_len` from Task 2.
- Produces: `apply_includes`, `apply_includes_with`, `copy_includes` and `copy_includes_with` all return `(usize, usize, Vec<String>)` — `(files_copied, links_created, warnings)`. Breaking signature changes; Task 4 updates the binary's callers. `fn make_link(src: &Path, dst: &Path, target: &Path, overwrite: bool, linked: &mut usize, warnings: &mut Vec<String>)` — private.

**`copy_out` keeps its two-value signature.** Task 2's `LinkMode::Follow` leaves
its plans with empty `links` vectors, so the link loop is a no-op and the count
is always zero. `copy_out` discards the middle value rather than exposing one
that cannot be non-zero.

**Links count toward a pattern's progress total.** `IncludeEvent::EntryStart`
reports `files` as the worklist plus the links, and `FileDone` counts through
both, so the display's denominator covers every unit of work in the pattern.
`EntryDone.copied` stays a count of files written — the renderer in
`src/bin/devkit/issue/setup.rs` ignores that field, and links are reported at
the summary level in Task 4.

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
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    std::fs::write(dest.join("real.txt"), "the worktree's own copy").unwrap();
    if !link_or_skip(Path::new("../real.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let (_, linked, warnings) = copy_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(linked, 1, "{warnings:?}");
    assert_eq!(
        std::fs::read_to_string(dest.join("inc/link.txt")).unwrap(),
        "the worktree's own copy",
        "the link resolves inside the destination, not back at the source"
    );
}

#[test]
fn an_absolute_target_is_reproduced_verbatim() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    let real = source.join("real.txt");
    std::fs::write(&real, "content").unwrap();
    if !link_or_skip(&real, &source.join("inc/link.txt"), false) {
        return;
    }

    let (_, linked, warnings) = copy_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(linked, 1, "{warnings:?}");
    assert_eq!(std::fs::read_link(dest.join("inc/link.txt")).unwrap(), real);
}

#[test]
fn a_broken_link_is_reproduced_broken() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    if !link_or_skip(Path::new("nowhere.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let (_, linked, warnings) = copy_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(linked, 1, "{warnings:?}");
    let landed = dest.join("inc/link.txt");
    assert!(
        std::fs::symlink_metadata(&landed)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!landed.exists(), "still broken, as the source is");
    assert_eq!(std::fs::read_link(&landed).unwrap(), Path::new("nowhere.txt"));
}

#[test]
fn an_existing_destination_is_left_alone_without_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(dest.join("inc")).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    std::fs::write(dest.join("inc/link.txt"), "already here").unwrap();
    if !link_or_skip(Path::new("../real.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let (_, linked, warnings) = copy_includes(&source, &dest, &["inc/".to_string()]);

    assert_eq!(linked, 0, "nothing replaced: {warnings:?}");
    assert_eq!(
        std::fs::read_to_string(dest.join("inc/link.txt")).unwrap(),
        "already here"
    );
}

#[test]
fn overwrite_replaces_an_existing_destination_with_the_link() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(dest.join("inc")).unwrap();
    std::fs::write(source.join("real.txt"), "content").unwrap();
    std::fs::write(dest.join("inc/link.txt"), "already here").unwrap();
    if !link_or_skip(Path::new("../real.txt"), &source.join("inc/link.txt"), false) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["inc/".to_string()]);
    let (_, linked, warnings) = apply_includes(&source, &dest, &plan, true);

    assert_eq!(linked, 1, "{warnings:?}");
    let landed = dest.join("inc/link.txt");
    assert!(
        std::fs::symlink_metadata(&landed)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the real file was replaced by a link"
    );
}

/// Replacing a link to a directory must remove the link, never recurse through
/// it: `remove_dir_all` on a symlinked directory deletes the target's contents.
#[test]
fn replacing_a_directory_link_does_not_delete_through_it() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("src");
    let dest = tmp.path().join("dst");
    std::fs::create_dir_all(source.join("inc")).unwrap();
    std::fs::create_dir_all(source.join("real_dir")).unwrap();
    std::fs::create_dir_all(dest.join("inc")).unwrap();
    let keep = tmp.path().join("other_dir");
    std::fs::create_dir_all(&keep).unwrap();
    std::fs::write(keep.join("keep.txt"), "do not delete").unwrap();
    std::fs::write(source.join("real_dir/inner.txt"), "inner").unwrap();
    if !link_or_skip(Path::new("../real_dir"), &source.join("inc/link_dir"), true) {
        return;
    }
    // The destination already holds a link, pointed somewhere else entirely.
    if !link_or_skip(&keep, &dest.join("inc/link_dir"), true) {
        return;
    }

    let plan = plan_includes(&source, &dest, &["inc/".to_string()]);
    let (_, linked, warnings) = apply_includes(&source, &dest, &plan, true);

    assert_eq!(linked, 1, "{warnings:?}");
    assert_eq!(
        std::fs::read_to_string(keep.join("keep.txt")).unwrap(),
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

- [ ] **Step 3: Create links in `apply_includes_with`**

Replace the body of `apply_includes_with` in
`crates/devkit-common/src/worktree.rs`, keeping its parameters and changing
only the return type to `(usize, usize, Vec<String>)`:

```rust
    let mut copied = 0usize;
    let mut linked = 0usize;
    let mut warnings = Vec::new();
    let of = plan.patterns.len();

    for (index, entry) in plan.patterns.iter().enumerate() {
        let worklist: Vec<&PathBuf> = if overwrite {
            entry.missing.iter().chain(entry.existing.iter()).collect()
        } else {
            entry.missing.iter().collect()
        };
        // A link is a unit of this pattern's work, so the denominator the
        // display draws against covers both lists.
        let total = worklist.len() + entry.links.len();
        on(IncludeEvent::EntryStart {
            pattern: &entry.pattern,
            index,
            of,
            files: total,
        });

        let before = copied;
        let mut done = 0usize;
        for rel in &worklist {
            copy_file(
                &source.join(rel),
                &dest.join(rel),
                overwrite,
                &mut copied,
                &mut warnings,
            );
            done += 1;
            on(IncludeEvent::FileDone {
                pattern: &entry.pattern,
                done,
                of: total,
            });
        }
        for (rel, target) in &entry.links {
            make_link(
                &source.join(rel),
                &dest.join(rel),
                target,
                overwrite,
                &mut linked,
                &mut warnings,
            );
            done += 1;
            on(IncludeEvent::FileDone {
                pattern: &entry.pattern,
                done,
                of: total,
            });
        }

        on(IncludeEvent::EntryDone {
            pattern: &entry.pattern,
            index,
            of,
            copied: copied - before,
        });
    }

    (copied, linked, warnings)
```

The `worklist` loop is otherwise untouched — Task 2 keeps links out of both
`missing` and `existing`, so `copy_file` never sees one. Every link goes through
`make_link`, which reads the live destination and honours `overwrite` itself.

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

- [ ] **Step 5: Widen the four public wrappers, keep `copy_out` at two**

`apply_includes`, `copy_includes` and `copy_includes_with` are one-line
delegates or near it. Change each return type to `(usize, usize, Vec<String>)`
and pass the middle value through:

```rust
pub fn copy_includes(source: &Path, dest: &Path, patterns: &[String]) -> (usize, usize, Vec<String>) {
    copy_includes_with(source, dest, patterns, &|_| {})
}

pub fn apply_includes(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
) -> (usize, usize, Vec<String>) {
    apply_includes_with(source, dest, plan, overwrite, &|_| {})
}

pub fn copy_includes_with(
    source: &Path,
    dest: &Path,
    patterns: &[String],
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> (usize, usize, Vec<String>) {
    let plan = plan_includes_with(source, dest, patterns, on);
    let (copied, linked, apply_warnings) = apply_includes_with(source, dest, &plan, false, on);
    let mut warnings = plan.warnings;
    warnings.extend(apply_warnings);
    (copied, linked, warnings)
}
```

In `copy_out`, discard the link count, which a `Follow` plan cannot make
non-zero:

```rust
    // A Follow-mode plan holds no links, so the count is always zero.
    let (copied, _, apply_warnings) = apply_includes(source, dest, &plan, true);
```

Update each of the four doc comments' last line from
`Returns (files_copied, warnings).` to
`Returns (files_copied, links_created, warnings).`, and add to `copy_includes`
and `copy_includes_with`: `A match that is a symlink is reproduced as a symlink
pointing at the same target; its contents are not copied.`

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit-common worktree:: -- --nocapture`

Expected: PASS, or skip lines on a platform that refuses symlink creation.
`copy_out_still_archives_a_links_contents` from Task 2 still passes.
`cargo test --workspace` will still fail to build the `devkit` binary — Task 4
fixes the callers.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(worktree): reproduce a matched symlink instead of its contents"
```

---

### Task 4: Report links at the call sites

Two callers destructure the two-value returns Task 3 widened, and neither says
anything about links. This task fixes both builds and gives links their own
reported line.

**Files:**
- Modify: `src/bin/devkit/issue/setup.rs` — the `copy_includes_with` call and the step detail
- Modify: `src/bin/devkit/issue/sync.rs` — the `apply_includes` call and the summary
- Test: `src/bin/devkit/issue/sync.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: the three-value returns from Task 3.
- Produces: `fn counts(copied: usize, linked: usize) -> Vec<String>` in `src/bin/devkit/issue/sync.rs`, `pub(crate)` so `setup.rs` uses the same wording. Each element is one line, already pluralised; an element is present only when its count is non-zero, and an empty vector means nothing happened.

**Links get their own line, never folded into the file count.** A link is not a
copied file. `copied 12 file(s)` stays a count of files and `linked 3` sits
beside it.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/bin/devkit/issue/sync.rs`:

```rust
#[test]
fn counts_name_each_kind_separately() {
    assert_eq!(counts(0, 0), Vec::<String>::new());
    assert_eq!(counts(1, 0), vec!["copied 1 file"]);
    assert_eq!(counts(3, 0), vec!["copied 3 files"]);
    assert_eq!(counts(0, 1), vec!["linked 1 symlink"]);
    assert_eq!(counts(0, 2), vec!["linked 2 symlinks"]);
    assert_eq!(counts(2, 1), vec!["copied 2 files", "linked 1 symlink"]);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit issue::sync`

Expected: FAIL to compile, `cannot find function 'counts' in this scope`.

- [ ] **Step 3: Add `counts`**

Add to `src/bin/devkit/issue/sync.rs`, beside `list`:

```rust
/// One line per non-empty count, pluralised. Links are never folded into the
/// file count: a link is not a copied file. An empty vector means nothing was
/// written at all, which the caller reports in its own words.
pub(crate) fn counts(copied: usize, linked: usize) -> Vec<String> {
    let mut out = Vec::new();
    if copied > 0 {
        out.push(format!(
            "copied {copied} {}",
            if copied == 1 { "file" } else { "files" }
        ));
    }
    if linked > 0 {
        out.push(format!(
            "linked {linked} {}",
            if linked == 1 { "symlink" } else { "symlinks" }
        ));
    }
    out
}
```

- [ ] **Step 4: Update `sync.rs`'s call site**

Replace the `apply_includes` call and the summary that follows it in `run`:

```rust
        let (copied, linked, warnings) = worktree::apply_includes(&source, &wt.path, &plan, clobber);
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        if !overwrite && plan.existing_len() > 0 {
            eprintln!(
                "warning: already in {label}, left alone (rerun with --overwrite to replace):\n{}",
                list(plan.existing(), verbose)
            );
        }
        let summary = counts(copied, linked);
        if summary.is_empty() {
            println!("  copied nothing");
        } else {
            if copied > 0 {
                let names = if clobber {
                    list(plan.missing().chain(plan.existing()), verbose)
                } else {
                    list(plan.missing(), verbose)
                };
                println!("  copied {copied} file(s):\n{names}");
            }
            if linked > 0 {
                println!("  {}", summary.last().expect("linked > 0 pushed a line"));
            }
        }
```

- [ ] **Step 5: Update `setup.rs`'s call site**

Replace the `copy_includes_with` call and the step detail:

```rust
        let (copied, linked, warnings) = devkit_common::worktree::copy_includes_with(
            std::path::Path::new(primary),
            worktree,
            patterns,
            &|e| render.on(e),
        );
        let summary = super::sync::counts(copied, linked);
        step.detail(&if summary.is_empty() {
            "0 files".to_string()
        } else {
            summary.join(", ")
        });
        warnings
```

- [ ] **Step 6: Run the whole gate**

Run: `cargo test --workspace`

Expected: PASS. The build is whole again and every pre-existing test still
passes.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/devkit/issue/sync.rs src/bin/devkit/issue/setup.rs
git commit -m "feat(issue): report created symlinks beside copied files"
```

---

### Task 5: Dry run names links as links

`report_dry` prints `would copy` / `would overwrite` / `would leave alone` from
`plan.missing()` and `plan.existing()`. It says nothing about links, so a dry
run of a tree with links reports nothing where the real run creates them.

**Files:**
- Modify: `src/bin/devkit/issue/sync.rs` — `report_dry` and its single call site in `run`
- Test: `src/bin/devkit/issue/sync.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `IncludePlan::links` and `links_len` from Task 2; `list` and `LIST_MAX` from the existing module.
- Produces: `fn link_list<'a>(links: impl IntoIterator<Item = (&'a Path, &'a Path)>, verbose: bool) -> String`. `report_dry` gains a `dest: &Path` parameter, becoming `fn report_dry(plan: &IncludePlan, dest: &Path, overwrite: bool, verbose: bool)`.

**`report_dry` needs the destination.** Whether a planned link will be created
or skipped is decided by `make_link` against the live filesystem, not recorded
in the plan, so the dry run has to make the same check to say the same thing.
`run` already holds `wt.path`.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/bin/devkit/issue/sync.rs`:

```rust
#[test]
fn link_list_names_each_target_and_caps_a_directory() {
    let pairs: Vec<(PathBuf, PathBuf)> = (0..7)
        .map(|i| {
            (
                PathBuf::from("inc").join(format!("l{i}")),
                PathBuf::from("..").join(format!("t{i}")),
            )
        })
        .collect();
    let borrowed = || pairs.iter().map(|(a, b)| (a.as_path(), b.as_path()));

    let capped = link_list(borrowed(), false);
    assert!(capped.contains("l0 -> "), "names the target: {capped}");
    assert!(capped.contains("...and 2 more"), "caps at LIST_MAX: {capped}");
    assert!(capped.contains("--verbose"), "says how to see them all");

    let full = link_list(borrowed(), true);
    assert!(full.contains("l6 -> "), "verbose names every link: {full}");
    assert!(!full.contains("more"), "nothing elided: {full}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p devkit issue::sync`

Expected: FAIL to compile, `cannot find function 'link_list' in this scope`.

- [ ] **Step 3: Add `link_list`**

A link's line has to carry its target, which `list` has no room for, so this is
its own renderer sharing `LIST_MAX`. Add beside `list` in
`src/bin/devkit/issue/sync.rs`:

```rust
/// Render `links` as an indented block, one `path -> target` per line, capped
/// at `LIST_MAX` the way `list` caps a directory. `verbose` names every link.
/// The block carries no trailing newline, and is empty when `links` yields
/// nothing.
fn link_list<'a>(
    links: impl IntoIterator<Item = (&'a Path, &'a Path)>,
    verbose: bool,
) -> String {
    let rendered: Vec<String> = links
        .into_iter()
        .map(|(rel, target)| format!("    {} -> {}", rel.display(), target.display()))
        .collect();
    let shown = if verbose {
        rendered.len()
    } else {
        rendered.len().min(LIST_MAX)
    };
    let mut lines: Vec<String> = rendered[..shown].to_vec();
    let rest = rendered.len() - shown;
    if rest > 0 {
        lines.push(format!("    ...and {rest} more"));
        lines.push("    (rerun with --verbose to name every link)".to_string());
    }
    lines.join("\n")
}
```

- [ ] **Step 4: Report links in `report_dry`**

Replace `report_dry` in `src/bin/devkit/issue/sync.rs`:

```rust
fn report_dry(plan: &IncludePlan, dest: &Path, overwrite: bool, verbose: bool) {
    if plan.missing_len() > 0 {
        println!("  would copy:\n{}", list(plan.missing(), verbose));
    }
    if plan.existing_len() > 0 {
        if overwrite {
            println!("  would overwrite:\n{}", list(plan.existing(), verbose));
        } else {
            println!(
                "  would leave alone (rerun with --overwrite to replace):\n{}",
                list(plan.existing(), verbose)
            );
        }
    }
    if plan.links_len() > 0 {
        // Whether a link is created is decided against the live destination,
        // so the preview makes the same check the copy will.
        let (fresh, occupied): (Vec<_>, Vec<_>) = plan
            .links()
            .partition(|(rel, _)| std::fs::symlink_metadata(dest.join(rel)).is_err());
        if !fresh.is_empty() {
            println!("  would link:\n{}", link_list(fresh, verbose));
        }
        if !occupied.is_empty() {
            let heading = if overwrite {
                "  would replace with a link:"
            } else {
                "  would leave alone (rerun with --overwrite to replace with a link):"
            };
            println!("{heading}\n{}", link_list(occupied, verbose));
        }
    }
    if plan.missing_len() == 0 && plan.existing_len() == 0 && plan.links_len() == 0 {
        println!("  nothing to copy");
    }
}
```

- [ ] **Step 5: Pass the destination at the call site**

In `run`, change `report_dry(&plan, overwrite, verbose);` to:

```rust
            report_dry(&plan, &wt.path, overwrite, verbose);
```

- [ ] **Step 6: Run the whole gate**

Run: `cargo test --workspace`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
git add src/bin/devkit/issue/sync.rs
git commit -m "feat(issue): name symlinks as links in a sync-includes dry run"
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
- [ ] Confirm `copy_out` is unchanged: put a symlink in a worktree matched by a `[preserve]` pattern, run `devkit issue end --preserve`, and confirm the archive holds a real file with the target's contents, not a link.
