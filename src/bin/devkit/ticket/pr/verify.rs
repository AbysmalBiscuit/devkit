//! The verify gate: a PR opens at a commit the project's required task is
//! recorded passing on. Only devrun writes the run ledger, so the
//! record is evidence an agent cannot type into the PR body itself. A
//! person can waive the gate for one head with `pr waive-verify`, which
//! refuses an agent caller unless a trusted key signs the waiver.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use devkit_common::{
    caller::Caller,
    vcs::{Checkout, Vcs, VersionControl, Worktree},
};
use devkit_config::{
    Gate as GateKey, GateMode, PrConfig, PrGates, RequiredTask, SignatureFormat, SignatureTools,
    SignatureVerifier,
};
use devkit_ports::{
    ledger::{self, Ledger, Run, Step, Waiver},
    task,
};

use super::signature::AccountTools;

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
    /// The latest run of the required task and args at `head`, when no run
    /// qualifies and that one did not exit 0.
    failed: Option<Run>,
    /// What the PR changes in what the required task runs, when it does.
    change: Option<String>,
    /// The steps `run` executed, when they differ from what HEAD's committed
    /// config defines.
    ran: String,
    /// A person's waiver for `head`, when no run qualifies.
    waiver: Option<Waiver>,
}

impl Gate {
    /// The gate `gates` configures for a PR opened at `head` from
    /// `checkout`, or `None` when it is off or names no task. A branch that
    /// changes what the required task runs since `merge_base` is not refused
    /// for it: the Verification section shows the change.
    pub(crate) fn read(
        checkout: &Checkout,
        merge_base: &str,
        gates: &PrGates,
        head: &str,
        caller: Caller,
        ledger: &Ledger,
    ) -> Result<Option<Self>> {
        let Some((required, mode)) = gates.all().into_iter().find_map(|(gate, mode)| match gate {
            GateKey::Verify(required) => Some((required, mode)),
            GateKey::WeakenedTests | GateKey::Repro(_) => None,
        }) else {
            return Ok(None);
        };
        if mode == GateMode::Off {
            return Ok(None);
        }
        let worktree = checkout.worktree()?;
        let repo = ledger::repo_key(checkout, &worktree.path);
        let runs: Vec<Run> = ledger.runs_in(&repo)?.into_iter().rev().collect();
        let at_head = heads_required(checkout, worktree, head, &required, caller, &runs);
        let (required, moved_from) = match at_head {
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
        let failed = match &run {
            Some(_) => None,
            None => runs
                .iter()
                .find(|r| {
                    r.task == required.task
                        && r.args == required.args
                        && r.head.as_deref() == Some(head)
                })
                .filter(|r| r.exit_code != Some(0))
                .cloned(),
        };
        let run_dir = run.as_ref().and_then(|r| r.dir.as_deref()).unwrap_or("");
        let change = definition_change(
            checkout,
            worktree,
            merge_base,
            head,
            run_dir,
            moved_from.as_ref().unwrap_or(&required),
            &required,
        );
        let ran = run
            .as_ref()
            .map(|r| ran_note(checkout, worktree, head, r, &required))
            .unwrap_or_default();
        let waiver = match run {
            Some(_) => None,
            None => ledger.waiver(&repo, head)?,
        };
        Ok(Some(Self {
            mode,
            required,
            head: head.to_string(),
            caller,
            run,
            rejected,
            failed,
            change,
            ran,
            waiver,
        }))
    }

    /// `body` with the Verification section appended, or a refusal when an
    /// agent's PR under `require` has no qualifying run and no waiver.
    pub(crate) fn apply(&self, body: String) -> Result<String> {
        if self.run.is_none() && self.waiver.is_none() {
            match (self.mode, self.caller) {
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
                (GateMode::Off | GateMode::Advise | GateMode::Require, _) => {}
            }
        }
        let section = self.section();
        let body = body.trim_end();
        Ok(if body.is_empty() {
            section
        } else {
            format!("{body}\n\n{section}")
        })
    }

    /// The Verification section: the qualifying run, else the waiver, else
    /// NOT RUN.
    fn section(&self) -> String {
        match (&self.run, &self.waiver) {
            (Some(run), _) => self.passed(run),
            (None, Some(waiver)) => self.waived(waiver),
            (None, None) => self.not_run(),
        }
    }

    fn waived(&self, waiver: &Waiver) -> String {
        format!(
            "### Verification\n\n\
             WAIVED: {by} accepted `{head}` at {at}{attested} with no run of `{command}` that \
             exited 0 on a clean tree there.\n\n\
             Reason: {reason}\n{failed}{change}",
            by = waiver.by,
            head = self.head,
            at = waiver.at,
            attested = waiver
                .signer
                .as_deref()
                .map(|signer| format!(", attested by {signer},"))
                .unwrap_or_default(),
            command = self.required.command(),
            reason = waiver.reason,
            failed = self.failed_note(),
            change = self.change.as_deref().unwrap_or_default(),
        )
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
        for app in &run.reused_at_head {
            out.push_str(&format!(
                "- Against reused server `{app}`, launched at this commit with the same plan\n"
            ));
        }
        out.push_str(&self.ran);
        out.push_str(self.change.as_deref().unwrap_or_default());
        out
    }

    fn not_run(&self) -> String {
        format!(
            "### Verification\n\n\
             NOT RUN: the run ledger records no run of `{command}` that exited 0 on a clean \
             tree at `{head}`.{rejected}\n{failed}{change}",
            command = self.required.command(),
            head = self.head,
            rejected = self.rejected_note(),
            failed = self.failed_note(),
            change = self.change.as_deref().unwrap_or_default(),
        )
    }

    /// The exit code, tree state and output tail of [`Self::failed`], or
    /// empty when no failed run is recorded.
    fn failed_note(&self) -> String {
        let Some(run) = &self.failed else {
            return String::new();
        };
        let exit = match run.exit_code {
            Some(code) => code.to_string(),
            None => "none: its program never started".to_string(),
        };
        let tail = match run.tail.as_deref() {
            Some(lines) if !lines.is_empty() => output_tail(lines),
            Some(_) => "\nThe run printed nothing.\n".to_string(),
            None => "\nNo output was captured: it went straight to a terminal.\n".to_string(),
        };
        format!(
            "\nThe latest run of `{command}` at `{head}`:\n\n\
             - Exit code: {exit}\n\
             - Tree: {tree}\n{tail}",
            command = self.required.command(),
            head = self.head,
            tree = tree_state(run),
        )
    }

    fn rejected_note(&self) -> String {
        self.rejected
            .as_deref()
            .map(|why| format!(" {why}"))
            .unwrap_or_default()
    }
}

/// `pr verification`: print the Verification section `pr create` writes
/// for a PR opened at `head`, HEAD when `None`, against PR base `base`.
/// Prints the NOT RUN form, never the refusal, when no run qualifies.
pub(crate) fn run(
    dir: &Path,
    config: Option<&str>,
    head: Option<&str>,
    base: Option<&str>,
) -> Result<()> {
    let checkout = Checkout::at(dir);
    let loaded = devkit_ports::load::load_in(&checkout, config.map(Path::new), dir)?;
    let caller = devkit_common::caller::caller();
    let base = base.unwrap_or(&loaded.config.defaults.pr_base);
    let vcs = Vcs::at(dir);
    let head = full_head(&vcs, dir, head)?;
    let (merge_base, cfg) = super::create::gate_settings(&checkout, base, &head)?;
    match Gate::read(
        &checkout,
        &merge_base,
        &cfg.gates,
        &head,
        caller,
        &Ledger::live()?,
    )? {
        Some(gate) => print!("{}", gate.section()),
        None => eprintln!(
            "no Verification section: the `devkit.toml` committed at the merge base turns the \
             verify gate off or names no `verify_task`"
        ),
    }
    Ok(())
}

/// The full commit id `head` names in `dir`, or HEAD's when `None`.
fn full_head(vcs: &Vcs, dir: &Path, head: Option<&str>) -> Result<String> {
    match head {
        Some(rev) => vcs.revision_of(dir, rev),
        None => vcs.revision(dir),
    }
}

/// `pr waive-verify`: record that the person running it accepts `head`,
/// HEAD when `None`, with no qualifying run, so the verify gate passes at
/// that head only. Refuses an agent caller, and any caller without an
/// interactive terminal.
pub(crate) fn waive(dir: &Path, head: Option<&str>, reason: &str) -> Result<()> {
    if devkit_common::caller::caller() == Caller::Agent {
        bail!(
            "refusing to waive the verify gate: an agent caller cannot write a waiver. A person \
             runs `devkit pr waive-verify` in a terminal"
        );
    }
    devkit_common::caller::require_terminal("waiving the verify gate")?;
    let reason = reason.trim();
    if reason.is_empty() {
        bail!("a waiver needs a reason: pass `--reason <text>`");
    }
    let vcs = Vcs::at(dir);
    let head = full_head(&vcs, dir, head)?;
    let Some((name, email)) = vcs.author(dir)? else {
        bail!("a waiver names its writer: set the repository's author name and email");
    };
    let checkout = Checkout::at(dir);
    let waiver = Waiver {
        repo: ledger::repo_key(&checkout, &checkout.worktree()?.path),
        head,
        by: format!("{name} <{email}>"),
        reason: reason.to_string(),
        at: devkit_common::harness_log::now_rfc3339(),
        signer: None,
    };
    let path = Ledger::live()?.waive(&waiver)?;
    eprintln!(
        "Waived the verify gate at `{}`: {}",
        waiver.head,
        path.display()
    );
    Ok(())
}

/// `pr waive-verify --signed`: write the waiver that `record` holds, once
/// `signature` signs it with a key that the user-level config trusts. No
/// terminal is needed and an agent may run it, since only a holder of a
/// trusted key can sign. `signature` is `<record>.sig` when `None`.
pub(crate) fn waive_signed(
    dir: &Path,
    record: &Path,
    signature: Option<&Path>,
    home: &Path,
    ledger: &Ledger,
) -> Result<()> {
    let signature = signature.map_or_else(
        || {
            let mut sig = record.as_os_str().to_owned();
            sig.push(".sig");
            PathBuf::from(sig)
        },
        Path::to_path_buf,
    );
    let tools = AccountTools {
        home: home.to_path_buf(),
    };
    write_signed(
        dir,
        record,
        &signature,
        &trusted_signers(home)?,
        &tools,
        ledger,
    )
}

/// Write the waiver that `record` holds, once `verifier` finds that
/// `signature` signs the bytes of `record` read here.
fn write_signed(
    dir: &Path,
    record: &Path,
    signature: &Path,
    verifier: &impl SignatureVerifier,
    tools: &dyn SignatureTools,
    ledger: &Ledger,
) -> Result<()> {
    let bytes = std::fs::read(record).with_context(|| format!("reading {}", record.display()))?;
    let signer = verifier.verify(tools, &bytes, signature).with_context(|| {
        format!(
            "the signature `{}` on `{}` does not verify against the trusted keys",
            signature.display(),
            record.display()
        )
    })?;
    let mut waiver: Waiver = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is no waiver record", record.display()))?;
    if !devkit_common::vcs::is_object_id(&waiver.head)
        || full_head(&Vcs::at(dir), dir, Some(&waiver.head))? != waiver.head
    {
        bail!(
            "the signed record names `{}`: a waiver covers one commit, so its head is a full \
             commit id",
            waiver.head
        );
    }
    let checkout = Checkout::at(dir);
    let repo = ledger::repo_key(&checkout, &checkout.worktree()?.path);
    if !devkit_common::paths::same_path(&waiver.repo, &repo) {
        bail!(
            "the signed record waives `{}`, not this repository `{}`",
            waiver.repo.display(),
            repo.display()
        );
    }
    if waiver.reason.trim().is_empty() {
        bail!("the signed record gives no reason");
    }
    waiver.repo = repo;
    waiver.signer = Some(signer);
    let path = ledger.waive(&waiver)?;
    eprintln!(
        "Waived the verify gate at `{}` for {}: {}",
        waiver.head,
        waiver.by,
        path.display()
    );
    Ok(())
}

/// The verifier that `[pr.signed_waivers]` configures in
/// `<home>/.config/devkit/config.toml`, where `home` comes from the account
/// database. Neither `$DEVKIT_CONFIG`, `$HOME` nor a project layer moves it,
/// so a caller cannot trust a key of its own.
fn trusted_signers(home: &Path) -> Result<SignatureFormat> {
    let path = home.join(".config/devkit/config.toml");
    let unset = || {
        anyhow::anyhow!(
            "no trusted waiver signers: set `[pr.signed_waivers]` in `{}`",
            path.display()
        )
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(unset()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    PrConfig::from_layer(&text)
        .with_context(|| format!("reading `[pr]` in {}", path.display()))?
        .signed_waivers
        .ok_or_else(unset)
}

/// Why `run`, which passed at the PR's head with the required args, does
/// not qualify: its `up` step reused a server already running before it
/// started, with no record of launching at that commit with the plan the
/// step would launch.
fn reused_note(required: &RequiredTask, run: &Run) -> String {
    format!(
        "A run at that commit passed, but its `up` step reused the server already running for \
         {apps}, which has no record of launching at that commit with the plan the step would \
         launch: stop it with `devrun down {names}` and run `{command}` again.",
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
    checkout: &Checkout,
    worktree: &Worktree,
    head: &str,
    required: &RequiredTask,
    caller: Caller,
    runs: &[Run],
) -> Option<RequiredTask> {
    let applies = |r: &RequiredTask| {
        let run_dir = runs
            .iter()
            .find(|run| run.proves(&r.task, &r.args, head))
            .and_then(|run| run.dir.as_deref())
            .unwrap_or("");
        task::committed_plan(
            checkout,
            head,
            run_dir,
            worktree,
            &r.task,
            &r.args,
            &BTreeMap::new(),
        )
        .is_ok_and(|plan| plan.accepts(caller))
    };
    if applies(required) {
        return None;
    }
    let gates = super::weakened::config_at(checkout, head).ok()?.gates;
    let at_head = gates.all().into_iter().find_map(|(gate, _)| match gate {
        GateKey::Verify(r) => Some(r),
        GateKey::WeakenedTests | GateKey::Repro(_) => None,
    })?;
    applies(&at_head).then_some(at_head)
}

/// The note on what the PR changes in what the required run executes:
/// `at_base` as the `devkit.toml` layers committed at `merge_base` define
/// it for a run started in `run_dir`, against `required` as those
/// committed at `head` do, both rendered for `worktree`. No other layer
/// enters either side, so an uncommitted edit, the primary checkout's
/// config, the home config or a `devkit.local.toml` shows as no change.
/// `None` when nothing differs.
fn definition_change(
    checkout: &Checkout,
    worktree: &Worktree,
    merge_base: &str,
    head: &str,
    run_dir: &str,
    at_base: &RequiredTask,
    required: &RequiredTask,
) -> Option<String> {
    let base_ports = Default::default();
    let plan = |rev, r: &RequiredTask| {
        task::committed_plan(
            checkout,
            rev,
            run_dir,
            worktree,
            &r.task,
            &r.args,
            &base_ports,
        )
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

/// The note that `run` executed other steps than the `devkit.toml` files
/// committed at `head` define the required task to run, listing the steps
/// it executed. A layer outside those files, such as a `devkit.local.toml`,
/// the primary checkout's config, `--config` or `$DEVKIT_CONFIG`, changes
/// what runs without dirtying the tree. Empty when the steps match.
fn ran_note(
    checkout: &Checkout,
    worktree: &Worktree,
    head: &str,
    run: &Run,
    required: &RequiredTask,
) -> String {
    let committed = |ports: &BTreeMap<String, u16>| {
        task::committed_plan(
            checkout,
            head,
            run.dir.as_deref().unwrap_or(""),
            worktree,
            &required.task,
            &required.args,
            ports,
        )
        .map(|plan| plan.commands())
    };
    if matches_committed(&run.steps, committed) != Some(false) {
        return String::new();
    }
    let mut out = format!(
        "\nAn uncommitted config layer changed what ran: these steps differ from what the \
         `devkit.toml` files committed at HEAD define for `{}`.\n",
        required.task
    );
    if !run.steps.is_empty() {
        let lines: Vec<String> = run
            .steps
            .iter()
            .map(|s| match &s.up {
                Some(app) => format!("up: {app}"),
                None => format!("{}: {}", s.task, s.argv.join(" ")),
            })
            .collect();
        out.push_str(&leading_block(&lines, "text"));
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
    if let Some(given) = env_given(run) {
        why.push(format!(
            "it was given {given}, which the committed config does not set, so run it without them"
        ));
    }
    if why.is_empty() && !run.clean {
        why.push(format!("its tree was not clean: {DIRTY_TREE}"));
    }
    (!why.is_empty()).then(|| format!("A run at that commit passed, but {}.", why.join("; and ")))
}

/// Why a run recorded as not clean, and given no env, was not clean.
const DIRTY_TREE: &str =
    "a file was modified, staged or untracked when it started or ended, or HEAD moved during it";

/// The `--env` names and `--env-file` that `run` was given, quoted, or
/// `None` when it was given neither. The values are never recorded.
fn env_given(run: &Run) -> Option<String> {
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
    (!given.is_empty()).then(|| given.join(" and "))
}

/// The tree state `run` recorded, as the section names it. A run given env
/// is recorded as not clean whatever its tree held, so it says so instead.
fn tree_state(run: &Run) -> String {
    match env_given(run) {
        Some(given) => format!("not proven clean: it was given {given}"),
        None if run.clean => "clean".to_string(),
        None => format!("not clean: {DIRTY_TREE}"),
    }
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
    output_tail_within(lines, TAIL_BUDGET)
}

/// [`output_tail`] keeping at most `budget` bytes of `lines`.
pub(super) fn output_tail_within(lines: &[String], budget: usize) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 0;
    let mut cut = false;
    for line in lines.iter().rev() {
        let cost = line.len() + 1;
        if used + cost > budget {
            if kept.is_empty() {
                let mut start = line.len() - budget;
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
            "\nThe line was cut to at most its last {budget} bytes to keep the PR body within \
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
            failed: None,
            change: None,
            ran: String::new(),
            waiver: None,
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

/// `pr waive-verify --signed` below the CLI: the trusted home comes from the
/// account database, which a test cannot point at a temporary directory, so
/// these tests pass the home in. The real `ssh-keygen` and `gpgv` run.
#[cfg(all(test, unix))]
mod signed_waiver_tests {
    use std::process::{Command, Stdio};

    use super::*;

    const ACCEPTED: &str = "accepted on the PR by its owner";

    /// A repository with one commit on `main`, and that commit's full id.
    struct Repo {
        dir: tempfile::TempDir,
        head: String,
    }

    impl Repo {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let git = |args: &[&str]| {
                devkit_git::Git::fixture(dir.path())
                    .args(args.iter().copied())
                    .output()
                    .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
            };
            git(&["init", "-q", "-b", "main"]);
            std::fs::write(dir.path().join("feature.txt"), "a thing\n").unwrap();
            git(&["add", "-A"]);
            git(&["commit", "-q", "-m", "a thing"]);
            let head = git(&["rev-parse", "HEAD"]).trim().to_string();
            Self { dir, head }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn key(&self) -> PathBuf {
            std::fs::canonicalize(self.path()).unwrap()
        }

        /// A record for `rev` in `repo`, written to `dir/waiver.json`.
        fn record(&self, dir: &Path, repo: &Path, rev: &str) -> PathBuf {
            let record = serde_json::json!({
                "repo": repo,
                "head": rev,
                "by": "Owen Owner <owen@example.com>",
                "reason": ACCEPTED,
                "at": "2026-11-12T10:00:00Z",
            });
            let path = dir.join("waiver.json");
            std::fs::write(&path, serde_json::to_string_pretty(&record).unwrap()).unwrap();
            path
        }

        /// The Verification section an agent's `pr create` writes at HEAD.
        fn section(&self, ledger: &Ledger) -> Result<String> {
            let gates = PrGates {
                verify_task: Some("verify".into()),
                ..PrGates::default()
            };
            let gate = Gate::read(
                &Checkout::at(self.path()),
                &self.head,
                &gates,
                &self.head,
                Caller::Agent,
                ledger,
            )?
            .expect("the gate is on");
            gate.apply(String::new())
        }
    }

    fn run_in(dir: &Path, program: &str, args: &[&str]) {
        let out = Command::new(program)
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("{program}: {e}"));
        assert!(
            out.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `<home>/.config/devkit/config.toml` trusting `trusted_keys`.
    fn trust(home: &Path, format: &str, trusted_keys: &str) {
        trust_with(home, format, trusted_keys, "");
    }

    /// [`trust`] with `extra` lines in the table, such as a `program`.
    fn trust_with(home: &Path, format: &str, trusted_keys: &str, extra: &str) {
        let dir = home.join(".config/devkit");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[pr.signed_waivers]\nformat = \"{format}\"\ntrusted_keys = \"{trusted_keys}\"\n{extra}"
            ),
        )
        .unwrap();
    }

    /// The absolute path of `tool` on this machine, as an owner would write
    /// it into `program` where the system directories lack the tool.
    fn installed(tool: &str) -> String {
        let out = Command::new("sh")
            .args(["-c", &format!("command -v {tool}")])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// An ssh key at `dir/<name>`, and `dir/allowed_signers` trusting it as
    /// `release-bot@example.com` for waivers.
    fn ssh_signer(dir: &Path, name: &str) {
        run_in(dir, "ssh-keygen", &[
            "-q", "-t", "ed25519", "-N", "", "-C", name, "-f", name,
        ]);
        let public = std::fs::read_to_string(dir.join(format!("{name}.pub"))).unwrap();
        std::fs::write(
            dir.join("allowed_signers"),
            format!("release-bot@example.com namespaces=\"{SSH_NAMESPACE}\" {public}"),
        )
        .unwrap();
    }

    fn ssh_sign(dir: &Path, key: &str, record: &Path) {
        let _ = std::fs::remove_file(format!("{}.sig", record.display()));
        run_in(dir, "ssh-keygen", &[
            "-q",
            "-Y",
            "sign",
            "-f",
            key,
            "-n",
            SSH_NAMESPACE,
            record.to_str().unwrap(),
        ]);
    }

    use devkit_config::SSH_NAMESPACE;

    #[test]
    fn a_record_signed_by_a_trusted_ssh_key_passes_the_verify_gate() {
        let repo = Repo::new();
        let home = tempfile::tempdir().unwrap();
        ssh_signer(home.path(), "owner");
        trust(home.path(), "ssh", "~/allowed_signers");
        let record = repo.record(home.path(), &repo.key(), &repo.head);
        ssh_sign(home.path(), "owner", &record);
        let ledger = Ledger::at(home.path().join("runs"));
        assert!(repo.section(&ledger).is_err(), "no run and no waiver");

        waive_signed(repo.path(), &record, None, home.path(), &ledger).unwrap();
        let section = repo.section(&ledger).unwrap();
        assert!(section.contains("WAIVED: "), "{section}");
        assert!(
            section.contains("Owen Owner <owen@example.com> accepted"),
            "names the person who accepted: {section}"
        );
        assert!(
            section.contains("attested by release-bot@example.com"),
            "names the verified signer: {section}"
        );
        assert!(section.contains(ACCEPTED), "{section}");
    }

    #[test]
    fn a_record_signed_by_a_trusted_openpgp_key_passes_the_verify_gate() {
        let repo = Repo::new();
        let home = tempfile::tempdir().unwrap();
        let gnupg = home.path().join("gnupg");
        std::fs::create_dir(&gnupg).unwrap();
        std::fs::set_permissions(&gnupg, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let gpg = |args: &[&str]| {
            let mut argv = vec!["--homedir", gnupg.to_str().unwrap(), "--batch", "--quiet"];
            argv.extend_from_slice(args);
            run_in(home.path(), "gpg", &argv);
        };
        gpg(&[
            "--passphrase",
            "",
            "--quick-gen-key",
            "Release Bot <release-bot@example.com>",
            "ed25519",
            "sign",
            "never",
        ]);
        gpg(&[
            "--output",
            "trusted.gpg",
            "--export",
            "release-bot@example.com",
        ]);
        trust_with(
            home.path(),
            "openpgp",
            "~/trusted.gpg",
            &format!("program = \"{}\"\n", installed("gpgv")),
        );
        let record = repo.record(home.path(), &repo.key(), &repo.head);
        gpg(&["--detach-sign", record.to_str().unwrap()]);
        let ledger = Ledger::at(home.path().join("runs"));

        waive_signed(repo.path(), &record, None, home.path(), &ledger).unwrap();
        let section = repo.section(&ledger).unwrap();
        assert!(
            section.contains("Owen Owner <owen@example.com> accepted"),
            "{section}"
        );
        assert!(
            section.contains("attested by Release Bot <release-bot@example.com> ("),
            "names the verified signer and its fingerprint: {section}"
        );
    }

    #[test]
    fn a_signed_record_must_name_a_full_commit_id() {
        let repo = Repo::new();
        let home = tempfile::tempdir().unwrap();
        ssh_signer(home.path(), "owner");
        trust(home.path(), "ssh", "~/allowed_signers");
        let ledger = Ledger::at(home.path().join("runs"));
        for rev in ["HEAD", "main", &repo.head[..12]] {
            let record = repo.record(home.path(), &repo.key(), rev);
            ssh_sign(home.path(), "owner", &record);
            let err = waive_signed(repo.path(), &record, None, home.path(), &ledger).unwrap_err();
            assert!(
                format!("{err:#}").contains("full commit id"),
                "`{rev}` could name a later commit: {err:#}"
            );
        }
        assert!(repo.section(&ledger).is_err(), "no waiver was written");
    }

    /// Swaps the record file for a forgery once it has checked the bytes it
    /// was given, so a second read of the file sees the forgery.
    struct Swapping<'a> {
        record: &'a Path,
    }

    impl SignatureVerifier for Swapping<'_> {
        fn verify(
            &self,
            _tools: &dyn SignatureTools,
            message: &[u8],
            _signature: &Path,
        ) -> Result<String> {
            let text = std::str::from_utf8(message)?;
            assert!(text.contains(ACCEPTED), "{text}");
            std::fs::write(
                self.record,
                text.replace(ACCEPTED, "forged after the check"),
            )?;
            Ok("swapper".into())
        }
    }

    #[test]
    fn the_waiver_holds_the_record_bytes_that_the_signature_verified() {
        let repo = Repo::new();
        let home = tempfile::tempdir().unwrap();
        let record = repo.record(home.path(), &repo.key(), &repo.head);
        let ledger = Ledger::at(home.path().join("runs"));

        write_signed(
            repo.path(),
            &record,
            Path::new("unused.sig"),
            &Swapping { record: &record },
            &AccountTools {
                home: home.path().to_path_buf(),
            },
            &ledger,
        )
        .unwrap();
        let section = repo.section(&ledger).unwrap();
        assert!(section.contains(ACCEPTED), "{section}");
        assert!(!section.contains("forged after the check"), "{section}");
    }

    #[test]
    fn a_record_writes_nothing_unless_a_trusted_key_signs_it_for_this_repository() {
        let repo = Repo::new();
        let home = tempfile::tempdir().unwrap();
        let ledger = Ledger::at(home.path().join("runs"));
        let agent = tempfile::tempdir().unwrap();
        ssh_signer(agent.path(), "agent");
        let record = repo.record(agent.path(), &repo.key(), &repo.head);
        ssh_sign(agent.path(), "agent", &record);

        std::fs::write(
            repo.path().join("devkit.toml"),
            format!(
                "[pr.signed_waivers]\nformat = \"ssh\"\ntrusted_keys = \"{}\"\n",
                agent.path().join("allowed_signers").display()
            ),
        )
        .unwrap();
        let err = waive_signed(repo.path(), &record, None, home.path(), &ledger).unwrap_err();
        assert!(
            format!("{err:#}").contains("no trusted waiver signers"),
            "a project layer cannot trust a key: {err:#}"
        );

        ssh_signer(home.path(), "owner");
        trust(home.path(), "ssh", "~/allowed_signers");
        let err = waive_signed(repo.path(), &record, None, home.path(), &ledger).unwrap_err();
        assert!(
            format!("{err:#}").contains("does not verify"),
            "a key the account does not trust signs nothing: {err:#}"
        );

        let elsewhere = repo.record(home.path(), Path::new("/some/other/repo"), &repo.head);
        ssh_sign(home.path(), "owner", &elsewhere);
        let err = waive_signed(repo.path(), &elsewhere, None, home.path(), &ledger).unwrap_err();
        assert!(
            format!("{err:#}").contains("/some/other/repo"),
            "a record for another repository waives nothing here: {err:#}"
        );
        assert!(repo.section(&ledger).is_err(), "no waiver was written");
    }

    #[test]
    fn a_relative_trusted_key_path_is_refused_for_either_format() {
        let repo = Repo::new();
        let home = tempfile::tempdir().unwrap();
        ssh_signer(home.path(), "owner");
        let record = repo.record(home.path(), &repo.key(), &repo.head);
        ssh_sign(home.path(), "owner", &record);
        let ledger = Ledger::at(home.path().join("runs"));
        for format in ["ssh", "openpgp"] {
            trust_with(
                home.path(),
                format,
                "allowed_signers",
                "program = \"/bin/sh\"\n",
            );
            let err = waive_signed(repo.path(), &record, None, home.path(), &ledger).unwrap_err();
            assert!(
                format!("{err:#}").contains("`allowed_signers` is relative"),
                "{format}: the caller's directory would pick the keys: {err:#}"
            );
        }
        assert!(repo.section(&ledger).is_err(), "no waiver was written");
    }

    #[test]
    fn the_verifier_program_comes_from_the_config_or_the_system_directories() {
        let tools = AccountTools {
            home: PathBuf::from("/home/owner"),
        };
        let found = tools.program("ssh-keygen", None).unwrap();
        assert!(
            ["/usr/bin", "/bin"]
                .iter()
                .any(|dir| found.parent() == Some(Path::new(dir))),
            "{}",
            found.display()
        );
        assert_eq!(
            tools.program("gpgv", Some("/opt/gnupg/bin/gpgv")).unwrap(),
            PathBuf::from("/opt/gnupg/bin/gpgv")
        );
        let err = tools.program("gpgv", Some("bin/gpgv")).unwrap_err();
        assert!(err.to_string().contains("relative"), "{err:#}");
    }
}
