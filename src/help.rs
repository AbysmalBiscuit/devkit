//! The two help views: clap's own rendering, and the full command tree.

use std::ffi::OsString;
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
/// as cp1252, where the trailing byte of a UTF-8 em dash, ellipsis or arrow
/// decodes to a curly quote that closes a string early.
fn truncate(s: &str) -> String {
    if s.chars().count() <= WIDTH {
        return s.to_string();
    }
    s.chars().take(WIDTH - 3).collect::<String>() + "..."
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

/// Argument ids the probe adds. Prefixed so they cannot collide with a real
/// argument id in any subcommand.
const ID_HELP: &str = "devkit_probe_help";
const ID_SHORT: &str = "devkit_probe_h";
const ID_FULL: &str = "devkit_probe_full";

/// A resolved help request.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// Canonical subcommand names from the root down to the target node, empty
    /// when the target is the root itself. Canonical, so an alias like
    /// `remove` arrives as `rm`.
    pub path: Vec<String>,
    /// A `-h` appeared. The caller renders terse and ignores everything else.
    pub short_help: bool,
    /// A `--full` appeared anywhere in the request.
    pub full_flag: bool,
}

fn long_flag(id: &'static str, long: &'static str) -> clap::Arg {
    clap::Arg::new(id)
        .long(long)
        .action(clap::ArgAction::SetTrue)
        .hide(true)
}

fn short_flag(id: &'static str, short: char) -> clap::Arg {
    clap::Arg::new(id)
        .short(short)
        .action(clap::ArgAction::SetTrue)
        .hide(true)
}

/// The `help` subcommand the probe defines in place of clap's, so `--full`
/// parses instead of erroring.
fn help_node() -> clap::Command {
    clap::Command::new("help")
        .arg(clap::Arg::new("path").num_args(0..))
        .arg(long_flag(ID_HELP, "help"))
        .arg(short_flag(ID_SHORT, 'h'))
        .arg(long_flag(ID_FULL, "full"))
}

/// Add the probe's own arguments to one node, then recurse.
///
/// The help arguments are per-node, never global: a global argument propagates
/// its value down the whole chain, which would erase *which* level asked for
/// help and make `root group --help status` target `status` instead of
/// `group`. Required arguments and required subcommands are cleared so a help
/// request parses cleanly; that is what removes the need for `ignore_errors`,
/// which would also swallow an unrecognized subcommand and turn invalid argv
/// into successful help output.
///
/// `required(false)` alone is not enough. `issue setup`'s positional is
/// `required_unless_present = "issue"`, a separate condition clap evaluates on
/// its own, so clearing it takes an explicit reset. `required_unless_present_all`,
/// `required_if_eq*` and required `ArgGroup`s are unused in this CLI today; a
/// future one needs the matching reset here, and the test that catches it is
/// `a_required_option_does_not_block_a_help_request`.
fn per_node(cmd: clap::Command) -> clap::Command {
    let has_subs = cmd.get_subcommands().next().is_some();
    let mut cmd = cmd
        .subcommand_required(false)
        .arg_required_else_help(false)
        .mut_args(|a| {
            a.required(false)
                .required_unless_present(clap::builder::Resettable::Reset)
        })
        .mut_subcommands(per_node)
        .arg(long_flag(ID_HELP, "help"))
        .arg(short_flag(ID_SHORT, 'h'))
        .arg(long_flag(ID_FULL, "full"));
    // Only where clap would have generated one, so a leaf keeps its
    // positional: `docs add help` registers a library named `help`.
    if has_subs {
        cmd = cmd.subcommand(help_node());
    }
    cmd
}

fn probe(root: &clap::Command) -> clap::Command {
    per_node(root.clone())
        .disable_help_flag(true)
        .disable_help_subcommand(true)
}

/// Resolve a help request out of `args`, or `None` when this is not a help
/// request or the arguments do not parse. Declining on a parse error is what
/// leaves an unrecognized subcommand for the real parse to report.
pub fn resolve(root: &clap::Command, args: &[OsString]) -> Option<Request> {
    let matches = probe(root).try_get_matches_from(args).ok()?;

    let mut path: Vec<String> = Vec::new();
    let mut short_help = matches.get_flag(ID_SHORT);
    let mut full_flag = matches.get_flag(ID_FULL);
    // `-h` counts as a help request too, even though the caller then renders
    // terse: without it a bare `-h` resolves to nothing and the short-help
    // precedence rule never gets a request to apply to.
    let mut help_depth = (matches.get_flag(ID_HELP) || short_help).then_some(0usize);
    let mut help_sub: Option<Vec<String>> = None;

    let mut cur = &matches;
    while let Some((name, sub)) = cur.subcommand() {
        short_help |= sub.get_flag(ID_SHORT);
        full_flag |= sub.get_flag(ID_FULL);
        if name == "help" {
            help_sub = Some(
                sub.get_many::<String>("path")
                    .map(|v| v.cloned().collect())
                    .unwrap_or_default(),
            );
            break;
        }
        path.push(name.to_string());
        if (sub.get_flag(ID_HELP) || sub.get_flag(ID_SHORT)) && help_depth.is_none() {
            help_depth = Some(path.len());
        }
        cur = sub;
    }

    // A help flag outranks the `help` subcommand's positionals, so
    // `root --help help group` targets the root. That is the same
    // first-help-wins rule clap applies to `root --help group`, and keeping
    // the two spellings on one rule is what stops them disagreeing.
    let target = match (help_depth, help_sub) {
        (Some(depth), _) => path[..depth].to_vec(),
        (None, Some(rest)) => {
            let mut t = path;
            t.extend(rest);
            t
        }
        (None, None) => return None,
    };

    Some(Request {
        path: target,
        short_help,
        full_flag,
    })
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use std::ffi::OsString;

    /// A stand-in for the real tree carrying the shapes that break a
    /// hand-written argv walker: a value-taking global flag, an alias, a
    /// required option, and a leaf with a positional.
    fn sample() -> clap::Command {
        clap::Command::new("root")
            .arg(
                clap::Arg::new("dir")
                    .short('C')
                    .long("dir")
                    .global(true)
                    .num_args(1),
            )
            .subcommand(
                clap::Command::new("group")
                    .subcommand(clap::Command::new("status"))
                    .subcommand(clap::Command::new("rm").visible_alias("remove"))
                    // Mirrors the real `issue setup`: a positional that is
                    // `required_unless_present`, not plainly `required`. A
                    // probe that only clears `required` still fails here.
                    .subcommand(
                        clap::Command::new("setup")
                            .arg(clap::Arg::new("slug").long("slug").num_args(1))
                            .arg(
                                clap::Arg::new("slug_pos")
                                    .required_unless_present("slug")
                                    .conflicts_with("slug"),
                            ),
                    )
                    .subcommand(
                        clap::Command::new("add").arg(clap::Arg::new("target").required(true)),
                    ),
            )
    }

    fn req(argv: &[&str]) -> Option<Request> {
        let args: Vec<OsString> = std::iter::once("root")
            .chain(argv.iter().copied())
            .map(OsString::from)
            .collect();
        resolve(&sample(), &args)
    }

    #[test]
    fn a_valued_global_flag_does_not_swallow_the_subcommand() {
        let r = req(&["group", "-C", "status", "status", "--help"]).expect("help request");
        assert_eq!(r.path, ["group", "status"]);
    }

    #[test]
    fn an_unknown_subcommand_declines_so_the_real_parse_errors() {
        assert!(req(&["group", "typo", "--help"]).is_none());
    }

    #[test]
    fn a_required_option_does_not_block_a_help_request() {
        let r = req(&["group", "setup", "--help"]).expect("help request");
        assert_eq!(r.path, ["group", "setup"]);
    }

    #[test]
    fn the_first_help_wins() {
        let r = req(&["group", "--help", "status"]).expect("help request");
        assert_eq!(
            r.path,
            ["group"],
            "help at the group level targets the group"
        );
    }

    #[test]
    fn a_separator_hides_a_later_help() {
        assert!(req(&["group", "add", "--", "--help"]).is_none());
    }

    #[test]
    fn a_leaf_positional_named_help_is_not_a_help_request() {
        assert!(req(&["group", "add", "help"]).is_none());
    }

    #[test]
    fn an_alias_resolves_to_the_canonical_name() {
        let r = req(&["group", "remove", "--help"]).expect("help request");
        assert_eq!(r.path, ["group", "rm"]);
    }

    #[test]
    fn short_help_is_reported_separately() {
        assert!(req(&["group", "-h"]).expect("help request").short_help);
        assert!(!req(&["group", "--help"]).expect("help request").short_help);
        assert!(req(&["--help", "-h"]).expect("help request").short_help);
    }

    #[test]
    fn the_help_subcommand_names_the_target() {
        assert_eq!(
            req(&["help"]).expect("help request").path,
            [] as [String; 0]
        );
        assert_eq!(
            req(&["help", "group"]).expect("help request").path,
            ["group"]
        );
        assert_eq!(
            req(&["help", "group", "status"])
                .expect("help request")
                .path,
            ["group", "status"]
        );
        assert_eq!(
            req(&["group", "help", "status"])
                .expect("help request")
                .path,
            ["group", "status"]
        );
    }

    #[test]
    fn a_help_flag_outranks_the_help_subcommand() {
        let r = req(&["--help", "help", "group"]).expect("help request");
        assert_eq!(r.path, [] as [String; 0], "the flag came first");
    }

    #[test]
    fn full_is_read_from_anywhere_in_a_help_request() {
        assert!(req(&["help", "--full"]).expect("help request").full_flag);
        assert!(req(&["--help", "--full"]).expect("help request").full_flag);
        assert!(
            req(&["help", "group", "add", "--full"])
                .expect("help request")
                .full_flag
        );
        assert!(!req(&["--help"]).expect("help request").full_flag);
    }
}
