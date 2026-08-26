//! The old command names, and the `devkit` subcommand each one selects.
//!
//! Dispatch reads `argv[0]` rather than `current_exe()`: the two disagree by
//! link type. `current_exe()` resolves a symlink to its target and reports
//! `devkit` for every shim, while `argv[0]` carries the name the caller
//! actually typed under both hardlinks and symlinks.

// The dispatcher that calls `resolve` lives in `main.rs`; this table has no
// caller inside this crate on its own.
#![allow(dead_code)]

use std::path::Path;

pub struct Shim {
    /// The executable name on PATH.
    pub name: &'static str,
    /// The `devkit` subcommand it selects.
    pub subcommand: &'static str,
}

pub const SHIMS: &[Shim] = &[
    Shim {
        name: "issue",
        subcommand: "issue",
    },
    Shim {
        name: "devrun",
        subcommand: "run",
    },
    Shim {
        name: "portm",
        subcommand: "ports",
    },
    Shim {
        name: "lockm",
        subcommand: "locks",
    },
    Shim {
        name: "docm",
        subcommand: "docs",
    },
    Shim {
        name: "devkit-mcp",
        subcommand: "mcp",
    },
];

/// The shim `argv0` names, if any. Accepts a bare name or a full path, with or
/// without a `.exe` extension.
pub fn resolve(argv0: &str) -> Option<&'static Shim> {
    // `Path` only splits on `\` when built for Windows, but a Windows-style
    // argv0 must resolve on every CI host (ubuntu, macos, windows), so
    // normalize the separator before handing it to `Path`.
    let normalized = argv0.replace('\\', "/");
    let stem = Path::new(&normalized).file_stem()?.to_str()?;
    SHIMS.iter().find(|s| s.name == stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_a_bare_shim_name() {
        assert_eq!(resolve("portm").map(|s| s.subcommand), Some("ports"));
    }

    #[test]
    fn resolves_a_full_path_with_windows_extension() {
        assert_eq!(
            resolve(r"C:\Users\Lev\.cargo\bin\issue.exe").map(|s| s.subcommand),
            Some("issue")
        );
    }

    #[test]
    fn resolves_a_unix_path() {
        assert_eq!(
            resolve("/home/lev/.cargo/bin/devrun").map(|s| s.subcommand),
            Some("run")
        );
    }

    /// A hyphenated shim must not be mistaken for its prefix.
    #[test]
    fn devkit_mcp_is_its_own_shim() {
        assert_eq!(resolve("devkit-mcp").map(|s| s.subcommand), Some("mcp"));
    }

    /// The tool's own name, and anything unknown, fall through to `devkit`
    /// parsing rather than erroring.
    #[test]
    fn unknown_and_own_name_do_not_resolve() {
        assert!(resolve("devkit").is_none());
        assert!(resolve("devkit.exe").is_none());
        assert!(resolve("some-other-tool").is_none());
        assert!(resolve("").is_none());
    }
}
