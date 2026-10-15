//! The verify gate: a PR opens at a commit the project's required task is
//! recorded passing on. Only `devrun task` writes the run ledger, so the
//! record is evidence an agent cannot type into the PR body itself.

use std::path::Path;

use anyhow::{Result, bail};
use devkit_common::caller::Caller;
use devkit_config::{Gate as GateKey, GateMode, PrGates, RequiredTask};
use devkit_ports::{
    ledger::{Ledger, Run},
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
        let run = ledger.passed(&repo, &required.task, &required.args, head)?;
        let rejected = match run {
            Some(_) => None,
            None => {
                let runs: Vec<Run> = ledger.runs_in(&repo)?.into_iter().rev().collect();
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
        let change = definition_change(dir, merge_base, head, run_dir, &required);
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

/// The note on what the PR changes in what the required task runs: the
/// task as the `devkit.toml` layers committed at `merge_base` define it for
/// a run started in `run_dir`, against the same at `head`, both rendered
/// for the checkout at `dir`. No other layer enters either side, so an
/// uncommitted edit, the primary checkout's config, the home config or a
/// `devkit.local.toml` shows as no change. `None` when nothing differs.
fn definition_change(
    dir: &Path,
    merge_base: &str,
    head: &str,
    run_dir: &str,
    required: &RequiredTask,
) -> Option<String> {
    let root = devkit_common::vcs::checkout_root(dir).unwrap_or_else(|_| dir.to_path_buf());
    let base_ports = Default::default();
    let plan = |rev| {
        task::committed_plan(
            dir,
            rev,
            run_dir,
            &root,
            &required.task,
            &required.args,
            &base_ports,
        )
    };
    let task = &required.task;
    let base = short(merge_base);
    let lines = match (plan(merge_base), plan(head)) {
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
/// `head` define the required task to run, rendered with the run's ports.
/// A layer outside those files, such as a `devkit.local.toml`, the primary
/// checkout's config, `--config` or `$DEVKIT_CONFIG`, changes what runs
/// without dirtying the tree.
fn ran_note(dir: &Path, head: &str, run: &Run, required: &RequiredTask) -> String {
    if run.steps.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = run
        .steps
        .iter()
        .map(|s| format!("{}: {}", s.task, s.argv.join(" ")))
        .collect();
    let mut out = format!("\nSteps run:\n{}", leading_block(&lines, "text"));
    let root = devkit_common::vcs::checkout_root(dir).unwrap_or_else(|_| dir.to_path_buf());
    let ports = run
        .steps
        .iter()
        .flat_map(|s| s.ports.iter().map(|(app, port)| (app.clone(), *port)))
        .collect();
    let committed = task::committed_plan(
        dir,
        head,
        run.dir.as_deref().unwrap_or(""),
        &root,
        &required.task,
        &required.args,
        &ports,
    );
    let same = |plan: &task::ExecutionPlan| {
        let committed = plan.commands();
        committed.len() == run.steps.len()
            && committed
                .iter()
                .zip(&run.steps)
                .all(|(c, r)| c.task == r.task && c.argv == r.argv)
    };
    if committed.is_ok_and(|plan| !same(&plan)) {
        out.push_str(&format!(
            "\nAn uncommitted config layer changed what ran: these steps differ from what the \
             `devkit.toml` files committed at HEAD define for `{}`.\n",
            required.task
        ));
    }
    out
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
    let flags = |args| match devkit_config::arg_flags(args) {
        none if none.is_empty() => "no `--arg`".to_string(),
        some => format!("`{some}`"),
    };
    let mut why = Vec::new();
    if run.args != required.args {
        why.push(format!(
            "it was given {} where the gate requires {}",
            flags(&run.args),
            flags(&required.args)
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
