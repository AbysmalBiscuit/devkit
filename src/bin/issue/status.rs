use crate::triage::{gather, render};
use anyhow::Result;

pub fn run(start: &str, ids: &[String]) -> Result<()> {
    let (rows, states, has_key, url_key) = gather(start, ids)?;
    let finished = render(&rows, &states, has_key, url_key.as_deref());
    if finished > 0 {
        println!("\n{finished} finished. Run `issue end` to remove them.");
    }
    if !has_key {
        println!(
            "\nLINEAR_API_KEY unset — Linear gate skipped. Create a key at https://linear.app/settings/api"
        );
    }
    Ok(())
}
