//! `devkit ticket event`: move a ticket's tracker status as `[issue.events]`
//! configures one event. Every trigger ends here: `workspace setup` and
//! `pr create` inline, the SessionStart hook in the background, and a
//! person rerunning one by hand.

use std::path::Path;

use anyhow::{Context, Result, bail};
use devkit_common::{
    record,
    tracker::{
        self,
        status::{Outcome, StatusWriter, writer_for},
    },
    vcs::{Vcs, VersionControl},
};
use devkit_config::{Health, IssueEvent};

/// The `ticket event` argument, spelled as `[issue.events]` spells it.
#[derive(Clone, Copy, Debug, PartialEq, clap::ValueEnum)]
pub(crate) enum EventArg {
    Setup,
    Start,
    #[value(name = "pr_open")]
    PrOpen,
}

impl From<EventArg> for IssueEvent {
    fn from(e: EventArg) -> Self {
        match e {
            EventArg::Setup => IssueEvent::Setup,
            EventArg::Start => IssueEvent::Start,
            EventArg::PrOpen => IssueEvent::PrOpen,
        }
    }
}

/// The issue `dir`'s worktree record names, when it names a tracker issue
/// and still binds the branch checked out there.
fn recorded_issue(dir: &Path) -> Result<String> {
    let root = devkit_common::vcs::checkout_root(dir)?;
    let branch = Vcs::at(&root).branch(&root)?;
    record::read_on(&root, &branch)
        .and_then(|r| r.tracker_issue())
        .with_context(|| format!("{} records no tracker issue; pass ISSUE", root.display()))
}

/// Fire `event` for `issue`, or for the issue `dir`'s worktree records:
/// read its status and move it when `[issue.events.<event>]` allows. Returns
/// the line that reports what happened. Claiming the event is the trigger's
/// job, not this one's, so a rerun by hand always runs.
pub(crate) fn fire(
    dir: &Path,
    config: Option<&Path>,
    event: IssueEvent,
    issue: Option<&str>,
) -> Result<String> {
    let start = dir.to_string_lossy();
    let sel = tracker::select(config, &start, None);
    if let Health::Broken(e) = &sel.health {
        bail!("devkit.toml does not load: {e}");
    }
    let cfg = sel.config.unwrap_or_default();
    let Some(t) = cfg.issue.events.get(event) else {
        return Ok(format!("[issue.events.{event}] is not configured"));
    };
    let writer = writer_for(sel.tracker.tracker.kind(), &cfg.github, &sel.forge.repos)?;
    let id = match issue {
        Some(input) => sel.tracker.tracker.issue_ref(input)?.id,
        None => recorded_issue(dir)?,
    };
    let shown = |s: &Option<String>| s.clone().unwrap_or_else(|| "(none)".into());
    Ok(match writer.move_status(&id, event, t)? {
        Outcome::Moved { from, to } => format!("moved {id}: {} -> {to}", shown(&from)),
        Outcome::Already(current) => format!("{id} is already {current}"),
        Outcome::NotFrom(current) => format!(
            "{id} is {}, not in [issue.events.{event}] from",
            shown(&current)
        ),
    })
}

/// Fire `event` from inside another command: report the outcome on stderr,
/// and turn a failure into a warning so the command it rides on succeeds.
pub(crate) fn fire_inline(dir: &Path, config: Option<&Path>, event: IssueEvent, issue: &str) {
    match fire(dir, config, event, Some(issue)) {
        Ok(line) => eprintln!("{line}"),
        Err(e) => eprintln!("warning: ticket event {event}: {e:#}"),
    }
}

pub(crate) fn run(
    dir: &Path,
    config: Option<&Path>,
    event: EventArg,
    issue: Option<&str>,
    log_file: Option<&Path>,
) -> Result<()> {
    let event = IssueEvent::from(event);
    let fired = fire(dir, config, event, issue).with_context(|| format!("ticket event {event}"));
    if let Some(path) = log_file {
        let line = match &fired {
            Ok(line) => line.clone(),
            Err(e) => format!("error: {e:#}"),
        };
        append_log(path, &line);
    }
    eprintln!("{}", fired?);
    Ok(())
}

/// Append `line` to `path` with a timestamp. Best-effort: a log that cannot
/// be written must not turn a successful move into a failure.
fn append_log(path: &Path, line: &str) {
    use std::io::Write;

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let now = devkit_common::harness_log::writer::now_rfc3339();
        let _ = writeln!(f, "{now} {line}");
    }
}
