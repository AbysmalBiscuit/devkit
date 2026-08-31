use crate::git::{self, Worktree};
use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// This worktree's issue id. The setup record is authoritative because it holds
/// whatever the tracker actually calls the issue; the branch and directory scan
/// is the fallback that keeps worktrees made without a record — by a plain
/// `git worktree add`, say — working.
pub fn issue_id_of(worktree: &std::path::Path, branch: &str) -> String {
    if let Some(rec) = crate::record::read(worktree)
        && !rec.issue.is_empty()
    {
        return rec.issue;
    }
    let dir = worktree.file_name().and_then(|s| s.to_str()).unwrap_or("");
    for src in [branch, dir] {
        if let Some(m) = find_id(src) {
            return m.to_uppercase();
        }
    }
    "UNKNOWN".into()
}

/// The first letters-dash-digits run in `s` (e.g. `eng-1234`), if any. A
/// `pr-<number>` run is the PR-checkout number marker, not an issue id, so it is
/// skipped in favour of a real id later in the string (e.g. the trailing
/// `swe-8603` in `pr-3255-…-swe-8603`).
pub fn find_id(s: &str) -> Option<String> {
    // first run of letters-dash-digits, e.g. eng-1234
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_alphabetic() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'-' {
                let dash = i;
                i += 1;
                let ds = i;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i > ds {
                    let key = &s[start..dash];
                    if key.eq_ignore_ascii_case("pr") {
                        continue; // PR-number marker, keep scanning for the id
                    }
                    return Some(format!("{}-{}", key, &s[ds..i]));
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// (main_repo_path, other_worktrees) from a path inside any worktree.
pub fn discover(start: &str) -> Result<(PathBuf, Vec<Worktree>)> {
    let mut all = git::worktrees(Path::new(start))?.into_iter();
    let main = all.next().expect("git never lists zero worktrees");
    Ok((main.path, all.collect()))
}

/// Copy files matching `patterns` (path globs relative to `source`) into `dest`
/// at the same relative path. A match that is a directory is copied recursively.
/// Patterns that match nothing are silently skipped; a destination file that
/// already exists is left untouched (never clobbered). Fail-open: a glob or copy
/// error is collected as a warning string rather than propagated, so backfill
/// never aborts worktree creation. A match that is a symlink is reproduced as a
/// symlink pointing at the same target; its contents are not copied. Returns
/// (files_copied, links_created, warnings).
pub fn copy_includes(
    source: &Path,
    dest: &Path,
    patterns: &[String],
) -> (usize, usize, Vec<String>) {
    copy_includes_with(source, dest, patterns, &|_| {})
}

/// Whether a pattern could read or write outside its roots. `plan_includes`
/// strips `source` lexically, so a `..` component survives into the path joined
/// onto the destination and escapes both; an absolute or root-relative pattern
/// replaces the base in `Path::join` outright. `has_root` catches `/etc/x` on
/// Windows, where `is_absolute` is false but `join` still discards the base.
fn escapes(pattern: &str) -> bool {
    let p = Path::new(pattern);
    p.is_absolute() || p.has_root() || p.components().any(|c| matches!(c, Component::ParentDir))
}

/// Copy files matching `patterns` (globs relative to `source`) out of a worktree
/// into `dest`, at the same relative path, replacing what is already there. The
/// outbound counterpart to `copy_includes`, fail-open the same way: returns
/// (files_copied, warnings). A pattern that would leave either root is skipped
/// with a warning, because the caller deletes `source` immediately afterward.
/// Symlinks are followed and archived as their target's content — the outbound
/// direction deliberately differs from the inbound one, which reproduces the
/// link.
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
    let plan = plan_with_mode(source, dest, &inside, &|_| {}, LinkMode::Follow);
    // A Follow-mode plan holds no links, so the count is always zero.
    let (copied, _, apply_warnings) = apply_includes(source, dest, &plan, true);
    warnings.extend(plan.warnings);
    warnings.extend(apply_warnings);
    (copied, warnings)
}

/// Copy the files an `IncludePlan` found: every path in a pattern's `missing`,
/// and every path in its `existing` only when `overwrite` is true. Patterns are
/// applied in configuration order. A plan is a snapshot, so unless `overwrite`
/// is set each copy re-checks the destination and skips a file that appeared
/// since. Fail-open, like `plan_includes`: a copy error is collected as a
/// warning string rather than propagated. Returns (files_copied, links_created,
/// warnings).
pub fn apply_includes(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
) -> (usize, usize, Vec<String>) {
    apply_includes_with(source, dest, plan, overwrite, &|_| {})
}

/// [`apply_includes`], bracketing each pattern with [`IncludeEvent::EntryStart`]
/// and [`IncludeEvent::EntryDone`] and reporting [`IncludeEvent::FileDone`] as
/// each file and link belonging to that pattern is handled.
///
/// The callback is `Sync` because `copy_includes_with` hands the same `on`
/// reference to both the walk and the copy.
pub fn apply_includes_with(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> (usize, usize, Vec<String>) {
    let copied = AtomicUsize::new(0);
    let linked = AtomicUsize::new(0);
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

        let before = copied.load(Ordering::Relaxed);
        let done = AtomicUsize::new(0);
        let entry_warnings = Mutex::new(Vec::new());
        let bump = |done: &AtomicUsize| IncludeEvent::FileDone {
            pattern: &entry.pattern,
            done: done.fetch_add(1, Ordering::Relaxed) + 1,
            of: total,
        };

        // Files and links run as two phases rather than one mixed worklist:
        // link creation is the failure-prone path on Windows, and keeping it
        // apart keeps its counter and its warnings separable.
        crate::pool::install(|| {
            worklist.par_iter().for_each(|rel| {
                copy_file(
                    &source.join(rel),
                    &dest.join(rel),
                    overwrite,
                    &copied,
                    &entry_warnings,
                );
                on(bump(&done));
            });
            entry.links.par_iter().for_each(|(rel, target)| {
                make_link(
                    &source.join(rel),
                    &dest.join(rel),
                    target,
                    overwrite,
                    &linked,
                    &entry_warnings,
                );
                on(bump(&done));
            });
        });

        // Sorted within the pattern, and patterns keep configuration order, so
        // two runs over one tree report warnings identically.
        let mut w = entry_warnings
            .into_inner()
            .unwrap_or_else(|e| e.into_inner());
        w.sort();
        warnings.extend(w);

        on(IncludeEvent::EntryDone {
            pattern: &entry.pattern,
            index,
            of,
            copied: copied.load(Ordering::Relaxed) - before,
        });
    }

    (copied.into_inner(), linked.into_inner(), warnings)
}

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
    /// The copy started for `pattern`, with `files` counting every file and
    /// link it will handle.
    EntryStart {
        pattern: &'a str,
        index: usize,
        of: usize,
        files: usize,
    },
    /// One file or link of `pattern` is handled: copied or linked, skipped
    /// because the destination already existed, or failed. `done` and `of`
    /// count within that pattern and may arrive out of order.
    FileDone {
        pattern: &'a str,
        done: usize,
        of: usize,
    },
    /// The copy finished for `pattern`. `copied` counts only the files this
    /// pattern wrote, excluding any links it reproduced, so it is not the
    /// pattern's total unit count; that total is the `files` this pattern's
    /// `EntryStart` reported.
    EntryDone {
        pattern: &'a str,
        index: usize,
        of: usize,
        copied: usize,
    },
}

/// Every file one `worktree_include` pattern matched, split by whether `dest`
/// already has it, plus every symlink it matched paired with the target that
/// link holds. Both vectors come back sorted, so rendering does not vary with
/// filesystem iteration order.
pub struct PatternPlan {
    pub pattern: String,
    pub missing: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
    /// (path relative to `dest`, target exactly as the source link holds it).
    /// A link's own contents are never planned, so a symlinked directory
    /// contributes one entry here and nothing to `missing`.
    pub links: Vec<(PathBuf, PathBuf)>,
}

/// The result of walking `patterns` without copying anything, kept grouped by
/// the pattern each match came from. Paths are relative to `dest`. Use
/// [`IncludePlan::missing`], [`IncludePlan::existing`], and
/// [`IncludePlan::links`] for a flat view.
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

    /// Total count of matches `dest` does not have, across every pattern.
    pub fn missing_len(&self) -> usize {
        self.patterns.iter().map(|p| p.missing.len()).sum()
    }

    /// Total count of matches `dest` already has, across every pattern.
    pub fn existing_len(&self) -> usize {
        self.patterns.iter().map(|p| p.existing.len()).sum()
    }

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
}

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

/// The callback and running match count a plan walk carries through its
/// recursion. `Cell` suffices because a walk runs on one thread. The `Sync`
/// bound on the callback is wider than this walk needs, so one renderer can
/// serve both the walk and a threaded copy.
struct Walk<'a> {
    dest: &'a Path,
    on: &'a (dyn Fn(IncludeEvent) + Sync),
    found: std::cell::Cell<usize>,
    mode: LinkMode,
    /// Records the pool-thread index each directory read ran on, so a test
    /// can observe real thread participation in a real call instead of
    /// reading the parallelism wiring in isolation. `None` in every real
    /// call; only a test constructs one with this set. `Arc`, not a
    /// borrow: `process_read_dir` requires its closure to be `'static`,
    /// which an owned handle satisfies independent of `Walk`'s own `'a`.
    #[cfg(test)]
    read_threads:
        Option<std::sync::Arc<std::sync::Mutex<std::collections::HashSet<Option<usize>>>>>,
}

impl Walk<'_> {
    /// Record a classified file. Runs on the calling thread, so `found` needs
    /// no synchronisation and `Found` still arrives in one order.
    fn record_file(&self, rel: PathBuf, exists: bool, out: &mut PatternPlan) {
        if exists {
            out.existing.push(rel);
        } else {
            out.missing.push(rel);
        }
        self.found.set(self.found.get() + 1);
        (self.on)(IncludeEvent::Found {
            files: self.found.get(),
        });
    }

    /// Record a source symlink and the target it holds, and count it as a
    /// match. Every matched link lands here whatever the destination holds:
    /// routing an occupied one into `existing` would hand it to `copy_file`
    /// under `overwrite`, which writes the target's contents instead of
    /// reproducing the link. `make_link` decides per link whether to skip or
    /// replace.
    fn record_link(&self, rel: PathBuf, target: PathBuf, out: &mut PatternPlan) {
        out.links.push((rel, target));
        self.found.set(self.found.get() + 1);
        (self.on)(IncludeEvent::Found {
            files: self.found.get(),
        });
    }

    /// Walk from `start` and classify what `matcher` claims, or everything
    /// under it when `matcher` is `None`, which is what a directory match asks
    /// for. Paths are recorded relative to `source`.
    ///
    /// Three passes, in order and for a reason. jwalk's callbacks are `'static`
    /// while `dest` and the event callback are borrowed, so classification
    /// cannot run in jwalk's workers; and draining through `par_bridge` would
    /// put consumers on the same threads the readers need, starving the walk.
    /// So: the calling thread drains, which costs no syscalls of its own
    /// because `file_type` comes cached from the directory read; the pool
    /// classifies, because `dest.join(rel).exists()` is a syscall per candidate
    /// and around 75µs on a drive mounted under WSL; the calling thread
    /// records.
    ///
    /// Two jwalk defaults are wrong here and both would fail quietly.
    /// `skip_hidden` is true, and every include devkit exists for is a dotfile.
    /// `parallelism` is rayon's global pool, which is the one the shared pool
    /// exists to avoid.
    fn walk_and_classify(
        &self,
        source: &Path,
        start: &Path,
        matcher: Option<&glob::Pattern>,
        opts: glob::MatchOptions,
        out: &mut PatternPlan,
        warnings: &mut Vec<String>,
    ) {
        let mode = self.mode;
        let dest = self.dest;
        // Owned copies for the `'static` callback. Cloning a compiled pattern
        // once per pattern is not a cost worth avoiding.
        let pruner = matcher.cloned();
        let prune_source = source.to_path_buf();
        // Built once per pattern rather than per directory. `None` when this
        // walk has no matcher (a directory match's own recursion), where
        // every path is claimed and nothing may be pruned.
        let prune_plan = pruner.as_ref().map(|p| Prune::for_pattern(p.as_str()));
        #[cfg(test)]
        let read_threads = self.read_threads.clone();

        let entries: Vec<_> = jwalk::WalkDir::new(start)
            .skip_hidden(false)
            .follow_links(true)
            .parallelism(crate::pool::jwalk_parallelism())
            .process_read_dir(move |_depth, dir, _state, children| {
                // Test-only: records which pool thread performed this read,
                // so a test can observe real parallel dispatch. No-op
                // (`read_threads` is always `None`) outside tests.
                #[cfg(test)]
                if let Some(rt) = &read_threads {
                    rt.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(rayon::current_thread_index());
                }
                for child in children.iter_mut().flatten() {
                    let full = dir.join(&child.file_name);
                    let Ok(rel) = full.strip_prefix(&prune_source) else {
                        continue;
                    };
                    // A link that is claimed becomes a link in the plan,
                    // so the walk must not read through it. A link that
                    // is claimed by nothing is traversed: it may be the
                    // only road to a file that is, and `glob_with` used
                    // to read through one the same way.
                    if mode == LinkMode::Preserve && child.path_is_symlink() {
                        let claimed = match &pruner {
                            Some(p) => matches_here(p, rel, opts),
                            None => true,
                        };
                        if claimed {
                            child.read_children = None;
                            continue;
                        }
                    }
                    // No pattern in play (a directory match's own
                    // recursion) claims everything, so there is nothing
                    // to bound the walk by.
                    if let (Some(plan), Some(p)) = (&prune_plan, &pruner)
                        && plan.should_prune(p, rel, opts)
                    {
                        child.read_children = None;
                    }
                }
            })
            .into_iter()
            .collect();

        let verdicts: Vec<Classified> = crate::pool::install(|| {
            entries
                .par_iter()
                .filter_map(|entry| {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(e) => {
                            // `follow_links` stats a symlink's target to
                            // decide whether to recurse into it, so a
                            // symlink whose target cannot be stat'd (broken,
                            // permission-denied, or a jwalk-detected loop
                            // back to an ancestor) never reaches this
                            // `filter_map` as an `Ok` entry with
                            // `path_is_symlink()` set. Recovering it here,
                            // from the failed entry's own path, is the only
                            // way such a link is ever classified.
                            if let Some(full) = e.path()
                                && is_symlink(full)
                            {
                                // Depth 0 is `start` itself, which the
                                // caller has already accounted for.
                                if e.depth() == 0 {
                                    return None;
                                }
                                let Ok(rel) = full.strip_prefix(source) else {
                                    return Some(Classified::Warning(format!(
                                        "match outside source: {}",
                                        full.display()
                                    )));
                                };
                                if let Some(m) = matcher
                                    && !matches_here(m, rel, opts)
                                {
                                    return None;
                                }
                                let rel = rel.to_path_buf();
                                return Some(match (mode, std::fs::read_link(full)) {
                                    (LinkMode::Preserve, Ok(target)) => {
                                        Classified::Link { rel, target }
                                    }
                                    // `copy_out` runs just before the source
                                    // worktree is deleted, so a link this
                                    // mode cannot follow has to be named now
                                    // or it is never archived and never
                                    // reported.
                                    (LinkMode::Follow, Ok(target)) => Classified::Warning(format!(
                                        "not archiving {}: target {} could not be resolved",
                                        full.display(),
                                        target.display()
                                    )),
                                    (_, Err(e)) => Classified::Warning(format!(
                                        "reading link {}: {e}",
                                        full.display()
                                    )),
                                });
                            }
                            // The directory read raced a deletion: the
                            // entry jwalk listed is already gone by the
                            // time it stats it.
                            if e.io_error().map(std::io::Error::kind)
                                == Some(std::io::ErrorKind::NotFound)
                            {
                                return None;
                            }
                            return Some(Classified::Warning(format!(
                                "reading {}: {e}",
                                e.path().unwrap_or(start).display()
                            )));
                        }
                    };
                    // Depth 0 is `start` itself, which the caller has already
                    // accounted for.
                    if entry.depth() == 0 {
                        return None;
                    }
                    let full = entry.path();
                    let Ok(rel) = full.strip_prefix(source) else {
                        return Some(Classified::Warning(format!(
                            "match outside source: {}",
                            full.display()
                        )));
                    };
                    if let Some(m) = matcher
                        && !matches_here(m, rel, opts)
                    {
                        return None;
                    }
                    let rel = rel.to_path_buf();
                    if mode == LinkMode::Preserve && entry.path_is_symlink() {
                        return Some(match std::fs::read_link(&full) {
                            Ok(target) => Classified::Link { rel, target },
                            Err(e) => {
                                Classified::Warning(format!("reading link {}: {e}", full.display()))
                            }
                        });
                    }
                    // A directory is never an entry in the plan; the files
                    // under it are, and the walk reaches them itself.
                    if entry.file_type().is_dir() {
                        return None;
                    }
                    Some(Classified::File {
                        exists: dest.join(&rel).exists(),
                        rel,
                    })
                })
                .collect()
        });

        for verdict in verdicts {
            match verdict {
                Classified::File { rel, exists } => self.record_file(rel, exists, out),
                Classified::Link { rel, target } => self.record_link(rel, target, out),
                Classified::Warning(w) => warnings.push(w),
            }
        }
    }

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
        // A leading `./` names the same path as the pattern without it, but
        // left in it survives into `walk_root`'s literal prefix and into the
        // compiled pattern, where it stops the two from agreeing on what a
        // component is: `walk_root("./apps/*")` stops at `.` and matches
        // nothing ever after.
        let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
        // An empty pattern would match the source directory itself and plan
        // every file under the root.
        if trimmed.is_empty() {
            return;
        }
        // `source.join` discards the base for a rooted pattern and keeps a `..`
        // component, so a pattern that leaves the tree is refused before any
        // path is built from it.
        if escapes(trimmed) {
            warnings.push(format!(
                "include pattern reaches outside the source tree, skipped: {pattern}"
            ));
            return;
        }
        let Some(root) = walk_root(trimmed) else {
            self.plan_literal(source, Path::new(trimmed), out, warnings);
            return;
        };
        let matcher = match glob::Pattern::new(trimmed) {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("bad include pattern `{pattern}`: {e}"));
                return;
            }
        };
        let start = source.join(&root);
        self.walk_and_classify(source, &start, Some(&matcher), opts, out, warnings);
    }

    /// A pattern with no wildcard names exactly one path, so it costs one stat
    /// rather than a walk. Mode decides what a link means: `Preserve` records
    /// the link, `Follow` resolves through it, which is why `copy_out` can
    /// promise that a Follow-mode plan holds none.
    fn plan_literal(
        &self,
        source: &Path,
        rel: &Path,
        out: &mut PatternPlan,
        warnings: &mut Vec<String>,
    ) {
        let full = source.join(rel);
        // A glob over a missing path yields nothing rather than an error, and
        // a literal pattern matching nothing behaves the same way.
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            return;
        };
        let opts = match_options();
        if meta.file_type().is_symlink() {
            if self.mode == LinkMode::Preserve {
                match std::fs::read_link(&full) {
                    Ok(target) => self.record_link(rel.to_path_buf(), target, out),
                    Err(e) => warnings.push(format!("reading link {}: {e}", full.display())),
                }
                return;
            }
            match std::fs::metadata(&full) {
                Ok(target) if target.is_dir() => {
                    self.walk_and_classify(source, &full, None, opts, out, warnings);
                }
                Ok(_) => {
                    let exists = self.dest.join(rel).exists();
                    self.record_file(rel.to_path_buf(), exists, out);
                }
                // The link itself read fine, but its target could not be
                // resolved; matches the wording `walk_and_classify` uses for
                // the same failure reached through a wildcard pattern.
                Err(_) => match std::fs::read_link(&full) {
                    Ok(target) => warnings.push(format!(
                        "not archiving {}: target {} could not be resolved",
                        full.display(),
                        target.display()
                    )),
                    Err(e) => warnings.push(format!("reading link {}: {e}", full.display())),
                },
            }
            return;
        }
        if meta.is_dir() {
            self.walk_and_classify(source, &full, None, opts, out, warnings);
        } else {
            let exists = self.dest.join(rel).exists();
            self.record_file(rel.to_path_buf(), exists, out);
        }
    }
}

/// Walk `patterns` the way `copy_includes` does, but classify matches instead
/// of copying them: each matched file lands in its pattern's `missing` or
/// `existing` depending on whether `dest` already has it, and each matched
/// symlink lands in its pattern's `links` instead, paired with the target it
/// holds. A directory match contributes its files recursively, except a
/// symlinked directory, which contributes exactly the directory entry itself,
/// as a link. Every configured pattern keeps an entry, and a file matched by
/// two patterns is planned once, under the first of them in configuration
/// order, so a caller's rendering does not vary with filesystem iteration
/// order. Fail-open, like `copy_includes`: a bad glob or an unreadable
/// directory becomes a warning string.
pub fn plan_includes(source: &Path, dest: &Path, patterns: &[String]) -> IncludePlan {
    plan_includes_with(source, dest, patterns, &|_| {})
}

/// [`plan_includes`], reporting [`IncludeEvent::Found`] as each match is
/// classified and [`IncludeEvent::ScanDone`] when the walk ends.
pub fn plan_includes_with(
    source: &Path,
    dest: &Path,
    patterns: &[String],
    on: &(dyn Fn(IncludeEvent) + Sync),
) -> IncludePlan {
    plan_with_mode(source, dest, patterns, on, LinkMode::Preserve)
}

fn plan_with_mode(
    source: &Path,
    dest: &Path,
    patterns: &[String],
    on: &(dyn Fn(IncludeEvent) + Sync),
    mode: LinkMode,
) -> IncludePlan {
    let opts = match_options();
    let walk = Walk {
        dest,
        on,
        found: std::cell::Cell::new(0),
        mode,
        #[cfg(test)]
        read_threads: None,
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
            links: Vec::new(),
        };
        walk.plan_one(source, pattern, opts, &mut out, &mut warnings);
        out.missing.sort();
        out.missing.dedup();
        out.existing.sort();
        out.existing.dedup();
        out.links.sort();
        out.links.dedup_by(|a, b| a.0 == b.0);
        plans.push(out);
    }
    claim_once(&mut plans);

    on(IncludeEvent::ScanDone {
        files: walk.found.get(),
    });
    IncludePlan {
        patterns: plans,
        warnings,
    }
}

/// Leave every planned path in the first entry that matched it. A pattern
/// naming a file and another naming its parent directory both match it, and
/// copying it twice would double the reported count, so the earlier pattern in
/// configuration order keeps it. Each entry's own counts still sum to the
/// plan's total, which is what a per-entry display reports against. Links are
/// claimed on the same rule, so one is created once.
fn claim_once(plans: &mut [PatternPlan]) {
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    for plan in plans {
        plan.missing.retain(|rel| claimed.insert(rel.clone()));
        plan.existing.retain(|rel| claimed.insert(rel.clone()));
        plan.links.retain(|(rel, _)| claimed.insert(rel.clone()));
    }
}

/// [`copy_includes`], reporting the walk's and the copy's progress through
/// `on`. The plan is built first, so every [`IncludeEvent::Found`] arrives
/// before the first [`IncludeEvent::EntryStart`]. A match that is a symlink is
/// reproduced as a symlink pointing at the same target; its contents are not
/// copied.
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

/// Copy a single file, creating its destination's parent directories as
/// needed. Unless `overwrite` is set, a destination that exists at this moment
/// is left untouched. Errors are pushed as warnings. Safe to call from several
/// threads at once: the counter and the warning list are shared.
fn copy_file(
    src: &Path,
    dst: &Path,
    overwrite: bool,
    copied: &AtomicUsize,
    warnings: &Mutex<Vec<String>>,
) {
    if !overwrite && dst.exists() {
        return;
    }
    if let Some(parent) = dst.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn(warnings, format!("creating {}: {e}", parent.display()));
        return;
    }
    match std::fs::copy(src, dst) {
        Ok(_) => {
            copied.fetch_add(1, Ordering::Relaxed);
        }
        Err(e) => warn(
            warnings,
            format!("copying {} -> {}: {e}", src.display(), dst.display()),
        ),
    }
}

/// Push a warning through a lock a poisoned thread may have left behind. A
/// panicking worker must not silence every warning that follows it.
fn warn(warnings: &Mutex<Vec<String>>, message: String) {
    match warnings.lock() {
        Ok(mut w) => w.push(message),
        Err(poisoned) => poisoned.into_inner().push(message),
    }
}

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
    linked: &AtomicUsize,
    warnings: &Mutex<Vec<String>>,
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
                warn(warnings, format!("replacing {}: {e}", dst.display()));
                return;
            }
        }
        Err(_) => {
            if let Some(parent) = dst.parent()
                && let Err(e) = std::fs::create_dir_all(parent)
            {
                warn(warnings, format!("creating {}: {e}", parent.display()));
                return;
            }
        }
    }
    match crate::sys::symlink(target, dst, src.is_dir()) {
        Ok(()) => {
            linked.fetch_add(1, Ordering::Relaxed);
        }
        Err(e) => warn(
            warnings,
            format!("linking {} -> {}: {e}", dst.display(), target.display()),
        ),
    }
}

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

/// One walked entry's verdict, decided on the pool and applied to the plan on
/// the calling thread. `PatternPlan` has a single owner this way, so the
/// syscall-heavy part parallelises without the collection needing a lock.
enum Classified {
    File { rel: PathBuf, exists: bool },
    Link { rel: PathBuf, target: PathBuf },
    Warning(String),
}

/// Whether `rel` is claimed by `matcher`, either by matching it or by sitting
/// under a directory that does. A directory match contributes its whole
/// subtree, and testing ancestors keeps that rule stateless, so the walk needs
/// nothing shared between threads to enforce it.
fn matches_here(matcher: &glob::Pattern, rel: &Path, opts: glob::MatchOptions) -> bool {
    rel.ancestors()
        .any(|a| !a.as_os_str().is_empty() && matcher.matches_path_with(a, opts))
}

fn is_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

/// The literal directory prefix of a pattern: its components up to the first
/// one holding a wildcard. Scopes a walk so `apps/*/.env.local` reads `apps/`
/// rather than the whole source tree.
///
/// `None` means the pattern holds no wildcard at all and costs one stat instead
/// of a walk. `Some` of an empty path means the walk starts at the source root,
/// which is what a leading `**` asks for.
///
/// The pattern must already be trimmed of a trailing `/` and checked with
/// [`escapes`]: this reads `Component::Normal` only, so a `..` or a root would
/// silently vanish from the prefix rather than being refused.
fn walk_root(pattern: &str) -> Option<PathBuf> {
    if !is_glob(pattern) {
        return None;
    }
    let mut root = PathBuf::new();
    for part in Path::new(pattern).components() {
        let Component::Normal(name) = part else { break };
        match name.to_str() {
            Some(literal) if !is_glob(literal) => root.push(literal),
            _ => break,
        }
    }
    Some(root)
}

/// Bounds a jwalk directory read to the subtrees a pattern could still match,
/// so a directory two levels under `apps/*/.env.local` is never read at all
/// rather than read and then discarded.
///
/// `require_literal_separator` makes a pattern's components line up with a
/// path's components one-to-one, except a bare `**` component, which spans
/// any number of them. That is what makes per-depth pruning sound: a
/// directory whose own path does not satisfy the pattern's first `d`
/// components cannot lead to a match at any depth below it, as long as
/// nothing before depth `d` was a `**`. Built once per pattern rather than
/// per directory.
struct Prune {
    /// `prefixes[k - 1]` is the pattern truncated to its first `k`
    /// components, for `k` in `1..=` the count of components this could be
    /// built for before `unbounded_at`. A directory is tested against the
    /// prefix at its own depth.
    prefixes: Vec<glob::Pattern>,
    /// 0-based index of the first component pruning cannot bound past: the
    /// pattern's first bare `**` component, or the first component whose
    /// accumulated prefix could not itself be compiled as a pattern (see
    /// `for_pattern`). A directory at or past this depth is never pruned.
    unbounded_at: Option<usize>,
}

impl Prune {
    fn for_pattern(pattern: &str) -> Self {
        let components: Vec<&str> = Path::new(pattern)
            .components()
            .filter_map(|c| match c {
                Component::Normal(n) => n.to_str(),
                _ => None,
            })
            .collect();
        // A `**` elsewhere in a component (`a**b`) never reaches here: glob
        // rejects it as a pattern error before a `Prune` is built for it, so
        // the only way a component reads exactly "**" is the real wildcard.
        let recursive_at = components.iter().position(|c| *c == "**");
        let bound = recursive_at.unwrap_or(components.len());
        let mut prefixes = Vec::with_capacity(bound);
        let mut joined = String::new();
        let mut unbounded_at = recursive_at;
        for (idx, part) in components[..bound].iter().enumerate() {
            if !joined.is_empty() {
                joined.push('/');
            }
            joined.push_str(part);
            // `Path::components()` splits on `/` without knowing glob
            // syntax, so a bracket class holding a literal `/` (`x[a/b]y`:
            // valid glob, pointless under `require_literal_separator`, but
            // legal) can truncate mid-class here, and the truncated prefix
            // then fails to compile even though the full pattern did.
            // Degrade instead of trusting every prefix compiles: stop
            // bounding at the first component this happens on, exactly the
            // way a `**` stops it.
            match glob::Pattern::new(&joined) {
                Ok(p) => prefixes.push(p),
                Err(_) => {
                    unbounded_at = Some(idx);
                    break;
                }
            }
        }
        Self {
            prefixes,
            unbounded_at,
        }
    }

    /// Whether the directory at `rel` (relative to the walk's source root)
    /// cannot lead to a match and its contents may be skipped unread.
    ///
    /// Never prunes a directory `matches_here` already claims — a directory
    /// match contributes its whole subtree, so nothing below it may ever be
    /// pruned — nor one at or past `unbounded_at`, which cannot be bounded by
    /// component count. Soundness over aggression: a directory this misses
    /// costs only walk time; a directory it wrongly prunes silently drops a
    /// file.
    fn should_prune(&self, matcher: &glob::Pattern, rel: &Path, opts: glob::MatchOptions) -> bool {
        let depth = rel.components().count();
        // Depth 0 is the walk's own start directory. jwalk surfaces it once
        // through a special root-entry callback rather than as an ordinary
        // child, and there is nothing below the walk's own root to bound.
        if depth == 0 {
            return false;
        }
        if matches_here(matcher, rel, opts) {
            return false;
        }
        if self.unbounded_at.is_some_and(|i| depth > i) {
            return false;
        }
        match self.prefixes.get(depth - 1) {
            Some(prefix) => !prefix.matches_path_with(rel, opts),
            None => true,
        }
    }
}

/// The options every include match is tested with.
///
/// `require_literal_separator` is true because that is what the old walker
/// actually did: `glob_with` forced it true whatever it was handed
/// (`glob-0.3.3/src/lib.rs:176`) and matched one path component at a time, so a
/// single `*` has never crossed a `/` here. `matches_path_with` honours the
/// flag, so building these with `false` would widen every pattern and pull
/// unrequested files into a worktree. `**` still
/// recurses; it is a different token.
fn match_options() -> glob::MatchOptions {
    glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::parse_porcelain;
    use std::path::Path;

    /// `walk_and_classify` must evaluate `crate::pool::jwalk_parallelism()`
    /// on the calling thread, not from inside `crate::pool::install`: doing
    /// the latter makes the walk see itself as already inside the pool and
    /// silently fall back to `Serial`, so every directory read for the whole
    /// walk lands on one thread. `pool::jwalk_parallelism_uses_the_shared_pool_
    /// from_outside_it` already asserted the helper's own behaviour and did
    /// not catch this, because the bug was in how `walk_and_classify` called
    /// it, not in the helper. This constructs a real `Walk` and drives the
    /// real, private `walk_and_classify` directly (accessible from this
    /// submodule), fanning out to enough independent directories that a
    /// serial reader could only ever touch one thread, and asserts more than
    /// one shows up. `Walk::read_threads` exists solely to make this
    /// observable: production code never sets it to `Some`.
    #[test]
    fn the_walk_reads_directories_on_more_than_one_pool_thread() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        for i in 0..200 {
            write(&src.join(format!("d{i}")).join("leaf.txt"), "x");
        }
        if crate::pool::width() < 2 {
            // Parallelism cannot be observed on a pool configured with one
            // worker; there is nothing to assert.
            return;
        }

        let read_threads =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
        let walk = Walk {
            dest: &dst,
            on: &|_| {},
            found: std::cell::Cell::new(0),
            mode: LinkMode::Preserve,
            read_threads: Some(read_threads.clone()),
        };
        let mut out = PatternPlan {
            pattern: String::new(),
            missing: Vec::new(),
            existing: Vec::new(),
            links: Vec::new(),
        };
        let mut warnings = Vec::new();

        walk.walk_and_classify(&src, &src, None, match_options(), &mut out, &mut warnings);

        assert_eq!(out.missing.len(), 200, "{warnings:?}");
        let threads = read_threads.lock().unwrap();
        assert!(
            threads.len() > 1,
            "directory reads landed on only one pool thread ({threads:?}); \
             the walk is running serial"
        );
    }

    /// A bracket class holding a literal `/` (`x[a/b]y`) compiles as a whole
    /// pattern -- glob does not forbid `/` inside a class -- but
    /// `Path::components()` splits inside the class, since it knows nothing
    /// of glob syntax, so the truncated prefix `x[a` fails to compile even
    /// though the full pattern did. `Prune::for_pattern` used to `expect`
    /// every prefix to compile and panicked on exactly this pattern; it now
    /// degrades to treating the failing component as unbounded, the same way
    /// a `**` is, and the match still happens correctly.
    #[test]
    fn a_bracket_class_holding_a_separator_does_not_panic() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("xay"), "matches the class");
        write(&src.join("xzy"), "does not match the class");

        let plan = plan_includes(&src, &dst, &["x[a/b]y".to_string()]);

        let missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        assert_eq!(missing, vec![PathBuf::from("xay")]);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    #[test]
    fn parses_two_worktrees() {
        let out = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /repo/eng-1\nHEAD def\nbranch refs/heads/lev/eng-1234-x\n";
        let wts = parse_porcelain(out);
        assert_eq!(wts.len(), 2);
        assert_eq!(wts[1].branch, "lev/eng-1234-x");
    }
    #[test]
    fn id_from_branch_then_dir() {
        assert_eq!(issue_id_of(Path::new("/x"), "lev/eng-1234-fix"), "ENG-1234");
        assert_eq!(issue_id_of(Path::new("/x/abc-9"), "DETACHED"), "ABC-9");
        assert_eq!(issue_id_of(Path::new("/x/scratch"), "main"), "UNKNOWN");
    }

    #[test]
    fn the_record_wins_over_the_branch_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            dir.path().join(".devkit").join("issue.toml"),
            "issue = \"87\"\nslug = \"fix\"\napps = []\n",
        )
        .unwrap();
        // The branch carries a Linear-shaped id that is NOT this worktree's issue.
        assert_eq!(issue_id_of(dir.path(), "lev/eng-1-something"), "87");
    }

    #[test]
    fn without_a_record_the_branch_scan_still_works() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(issue_id_of(dir.path(), "lev/eng-1-something"), "ENG-1");
    }

    #[test]
    fn a_worktree_with_neither_is_unknown() {
        // The directory name is a fallback id source, so the worktree is given a
        // name carrying no letters-dash-digits run rather than the scratch
        // directory's own.
        let scratch = tempfile::tempdir().unwrap();
        let worktree = scratch.path().join("noidhere");
        std::fs::create_dir_all(&worktree).unwrap();
        assert_eq!(issue_id_of(&worktree, "lev/no-id-here"), "UNKNOWN");
    }

    /// The record holds the tracker's own spelling, so it comes back untouched.
    /// Uppercasing it would corrupt any id a tracker does not spell in caps.
    #[test]
    fn a_record_id_is_returned_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            dir.path().join(".devkit").join("issue.toml"),
            "issue = \"eng-1234\"\nslug = \"x\"\napps = []\n",
        )
        .unwrap();
        assert_eq!(issue_id_of(dir.path(), "DETACHED"), "eng-1234");
    }

    /// A record with no id is no answer at all: fall through to the scan rather
    /// than reporting an empty id.
    #[test]
    fn an_empty_record_id_falls_through_to_the_branch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            dir.path().join(".devkit").join("issue.toml"),
            "issue = \"\"\nslug = \"x\"\napps = []\n",
        )
        .unwrap();
        assert_eq!(issue_id_of(dir.path(), "lev/eng-1-something"), "ENG-1");
    }

    #[test]
    fn skips_pr_number_marker() {
        // A `pr-<number>` PR-checkout marker is the PR number, not an issue id:
        // skip it and return the real id that follows.
        assert_eq!(
            find_id("pr-3255-feat-api-migrate-view-v2-to-kysely-u11-swe-8603").as_deref(),
            Some("swe-8603")
        );
        assert_eq!(
            issue_id_of(
                Path::new("/x"),
                "pr-3255-feat-api-migrate-view-v2-to-kysely-u11-swe-8603"
            ),
            "SWE-8603"
        );
        // Normal worktree branch still resolves from its leading id.
        assert_eq!(
            find_id("swe-9959-optimize-tasks-list-payload").as_deref(),
            Some("swe-9959")
        );
        // A PR marker with no trailing id yields no issue id (PR number is
        // surfaced separately, never as the issue id).
        assert_eq!(find_id("pr-3255-feat-only"), None);
        assert_eq!(find_id("PR-42"), None);
    }

    use std::fs;

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn copies_a_matching_file_preserving_relative_path() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("apps/web/.env.local"), "SECRET=1");

        let (n, _, warnings) = copy_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        assert_eq!(n, 1);
        assert!(warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join("apps/web/.env.local")).unwrap(),
            "SECRET=1"
        );
    }

    #[test]
    fn double_star_matches_nested_file() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("a/b/c/.env.local"), "X=1");

        let (n, _, _) = copy_includes(&src, &dst, &["**/.env.local".to_string()]);

        assert_eq!(n, 1);
        assert!(dst.join("a/b/c/.env.local").exists());
    }

    #[test]
    fn directory_pattern_copies_recursively() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".claude/hooks/pre.sh"), "echo pre");
        write(&src.join(".claude/hooks/sub/post.sh"), "echo post");

        // Trailing slash must behave like the bare directory.
        let (n, _, warnings) = copy_includes(&src, &dst, &[".claude/hooks/".to_string()]);

        assert_eq!(n, 2);
        assert!(warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join(".claude/hooks/pre.sh")).unwrap(),
            "echo pre"
        );
        assert_eq!(
            fs::read_to_string(dst.join(".claude/hooks/sub/post.sh")).unwrap(),
            "echo post"
        );
    }

    #[test]
    fn pattern_matching_nothing_is_silently_skipped() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        fs::create_dir_all(&src).unwrap();

        let (n, _, warnings) = copy_includes(&src, &dst, &["does/not/exist".to_string()]);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
    }

    #[test]
    fn existing_destination_file_is_not_clobbered() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".tool-versions"), "node 20");
        write(&dst.join(".tool-versions"), "KEEP ME");

        let (n, _, _) = copy_includes(&src, &dst, &[".tool-versions".to_string()]);

        assert_eq!(n, 0);
        assert_eq!(
            fs::read_to_string(dst.join(".tool-versions")).unwrap(),
            "KEEP ME"
        );
    }

    #[test]
    fn plan_includes_puts_a_new_match_in_missing() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".env.local"), "SECRET=1");

        let plan = plan_includes(&src, &dst, &[".env.local".to_string()]);

        assert_eq!(
            plan.missing().collect::<Vec<_>>(),
            [Path::new(".env.local")]
        );
        assert_eq!(plan.existing_len(), 0);
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn plan_includes_puts_an_already_present_match_in_existing() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".tool-versions"), "node 20");
        write(&dst.join(".tool-versions"), "KEEP ME");

        let plan = plan_includes(&src, &dst, &[".tool-versions".to_string()]);

        assert_eq!(
            plan.existing().collect::<Vec<_>>(),
            [Path::new(".tool-versions")]
        );
        assert_eq!(plan.missing_len(), 0);
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn plan_includes_directory_pattern_enumerates_files_not_the_directory() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".claude/hooks/pre.sh"), "echo pre");
        write(&src.join(".claude/hooks/sub/post.sh"), "echo post");
        write(&dst.join(".claude/hooks/pre.sh"), "echo old");

        let plan = plan_includes(&src, &dst, &[".claude/hooks/".to_string()]);

        assert_eq!(
            plan.existing().collect::<Vec<_>>(),
            [Path::new(".claude/hooks/pre.sh")]
        );
        assert_eq!(
            plan.missing().collect::<Vec<_>>(),
            [Path::new(".claude/hooks/sub/post.sh")]
        );
        assert!(plan.warnings.is_empty());
    }

    /// `read_dir` order is the filesystem's, so an unsorted plan would print a
    /// different `copied N file(s)` list on every run and platform. Two
    /// patterns pin both properties at once: patterns appear in configuration
    /// order (`hooks/` before `configs/`, though `configs` sorts first
    /// alphabetically), and matches are sorted within each pattern.
    #[test]
    fn plan_vectors_come_back_sorted() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        for name in ["d.sh", "b.sh", "a.sh", "c.sh"] {
            write(&src.join("hooks").join(name), "x");
        }
        for name in ["z.txt", "x.txt", "y.txt"] {
            write(&src.join("configs").join(name), "x");
        }
        write(&dst.join("hooks/c.sh"), "old");
        write(&dst.join("hooks/a.sh"), "old");

        let plan = plan_includes(&src, &dst, &["hooks/".to_string(), "configs/".to_string()]);

        assert_eq!(
            plan.missing().collect::<Vec<_>>(),
            [
                Path::new("hooks/b.sh"),
                Path::new("hooks/d.sh"),
                Path::new("configs/x.txt"),
                Path::new("configs/y.txt"),
                Path::new("configs/z.txt"),
            ]
        );
        assert_eq!(
            plan.existing().collect::<Vec<_>>(),
            [Path::new("hooks/a.sh"), Path::new("hooks/c.sh")]
        );
    }

    #[test]
    fn plan_includes_pattern_matching_nothing_yields_empty_plan() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        fs::create_dir_all(&src).unwrap();

        let plan = plan_includes(&src, &dst, &["does/not/exist".to_string()]);

        assert_eq!(plan.missing_len(), 0);
        assert_eq!(plan.existing_len(), 0);
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn plan_includes_bad_glob_warns_without_panicking() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        fs::create_dir_all(&src).unwrap();

        let plan = plan_includes(&src, &dst, &["[".to_string()]);

        assert_eq!(plan.missing_len(), 0);
        assert_eq!(plan.existing_len(), 0);
        assert_eq!(plan.warnings.len(), 1);
    }

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
        assert_eq!(
            plan.patterns[0].missing,
            vec![PathBuf::from(".tool-versions")]
        );
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
        assert_eq!(
            plan.patterns[1].missing,
            vec![PathBuf::from(".tool-versions")]
        );
    }

    /// missing() and existing() flatten every pattern's matches into one ordered sequence.
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

        assert_eq!(
            plan.missing().collect::<Vec<_>>(),
            [Path::new("hooks/a.sh")]
        );
        assert_eq!(
            plan.existing().collect::<Vec<_>>(),
            [Path::new(".tool-versions")]
        );
        assert_eq!(plan.missing_len(), 1);
        assert_eq!(plan.existing_len(), 1);
    }

    #[test]
    fn apply_includes_without_overwrite_leaves_existing_untouched() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".tool-versions"), "node 20");
        write(&dst.join(".tool-versions"), "KEEP ME");

        let plan = plan_includes(&src, &dst, &[".tool-versions".to_string()]);
        let (n, _, warnings) = apply_includes(&src, &dst, &plan, false);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join(".tool-versions")).unwrap(),
            "KEEP ME"
        );
    }

    /// The plan is a snapshot; the never-clobber guarantee is about the write.
    /// A file that appears between planning and applying must survive.
    #[test]
    fn a_destination_appearing_after_the_plan_is_not_clobbered() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".tool-versions"), "node 20");

        let plan = plan_includes(&src, &dst, &[".tool-versions".to_string()]);
        assert_eq!(
            plan.missing().collect::<Vec<_>>(),
            [Path::new(".tool-versions")]
        );
        write(&dst.join(".tool-versions"), "KEEP ME");

        let (n, _, warnings) = apply_includes(&src, &dst, &plan, false);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join(".tool-versions")).unwrap(),
            "KEEP ME"
        );
    }

    #[test]
    fn apply_includes_with_overwrite_replaces_existing() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".tool-versions"), "node 20");
        write(&dst.join(".tool-versions"), "KEEP ME");

        let plan = plan_includes(&src, &dst, &[".tool-versions".to_string()]);
        let (n, _, warnings) = apply_includes(&src, &dst, &plan, true);

        assert_eq!(n, 1);
        assert!(warnings.is_empty());
        assert_eq!(
            fs::read_to_string(dst.join(".tool-versions")).unwrap(),
            "node 20"
        );
    }

    #[test]
    fn empty_patterns_is_a_no_op() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        fs::create_dir_all(&src).unwrap();

        let (n, _, warnings) = copy_includes(&src, &dst, &[]);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
        assert!(!dst.exists());
    }

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

        assert_eq!(
            plan.missing_len(),
            0,
            "planned {:?}",
            plan.missing().collect::<Vec<_>>()
        );
        assert_eq!(
            plan.existing_len(),
            0,
            "planned {:?}",
            plan.existing().collect::<Vec<_>>()
        );
        assert!(plan.warnings.is_empty(), "warned {:?}", plan.warnings);
    }

    /// `plan_includes` strips `source` lexically, so `source.join("../x")` still
    /// carries the prefix and yields `../x` as the "relative" path — escaping the
    /// destination as well as the source. Absolute patterns replace the base
    /// outright, and so does a root-relative one: `/etc/x` is not absolute on
    /// Windows, but `Path::join` discards the base for it just the same. None
    /// may reach the glob.
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
                "/etc/passwd".to_string(),
                "keep.md".to_string(),
            ],
        );

        assert_eq!(copied, 1, "only the in-tree file is copied");
        let archived: Vec<String> = std::fs::read_dir(&dst)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(archived, vec!["keep.md".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("outside.md")).unwrap(),
            "secret",
            "the file outside the worktree is untouched"
        );
        assert_eq!(
            warnings.len(),
            2,
            "one warning per rejected pattern: {warnings:?}"
        );
        assert!(warnings[0].contains("../outside.md"), "{warnings:?}");
        assert!(warnings[1].contains("/etc/passwd"), "{warnings:?}");
    }

    /// `plan_one` builds its pattern from `source.join(trimmed)`, and
    /// `Path::join` discards the base for a rooted pattern — on Windows too,
    /// where `is_absolute` is false but `join` still replaces the base. Without
    /// a gate, an include reads from outside the tree it is supposed to copy.
    ///
    /// The gate sits above the wildcard/literal split, so `../*/secrets` is
    /// refused by the same check as `../outside.md`.
    #[test]
    fn an_escaping_include_pattern_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("keep.txt"), "x");
        write(&base.path().join("outside.md"), "secret");

        let plan = plan_includes(
            &src,
            &dst,
            &[
                "../outside.md".to_string(),
                "/etc/passwd".to_string(),
                "../*/secrets".to_string(),
                "keep.txt".to_string(),
            ],
        );

        assert_eq!(plan.patterns.len(), 4, "every pattern keeps its entry");
        assert_eq!(plan.missing_len(), 1);
        assert_eq!(plan.missing().next().unwrap(), Path::new("keep.txt"));
        assert_eq!(plan.warnings.len(), 3, "{:?}", plan.warnings);
        assert!(plan.warnings.iter().any(|w| w.contains("../outside.md")));
        assert!(plan.warnings.iter().any(|w| w.contains("/etc/passwd")));
        assert!(plan.warnings.iter().any(|w| w.contains("../*/secrets")));
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

        let (copied, _, warnings) = copy_includes(&src, &dst, &["notes.md".to_string()]);
        assert_eq!(copied, 0, "backfill skips an existing file");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            std::fs::read_to_string(dst.join("notes.md")).unwrap(),
            "old"
        );

        let (copied, warnings) = copy_out(&src, &dst, &["notes.md".to_string()]);
        assert_eq!(copied, 1, "archive replaces it");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            std::fs::read_to_string(dst.join("notes.md")).unwrap(),
            "new"
        );
    }

    /// A directory pattern archives recursively, at the same relative path.
    #[test]
    fn copy_out_copies_a_directory_match_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        let dst = dir.path().join("archive");
        write(&wt.join("scratch/a.md"), "a");
        write(&wt.join("scratch/deep/b.md"), "b");

        let (copied, warnings) = copy_out(&wt, &dst, &["scratch/".to_string()]);

        assert_eq!(copied, 2, "{warnings:?}");
        assert_eq!(
            std::fs::read_to_string(dst.join("scratch/deep/b.md")).unwrap(),
            "b"
        );
    }

    /// An empty directory is the normal case for a worktree that produced no
    /// scratch: the pattern matches the directory and finds no files, which
    /// must not warn. A pattern naming a directory that is absent entirely
    /// matches nothing and is equally quiet.
    #[test]
    fn copy_out_is_quiet_when_a_directory_pattern_holds_no_files() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(wt.join("scratch")).unwrap();
        let dst = dir.path().join("archive");

        let (copied, warnings) = copy_out(&wt, &dst, &["scratch/".to_string()]);
        assert_eq!(copied, 0);
        assert!(warnings.is_empty(), "{warnings:?}");

        let (copied, warnings) = copy_out(&wt, &dst, &["absent/".to_string()]);
        assert_eq!(copied, 0);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// A file named both directly and through its parent directory is one
    /// file: planning it twice would copy it twice and double the count the
    /// caller reports.
    #[test]
    fn overlapping_patterns_plan_each_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        write(&src.join("scratch/a.md"), "a");

        let plan = plan_includes(
            &src,
            &dst,
            &["scratch/".to_string(), "scratch/a.md".to_string()],
        );
        assert_eq!(
            plan.missing().collect::<Vec<_>>(),
            [PathBuf::from("scratch").join("a.md").as_path()]
        );
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

        let (copied, warnings) = copy_out(
            &src,
            &dst,
            &["scratch/".to_string(), "scratch/a.md".to_string()],
        );
        assert_eq!(copied, 1, "{warnings:?}");
    }

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

    /// Sort each contiguous run of `file …` lines. The copy is parallel within
    /// one entry, so those lines may arrive in any order; the `start` and
    /// `done` brackets around them may not.
    fn sort_file_runs(log: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(log.len());
        let mut run: Vec<String> = Vec::new();
        for line in log {
            if line.starts_with("file ") {
                run.push(line.clone());
            } else {
                run.sort();
                out.append(&mut run);
                out.push(line.clone());
            }
        }
        run.sort();
        out.append(&mut run);
        out
    }

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
        let (copied, _, warnings) = apply_includes_with(&src, &dst, &plan, false, &|e| match e {
            IncludeEvent::EntryStart {
                pattern,
                index,
                of,
                files,
            } => log
                .lock()
                .unwrap()
                .push(format!("start {pattern} {index}/{of} {files}")),
            IncludeEvent::FileDone { pattern, done, of } => log
                .lock()
                .unwrap()
                .push(format!("file {pattern} {done}/{of}")),
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
            sort_file_runs(&log.lock().unwrap()),
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
        assert_eq!(
            plan.missing_len(),
            1,
            "planned before the destination existed"
        );
        write(&dst.join(".tool-versions"), "APPEARED SINCE");

        let files = std::sync::Mutex::new(Vec::new());
        let (copied, _, warnings) = apply_includes_with(&src, &dst, &plan, false, &|e| {
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
        let (copied, _, _) = apply_includes_with(&src, &dst, &plan, true, &|e| {
            if let IncludeEvent::FileDone { done, of, .. } = e {
                files.lock().unwrap().push((done, of));
            }
        });

        assert_eq!(copied, 1);
        assert_eq!(*files.lock().unwrap(), vec![(1, 1)]);
    }

    /// A link is a unit of a pattern's work exactly like a file, so
    /// `EntryStart`'s denominator and the `FileDone` count that ticks toward it
    /// both include it.
    #[test]
    fn the_entry_denominator_counts_a_link_alongside_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("src");
        let dest = tmp.path().join("dst");
        std::fs::create_dir_all(source.join("inc")).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(source.join("inc/plain.txt"), "content").unwrap();
        std::fs::write(source.join("real.txt"), "content").unwrap();
        if !link_or_skip(
            Path::new("../real.txt"),
            &source.join("inc/link.txt"),
            false,
        ) {
            return;
        }

        let plan = plan_includes(&source, &dest, &["inc/".to_string()]);
        let start_files = std::sync::Mutex::new(Vec::new());
        let done = std::sync::Mutex::new(Vec::new());
        let (_, linked, warnings) =
            apply_includes_with(&source, &dest, &plan, false, &|e| match e {
                IncludeEvent::EntryStart { files, .. } => start_files.lock().unwrap().push(files),
                IncludeEvent::FileDone { done: d, of, .. } => done.lock().unwrap().push((d, of)),
                _ => {}
            });

        assert_eq!(linked, 1, "{warnings:?}");
        assert_eq!(*start_files.lock().unwrap(), vec![2]);
        assert_eq!(*done.lock().unwrap(), vec![(1, 2), (2, 2)]);
    }

    /// A pattern that matches nothing still occupies its slot between two that
    /// do, so `index` stays gap-free and `of` stays the configured pattern count
    /// rather than the matched count. Callers number a display off these.
    #[test]
    fn an_empty_pattern_between_two_matching_ones_still_brackets() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".tool-versions"), "node 20");
        write(&src.join("hooks/a.sh"), "x");

        let patterns = [
            ".tool-versions".to_string(),
            "does/not/exist".to_string(),
            "hooks/".to_string(),
        ];
        let plan = plan_includes(&src, &dst, &patterns);

        let starts = std::sync::Mutex::new(Vec::new());
        let dones = std::sync::Mutex::new(Vec::new());
        apply_includes_with(&src, &dst, &plan, false, &|e| match e {
            IncludeEvent::EntryStart {
                pattern,
                index,
                of,
                files,
            } => starts
                .lock()
                .unwrap()
                .push((pattern.to_string(), index, of, files)),
            IncludeEvent::EntryDone {
                pattern,
                index,
                of,
                copied,
            } => dones
                .lock()
                .unwrap()
                .push((pattern.to_string(), index, of, copied)),
            _ => {}
        });

        assert_eq!(
            *starts.lock().unwrap(),
            vec![
                (".tool-versions".to_string(), 0, 3, 1),
                ("does/not/exist".to_string(), 1, 3, 0),
                ("hooks/".to_string(), 2, 3, 1),
            ]
        );
        assert_eq!(
            *dones.lock().unwrap(),
            vec![
                (".tool-versions".to_string(), 0, 3, 1),
                ("does/not/exist".to_string(), 1, 3, 0),
                ("hooks/".to_string(), 2, 3, 1),
            ]
        );
    }

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
        if !link_or_skip(
            Path::new("../real.txt"),
            &source.join("inc/link.txt"),
            false,
        ) {
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

    /// A directory include walks its whole subtree, and a symlink inside it is
    /// reproduced as a link rather than descended into. jwalk with
    /// `follow_links(true)` reads through a symlinked directory unless the walk
    /// clears its children, so this pins the clearing.
    #[cfg(unix)]
    #[test]
    fn a_directory_include_keeps_a_nested_symlink_as_a_link() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("inc/a.txt"), "x");
        write(&src.join("inc/deep/b.txt"), "y");
        write(&src.join("target/c.txt"), "z");
        std::os::unix::fs::symlink("../target", src.join("inc/linked")).unwrap();

        let plan = plan_includes(&src, &dst, &["inc/".to_string()]);

        let mut missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        missing.sort();
        assert_eq!(
            missing,
            vec![PathBuf::from("inc/a.txt"), PathBuf::from("inc/deep/b.txt")],
            "the link's contents are not planned as files"
        );
        assert_eq!(plan.patterns[0].links.len(), 1);
        assert_eq!(plan.patterns[0].links[0].0, PathBuf::from("inc/linked"));
        assert_eq!(plan.patterns[0].links[0].1, PathBuf::from("../target"));
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
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
    /// the target's contents. `make_link` decides skip-or-replace per link.
    #[test]
    fn an_occupied_link_destination_stays_a_link_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("src");
        let dest = tmp.path().join("dst");
        std::fs::create_dir_all(source.join("inc")).unwrap();
        std::fs::create_dir_all(dest.join("inc")).unwrap();
        std::fs::write(source.join("real.txt"), "content").unwrap();
        std::fs::write(dest.join("inc/link.txt"), "already here").unwrap();
        if !link_or_skip(
            Path::new("../real.txt"),
            &source.join("inc/link.txt"),
            false,
        ) {
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
        // Windows only resolves a relative reparse-point target when it uses the
        // native separator, so the target is built with `join` rather than a
        // literal `../real.txt`, which `read_link` would return unresolved but
        // `copy_out`, which opens through the link, cannot.
        let target = Path::new("..").join("real.txt");
        if !link_or_skip(&target, &source.join("inc/link.txt"), false) {
            return;
        }

        let (copied, warnings) = copy_out(&source, &dest, &["inc/".to_string()]);

        assert_eq!(
            copied, 1,
            "the target's contents were archived: {warnings:?}"
        );
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

    /// `copy_out` cannot open through a broken link to archive its target, and
    /// the worktree it walked is deleted right after, so a silently dropped
    /// link here means the archive is incomplete with no record of why.
    #[cfg(unix)]
    #[test]
    fn copy_out_warns_about_a_broken_link_it_cannot_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wt");
        let dest = tmp.path().join("archive");
        std::fs::create_dir_all(source.join("inc")).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::os::unix::fs::symlink("nowhere.txt", source.join("inc/link.txt")).unwrap();

        let (copied, warnings) = copy_out(&source, &dest, &["inc/".to_string()]);

        assert_eq!(copied, 0, "{warnings:?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("link.txt") && w.contains("nowhere.txt")),
            "no warning names the unarchived link: {warnings:?}"
        );
        assert!(!dest.join("inc/link.txt").exists());
    }

    #[test]
    fn a_symlinked_file_is_reproduced_as_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("src");
        let dest = tmp.path().join("dst");
        std::fs::create_dir_all(source.join("inc")).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(source.join("real.txt"), "content").unwrap();
        if !link_or_skip(
            Path::new("../real.txt"),
            &source.join("inc/link.txt"),
            false,
        ) {
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
    ///
    /// Windows only resolves a relative reparse-point target when it uses the
    /// native separator, so the target is built with `join` rather than a
    /// literal `../real.txt`, which `read_link` would return unresolved but
    /// reading through the link here cannot.
    #[test]
    fn a_relative_target_resolves_inside_the_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("src");
        let dest = tmp.path().join("dst");
        std::fs::create_dir_all(source.join("inc")).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(source.join("real.txt"), "content").unwrap();
        std::fs::write(dest.join("real.txt"), "the worktree's own copy").unwrap();
        let target = Path::new("..").join("real.txt");
        if !link_or_skip(&target, &source.join("inc/link.txt"), false) {
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
        if !link_or_skip(
            Path::new("nowhere.txt"),
            &source.join("inc/link.txt"),
            false,
        ) {
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
        assert_eq!(
            std::fs::read_link(&landed).unwrap(),
            Path::new("nowhere.txt")
        );
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
        if !link_or_skip(
            Path::new("../real.txt"),
            &source.join("inc/link.txt"),
            false,
        ) {
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
        if !link_or_skip(
            Path::new("../real.txt"),
            &source.join("inc/link.txt"),
            false,
        ) {
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

    /// The copy runs several files at once, so its counters and its warnings
    /// have to survive concurrent writers. A hundred files is enough for the
    /// pool to hand work to more than one thread.
    #[test]
    fn a_parallel_copy_counts_every_file_exactly_once() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        for i in 0..100 {
            write(&src.join("many").join(format!("f{i}.txt")), "x");
        }

        let plan = plan_includes(&src, &dst, &["many/".to_string()]);
        let seen = std::sync::Mutex::new(Vec::new());
        let (copied, _, warnings) = apply_includes_with(&src, &dst, &plan, false, &|e| {
            if let IncludeEvent::FileDone { done, .. } = e {
                seen.lock().unwrap().push(done);
            }
        });

        assert_eq!(copied, 100, "{warnings:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut seen = seen.lock().unwrap().clone();
        seen.sort_unstable();
        assert_eq!(seen, (1..=100).collect::<Vec<_>>());
        for i in 0..100 {
            assert!(dst.join("many").join(format!("f{i}.txt")).exists());
        }
    }

    /// A copy started from inside the pool must finish rather than wait on
    /// threads its own caller is holding. Carries a timeout because a
    /// regression hangs rather than fails.
    #[test]
    fn a_copy_started_from_inside_the_pool_completes() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        for i in 0..20 {
            write(&src.join("many").join(format!("f{i}.txt")), "x");
        }

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let done = crate::pool::install(|| {
                let plan = plan_includes(&src, &dst, &["many/".to_string()]);
                apply_includes(&src, &dst, &plan, false).0
            });
            let _ = tx.send(done);
        });

        let copied = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("a nested copy exhausted the pool instead of running serially");
        assert_eq!(copied, 20);
    }

    #[test]
    fn walk_root_stops_at_the_first_wildcard() {
        assert_eq!(walk_root("apps/*/.env.local"), Some(PathBuf::from("apps")));
        assert_eq!(
            walk_root("apps/web/config/*.json"),
            Some(PathBuf::from("apps/web/config"))
        );
        assert_eq!(walk_root("src/[abc]/x"), Some(PathBuf::from("src")));
        assert_eq!(walk_root("logs/?.txt"), Some(PathBuf::from("logs")));
        // Wildcards in non-trailing components: stops at the first wildcard-containing component.
        assert_eq!(walk_root("apps/web-*/x"), Some(PathBuf::from("apps")));
        assert_eq!(walk_root("apps/web*/config"), Some(PathBuf::from("apps")));
        // Wildcard-suffixed component as the last component still returns the prefix.
        assert_eq!(walk_root("apps/web-*"), Some(PathBuf::from("apps")));
    }

    /// A leading `**` scopes to nothing, so the walk starts at the source
    /// root. That matches a leading `**`'s meaning, not a widening.
    #[test]
    fn walk_root_of_a_leading_recursive_wildcard_is_the_source_root() {
        assert_eq!(walk_root("**/.env.local"), Some(PathBuf::new()));
    }

    /// A pattern with no wildcard costs one stat, not a walk.
    #[test]
    fn walk_root_is_none_without_a_wildcard() {
        assert_eq!(walk_root(".tool-versions"), None);
        assert_eq!(walk_root(".claude/hooks"), None);
    }

    /// `require_literal_separator: true` is what keeps a single `*` from
    /// crossing a directory separator. Getting this wrong would widen every
    /// pattern: `apps/*/.env.local` would start matching two directories down
    /// and pull unrequested files into a worktree.
    #[test]
    fn a_single_wildcard_does_not_cross_a_directory_separator() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("apps/web/.env.local"), "shallow");
        write(&src.join("apps/web/nested/.env.local"), "deep");

        let plan = plan_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        let missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        assert_eq!(missing, vec![PathBuf::from("apps/web/.env.local")]);
    }

    /// A leading `./` names the same pattern as one without it.
    /// `walk_root("./apps/*")` stops at the `.` component before the walk
    /// ever starts, and the compiled pattern with `./` left in never matches
    /// a relative path that lacks it, so an untrimmed leading `./` used to
    /// walk the whole tree and match nothing, silently.
    #[test]
    fn a_leading_current_dir_component_is_stripped() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("apps/web/.env.local"), "x");

        let plan = plan_includes(&src, &dst, &["./apps/*/.env.local".to_string()]);

        let missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        assert_eq!(missing, vec![PathBuf::from("apps/web/.env.local")]);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    /// `**` matches across separators including zero of them, which is what
    /// makes `**/.env.local` find a root-level file as well as a nested one.
    #[test]
    fn a_recursive_wildcard_matches_at_every_depth_including_none() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".env.local"), "root");
        write(&src.join("apps/web/.env.local"), "nested");

        let plan = plan_includes(&src, &dst, &["**/.env.local".to_string()]);

        let mut missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        missing.sort();
        assert_eq!(
            missing,
            vec![
                PathBuf::from(".env.local"),
                PathBuf::from("apps/web/.env.local"),
            ]
        );
    }

    /// A symlinked directory in the middle of a pattern is read through.
    /// jwalk's default would give it no children and drop the file underneath
    /// with no warning at all.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_mid_pattern_is_traversed() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("shared/web/.env.local"), "x");
        std::fs::create_dir_all(src.join("apps")).unwrap();
        std::os::unix::fs::symlink("../shared/web", src.join("apps/web")).unwrap();

        let plan = plan_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        let missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        assert_eq!(missing, vec![PathBuf::from("apps/web/.env.local")]);
        assert!(plan.patterns[0].links.is_empty());
    }

    /// A link the pattern claims is still reproduced as a link, not read
    /// through, even though the walk follows links to reach the case above.
    /// This is what the child-clearing in `process_read_dir` protects.
    #[cfg(unix)]
    #[test]
    fn a_claimed_symlinked_directory_is_planned_as_a_link() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("shared/web/.env.local"), "x");
        std::fs::create_dir_all(src.join("apps")).unwrap();
        std::os::unix::fs::symlink("../shared/web", src.join("apps/web")).unwrap();

        let plan = plan_includes(&src, &dst, &["apps/*".to_string()]);

        assert_eq!(plan.missing_len(), 0, "the link's contents are not planned");
        assert_eq!(plan.patterns[0].links.len(), 1);
        assert_eq!(plan.patterns[0].links[0].0, PathBuf::from("apps/web"));
        assert_eq!(plan.patterns[0].links[0].1, PathBuf::from("../shared/web"));
    }

    /// A matched directory contributes its whole subtree, however deep, so
    /// `should_prune`'s `matches_here` check must never be skipped: without
    /// it, a directory past the pattern's own component count (`depth - 1`
    /// beyond `prefixes.len()`) reads as unreachable and gets pruned, even
    /// though it sits under a directory the pattern already matched. This
    /// is the failure mode the whole pruning change exists to prevent, so it
    /// gets its own test independent of the symlink case above, whose link
    /// contents are deliberately never planned at all.
    #[test]
    fn a_matched_directory_plans_files_arbitrarily_deep_beneath_it() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("apps/web/deep/nested/file"), "x");

        let plan = plan_includes(&src, &dst, &["apps/*".to_string()]);

        let missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        assert_eq!(missing, vec![PathBuf::from("apps/web/deep/nested/file")]);
    }

    /// A wildcard pattern whose literal prefix does not exist is silent, as it
    /// is today: `apps/*/.env.local` in a repository with no `apps/` is a
    /// common configuration and must not warn on every setup.
    #[test]
    fn a_missing_walk_root_is_silent() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("keep.txt"), "x");

        let plan = plan_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        assert_eq!(plan.missing_len(), 0);
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    }

    /// A `copy_out` pattern naming a symlink resolves through it and archives
    /// the target's contents. `copy_out`'s own comment that a Follow-mode plan
    /// holds no links depends on this, and nothing covered it before.
    #[cfg(unix)]
    #[test]
    fn copy_out_resolves_a_pattern_that_names_a_link() {
        let base = tempfile::tempdir().unwrap();
        let wt = base.path().join("wt");
        let dst = base.path().join("dst");
        write(&wt.join("real/notes.md"), "kept");
        std::os::unix::fs::symlink("real", wt.join("archive")).unwrap();

        let (copied, warnings) = copy_out(&wt, &dst, &["archive".to_string()]);

        assert_eq!(copied, 1, "{warnings:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            std::fs::read_to_string(dst.join("archive/notes.md")).unwrap(),
            "kept"
        );
    }

    /// A `copy_out` pattern naming a broken link directly warns the same way a
    /// broken link found through a wildcard pattern does: naming the target
    /// that could not be resolved, not just relaying the bare OS error. Before
    /// the wording was aligned this warning named the link but never its
    /// target, so this test would have failed against that message.
    #[cfg(unix)]
    #[test]
    fn copy_out_warns_the_same_way_for_a_broken_link_named_directly() {
        let base = tempfile::tempdir().unwrap();
        let wt = base.path().join("wt");
        let dst = base.path().join("dst");
        std::fs::create_dir_all(&wt).unwrap();
        std::os::unix::fs::symlink("nowhere.txt", wt.join("link.txt")).unwrap();

        let (copied, warnings) = copy_out(&wt, &dst, &["link.txt".to_string()]);

        assert_eq!(copied, 0, "{warnings:?}");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("link.txt") && w.contains("nowhere.txt")),
            "no warning names the unarchived link and its target: {warnings:?}"
        );
    }

    /// A directory this walk would have to read in order to learn it doesn't
    /// belong is made unreadable, so pruning it before ever reading it is what
    /// keeps `warnings` empty. A correct-but-unpruned walk would still land on
    /// the same matched file, so only an observation of the read itself — not
    /// of the plan's contents — tells pruning apart from a walk that merely
    /// found nothing else worth reporting.
    #[cfg(unix)]
    #[test]
    fn an_unreachable_directory_below_a_pattern_is_pruned_unread() {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join("apps/web/.env.local"), "x");
        let blocked = src.join("apps/web/node_modules");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root, and some CI containers, ignore directory permissions, which
        // would make this test observe nothing either way. Detect that and
        // skip rather than assert a pass that proves nothing.
        let permissions_are_enforced = std::fs::read_dir(&blocked).is_err();

        let plan = plan_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !permissions_are_enforced {
            return;
        }
        assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
        let missing: Vec<_> = plan.missing().map(Path::to_path_buf).collect();
        assert_eq!(missing, vec![PathBuf::from("apps/web/.env.local")]);
    }
}
