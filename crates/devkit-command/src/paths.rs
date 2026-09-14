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
        if is_absolute(part, style) {
            return false;
        }
        for segment in part.split(['/', '\\']) {
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
