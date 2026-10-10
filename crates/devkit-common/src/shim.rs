//! The command names devkit installs beside itself on PATH.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use strum::{EnumIter, IntoEnumIterator};

/// A name devkit installs beside itself, and the `devkit` subcommand it
/// selects. `docm list` and `devkit docs list` are the same command.
///
/// One enum, iterated everywhere, rather than a list per consumer: the
/// binary's dispatch, its link installer and the command guard all read this,
/// and the guard lives in a crate that cannot see the binary. A name that
/// reached one of them and missed another would be a shim the guard gates or
/// a link nobody creates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, EnumIter)]
pub enum Shim {
    Ticket,
    Workspace,
    Issue,
    Run,
    Ports,
    Locks,
    Docs,
    Mcp,
    Rules,
}

impl Shim {
    /// The executable name on PATH.
    pub fn name(self) -> &'static str {
        match self {
            Shim::Ticket => "ticket",
            Shim::Workspace => "workspace",
            Shim::Issue => "issue",
            Shim::Run => "devrun",
            Shim::Ports => "portm",
            Shim::Locks => "lockm",
            Shim::Docs => "docm",
            Shim::Mcp => "devkit-mcp",
            Shim::Rules => "devrules",
        }
    }

    /// The subcommand name clap registers this under.
    pub fn subcommand(self) -> &'static str {
        match self {
            Shim::Ticket => "ticket",
            Shim::Workspace => "workspace",
            Shim::Issue => "issue",
            Shim::Run => "run",
            Shim::Ports => "ports",
            Shim::Locks => "locks",
            Shim::Docs => "docs",
            Shim::Mcp => "mcp",
            Shim::Rules => "rules",
        }
    }

    /// Whether this name is a hidden alias for commands that moved: it still
    /// runs and is still linked, but help and completions never offer it.
    pub fn is_alias(self) -> bool {
        match self {
            Shim::Issue => true,
            Shim::Ticket
            | Shim::Workspace
            | Shim::Run
            | Shim::Ports
            | Shim::Locks
            | Shim::Docs
            | Shim::Mcp
            | Shim::Rules => false,
        }
    }

    /// The shim `argv0` names, if any. Accepts a bare name or a full path,
    /// with or without a `.exe` extension.
    pub fn from_argv0(argv0: &str) -> Option<Self> {
        // `Path` only splits on `\` when built for Windows, but a
        // Windows-style argv0 must resolve on every CI host, so normalize the
        // separator before handing it to `Path`.
        let normalized = argv0.replace('\\', "/");
        let stem = Path::new(&normalized).file_stem()?.to_str()?;
        Shim::iter().find(|s| s.name() == stem)
    }
}

/// Whether two paths name the same file, so a hardlink is recognized as the
/// file it links. Delegated to the `same-file` crate rather than hand-rolled
/// per-platform metadata comparison: the Windows identity check needs
/// `GetFileInformationByHandle`, which is unsafe FFI, and `same-file` already
/// wraps it safely (as it does the Unix `dev`+`ino` pair). An error resolving
/// either path (e.g. one no longer exists) is "not the same file".
pub fn same_file(a: &Path, b: &Path) -> bool {
    same_file::is_same_file(a, b).unwrap_or(false)
}

/// The file name a shim occupies in a directory: its own name, plus the `.exe`
/// suffix Windows requires to execute it.
pub fn shim_file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// The running devkit binary and the search path a command word resolves
/// against, for telling devkit's own shim links from a foreign program that
/// happens to share a shim name. `devkit install-links` leaves such a program
/// in place, so the name alone says nothing about what runs.
pub struct OwnBinary {
    exe: Option<PathBuf>,
    search_path: Option<OsString>,
    cwd: Option<PathBuf>,
}

impl OwnBinary {
    pub fn new(exe: PathBuf, search_path: Option<OsString>, cwd: Option<PathBuf>) -> Self {
        OwnBinary {
            exe: Some(exe),
            search_path,
            cwd,
        }
    }

    /// This process's binary and `PATH`, with relative program paths resolved
    /// against `cwd`. When the running executable cannot be named, no shim
    /// name resolves to it.
    pub fn current(cwd: Option<PathBuf>) -> Self {
        OwnBinary {
            exe: std::env::current_exe().ok(),
            search_path: std::env::var_os("PATH"),
            cwd,
        }
    }

    /// Whether `program` is devkit itself or one of the names it installs,
    /// resolving to this binary. A shim name is judged by the file it resolves
    /// to, the same-file check `devkit install-links` uses for its own links;
    /// one that resolves elsewhere, or nowhere, is not devkit's.
    pub fn runs(&self, program: &str) -> bool {
        let normalized = program.replace('\\', "/");
        let Some(name) = Path::new(&normalized).file_stem().and_then(|s| s.to_str()) else {
            return false;
        };
        if name == "devkit" {
            return true;
        }
        if Shim::iter().all(|s| s.name() != name) {
            return false;
        }
        let Some(exe) = &self.exe else {
            return false;
        };
        self.resolve(program)
            .is_some_and(|path| same_file(exe, &path))
    }

    /// The file `program` runs: itself when it names a path, else the first
    /// `PATH` entry holding it.
    fn resolve(&self, program: &str) -> Option<PathBuf> {
        if program.contains(['/', '\\']) {
            let path = Path::new(program);
            return Some(match &self.cwd {
                Some(cwd) => cwd.join(path),
                None => path.to_path_buf(),
            });
        }
        let file = shim_file_name(program);
        std::env::split_paths(self.search_path.as_ref()?)
            .map(|dir| dir.join(&file))
            .find(|candidate| candidate.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_bare_shim_name() {
        assert_eq!(Shim::from_argv0("portm"), Some(Shim::Ports));
    }

    #[test]
    fn resolves_a_full_path_with_windows_extension() {
        assert_eq!(
            Shim::from_argv0(r"C:\Users\Lev\.cargo\bin\issue.exe"),
            Some(Shim::Issue)
        );
    }

    #[test]
    fn resolves_a_unix_path() {
        assert_eq!(
            Shim::from_argv0("/home/lev/.cargo/bin/devrun"),
            Some(Shim::Run)
        );
    }

    #[test]
    fn resolves_the_rules_shim() {
        assert_eq!(Shim::from_argv0("devrules"), Some(Shim::Rules));
    }

    /// A hyphenated shim must not be mistaken for its prefix.
    #[test]
    fn devkit_mcp_is_its_own_shim() {
        assert_eq!(Shim::from_argv0("devkit-mcp"), Some(Shim::Mcp));
    }

    /// The tool's own name, and anything unknown, fall through to `devkit`
    /// parsing rather than erroring.
    #[test]
    fn unknown_and_own_name_do_not_resolve() {
        assert!(Shim::from_argv0("devkit").is_none());
        assert!(Shim::from_argv0("devkit.exe").is_none());
        assert!(Shim::from_argv0("some-other-tool").is_none());
        assert!(Shim::from_argv0("").is_none());
    }

    /// Every shim name linked to devkit's binary is devkit's, as is devkit
    /// itself; an unrelated name never is.
    #[test]
    fn devkit_and_every_linked_shim_are_devkit_commands() {
        let exe = std::env::current_exe().unwrap();
        let dir = tempfile::Builder::new()
            .tempdir_in(exe.parent().unwrap())
            .unwrap();
        for s in Shim::iter() {
            std::fs::hard_link(&exe, dir.path().join(shim_file_name(s.name()))).unwrap();
        }
        let own = OwnBinary::new(exe, Some(dir.path().as_os_str().to_owned()), None);
        assert!(own.runs("devkit"));
        for s in Shim::iter() {
            assert!(own.runs(s.name()), "{} is gated", s.name());
        }
        assert!(!own.runs("some-other-tool"));
    }

    /// A shim name held by another program, by path or on `PATH`, or found
    /// nowhere, is not devkit's.
    #[test]
    fn a_shim_name_that_resolves_elsewhere_is_not_devkit() {
        let exe = std::env::current_exe().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let foreign = dir.path().join(shim_file_name("ticket"));
        std::fs::write(&foreign, "foreign").unwrap();
        let own = OwnBinary::new(exe.clone(), Some(dir.path().as_os_str().to_owned()), None);
        assert!(!own.runs("ticket"));
        assert!(!own.runs(foreign.to_str().unwrap()));
        let empty = tempfile::tempdir().unwrap();
        let nowhere = OwnBinary::new(exe, Some(empty.path().as_os_str().to_owned()), None);
        assert!(!nowhere.runs("ticket"));
    }

    /// Two shims sharing a name would make `from_argv0` order-dependent, and
    /// two sharing a subcommand would mean one of them can never be reached.
    #[test]
    fn names_and_subcommands_are_unique() {
        let mut names: Vec<&str> = Shim::iter().map(|s| s.name()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "two shims share a name");

        let mut subs: Vec<&str> = Shim::iter().map(|s| s.subcommand()).collect();
        subs.sort_unstable();
        let count = subs.len();
        subs.dedup();
        assert_eq!(subs.len(), count, "two shims share a subcommand");
    }
}
