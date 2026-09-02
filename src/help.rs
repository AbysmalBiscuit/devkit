//! The two help views: clap's own rendering, and the full command tree.

use std::io::{self, Write};

/// Longest `about` the full-tree view can print without truncating, given the
/// hundred-column line cap and the longest command path in the tree.
pub const ABOUT_MAX: usize = 70;

/// Longest line the tree emits. Fixed rather than terminal-derived: the tree's
/// reader is usually a pipe, and a fixed width keeps the output deterministic
/// and the test that asserts it meaningful.
pub(crate) const WIDTH: usize = 100;

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
        assert_eq!(
            decide(false, Some("terse"), false).verbosity,
            Verbosity::Terse
        );
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
        assert!(
            warning.contains("loud"),
            "warning names the value: {warning}"
        );
        assert!(
            warning.contains(ENV),
            "warning names the variable: {warning}"
        );
        assert!(decide(false, Some("loud"), false).warning.is_some());
    }

    #[test]
    fn recognized_values_do_not_warn() {
        for env in [None, Some("terse"), Some("full")] {
            assert!(decide(false, env, true).warning.is_none());
        }
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    fn sample() -> clap::Command {
        clap::Command::new("root")
            .about("Root about")
            .after_help("Footer line")
            .subcommand(
                clap::Command::new("group")
                    .about("Group about")
                    .subcommand(clap::Command::new("leaf").about("Leaf about")),
            )
            .subcommand(
                clap::Command::new("hidden")
                    .about("Hidden about")
                    .hide(true),
            )
            .subcommand(clap::Command::new("help").about("Print this message"))
    }

    fn render(cmd: &clap::Command, path: &str) -> String {
        let mut out = Vec::new();
        tree(cmd, path, &mut out).expect("render");
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn the_root_gets_a_line_before_its_children() {
        let text = render(&sample(), "root");
        let first = text.lines().next().expect("a first line");
        assert!(first.starts_with("root "), "root line comes first: {first}");
        assert!(
            first.contains("Root about"),
            "root line carries its about: {first}"
        );
    }

    #[test]
    fn every_path_is_rooted_at_the_invoked_name() {
        let text = render(&sample(), "devkit sub");
        assert!(text.contains("devkit sub group leaf"), "{text}");
        assert!(!text.contains("\nroot"), "no bare command names: {text}");
    }

    #[test]
    fn children_follow_their_parent_in_declaration_order() {
        let text = render(&sample(), "root");
        let group = text.find("root group ").expect("group line");
        let leaf = text.find("root group leaf").expect("leaf line");
        assert!(group < leaf, "a group precedes its own children: {text}");
    }

    #[test]
    fn help_and_hidden_nodes_are_skipped() {
        let text = render(&sample(), "root");
        assert!(!text.contains("root help"), "no help node: {text}");
        assert!(!text.contains("root hidden"), "no hidden node: {text}");
    }

    #[test]
    fn the_after_help_footer_is_appended() {
        assert!(render(&sample(), "root").contains("Footer line"));
    }

    #[test]
    fn a_command_without_after_help_gets_no_footer() {
        let bare = clap::Command::new("bare").about("Bare about");
        assert_eq!(render(&bare, "bare").trim_end(), "bare  Bare about");
    }

    #[test]
    fn lines_stay_inside_the_cap_and_stay_ascii() {
        let long = "x".repeat(ABOUT_MAX * 3);
        let cmd = clap::Command::new("root")
            .about("Root about")
            .subcommand(clap::Command::new("wide").about(long));
        let text = render(&cmd, "root");
        for line in text.lines() {
            assert!(line.chars().count() <= WIDTH, "line over cap: {line}");
            assert!(line.is_ascii(), "line is not ascii: {line}");
        }
        assert!(text.contains("..."), "an over-cap line is marked: {text}");
    }
}

/// Render `cmd` and every subcommand under it, one line per node, as
/// `<path>  <about>`, followed by `cmd`'s `after_help` when it has one.
///
/// `path` is the full invoked path of `cmd` itself, so a shim renders under the
/// name the caller typed: `docm add`, never `add` or `devkit docs add`.
pub fn tree(cmd: &clap::Command, path: &str, out: &mut dyn Write) -> io::Result<()> {
    let mut rows = Vec::new();
    collect(cmd, path.to_string(), &mut rows);
    let pad = rows
        .iter()
        .map(|(p, _)| p.chars().count())
        .max()
        .unwrap_or(0);
    for (path, about) in &rows {
        let line = if about.is_empty() {
            path.clone()
        } else {
            format!("{path:<pad$}  {about}")
        };
        writeln!(out, "{}", truncate(&line))?;
    }
    if let Some(after) = cmd.get_after_help() {
        writeln!(out)?;
        writeln!(out, "{after}")?;
    }
    Ok(())
}

/// Depth-first in declaration order, so a group is immediately followed by its
/// own children and the shape of the CLI survives the flattening.
fn collect(cmd: &clap::Command, path: String, rows: &mut Vec<(String, String)>) {
    let about = cmd.get_about().map(ToString::to_string).unwrap_or_default();
    rows.push((path.clone(), about));
    for sub in cmd.get_subcommands() {
        if sub.get_name() == "help" || sub.is_hide_set() {
            continue;
        }
        collect(sub, format!("{path} {}", sub.get_name()), rows);
    }
}

/// Cut `s` to `WIDTH` columns, marking the cut with an ASCII ellipsis.
///
/// ASCII on purpose: help text reaches the generated PowerShell completion
/// scripts verbatim, and Windows PowerShell 5.1 reads a BOM-less UTF-8 `.ps1`
/// as cp1252, where the trailing byte of `…` becomes a quote character that
/// closes a string early.
fn truncate(s: &str) -> String {
    if s.chars().count() <= WIDTH {
        return s.to_string();
    }
    s.chars().take(WIDTH - 3).collect::<String>() + "..."
}
