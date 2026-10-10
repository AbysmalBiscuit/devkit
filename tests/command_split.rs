//! The `ticket`, `workspace` and `pr` commands, and the hidden `issue` alias.
//!
//! Which verb each spelling reaches is pinned in `src/bin/devkit/main.rs`;
//! these run the built binary to show the names parse from a command line.

#[path = "common/shimtest.rs"]
mod shimtest;
#[path = "common/testenv.rs"]
mod testenv;

use std::process::Output;

fn devkit(args: &[&str]) -> (tempfile::TempDir, Output) {
    let (home, mut cmd) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
    let out = cmd.args(args).output().expect("spawn devkit");
    (home, out)
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Every verb under its new name, under `devkit` and under the name `ticket pr`
/// gives it. `-h` answers from clap's own parse, so success means the path
/// resolved to a real leaf.
#[test]
fn every_new_spelling_parses() {
    let paths: &[&[&str]] = &[
        &["ticket", "create"],
        &["ticket", "edit"],
        &["ticket", "render"],
        &["ticket", "event"],
        &["ticket", "dashboard"],
        &["workspace", "setup"],
        &["workspace", "status"],
        &["workspace", "end"],
        &["workspace", "sync-includes"],
        &["pr", "create"],
        &["pr", "render"],
        &["pr", "ready"],
        &["pr", "status"],
        &["pr", "checkout"],
        &["pr", "list"],
        &["pr", "review", "request"],
        &["pr", "review", "finish"],
        &["ticket", "pr", "create"],
        &["ticket", "pr", "render"],
        &["ticket", "pr", "ready"],
        &["ticket", "pr", "status"],
        &["ticket", "pr", "checkout"],
        &["ticket", "pr", "list"],
        &["ticket", "pr", "review", "request"],
        &["ticket", "pr", "review", "finish"],
    ];
    for path in paths {
        let mut args = path.to_vec();
        args.push("-h");
        let (_home, out) = devkit(&args);
        assert!(
            out.status.success(),
            "`devkit {}` did not parse: {}",
            args.join(" "),
            text(&out)
        );
    }
}

/// The old verbs, as the `issue` alias spells them.
const OLD_VERBS: &[&str] = &[
    "setup",
    "status",
    "end",
    "sync-includes",
    "create",
    "edit",
    "render",
    "event",
    "dashboard",
    "pr",
    "prs",
    "review",
    "info",
    "checkout-pr",
];

/// Help an agent reads to learn a verb names that verb's new command, never
/// the `issue` spelling it replaced.
#[test]
fn no_verb_help_names_the_issue_alias() {
    let paths: &[&[&str]] = &[
        &["ticket"],
        &["ticket", "create"],
        &["ticket", "edit"],
        &["ticket", "render"],
        &["ticket", "event"],
        &["ticket", "dashboard"],
        &["workspace"],
        &["workspace", "setup"],
        &["workspace", "status"],
        &["workspace", "end"],
        &["workspace", "sync-includes"],
        &["pr"],
        &["pr", "create"],
        &["pr", "render"],
        &["pr", "ready"],
        &["pr", "status"],
        &["pr", "checkout"],
        &["pr", "list"],
        &["pr", "review"],
        &["pr", "review", "request"],
        &["pr", "review", "finish"],
    ];
    for path in paths {
        for flag in ["-h", "--help"] {
            let mut args = path.to_vec();
            args.push(flag);
            let (_home, out) = devkit(&args);
            let help = text(&out);
            assert!(out.status.success(), "{}: {help}", args.join(" "));
            for verb in OLD_VERBS {
                assert!(
                    !help.contains(&format!("issue {verb} "))
                        && !help.contains(&format!("issue {verb}`")),
                    "`devkit {}` names `issue {verb}`: {help}",
                    args.join(" ")
                );
            }
        }
    }
}

/// The `ticket` and `workspace` links parse as their own roots.
#[test]
fn the_new_links_parse_their_own_verbs() {
    for (name, verbs) in [
        (
            "ticket",
            &["create", "edit", "render", "event", "dashboard"][..],
        ),
        (
            "workspace",
            &["setup", "status", "end", "sync-includes"][..],
        ),
    ] {
        let (_dir, link) = shimtest::linked(name);
        for verb in verbs {
            let (_home, mut cmd) = testenv::isolated(&link);
            let out = cmd.args([verb, "-h"]).output().expect("spawn link");
            assert!(
                out.status.success(),
                "`{name} {verb} -h` did not parse: {}",
                text(&out)
            );
            assert!(
                String::from_utf8_lossy(&out.stdout).contains(&format!("Usage: {name} {verb}")),
                "`{name} {verb} -h` should be rooted at `{name}`: {}",
                text(&out)
            );
        }
    }
    let (_dir, link) = shimtest::linked("ticket");
    let (_home, mut cmd) = testenv::isolated(&link);
    let out = cmd
        .args(["pr", "review", "request", "-h"])
        .output()
        .expect("spawn ticket");
    assert!(
        out.status.success(),
        "`ticket pr review request -h` did not parse: {}",
        text(&out)
    );
}

/// The old spellings that moved or were renamed still parse.
#[test]
fn every_old_spelling_parses() {
    let paths: &[&[&str]] = &[
        &["issue", "create"],
        &["issue", "edit"],
        &["issue", "render"],
        &["issue", "event"],
        &["issue", "dashboard"],
        &["issue", "setup"],
        &["issue", "status"],
        &["issue", "end"],
        &["issue", "sync-includes"],
        &["issue", "pr", "create"],
        &["issue", "pr", "render"],
        &["issue", "pr", "ready"],
        &["issue", "pr", "status"],
        &["issue", "pr", "checkout"],
        &["issue", "prs"],
        &["issue", "review", "request"],
        &["issue", "review", "finish"],
        &["issue", "info"],
        &["issue", "checkout-pr"],
    ];
    for path in paths {
        let mut args = path.to_vec();
        args.push("-h");
        let (_home, out) = devkit(&args);
        assert!(
            out.status.success(),
            "`devkit {}` did not parse: {}",
            args.join(" "),
            text(&out)
        );
    }
}

/// `issue` is gone from every help view of `devkit`, its footer included, and
/// the names that replace it are there.
#[test]
fn devkit_help_never_names_issue() {
    for view in ["full", "terse"] {
        let (home, mut cmd) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
        let out = cmd
            .env("DEVKIT_HELP", view)
            .arg("--help")
            .output()
            .expect("spawn devkit --help");
        drop(home);
        let help = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(out.status.success(), "devkit --help failed: {}", text(&out));
        let names = |name: &str| {
            help.lines().map(str::trim_start).any(|l| {
                l.starts_with(&format!("{name} "))
                    || l.starts_with(&format!("devkit {name} "))
                    || l.contains(&format!("= devkit {name}"))
            })
        };
        assert!(!names("issue"), "{view} help names `issue`: {help}");
        for name in ["ticket", "workspace", "pr"] {
            assert!(names(name), "{view} help never names `{name}`: {help}");
        }
    }
}

/// A hidden alias still runs: bare `devkit issue` renders the same status
/// table bare `devkit workspace` does.
#[test]
fn bare_issue_still_runs_and_matches_bare_workspace() {
    let project = tempfile::tempdir().expect("project dir");
    devkit_git::Git::fixture(project.path())
        .args(["init", "-q"])
        .output()
        .expect("git init");
    let run = |args: &[&str]| {
        let (_home, mut cmd) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
        cmd.args(args)
            .current_dir(project.path())
            .output()
            .expect("spawn devkit")
    };
    let issue = run(&["issue"]);
    let workspace = run(&["workspace"]);
    assert!(issue.status.success(), "bare issue: {}", text(&issue));
    assert!(
        workspace.status.success(),
        "bare workspace: {}",
        text(&workspace)
    );
    assert!(
        String::from_utf8_lossy(&workspace.stdout).contains("ISSUE WORKTREES"),
        "bare workspace should render the status table: {}",
        text(&workspace)
    );
    assert_eq!(issue.stdout, workspace.stdout);
}
