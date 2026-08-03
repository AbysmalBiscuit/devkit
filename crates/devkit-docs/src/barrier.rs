//! Test rendezvous controlled by an environment variable.
//!
//! Files are `<base>.<suffix>`, where `<base>` is the variable value. Bounded
//! waits prevent a broken rendezvous from hanging a CI worker indefinitely.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const VAR: &str = "DEVKIT_DOCS_MANIFEST_BARRIER";
const TIMEOUT: Duration = Duration::from_secs(60);

fn base() -> Option<PathBuf> {
    std::env::var_os(VAR).map(PathBuf::from)
}

pub fn signal(suffix: &str) -> Result<()> {
    let Some(base) = base() else {
        return Ok(());
    };
    let path = base.with_extension(suffix);
    let parent = path.parent().context("barrier path has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    std::fs::write(&path, "").with_context(|| format!("signalling {}", path.display()))
}

pub fn wait(suffix: &str) -> Result<()> {
    let Some(base) = base() else {
        return Ok(());
    };
    let path = base.with_extension(suffix);
    let deadline = Instant::now() + TIMEOUT;
    while !path
        .try_exists()
        .with_context(|| format!("checking {}", path.display()))?
    {
        if Instant::now() > deadline {
            bail!(
                "barrier timed out after {TIMEOUT:?} waiting for {}",
                path.display()
            );
        }
        std::thread::yield_now();
    }
    Ok(())
}
