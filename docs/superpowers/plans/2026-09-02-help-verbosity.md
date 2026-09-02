# Help verbosity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `devkit --help` and `devkit help` print the whole command tree when stdout is not a terminal, while `-h` stays terse under every condition.

**Architecture:** A new `src/help.rs` in the `devkit` library holds three pieces: a pure `decide` for verbosity, a `tree` renderer over a `clap::Command`, and a `resolve` that answers "which node, and was help asked for" by parsing argv against a probe clone of the command tree. `main()` calls `resolve` after `links::ensure_current` and before `Cli::parse()`, printing and exiting only when the view is full; every other path falls through to clap untouched.

**Tech Stack:** Rust edition 2024, clap 4 (derive), `cargo nextest`, `tempfile` for test scratch.

**Spec:** `docs/superpowers/specs/2026-09-02-help-verbosity-design.md`

## Global Constraints

- Every `about` string in the command tree is at most **70** characters. Prose goes in `long_about` (a blank line in the doc comment).
- Rendered tree lines are capped at **100** columns; truncation marks the cut with ASCII `...`, never `…`. Help text reaches the generated PowerShell scripts verbatim, and Windows PowerShell 5.1 reads a BOM-less UTF-8 `.ps1` as cp1252.
- No `ignore_errors(true)` on the probe. A parse error must decline the intercept so the real parse reports it.
- The intercept goes **after** `links::ensure_current` in `main()`. `docs/install.md` names `devkit --help` as an invocation that creates the shim hardlinks.
- `-h` outranks the TTY signal, `DEVKIT_HELP`, and `--full`, always.
- Verification gate, run before every commit: `cargo nextest run --workspace --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`.
- Commits follow Conventional Commits.

## File Structure

| Path | Responsibility |
|---|---|
| `src/help.rs` | new. `Verbosity`, `Decision`, `decide`, `tree`, `Request`, `resolve`, and the private probe builder. Knows nothing about `Cli` or the shims; everything arrives as a `clap::Command`. |
| `src/lib.rs` | declares the module |
| `src/bin/devkit/main.rs` | `intercept_help`, its call site, the `about` shortening for top-level subcommands, the `about` cap unit test |
| `src/bin/devkit/{docs,locks}.rs`, `src/bin/devkit/{run,issue}/mod.rs` | `about` shortening |
| `src/completions.rs` | `disable_help_subcommand(true)` before `build()` |
| `tests/cli_ergonomics.rs` | help integration tests |
| `tests/shim_dispatch.rs` | existing `portm --help` test, unchanged, now exercising the tree; plus the shim-rooting test |
| `tests/install_links.rs` | the guard that `devkit --help` still creates the shim hardlinks, using that file's isolated-state helpers |
| `tests/completions.rs` | assert no `help` declarations |
| `docs/commands.md`, `docs/agents.md` | user-facing documentation |
| `src/bin/devkit/run/mod.rs` | `available_apps`, and the two `cmd_up` failure paths that call it (Task 8) |
| `tests/up_apps_hint.rs` | new. `devrun up` naming the configured apps on both failure paths (Task 8) |

---

### Task 1: Cap `about` at 70 characters, enable `wrap_help`

**Files:**
- Modify: `Cargo.toml:67`
- Modify: `src/bin/devkit/main.rs` (subcommand doc comments; test module at the bottom)
- Modify: `src/bin/devkit/docs.rs`, `src/bin/devkit/locks.rs`, `src/bin/devkit/ports.rs`, `src/bin/devkit/run/mod.rs`, `src/bin/devkit/issue/mod.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: every `about` in `Cli::command()` is at most 70 characters, which Task 3's renderer relies on to avoid truncating.

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` at the bottom of `src/bin/devkit/main.rs`, beside `every_shim_names_a_real_subcommand`:

```rust
/// The full-tree help view prints one line per node as `<path>  <about>`,
/// capped at a hundred columns. An `about` longer than this budget would be
/// truncated in that view, so the cap is enforced here rather than papered
/// over at render time. Prose belongs in `long_about`, which a leaf's
/// `--help` still prints in full.
#[test]
fn every_about_fits_the_tree_line() {
    fn walk(cmd: &clap::Command, path: &str, over: &mut Vec<String>) {
        if let Some(about) = cmd.get_about() {
            let text = about.to_string();
            if text.chars().count() > devkit::help::ABOUT_MAX {
                over.push(format!("{path} ({} chars): {text}", text.chars().count()));
            }
        }
        for sub in cmd.get_subcommands() {
            walk(sub, &format!("{path} {}", sub.get_name()), over);
        }
    }
    let root = Cli::command();
    let mut over = Vec::new();
    for sub in root.get_subcommands() {
        walk(sub, sub.get_name(), &mut over);
    }
    assert!(
        over.is_empty(),
        "about strings over {} chars:\n{}",
        devkit::help::ABOUT_MAX,
        over.join("\n")
    );
}
```

- [ ] **Step 2: Create the constant the test needs**

The test references `devkit::help::ABOUT_MAX`, which does not exist yet. Create `src/help.rs` with only this, and declare it:

```rust
//! The two help views: clap's own rendering, and the full command tree.

/// Longest `about` the full-tree view can print without truncating, given the
/// hundred-column line cap and the longest command path in the tree.
pub const ABOUT_MAX: usize = 70;
```

Append to `src/lib.rs`:

```rust
pub mod help;
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo nextest run --bin devkit every_about_fits_the_tree_line`
Expected: FAIL, listing 18 over-cap strings, the longest being `auth` at 201 characters. The count includes the hidden `locks hook`, which the test walks because a hidden node still occupies a line in nothing but is still an `about` the cap governs.

Three commands look over cap in `devkit --help` but are not: `docs rm`, `ports alloc`, and `locks status` are inflated by the `[aliases: ...]` suffix clap appends at render time. Their `about` strings are already under the cap; leave them alone.

- [ ] **Step 4: Shorten every over-cap `about`**

For each row, replace the doc comment's first paragraph with the new text. Prose the shortening drops moves into a second paragraph after a blank line, which clap keeps as `long_about`.

Two of these carry no prose worth keeping. `issue` and `run` are enumerations of their own children (`Issue lifecycle: setup, checkout-pr, status, ...`), and the tree below them already lists every one. Shorten those two and add no `long_about`; copying the list into a second paragraph would duplicate what the next lines of the tree say.

| File | Command | New `about` |
|---|---|---|
| `main.rs` | `auth` | `Store a Linear or Slack token, or report the GitHub identity` |
| `main.rs` | `brief` | `Print a project brief for the current checkout` |
| `main.rs` | `schema` | `Print the JSON Schema for` + backticked `devkit.toml` + ` to stdout` |
| `main.rs` | `schema init` | `Point a devkit.toml at the published schema` |
| `main.rs` | `run` | `Supervised dev servers and canned project tasks` |
| `main.rs` | `issue` | `Issue lifecycle: setup, status, review, end` |
| `main.rs` | `install-links` | `Install the old command names as hardlinks beside this binary` |
| `run/mod.rs` | `down` | `Stop servers and release ports` |
| `run/mod.rs` | `reap` | `Kill untracked dev servers (interactive terminal only)` |
| `locks.rs` | `hook` | `Internal: evaluate a coding-agent hook payload` |
| `locks.rs` | `acquire` | `Claim one or more paths; fails if another session holds any` |
| `docs.rs` | `add` | `Register a library by package name or git URL` |
| `docs.rs` | `forget` | `Release this project's reference to libraries` |
| `ports.rs` | `release` | `Release a holder's reservations (default: this worktree)` |
| `issue/mod.rs` | `setup` | `Prepare an issue worktree: branch, setup commands, ports` |
| `issue/mod.rs` | `checkout-pr` | `Check out an existing PR into a new worktree` |
| `issue/mod.rs` | `sync-includes` | `Re-copy worktree_include files into existing worktrees` |
| `issue/mod.rs` | `review finish` | `Announce over Slack that you finished reviewing` |

Worked example, `main.rs` `auth`, before:

```rust
    /// Validate and store a Linear or Slack credential, or report the GitHub
    /// identity devkit would use (GitHub stores nothing, since `gh auth login`
    /// or `GH_TOKEN`/`GITHUB_TOKEN` already cover that credential).
    Auth {
```

after:

```rust
    /// Store a Linear or Slack token, or report the GitHub identity.
    ///
    /// Validates the credential before writing it. GitHub stores nothing,
    /// since `gh auth login` or `GH_TOKEN`/`GITHUB_TOKEN` already cover that
    /// credential; for `github` this reports the identity behind whichever
    /// token resolves.
    Auth {
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo nextest run --bin devkit every_about_fits_the_tree_line`
Expected: PASS

- [ ] **Step 6: Enable `wrap_help`**

`Cargo.toml:67`, add the feature to the existing list:

```toml
clap = { version = "4", default-features = false, features = ["std", "derive", "help", "usage", "error-context", "wrap_help"] }
```

- [ ] **Step 7: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all green. `tests/cli_ergonomics.rs` and `tests/shim_dispatch.rs` still pass: this task changes description text, not structure, and neither asserts on a shortened string.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml src/help.rs src/lib.rs src/bin/devkit/
git commit -m "refactor(cli): cap every about at a phrase, wrap help"
```

---

### Task 2: The verbosity decision

**Files:**
- Modify: `src/help.rs`

**Interfaces:**
- Consumes: `ABOUT_MAX` exists in this module from Task 1.
- Produces: `pub enum Verbosity { Terse, Full }`, `pub struct Decision { pub verbosity: Verbosity, pub warning: Option<String> }`, `pub const ENV: &str`, and `pub fn decide(full_flag: bool, env: Option<&str>, stdout_is_tty: bool) -> Decision`. Task 5 calls `decide` and prints `warning` itself.

- [ ] **Step 1: Write the failing tests**

Append to `src/help.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p devkit decide_tests`
Expected: FAIL to compile, `cannot find function decide in this scope`.

- [ ] **Step 3: Implement**

Add to `src/help.rs`, above the test module:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p devkit decide_tests`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src/help.rs
git commit -m "feat(help): add the verbosity decision"
```

---

### Task 3: The tree renderer

**Files:**
- Modify: `src/help.rs`

**Interfaces:**
- Consumes: `ABOUT_MAX` from Task 1.
- Produces: `pub fn tree(cmd: &clap::Command, path: &str, out: &mut dyn std::io::Write) -> std::io::Result<()>`. `path` is the full invoked path of `cmd` itself, for instance `"devkit"` or `"devkit issue"` or `"docm"`. Task 5 builds that string and calls this.

- [ ] **Step 1: Write the failing tests**

Append to `src/help.rs`:

```rust
#[cfg(test)]
mod tree_tests {
    use super::*;

    fn sample() -> clap::Command {
        clap::Command::new("root")
            .about("Root about")
            .after_help("Footer line")
            .subcommand(
                clap::Command::new("group").about("Group about").subcommand(
                    clap::Command::new("leaf").about("Leaf about"),
                ),
            )
            .subcommand(clap::Command::new("hidden").about("Hidden about").hide(true))
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
        assert!(first.contains("Root about"), "root line carries its about: {first}");
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p devkit tree_tests`
Expected: FAIL to compile, `cannot find function tree in this scope`.

- [ ] **Step 3: Implement**

Add to `src/help.rs`:

```rust
use std::io::{self, Write};

/// Longest line the tree emits. Fixed rather than terminal-derived: the tree's
/// reader is usually a pipe, and a fixed width keeps the output deterministic
/// and the test that asserts it meaningful.
pub(crate) const WIDTH: usize = 100;

/// Render `cmd` and every subcommand under it, one line per node, as
/// `<path>  <about>`, followed by `cmd`'s `after_help` when it has one.
///
/// `path` is the full invoked path of `cmd` itself, so a shim renders under the
/// name the caller typed: `docm add`, never `add` or `devkit docs add`.
pub fn tree(cmd: &clap::Command, path: &str, out: &mut dyn Write) -> io::Result<()> {
    let mut rows = Vec::new();
    collect(cmd, path.to_string(), &mut rows);
    let pad = rows.iter().map(|(p, _)| p.chars().count()).max().unwrap_or(0);
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p devkit tree_tests`
Expected: PASS, 7 tests.

- [ ] **Step 5: Commit**

```bash
git add src/help.rs
git commit -m "feat(help): render the command tree"
```

---

### Task 4: Probe builder and node resolution

**Files:**
- Modify: `src/help.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub struct Request { pub path: Vec<String>, pub short_help: bool, pub full_flag: bool }` and `pub fn resolve(root: &clap::Command, args: &[std::ffi::OsString]) -> Option<Request>`. `path` holds canonical subcommand names from the root down to the target node, empty when the target is the root. Task 5 walks the real tree with it.

- [ ] **Step 1: Write the failing tests**

Append to `src/help.rs`:

```rust
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
        assert_eq!(r.path, ["group"], "help at the group level targets the group");
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
        assert_eq!(req(&["help"]).expect("help request").path, [] as [String; 0]);
        assert_eq!(req(&["help", "group"]).expect("help request").path, ["group"]);
        assert_eq!(
            req(&["help", "group", "status"]).expect("help request").path,
            ["group", "status"]
        );
        assert_eq!(
            req(&["group", "help", "status"]).expect("help request").path,
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p devkit resolve_tests`
Expected: FAIL to compile, `cannot find function resolve in this scope`.

- [ ] **Step 3: Implement**

Add to `src/help.rs`:

```rust
use std::ffi::OsString;

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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p devkit resolve_tests`
Expected: PASS, 11 tests.

- [ ] **Step 5: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all green. Nothing calls `resolve` yet, so no behavior has changed.

- [ ] **Step 6: Commit**

```bash
git add src/help.rs
git commit -m "feat(help): resolve the target node through clap"
```

---

### Task 5: Wire the intercept into `main`

**Files:**
- Modify: `src/bin/devkit/main.rs`
- Modify: `tests/cli_ergonomics.rs`
- Modify: `tests/shim_dispatch.rs`
- Modify: `tests/install_links.rs`

**Interfaces:**
- Consumes: `devkit::help::{decide, resolve, tree, Verbosity, ENV}` from Tasks 2 through 4.
- Produces: the runtime behavior. Nothing later depends on its internals.

- [ ] **Step 1: Write the failing tests**

Append to `tests/cli_ergonomics.rs`. It already imports `std::process::Command`; add `use std::path::Path;` if absent.

```rust
/// Run the built `devkit` with `DEVKIT_HELP` pinned, since `cargo nextest`
/// cannot hand a test a terminal and the piped stdout would otherwise decide
/// the view for us.
fn help_run(view: &str, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
        .args(args)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_HELP", view)
        .output()
        .expect("spawn devkit");
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (text, out.status.success())
}

#[test]
fn the_full_view_descends_into_every_group() {
    let (text, ok) = help_run("full", &["--help"]);
    assert!(ok, "devkit --help failed: {text}");
    assert!(text.contains("devkit docs prune"), "{text}");
    assert!(text.contains("devkit issue review request"), "{text}");
}

#[test]
fn the_terse_view_lists_only_the_top_level() {
    let (text, _) = help_run("terse", &["--help"]);
    assert!(!text.contains("docs prune"), "{text}");
    assert!(!text.contains("issue review request"), "{text}");
}

#[test]
fn short_help_stays_terse_under_a_full_environment() {
    let (text, _) = help_run("full", &["-h"]);
    assert!(!text.contains("docs prune"), "-h is unconditional: {text}");
    assert!(text.contains("issue       = devkit issue"), "{text}");
}

#[test]
fn the_full_view_keeps_the_shim_footer() {
    let (text, _) = help_run("full", &["--help"]);
    assert!(text.contains("issue       = devkit issue"), "{text}");
}

#[test]
fn help_matches_the_long_flag() {
    assert_eq!(help_run("full", &["help"]).0, help_run("full", &["--help"]).0);
    assert_eq!(help_run("terse", &["help"]).0, help_run("terse", &["--help"]).0);
}

#[test]
fn full_outranks_a_terse_environment() {
    let (text, ok) = help_run("terse", &["help", "--full"]);
    assert!(ok, "help --full failed: {text}");
    assert!(text.contains("devkit docs prune"), "{text}");
}

#[test]
fn a_group_renders_only_its_own_subtree() {
    let (text, _) = help_run("full", &["issue", "--help"]);
    assert!(text.contains("devkit issue setup"), "{text}");
    assert!(!text.contains("docs prune"), "no sibling groups: {text}");
}

/// A help flag claims the node it appears under, so a later token that also
/// names a real subcommand does not move the target deeper. `issue setup`
/// appears in a tree rooted at `issue` and could not appear in one rooted at
/// `issue review`, which is what separates the two outcomes.
#[test]
fn the_first_help_wins() {
    let (text, _) = help_run("full", &["issue", "--help", "review"]);
    assert!(
        text.contains("devkit issue setup"),
        "rooted at issue, not at the trailing token: {text}"
    );
    assert!(!text.contains("docs prune"), "not the whole tree: {text}");
}

#[test]
fn an_alias_resolves_to_its_canonical_node() {
    let (text, ok) = help_run("full", &["docs", "remove", "--help"]);
    assert!(ok, "docs remove --help failed: {text}");
    assert!(text.contains("Usage: devkit docs rm"), "{text}");
}

#[test]
fn a_valued_global_flag_does_not_swallow_the_subcommand() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().to_string_lossy().to_string();
    let (text, _) = help_run("full", &["issue", "-C", &path, "status", "--help"]);
    assert!(
        text.contains("Usage: devkit issue status"),
        "resolved to issue status, not the issue root: {text}"
    );
    assert!(!text.contains("devkit issue setup"), "not the issue tree: {text}");
}

#[test]
fn an_unknown_subcommand_is_still_an_error() {
    let (text, ok) = help_run("full", &["issue", "typo", "--help"]);
    assert!(!ok, "an unrecognized subcommand must fail: {text}");
    assert!(text.contains("typo"), "{text}");
}

#[test]
fn a_required_option_does_not_block_a_help_request() {
    let (text, ok) = help_run("full", &["issue", "setup", "--help"]);
    assert!(ok, "issue setup --help failed: {text}");
    assert!(text.contains("--slug"), "{text}");
}

#[test]
fn a_separator_hides_a_later_help() {
    let (text, ok) = help_run("full", &["docs", "path", "--", "--help"]);
    assert!(!ok || !text.contains("devkit docs prune"), "no tree after --: {text}");
}

#[test]
fn full_help_for_a_leaf_prints_its_long_help() {
    let (text, ok) = help_run("full", &["help", "docs", "add", "--full"]);
    assert!(ok, "help docs add --full failed: {text}");
    assert!(text.contains("--eco"), "leaf argument help: {text}");
    assert!(!text.contains("devkit docs prune"), "not a tree: {text}");
}

#[test]
fn short_help_outranks_a_long_help_in_the_same_argv() {
    let (text, _) = help_run("full", &["--help", "-h"]);
    assert!(!text.contains("docs prune"), "-h wins: {text}");
}

#[test]
fn the_three_help_spellings_agree() {
    let a = help_run("full", &["issue", "help", "status"]).0;
    let b = help_run("full", &["help", "issue", "status"]).0;
    let c = help_run("full", &["issue", "status", "--help"]).0;
    assert_eq!(a, b);
    assert_eq!(b, c);
}

#[test]
fn the_full_view_stays_inside_the_line_cap_and_ascii() {
    let (text, _) = help_run("full", &["--help"]);
    for line in text.lines() {
        assert!(line.chars().count() <= 100, "line over cap: {line}");
        assert!(line.is_ascii(), "line is not ascii: {line}");
    }
    assert!(!text.contains("devkit help "), "no help nodes: {text}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run --test cli_ergonomics`
Expected: FAIL. The tree tests fail because `--help` still prints the terse view; `an_unknown_subcommand_is_still_an_error` passes already, which is correct, it is a regression guard.

- [ ] **Step 3: Implement the intercept**

Add to `src/bin/devkit/main.rs`, above `fn main()`:

```rust
/// Answer a help request that the full view owns. Returns `true` when it
/// printed, meaning `main` is done; `false` hands the arguments back to clap
/// untouched, which is what keeps the terse view clap's own rendering.
fn intercept_help(root: &clap::Command, args: &[OsString]) -> Result<bool> {
    let Some(req) = devkit::help::resolve(root, args) else {
        return Ok(false);
    };
    if req.short_help {
        return Ok(false);
    }
    let decision = devkit::help::decide(
        req.full_flag,
        std::env::var(devkit::help::ENV).ok().as_deref(),
        devkit_common::ui::stdout_is_tty(),
    );
    if let Some(warning) = &decision.warning {
        eprintln!("warning: {warning}");
    }
    if decision.verbosity == devkit::help::Verbosity::Terse {
        return Ok(false);
    }

    // Build before walking. `build()` is what assigns each subcommand its
    // `devkit issue status` usage name and copies the parent's `global(true)`
    // arguments down; a subcommand cloned out of an unbuilt tree prints
    // `Usage: status [IDS]...` with no `-C`, `--config` or `--timing`.
    let mut built = root.clone();
    built.build();
    let mut node = built;
    for name in &req.path {
        node = node
            .find_subcommand(name)
            .cloned()
            .with_context(|| format!("no `{name}` subcommand"))?;
    }
    let path = std::iter::once(root.get_name().to_string())
        .chain(req.path.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");

    let mut out = std::io::stdout().lock();
    let printed = if node.get_subcommands().next().is_some() {
        // `build()` added a `help` subcommand at every level; the renderer
        // skips them, so building first costs the tree nothing.
        devkit::help::tree(&node, &path, &mut out)
    } else {
        // A leaf has no tree. Printing its long help directly is also what
        // keeps `--full` away from the real parse, which would reject it.
        node.print_long_help()
    };
    match printed {
        // `devkit --help | head` closes the pipe on us. The reader is done,
        // which is not this command failing; `completions::emit` treats a
        // broken pipe the same way.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(true),
        other => other.map(|()| true).map_err(Into::into),
    }
}
```

Add `use anyhow::Context;` to the imports if absent.

Then in `fn main()`, after the `links::ensure_current(&exe);` block and before `match shim {`:

```rust
    // After the automatic linking pass on purpose: `docs/install.md` promises
    // that running devkit at all creates the shim hardlinks, and names
    // `devkit --help` as an invocation that does it.
    let root = match shim {
        Some(s) => shim_command(s.sub.name(), s.name),
        None => Cli::command(),
    };
    if intercept_help(&root, &args)? {
        return Ok(());
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run --test cli_ergonomics --test shim_dispatch`
Expected: PASS. `devkit_help_names_every_shim` passes on the tree because the tree appends `after_help`; `portm_shim_parses_portm_arguments` passes because the tree prints a root line carrying the `ports` about text.

- [ ] **Step 5: Add the placement guard**

This one goes in `tests/install_links.rs`, not `cli_ergonomics.rs`, and reuses that file's helpers. `staged()` copies the binary into a temp dir beside the build output, and the `HOME`/`XDG_STATE_HOME` overrides are not optional: `links::ensure_current` takes the `links.lock` gate and writes a `links-version` stamp, so without them every run rewrites the developer's real state dir to name a temp path. `retry_on_busy` covers the Windows `ExecutableFileBusy` a just-copied executable can return.

Add it beside `first_run_links_automatically`, which is the same shape with `doctor` in place of `--help`:

```rust
/// `docs/install.md` promises that running devkit at all links the old names
/// beside the executable, and names `devkit --help` as one such invocation.
/// The help intercept sits a few lines below `links::ensure_current`, so
/// moving it above would silently break exactly the command the docs cite.
#[test]
fn full_help_still_links_the_old_names() {
    let (dir, exe) = staged();
    let state = tempfile::tempdir().expect("state dir");
    let out = retry_on_busy(|| {
        Command::new(&exe)
            .arg("--help")
            .env("HOME", state.path())
            .env("XDG_STATE_HOME", state.path())
            .env("DEVKIT_HELP", "full")
            .output()
    });
    assert!(
        out.status.success(),
        "devkit --help failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let docm = shim_path(dir.path(), "docm");
    assert!(docm.exists(), "devkit --help never linked docm");
}
```

- [ ] **Step 6: Run the guard**

Run: `cargo nextest run --test install_links full_help_still_links_the_old_names`
Expected: PASS.

- [ ] **Step 7: Add the shim-rooting tests**

These go in `tests/shim_dispatch.rs`, which already carries `#[path = "common/shimtest.rs"] mod shimtest;` and its `linked()` helper:

```rust
/// Under a shim name the tree roots every path at the name the caller typed,
/// so a line can be copied and run as-is. The scope test is the other half:
/// `docm` shows the docs subtree and nothing else in the CLI.
#[test]
fn the_shim_tree_is_rooted_at_the_shim_name() {
    let (_dir, link) = shimtest::linked("docm");
    let out = Command::new(&link)
        .arg("--help")
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env("DEVKIT_HELP", "full")
        .output()
        .expect("spawn docm --help");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "docm --help exited non-zero: {text}");
    assert!(text.contains("docm add"), "rooted at the shim name: {text}");
    assert!(!text.contains("devkit docs add"), "not the canonical path: {text}");
    assert!(!text.contains("issue setup"), "docs subtree only: {text}");
}
```

- [ ] **Step 8: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all green.

- [ ] **Step 9: Commit**

```bash
git add src/bin/devkit/main.rs tests/cli_ergonomics.rs tests/shim_dispatch.rs tests/install_links.rs
git commit -m "feat(help): print the command tree when stdout is not a tty"
```

---

### Task 6: Drop the `help` subtree from completions

**Files:**
- Modify: `src/completions.rs:104`
- Modify: `tests/completions.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: no declarations under a `help` path in any generated script.

- [ ] **Step 1: Write the failing test**

Append to `tests/completions.rs`:

```rust
/// clap generates a `help` subcommand at every level, and a generator declares
/// each one. In nushell those declarations are commands named `devkit help
/// docs list`, and nushell lists every declaration sharing the `devkit `
/// prefix, so each real command appears twice. Nothing wants completion for
/// them, so they are removed before the tree is built.
#[test]
fn no_generated_script_declares_a_help_path() {
    for shell in ["nushell", "fish", "zsh", "bash", "powershell", "elvish"] {
        let out = Command::new(env!("CARGO_BIN_EXE_devkit"))
            .args(["completions", shell, "--all"])
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .output()
            .unwrap_or_else(|e| panic!("spawn completions {shell}: {e}"));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            !text.contains("devkit help "),
            "{shell} script declares a help subtree"
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run --test completions no_generated_script_declares_a_help_path`
Expected: FAIL on the first shell, reporting that the script declares a help subtree.

- [ ] **Step 3: Implement**

In `src/completions.rs`, inside `emit`'s loop, replace:

```rust
        cmd.set_bin_name(bin_name);
        cmd.build();
```

with:

```rust
        cmd.set_bin_name(bin_name);
        // Before `build()`, which is what creates the per-level `help`
        // subcommands; clap has no way to remove one afterwards. A global
        // setting, so this one call covers every level. `cmd` is this
        // function's own value, so no runtime behavior changes: `devkit help
        // docs` keeps working, it just stops being declared.
        cmd = cmd.disable_help_subcommand(true);
        cmd.build();
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run --test completions`
Expected: PASS.

- [ ] **Step 5: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add src/completions.rs tests/completions.rs
git commit -m "fix(completions): stop declaring the help subtree"
```

---

### Task 7: Document the two views

**Files:**
- Modify: `docs/commands.md`
- Modify: `docs/agents.md`

**Interfaces:**
- Consumes: the shipped behavior from Tasks 1 through 6.
- Produces: nothing code depends on.

- [ ] **Step 1: Add the section to `docs/commands.md`**

Insert near the top, after the opening paragraph about shim names:

```markdown
## Two help views

Every command answers help in two shapes, and which one you get depends on
where stdout points.

| Spelling | At a terminal | Piped or redirected |
|---|---|---|
| `<cmd> -h` | terse | terse |
| `<cmd> --help` | terse | full command tree |
| `<cmd> help` | terse | full command tree |
| `<cmd> help --full` | full command tree | full command tree |

Terse is one line per direct subcommand. The full tree descends through every
level, one line per command, so a coding agent reading help through a pipe
learns the whole surface in a single call instead of one call per group.

`-h` is terse under every condition and is the stable view to reach for. Set
`DEVKIT_HELP=terse` or `DEVKIT_HELP=full` to pin the choice regardless of where
output goes.

The tree is scoped, never cascading: `devkit issue --help` descends through
`issue` and stops. A command with no subcommands keeps clap's own rendering of
its flags and arguments, since a tree cannot carry those.
```

- [ ] **Step 2: Add the note to `docs/agents.md`**

Add under whichever section covers what an agent should run first:

```markdown
Running `devkit --help` through a pipe, which is what a tool call does, prints
the whole command tree rather than the top-level list. One call is enough to
learn every verb. Reach for a specific command's `--help` when you need its
flags, arguments, and gates, which the tree does not carry.
```

- [ ] **Step 3: Verify the documented behavior matches the build**

Run:

```bash
cargo build --release
./target/release/devkit --help | rg -c 'devkit issue review request'
DEVKIT_HELP=terse ./target/release/devkit --help | rg -c 'devkit issue review request'
```

Expected: `1` then `0`.

- [ ] **Step 4: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add docs/commands.md docs/agents.md
git commit -m "docs(help): document the terse and full views"
```

---

### Task 8: Name the configured apps when `devrun up` has none to run

**Files:**
- Modify: `src/bin/devkit/run/mod.rs` (`cmd_up`, and the `#[cfg(test)] mod tests` at the bottom)
- Create: `tests/up_apps_hint.rs`

**Interfaces:**
- Consumes: nothing from the help-verbosity tasks. This task is independent of Tasks 1 through 7.
- Produces: nothing later depends on.

#### Why

`devrun task` with no name lists the configured tasks, so a reader who forgot a
name gets it back immediately. `devrun up` has no such affordance: naming an app
that is not in the catalog gets `unknown app \`web\``, and a bare `devrun up`
whose diff detects nothing gets `no apps to run (none given and none detected in
diff vs origin/main)`. Both are exactly the moment the reader needs the list, and
neither gives it.

Bare `devrun up` keeps inferring apps from the diff. That inference is the
documented behavior and several tests cover it, so this task does not replace it
with a listing. The listing is appended to the two failure paths instead.

#### The helper

Add to `src/bin/devkit/run/mod.rs`, near `apps_from_diff`:

```rust
/// The configured app names, appended to an error that leaves the caller
/// without a usable one. Sorted, because the catalog is a hash map and an
/// arbitrary order reads as noise.
fn available_apps(known: &[String]) -> String {
    if known.is_empty() {
        return "no apps configured (add [apps.<name>] to devkit.toml)".into();
    }
    let mut names: Vec<&str> = known.iter().map(String::as_str).collect();
    names.sort_unstable();
    format!("available apps: {}", names.join(", "))
}
```

#### The two call sites

In `cmd_up`, `known` is already bound a few lines above as
`catalog.keys().cloned().collect()`. Replace the two `anyhow::ensure!` calls:

```rust
    for a in &apps {
        anyhow::ensure!(
            catalog.contains_key(a),
            "unknown app `{a}`\n{}",
            available_apps(&known)
        );
    }
    anyhow::ensure!(
        !apps.is_empty(),
        "no apps to run (none given and none detected in diff vs {})\n{}",
        cfg.defaults.baseline_ref,
        available_apps(&known)
    );
```

Leave every other `unknown app` site alone. The MCP `devrun.up` handler,
`issue setup`, `issue checkout-pr` and `devkit ports` each raise their own; this
task changes `devrun up` only.

#### Step 1: Write the failing unit tests

Add to the existing `#[cfg(test)] mod tests` at the bottom of
`src/bin/devkit/run/mod.rs`:

```rust
#[test]
fn available_apps_sorts_the_names() {
    let known = ["web".to_string(), "api".to_string(), "docs".to_string()];
    assert_eq!(available_apps(&known), "available apps: api, docs, web");
}

#[test]
fn available_apps_says_so_when_none_are_configured() {
    assert_eq!(
        available_apps(&[]),
        "no apps configured (add [apps.<name>] to devkit.toml)"
    );
}
```

#### Step 2: Run them to verify they fail

Run: `cargo nextest run --bin devkit available_apps`
Expected: FAIL to compile, `cannot find function available_apps in this scope`.

#### Step 3: Write the helper and rewire the two call sites

Use the code above.

#### Step 4: Run the unit tests to verify they pass

Run: `cargo nextest run --bin devkit available_apps`
Expected: PASS, 2 tests.

#### Step 5: Write the failing integration test

The error text is what a person reads on a terminal, so it is asserted through
the real binary rather than the helper alone. Create `tests/up_apps_hint.rs`:

```rust
//! `devrun up` names the configured apps on the two paths that leave the
//! caller without one: an app that is not in the catalog, and a bare `up`
//! whose diff detects nothing. Drives `devkit run` directly (not the `devrun`
//! shim). Both paths fail before anything spawns, so no server is ever
//! started. Uses an isolated HOME/XDG_STATE_HOME so the port registry never
//! touches the real one.

use std::path::Path;
use std::process::Command;

/// A temp dir that is a git repo (`cmd_up` resolves the worktree root) with a
/// devkit.toml defining two apps and no tasks. `baseline_ref` names a ref that
/// does not exist here, so the diff-inference pass finds nothing and the bare
/// `up` reaches the "no apps to run" arm.
fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let git = |args: &[&str]| {
        devkit_common::git::Git::fixture(root)
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    };
    git(&["init", "-q"]);
    std::fs::write(
        root.join("devkit.toml"),
        r#"
[defaults]
worktree_root = "wts"
branch_prefix = "x/"
baseline_ref = "origin/main"
baseline_path = "b"

[apps.web]
base_port = 39240
path = "."
launch = ["git", "version"]

[apps.api]
base_port = 39250
path = "."
launch = ["git", "version"]
"#,
    )
    .expect("write devkit.toml");
    dir
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    let state = dir.join("state");
    Command::new(env!("CARGO_BIN_EXE_devkit"))
        .arg("run")
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_STATE_HOME", &state)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("LOCALAPPDATA", &state) // Windows: keep the registry off the real one
        .env("USERPROFILE", dir) // Windows: keep config resolution off the real home
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .output()
        .expect("run devkit run")
}

#[test]
fn an_unknown_app_names_the_configured_ones() {
    let dir = setup();
    let out = run_in(dir.path(), &["up", "nope"]);
    assert!(!out.status.success(), "up with an unknown app should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("unknown app `nope`"),
        "error should name the app that was not found: {err}"
    );
    assert!(
        err.contains("available apps: api, web"),
        "error should list the configured apps in sorted order: {err}"
    );
}

#[test]
fn a_bare_up_with_nothing_detected_names_the_configured_apps() {
    let dir = setup();
    let out = run_in(dir.path(), &["up"]);
    assert!(!out.status.success(), "bare up with no diff should fail");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no apps to run"),
        "error should say nothing was resolved: {err}"
    );
    assert!(
        err.contains("available apps: api, web"),
        "error should list the configured apps in sorted order: {err}"
    );
}
```

#### Step 6: Run the integration tests to verify they pass

Run: `cargo nextest run --test up_apps_hint`
Expected: PASS, 2 tests.

If `a_bare_up_with_nothing_detected_names_the_configured_apps` reaches a
different error than "no apps to run" (for instance the diff inference picking
something up, or the worktree-root resolution failing first), report what it
actually printed rather than reshaping the assertion to match. The fixture is
meant to reach that arm; if it does not, the fixture needs a fix and the
controller wants to know.

#### Step 7: Run the gate

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all`
Expected: all green. `apps_from_diff`'s existing tests are untouched: this task
adds to the error paths and changes no inference behavior.

#### Step 8: Commit

```bash
git add src/bin/devkit/run/mod.rs tests/up_apps_hint.rs
git commit -m "feat(devrun): name configured apps when up has none"
```

#### Step 9: Document it

Check whether `docs/commands.md`'s `devrun up` section describes the failure
behavior. If it does, extend that description to say the error names the
configured apps. If it does not, add nothing: the error text speaks for itself
and a living doc should not restate it. Report which you found and what you did.

---

## Open decisions carried from the spec

Neither blocks implementation; both were left open deliberately.

1. The tree omits aliases. `docs rm` also answers to `remove` and `delete`, and none of that appears. Adding a column that is empty for nearly every row was judged not worth it.
2. `ABOUT_MAX` is a fixed 70 rather than derived at render time from the longest path. Fixed keeps the failing test's message actionable.
