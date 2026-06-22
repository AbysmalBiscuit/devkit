use crate::triage::render;
use anyhow::Result;
use devkit_common::ui;

pub fn run(start: &str, ids: &[String]) -> Result<()> {
    let pb = crate::spin::spinner("Discovering worktrees…");
    let report = devkit_issue::status::gather(start, ids)?;
    pb.finish_and_clear();
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
