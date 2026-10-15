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
    proof::DoneWhen,
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
    /// The issue ticks the item off, so an earlier change landed it.
    Done,
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
    /// The checkout the PR is opened from.
    dir: PathBuf,
    /// The `test_files` globs the merge base's config sets.
    test_files: Vec<String>,
    /// The issue's `Done when` items.
    texts: Vec<DoneWhen>,
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
    /// The gate `gates` configures for a PR opened at `head`, to check the
    /// items `items` returns: `None` when the gate is off, names no task,
    /// or the worktree's issue lists no items. Nothing runs until
    /// [`Gate::apply`].
    pub(crate) fn read(
        dir: &Path,
        merge_base: &str,
        gates: &PrGates,
        test_files: &[String],
        head: &str,
        caller: Caller,
        items: impl FnOnce() -> Result<Option<Vec<DoneWhen>>>,
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
            dir: dir.to_path_buf(),
            test_files: test_files.to_vec(),
            texts: Vec::new(),
            reverted: Vec::new(),
            items: Vec::new(),
            unread: None,
        };
        match items() {
            Ok(Some(texts)) if !texts.is_empty() => gate.texts = texts,
            Ok(_) => return Ok(None),
            Err(e) => gate.unread = Some(format!("{e:#}")),
        }
        Ok(Some(gate))
    }

    /// Runs the check of each item, filling in its outcome.
    fn evaluate(&mut self) -> Result<()> {
        if self.unread.is_some() {
            return Ok(());
        }
        let texts = std::mem::take(&mut self.texts);
        let dir = self.dir.clone();
        let marked: Vec<(usize, Option<String>)> = texts
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.done)
            .filter_map(|(i, t)| marker(&t.text).map(|sel| (i, sel)))
            .collect();
        let mut outcomes: Vec<Outcome> = texts
            .iter()
            .map(|t| {
                if t.done {
                    Outcome::Done
                } else {
                    Outcome::Unmarked
                }
            })
            .collect();
        if !marked.is_empty() {
            let tests = TestFiles::new(&self.test_files)?;
            match self.check(&dir, &tests, &marked) {
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
        self.items = texts
            .iter()
            .zip(outcomes)
            .enumerate()
            .map(|(i, (text, outcome))| Item {
                number: i + 1,
                title: title(&text.text).to_string(),
                outcome,
            })
            .collect();
        Ok(())
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
    /// selector passes with the fix reverted or matches no test. The check
    /// runs here, so a PR another gate refuses first never pays for it.
    pub(crate) fn apply(mut self, body: String, verification: bool) -> Result<String> {
        self.evaluate()?;
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
                    Outcome::Unmarked
                    | Outcome::Done
                    | Outcome::Inconclusive(_)
                    | Outcome::Error(_) => None,
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
            Outcome::Done => format!(
                "{lead}: already ticked in the issue, so an earlier change landed it and \
                 nothing checked it here.\n"
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
    let is_test = |rev: &str, path: &str| -> Result<bool> {
        Ok(tests.matches(path)
            || test_module_file(path, tests, &|p: &str| vcs.file_at(dir, rev, p))?)
    };
    for change in vcs.changed_files(dir, merge_base, head)? {
        let old = match change.old {
            Some(p) if !is_test(merge_base, &p)? => Some(p),
            _ => None,
        };
        let new = match change.new {
            Some(p) if !is_test(head, &p)? => Some(p),
            _ => None,
        };
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
    // A path both restored and deleted, as by a rename onto a path the branch
    // also recreates, ends at its merge-base contents.
    out.delete.retain(|d| !out.restore.contains(d));
    Ok(out)
}

/// The deepest chain of out-of-line modules [`test_module_file`] follows up
/// to a test-only declaration.
const MODULE_DEPTH: usize = 8;

/// Whether the Rust file at `path` is an out-of-line module only test builds
/// compile.
///
/// That is a module a `mod` declaration under a test-only `#[cfg]` resolves
/// to, or one declared in a test file or in such a module. `read` gives a
/// file's text at the revision in question, `None` where it does not exist.
fn test_module_file(
    path: &str,
    tests: &TestFiles,
    read: &dyn Fn(&str) -> Result<Option<String>>,
) -> Result<bool> {
    declared_in_tests(path, tests, read, MODULE_DEPTH)
}

fn declared_in_tests(
    path: &str,
    tests: &TestFiles,
    read: &dyn Fn(&str) -> Result<Option<String>>,
    depth: usize,
) -> Result<bool> {
    let Some(stem) = path.strip_suffix(".rs") else {
        return Ok(false);
    };
    if depth == 0 {
        return Ok(false);
    }
    let (dir, name) = split_last(stem);
    let (dir, name) = if name == "mod" {
        split_last(dir)
    } else {
        (dir, name)
    };
    if name.is_empty() {
        return Ok(false);
    }
    for declarer in declarers(dir) {
        let Some(text) = read(&declarer)? else {
            continue;
        };
        let (declarer_dir, _) = split_last(&declarer);
        let declarations: Vec<ModDeclaration> = mod_declarations(&text)
            .into_iter()
            .filter(|d| match &d.path {
                Some(attr) => joined(declarer_dir, attr) == path,
                None => d.name == name,
            })
            .collect();
        if declarations.iter().any(|d| d.test_only) {
            return Ok(true);
        }
        if !declarations.is_empty()
            && (tests.matches(&declarer) || declared_in_tests(&declarer, tests, read, depth - 1)?)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `path` split at its last `/`: the directory, empty at the root, and the
/// last component.
fn split_last(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// `rel` resolved against the directory `dir`, with `.` and `..` folded.
fn joined(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// The files whose `mod x;` resolves to a module file in `dir`: the module
/// roots inside it, and the non-`mod.rs` file named for it.
fn declarers(dir: &str) -> Vec<String> {
    let inside = |file: &str| {
        if dir.is_empty() {
            file.to_string()
        } else {
            format!("{dir}/{file}")
        }
    };
    let mut out: Vec<String> = ["mod.rs", "lib.rs", "main.rs"]
        .into_iter()
        .map(inside)
        .collect();
    if !dir.is_empty() {
        out.push(format!("{dir}.rs"));
    }
    out
}

/// A `mod x;` declaration of an out-of-line module.
#[derive(Debug, PartialEq, Eq)]
struct ModDeclaration {
    name: String,
    /// Its `#[path]` attribute's value.
    path: Option<String>,
    /// Whether a `#[cfg]` on it holds only in test builds.
    test_only: bool,
}

/// The `mod x;` declarations in `text`, with the attributes on their line
/// and on the attribute lines above it.
fn mod_declarations(text: &str) -> Vec<ModDeclaration> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        let (mut attrs, rest) = attributes(line.trim());
        let Some(name) = strip_visibility(rest)
            .strip_prefix("mod ")
            .and_then(|m| m.trim().strip_suffix(';'))
            .map(str::trim)
        else {
            continue;
        };
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        for above in lines[..at].iter().rev().map(|l| l.trim()) {
            if above.starts_with("//") {
                continue;
            }
            let (found, rest) = attributes(above);
            if found.is_empty() || !rest.is_empty() {
                break;
            }
            attrs.extend(found);
        }
        let mut decl = ModDeclaration {
            name: name.to_string(),
            path: None,
            test_only: false,
        };
        for attr in attrs {
            if let Some((predicate, "")) = attr.strip_prefix("cfg(").and_then(balanced) {
                decl.test_only |= only_in_tests(predicate);
            } else if let Some(value) = attr
                .strip_prefix("path")
                .and_then(|a| a.trim_start().strip_prefix('='))
            {
                decl.path = Some(value.trim().trim_matches('"').to_string());
            }
        }
        out.push(decl);
    }
    out
}

/// The outer attributes leading `line`, each without its `#[` and `]`, and
/// what follows them.
fn attributes(line: &str) -> (Vec<&str>, &str) {
    let mut attrs = Vec::new();
    let mut rest = line;
    while let Some(inner) = rest.strip_prefix("#[") {
        let mut depth = 0usize;
        let Some(end) = inner.char_indices().find_map(|(i, c)| match c {
            '[' | '(' => {
                depth += 1;
                None
            }
            ')' => {
                depth = depth.saturating_sub(1);
                None
            }
            ']' if depth == 0 => Some(i),
            ']' => {
                depth -= 1;
                None
            }
            _ => None,
        }) else {
            break;
        };
        attrs.push(inner[..end].trim());
        rest = inner[end + 1..].trim_start();
    }
    (attrs, rest)
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
/// its test modules (see [`rust_parts`]), and a file whose test modules
/// cannot be delimited counts as both when it changed; in another language
/// the scanner reads, a change in its count of test declarations or
/// assertions counts as both, since the two cannot be told apart.
fn split(path: &str, before: Option<&str>, after: Option<&str>) -> Split {
    let (before, after) = (before.unwrap_or(""), after.unwrap_or(""));
    match Lang::of(path) {
        Some(Lang::Rust) => {
            let (Some((src_was, tests_was)), Some((src_now, tests_now))) =
                (rust_parts(before), rust_parts(after))
            else {
                return if before == after {
                    Split::Source
                } else {
                    Split::Both
                };
            };
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

/// `text` split into its source lines and the lines of its test modules: a
/// `mod` item under a `#[cfg]` that holds only in test builds, from that
/// attribute to the `}` line indented as the `mod` line is. `None` when a
/// test module's closing line is not found, or a test attribute follows it
/// before the next test module, so the split is unknown.
fn rust_parts(text: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let (mut source, mut tests) = (String::new(), String::new());
    let mut i = 0;
    while i < lines.len() {
        match test_module(&lines, i) {
            Module::Ends(end) => {
                tests.extend(lines[i..=end].iter().copied());
                i = end + 1;
            }
            Module::Absent => {
                source.push_str(lines[i]);
                i += 1;
            }
            Module::Unbounded => return None,
        }
    }
    Some((source, tests))
}

/// Whether a test module starts at `lines[at]`, and where it ends.
enum Module {
    Absent,
    /// It ends at this line index.
    Ends(usize),
    /// It starts here, but where it ends was not found.
    Unbounded,
}

fn test_module(lines: &[&str], at: usize) -> Module {
    let first = lines[at].trim_start();
    let Some(rest) = first.strip_prefix("#[cfg(") else {
        return Module::Absent;
    };
    let Some((predicate, after)) = balanced(rest) else {
        return Module::Absent;
    };
    let Some(after) = after.strip_prefix(']') else {
        return Module::Absent;
    };
    if !only_in_tests(predicate) {
        return Module::Absent;
    }
    let mut item = at;
    let mut text = after.trim();
    while text.is_empty() || text.starts_with("#[") || text.starts_with("//") {
        item += 1;
        let Some(line) = lines.get(item) else {
            return Module::Absent;
        };
        text = line.trim();
    }
    let Some(module) = strip_visibility(text).strip_prefix("mod ") else {
        return Module::Absent;
    };
    if module.trim_end().ends_with(';') || module.trim_end().ends_with('}') {
        return Module::Ends(item);
    }
    if !module.trim_end().ends_with('{') {
        return Module::Unbounded;
    }
    let indent = &lines[item][..lines[item].len() - lines[item].trim_start().len()];
    let Some(end) = lines.iter().skip(item + 1).position(|line| {
        line.strip_prefix(indent)
            .and_then(|l| l.strip_prefix('}'))
            .is_some_and(|l| l.trim().is_empty() || l.trim_start().starts_with("//"))
    }) else {
        return Module::Unbounded;
    };
    let end = item + 1 + end;
    // A `}` line inside a string ends the search early; a test after it
    // shows the module went on.
    let tests_follow = lines[end + 1..]
        .iter()
        .map(|l| l.trim_start())
        .take_while(|l| !starts_test_cfg(l))
        .any(is_test_attribute);
    if tests_follow {
        Module::Unbounded
    } else {
        Module::Ends(end)
    }
}

/// Whether `line` opens with a `#[cfg]` that holds only in test builds.
fn starts_test_cfg(line: &str) -> bool {
    line.strip_prefix("#[cfg(")
        .and_then(balanced)
        .is_some_and(|(predicate, _)| only_in_tests(predicate))
}

/// Whether `line` opens with an attribute marking a test function:
/// `#[test]`, a `#[...::test]` such as `#[tokio::test]`, `#[rstest]` or
/// `#[test_case(...)]`.
fn is_test_attribute(line: &str) -> bool {
    let Some(attr) = line.strip_prefix("#[") else {
        return false;
    };
    let name = attr
        .split(|c: char| c == '(' || c == ']' || c.is_whitespace())
        .next()
        .unwrap_or("");
    name == "test" || name.ends_with("::test") || name == "rstest" || name == "test_case"
}

/// `text` up to the `)` closing a `(` already read, and what follows it.
fn balanced(text: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return Some((&text[..i], &text[i + 1..])),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Whether the `cfg` predicate `predicate` holds only in test builds: it is
/// `test`, or an `all(...)` with `test` among its arguments.
fn only_in_tests(predicate: &str) -> bool {
    let predicate = predicate.trim();
    if predicate == "test" {
        return true;
    }
    let Some((args, rest)) = predicate.strip_prefix("all(").and_then(balanced) else {
        return false;
    };
    if !rest.trim().is_empty() {
        return false;
    }
    let mut depth = 0usize;
    let mut start = 0;
    let mut found = false;
    for (i, c) in args.char_indices().chain([(args.len(), ',')]) {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                found |= only_in_tests(&args[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    found
}

/// `item` without a leading `pub` or `pub(...)`.
fn strip_visibility(item: &str) -> &str {
    let Some(rest) = item.strip_prefix("pub") else {
        return item;
    };
    let rest = match rest.strip_prefix('(').and_then(balanced) {
        Some((_, after)) => after,
        None => rest,
    };
    if rest.starts_with(char::is_whitespace) {
        rest.trim_start()
    } else {
        item
    }
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

    #[test]
    fn a_cfg_test_item_that_is_not_a_module_is_source() {
        let src = "#[cfg(test)]\nuse std::fmt;\n\nfn f() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests \
                   {\n    fn t() {}\n}\n";
        let fixed = src.replace("{ 1 }", "{ 2 }");
        let tested = src.replace("fn t() {}", "fn t() { assert!(true) }");
        assert_eq!(split("a.rs", Some(src), Some(&fixed)), Split::Source);
        assert_eq!(split("a.rs", Some(src), Some(&tested)), Split::Tests);
        let both = fixed.replace("fn t() {}", "fn t() { assert!(true) }");
        assert_eq!(split("a.rs", Some(src), Some(&both)), Split::Both);
    }

    #[test]
    fn a_module_under_a_cfg_naming_test_among_others_holds_tests() {
        let src = "fn f() -> u8 { 1 }\n\n#[cfg(all(test, target_os = \"linux\"))]\nmod tests {\n    \
                   fn t() {}\n}\n";
        let tested = src.replace("fn t() {}", "fn t() { assert!(true) }");
        let fixed = src.replace("{ 1 }", "{ 2 }");
        assert_eq!(split("a.rs", Some(src), Some(&tested)), Split::Tests);
        assert_eq!(split("a.rs", Some(src), Some(&fixed)), Split::Source);
    }

    #[test]
    fn source_after_a_test_module_is_source() {
        let src = "#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n\nfn f() -> u8 { 1 }\n";
        let fixed = src.replace("{ 1 }", "{ 2 }");
        assert_eq!(split("a.rs", Some(src), Some(&fixed)), Split::Source);
    }

    #[test]
    fn a_module_compiled_outside_tests_is_source() {
        for cfg in ["not(test)", "any(test, feature = \"x\")"] {
            let src = format!("#[cfg({cfg})]\nmod real {{\n    fn f() -> u8 {{ 1 }}\n}}\n");
            let fixed = src.replace("{ 1 }", "{ 2 }");
            assert_eq!(
                split("a.rs", Some(&src), Some(&fixed)),
                Split::Source,
                "{cfg}"
            );
        }
    }

    #[test]
    fn a_test_module_with_no_closing_brace_is_both() {
        let src = "fn f() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests {\n    fn t() {}\n";
        let fixed = src.replace("{ 1 }", "{ 2 }");
        assert_eq!(split("a.rs", Some(src), Some(&fixed)), Split::Both);
    }

    /// Whether `path` is a test module file in a tree holding `files`.
    fn test_module_in(files: &[(&str, &str)], path: &str) -> bool {
        let tests = TestFiles::new(&devkit_config::default_test_files()).unwrap();
        let read = |p: &str| -> Result<Option<String>> {
            Ok(files
                .iter()
                .find(|(f, _)| *f == p)
                .map(|(_, t)| (*t).to_string()))
        };
        test_module_file(path, &tests, &read).unwrap()
    }

    #[test]
    fn a_module_file_a_test_only_declaration_resolves_to_is_a_test() {
        let decl = "fn f() {}\n\n#[cfg(test)]\nmod tests;\n";
        for (declarer, module) in [
            ("src/hook/shell.rs", "src/hook/shell/tests.rs"),
            ("src/hook/shell/mod.rs", "src/hook/shell/tests.rs"),
            ("src/hook/shell/mod.rs", "src/hook/shell/tests/mod.rs"),
            ("src/lib.rs", "src/tests.rs"),
            ("src/main.rs", "src/tests.rs"),
            ("lib.rs", "tests.rs"),
        ] {
            assert!(
                test_module_in(&[(declarer, decl)], module),
                "{declarer} -> {module}"
            );
        }
        for inline in [
            "#[cfg(test)] mod tests;\n",
            "#[cfg(all(test, unix))]\n// why\n#[allow(dead_code)]\npub(crate) mod tests;\n",
        ] {
            assert!(
                test_module_in(&[("src/a.rs", inline)], "src/a/tests.rs"),
                "{inline}"
            );
        }
        assert!(test_module_in(
            &[(
                "src/a.rs",
                "#[cfg(test)]\n#[path = \"./a/checks.rs\"]\nmod tests;\n"
            )],
            "src/a/checks.rs"
        ));
    }

    #[test]
    fn a_module_declared_inside_a_test_module_file_is_a_test() {
        let files = [
            ("src/a.rs", "#[cfg(test)]\nmod tests;\n"),
            ("src/a/tests.rs", "mod helpers;\n"),
        ];
        assert!(test_module_in(&files, "src/a/tests/helpers.rs"));
        assert!(test_module_in(
            &[("tests/it/main.rs", "mod helpers;\n")],
            "tests/it/helpers.rs"
        ));
    }

    #[test]
    fn a_module_file_compiled_outside_tests_is_source() {
        for decl in [
            "mod tests;\n",
            "#[cfg(not(test))]\nmod tests;\n",
            "#[cfg(test)]\nfn f() {}\nmod tests;\n",
            "#[cfg(test)]\nmod other;\n",
            "#[cfg(test)]\n#[path = \"elsewhere.rs\"]\nmod tests;\n",
        ] {
            assert!(
                !test_module_in(&[("src/a.rs", decl)], "src/a/tests.rs"),
                "{decl}"
            );
        }
        assert!(!test_module_in(&[], "src/a/tests.rs"));
        assert!(!test_module_in(
            &[("src/a.rs", "#[cfg(test)]\nmod tests;\n")],
            "src/a/tests.txt"
        ));
    }

    #[test]
    fn a_test_after_a_test_modules_closing_line_leaves_it_unbounded() {
        let src = "fn f() -> u8 { 1 }\n\n#[cfg(test)]\nmod tests {\n    const FIXTURE: &str = \
                   r#\"\nfn x() {\n}\n\"#;\n\n    #[test]\n    fn t() {}\n}\n";
        let tested = src.replace("fn t() {}", "fn t() { assert!(true) }");
        assert_eq!(split("a.rs", Some(src), Some(&tested)), Split::Both);
        let later = format!("{src}\n#[cfg(test)]\nmod more {{\n    #[test]\n    fn u() {{}}\n}}\n");
        let fixed = later.replace("{ 1 }", "{ 2 }");
        assert_eq!(
            split("a.rs", Some(&later), Some(&fixed)),
            Split::Both,
            "{later}"
        );
        let two = "#[cfg(test)]\nmod a {\n    #[test]\n    fn t() {}\n}\n\n#[cfg(test)]\nmod b \
                   {\n    #[tokio::test]\n    async fn u() {}\n}\n\nfn f() -> u8 { 1 }\n";
        let fixed = two.replace("{ 1 }", "{ 2 }");
        assert_eq!(split("a.rs", Some(two), Some(&fixed)), Split::Source);
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
