//! The stdio MCP server. `.mcp.json` starts it as `devkit-mcp`, so the shim
//! name stays even though the code now lives in `devkit`.

use anyhow::Result;
use std::io::{BufReader, Write};

#[derive(clap::Args)]
pub struct McpCli {}

pub fn run(_cli: McpCli) -> Result<()> {
    // Resolved once, from where the server was started: it is the identity every
    // mutating action is checked against, so it must not be re-derived per call
    // from anything the caller supplies.
    let own_worktree = std::env::current_dir()
        .ok()
        .and_then(|cwd| devkit_common::git::checkout_root(&cwd).ok());
    let ctx = devkit_mcp::ServerCtx {
        default_holder: devkit_mcp::mint_holder(),
        own_worktree,
    };
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    devkit_mcp::run(&mut reader, &mut writer, &ctx)?;
    writer.flush()?;
    Ok(())
}
