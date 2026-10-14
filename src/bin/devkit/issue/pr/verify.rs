//! The verify gate: a PR opens at a commit the project's required task is
//! recorded passing on. Only `devrun task` writes the run ledger, so the
//! record is evidence an agent cannot type into the PR body itself.

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result, bail};
use devkit_common::{
    caller::Caller,
    vcs::{Vcs, VersionControl},
};
use devkit_config::{CONFIG_FILE, Config, Gate as GateKey, GateMode, PrGates, RequiredTask};
use devkit_ports::{
    apps::{self, App},
    doppler,
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
}

impl Gate {
    /// The gate `gates` configures for a PR opened at `head` from the
    /// checkout at `dir`, or `None` when it is off or names no task. A run
    /// qualifies only when the steps it ran are the ones the required task
    /// renders to as `merge_base` defines it, so a branch cannot redefine
    /// the task it is checked by.
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
        let passes: Vec<Run> = ledger
            .runs_in(&repo)?
            .into_iter()
            .rev()
            .filter(|r| r.proves_with(&required.task, &required.args, head))
            .collect();
        let (run, rejected) = if passes.is_empty() {
            (None, None)
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
            Some(lines) if !lines.is_empty() => {
                let fence = fence(lines);
                out.push_str(&format!(
                    "\nOutput tail:\n\n{fence}text\n{}\n{fence}\n",
                    lines.join("\n")
                ));
            }
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

/// The latest of `passes` whose steps are the ones `required` renders to,
/// at the run's own worktree, as the merge base defines the task; or, when
/// none is, why the latest one is not.
fn qualifying(
    dir: &Path,
    merge_base: &str,
    required: &RequiredTask,
    passes: Vec<Run>,
) -> (Option<Run>, Option<String>) {
    let (cfg, catalog) = match tasks_at(dir, merge_base) {
        Ok(found) => found,
        Err(e) => {
            return (
                None,
                Some(format!(
                    "A run at that commit passed, but the task's definition at the merge base \
                     cannot be read: {e:#}"
                )),
            );
        }
    };
    let mut rejected = None;
    for pass in passes {
        let planned = task::planned_steps(
            &cfg,
            &catalog,
            &pass.worktree,
            &required.task,
            &required.args,
        );
        let why = match planned {
            Ok(steps) if steps == pass.steps => return (Some(pass), None),
            Ok(_) => format!(
                "A run at that commit passed, but it ran other steps than `{task}` has at the \
                 merge base {base}: the task's definition changed from the merge base.",
                task = required.task,
                base = short(merge_base),
            ),
            Err(e) => format!(
                "A run at that commit passed, but `{task}` as the merge base {base} defines it \
                 does not render: {e:#}",
                task = required.task,
                base = short(merge_base),
            ),
        };
        rejected.get_or_insert(why);
    }
    (None, rejected)
}

/// The config and app catalog the required task renders from, as the
/// `devkit.toml` committed at `rev` defines them.
fn tasks_at(dir: &Path, rev: &str) -> Result<(Config, HashMap<String, App>)> {
    let cfg = match Vcs::at(dir).file_at(dir, rev, CONFIG_FILE)? {
        Some(text) => Config::tasks_layer(&text)
            .with_context(|| format!("reading {CONFIG_FILE} at {}", short(rev)))?,
        None => Config::default(),
    };
    let yaml = devkit_config::expand_tilde(&cfg.defaults.doppler_yaml);
    let p2p = match std::fs::read_to_string(&yaml) {
        Ok(y) => doppler::path_to_project(&y)?,
        Err(_) => HashMap::new(),
    };
    let (catalog, _) = apps::catalog(&cfg, &p2p)?;
    Ok((cfg, catalog))
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(12)]
}

/// A code fence longer than any run of backticks in `lines`, so the output
/// cannot close it early.
fn fence(lines: &[String]) -> String {
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
        assert_eq!(fence(&["plain".into()]), "```");
        assert_eq!(fence(&["```rust".into(), "x".into()]), "````");
        assert_eq!(fence(&["a ````` b".into()]), "``````");
    }
}
