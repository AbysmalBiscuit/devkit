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
    vcs::{Checkout, NewWorktree, Vcs, VersionControl},
};
use devkit_config::{Gate as GateKey, GateMode, PrGates, ReproTask, Step};
use serde::Serialize;

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
    checkout: Checkout,
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
    /// Changed Rust files a `#[path]` attribute reaches, which could be test
    /// modules the check does not resolve.
    reached: Vec<String>,
    /// Rust files holding both, each with the text that keeps its test
    /// modules and reverts its source.
    splice: Vec<(String, String)>,
}

impl Gate {
    /// The gate `gates` configures for a PR opened at `head`, to check the
    /// items `items` returns: `None` when the gate is off, names no task,
    /// or the worktree's issue lists no items. Nothing runs until
    /// [`Gate::apply`].
    pub(crate) fn read(
        checkout: &Checkout,
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
        prune_stale(checkout);
        let mut gate = Self {
            mode,
            caller,
            task,
            head: head.to_string(),
            merge_base: merge_base.to_string(),
            checkout: checkout.clone(),
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
        let checkout = self.checkout.clone();
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
            match self.check(&checkout, &tests, &marked) {
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
        checkout: &Checkout,
        tests: &TestFiles,
        marked: &[(usize, Option<String>)],
    ) -> Result<Vec<(usize, Outcome)>> {
        let revert = revert(checkout, &self.merge_base, &self.head, tests)?;
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
        } else if !revert.reached.is_empty() {
            Some(format!(
                "{} {} a `#[path]` attribute, so the check cannot tell whether reverting it \
                 would revert tests",
                quoted(&revert.reached),
                if revert.reached.len() == 1 {
                    "is a module file reached through"
                } else {
                    "are module files reached through"
                },
            ))
        } else if revert.restore.is_empty() && revert.delete.is_empty() && revert.splice.is_empty()
        {
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
        let ran = match self.run(checkout, &revert, &runnable)? {
            Ok(ran) => ran,
            Err(why) => {
                out.extend(
                    runnable
                        .into_iter()
                        .map(|(i, _)| (i, Outcome::Inconclusive(why.clone()))),
                );
                return Ok(out);
            }
        };
        self.reverted = revert
            .restore
            .into_iter()
            .chain(revert.delete)
            .chain(revert.splice.into_iter().map(|(path, _)| path))
            .collect();
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
    /// in a throwaway worktree, or why the runs could not be compared.
    fn run(
        &self,
        checkout: &Checkout,
        revert: &Revert,
        runnable: &[(usize, String)],
    ) -> Result<std::result::Result<Vec<(Ran, Ran)>, String>> {
        let steps = Steps::new();
        let worktree = Throwaway::new(checkout, &self.head)?;
        let at = &worktree.path();
        let throwaway = Checkout::at(at);
        if let Some(why) = self.starts_servers(&throwaway) {
            return Ok(Err(why));
        }
        let mut heads = Vec::new();
        for (i, selector) in runnable {
            heads.push(steps.during_result(
                &format!("Running item {}'s repro at HEAD\u{2026}", i + 1),
                || self.devrun(at, selector),
            )?);
        }
        let vcs = Vcs::at(at);
        vcs.restore_paths(at, &self.merge_base, &revert.restore)?;
        for (path, text) in &revert.splice {
            std::fs::write(at.join(path), text)
                .with_context(|| format!("reverting the source of {path} in {}", at.display()))?;
        }
        for path in &revert.delete {
            match std::fs::remove_file(at.join(path)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(e).with_context(|| format!("deleting {path} in {}", at.display()));
                }
                _ => {}
            }
        }
        if let Some(why) = self.starts_servers(&throwaway) {
            return Ok(Err(why));
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
        Ok(Ok(out))
    }

    /// Why the task, as the config in `at` defines it, cannot be run: it is
    /// a sequence with an `up` step, whose server the run at HEAD would leave
    /// running for the run with the fix reverted to reuse, and outlive the
    /// throwaway worktree. `None` when the config does not load there, so
    /// the run reports that itself.
    fn starts_servers(&self, at: &Checkout) -> Option<String> {
        let (cfg, _) = devkit_common::config::resolve_in(at, None, at.dir()).ok()?;
        let apps: Vec<String> = cfg
            .tasks
            .get(&self.task.task)?
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Up(app) => Some(app.clone()),
                Step::Task(_) => None,
            })
            .collect();
        (!apps.is_empty()).then(|| {
            format!(
                "`{}` starts {}, whose server would carry over from the run at HEAD to the run \
                 with the fix reverted, so neither ran",
                self.task.task,
                quoted(&apps)
            )
        })
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

    /// Runs the check and reports each item. `refused` is whether the
    /// `require` strength would refuse an agent's PR for these results.
    pub(crate) fn report(mut self) -> Result<Report> {
        self.evaluate()?;
        let refused =
            self.mode == GateMode::Require && self.caller == Caller::Agent && self.refuses();
        let items = self
            .items
            .iter()
            .map(|item| Reported {
                number: item.number,
                title: item.title.clone(),
                verdict: self.item_verdict(item),
                head_exit: exit_of(item, true),
                reverted_exit: exit_of(item, false),
                text: self.render_item(item),
            })
            .collect();
        Ok(Report {
            items,
            text: self.render(),
            refused,
        })
    }

    /// Whether an agent's PR under `require` is refused for these results.
    fn refuses(&self) -> bool {
        self.items.iter().any(|item| match &item.outcome {
            Outcome::Ran { base, head, .. } => {
                Verdict::of(base, head, self.task.no_tests_exit).refuses()
            }
            Outcome::Unmarked | Outcome::Done | Outcome::Inconclusive(_) | Outcome::Error(_) => {
                false
            }
        })
    }

    fn item_verdict(&self, item: &Item) -> Option<&'static str> {
        let (base, head) = match &item.outcome {
            Outcome::Ran { base, head, .. } => (base, head),
            Outcome::Inconclusive(_) => return Some("INCONCLUSIVE"),
            Outcome::Unmarked | Outcome::Done | Outcome::Error(_) => return None,
        };
        Some(match Verdict::of(base, head, self.task.no_tests_exit) {
            Verdict::Passed => "PASSED",
            Verdict::PassesReverted | Verdict::NoTests => "FAILED",
            Verdict::NoTestsReverted => "INCONCLUSIVE",
            Verdict::FailsAtHead => "FAILS AT HEAD",
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

/// A detached worktree at a commit, removed when dropped. It lives in a
/// holder directory under [`throwaway_root`] beside a `lock` file it holds
/// locked while in use, so [`prune_stale`] can tell a leaked one from one
/// another `pr create` is running.
struct Throwaway {
    main: PathBuf,
    /// Declared before `holder` so the lock is released before the holder
    /// directory is deleted, which Windows refuses while the file is open.
    _lock: std::fs::File,
    holder: tempfile::TempDir,
}

/// The prefix of each throwaway worktree's holder directory.
const HOLDER_PREFIX: &str = "devkit-repro-";

/// Where throwaway worktrees go: under devkit's state directory, since a
/// build in one can outgrow a tmpfs `$TMPDIR`.
fn throwaway_root() -> PathBuf {
    devkit_common::paths::state_dir().join("repro")
}

impl Throwaway {
    fn new(checkout: &Checkout, head: &str) -> Result<Self> {
        let here = checkout.worktree()?;
        let main = checkout.primary().unwrap_or(&here.path).to_path_buf();
        let root = throwaway_root();
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        let holder = tempfile::Builder::new()
            .prefix(HOLDER_PREFIX)
            .tempdir_in(&root)
            .context("creating a directory for the throwaway worktree")?;
        let lock = std::fs::File::create(holder.path().join("lock"))
            .context("creating the throwaway worktree's lock")?;
        lock.lock()
            .context("locking the throwaway worktree's lock")?;
        let worktree = Self {
            main,
            _lock: lock,
            holder,
        };
        Vcs::at(checkout.dir())
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
        remove_worktree(&self.main, &self.path());
    }
}

fn remove_worktree(main: &Path, path: &Path) {
    let vcs = Vcs::at(main);
    if vcs.remove_worktree(main, path, true).is_err() {
        let _ = std::fs::remove_dir_all(path);
        let _ = vcs.prune(main);
    }
}

/// Removes the throwaway worktrees of `checkout`'s repository that an
/// earlier `pr create` leaked, as when it was interrupted: those whose
/// holder's lock no process holds. A failure leaves the worktree for the
/// next try.
pub(crate) fn prune_stale(checkout: &Checkout) {
    let Ok(root) = throwaway_root().canonicalize() else {
        return;
    };
    let Some(main) = checkout.primary() else {
        return;
    };
    for worktree in checkout.worktrees() {
        let Some(holder) = worktree.path.parent().and_then(|h| h.canonicalize().ok()) else {
            continue;
        };
        let ours = holder.parent() == Some(root.as_path())
            && holder
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(HOLDER_PREFIX));
        if !ours {
            continue;
        }
        let Ok(lock) = std::fs::OpenOptions::new()
            .write(true)
            .open(holder.join("lock"))
        else {
            continue;
        };
        if lock.try_lock().is_err() {
            continue;
        }
        remove_worktree(main, &worktree.path);
        drop(lock);
        let _ = std::fs::remove_dir_all(&holder);
    }
}

/// What reverting the branch's non-test changes between `merge_base` and
/// `head` takes. A changed file whose change is all inline tests is left
/// alone, as a test file is.
fn revert(checkout: &Checkout, merge_base: &str, head: &str, tests: &TestFiles) -> Result<Revert> {
    let dir = checkout.dir();
    let vcs = Vcs::at(dir);
    let mut out = Revert::default();
    let is_test = |rev: &str, path: &str| -> Result<bool> {
        Ok(tests.matches(path)
            || test_module_file(path, tests, &|p: &str| checkout.file_at(rev, p))?)
    };
    let changes = vcs.changed_files(dir, merge_base, head)?;
    let rust_changed = changes
        .iter()
        .flat_map(|c| [&c.old, &c.new])
        .flatten()
        .any(|p| p.ends_with(".rs"));
    let targets = |rev: &str| -> Result<Vec<PathTarget>> {
        if !rust_changed {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for file in vcs.files_containing(dir, rev, "#[path")? {
            if let Some(text) = checkout
                .file_at(rev, &file)?
                .filter(|_| file.ends_with(".rs"))
            {
                out.extend(path_targets(&file, &text));
            }
        }
        Ok(out)
    };
    let (targets_was, targets_now) = (targets(merge_base)?, targets(head)?);
    let reached = |targets: &[PathTarget], p: &Option<String>| {
        p.as_ref()
            .is_some_and(|p| targets.iter().any(|t| t.reaches(p)))
    };
    for change in changes {
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
        if reached(&targets_was, &old) || reached(&targets_now, &new) {
            out.reached.push(label);
            continue;
        }
        let read = |rev: &str, p: &Option<String>| -> Result<Option<String>> {
            match p {
                Some(p) => checkout.file_at(rev, p),
                None => Ok(None),
            }
        };
        let before = read(merge_base, &old)?;
        let after = read(head, &new)?;
        match split(&label, before.as_deref(), after.as_deref()) {
            Split::Tests => continue,
            Split::Both => {
                let spliced = match (&before, &after, Lang::of(&label)) {
                    (Some(b), Some(a), Some(Lang::Rust)) if old == new => splice(b, a),
                    _ => None,
                };
                match spliced {
                    Some(text) => out.splice.push((label, text)),
                    None => out.mixed.push(label),
                }
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
    out.reached.sort();
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
    let components: Vec<&str> = dir.split('/').filter(|c| !c.is_empty()).collect();
    for k in (0..=components.len()).rev() {
        let parent = components[..k].join("/");
        let inline = &components[k..];
        for declarer in declarers(&parent) {
            let Some(text) = read(&declarer)? else {
                continue;
            };
            let declarations: Vec<ModDeclaration> = mod_declarations(&text)
                .into_iter()
                .filter(|d| d.resolves_to(path, name, &declarer, &parent, inline))
                .collect();
            if declarations.iter().any(|d| d.test_only) {
                return Ok(true);
            }
            if !declarations.is_empty()
                && (tests.matches(&declarer)
                    || declared_in_tests(&declarer, tests, read, depth - 1)?)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Where a `mod` declaration's `#[path]` attribute leads.
#[derive(Debug, PartialEq, Eq)]
enum PathTarget {
    /// The module file it resolves to.
    File(String),
    /// A file of this name somewhere, as the inline modules around the
    /// declaration could not be read.
    Named(String),
}

impl PathTarget {
    fn reaches(&self, path: &str) -> bool {
        match self {
            PathTarget::File(file) => file == path,
            PathTarget::Named(name) => split_last(path).1 == name,
        }
    }
}

/// Where the `#[path]` attributes on the `mod x;` declarations in `file`,
/// whose text is `text`, lead.
fn path_targets(file: &str, text: &str) -> Vec<PathTarget> {
    let (dir, base) = split_last(file);
    let children = match base {
        "mod.rs" | "lib.rs" | "main.rs" => dir.to_string(),
        _ => file.trim_end_matches(".rs").to_string(),
    };
    mod_declarations(text)
        .into_iter()
        .filter_map(|d| {
            let attr = d.path?;
            Some(match d.inline {
                Some(inline) if inline.is_empty() => PathTarget::File(joined(dir, &attr)),
                Some(inline) => {
                    PathTarget::File(joined(&format!("{children}/{}", inline.join("/")), &attr))
                }
                None => PathTarget::Named(split_last(&joined("", &attr)).1.to_string()),
            })
        })
        .collect()
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
    /// The inline modules it sits in, outermost first, or `None` when what
    /// encloses it could not be read.
    inline: Option<Vec<String>>,
    /// Whether a `#[cfg]` on it or on an inline module around it holds only
    /// in test builds.
    test_only: bool,
}

impl ModDeclaration {
    /// Whether this declaration, in the file `declarer` whose child modules
    /// live in `children`, resolves to `path`: the module file named `name`
    /// under the inline modules `inline` there. A declaration whose
    /// enclosing modules could not be read matches by name alone.
    fn resolves_to(
        &self,
        path: &str,
        name: &str,
        declarer: &str,
        children: &str,
        inline: &[&str],
    ) -> bool {
        let (declarer_dir, _) = split_last(declarer);
        match (&self.path, &self.inline) {
            (Some(attr), Some(enclosing)) if !enclosing.is_empty() => {
                joined(&format!("{children}/{}", enclosing.join("/")), attr) == path
            }
            (Some(attr), _) => joined(declarer_dir, attr) == path,
            (None, Some(enclosing)) => self.name == name && enclosing == inline,
            (None, None) => self.name == name,
        }
    }
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
        if !is_identifier(name) {
            continue;
        }
        attrs.extend(attributes_above(&lines, at));
        let inline = enclosing(&lines, at);
        let mut decl = ModDeclaration {
            name: name.to_string(),
            path: None,
            test_only: inline.as_ref().is_some_and(|(_, test_only)| *test_only),
            inline: inline.map(|(names, _)| names),
        };
        for attr in attrs {
            if let Some(predicate) = cfg_predicate(attr) {
                decl.test_only |= only_in_tests(predicate);
            } else if let Some(value) = path_value(attr) {
                decl.path = Some(value);
            }
        }
        out.push(decl);
    }
    out
}

fn is_identifier(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// The predicate of a `cfg(...)` attribute.
fn cfg_predicate(attr: &str) -> Option<&str> {
    match attr.strip_prefix("cfg(").and_then(balanced) {
        Some((predicate, "")) => Some(predicate),
        _ => None,
    }
}

/// The value of a `path = "..."` attribute.
fn path_value(attr: &str) -> Option<String> {
    attr.strip_prefix("path")
        .and_then(|a| a.trim_start().strip_prefix('='))
        .map(|value| value.trim().trim_matches('"').to_string())
}

/// The attributes on the attribute lines directly above `lines[at]`,
/// comments between them skipped.
fn attributes_above<'a>(lines: &[&'a str], at: usize) -> Vec<&'a str> {
    let mut attrs = Vec::new();
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
    attrs
}

/// The inline modules around `lines[at]`, outermost first, and whether a
/// `#[cfg]` on any of them holds only in test builds. Each is the nearest
/// line above that is indented less, so formatted code is assumed; `None`
/// when such a line is not a plain `mod x {`, or one carries `#[path]`.
fn enclosing(lines: &[&str], at: usize) -> Option<(Vec<String>, bool)> {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let (mut names, mut test_only) = (Vec::new(), false);
    let (mut width, mut below) = (indent(lines[at]), at);
    while width > 0 {
        let (opener, line) = lines[..below].iter().enumerate().rev().find(|(_, l)| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with("//") && indent(l) < width
        })?;
        let (mut attrs, rest) = attributes(line.trim());
        let name = strip_visibility(rest)
            .strip_prefix("mod ")?
            .trim_end()
            .strip_suffix('{')?
            .trim();
        if !is_identifier(name) {
            return None;
        }
        attrs.extend(attributes_above(lines, opener));
        for attr in attrs {
            if let Some(predicate) = cfg_predicate(attr) {
                test_only |= only_in_tests(predicate);
            } else if path_value(attr).is_some() {
                return None;
            }
        }
        names.push(name.to_string());
        (width, below) = (indent(line), opener);
    }
    names.reverse();
    Some((names, test_only))
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
    let (mut source, mut tests) = (String::new(), String::new());
    for (is_test, chunk) in rust_segments(text)? {
        if is_test {
            tests.push_str(&chunk);
        } else {
            source.push_str(&chunk);
        }
    }
    Some((source, tests))
}

/// `text` as its runs of source lines and of test-module lines, in order,
/// each marked whether it is tests. `None` when [`rust_parts`] gives none.
fn rust_segments(text: &str) -> Option<Vec<(bool, String)>> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut push = |is_test: bool, part: &[&str]| match out.last_mut() {
        Some((kind, chunk)) if *kind == is_test => chunk.extend(part.iter().copied()),
        _ => out.push((is_test, part.concat())),
    };
    let mut i = 0;
    while i < lines.len() {
        match test_module(&lines, i) {
            Module::Ends(end) => {
                push(true, &lines[i..=end]);
                i = end + 1;
            }
            Module::Absent => {
                push(false, &lines[i..=i]);
                i += 1;
            }
            Module::Unbounded => return None,
        }
    }
    Some(out)
}

/// `before`'s source with `after`'s test modules in their places: the file
/// with only its source change reverted. `None` when either side cannot be
/// split, or the two do not lay out source and test modules alike, so where
/// each test module goes is unclear.
fn splice(before: &str, after: &str) -> Option<String> {
    let (was, now) = (rust_segments(before)?, rust_segments(after)?);
    let kinds = |s: &[(bool, String)]| s.iter().map(|(k, _)| *k).collect::<Vec<_>>();
    if kinds(&was) != kinds(&now) {
        return None;
    }
    Some(
        was.into_iter()
            .zip(now)
            .map(|((is_test, source), (_, tests))| if is_test { tests } else { source })
            .collect(),
    )
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

/// One item of a [`Report`], as `--json` prints it.
#[derive(Debug, Serialize)]
pub(crate) struct Reported {
    number: usize,
    title: String,
    /// `PASSED`, `FAILED`, `FAILS AT HEAD` or `INCONCLUSIVE` for an item the
    /// check judged, and absent for one it did not run.
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict: Option<&'static str>,
    /// The exit code of the run at HEAD, when the selector ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    head_exit: Option<Option<i32>>,
    /// The exit code of the run with the fix reverted, when the selector ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    reverted_exit: Option<Option<i32>>,
    /// The item as the text report prints it.
    text: String,
}

/// What [`Gate::report`] found: each item, the text the command prints, and
/// whether the `require` strength would refuse the PR.
pub(crate) struct Report {
    items: Vec<Reported>,
    text: String,
    refused: bool,
}

impl Report {
    fn print(&self, json: bool) {
        if json {
            for item in &self.items {
                println!(
                    "{}",
                    serde_json::to_string(item).expect("a report item serializes")
                );
            }
            return;
        }
        print!("{}", self.text);
    }
}

/// The exit code of one of an item's runs: the HEAD run when `head`, else
/// the run with the fix reverted. `None` when the selector did not run.
fn exit_of(item: &Item, head: bool) -> Option<Option<i32>> {
    let Outcome::Ran { base, head: at, .. } = &item.outcome else {
        return None;
    };
    Some(if head { at.exit } else { base.exit })
}

/// `pr repro`: the repro gate's check on its own, for the current
/// worktree's branch. Prints each item's verdict and exits nonzero exactly
/// when the `require` strength would refuse the PR. Touches no forge.
pub(crate) fn run(dir: &Path, config: Option<&str>, json: bool) -> Result<()> {
    let checkout = Checkout::at(dir);
    let loaded = devkit_ports::load::load_in(&checkout, config.map(Path::new), dir)?;
    let base = &loaded.config.defaults.pr_base;
    let Some(merge_base) = super::weakened::merge_base(dir, base)? else {
        bail!(
            "the repro check reads its settings from the merge base with `{base}`, and neither \
             `origin/{base}` nor `{base}` is a commit here: fetch it, or set defaults.pr_base"
        );
    };
    let cfg = super::weakened::config_at(&checkout, &merge_base)?;
    let worktree = checkout.worktree()?;
    let record = devkit_common::record::read_on(&worktree.path, &worktree.branch);
    let issue = record.as_ref().and_then(|r| r.tracker_issue());
    let head = Vcs::at(dir).revision(dir)?;
    let caller = devkit_common::caller::caller();
    let config = &loaded.config;
    let repos = devkit_common::forge::resolve(
        &config.forge,
        &config.github,
        &dir.display().to_string(),
        None,
    )
    .repos;
    let items = || -> Result<Option<Vec<DoneWhen>>> {
        let Some(issue) = issue else {
            return Ok(None);
        };
        let tracker = devkit_common::tracker::resolve(config.tracker.kind, dir, &repos);
        let details = tracker.tracker.details(&issue)?.with_context(|| {
            format!("no issue {issue} in the {} tracker", tracker.tracker.kind())
        })?;
        Ok(Some(super::proof::done_when_entries(&details.description)))
    };
    let Some(gate) = Gate::read(
        &checkout,
        &merge_base,
        &cfg.gates,
        &cfg.test_files,
        &head,
        caller,
        items,
    )?
    else {
        println!("no repro check: the gate is off, or this worktree's issue lists no items");
        return Ok(());
    };
    let report = gate.report()?;
    let refused = report.refused;
    report.print(json);
    if refused {
        bail!(
            "the `repro` gate would refuse the PR: an item marked `(proof: repro <selector>)` \
             passes with the fix reverted or matches no test"
        );
    }
    Ok(())
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
    fn a_module_declared_inside_inline_modules_resolves_through_them() {
        for (declarer, decl, module) in [
            (
                "src/lib.rs",
                "mod outer {\n    #[cfg(test)]\n    mod tests;\n}\n",
                "src/outer/tests.rs",
            ),
            (
                "src/a.rs",
                "pub mod b {\n    mod c {\n        #[cfg(test)] mod tests;\n    }\n}\n",
                "src/a/b/c/tests.rs",
            ),
            (
                "src/lib.rs",
                "#[cfg(test)]\nmod outer {\n    mod helpers;\n}\n",
                "src/outer/helpers.rs",
            ),
            (
                "src/lib.rs",
                "mod outer {\n    #[cfg(test)]\n    #[path = \"checks.rs\"]\n    mod tests;\n}\n",
                "src/outer/checks.rs",
            ),
        ] {
            assert!(test_module_in(&[(declarer, decl)], module), "{decl}");
        }
        for (decl, module) in [
            ("mod outer {\n    mod tests;\n}\n", "src/outer/tests.rs"),
            (
                "mod outer {\n    #[cfg(test)]\n    mod tests;\n}\n",
                "src/tests.rs",
            ),
            (
                "#[cfg(test)]\nmod outer {\n    mod helpers;\n}\nmod helpers;\n",
                "src/helpers.rs",
            ),
        ] {
            assert!(!test_module_in(&[("src/lib.rs", decl)], module), "{decl}");
        }
    }

    #[test]
    fn a_path_attribute_leads_from_its_declarations_directory() {
        let decls = "#[path = \"../x/a.rs\"]\nmod a;\nmod outer {\n    #[path = \"b.rs\"]\n    \
                     mod b;\n}\nfn f() {\n  if x {\n    #[path = \"deep/c.rs\"]\n    mod c;\n  \
                     }\n}\nmod d;\n";
        let want = |children: &str| {
            vec![
                PathTarget::File("x/a.rs".into()),
                PathTarget::File(format!("{children}/outer/b.rs")),
                PathTarget::Named("c.rs".into()),
            ]
        };
        assert_eq!(path_targets("src/lib.rs", decls), want("src"));
        assert_eq!(path_targets("src/shell.rs", decls), want("src/shell"));
        assert!(PathTarget::Named("c.rs".into()).reaches("anywhere/c.rs"));
        assert!(!PathTarget::File("x/a.rs".into()).reaches("y/x/a.rs"));
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

    #[test]
    fn a_splice_keeps_head_tests_over_base_source_or_gives_up() {
        let module = |body: &str| format!("#[cfg(test)]\nmod tests {{\n    {body}\n}}\n");
        let before = format!("fn f() -> u8 {{ 1 }}\n\n{}\nfn g() {{}}\n", module("old"));
        let after = format!(
            "fn f() -> u8 {{ 2 }}\n\n{}\nfn g() {{ h() }}\n",
            module("new")
        );
        assert_eq!(
            splice(&before, &after).as_deref(),
            Some(format!("fn f() -> u8 {{ 1 }}\n\n{}\nfn g() {{}}\n", module("new")).as_str())
        );
        let moved = format!("{}\nfn f() -> u8 {{ 2 }}\n", module("new"));
        assert_eq!(splice(&before, &moved), None, "the layout changed");
        let unclosed = "fn f() -> u8 { 2 }\n\n#[cfg(test)]\nmod tests {\n    new\n";
        assert_eq!(splice(&before, unclosed), None, "the end is unknown");
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
