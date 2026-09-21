//! The stdio MCP server. `.mcp.json` starts it as `devkit-mcp`, so the shim
//! name stays even though the code now lives in `devkit`.

use std::io::{BufReader, Write};

use anyhow::Result;

#[derive(clap::Args)]
pub struct McpCli {}

pub fn run(_cli: McpCli) -> Result<()> {
    // Resolved once, from where the server was started: it is the identity
    // every mutating action is checked against, so it must not be
    // re-derived per call from anything the caller supplies.
    let cwd = std::env::current_dir().ok();
    let own_worktree = cwd
        .as_deref()
        .and_then(|cwd| devkit_common::git::checkout_root(cwd).ok());
    let ctx = devkit_mcp::ServerCtx {
        default_holder: devkit_mcp::mint_holder(),
        own_worktree,
        enabled: cwd.as_deref().is_none_or(enabled_in),
    };
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    devkit_mcp::run(&mut reader, &mut writer, &ctx)?;
    writer.flush()?;
    Ok(())
}

/// `[mcp] enabled` for `cwd`. An unreadable config falls open to enabled, as
/// the brief does: a typo in an unrelated table must not take the tools away.
fn enabled_in(cwd: &std::path::Path) -> bool {
    devkit_common::config::resolve(None, cwd).map_or(true, |(cfg, _)| cfg.mcp.enabled)
}
