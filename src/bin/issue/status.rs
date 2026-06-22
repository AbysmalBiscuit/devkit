use crate::triage::{gather, render};
use anyhow::Result;
use devkit_common::ui;

pub fn run(start: &str, ids: &[String]) -> Result<()> {
    let (rows, states, has_key, url_key) = gather(start, ids)?;
    let finished = render(&rows, &states, has_key, url_key.as_deref());
    if finished > 0 {
        println!(
            "\n{} Run `issue end` to remove them.",
            ui::green(&format!("{finished} finished."))
        );
    }
    if !has_key {
        println!(
            "\n{}",
            ui::dim(
                "LINEAR_API_KEY unset — Linear gate skipped. Create a key at https://linear.app/settings/api"
            )
        );
    }
    Ok(())
}
