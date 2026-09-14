//! Resolve a statically known path against the execution directory, using the
//! rules of the machine the command runs on rather than the analyzer's own.

#![allow(dead_code)]

use crate::{
    context::PathStyle,
    model::{Target, Value},
};

pub(crate) fn is_absolute(p: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Unix => p.starts_with('/'),
        PathStyle::Windows => {
            let b = p.as_bytes();
            p.starts_with(['/', '\\'])
                || (b.len() >= 3
                    && b[0].is_ascii_alphabetic()
                    && b[1] == b':'
                    && matches!(b[2], b'/' | b'\\'))
        }
    }
}

/// What separates path components on the machine the command runs on. A
/// backslash is an ordinary filename character under Unix, so counting it as a
/// separator would read one directory name as two.
fn separators(style: PathStyle) -> &'static [char] {
    match style {
        PathStyle::Unix => &['/'],
        PathStyle::Windows => &['/', '\\'],
    }
}

/// The directory part of a path, keeping a filesystem root as itself. `mktemp`
/// and `fs.mkdtemp` append to a prefix rather than to a directory, so `/fresh-`
/// is made in `/`, not in the working directory.
pub(crate) fn parent_dir(p: &str, style: PathStyle) -> String {
    match p.rfind(separators(style)) {
        None => ".".to_string(),
        Some(0) => p[..1].to_string(),
        Some(2) if style == PathStyle::Windows && has_drive_prefix(p) => format!("{}/", &p[..2]),
        Some(i) => p[..i].to_string(),
    }
}

fn has_drive_prefix(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

fn from_msys(p: &str) -> Option<String> {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/')
        .then(|| format!("{}:{}", (b[1] as char).to_ascii_uppercase(), &p[2..]))
}

pub(crate) fn join(base: &str, rel: &str, style: PathStyle) -> String {
    let rel = rel.strip_prefix("./").unwrap_or(rel);
    if rel == "." || rel.is_empty() {
        return base.to_string();
    }
    let base = match style {
        PathStyle::Unix => base.trim_end_matches('/'),
        PathStyle::Windows => base.trim_end_matches(['/', '\\']),
    };
    format!("{base}/{rel}")
}

pub(crate) fn resolve(value: &Value, cwd: Option<&str>, style: PathStyle) -> Target {
    if let Value::Ephemeral(dir) = value {
        return Target::Ephemeral { dir: dir.clone() };
    }
    let Some(p) = value.known() else {
        return Target::Unresolved;
    };
    if p.is_empty() || p.starts_with('~') || p.contains(['*', '?', '[']) {
        return Target::Unresolved;
    }
    if style == PathStyle::Windows
        && let Some(win) = from_msys(p)
    {
        return Target::Path(win);
    }
    if is_absolute(p, style) {
        return Target::Path(p.to_string());
    }
    match cwd {
        Some(dir) => Target::Path(join(dir, p, style)),
        None => Target::Unresolved,
    }
}

/// Whether appending `parts` to a directory lands inside it. A component that
/// walks up, or one that restarts from the filesystem root, reaches paths the
/// directory never contained, and in Python's `join` an absolute component
/// discards everything before it outright.
pub(crate) fn stays_within(parts: &[&str], style: PathStyle) -> bool {
    let mut depth = 0i32;
    for part in parts {
        // A drive-letter prefix without a separator, `C:foo`, is relative to
        // that drive's own working directory rather than to this one.
        if is_absolute(part, style) || (style == PathStyle::Windows && has_drive_prefix(part)) {
            return false;
        }
        for segment in part.split(separators(style)) {
            match segment {
                "" | "." => {}
                ".." => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                }
                _ => depth += 1,
            }
        }
    }
    true
}

pub(crate) fn parent(p: &str) -> Option<&str> {
    p.rfind(['/', '\\'])
        .map(|i| &p[..i])
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Value {
        Value::Known(s.into())
    }

    #[test]
    fn a_relative_path_joins_the_cwd() {
        assert_eq!(
            resolve(&k("src/a.rs"), Some("/repo"), PathStyle::Unix),
            Target::Path("/repo/src/a.rs".into())
        );
        assert_eq!(
            resolve(&k("./a.rs"), Some("/repo/"), PathStyle::Unix),
            Target::Path("/repo/a.rs".into())
        );
    }

    #[test]
    fn a_relative_dot_path_preserves_a_unix_root_cwd() {
        assert_eq!(
            resolve(&k("."), Some("/"), PathStyle::Unix),
            Target::Path("/".into())
        );
    }

    #[test]
    fn a_relative_dot_path_preserves_a_windows_root_cwd() {
        assert_eq!(
            resolve(&k("."), Some("C:/"), PathStyle::Windows),
            Target::Path("C:/".into())
        );
    }

    #[test]
    fn a_relative_path_without_a_cwd_is_unresolved() {
        assert_eq!(
            resolve(&k("a.rs"), None, PathStyle::Unix),
            Target::Unresolved
        );
    }

    #[test]
    fn an_absolute_path_needs_no_cwd() {
        assert_eq!(
            resolve(&k("/etc/x"), None, PathStyle::Unix),
            Target::Path("/etc/x".into())
        );
        assert_eq!(
            resolve(&k(r"C:\repo\a.txt"), None, PathStyle::Windows),
            Target::Path(r"C:\repo\a.txt".into())
        );
    }

    #[test]
    fn windows_rules_do_not_apply_on_unix() {
        assert_eq!(
            resolve(&k(r"C:\repo\a.txt"), Some("/home/u"), PathStyle::Unix),
            Target::Path(r"/home/u/C:\repo\a.txt".into())
        );
    }

    #[test]
    fn a_git_bash_drive_path_reads_as_a_windows_path() {
        assert_eq!(
            resolve(&k("/c/repo/a.txt"), None, PathStyle::Windows),
            Target::Path("C:/repo/a.txt".into())
        );
    }

    #[test]
    fn containment_rejects_climbing_out_and_restarting_at_the_root() {
        let u = PathStyle::Unix;
        assert!(stays_within(&["out.txt"], u));
        assert!(stays_within(&["sub", "out.txt"], u));
        assert!(stays_within(&["sub/./deep/../out.txt"], u));
        assert!(!stays_within(&["../victim.txt"], u));
        assert!(!stays_within(&["sub/../../victim.txt"], u));
        assert!(!stays_within(&["/repo/victim.txt"], u));
        assert!(!stays_within(&["out.txt", "/repo/victim.txt"], u));
        assert!(!stays_within(&[r"C:\repo\victim.txt"], PathStyle::Windows));
        assert!(!stays_within(&["C:victim.txt"], PathStyle::Windows));
        assert!(!stays_within(&["C:"], PathStyle::Windows));
        assert!(!stays_within(
            &[r"sub\..\..\victim.txt"],
            PathStyle::Windows
        ));
        // A colon is an ordinary filename character off Windows.
        assert!(stays_within(&["C:victim.txt"], PathStyle::Unix));
        // So is a backslash, which makes `a\\b` one directory name, not two.
        assert!(!stays_within(&[r"a\b/../../victim.txt"], PathStyle::Unix));
        assert!(stays_within(&[r"a\b/../victim.txt"], PathStyle::Unix));
    }

    #[test]
    fn a_prefix_directly_under_a_root_keeps_that_root_as_its_parent() {
        assert_eq!(parent_dir("/fresh-", PathStyle::Unix), "/");
        assert_eq!(parent_dir("fresh-", PathStyle::Unix), ".");
        assert_eq!(parent_dir("/tmp/fresh-", PathStyle::Unix), "/tmp");
        assert_eq!(parent_dir(r"C:\fresh-", PathStyle::Windows), "C:/");
        assert_eq!(parent_dir(r"C:\tmp\fresh-", PathStyle::Windows), r"C:\tmp");
    }

    #[test]
    fn globs_tildes_and_unknowns_are_unresolved() {
        for v in [k("src/*.rs"), k("~/x"), k(""), Value::Unknown] {
            assert_eq!(
                resolve(&v, Some("/repo"), PathStyle::Unix),
                Target::Unresolved,
                "{v:?}"
            );
        }
    }
}
