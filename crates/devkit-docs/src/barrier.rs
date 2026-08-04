//! Test rendezvous controlled by an environment variable.
//!
//! Files are `<base>.<suffix>`, where `<base>` is the variable value. Bounded
//! waits prevent a broken rendezvous from hanging a CI worker indefinitely.
//!
//! Every suffix in the tree, and where it is reached. A test that sets [`VAR`]
//! must release each `wait` the processes it spawns will run into, or one of
//! them stalls for a minute and fails there rather than where the test looks:
//!
//! | suffix | kind | reached at |
//! |---|---|---|
//! | `ready`, `go` | signal, wait | `add_library`, between the manifest read and the manifest write, holding only the library lock |
//! | `manifest-ready`, `manifest-go` | signal, wait | `manifest::upsert_global`, between its read and its write, holding the manifest lock |
//! | `materialized`, `commit` | signal, wait | `resolve::resolve_locked`, after materialization and before the reference-registry commit |
//! | `contended.<stem>` | signal | `locks::hold`, after a non-blocking acquisition of `<stem>.lock` fails — reachable only while another process holds that same lock |
//!
//! The two manifest rendezvous are named apart deliberately, and the
//! contention signal carries its lock file's stem. Sharing one name lets a
//! pause taken *inside* the manifest lock satisfy a wait meant for a pause
//! taken outside it, and a race test then cannot tell a contender blocked on
//! the lock it is about from one blocked on a different lock.

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
