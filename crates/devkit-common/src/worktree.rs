use crate::cmd::git;
use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String, // "DETACHED" if none
}

/// Parse `git worktree list --porcelain` output. First entry is the main repo.
pub fn parse_porcelain(out: &str) -> Vec<Worktree> {
    let mut all = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let flush = |p: &mut Option<String>, b: &mut Option<String>, v: &mut Vec<Worktree>| {
        if let Some(pp) = p.take() {
            v.push(Worktree {
                path: PathBuf::from(pp),
                branch: b.take().unwrap_or_else(|| "DETACHED".into()),
            });
        }
    };
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            flush(&mut path, &mut branch, &mut all);
            path = Some(p.to_string());
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            branch = Some(b.to_string());
        }
    }
    flush(&mut path, &mut branch, &mut all);
    all
}

/// Derive an `ENG-1234`-style id from a branch or directory name, uppercased.
pub fn issue_id_of(branch: &str, path: &std::path::Path) -> String {
    let dir = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    for src in [branch, dir] {
        if let Some(m) = find_id(src) {
            return m.to_uppercase();
        }
    }
    "UNKNOWN".into()
}

/// The first letters-dash-digits run in `s` (e.g. `eng-1234`), if any.
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
                    return Some(format!("{}-{}", &s[start..dash], &s[ds..i]));
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
    let out = git(&["worktree", "list", "--porcelain"], start)?;
    let mut all = parse_porcelain(&out);
    anyhow::ensure!(!all.is_empty(), "not inside a git repo: {start}");
    let main = all.remove(0);
    Ok((main.path, all))
}

/// Copy files matching `patterns` (path globs relative to `source`) into `dest`
/// at the same relative path. A match that is a directory is copied recursively.
/// Patterns that match nothing are silently skipped; a destination file that
/// already exists is left untouched (never clobbered). Fail-open: a glob or copy
/// error is collected as a warning string rather than propagated, so backfill
/// never aborts worktree creation. Returns (files_copied, warnings).
pub fn copy_includes(source: &Path, dest: &Path, patterns: &[String]) -> (usize, Vec<String>) {
    let opts = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: false,
        // Match dotfiles with wildcards, mirroring shell `dotglob`.
        require_literal_leading_dot: false,
    };
    let mut copied = 0usize;
    let mut warnings = Vec::new();

    for pattern in patterns {
        // A trailing slash signals a directory (gitignore idiom); strip it so the
        // glob matches the directory entry, then recurse because it is a dir.
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
                copy_dir(&matched, &target, &mut copied, &mut warnings);
            } else {
                copy_file(&matched, &target, &mut copied, &mut warnings);
            }
        }
    }
    (copied, warnings)
}

/// Copy a single file, skipping if the destination already exists. Errors are
/// pushed as warnings.
fn copy_file(src: &Path, dst: &Path, copied: &mut usize, warnings: &mut Vec<String>) {
    if dst.exists() {
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

/// Recursively copy a directory's files, skipping existing destinations.
fn copy_dir(src: &Path, dst: &Path, copied: &mut usize, warnings: &mut Vec<String>) {
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
        let child = entry.path();
        let target = dst.join(entry.file_name());
        if child.is_dir() {
            copy_dir(&child, &target, copied, warnings);
        } else {
            copy_file(&child, &target, copied, warnings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(issue_id_of("lev/eng-1234-fix", Path::new("/x")), "ENG-1234");
        assert_eq!(issue_id_of("DETACHED", Path::new("/x/abc-9")), "ABC-9");
        assert_eq!(issue_id_of("main", Path::new("/x/scratch")), "UNKNOWN");
    }

    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("devkit-incl-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn copies_a_matching_file_preserving_relative_path() {
        let base = tmp("file");
        let src = base.join("src");
        let dst = base.join("dst");
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
        let base = tmp("nested");
        let src = base.join("src");
        let dst = base.join("dst");
        write(&src.join("a/b/c/.env.local"), "X=1");

        let (n, _) = copy_includes(&src, &dst, &["**/.env.local".to_string()]);

        assert_eq!(n, 1);
        assert!(dst.join("a/b/c/.env.local").exists());
    }

    #[test]
    fn directory_pattern_copies_recursively() {
        let base = tmp("dir");
        let src = base.join("src");
        let dst = base.join("dst");
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
        let base = tmp("nomatch");
        let src = base.join("src");
        let dst = base.join("dst");
        fs::create_dir_all(&src).unwrap();

        let (n, warnings) = copy_includes(&src, &dst, &["does/not/exist".to_string()]);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
    }

    #[test]
    fn existing_destination_file_is_not_clobbered() {
        let base = tmp("noclobber");
        let src = base.join("src");
        let dst = base.join("dst");
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
    fn empty_patterns_is_a_no_op() {
        let base = tmp("empty");
        let src = base.join("src");
        let dst = base.join("dst");
        fs::create_dir_all(&src).unwrap();

        let (n, warnings) = copy_includes(&src, &dst, &[]);

        assert_eq!(n, 0);
        assert!(warnings.is_empty());
        assert!(!dst.exists());
    }
}
