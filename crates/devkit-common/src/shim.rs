//! The command names devkit installs beside itself on PATH.

use std::path::Path;

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
            Shim::Issue => "issue",
            Shim::Run => "run",
            Shim::Ports => "ports",
            Shim::Locks => "locks",
            Shim::Docs => "docs",
            Shim::Mcp => "mcp",
            Shim::Rules => "rules",
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

/// Whether `name` is devkit itself or one of the names it installs. The
/// command guard never gates these: routing work to them is its whole purpose.
pub fn is_devkit_command(name: &str) -> bool {
    name == "devkit" || Shim::iter().any(|s| s.name() == name)
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

    /// The guard's allow-list covers devkit itself as well as every shim.
    #[test]
    fn devkit_and_every_shim_are_devkit_commands() {
        assert!(is_devkit_command("devkit"));
        for s in Shim::iter() {
            assert!(is_devkit_command(s.name()), "{} is gated", s.name());
        }
        assert!(!is_devkit_command("some-other-tool"));
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
