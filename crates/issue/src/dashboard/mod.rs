use crate::{prs, triage};
use anyhow::Result;

// Task 4.5 wires these items into the timeline rendering; allow until then.
#[allow(dead_code)]
mod bucket;

// Task 4.5 consumes chart's render_stacked_bars, render_lines, and term_width.
#[allow(dead_code)]
mod chart;

// bucket, chart, mode, all_roles, and author are unused until Phase 4's timeline
// rendering. Remove this allow when Task 4.5 implements those features.
#[allow(dead_code)]
pub struct DashboardArgs {
    pub bucket: String,
    pub chart: String,
    pub mode: String,
    pub all_roles: bool,
    pub author: Option<String>,
    pub no_plots: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub fn run(args: DashboardArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());

    // At-a-glance: worktree triage, then my PRs + PRs awaiting my review.
    let (rows, states, has_key, url_key) = triage::gather(&start, &[])?;
    triage::render(&rows, &states, has_key, url_key.as_deref());
    println!();
    prs::run(true, true, None, false)?;

    if args.no_plots {
        return Ok(());
    }

    // Timelines (Phase 4).
    Ok(())
}
