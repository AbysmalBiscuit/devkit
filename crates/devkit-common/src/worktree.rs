use crate::git::{self, Worktree};
use anyhow::Result;
use std::path::{Path, PathBuf};

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
/// never aborts worktree creation. Returns (files_copied, warnings).
pub fn copy_includes(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>) {
    let plan = plan_includes(source, dest, patterns);
    let (copied, apply_warnings) = apply_includes(source, dest, &plan, false);
    let mut warnings = plan.warnings;
    warnings.extend(apply_warnings);
    (copied, warnings)
}

/// Copy the files an `IncludePlan` found: every path in `missing`, and every
/// path in `existing` only when `overwrite` is true. Both vectors hold paths
/// relative to `dest`, as `plan_includes` returns them. A plan is a snapshot,
/// so unless `overwrite` is set each copy re-checks the destination and skips a
/// file that appeared since. Fail-open, like `plan_includes`: a copy error is
/// collected as a warning string rather than propagated. Returns
/// (files_copied, warnings).
pub fn apply_includes(
    source: &Path,
    dest: &Path,
    plan: &IncludePlan,
    overwrite: bool,
) -> (usize, Vec<String>) {
    let mut copied = 0usize;
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

    (copied, warnings)
}

/// The result of walking `patterns` without copying anything: every matched
/// file sorted by whether `dest` already has it, so a caller can prompt before
/// clobbering. Paths are relative to `dest`.
pub struct IncludePlan {
    pub missing: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

/// Walk `patterns` the way `copy_includes` does, but classify matches instead
/// of copying them: each matched file lands in `missing` or `existing`
/// depending on whether `dest` already has it. Both vectors come back sorted,
/// so a caller's rendering does not vary with filesystem iteration order. A
/// directory match contributes its files recursively, never the directory
/// entry itself. Fail-open, like
/// `copy_includes`: a bad glob, an unreadable directory, or a non-UTF-8
/// pattern becomes a warning string.
pub fn plan_includes(source: &Path, dest: &Path, patterns: &[String]) -> IncludePlan {
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        require_literal_leading_dot: false,
    };
    let mut missing = Vec::new();
    let mut existing = Vec::new();
    let mut warnings = Vec::new();

    for pattern in patterns {
        let trimmed = pattern.trim_end_matches('/');
        let joined = source.join(trimmed);
        let Some(pat_str) = joined.to_str() else {
            warnings.push(format!("include pattern is not valid UTF-8: {pattern}"));
            continue;
        };
        let entries = match glob::glob_with(pat_str, opts) {
            Ok(paths) => paths,
            Err(e) => {
                warnings.push(format!("bad include pattern `{pattern}`: {e}"));
                continue;
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
            let target = dest.join(rel);
            if matched.is_dir() {
                plan_dir(
                    &matched,
                    rel,
                    dest,
                    &mut missing,
                    &mut existing,
                    &mut warnings,
                );
            } else {
                classify_file(&target, rel, &mut missing, &mut existing);
            }
        }
    }
    missing.sort();
    existing.sort();
    IncludePlan {
        missing,
        existing,
        warnings,
    }
}

fn classify_file(dst: &Path, rel: &Path, missing: &mut Vec<PathBuf>, existing: &mut Vec<PathBuf>) {
    if dst.exists() {
        existing.push(rel.to_path_buf());
    } else {
        missing.push(rel.to_path_buf());
    }
}

/// Recursively classify a directory's files without writing anything. `rel`
/// tracks the path relative to `dest` in lockstep with `src` so classified
/// paths stay dest-relative.
fn plan_dir(
    src: &Path,
    rel: &Path,
    dest: &Path,
    missing: &mut Vec<PathBuf>,
    existing: &mut Vec<PathBuf>,
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
        let target = dest.join(&child_rel);
        if child.is_dir() {
            plan_dir(&child, &child_rel, dest, missing, existing, warnings);
        } else {
            classify_file(&target, &child_rel, missing, existing);
        }
    }
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

        let (n, warnings) = copy_includes(&src, &dst, &["apps/*/.env.local".to_string()]);

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

        let (n, _) = copy_includes(&src, &dst, &["**/.env.local".to_string()]);

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
        let (n, warnings) = copy_includes(&src, &dst, &[".claude/hooks/".to_string()]);

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

        let (n, warnings) = copy_includes(&src, &dst, &["does/not/exist".to_string()]);

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

        let (n, _) = copy_includes(&src, &dst, &[".tool-versions".to_string()]);

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

        assert_eq!(plan.missing, vec![PathBuf::from(".env.local")]);
        assert!(plan.existing.is_empty());
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

        assert_eq!(plan.existing, vec![PathBuf::from(".tool-versions")]);
        assert!(plan.missing.is_empty());
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

        assert_eq!(plan.existing, vec![PathBuf::from(".claude/hooks/pre.sh")]);
        assert_eq!(
            plan.missing,
            vec![PathBuf::from(".claude/hooks/sub/post.sh")]
        );
        assert!(plan.warnings.is_empty());
    }

    /// `read_dir` order is the filesystem's, so an unsorted plan would print a
    /// different `copied N file(s)` list on every run and platform.
    #[test]
    fn plan_vectors_come_back_sorted() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        for name in ["d.sh", "b.sh", "a.sh", "c.sh"] {
            write(&src.join("hooks").join(name), "x");
        }
        write(&dst.join("hooks/c.sh"), "old");
        write(&dst.join("hooks/a.sh"), "old");

        let plan = plan_includes(&src, &dst, &["hooks/".to_string()]);

        assert_eq!(
            plan.missing,
            vec![PathBuf::from("hooks/b.sh"), PathBuf::from("hooks/d.sh")]
        );
        assert_eq!(
            plan.existing,
            vec![PathBuf::from("hooks/a.sh"), PathBuf::from("hooks/c.sh")]
        );
    }

    #[test]
    fn plan_includes_pattern_matching_nothing_yields_empty_plan() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        fs::create_dir_all(&src).unwrap();

        let plan = plan_includes(&src, &dst, &["does/not/exist".to_string()]);

        assert!(plan.missing.is_empty());
        assert!(plan.existing.is_empty());
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn plan_includes_bad_glob_warns_without_panicking() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        fs::create_dir_all(&src).unwrap();

        let plan = plan_includes(&src, &dst, &["[".to_string()]);

        assert!(plan.missing.is_empty());
        assert!(plan.existing.is_empty());
        assert_eq!(plan.warnings.len(), 1);
    }

    #[test]
    fn apply_includes_without_overwrite_leaves_existing_untouched() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        write(&src.join(".tool-versions"), "node 20");
        write(&dst.join(".tool-versions"), "KEEP ME");

        let plan = plan_includes(&src, &dst, &[".tool-versions".to_string()]);
        let (n, warnings) = apply_includes(&src, &dst, &plan, false);

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
        assert_eq!(plan.missing, vec![PathBuf::from(".tool-versions")]);
        write(&dst.join(".tool-versions"), "KEEP ME");

        let (n, warnings) = apply_includes(&src, &dst, &plan, false);

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
        let (n, warnings) = apply_includes(&src, &dst, &plan, true);

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

        let (n, warnings) = copy_includes(&src, &dst, &[]);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
        assert!(!dst.exists());
    }
}
