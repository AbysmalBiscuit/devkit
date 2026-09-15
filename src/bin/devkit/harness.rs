//! `devkit harness shell`, retired in favour of `devkit hook pre-tool-use`.
//!
//! Kept as a hidden alias because an installed plugin manifest can outlive the
//! binary it was installed beside: a manifest naming a subcommand the binary
//! lacks is an error on every tool call, and `docs/agents.md` records that a
//! binary the bootstrap hook did not install is never upgraded.

use anyhow::Result;
use clap::{Args, Subcommand};

#[derive(Args)]
pub struct HarnessCli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Guard a shell command about to run, reading the payload on stdin.
    #[command(hide = true)]
    Shell,
}

pub fn run(cli: HarnessCli) -> Result<()> {
    match cli.cmd {
        Cmd::Shell => crate::hook::pre_tool_use(None),
    }
}
