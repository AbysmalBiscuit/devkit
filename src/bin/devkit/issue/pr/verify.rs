//! The verify gate: a PR opens at a commit the project's required task is
//! recorded passing on. Only `devrun task` writes the run ledger, so the
//! record is evidence an agent cannot type into the PR body itself.

use std::path::Path;

use anyhow::{Result, bail};
use devkit_common::caller::Caller;
use devkit_config::{Gate as GateKey, GateMode, PrGates, RequiredTask};
use devkit_ports::{
    ledger::{Ledger, Run},
    load, task,
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
}

impl Gate {
    /// The gate `gates` configures for a PR opened at `head` from the
    /// checkout at `dir`, or `None` when it is off or names no task. A run
    /// qualifies only when it ran exactly what the required task runs at
    /// `merge_base`, so a branch cannot redefine the task it is checked by.
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
        let passes: Vec<Run> = runs
            .iter()
            .filter(|r| r.proves_with(&required.task, &required.args, head))
            .cloned()
            .collect();
        let (run, rejected) = if passes.is_empty() {
            let near = runs.iter().find(|r| {
                r.task == required.task && r.exit_code == Some(0) && r.head.as_deref() == Some(head)
            });
            (None, near.and_then(|r| unproven(&required, r)))
        } else {
            qualifying(dir, merge_base, &required, passes)
        };
        Ok(Some(Self {
            mode,
            required,
            head: head.to_string(),
            caller,
            run,
            rejected,
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
        match run.tail.as_deref() {
            Some(lines) if !lines.is_empty() => out.push_str(&output_tail(lines)),
            Some(_) => out.push_str("\nThe run printed nothing.\n"),
            None => out.push_str("\nNo output was captured: it went straight to a terminal.\n"),
        }
        out
    }

    fn not_run(&self) -> String {
        format!(
            "### Verification\n\n\
             NOT RUN: the run ledger records no run of `{command}` that exited 0 on a clean \
             tree at `{head}`.{rejected}\n",
            command = self.required.command(),
            head = self.head,
            rejected = self.rejected_note(),
        )
    }

    fn rejected_note(&self) -> String {
        self.rejected
            .as_deref()
            .map(|why| format!(" {why}"))
            .unwrap_or_default()
    }
}

/// The latest of `passes`, all runs at `head`, that ran what the required
/// task runs at the merge base: the digest of the plan it executed is the
/// digest of the [`task::ExecutionPlan`] the merge base's committed config
/// layers give now, in the directory the run started in, with the
/// worktree's branch and issue as they are now. Or, when none qualifies,
/// why the latest one does not.
fn qualifying(
    dir: &Path,
    merge_base: &str,
    required: &RequiredTask,
    passes: Vec<Run>,
) -> (Option<Run>, Option<String>) {
    let path_to_project = match load::load_quiet(None, dir) {
        Ok(loaded) => loaded.path_to_project,
        Err(e) => {
            return (
                None,
                Some(format!(
                    "A run at that commit passed, but this checkout's config does not load: {e:#}"
                )),
            );
        }
    };
    let task = &required.task;
    let base = short(merge_base);
    let mut rejected = None;
    for pass in passes {
        let why = match (pass.dir.as_deref(), pass.plan.as_deref()) {
            _ if !pass.reused_servers.is_empty() => format!(
                "A run at that commit passed, but its `up` step reused the server already \
                 running for {apps}, which may have started at an earlier commit: stop it with \
                 `devrun down {names}` and run `{command}` again.",
                apps = pass
                    .reused_servers
                    .iter()
                    .map(|a| format!("`{a}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                names = pass
                    .reused_servers
                    .iter()
                    .map(|a| format!("--app {a}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                command = required.command(),
            ),
            (None, _) => format!(
                "A run at that commit passed, but its record does not say which directory it \
                 started in, so what it ran cannot be rebuilt: run `{}` again.",
                required.command()
            ),
            (_, None) => format!(
                "A run at that commit passed, but its record does not say what it ran: run \
                 `{}` again.",
                required.command()
            ),
            (Some(run_dir), Some(ran)) => match task::committed_plan(
                dir,
                merge_base,
                run_dir,
                &path_to_project,
                &pass.worktree,
                task,
                &required.args,
            ) {
                Ok(at_base) if at_base.digest() == ran => return (Some(pass), None),
                Ok(_) => format!(
                    "A run at that commit passed, but it did not run what `{task}` runs at the \
                     merge base {base}: the task's definition changed from the merge base, or \
                     the branch or issue record it renders changed since the run."
                ),
                Err(e) => format!(
                    "A run at that commit passed, but `{task}` as the merge base {base} \
                     defines it does not render: {e:#}"
                ),
            },
        };
        rejected.get_or_insert(why);
    }
    (None, rejected)
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
    if !run.committed_definition {
        why.push(if run.uncommitted_layers.is_empty() {
            "what it ran was not what the committed devkit.toml files define: a config layer \
             outside them (devkit.local.toml, the home config, `--config` or `$DEVKIT_CONFIG`) \
             changed it, so run it without that layer"
                .to_string()
        } else {
            format!(
                "a config layer outside the committed devkit.toml files changed what it ran: \
                 {}; move that layer aside or commit its change",
                run.uncommitted_layers
                    .iter()
                    .map(|l| format!("`{}`", l.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
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
    fn a_tail_within_the_budget_is_quoted_whole() {
        let lines = vec!["one".to_string(), "```two".to_string()];
        assert_eq!(
            output_tail(&lines),
            "\nOutput tail:\n\n````text\none\n```two\n````\n"
        );
    }
}
