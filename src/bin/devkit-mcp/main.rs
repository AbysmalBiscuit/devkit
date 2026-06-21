use std::io::{BufReader, Write};

use anyhow::Result;

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkit-mcp");
    devkit_common::paths::migrate_legacy_state();
    let ctx = devkit_mcp::ServerCtx {
        default_holder: devkit_mcp::mint_holder(),
    };
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    devkit_mcp::run(&mut reader, &mut writer, &ctx)?;
    writer.flush()?;
    Ok(())
}
