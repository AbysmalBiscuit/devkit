//! The repro gate: a `Done when` item ending `(proof: repro <selector>)` is
//! proven only by a test that fails without the fix and passes with it. The
//! check runs in a throwaway worktree at HEAD with the branch's non-test
//! changes restored to the merge base, so dependencies and fixtures stay in
//! place and the caller's worktree and index are never touched.

use std::{
    io::{Read, Seek},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};
use devkit_common::{
    caller::Caller,
    progress::Steps,
    test_files::TestFiles,
    vcs::{NewWorktree, Vcs, VersionControl},
};
use devkit_config::{Gate as GateKey, GateMode, PrGates, ReproTask};

use super::{
    verify::output_tail_within,
    weakened::scan::{self, Lang},
};

/// The most of a run's output one item quotes, in bytes, so a PR with
/// several items stays well within GitHub's body limit.
const EXCERPT_BUDGET: usize = 2 * 1024;

/// The template variable `repro_task` takes the selector as.
const SELECTOR: &str = "selector";

/// One `devrun task` run of a selector.
#[derive(Debug, Clone)]
struct Ran {
    exit: Option<i32>,
    output: Vec<String>,
}

/// What the check found for one `Done when` item.
#[derive(Debug)]
enum Outcome {
    /// The item carries no repro marker.
    Unmarked,
    /// The check could not judge the item, for the reason given.
    Inconclusive(String),
    /// The check could not run.
    Error(String),
    /// The selector ran with the fix reverted and at HEAD.
    Ran {
        selector: String,
        base: Ran,
        head: Ran,
    },
}

/// How a pair of runs reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Passed,
    PassesReverted,
    NoTests,
    NoTestsReverted,
    FailsAtHead,
}

impl Verdict {
    fn of(base: &Ran, head: &Ran, no_tests_exit: Option<i32>) -> Self {
        let no_tests = |r: &Ran| no_tests_exit.is_some() && r.exit == no_tests_exit;
        if no_tests(head) {
            Self::NoTests
        } else if base.exit == Some(0) {
            Self::PassesReverted
        } else if no_tests(base) {
            Self::NoTestsReverted
        } else if head.exit != Some(0) {
            Self::FailsAtHead
        } else {
            Self::Passed
        }
    }

    /// Whether the verdict refuses an agent's PR under `require`.
    fn refuses(self) -> bool {
        match self {
            Self::PassesReverted | Self::NoTests => true,
            Self::Passed | Self::NoTestsReverted | Self::FailsAtHead => false,
        }
    }
}

struct Item {
    number: usize,
    title: String,
    outcome: Outcome,
}

/// The repro gate for one `pr create`.
pub(crate) struct Gate {
    mode: GateMode,
    caller: Caller,
    task: ReproTask,
    head: String,
    merge_base: String,
    /// The non-test paths the check restored or deleted.
    reverted: Vec<String>,
    items: Vec<Item>,
    /// Why the issue's items could not be read, when they could not.
    unread: Option<String>,
}

/// What reverting the fix does to the throwaway worktree.
#[derive(Debug, Default, PartialEq, Eq)]
struct Revert {
    /// Paths to set to their merge-base contents.
    restore: Vec<String>,
    /// Paths the branch added, to delete.
    delete: Vec<String>,
    /// Changed paths holding both test and source changes.
    mixed: Vec<String>,
}

impl Gate {
    /// The gate `gates` configures for a PR opened at `head`, checking the
    /// items `items` returns: `None` when the gate is off, names no task,
    /// or the worktree's issue lists no items.
    pub(crate) fn read(
        dir: &Path,
        merge_base: &str,
        gates: &PrGates,
        test_files: &[String],
        head: &str,
        caller: Caller,
        items: impl FnOnce() -> Result<Option<Vec<String>>>,
    ) -> Result<Option<Self>> {
        let Some((task, mode)) = gates.all().into_iter().find_map(|(gate, mode)| match gate {
            GateKey::Repro(task) => Some((task, mode)),
            GateKey::WeakenedTests | GateKey::Verify(_) => None,
        }) else {
            return Ok(None);
        };
        if mode == GateMode::Off {
            return Ok(None);
        }
        let mut gate = Self {
            mode,
            caller,
            task,
            head: head.to_string(),
            merge_base: merge_base.to_string(),
            reverted: Vec::new(),
            items: Vec::new(),
            unread: None,
        };
        let texts = match items() {
            Ok(Some(texts)) if !texts.is_empty() => texts,
            Ok(_) => return Ok(None),
            Err(e) => {
                gate.unread = Some(format!("{e:#}"));
                return Ok(Some(gate));
            }
        };
        let marked: Vec<(usize, Option<String>)> = texts
            .iter()
            .enumerate()
            .filter_map(|(i, t)| marker(t).map(|sel| (i, sel)))
            .collect();
        let mut outcomes: Vec<Outcome> = texts.iter().map(|_| Outcome::Unmarked).collect();
        if !marked.is_empty() {
            let tests = TestFiles::new(test_files)?;
            match gate.check(dir, &tests, &marked) {
                Ok(found) => {
                    for (i, outcome) in found {
                        outcomes[i] = outcome;
                    }
                }
                Err(e) => {
                    for (i, _) in &marked {
                        outcomes[*i] = Outcome::Error(format!("{e:#}"));
                    }
                }
            }
        }
        gate.items = texts
            .iter()
            .zip(outcomes)
            .enumerate()
            .map(|(i, (text, outcome))| Item {
                number: i + 1,
                title: title(text).to_string(),
                outcome,
            })
            .collect();
        Ok(Some(gate))
    }

    /// The outcome of each item of `marked`, by its index, with the
    /// selector it names.
    fn check(
        &mut self,
        dir: &Path,
        tests: &TestFiles,
        marked: &[(usize, Option<String>)],
    ) -> Result<Vec<(usize, Outcome)>> {
        let revert = revert(dir, &self.merge_base, &self.head, tests)?;
        let mut out = Vec::new();
        let mut runnable = Vec::new();
        for (i, selector) in marked {
            match selector {
                Some(selector) => runnable.push((*i, selector.clone())),
                None => out.push((
                    *i,
                    Outcome::Inconclusive(
                        "the marker names no selector: write it `(proof: repro <selector>)`".into(),
                    ),
                )),
            }
        }
        let why_not = if !revert.mixed.is_empty() {
            Some(format!(
                "{} {} both test and source changes, so reverting the fix would revert tests \
                 too",
                quoted(&revert.mixed),
                if revert.mixed.len() == 1 {
                    "holds"
                } else {
                    "hold"
                },
            ))
        } else if revert.restore.is_empty() && revert.delete.is_empty() {
            Some("the branch changes no non-test path, so there is no fix to revert".into())
        } else {
            None
        };
        if let Some(why) = why_not {
            out.extend(
                runnable
                    .into_iter()
                    .map(|(i, _)| (i, Outcome::Inconclusive(why.clone()))),
            );
            return Ok(out);
        }
        if runnable.is_empty() {
            return Ok(out);
        }
        let ran = self.run(dir, &revert, &runnable)?;
        self.reverted = revert.restore.into_iter().chain(revert.delete).collect();
        self.reverted.sort();
        self.reverted.dedup();
        out.extend(
            runnable
                .into_iter()
                .zip(ran)
                .map(|((i, selector), (head, base))| {
                    (i, Outcome::Ran {
                        selector,
                        base,
                        head,
                    })
                }),
        );
        Ok(out)
    }

    /// Each selector of `runnable` run at HEAD, then with `revert` applied,
    /// in a throwaway worktree.
    fn run(
        &self,
        dir: &Path,
        revert: &Revert,
        runnable: &[(usize, String)],
    ) -> Result<Vec<(Ran, Ran)>> {
        let steps = Steps::new();
        let worktree = Throwaway::new(dir, &self.head)?;
        let at = &worktree.path();
        let mut heads = Vec::new();
        for (i, selector) in runnable {
            heads.push(steps.during_result(
                &format!("Running item {}'s repro at HEAD\u{2026}", i + 1),
                || self.devrun(at, selector),
            )?);
        }
        let vcs = Vcs::at(at);
        vcs.restore_paths(at, &self.merge_base, &revert.restore)?;
        for path in &revert.delete {
            match std::fs::remove_file(at.join(path)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("deleting {path} in {}", at.display()));
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for ((i, selector), head) in runnable.iter().zip(heads) {
            let base = steps.during_result(
                &format!(
                    "Running item {}'s repro with the fix reverted\u{2026}",
                    i + 1
                ),
                || self.devrun(at, selector),
            )?;
            out.push((head, base));
        }
        Ok(out)
    }

    /// `devrun task <repro_task> --arg selector=<selector>` in `at`, which
    /// records the run in the ledger.
    fn devrun(&self, at: &Path, selector: &str) -> Result<Ran> {
        let exe = std::env::current_exe().context("finding the devkit executable")?;
        let mut log = tempfile::tempfile().context("creating a file for the run's output")?;
        let status = Command::new(&exe)
            .args(["run", "task", &self.task.task, "--arg"])
            .arg(format!("{SELECTOR}={selector}"))
            .current_dir(at)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log.try_clone()?)
            .status()
            .with_context(|| format!("running `{}`", self.task.command(selector)))?;
        log.rewind()?;
        let mut bytes = Vec::new();
        log.read_to_end(&mut bytes)?;
        let output = String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_string)
            .collect();
        Ok(Ran {
            exit: status.code(),
            output,
        })
    }

    /// `body` with the repro checks appended, under the Verification
    /// section `verification` says the body already ends with, or a new
    /// one. Refused when an agent's PR under `require` has an item whose
    /// selector passes with the fix reverted or matches no test.
    pub(crate) fn apply(&self, body: String, verification: bool) -> Result<String> {
        if let (GateMode::Require, Caller::Agent) = (self.mode, self.caller) {
            let refused: Vec<String> = self
                .items
                .iter()
                .filter_map(|item| match &item.outcome {
                    Outcome::Ran {
                        selector,
                        base,
                        head,
                    } => {
                        let verdict = Verdict::of(base, head, self.task.no_tests_exit);
                        verdict.refuses().then(|| {
                            format!(
                                "  {}. {}: `{}` {}",
                                item.number,
                                item.title,
                                self.task.command(selector),
                                self.verdict_text(verdict, base, head)
                            )
                        })
                    }
                    Outcome::Unmarked | Outcome::Inconclusive(_) | Outcome::Error(_) => None,
                })
                .collect();
            if !refused.is_empty() {
                bail!(
                    "refusing to open the PR: the `repro` gate needs each item marked `(proof: \
                     repro <selector>)` to name a test that fails with the branch's non-test \
                     changes reverted and passes at HEAD:\n{}\n\
                     Point each selector at a test that fails without the fix, then retry",
                    refused.join("\n")
                );
            }
        }
        let body = body.trim_end();
        let heading = if verification {
            ""
        } else {
            "### Verification\n\n"
        };
        let section = format!("{heading}#### Repro checks\n\n{}", self.render());
        Ok(if body.is_empty() {
            section
        } else {
            format!("{body}\n\n{section}")
        })
    }

    fn render(&self) -> String {
        if let Some(why) = &self.unread {
            return format!("NOT RUN: the issue's `Done when` items could not be read: {why}\n");
        }
        let mut out = String::new();
        if !self.reverted.is_empty() {
            out.push_str(&format!(
                "Each selector ran through `{}` in a throwaway worktree at HEAD `{}`, then again \
                 with {} restored to the merge base `{}`.\n\n",
                self.task.task,
                self.head,
                quoted(&self.reverted),
                short(&self.merge_base),
            ));
        }
        for item in &self.items {
            out.push_str(&self.render_item(item));
        }
        out
    }

    fn render_item(&self, item: &Item) -> String {
        let lead = format!("{}. {}", item.number, item.title);
        match &item.outcome {
            Outcome::Unmarked => format!(
                "{lead}: no `(proof: repro <selector>)` marker, so nothing checked that its test \
                 fails without the fix.\n"
            ),
            Outcome::Inconclusive(why) => format!("{lead}: INCONCLUSIVE: {why}.\n"),
            Outcome::Error(why) => format!("{lead}: NOT RUN: the check could not run: {why}\n"),
            Outcome::Ran {
                selector,
                base,
                head,
            } => {
                let verdict = Verdict::of(base, head, self.task.no_tests_exit);
                let status = match verdict {
                    Verdict::Passed => "PASSED",
                    Verdict::PassesReverted | Verdict::NoTests => "FAILED",
                    Verdict::NoTestsReverted => "INCONCLUSIVE",
                    Verdict::FailsAtHead => "FAILS AT HEAD",
                };
                let mut out = format!(
                    "{lead}: {status}: `{}` {}\n   - With the fix reverted: {}\n   - At HEAD \
                     `{}`: {}\n",
                    self.task.command(selector),
                    self.verdict_text(verdict, base, head),
                    exited(base),
                    self.head,
                    exited(head),
                );
                let excerpt = match verdict {
                    Verdict::Passed | Verdict::PassesReverted | Verdict::NoTestsReverted => {
                        ("Output with the fix reverted", base)
                    }
                    Verdict::NoTests | Verdict::FailsAtHead => ("Output at HEAD", head),
                };
                out.push_str(&excerpt_block(excerpt.0, &excerpt.1.output));
                out
            }
        }
    }

    fn verdict_text(&self, verdict: Verdict, base: &Ran, head: &Ran) -> String {
        match verdict {
            Verdict::Passed => "fails with the fix reverted and passes at HEAD.".into(),
            Verdict::PassesReverted => {
                "passes with the fix reverted, so it does not show the fix.".into()
            }
            Verdict::NoTests => format!(
                "matches no test: it {} at HEAD, the exit code `repro_no_tests_exit` names.",
                exited(head)
            ),
            Verdict::NoTestsReverted => format!(
                "matches no test with the fix reverted: it {}, the exit code \
                 `repro_no_tests_exit` names, so the test lives in a path reverted with the fix.",
                exited(base)
            ),
            Verdict::FailsAtHead => "fails at HEAD as well as with the fix reverted.".into(),
        }
    }
}

/// A detached worktree at a commit, removed when dropped.
struct Throwaway {
    main: PathBuf,
    holder: tempfile::TempDir,
}

impl Throwaway {
    fn new(dir: &Path, head: &str) -> Result<Self> {
        let main = devkit_common::vcs::primary_checkout(dir)?;
        let holder = tempfile::Builder::new()
            .prefix("devkit-repro-")
            .tempdir()
            .context("creating a directory for the throwaway worktree")?;
        let worktree = Self { main, holder };
        Vcs::at(dir)
            .create_worktree(&NewWorktree {
                main: &worktree.main,
                path: &worktree.path(),
                start: head,
                branch: None,
            })
            .context("creating the throwaway worktree")?;
        Ok(worktree)
    }

    fn path(&self) -> PathBuf {
        self.holder.path().join("repro")
    }
}

impl Drop for Throwaway {
    fn drop(&mut self) {
        let vcs = Vcs::at(&self.main);
        if vcs.remove_worktree(&self.main, &self.path(), true).is_err() {
            let _ = std::fs::remove_dir_all(self.path());
            let _ = vcs.prune(&self.main);
        }
    }
}

/// What reverting the branch's non-test changes between `merge_base` and
/// `head` takes. A changed file whose change is all inline tests is left
/// alone, as a test file is.
fn revert(dir: &Path, merge_base: &str, head: &str, tests: &TestFiles) -> Result<Revert> {
    let vcs = Vcs::at(dir);
    let mut out = Revert::default();
    for change in vcs.changed_files(dir, merge_base, head)? {
        let old = change.old.filter(|p| !tests.matches(p));
        let new = change.new.filter(|p| !tests.matches(p));
        let Some(label) = new.clone().or_else(|| old.clone()) else {
            continue;
        };
        let read = |rev: &str, p: &Option<String>| -> Result<Option<String>> {
            match p {
                Some(p) => vcs.file_at(dir, rev, p),
                None => Ok(None),
            }
        };
        let before = read(merge_base, &old)?;
        let after = read(head, &new)?;
        match split(&label, before.as_deref(), after.as_deref()) {
            Split::Tests => continue,
            Split::Both => {
                out.mixed.push(label);
                continue;
            }
            Split::Source => {}
        }
        if let Some(old) = &old {
            out.restore.push(old.clone());
        }
        if let Some(new) = new.filter(|n| old.as_ref() != Some(n)) {
            out.delete.push(new);
        }
    }
    out.mixed.sort();
    Ok(out)
}

/// Which side of a changed non-test file its change touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Split {
    Source,
    Tests,
    Both,
}

/// How the change from `before` to `after` of the file at `path` splits
/// between its source and the tests inline in it. A Rust file's tests are
/// everything from its first `#[cfg(test)]` line on; in another language
/// the scanner reads, a change in its count of test declarations or
/// assertions counts as both, since the two cannot be told apart.
fn split(path: &str, before: Option<&str>, after: Option<&str>) -> Split {
    let (before, after) = (before.unwrap_or(""), after.unwrap_or(""));
    match Lang::of(path) {
        Some(Lang::Rust) => {
            let (src_was, tests_was) = rust_parts(before);
            let (src_now, tests_now) = rust_parts(after);
            match (src_was != src_now, tests_was != tests_now) {
                (_, false) => Split::Source,
                (false, true) => Split::Tests,
                (true, true) => Split::Both,
            }
        }
        Some(lang) => {
            let (was, now) = (scan::counts(lang, before), scan::counts(lang, after));
            if was.tests != now.tests || was.assertions != now.assertions {
                Split::Both
            } else {
                Split::Source
            }
        }
        None => Split::Source,
    }
}

/// `text` split at the start of its first `#[cfg(test)]` line.
fn rust_parts(text: &str) -> (&str, &str) {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("#[cfg(test)]") {
            return text.split_at(at);
        }
        at += line.len();
    }
    (text, "")
}

/// The selector an item ending `(proof: repro <selector>)` names: `None`
/// for an item with no repro marker, `Some(None)` for one naming no
/// selector. Backticks around the selector are dropped.
fn marker(item: &str) -> Option<Option<String>> {
    let proof = proof_of(item)?;
    let rest = proof.strip_prefix("repro")?;
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    let selector = rest.trim();
    let selector = selector
        .strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .unwrap_or(selector)
        .trim();
    Some((!selector.is_empty()).then(|| selector.to_string()))
}

/// The text of an item's trailing `(proof: ...)`.
fn proof_of(item: &str) -> Option<&str> {
    let item = item.trim_end().strip_suffix(')')?;
    let at = item.rfind("(proof:")?;
    Some(item[at + "(proof:".len()..].trim())
}

/// An item's text without its trailing `(proof: ...)`.
fn title(item: &str) -> &str {
    match proof_of(item) {
        Some(_) => item[..item.rfind("(proof:").expect("proof_of found it")].trim_end(),
        None => item.trim_end(),
    }
}

fn exited(ran: &Ran) -> String {
    match ran.exit {
        Some(code) => format!("exited {code}"),
        None => "was killed by a signal".into(),
    }
}

/// `lines` as a fenced block indented into a list item, labelled `label`.
fn excerpt_block(label: &str, lines: &[String]) -> String {
    if lines.is_empty() {
        return format!("\n   {label}: none.\n\n");
    }
    let block = output_tail_within(lines, EXCERPT_BUDGET);
    let block = block
        .strip_prefix("\nOutput tail:\n")
        .unwrap_or(&block)
        .trim_end_matches('\n');
    let indented: String = block
        .lines()
        .map(|l| {
            if l.is_empty() {
                "\n".to_string()
            } else {
                format!("   {l}\n")
            }
        })
        .collect();
    format!("\n   {label}:\n{indented}\n")
}

fn quoted(paths: &[String]) -> String {
    let quoted: Vec<String> = paths.iter().map(|p| format!("`{p}`")).collect();
    match quoted.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(12)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_names_its_selector() {
        assert_eq!(
            marker("Fixed (proof: repro tests::a_fix)"),
            Some(Some("tests::a_fix".into()))
        );
        assert_eq!(
            marker("Fixed (proof: repro `pkg  tests/a b.rs`)  "),
            Some(Some("pkg  tests/a b.rs".into()))
        );
        assert_eq!(marker("Fixed (proof: repro )"), Some(None));
        assert_eq!(marker("Fixed (proof: repro)"), Some(None));
        assert_eq!(marker("Fixed (proof: test repro)"), None);
        assert_eq!(marker("Fixed (proof: reproduce x)"), None);
        assert_eq!(marker("Fixed"), None);
        assert_eq!(marker(""), None);
    }

    #[test]
    fn the_title_drops_the_proof() {
        assert_eq!(title("Fixed (proof: repro x)"), "Fixed");
        assert_eq!(title("Fixed"), "Fixed");
    }

    #[test]
    fn a_rust_file_splits_at_its_test_module() {
        let src = "fn f() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        let fixed = src.replace("{ 1 }", "{ 2 }");
        let tested = src.replace("fn t() {}", "fn t() { assert!(true) }");
        let both = fixed.replace("fn t() {}", "fn t() { assert!(true) }");
        assert_eq!(split("a.rs", Some(src), Some(&fixed)), Split::Source);
        assert_eq!(split("a.rs", Some(src), Some(&tested)), Split::Tests);
        assert_eq!(split("a.rs", Some(src), Some(&both)), Split::Both);
        assert_eq!(split("a.rs", None, Some(src)), Split::Both);
        assert_eq!(split("a.rs", None, Some("fn f() {}\n")), Split::Source);
        assert_eq!(split("a.txt", Some("a"), Some("b")), Split::Source);
    }

    fn ran(exit: i32) -> Ran {
        Ran {
            exit: Some(exit),
            output: Vec::new(),
        }
    }

    #[test]
    fn verdicts_refuse_only_a_pass_without_the_fix_and_no_tests() {
        let v = |base, head| Verdict::of(&ran(base), &ran(head), Some(4));
        assert_eq!(v(1, 0), Verdict::Passed);
        assert_eq!(v(0, 0), Verdict::PassesReverted);
        assert_eq!(v(4, 4), Verdict::NoTests);
        assert_eq!(v(4, 0), Verdict::NoTestsReverted);
        assert_eq!(v(1, 1), Verdict::FailsAtHead);
        let refusing: Vec<Verdict> = [v(1, 0), v(0, 0), v(4, 4), v(4, 0), v(1, 1)]
            .into_iter()
            .filter(|v| v.refuses())
            .collect();
        assert_eq!(refusing, [Verdict::PassesReverted, Verdict::NoTests]);
        assert_eq!(
            Verdict::of(&ran(4), &ran(4), None),
            Verdict::FailsAtHead,
            "with no no-tests code, a non-zero exit is a failure"
        );
    }
}
