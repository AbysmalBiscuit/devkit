# Worktree include progress implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the `worktree_include` backfill a numbered progress step with one persisted sub-step per include entry, so a large include list no longer looks like a hang.

**Architecture:** `IncludePlan` stops flattening its matches and keeps them grouped per pattern, which is what lets the display count per entry. A progress callback (`IncludeEvent` plus `*_with` function variants) is threaded through the walk and the copy. `Steps` gains a step that hands its closure a `Step` handle, so a step can draw a transient sub-line and persist finished sub-steps. `backfill_includes` maps events onto that handle.

**Tech Stack:** Rust edition 2024, `indicatif` (already a dependency), `glob` (already a dependency), `anyhow`, `tempfile` for test scratch.

**Spec:** `docs/superpowers/specs/2026-08-30-worktree-include-progress-design.md`

## Global Constraints

- Branch `worktree-include-progress`, worktree `C:/Users/Lev/Git/lev/devkit-worktrees/worktree-include-progress`, based on `main` at `141cc4c`. Never work in the primary clone.
- No new workspace dependencies. Everything here uses crates already in the tree.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all` must all pass before each commit.
- No em dashes anywhere: not in code, comments, doc comments, commit messages, or emitted strings.
- Comments follow the repo rule: default to none, and earn their place with a non-obvious *why*. No narration of what a line does, no references to this plan, no task numbers.
- Every fail-open path stays fail-open. A bad glob, an unreadable directory, a non-UTF-8 pattern, or a copy error is collected as a warning string and never propagated.
- `devkit-common` is a library crate. Public items need doc comments; the repo uses them throughout `worktree.rs` and `progress.rs`.
- Progress bars are hidden when stderr is not a terminal, which is always true under `cargo test`. Never write a test that asserts on rendered output. Assert on the event stream and on `Steps::started()`.
- Test scratch comes from `tempfile::tempdir()`, bound to a variable that outlives every use of paths derived from it.
- `parallel_includes` is building a parallel copy on top of this branch and is blocked until it lands. Do not leave the branch half-finished.

---

### Task 1: `IncludePlan` keeps per-pattern grouping

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs:73-260` (`copy_includes`, `apply_includes`, `IncludePlan`, `plan_includes`, `classify_file`, `plan_dir`)
- Modify: `crates/devkit-common/src/worktree.rs:380-640` (the include tests)
- Modify: `src/bin/devkit/issue/sync.rs:31` (`list`), `:100-118` (`report_dry`), `:183-224` (`run`), `:312` (one test)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub struct PatternPlan { pub pattern: String, pub missing: Vec<PathBuf>, pub existing: Vec<PathBuf> }`
  - `pub struct IncludePlan { pub patterns: Vec<PatternPlan>, pub warnings: Vec<String> }`
  - `IncludePlan::missing(&self) -> impl Iterator<Item = &Path>`
  - `IncludePlan::existing(&self) -> impl Iterator<Item = &Path>`
  - `IncludePlan::missing_len(&self) -> usize`
  - `IncludePlan::existing_len(&self) -> usize`
  - `fn plan_one(source: &Path, dest: &Path, pattern: &str, opts: glob::MatchOptions, out: &mut PatternPlan, warnings: &mut Vec<String>)` (private)
  - `plan_includes`, `apply_includes`, `copy_includes` keep their current signatures.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-common/src/worktree.rs`:

```rust
/// The copy display counts per include entry, so the plan has to remember
/// which pattern produced each match instead of pouring them into one list.
#[test]
fn plan_groups_matches_by_the_pattern_that_found_them() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    write(&src.join(".tool-versions"), "node 20");
    write(&src.join("hooks/a.sh"), "x");
    write(&src.join("hooks/b.sh"), "x");

    let plan = plan_includes(
        &src,
        &dst,
        &[".tool-versions".to_string(), "hooks/".to_string()],
    );

    assert_eq!(plan.patterns.len(), 2);
    assert_eq!(plan.patterns[0].pattern, ".tool-versions");
    assert_eq!(plan.patterns[0].missing, vec![PathBuf::from(".tool-versions")]);
    assert_eq!(plan.patterns[1].pattern, "hooks/");
    assert_eq!(
        plan.patterns[1].missing,
        vec![PathBuf::from("hooks/a.sh"), PathBuf::from("hooks/b.sh")]
    );
}

/// Sub-step numbering is one-to-one with the configured include list, so a
/// pattern that only produced a warning still occupies its slot.
#[test]
fn a_pattern_that_warns_still_gets_its_own_entry() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    write(&src.join(".tool-versions"), "node 20");

    let plan = plan_includes(&src, &dst, &["[".to_string(), ".tool-versions".to_string()]);

    assert_eq!(plan.patterns.len(), 2);
    assert_eq!(plan.patterns[0].pattern, "[");
    assert!(plan.patterns[0].missing.is_empty());
    assert!(plan.patterns[0].existing.is_empty());
    assert_eq!(plan.warnings.len(), 1);
    assert_eq!(plan.patterns[1].missing, vec![PathBuf::from(".tool-versions")]);
}

/// The flattening views replace what the old flat vectors held.
#[test]
fn the_flattening_views_yield_every_match() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    write(&src.join(".tool-versions"), "node 20");
    write(&dst.join(".tool-versions"), "KEEP ME");
    write(&src.join("hooks/a.sh"), "x");

    let plan = plan_includes(
        &src,
        &dst,
        &[".tool-versions".to_string(), "hooks/".to_string()],
    );

    assert_eq!(plan.missing().collect::<Vec<_>>(), [Path::new("hooks/a.sh")]);
    assert_eq!(
        plan.existing().collect::<Vec<_>>(),
        [Path::new(".tool-versions")]
    );
    assert_eq!(plan.missing_len(), 1);
    assert_eq!(plan.existing_len(), 1);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common --lib worktree::tests::plan_groups_matches_by_the_pattern_that_found_them`
Expected: compile error, `no field 'patterns' on type 'IncludePlan'`.

- [ ] **Step 3: Restructure `IncludePlan` and `plan_includes`**

Replace the `IncludePlan` definition and `plan_includes` in `crates/devkit-common/src/worktree.rs`:

```rust
/// Every file one `worktree_include` pattern matched, split by whether `dest`
/// already has it. Both vectors come back sorted, so rendering does not vary
/// with filesystem iteration order.
pub struct PatternPlan {
    pub pattern: String,
    pub missing: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
}

/// The result of walking `patterns` without copying anything, kept grouped by
/// the pattern each match came from. Paths are relative to `dest`. Use
/// [`IncludePlan::missing`] and [`IncludePlan::existing`] for a flat view.
pub struct IncludePlan {
    /// One entry per configured pattern, in configuration order, including a
    /// pattern that matched nothing or only produced a warning.
    pub patterns: Vec<PatternPlan>,
    pub warnings: Vec<String>,
}

impl IncludePlan {
    /// Every match `dest` does not have, in pattern order then sorted within a
    /// pattern.
    pub fn missing(&self) -> impl Iterator<Item = &Path> {
        self.patterns
            .iter()
            .flat_map(|p| p.missing.iter().map(PathBuf::as_path))
    }

    /// Every match `dest` already has, ordered as [`IncludePlan::missing`] is.
    pub fn existing(&self) -> impl Iterator<Item = &Path> {
        self.patterns
            .iter()
            .flat_map(|p| p.existing.iter().map(PathBuf::as_path))
    }

    pub fn missing_len(&self) -> usize {
        self.patterns.iter().map(|p| p.missing.len()).sum()
    }

    pub fn existing_len(&self) -> usize {
        self.patterns.iter().map(|p| p.existing.len()).sum()
    }
}

/// Walk `patterns` the way `copy_includes` does, but classify matches instead
/// of copying them: each matched file lands in its pattern's `missing` or
/// `existing` depending on whether `dest` already has it. A directory match
/// contributes its files recursively, never the directory entry itself.
/// Fail-open, like `copy_includes`: a bad glob, an unreadable directory, or a
/// non-UTF-8 pattern becomes a warning string.
pub fn plan_includes(source: &Path, dest: &Path, patterns: &[String]) -> IncludePlan {
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };
    let mut warnings = Vec::new();
    let mut plans = Vec::with_capacity(patterns.len());

    for pattern in patterns {
        // Every configured pattern gets an entry, warnings included, so a
        // plan's entries line up one-to-one with the include list.
        let mut out = PatternPlan {
            pattern: pattern.clone(),
            missing: Vec::new(),
            existing: Vec::new(),
        };
        plan_one(source, dest, pattern, opts, &mut out, &mut warnings);
        out.missing.sort();
        out.existing.sort();
        plans.push(out);
    }

    IncludePlan {
        patterns: plans,
        warnings,
    }
}

/// Walk one pattern into `out`. Every failure is a warning, never a return
/// value, so one bad pattern cannot stop the rest of the list.
fn plan_one(
    source: &Path,
    dest: &Path,
    pattern: &str,
    opts: glob::MatchOptions,
    out: &mut PatternPlan,
    warnings: &mut Vec<String>,
) {
    let trimmed = pattern.trim_end_matches('/');
    let joined = source.join(trimmed);
    let Some(pat_str) = joined.to_str() else {
        warnings.push(format!("include pattern is not valid UTF-8: {pattern}"));
        return;
    };
    let entries = match glob::glob_with(pat_str, opts) {
        Ok(paths) => paths,
        Err(e) => {
            warnings.push(format!("bad include pattern `{pattern}`: {e}"));
            return;
        }
    };
    for entry in entries {
        let matched = match entry {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("reading match for `{pattern}`: {e}"));
                continue;
            }
        };
        let Ok(rel) = matched.strip_prefix(source) else {
            warnings.push(format!("match outside source: {}", matched.display()));
            continue;
        };
        if matched.is_dir() {
            plan_dir(
                &matched,
                rel,
                dest,
                &mut out.missing,
                &mut out.existing,
                warnings,
            );
        } else {
            classify_file(&dest.join(rel), rel, &mut out.missing, &mut out.existing);
        }
    }
}
```

`classify_file` and `plan_dir` keep their current bodies and signatures.

- [ ] **Step 4: Update `apply_includes` to iterate per pattern**

Replace `apply_includes` in `crates/devkit-common/src/worktree.rs`:

```rust
/// Copy the files an `IncludePlan` found: every path in a pattern's `missing`,
/// and every path in its `existing` only when `overwrite` is true. Patterns are
/// applied in configuration order. A plan is a snapshot, so unless `overwrite`
/// is set each copy re-checks the destination and skips a file that appeared
/// since. Fail-open, like `plan_includes`: a copy error is collected as a
/// warning string rather than propagated. Returns (files_copied, warnings).
pub fn apply_includes(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
) -> (usize, Vec<String>) {
    let mut copied = 0usize;
    let mut warnings = Vec::new();

    for entry in &plan.patterns {
        for rel in &entry.missing {
            copy_file(
                &source.join(rel),
                &dest.join(rel),
                overwrite,
                &mut copied,
                &mut warnings,
            );
        }
        if overwrite {
            for rel in &entry.existing {
                copy_file(
                    &source.join(rel),
                    &dest.join(rel),
                    true,
                    &mut copied,
                    &mut warnings,
                );
            }
        }
    }

    (copied, warnings)
}
```

`copy_includes` is unchanged.

- [ ] **Step 5: Migrate the existing `worktree.rs` tests off the flat fields**

In `crates/devkit-common/src/worktree.rs`, rewrite these assertions:

`plan_includes_puts_a_new_match_in_missing`:

```rust
    assert_eq!(plan.missing().collect::<Vec<_>>(), [Path::new(".env.local")]);
    assert_eq!(plan.existing_len(), 0);
    assert!(plan.warnings.is_empty());
```

`plan_includes_puts_an_already_present_match_in_existing`:

```rust
    assert_eq!(
        plan.existing().collect::<Vec<_>>(),
        [Path::new(".tool-versions")]
    );
    assert_eq!(plan.missing_len(), 0);
    assert!(plan.warnings.is_empty());
```

`plan_includes_directory_pattern_enumerates_files_not_the_directory`:

```rust
    assert_eq!(
        plan.existing().collect::<Vec<_>>(),
        [Path::new(".claude/hooks/pre.sh")]
    );
    assert_eq!(
        plan.missing().collect::<Vec<_>>(),
        [Path::new(".claude/hooks/sub/post.sh")]
    );
    assert!(plan.warnings.is_empty());
```

`plan_vectors_come_back_sorted`:

```rust
    assert_eq!(
        plan.missing().collect::<Vec<_>>(),
        [Path::new("hooks/b.sh"), Path::new("hooks/d.sh")]
    );
    assert_eq!(
        plan.existing().collect::<Vec<_>>(),
        [Path::new("hooks/a.sh"), Path::new("hooks/c.sh")]
    );
```

`plan_includes_pattern_matching_nothing_yields_empty_plan`:

```rust
    assert_eq!(plan.missing_len(), 0);
    assert_eq!(plan.existing_len(), 0);
    assert!(plan.warnings.is_empty());
```

`plan_includes_bad_glob_warns_without_panicking`:

```rust
    assert_eq!(plan.missing_len(), 0);
    assert_eq!(plan.existing_len(), 0);
    assert_eq!(plan.warnings.len(), 1);
```

- [ ] **Step 6: Update `sync.rs` to the flattening views**

In `src/bin/devkit/issue/sync.rs`, change `list`'s signature and its first loop line. Everything else in the function body is unchanged:

```rust
fn list<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>, verbose: bool) -> String {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut bare: Vec<String> = Vec::new();
    for path in paths {
        let path = path.as_ref();
        let mut components = path.components();
```

Replace `report_dry`:

```rust
fn report_dry(plan: &IncludePlan, overwrite: bool, verbose: bool) {
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
    if plan.missing_len() == 0 && plan.existing_len() == 0 {
        println!("  nothing to copy");
    }
}
```

In `run`, replace the four remaining uses. The overwrite prompt:

```rust
        let mut clobber = overwrite;
        if overwrite && plan.existing_len() > 0 {
            println!("  will overwrite:\n{}", list(plan.existing(), verbose));
```

The left-alone warning:

```rust
        if !overwrite && plan.existing_len() > 0 {
            eprintln!(
                "warning: already in {label}, left alone (rerun with --overwrite to replace):\n{}",
                list(plan.existing(), verbose)
            );
        }
```

The copied report, which no longer needs to build a combined vector:

```rust
        if copied == 0 {
            println!("  copied nothing");
        } else {
            let names = if clobber {
                list(plan.missing().chain(plan.existing()), verbose)
            } else {
                list(plan.missing(), verbose)
            };
            println!("  copied {copied} file(s):\n{names}");
        }
```

Delete the now-unused `let mut all = plan.missing.clone(); all.extend(...);` lines that preceded it.

In the `sync.rs` test module, one call site needs an explicit type now that `list` is generic:

```rust
        assert_eq!(list(Vec::<PathBuf>::new(), false), "");
```

Every other `list(&under(...), false)` call still compiles, because `&Vec<PathBuf>` iterates as `&PathBuf` and `&PathBuf: AsRef<Path>`.

- [ ] **Step 7: Run the full gate**

Run: `cargo test --workspace`
Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --all`

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-common/src/worktree.rs src/bin/devkit/issue/sync.rs
git commit -m "refactor(worktree): group an include plan by pattern

The plan flattened every pattern's matches into two sorted vectors, which
threw away the provenance a per-entry copy display needs. Keep one entry
per configured pattern, warnings included, so entries line up one-to-one
with the include list, and add missing()/existing() as flattening views
for the callers that want the flat form.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: `IncludeEvent` and a progress-aware plan walk

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` (add `IncludeEvent`, `plan_includes_with`, a `Walk` helper; `plan_includes` becomes a delegate)

**Interfaces:**
- Consumes: `PatternPlan`, `IncludePlan`, `plan_one`, `classify_file`, `plan_dir` from Task 1.
- Produces:
  - `pub enum IncludeEvent<'a>` with variants `Found { files: usize }`, `ScanDone { files: usize }`, `EntryStart { pattern: &'a str, index: usize, of: usize, files: usize }`, `FileDone { pattern: &'a str, done: usize, of: usize }`, `EntryDone { pattern: &'a str, index: usize, of: usize, copied: usize }`
  - `pub fn plan_includes_with(source: &Path, dest: &Path, patterns: &[String], on: &(dyn Fn(IncludeEvent) + Sync)) -> IncludePlan`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-common/src/worktree.rs`:

```rust
/// The plan walk is the first of the two silences the display has to fill, so
/// it has to report matches as it finds them, not only at the end.
#[test]
fn the_plan_walk_reports_a_running_count_and_a_total() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    for name in ["a.sh", "b.sh", "c.sh"] {
        write(&src.join("hooks").join(name), "x");
    }

    let found = std::sync::Mutex::new(Vec::new());
    let done = std::sync::Mutex::new(Vec::new());
    let plan = plan_includes_with(&src, &dst, &["hooks/".to_string()], &|e| match e {
        IncludeEvent::Found { files } => found.lock().unwrap().push(files),
        IncludeEvent::ScanDone { files } => done.lock().unwrap().push(files),
        _ => {}
    });

    assert_eq!(plan.missing_len(), 3);
    assert_eq!(*found.lock().unwrap(), vec![1, 2, 3]);
    assert_eq!(*done.lock().unwrap(), vec![3]);
}

/// The count spans the whole list, not one pattern, because the discovery
/// sub-step covers the entire walk.
#[test]
fn the_plan_walk_count_spans_every_pattern() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    write(&src.join(".tool-versions"), "node 20");
    write(&src.join("hooks/a.sh"), "x");

    let done = std::sync::Mutex::new(Vec::new());
    plan_includes_with(
        &src,
        &dst,
        &[".tool-versions".to_string(), "hooks/".to_string()],
        &|e| {
            if let IncludeEvent::ScanDone { files } = e {
                done.lock().unwrap().push(files);
            }
        },
    );

    assert_eq!(*done.lock().unwrap(), vec![2]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common --lib worktree::tests::the_plan_walk`
Expected: compile error, `cannot find function 'plan_includes_with'`.

- [ ] **Step 3: Add the event enum**

Add above `IncludePlan` in `crates/devkit-common/src/worktree.rs`:

```rust
/// What an include walk or copy reports as it runs, so a caller can draw
/// progress without the walk knowing anything about how it is displayed.
///
/// `index` and `of` are a pattern's position in the configured include list,
/// not a display number. A caller that draws extra sub-steps of its own numbers
/// them itself.
pub enum IncludeEvent<'a> {
    /// Files matched so far across the whole walk. Fires once per match.
    Found { files: usize },
    /// The walk finished, having matched `files` in total.
    ScanDone { files: usize },
    /// The copy started for `pattern`, with `files` in its worklist.
    EntryStart {
        pattern: &'a str,
        index: usize,
        of: usize,
        files: usize,
    },
    /// One file of `pattern`'s worklist is handled: copied, skipped because the
    /// destination already existed, or failed. `done` and `of` count within
    /// that pattern and may arrive out of order.
    FileDone {
        pattern: &'a str,
        done: usize,
        of: usize,
    },
    /// The copy finished for `pattern`, having written `copied` files.
    EntryDone {
        pattern: &'a str,
        index: usize,
        of: usize,
        copied: usize,
    },
}
```

- [ ] **Step 4: Thread the callback through the walk**

In `crates/devkit-common/src/worktree.rs`, add a walk context so the callback and the running count do not have to be extra parameters on every recursive call:

```rust
/// The callback and running match count a plan walk carries through its
/// recursion. `Cell` is enough because a walk is single threaded; the callback
/// is `Sync` for the copy side, which is not.
struct Walk<'a> {
    dest: &'a Path,
    on: &'a (dyn Fn(IncludeEvent) + Sync),
    found: std::cell::Cell<usize>,
}

impl Walk<'_> {
    fn classify_file(&self, rel: &Path, out: &mut PatternPlan) {
        if self.dest.join(rel).exists() {
            out.existing.push(rel.to_path_buf());
        } else {
            out.missing.push(rel.to_path_buf());
        }
        self.found.set(self.found.get() + 1);
        (self.on)(IncludeEvent::Found {
            files: self.found.get(),
        });
    }

    /// Recursively classify a directory's files without writing anything. `rel`
    /// tracks the path relative to `dest` in lockstep with `src` so classified
    /// paths stay dest-relative.
    fn plan_dir(
        &self,
        src: &Path,
        rel: &Path,
        out: &mut PatternPlan,
        warnings: &mut Vec<String>,
    ) {
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(e) => {
                warnings.push(format!("reading dir {}: {e}", src.display()));
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    warnings.push(format!("reading entry in {}: {e}", src.display()));
                    continue;
                }
            };
            let child = src.join(entry.file_name());
            let child_rel = rel.join(entry.file_name());
            if child.is_dir() {
                self.plan_dir(&child, &child_rel, out, warnings);
            } else {
                self.classify_file(&child_rel, out);
            }
        }
    }
}
```

Delete the free functions `classify_file` and `plan_dir`; `Walk`'s methods replace them.

Rename `plan_one` to a `Walk` method and give it the callback:

```rust
impl Walk<'_> {
    /// Walk one pattern into `out`. Every failure is a warning, never a return
    /// value, so one bad pattern cannot stop the rest of the list.
    fn plan_one(
        &self,
        source: &Path,
        pattern: &str,
        opts: glob::MatchOptions,
        out: &mut PatternPlan,
        warnings: &mut Vec<String>,
    ) {
        let trimmed = pattern.trim_end_matches('/');
        let joined = source.join(trimmed);
        let Some(pat_str) = joined.to_str() else {
            warnings.push(format!("include pattern is not valid UTF-8: {pattern}"));
            return;
        };
        let entries = match glob::glob_with(pat_str, opts) {
            Ok(paths) => paths,
            Err(e) => {
                warnings.push(format!("bad include pattern `{pattern}`: {e}"));
                return;
            }
        };
        for entry in entries {
            let matched = match entry {
                Ok(p) => p,
                Err(e) => {
                    warnings.push(format!("reading match for `{pattern}`: {e}"));
                    continue;
                }
            };
            let Ok(rel) = matched.strip_prefix(source) else {
                warnings.push(format!("match outside source: {}", matched.display()));
                continue;
            };
            if matched.is_dir() {
                self.plan_dir(&matched, rel, out, warnings);
            } else {
                self.classify_file(rel, out);
            }
        }
    }
}
```

- [ ] **Step 5: Add `plan_includes_with` and make `plan_includes` delegate**

```rust
/// [`plan_includes`], reporting [`IncludeEvent::Found`] as each match is
/// classified and [`IncludeEvent::ScanDone`] when the walk ends.
pub fn plan_includes_with(
    source: &Path,
    dest: &Path,
    patterns: &[String],
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> IncludePlan {
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };
    let walk = Walk {
        dest,
        on,
        found: std::cell::Cell::new(0),
    };
    let mut warnings = Vec::new();
    let mut plans = Vec::with_capacity(patterns.len());

    for pattern in patterns {
        // Every configured pattern gets an entry, warnings included, so a
        // plan's entries line up one-to-one with the include list.
        let mut out = PatternPlan {
            pattern: pattern.clone(),
            missing: Vec::new(),
            existing: Vec::new(),
        };
        walk.plan_one(source, pattern, opts, &mut out, &mut warnings);
        out.missing.sort();
        out.existing.sort();
        plans.push(out);
    }

    on(IncludeEvent::ScanDone {
        files: walk.found.get(),
    });
    IncludePlan {
        patterns: plans,
        warnings,
    }
}
```

Replace the body of `plan_includes` with the delegate, keeping its existing doc comment:

```rust
pub fn plan_includes(source: &Path, dest: &Path, patterns: &[String]) -> IncludePlan {
    plan_includes_with(source, dest, patterns, &|_| {})
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit-common --lib worktree`
Expected: PASS, including every test migrated in Task 1.

- [ ] **Step 7: Run the full gate and commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(worktree): report progress from the include walk

The glob and directory walk that builds an include plan runs silently,
which is half of why a large worktree_include list looks like a hang. Add
an IncludeEvent stream and a plan_includes_with that reports a running
match count and a final total; plan_includes delegates with a no-op, so
every existing caller is unchanged.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: A progress-aware copy

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` (add `apply_includes_with`, `copy_includes_with`; `apply_includes` and `copy_includes` become delegates)

**Interfaces:**
- Consumes: `IncludeEvent`, `plan_includes_with`, `IncludePlan`, `PatternPlan` from Tasks 1 and 2.
- Produces:
  - `pub fn apply_includes_with(source: &Path, dest: &Path, plan: &IncludePlan, overwrite: bool, on: &(dyn Fn(IncludeEvent) + Sync)) -> (usize, Vec<String>)`
  - `pub fn copy_includes_with(source: &Path, dest: &Path, patterns: &[String], on: &(dyn Fn(IncludeEvent) + Sync)) -> (usize, Vec<String>)`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-common/src/worktree.rs`:

```rust
/// The copy display draws one sub-step per include entry, so the copy has to
/// bracket each pattern and count within it.
#[test]
fn the_copy_brackets_each_pattern_and_counts_within_it() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    write(&src.join(".tool-versions"), "node 20");
    write(&src.join("hooks/a.sh"), "x");
    write(&src.join("hooks/b.sh"), "x");

    let patterns = [".tool-versions".to_string(), "hooks/".to_string()];
    let plan = plan_includes(&src, &dst, &patterns);

    let log = std::sync::Mutex::new(Vec::new());
    let (copied, warnings) = apply_includes_with(&src, &dst, &plan, false, &|e| match e {
        IncludeEvent::EntryStart {
            pattern,
            index,
            of,
            files,
        } => log
            .lock()
            .unwrap()
            .push(format!("start {pattern} {index}/{of} {files}")),
        IncludeEvent::FileDone { pattern, done, of } => {
            log.lock().unwrap().push(format!("file {pattern} {done}/{of}"))
        }
        IncludeEvent::EntryDone {
            pattern,
            index,
            of,
            copied,
        } => log
            .lock()
            .unwrap()
            .push(format!("done {pattern} {index}/{of} {copied}")),
        _ => {}
    });

    assert_eq!(copied, 3);
    assert!(warnings.is_empty());
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            "start .tool-versions 0/2 1",
            "file .tool-versions 1/1",
            "done .tool-versions 0/2 1",
            "start hooks/ 1/2 2",
            "file hooks/ 1/2",
            "file hooks/ 2/2",
            "done hooks/ 1/2 2",
        ]
    );
}

/// A plan is a snapshot, and the copy re-checks each destination. A file whose
/// destination appeared in the gap is skipped but still advances the display,
/// so a run that ends up writing nothing does not look stuck.
#[test]
fn a_file_skipped_since_the_plan_still_advances_the_count() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    write(&src.join(".tool-versions"), "node 20");

    let plan = plan_includes(&src, &dst, &[".tool-versions".to_string()]);
    assert_eq!(plan.missing_len(), 1, "planned before the destination existed");
    write(&dst.join(".tool-versions"), "APPEARED SINCE");

    let files = std::sync::Mutex::new(Vec::new());
    let (copied, warnings) = apply_includes_with(&src, &dst, &plan, false, &|e| {
        if let IncludeEvent::FileDone { done, of, .. } = e {
            files.lock().unwrap().push((done, of));
        }
    });

    assert_eq!(copied, 0, "the file that appeared was not clobbered");
    assert!(warnings.is_empty());
    assert_eq!(*files.lock().unwrap(), vec![(1, 1)], "it still counted");
    assert_eq!(
        fs::read_to_string(dst.join(".tool-versions")).unwrap(),
        "APPEARED SINCE"
    );
}

/// An overwrite run puts the existing files in the worklist, so they count.
#[test]
fn an_overwrite_run_counts_the_files_it_replaces() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    write(&src.join(".tool-versions"), "node 20");
    write(&dst.join(".tool-versions"), "OLD");

    let plan = plan_includes(&src, &dst, &[".tool-versions".to_string()]);
    let files = std::sync::Mutex::new(Vec::new());
    let (copied, _) = apply_includes_with(&src, &dst, &plan, true, &|e| {
        if let IncludeEvent::FileDone { done, of, .. } = e {
            files.lock().unwrap().push((done, of));
        }
    });

    assert_eq!(copied, 1);
    assert_eq!(*files.lock().unwrap(), vec![(1, 1)]);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common --lib worktree::tests::the_copy_brackets`
Expected: compile error, `cannot find function 'apply_includes_with'`.

- [ ] **Step 3: Implement `apply_includes_with`**

Add to `crates/devkit-common/src/worktree.rs`, and add `use std::sync::atomic::{AtomicUsize, Ordering};` to the file's imports:

```rust
/// [`apply_includes`], bracketing each pattern with
/// [`IncludeEvent::EntryStart`] and [`IncludeEvent::EntryDone`] and reporting
/// [`IncludeEvent::FileDone`] as each file in that pattern's worklist is
/// handled.
///
/// The counter is atomic and the callback is `Sync` so a parallel copy can
/// drive the same events from worker threads.
pub fn apply_includes_with(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> (usize, Vec<String>) {
    let mut copied = 0usize;
    let mut warnings = Vec::new();
    let of = plan.patterns.len();

    for (index, entry) in plan.patterns.iter().enumerate() {
        let worklist: Vec<&PathBuf> = if overwrite {
            entry.missing.iter().chain(entry.existing.iter()).collect()
        } else {
            entry.missing.iter().collect()
        };
        on(IncludeEvent::EntryStart {
            pattern: &entry.pattern,
            index,
            of,
            files: worklist.len(),
        });

        let before = copied;
        let done = AtomicUsize::new(0);
        for rel in &worklist {
            copy_file(
                &source.join(rel),
                &dest.join(rel),
                overwrite,
                &mut copied,
                &mut warnings,
            );
            on(IncludeEvent::FileDone {
                pattern: &entry.pattern,
                done: done.fetch_add(1, Ordering::Relaxed) + 1,
                of: worklist.len(),
            });
        }

        on(IncludeEvent::EntryDone {
            pattern: &entry.pattern,
            index,
            of,
            copied: copied - before,
        });
    }

    (copied, warnings)
}
```

- [ ] **Step 4: Make `apply_includes` and `copy_includes` delegate**

Replace their bodies, keeping their existing doc comments, and add `copy_includes_with`:

```rust
pub fn apply_includes(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
) -> (usize, Vec<String>) {
    apply_includes_with(source, dest, plan, overwrite, &|_| {})
}

pub fn copy_includes(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>) {
    copy_includes_with(source, dest, patterns, &|_| {})
}

/// [`copy_includes`], reporting the walk's and the copy's progress through
/// `on`. The plan is built first, so every [`IncludeEvent::Found`] arrives
/// before the first [`IncludeEvent::EntryStart`].
pub fn copy_includes_with(
    source: &Path,
    dest: &Path,
    patterns: &[String],
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> (usize, Vec<String>) {
    let plan = plan_includes_with(source, dest, patterns, on);
    let (copied, apply_warnings) = apply_includes_with(source, dest, &plan, false, on);
    let mut warnings = plan.warnings;
    warnings.extend(apply_warnings);
    (copied, warnings)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit-common --lib worktree`
Expected: PASS.

- [ ] **Step 6: Run the full gate and commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(worktree): report progress from the include copy

The copy is the longer of the two silences during an include backfill.
Bracket each pattern with EntryStart/EntryDone and report a per-file count
within it, so a caller can draw one sub-step per include entry. The
counter is atomic and the callback is Sync so a parallel copy can drive
the same events from workers.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: `needs_discovery`

**Files:**
- Modify: `crates/devkit-common/src/worktree.rs` (add `needs_discovery` and its private `is_glob`)

**Interfaces:**
- Consumes: nothing.
- Produces: `pub fn needs_discovery(patterns: &[String]) -> bool`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-common/src/worktree.rs`:

```rust
#[test]
fn a_wildcard_or_directory_include_wants_a_discovery_step() {
    assert!(needs_discovery(&["apps/*/.env.local".to_string()]));
    assert!(needs_discovery(&[".claude/hooks/".to_string()]));
    assert!(needs_discovery(&["conf.?".to_string()]));
    assert!(needs_discovery(&["[abc].txt".to_string()]));
    assert!(needs_discovery(&[
        ".tool-versions".to_string(),
        "hooks/".to_string()
    ]));
}

/// A list of literal file paths costs one stat each, which is not worth a
/// sub-step of its own.
#[test]
fn a_literal_include_list_wants_no_discovery_step() {
    assert!(!needs_discovery(&[
        ".tool-versions".to_string(),
        "apps/web/.env.local".to_string()
    ]));
    assert!(!needs_discovery(&[]));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common --lib worktree::tests::a_wildcard_or_directory`
Expected: compile error, `cannot find function 'needs_discovery'`.

- [ ] **Step 3: Implement**

Add to `crates/devkit-common/src/worktree.rs`:

```rust
/// Whether walking `patterns` is expensive enough to deserve its own reported
/// phase. Decided from the pattern text alone, because a caller drawing a fixed
/// number of sub-steps has to know the count before the walk starts.
///
/// A wildcard makes `glob` walk directories to expand it, and a pattern ending
/// in `/` is a directory include, which walks recursively. A literal file path
/// costs one stat.
pub fn needs_discovery(patterns: &[String]) -> bool {
    patterns.iter().any(|p| p.ends_with('/') || is_glob(p))
}

/// `glob` treats all three of these as wildcards, so checking only `*` would
/// miss a pattern that is just as expensive to expand.
fn is_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit-common --lib worktree::tests::a_wildcard_or_directory worktree::tests::a_literal_include_list`
Expected: PASS.

- [ ] **Step 5: Run the full gate and commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/devkit-common/src/worktree.rs
git commit -m "feat(worktree): tell cheap includes from expensive ones

A caller drawing a fixed number of sub-steps has to know the count before
the walk starts, so the decision cannot come from what the patterns match.
Read it off the pattern text: a wildcard or a trailing slash means a
directory walk, a literal path means one stat.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: A step that owns its bar

**Files:**
- Modify: `crates/devkit-common/src/progress.rs:148-235` (`during`, `during_result`, `settle`, `finish_line`; add `Step`, `during_step`, `during_step_result`)

**Interfaces:**
- Consumes: `Steps`, `add_spinner`, `finish_line` (existing).
- Produces:
  - `pub struct Step<'a>` with `pub fn activity(&self, msg: &str)`, `pub fn substep(&self, msg: &str)`, `pub fn detail(&self, d: &str)`
  - `pub fn during_step<T>(&self, msg: &str, f: impl FnOnce(&Step<'_>) -> T) -> T`
  - `pub fn during_step_result<T>(&self, msg: &str, f: impl FnOnce(&Step<'_>) -> anyhow::Result<T>) -> anyhow::Result<T>`
  - `during` and `during_result` keep their signatures and their output.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/devkit-common/src/progress.rs`:

```rust
/// A step that draws sub-progress still consumes exactly one ordinal, so a
/// numbered run does not end short of its total.
#[test]
fn a_step_with_sub_progress_consumes_one_ordinal() {
    let steps = Steps::persistent_with_total(2);
    steps.during_step("first", |step| {
        step.activity("working");
        step.substep("1/2 one");
        step.substep("2/2 two");
        step.detail("2 things");
    });
    steps.during("second", || {});
    assert_eq!(steps.started(), 2);
}

/// The handle's methods take &self so a Fn callback can drive them, which is
/// what a parallel producer needs.
#[test]
fn a_step_handle_is_usable_from_a_shared_reference() {
    fn assert_sync<T: Sync>(_: &T) {}
    let steps = Steps::persistent();
    steps.during_step("first", |step| {
        assert_sync(step);
        let emit: &(dyn Fn(&str) + Sync) = &|m| step.activity(m);
        emit("from a shared reference");
    });
    assert_eq!(steps.started(), 1);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-common --lib progress::tests::a_step_with_sub_progress`
Expected: compile error, `no method named 'during_step' found`.

- [ ] **Step 3: Add the `Step` handle**

Add to `crates/devkit-common/src/progress.rs`, and add `use std::sync::Mutex;` to the file's imports:

```rust
/// A running step that draws sub-progress. Its transient line is rewritten in
/// place and never persists; finished sub-steps persist as their own indented
/// lines. Every method takes `&self` so a shared callback can drive them.
pub struct Step<'a> {
    steps: &'a Steps,
    activity: Mutex<Option<ProgressBar>>,
    detail: Mutex<Option<String>>,
}

impl Step<'_> {
    /// Rewrite the transient line beneath this step, creating it on first call.
    pub fn activity(&self, msg: &str) {
        let mut slot = self.activity.lock().expect("step activity bar");
        match slot.as_ref() {
            Some(pb) => pb.set_message(format!("  {msg}")),
            None => *slot = Some(add_spinner(&self.steps.mp, &format!("  {msg}"))),
        }
    }

    /// Persist a finished sub-step line beneath this step and clear the
    /// transient line, so the next sub-step starts from an empty slot.
    ///
    /// `MultiProgress` prints above its live bars and this step's bar is still
    /// live, so sub-steps land above the step's own settled line.
    pub fn substep(&self, msg: &str) {
        self.clear_activity();
        let paint = crate::ui::Paint::on(crate::ui::Stream::Stderr);
        let _ = self
            .steps
            .mp
            .println(format!("     {} {msg}", paint.green("✓")));
    }

    /// Text folded into the settled line's parens, ahead of the elapsed time.
    pub fn detail(&self, d: &str) {
        *self.detail.lock().expect("step detail") = Some(d.to_string());
    }

    fn clear_activity(&self) {
        if let Some(pb) = self.activity.lock().expect("step activity bar").take() {
            pb.finish_and_clear();
        }
    }
}
```

- [ ] **Step 4: Add `during_step` and rebuild the existing step methods on it**

Replace `during`, `during_result`, and the private `settle` in `impl Steps`
with the following. `run_step` absorbs what `settle` did, so `settle` goes.

```rust
    /// Run `f` under a spinner (auto-numbered in numbered mode). Transient
    /// mode clears the bar before returning, so the spinner never stays live
    /// across a `?`, a stdin prompt, or stdout output. Persistent mode prints
    /// the settled line into scrollback instead; the bar itself is still
    /// cleared, so no bar is ever active across a prompt in either mode.
    ///
    /// Every completion counts as success here. A closure returning
    /// `anyhow::Result` belongs in [`Steps::during_result`], which marks the
    /// step failed on error instead of logging a failure as succeeded.
    pub fn during<T>(&self, msg: &str, f: impl FnOnce() -> T) -> T {
        self.during_step(msg, |_| f())
    }

    /// [`Steps::during`] for fallible steps: in persistent mode the settled
    /// line is marked failed when the closure errors, so the failed step stays
    /// identifiable in the log.
    pub fn during_result<T>(
        &self,
        msg: &str,
        f: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.during_step_result(msg, |_| f())
    }

    /// [`Steps::during`] for a step that draws its own sub-progress. The
    /// closure gets a [`Step`] handle for a transient line, persisted
    /// sub-steps, and a detail folded into the settled line.
    pub fn during_step<T>(&self, msg: &str, f: impl FnOnce(&Step<'_>) -> T) -> T {
        self.run_step(msg, |step| (f(step), true))
    }

    /// [`Steps::during_step`] for fallible steps.
    pub fn during_step_result<T>(
        &self,
        msg: &str,
        f: impl FnOnce(&Step<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        self.run_step(msg, |step| {
            let out = f(step);
            let ok = out.is_ok();
            (out, ok)
        })
    }

    /// Mint the ordinal, draw the step's spinner, run `f`, then settle. `f`
    /// returns its value and whether the step succeeded.
    fn run_step<T>(&self, msg: &str, f: impl FnOnce(&Step<'_>) -> (T, bool)) -> T {
        let label = self.label(msg);
        let pb = self.spinner(&label);
        let step = Step {
            steps: self,
            activity: Mutex::new(None),
            detail: Mutex::new(None),
        };
        let (out, ok) = f(&step);
        step.clear_activity();
        let detail = step.detail.lock().expect("step detail").take();
        let line = self
            .persist
            .then(|| finish_line(ok, &label, pb.elapsed(), detail.as_deref()));
        pb.finish_and_clear();
        if let Some(line) = line {
            let _ = self.mp.println(line);
        }
        out
    }
```

- [ ] **Step 5: Give `finish_line` an optional detail**

Replace `finish_line` in `crates/devkit-common/src/progress.rs`:

```rust
/// The persistent line a settled step leaves behind. It prints on stderr, so
/// the mark's colour keys off that stream, not stdout. `detail` rides inside
/// the same parens as the elapsed time rather than adding a separator.
fn finish_line(ok: bool, label: &str, elapsed: Duration, detail: Option<&str>) -> String {
    let paint = crate::ui::Paint::on(crate::ui::Stream::Stderr);
    let mark = if ok {
        paint.green("✓")
    } else {
        paint.red("✗")
    };
    match detail {
        Some(d) => format!("{mark} {label} ({d}, {})", fmt_elapsed(elapsed)),
        None => format!("{mark} {label} ({})", fmt_elapsed(elapsed)),
    }
}
```

The file spells the marks as literal characters, not escapes. Leave them as
they are; only the signature and the final `match` change.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit-common --lib progress`
Expected: PASS, including the two pre-existing `steps.started()` tests, which
prove `during` and `during_result` still mint exactly one ordinal each.

- [ ] **Step 7: Run the full gate and commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add crates/devkit-common/src/progress.rs
git commit -m "feat(progress): let a step draw its own sub-progress

during and during_result build their spinner internally, so nothing can
touch it mid-step. Add a Step handle with a transient line, persisted
sub-steps, and a detail folded into the settled line, and rebuild both
existing methods on it so their output is unchanged.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 6: The include backfill draws its step

**Files:**
- Modify: `src/bin/devkit/issue/setup.rs:209-220` (`backfill_includes`), `:378` (`total`), `:436` (the call)
- Modify: `src/bin/devkit/issue/checkout.rs:430` (the call)
- Test: `src/bin/devkit/issue/setup.rs` (its existing `tests` module)

**Interfaces:**
- Consumes: `IncludeEvent`, `copy_includes_with`, `needs_discovery` from Tasks 2 to 4; `Steps::during_step` and `Step` from Task 5.
- Produces: `pub fn backfill_includes(primary: &str, worktree: &Path, patterns: &[String], steps: &Steps)`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/bin/devkit/issue/setup.rs`:

```rust
/// The backfill is a step of the run like any other, so a caller's `[i/N]`
/// numbering has to account for it.
#[test]
fn the_backfill_consumes_one_step() {
    let base = tempfile::tempdir().unwrap();
    let src = base.path().join("src");
    let dst = base.path().join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join(".tool-versions"), "node 20").unwrap();

    let steps = Steps::persistent_with_total(1);
    backfill_includes(
        src.to_str().unwrap(),
        &dst,
        &[".tool-versions".to_string()],
        &steps,
    );

    assert_eq!(steps.started(), 1);
    assert!(dst.join(".tool-versions").exists(), "the file was copied");
}

/// An empty include list is not work, so it must not draw a step or the run
/// ends one short of its total.
#[test]
fn an_empty_include_list_consumes_no_step() {
    let base = tempfile::tempdir().unwrap();
    let steps = Steps::persistent_with_total(0);

    backfill_includes(base.path().to_str().unwrap(), base.path(), &[], &steps);

    assert_eq!(steps.started(), 0);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --bin devkit issue::setup::tests::the_backfill_consumes_one_step`
Expected: compile error, `this function takes 3 arguments but 4 arguments were supplied`.

- [ ] **Step 3: Add the event renderer**

Add to `src/bin/devkit/issue/setup.rs`, above `backfill_includes`:

```rust
/// How many files may pass before the transient line is redrawn. A large
/// include list fires one event per file, and every redraw allocates a message.
const INCLUDE_REDRAW_EVERY: usize = 64;

/// Draws an include backfill's events as sub-steps of one `Step`. Sub-step
/// numbers are a display concern, so the offset the discovery sub-step
/// introduces is applied here rather than in the event stream.
///
/// The counters are atomic and every method takes `&self`, because the copy
/// may report from more than one thread.
struct IncludeRender<'a> {
    step: &'a devkit_common::progress::Step<'a>,
    discovery: bool,
    subs: usize,
    entry: std::sync::atomic::AtomicUsize,
    drawn: std::sync::atomic::AtomicUsize,
}

impl IncludeRender<'_> {
    fn on(&self, event: devkit_common::worktree::IncludeEvent<'_>) {
        use devkit_common::worktree::IncludeEvent as E;
        use std::sync::atomic::Ordering::Relaxed;
        match event {
            E::Found { files } => {
                if self.discovery && self.due(files) {
                    self.step.activity(&format!(
                        "[1/{}] discovering files\u{2026} ({files} found)",
                        self.subs
                    ));
                }
            }
            E::ScanDone { files } => {
                if self.discovery {
                    self.step
                        .substep(&format!("1/{} discovering files ({files} found)", self.subs));
                }
            }
            E::EntryStart {
                pattern,
                index,
                files,
                ..
            } => {
                self.entry.store(self.number(index), Relaxed);
                self.drawn.store(0, Relaxed);
                self.step.activity(&format!(
                    "[{}/{}] {pattern} 0/{files}",
                    self.number(index),
                    self.subs
                ));
            }
            E::FileDone { pattern, done, of } => {
                if self.due(done) || done == of {
                    self.step.activity(&format!(
                        "[{}/{}] {pattern} {done}/{of}",
                        self.entry.load(Relaxed),
                        self.subs
                    ));
                }
            }
            E::EntryDone { pattern, index, .. } => {
                self.step
                    .substep(&format!("{}/{} {pattern}", self.number(index), self.subs));
            }
        }
    }

    /// Whether `count` has moved far enough since the last redraw to be worth
    /// another one.
    fn due(&self, count: usize) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        if count.saturating_sub(self.drawn.load(Relaxed)) < INCLUDE_REDRAW_EVERY {
            return false;
        }
        self.drawn.store(count, Relaxed);
        true
    }

    /// A pattern's display number, offset past the discovery sub-step when
    /// there is one.
    fn number(&self, index: usize) -> usize {
        index + 1 + usize::from(self.discovery)
    }
}
```

- [ ] **Step 4: Rewrite `backfill_includes`**

Replace it in `src/bin/devkit/issue/setup.rs`:

```rust
/// Copy the configured `worktree_include` globs from the primary checkout into
/// a freshly created worktree under a step of its own, one sub-step per include
/// entry. Fail-open warnings print to stderr after the step settles, so a live
/// bar cannot tear them. A no-op that draws nothing when the include list is
/// empty.
pub fn backfill_includes(
    primary: &str,
    worktree: &std::path::Path,
    patterns: &[String],
    steps: &Steps,
) {
    if patterns.is_empty() {
        return;
    }
    let discovery = devkit_common::worktree::needs_discovery(patterns);
    let subs = patterns.len() + usize::from(discovery);

    let warnings = steps.during_step("Copying worktree includes\u{2026}", |step| {
        let render = IncludeRender {
            step,
            discovery,
            subs,
            entry: std::sync::atomic::AtomicUsize::new(0),
            drawn: std::sync::atomic::AtomicUsize::new(0),
        };
        let (copied, warnings) = devkit_common::worktree::copy_includes_with(
            std::path::Path::new(primary),
            worktree,
            patterns,
            &|e| render.on(e),
        );
        step.detail(&format!("{copied} files"));
        warnings
    });

    for w in warnings {
        eprintln!("warning: {w}");
    }
}
```

- [ ] **Step 5: Update both call sites and the step total**

In `src/bin/devkit/issue/setup.rs`, the `total` line becomes:

```rust
    let total = 2
        + usize::from(!args.apps.is_empty())
        + cfg.hooks.after_worktree_create.len()
        + usize::from(!cfg.defaults.worktree_include.is_empty());
```

and the call becomes:

```rust
    backfill_includes(
        primary_s,
        &worktree,
        &cfg.defaults.worktree_include,
        &steps,
    );
```

In `src/bin/devkit/issue/checkout.rs`, the call becomes:

```rust
    crate::issue::setup::backfill_includes(
        primary_s,
        &worktree,
        &cfg.defaults.worktree_include,
        &steps,
    );
```

`checkout.rs` uses the unnumbered `Steps::persistent()`, so it needs no total
change.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --bin devkit issue::setup`
Expected: PASS, including `every_hook_consumes_a_step_even_when_it_cannot_render`.

- [ ] **Step 7: Run the full gate and commit**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
git add src/bin/devkit/issue/setup.rs src/bin/devkit/issue/checkout.rs
git commit -m "feat(issue): show progress while copying worktree includes

The worktree_include backfill ran between the record write and per-app
prep without drawing anything, so a large include list looked like a hang
for the length of the walk and the copy. Give it a step of its own with
one sub-step per include entry, a per-file line inside each, and a leading
discovery sub-step when the pattern list warrants one.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 7: See it run

**Files:** none. This is a manual check, not a code change.

**Interfaces:**
- Consumes: everything from Tasks 1 to 6.
- Produces: confirmation, or a defect report.

- [ ] **Step 1: Build**

```bash
cargo build --release
```

- [ ] **Step 2: Run a setup against a project with a large include list**

Ask Lev which project to point it at, and run `issue setup` there with
`--dry-run` first to confirm the target, then for real. Watch for:

- the step numbering ending at its total, not one short
- a transient sub-line that visibly moves during both the walk and the copy
- one persisted sub-step per `worktree_include` entry
- the parent's settled line carrying the file count
- warnings printing after the step settles, not torn across a live bar

- [ ] **Step 3: Confirm the non-TTY path stays silent**

```bash
cargo run --release --bin devkit -- issue setup <issue> --dry-run 2>&1 | cat
```

Expected: no bar output, no stray indentation. The pipe hides the whole group.

- [ ] **Step 4: Tell `parallel_includes` the branch has landed**

Send it a message: the branch is in, `IncludePlan` is now per-pattern,
`apply_includes` iterates `plan.patterns`, and `apply_includes_with` is where
its parallel copy goes.

---

## Notes for the executor

**The sub-step ordering is deliberate.** Persisted sub-steps land above the
parent step's settled line, because `MultiProgress::println` prints above live
bars and the parent's bar is still live while its sub-steps complete. Do not
try to buffer sub-steps to make them print underneath; that gives up the live
record of which entries are already done, which is the point.

**Do not add progress to `issue sync-includes`.** It prints its own per-worktree
report and prompts on stdin. Mixing live bars into that is a separate design.

**Do not touch `plan_dir`'s `child.is_dir()`.** Swapping it for
`entry.file_type()` looks like a free saved stat but changes behaviour:
`DirEntry::file_type` does not traverse symlinks and `Path::is_dir` does, so
the swap stops recursing into a symlinked directory inside an include.

**If a step's code and this plan disagree, the spec wins.** The plan's code is
written against `main` at `141cc4c`; if the file has moved under you, adapt and
say so rather than forcing the literal text in.
