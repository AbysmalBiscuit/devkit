use crate::triage::render;
use anyhow::Result;
use devkit_common::{linear, ui};
use devkit_issue::status::{self as st, StatusReport};
use std::collections::HashMap;

/// Discover worktrees, then fetch PRs and Linear state concurrently behind a
/// numbered group of progress bars, clearing them before the caller renders.
pub fn gather_with_bars(start: &str, ids: &[String]) -> Result<StatusReport> {
    let steps = crate::spin::Steps::new();

    let p1 = steps.spinner("[1/4] Discovering worktrees…");
    let disco = st::discover(start, ids)?;
    p1.finish_and_clear();

    let key = devkit_common::secrets::resolve("LINEAR_API_KEY");
    let has_key = key.is_some();
    if disco.is_empty() {
        steps.clear();
        let prs = st::fetch_prs(&disco)?;
        return Ok(st::assemble(
            disco,
            Vec::new(),
            prs,
            HashMap::new(),
            None,
            has_key,
        ));
    }

    let m = disco.len();
    let paths = disco.worktree_paths();
    let ids_v: Vec<String> = disco.issue_ids().to_vec();

    let bar2 = steps.bar(&format!("[2/4] Checking {m} worktrees"), m as u64);
    let _bar3 = steps.spinner("[3/4] Fetching PRs from GitHub…");
    let _bar4 = steps.spinner("[4/4] Fetching Linear states…");

    let (dirty, prs, linear, ws) = std::thread::scope(|s| {
        let b2 = bar2.clone();
        let dt = s.spawn(move || {
            paths
                .iter()
                .map(|p| {
                    let d = st::dirty_of(p);
                    b2.inc(1);
                    d
                })
                .collect::<Vec<bool>>()
        });
        let pt = s.spawn(|| st::fetch_prs(&disco));
        let lt = s.spawn(|| {
            (
                linear::states(&ids_v, key.as_deref()),
                linear::workspace_url_key(),
            )
        });
        let dirty = dt.join().expect("dirty thread panicked");
        let prs = pt.join().expect("prs thread panicked")?;
        let (linear, ws) = lt.join().expect("linear thread panicked");
        Ok::<_, anyhow::Error>((dirty, prs, linear, ws))
    })?;

    steps.clear();
    Ok(st::assemble(disco, dirty, prs, linear, ws, has_key))
}

pub fn run(start: &str, ids: &[String]) -> Result<()> {
    let report = gather_with_bars(start, ids)?;
    let finished = render(&report);
    if finished > 0 {
        println!(
            "\n{} Run `issue end` to remove them.",
            ui::green(&format!("{finished} finished."))
        );
    }
    if !report.has_linear_key {
        println!(
            "\n{}",
            ui::dim(
                "LINEAR_API_KEY unset — Linear gate skipped. Create a key at https://linear.app/settings/api"
            )
        );
    }
    Ok(())
}
