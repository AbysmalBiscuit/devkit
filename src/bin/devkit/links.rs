//! Create the old command names as hardlinks beside the running executable.
//!
//! A hardlink is a second name for the same inode: no disk cost, no exec-time
//! indirection, and no symlink privilege requirement on Windows. It also keeps
//! `argv[0]` reporting the name the caller typed, which is what dispatch reads.

use anyhow::{Context, Result};
use std::path::Path;

use crate::shim::SHIMS;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Replaced,
    AlreadyLinked,
    SkippedForeign,
    Failed(String),
}

#[derive(clap::Args)]
pub struct InstallLinksArgs {
    /// Claim a shim name even when the file there is not a devkit binary.
    #[arg(long)]
    pub force: bool,
}

/// Whether two paths name the same file, so an existing correct hardlink is
/// left alone instead of being deleted and recreated. On Windows that matters
/// beyond tidiness: deleting a running executable fails.
pub fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ma.dev() == mb.dev() && ma.ino() == mb.ino()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ma.file_size() == mb.file_size() && ma.creation_time() == mb.creation_time()
    }
}

/// Whether the file at `path` is a devkit binary, asked by running it. A file
/// that will not execute, or whose output does not name the package, is
/// treated as foreign and left alone.
pub fn is_devkit_binary(path: &Path) -> bool {
    let Ok(out) = std::process::Command::new(path).arg("--version").output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // Any devkit binary, current or older, prints a name from the shim set or
    // `devkit` itself, followed by a version.
    SHIMS
        .iter()
        .map(|s| s.name)
        .chain(std::iter::once("devkit"))
        .any(|n| text.starts_with(n))
}

fn shim_file_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Link every shim name in `dir` at `exe`. Returns one outcome per shim, in
/// `SHIMS` order, so the caller renders and exits on the whole set.
pub fn link_all(exe: &Path, dir: &Path, force: bool) -> Vec<(&'static str, Outcome)> {
    SHIMS
        .iter()
        .map(|s| {
            (
                s.name,
                link_one(exe, &dir.join(shim_file_name(s.name)), force),
            )
        })
        .collect()
}

fn link_one(exe: &Path, dest: &Path, force: bool) -> Outcome {
    if dest.exists() {
        if same_file(exe, dest) {
            return Outcome::AlreadyLinked;
        }
        if !force && !is_devkit_binary(dest) {
            return Outcome::SkippedForeign;
        }
        if let Err(e) = std::fs::remove_file(dest) {
            return Outcome::Failed(format!("removing {}: {e}", dest.display()));
        }
        return match std::fs::hard_link(exe, dest) {
            Ok(()) => Outcome::Replaced,
            Err(e) => Outcome::Failed(format!("linking {}: {e}", dest.display())),
        };
    }
    match std::fs::hard_link(exe, dest) {
        Ok(()) => Outcome::Created,
        Err(e) => Outcome::Failed(format!("linking {}: {e}", dest.display())),
    }
}

pub fn run(args: InstallLinksArgs) -> Result<()> {
    let exe = std::env::current_exe().context("resolving the running executable")?;
    let dir = exe
        .parent()
        .context("the running executable has no parent directory")?;
    let results = link_all(&exe, dir, args.force);
    let mut failed = 0;
    for (name, outcome) in &results {
        match outcome {
            Outcome::Created => println!("created   {name}"),
            Outcome::Replaced => println!("replaced  {name}"),
            Outcome::AlreadyLinked => println!("current   {name}"),
            Outcome::SkippedForeign => {
                println!("skipped   {name} (not a devkit binary; --force to claim it)");
            }
            Outcome::Failed(e) => {
                failed += 1;
                eprintln!("failed    {name}: {e}");
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} link(s) could not be created");
    Ok(())
}
