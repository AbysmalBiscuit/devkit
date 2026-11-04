//! Look a command word up the way a shell runs it.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

/// The first file that `program`, run from `cwd`, names and that `accept`
/// takes. A `program` with a path separator names itself, relative to `cwd`.
/// On Windows that path with `.exe` also counts when it has no extension.
/// Any other `program` names that file in each `search_path` entry, in order.
/// A relative entry, the empty one included, is a directory under `cwd`. With
/// `cwd` unknown, a relative path or entry names nothing.
pub fn find_program(
    program: &str,
    search_path: Option<&OsStr>,
    cwd: Option<&Path>,
    accept: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let under_cwd = |path: &Path| match path.is_absolute() {
        true => Some(path.to_path_buf()),
        false => cwd.map(|cwd| cwd.join(path)),
    };
    if program.contains(['/', '\\']) {
        let path = under_cwd(Path::new(program))?;
        let exe = (cfg!(windows) && path.extension().is_none()).then(|| path.with_extension("exe"));
        return std::iter::once(path)
            .chain(exe)
            .find(|candidate| accept(candidate));
    }
    let file = file_name(program);
    std::env::split_paths(search_path?)
        .filter_map(|dir| under_cwd(&dir))
        .map(|dir| dir.join(&file))
        .find(|candidate| accept(candidate))
}

/// The file a bare command word runs. Windows drops a `.exe` the word ends
/// with and appends its own, so `ticket` and `ticket.EXE` find one file.
fn file_name(program: &str) -> String {
    let stem = match program.len().checked_sub(4) {
        Some(at)
            if cfg!(windows)
                && program.is_char_boundary(at)
                && program[at..].eq_ignore_ascii_case(".exe") =>
        {
            &program[..at]
        }
        _ => program,
    };
    crate::shim::shim_file_name(stem)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn tool_in(dir: &Path) -> PathBuf {
        let file = dir.join(file_name("tool"));
        std::fs::write(&file, "").unwrap();
        file
    }

    #[test]
    fn a_relative_search_path_entry_is_under_cwd() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join("bin")).unwrap();
        let tool = tool_in(&cwd.path().join("bin"));
        let path = OsString::from("bin");
        assert_eq!(
            find_program("tool", Some(&path), Some(cwd.path()), Path::is_file),
            Some(tool)
        );
        assert_eq!(find_program("tool", Some(&path), None, Path::is_file), None);
    }

    #[test]
    fn an_empty_search_path_entry_is_cwd() {
        let cwd = tempfile::tempdir().unwrap();
        let tool = tool_in(cwd.path());
        let other = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([other.path(), Path::new("")]).unwrap();
        assert_eq!(
            find_program("tool", Some(&path), Some(cwd.path()), Path::is_file),
            Some(tool)
        );
    }

    #[test]
    fn the_first_accepted_entry_wins() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        tool_in(first.path());
        let tool = tool_in(second.path());
        let path = std::env::join_paths([first.path(), second.path()]).unwrap();
        assert_eq!(
            find_program("tool", Some(&path), None, |p| p.starts_with(second.path())),
            Some(tool)
        );
    }

    #[test]
    fn a_path_names_itself_and_ignores_the_search_path() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join("bin")).unwrap();
        let tool = tool_in(&cwd.path().join("bin"));
        let program = format!("bin/{}", file_name("tool"));
        assert_eq!(
            find_program(&program, None, Some(cwd.path()), Path::is_file),
            Some(tool)
        );
        assert_eq!(find_program(&program, None, None, Path::is_file), None);
    }
}
