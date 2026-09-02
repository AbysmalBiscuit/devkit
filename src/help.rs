//! The two help views: clap's own rendering, and the full command tree.

/// Longest `about` the full-tree view can print without truncating, given the
/// hundred-column line cap and the longest command path in the tree.
pub const ABOUT_MAX: usize = 70;

/// How much help to print.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verbosity {
    Terse,
    Full,
}

/// A verbosity plus any diagnostic the caller should print. The warning is
/// returned rather than printed so `decide` stays pure and its tests need no
/// captured output.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Decision {
    pub verbosity: Verbosity,
    pub warning: Option<String>,
}

/// Pins the view regardless of where stdout points. Also the seam the
/// integration tests use, since `cargo nextest` cannot hand a test a terminal.
pub const ENV: &str = "DEVKIT_HELP";

/// Precedence: an explicit `--full`, then `DEVKIT_HELP`, then whether stdout is
/// a terminal. An unrecognized env value falls through to the terminal signal
/// rather than failing a help request.
pub fn decide(full_flag: bool, env: Option<&str>, stdout_is_tty: bool) -> Decision {
    let plain = |verbosity| Decision {
        verbosity,
        warning: None,
    };
    if full_flag {
        return plain(Verbosity::Full);
    }
    let from_tty = if stdout_is_tty {
        Verbosity::Terse
    } else {
        Verbosity::Full
    };
    match env {
        None => plain(from_tty),
        Some("terse") => plain(Verbosity::Terse),
        Some("full") => plain(Verbosity::Full),
        Some(other) => Decision {
            verbosity: from_tty,
            warning: Some(format!(
                "{ENV}=`{other}` is neither `terse` nor `full`; ignoring it"
            )),
        },
    }
}

#[cfg(test)]
mod decide_tests {
    use super::*;

    #[test]
    fn full_flag_outranks_everything() {
        for env in [None, Some("terse"), Some("full"), Some("nonsense")] {
            for tty in [true, false] {
                assert_eq!(decide(true, env, tty).verbosity, Verbosity::Full);
            }
        }
    }

    #[test]
    fn env_outranks_the_tty_signal() {
        assert_eq!(decide(false, Some("full"), true).verbosity, Verbosity::Full);
        assert_eq!(decide(false, Some("terse"), false).verbosity, Verbosity::Terse);
    }

    #[test]
    fn tty_decides_when_nothing_else_does() {
        assert_eq!(decide(false, None, true).verbosity, Verbosity::Terse);
        assert_eq!(decide(false, None, false).verbosity, Verbosity::Full);
    }

    #[test]
    fn an_unknown_env_value_warns_and_falls_through() {
        let d = decide(false, Some("loud"), true);
        assert_eq!(d.verbosity, Verbosity::Terse);
        let warning = d.warning.expect("unknown value warns");
        assert!(warning.contains("loud"), "warning names the value: {warning}");
        assert!(warning.contains(ENV), "warning names the variable: {warning}");
        assert!(decide(false, Some("loud"), false).warning.is_some());
    }

    #[test]
    fn recognized_values_do_not_warn() {
        for env in [None, Some("terse"), Some("full")] {
            assert!(decide(false, env, true).warning.is_none());
        }
    }
}
