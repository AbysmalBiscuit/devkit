use crate::git::{self, Worktree};
use anyhow::Result;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

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

    /// Recursively classify a directory's files without writing anything. `rel`
    /// tracks the path relative to `dest` in lockstep with `src` so classified
    /// paths stay dest-relative.
    fn plan_dir(&self, src: &Path, rel: &Path, out: &mut PatternPlan, warnings: &mut Vec<String>) {
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
            if self.mode == LinkMode::Preserve && is_symlink(&child) {
                self.classify_link(&child_rel, &child, out, warnings);
            } else if child.is_dir() {
                self.plan_dir(&child, &child_rel, out, warnings);
            } else {
                self.classify_file(&child_rel, out);
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
        // An empty pattern joins to `source` itself, which globs to the source
        // directory and strips to an empty relative path, planning every file
        // under the root. Drop it before the join rather than after.
        if trimmed.is_empty() {
            return;
        }
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
            // A link is classified before any is_dir test, which follows it and
            // would send a symlinked directory into the recursion.
            if self.mode == LinkMode::Preserve && is_symlink(&matched) {
                self.classify_link(rel, &matched, out, warnings);
            } else if matched.is_dir() {
                self.plan_dir(&matched, rel, out, warnings);
            } else {
                self.classify_file(rel, out);
            }
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
/// order. Fail-open, like `copy_includes`: a bad glob, an unreadable
/// directory, or a non-UTF-8 pattern becomes a warning string.
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
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };
    let walk = Walk {
        dest,
        on,
        found: std::cell::Cell::new(0),
        mode,
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
/// is left untouched. Errors are pushed as warnings.
fn copy_file(
    src: &Path,
    dst: &Path,
    overwrite: bool,
    copied: &mut usize,
    warnings: &mut Vec<String>,
) {
    if !overwrite && dst.exists() {
        return;
    }
    if let Some(parent) = dst.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warnings.push(format!("creating {}: {e}", parent.display()));
        return;
    }
    match std::fs::copy(src, dst) {
        Ok(_) => *copied += 1,
        Err(e) => warnings.push(format!(
            "copying {} -> {}: {e}",
            src.display(),
            dst.display()
        )),
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

fn is_glob(pattern: &str) -> bool {
    pattern.contains(['*', '?', '['])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::parse_porcelain;
    use std::path::Path;
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
}
