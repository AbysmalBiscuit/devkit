//! `devkit hook-log`: read and sweep the harness log.
//!
//! Named `hook-log` rather than `log`. `paths::logs_dir()` is already
//! `state_dir()/logs` for daemon and server logs and `devrun logs` already
//! tails those, so a bare `devkit log` would name the wrong thing twice. Part A
//! also empties the `harness` CLI family, so putting these under `harness log`
//! would resurrect that verb for an unrelated purpose.
//!
//! No short-name hardlink: this is occasional rather than per-session.
//!
//! Cohort statistics stay offline. The shipped surface is `path`, `prune` and
//! the `doctor` row; `crates/devkit-command/examples/corpus_probe.rs` is where
//! analysis of the corpus lives, behind the `corpus` feature, so its output
//! format is free to churn.

use anyhow::Result;
use clap::{Args, Subcommand};
use devkit_common::harness_log;

#[derive(Args)]
pub struct HookLogCli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the resolved log directory, for piping into jq.
    Path,
    /// Delete records past the configured retention caps.
    Prune {
        /// Report what would go without deleting anything.
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn run(cli: HookLogCli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let settings = harness_log::resolve(&cwd);
    match cli.cmd {
        Cmd::Path => {
            println!("{}", settings.dir.display());
            Ok(())
        }
        Cmd::Prune { dry_run } => prune(&settings, dry_run),
    }
}

fn prune(settings: &harness_log::Settings, dry_run: bool) -> Result<()> {
    if settings.max_age_days.is_none() && settings.max_bytes.is_none() {
        println!(
            "no retention caps set; nothing to prune. \
             Set [harness.log] max_age_days or max_bytes in the global config."
        );
        return Ok(());
    }
    if dry_run {
        println!(
            "would sweep {} against max_age_days={} max_bytes={}",
            settings.dir.display(),
            settings
                .max_age_days
                .map_or("unlimited".to_string(), |d| d.to_string()),
            settings
                .max_bytes
                .map_or("unlimited".to_string(), |b| b.to_string()),
        );
        return Ok(());
    }
    let out = harness_log::prune::sweep(settings);
    // A bail is silent on the auto path and said out loud here: someone who
    // typed the command is owed the reason nothing happened.
    if out.bailed {
        println!("another prune holds the lock; nothing swept");
        return Ok(());
    }
    println!(
        "removed {} file(s) and {} empty director(ies), freeing {} bytes",
        out.files_removed, out.dirs_removed, out.bytes_freed
    );
    if !out.cap_reached {
        println!(
            "the size cap could not be met without deleting today's records or a live \
             session's; stopped instead"
        );
    }
    Ok(())
}
