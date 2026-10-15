//! The verify gate: a PR opens at a commit the project's required task is
//! recorded passing on. Only `devrun task` writes the run ledger, so the
//! record is evidence an agent cannot type into the PR body itself.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use devkit_common::caller::Caller;
use devkit_config::{Gate as GateKey, GateMode, PrGates, RequiredTask};
use devkit_ports::{
    ledger::{Ledger, Run, Step},
    task,
};

/// The verify gate for one `pr create`: the required run and the run that
/// satisfies it, if the ledger has one.
pub(crate) struct Gate {
    mode: GateMode,
    required: RequiredTask,
    head: String,
    caller: Caller,
    run: Option<Run>,
    /// Why the latest run that passed at `head` does not qualify, when one
    /// passed and none qualifies.
    rejected: Option<String>,
    /// What the PR changes in what the required task runs, when it does.
    change: Option<String>,
    /// The steps `run` executed, and whether they differ from what HEAD's
    /// committed config defines.
    ran: String,
}

impl Gate {
    /// The gate `gates` configures for a PR opened at `head` from the
    /// checkout at `dir`, or `None` when it is off or names no task. A
    /// branch that changes what the required task runs since `merge_base`
    /// is not refused for it: the Verification section shows the change.
    pub(crate) fn read(
        dir: &Path,
        merge_base: &str,
        gates: &PrGates,
        head: &str,
        caller: Caller,
        ledger: &Ledger,
    ) -> Result<Option<Self>> {
        let Some((required, mode)) = gates.all().into_iter().find_map(|(gate, mode)| match gate {
            GateKey::Verify(required) => Some((required, mode)),
            GateKey::WeakenedTests => None,
        }) else {
            return Ok(None);
        };
        if mode == GateMode::Off {
            return Ok(None);
        }
        let repo = devkit_common::vcs::primary_checkout(dir)?;
        let repo = std::fs::canonicalize(&repo).unwrap_or(repo);
        let runs: Vec<Run> = ledger.runs_in(&repo)?.into_iter().rev().collect();
        let (required, moved_from) = match heads_required(dir, head, &required, caller, &runs) {
            Some(at_head) => (at_head, Some(required)),
            None => (required, None),
        };
        let run = ledger.passed(&repo, &required.task, &required.args, head)?;
        let rejected = match run {
            Some(_) => None,
            None => {
                match runs
                    .iter()
                    .find(|r| r.proves(&required.task, &required.args, head))
                {
                    Some(reused) => Some(reused_note(&required, reused)),
                    None => runs
                        .iter()
                        .find(|r| {
                            r.task == required.task
                                && r.exit_code == Some(0)
                                && r.head.as_deref() == Some(head)
                        })
                        .and_then(|r| unproven(&required, r)),
                }
            }
        };
        let run_dir = run.as_ref().and_then(|r| r.dir.as_deref()).unwrap_or("");
        let change = definition_change(
            dir,
            merge_base,
            head,
            run_dir,
            moved_from.as_ref().unwrap_or(&required),
            &required,
        );
        let ran = run
            .as_ref()
            .map(|r| ran_note(dir, head, r, &required))
            .unwrap_or_default();
        Ok(Some(Self {
            mode,
            required,
            head: head.to_string(),
            caller,
            run,
            rejected,
            change,
            ran,
        }))
    }

    /// `body` with the Verification section appended, or a refusal when an
    /// agent's PR under `require` has no qualifying run.
    pub(crate) fn apply(&self, body: String) -> Result<String> {
        let section = match &self.run {
            Some(run) => self.passed(run),
            None => match (self.mode, self.caller) {
                (GateMode::Require, Caller::Agent) => bail!(
                    "refusing to open the PR: the `verify` gate needs a run of `{task}` that \
                     exited 0 on a clean tree at {head}, with the required args, and the run \
                     ledger records none.{rejected}\n\
                     Commit or remove every change, run `{command}`, then retry",
                    task = self.required.task,
                    head = self.head,
                    command = self.required.command(),
                    rejected = self.rejected_note(),
                ),
                (GateMode::Off | GateMode::Advise | GateMode::Require, _) => self.not_run(),
            },
        };
        let body = body.trim_end();
        Ok(if body.is_empty() {
            section
        } else {
            format!("{body}\n\n{section}")
        })
    }

    fn passed(&self, run: &Run) -> String {
        let exit = run
            .exit_code
            .map_or_else(|| "none".to_string(), |c| c.to_string());
        let mut out = format!(
            "### Verification\n\n\
             - Command: `{command}`\n\
             - Exit code: {exit}\n\
             - SHA: `{head}`\n\
             - Tree: clean\n",
            command = self.required.command(),
            head = self.head,
        );
        for server in &run.live_servers {
            out.push_str(&format!(
                "- Against live server `{}`, started {}\n",
                server.app, server.started_at
            ));
        }
        out.push_str(&self.ran);
        match run.tail.as_deref() {
            Some(lines) if !lines.is_empty() => out.push_str(&output_tail(lines)),
            Some(_) => out.push_str("\nThe run printed nothing.\n"),
            None => out.push_str("\nNo output was captured: it went straight to a terminal.\n"),
        }
        out.push_str(self.change.as_deref().unwrap_or_default());
        out
    }

    fn not_run(&self) -> String {
        format!(
            "### Verification\n\n\
             NOT RUN: the run ledger records no run of `{command}` that exited 0 on a clean \
             tree at `{head}`.{rejected}\n{change}",
            command = self.required.command(),
            head = self.head,
            rejected = self.rejected_note(),
            change = self.change.as_deref().unwrap_or_default(),
        )
    }

    fn rejected_note(&self) -> String {
        self.rejected
            .as_deref()
            .map(|why| format!(" {why}"))
            .unwrap_or_default()
    }
}

/// Why `run`, which passed at the PR's head with the required args, does
/// not qualify: its `up` step reused a server already running before it
/// started, which may have started at another commit.
fn reused_note(required: &RequiredTask, run: &Run) -> String {
    format!(
        "A run at that commit passed, but its `up` step reused the server already running for \
         {apps}, which may have started at an earlier commit: stop it with `devrun down \
         {names}` and run `{command}` again.",
        apps = run
            .reused_servers
            .iter()
            .map(|a| format!("`{a}`"))
            .collect::<Vec<_>>()
            .join(", "),
        names = run
            .reused_servers
            .iter()
            .map(|a| format!("--app {a}"))
            .collect::<Vec<_>>()
            .join(" "),
        command = required.command(),
    )
}

/// HEAD's committed required run, when the merge base's `required` no
/// longer applies at HEAD: its task does not render there, or does not
/// take its args from `caller`. A branch can rename the task or a variable
/// its args name, and no run at HEAD could then take the merge base's.
/// Each candidate renders in the directory the latest of `runs` (newest
/// first) proving it at `head` ran from, since a nested layer may define
/// it, else at the checkout root. `None` when the merge base's applies, or
/// HEAD's committed config names no required run that does.
fn heads_required(
    dir: &Path,
    head: &str,
    required: &RequiredTask,
    caller: Caller,
    runs: &[Run],
) -> Option<RequiredTask> {
    let root = devkit_common::vcs::checkout_root(dir).unwrap_or_else(|_| dir.to_path_buf());
    let applies = |r: &RequiredTask| {
        let run_dir = runs
            .iter()
            .find(|run| run.proves(&r.task, &r.args, head))
            .and_then(|run| run.dir.as_deref())
            .unwrap_or("");
        task::committed_plan(
            dir,
            head,
            run_dir,
            &root,
            &r.task,
            &r.args,
            &BTreeMap::new(),
        )
        .is_ok_and(|plan| plan.accepts(caller))
    };
    if applies(required) {
        return None;
    }
    let gates = super::weakened::config_at(dir, head).ok()?.gates;
    let at_head = gates.all().into_iter().find_map(|(gate, _)| match gate {
        GateKey::Verify(r) => Some(r),
        GateKey::WeakenedTests => None,
    })?;
    applies(&at_head).then_some(at_head)
}

/// The note on what the PR changes in what the required run executes:
/// `at_base` as the `devkit.toml` layers committed at `merge_base` define
/// it for a run started in `run_dir`, against `required` as those
/// committed at `head` do, both rendered for the checkout at `dir`. No
/// other layer enters either side, so an uncommitted edit, the primary
/// checkout's config, the home config or a `devkit.local.toml` shows as no
/// change. `None` when nothing differs.
fn definition_change(
    dir: &Path,
    merge_base: &str,
    head: &str,
    run_dir: &str,
    at_base: &RequiredTask,
    required: &RequiredTask,
) -> Option<String> {
    let root = devkit_common::vcs::checkout_root(dir).unwrap_or_else(|_| dir.to_path_buf());
    let base_ports = Default::default();
    let plan = |rev, r: &RequiredTask| {
        task::committed_plan(dir, rev, run_dir, &root, &r.task, &r.args, &base_ports)
    };
    let moved = (at_base != required).then(|| {
        format!(
            "\nThis PR changes the required run from `{}` to `{}`: the merge base's no longer \
             applies at HEAD, so the gate takes HEAD's committed `verify_task` and \
             `verify_args`.\n",
            at_base.command(),
            required.command()
        )
    });
    let change = changed_plan(
        merge_base,
        &required.task,
        plan(merge_base, at_base),
        plan(head, required),
    );
    match moved {
        None => change,
        Some(moved) => Some(moved + &change.unwrap_or_default()),
    }
}

/// The note on how the plan at `merge_base` differs from the one at HEAD,
/// either of which may have failed to render. `None` when they match.
fn changed_plan(
    merge_base: &str,
    task: &str,
    at_base: Result<task::ExecutionPlan>,
    at_head: Result<task::ExecutionPlan>,
) -> Option<String> {
    let base = short(merge_base);
    let lines = match (at_base, at_head) {
        (Ok(at_base), Ok(at_head)) => task::plan_diff(&at_base, &at_head),
        (Err(_), Err(e)) => {
            return Some(format!(
                "\nThe `devkit.toml` files committed at the merge base `{base}` and at HEAD do \
                 not define `{task}` as a task devrun can run: {e:#}\n"
            ));
        }
        (Ok(_), Err(e)) => {
            return Some(format!(
                "\nThis PR changes `{task}`: it renders at the merge base `{base}` but not at \
                 HEAD: {e:#}\n"
            ));
        }
        (Err(e), Ok(_)) => {
            return Some(format!(
                "\nThis PR changes `{task}`: it renders at HEAD but not at the merge base \
                 `{base}`: {e:#}\n"
            ));
        }
    };
    (!lines.is_empty()).then(|| {
        format!(
            "\nThis PR changes what `{task}` runs, as the `devkit.toml` files committed at the \
             merge base `{base}` and at HEAD define it, rendered env values left out:\n{}",
            leading_block(&lines, "diff")
        )
    })
}

/// The steps `run` executed, as the section lists them, and a line saying
/// so when they differ from what the `devkit.toml` files committed at
/// `head` define the required task to run. A layer outside those files,
/// such as a `devkit.local.toml`, the primary checkout's config, `--config`
/// or `$DEVKIT_CONFIG`, changes what runs without dirtying the tree.
fn ran_note(dir: &Path, head: &str, run: &Run, required: &RequiredTask) -> String {
    let mut out = String::new();
    if !run.steps.is_empty() {
        let lines: Vec<String> = run
            .steps
            .iter()
            .map(|s| match &s.up {
                Some(app) => format!("up: {app}"),
                None => format!("{}: {}", s.task, s.argv.join(" ")),
            })
            .collect();
        out = format!("\nSteps run:\n{}", leading_block(&lines, "text"));
    }
    let root = devkit_common::vcs::checkout_root(dir).unwrap_or_else(|_| dir.to_path_buf());
    let committed = |ports: &BTreeMap<String, u16>| {
        task::committed_plan(
            dir,
            head,
            run.dir.as_deref().unwrap_or(""),
            &root,
            &required.task,
            &required.args,
            ports,
        )
        .map(|plan| plan.commands())
    };
    if matches_committed(&run.steps, committed) == Some(false) {
        out.push_str(&format!(
            "\nAn uncommitted config layer changed what ran: these steps differ from what the \
             `devkit.toml` files committed at HEAD define for `{}`.\n",
            required.task
        ));
    }
    out
}

/// Whether `recorded` are the command steps `committed` renders, each
/// rendered with the ports that step recorded, since two steps can
/// resolve one app to different ports. `None` when the committed config
/// does not render.
fn matches_committed(
    recorded: &[Step],
    committed: impl Fn(&BTreeMap<String, u16>) -> Result<Vec<Step>>,
) -> Option<bool> {
    if recorded.is_empty() {
        return committed(&BTreeMap::new()).ok().map(|c| c.is_empty());
    }
    for (i, step) in recorded.iter().enumerate() {
        let plan = committed(&step.ports).ok()?;
        if plan.len() != recorded.len()
            || plan[i].task != step.task
            || plan[i].argv != step.argv
            || plan[i].up != step.up
        {
            return Some(false);
        }
    }
    Some(true)
}

/// At most the first [`ARG_BUDGET`] characters of `value`, with `...` when
/// the rest was dropped.
fn capped(value: &str) -> String {
    match value.char_indices().nth(ARG_BUDGET) {
        Some((end, _)) => format!("{}...", &value[..end]),
        None => value.to_string(),
    }
}

/// A fenced block of `lines` tagged `info`: the first lines that fit in
/// [`TAIL_BUDGET`], with a note when later lines were dropped.
fn leading_block(lines: &[String], info: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    for line in lines {
        let cost = line.len() + 1;
        if used + cost > TAIL_BUDGET {
            if kept.is_empty() {
                let mut end = TAIL_BUDGET;
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                kept.push(&line[..end]);
            }
            break;
        }
        used += cost;
        kept.push(line);
    }
    let fence = fence(&kept);
    let note = match lines.len() - kept.len() {
        0 => String::new(),
        1 => "\n1 later line was dropped to keep the PR body within GitHub's limit.\n".to_string(),
        n => format!("\n{n} later lines were dropped to keep the PR body within GitHub's limit.\n"),
    };
    format!("\n{fence}{info}\n{}\n{fence}\n{note}", kept.join("\n"))
}

/// Why `run`, which exited 0 at the PR's head, does not prove `required`,
/// and what to do about it; `None` when nothing recorded says why.
fn unproven(required: &RequiredTask, run: &Run) -> Option<String> {
    let flags = |args: &BTreeMap<String, String>, files: &BTreeMap<String, PathBuf>| {
        if args.is_empty() {
            return "no `--arg`".to_string();
        }
        let quoted: Vec<String> = args
            .iter()
            .map(|(key, value)| match files.get(key) {
                Some(path) => format!("`--arg-file {key}={}`", path.display()),
                None => format!("`--arg {key}={}`", capped(value)),
            })
            .collect();
        quoted.join(" ")
    };
    let mut why = Vec::new();
    if run.args != required.args {
        why.push(format!(
            "it was given {} where the gate requires {}",
            flags(&run.args, &run.arg_files),
            flags(&required.args, &BTreeMap::new())
        ));
    }
    if !run.env.is_empty() || run.env_file.is_some() {
        let mut given = Vec::new();
        if !run.env.is_empty() {
            given.push(format!(
                "`--env` for {}",
                run.env
                    .iter()
                    .map(|v| format!("`{v}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(file) = &run.env_file {
            given.push(format!("`--env-file {}`", file.display()));
        }
        why.push(format!(
            "it was given {}, which the committed config does not set, so run it without them",
            given.join(" and ")
        ));
    }
    if why.is_empty() && !run.clean {
        why.push(
            "its tree was not clean: a file was modified, staged or untracked when it started \
             or ended, or HEAD moved during it"
                .to_string(),
        );
    }
    (!why.is_empty()).then(|| format!("A run at that commit passed, but {}.", why.join("; and ")))
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(12)]
}

/// The most output, in bytes, the Verification section quotes. GitHub
/// refuses a PR body over 65536 characters, and the ledger's tail can hold
/// more than that, so the section keeps the body well under the limit.
const TAIL_BUDGET: usize = 8 * 1024;

/// The most characters of one arg's value a note on a run quotes.
const ARG_BUDGET: usize = 200;

/// The output-tail block for `lines`: the last lines that fit in
/// [`TAIL_BUDGET`], fenced, with a note when earlier lines were dropped. A
/// lone last line over the budget keeps its end, cut on a char boundary.
fn output_tail(lines: &[String]) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    let mut cut = false;
    for line in lines.iter().rev() {
        let cost = line.len() + 1;
        if used + cost > TAIL_BUDGET {
            if kept.is_empty() {
                let mut start = line.len() - TAIL_BUDGET;
                while !line.is_char_boundary(start) {
                    start += 1;
                }
                kept.push(&line[start..]);
            }
            cut = true;
            break;
        }
        used += cost;
        kept.push(line);
    }
    kept.reverse();
    let fence = fence(&kept);
    let dropped = lines.len() - kept.len();
    let note = match (dropped, cut) {
        (0, false) => String::new(),
        (0, true) => format!(
            "\nThe line was cut to at most its last {TAIL_BUDGET} bytes to keep the PR body within \
             GitHub's limit.\n"
        ),
        (1, _) => {
            "\n1 earlier line was dropped to keep the PR body within GitHub's limit.\n".to_string()
        }
        (n, _) => {
            format!("\n{n} earlier lines were dropped to keep the PR body within GitHub's limit.\n")
        }
    };
    format!(
        "\nOutput tail:\n\n{fence}text\n{}\n{fence}\n{note}",
        kept.join("\n")
    )
}

/// A code fence longer than any run of backticks in `lines`, so the output
/// cannot close it early.
fn fence(lines: &[&str]) -> String {
    let longest = lines
        .iter()
        .flat_map(|l| l.split(|c| c != '`'))
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(task: &str, app_port: u16) -> Step {
        Step {
            task: task.into(),
            argv: vec![task.into(), app_port.to_string()],
            up: None,
            ports: BTreeMap::from([("srv".to_string(), app_port)]),
        }
    }

    /// Renders `first` and `second`, each reading `srv`'s port, the way a
    /// committed sequence would.
    fn sequence(ports: &BTreeMap<String, u16>) -> Result<Vec<Step>> {
        let port = ports.get("srv").copied().unwrap_or(1);
        Ok(vec![step("first", port), step("second", port)])
    }

    #[test]
    fn steps_that_resolved_one_app_to_different_ports_match_the_committed_plan() {
        let recorded = [step("first", 4001), step("second", 4002)];
        assert_eq!(matches_committed(&recorded, sequence), Some(true));
        let changed = [step("first", 4001), step("third", 4002)];
        assert_eq!(matches_committed(&changed, sequence), Some(false));
    }

    #[test]
    fn an_up_step_for_another_app_differs_from_the_committed_plan() {
        let up = |app: &str| Step {
            task: String::new(),
            argv: Vec::new(),
            up: Some(app.into()),
            ports: BTreeMap::new(),
        };
        let committed = |_: &BTreeMap<String, u16>| Ok(vec![up("srv")]);
        assert_eq!(matches_committed(&[up("srv")], committed), Some(true));
        assert_eq!(matches_committed(&[up("db")], committed), Some(false));
    }

    #[test]
    fn a_blank_body_becomes_the_section_alone() {
        let gate = Gate {
            mode: GateMode::Advise,
            required: RequiredTask {
                task: "verify".into(),
                args: Default::default(),
            },
            head: "0123".into(),
            caller: Caller::Agent,
            run: None,
            rejected: None,
            change: None,
            ran: String::new(),
        };
        for body in ["", "  \n\n"] {
            let out = gate.apply(body.into()).unwrap();
            assert!(out.starts_with("### Verification\n\nNOT RUN"), "{out:?}");
        }
    }

    #[test]
    fn the_fence_outruns_backticks_in_the_output() {
        assert_eq!(fence(&["plain"]), "```");
        assert_eq!(fence(&["```rust", "x"]), "````");
        assert_eq!(fence(&["a ````` b"]), "``````");
    }

    #[test]
    fn an_oversized_tail_keeps_its_last_lines_within_the_budget() {
        let lines: Vec<String> = (0..100)
            .map(|i| format!("{i:03}{}", "x".repeat(4093)))
            .collect();
        assert!(lines.iter().all(|l| l.len() == 4096));
        let out = output_tail(&lines);
        assert!(out.len() < TAIL_BUDGET + 512, "{} bytes", out.len());
        assert!(out.contains(&format!("\n{}\n```\n", lines[99])), "{out}");
        assert!(!out.contains("\n098x"), "{out}");
        assert!(
            out.contains("\n99 earlier lines were dropped to keep the PR body"),
            "{out}"
        );
    }

    #[test]
    fn a_lone_line_over_the_budget_keeps_its_end_on_a_char_boundary() {
        let line = format!("{}end", "\u{e9}".repeat(TAIL_BUDGET));
        let out = output_tail(&[line]);
        assert!(out.len() < TAIL_BUDGET + 512, "{} bytes", out.len());
        assert!(out.contains("end\n```\n"), "{out}");
        assert!(
            out.contains("The line was cut to at most its last 8192 bytes"),
            "{out}"
        );
    }

    #[test]
    fn an_oversized_diff_keeps_its_first_lines_within_the_budget() {
        let lines: Vec<String> = (0..100)
            .map(|i| format!("+ {i:03}{}", "x".repeat(4090)))
            .collect();
        let out = leading_block(&lines, "diff");
        assert!(out.len() < TAIL_BUDGET + 512, "{} bytes", out.len());
        assert!(
            out.starts_with(&format!("\n```diff\n{}\n", lines[0])),
            "{out}"
        );
        assert!(!out.contains("+ 002"), "{out}");
        assert!(
            out.contains("\n98 later lines were dropped to keep the PR body"),
            "{out}"
        );
        assert_eq!(
            leading_block(&["- a".to_string(), "+ b".to_string()], "diff"),
            "\n```diff\n- a\n+ b\n```\n"
        );
    }

    #[test]
    fn a_tail_within_the_budget_is_quoted_whole() {
        let lines = vec!["one".to_string(), "```two".to_string()];
        assert_eq!(
            output_tail(&lines),
            "\nOutput tail:\n\n````text\none\n```two\n````\n"
        );
    }
}
