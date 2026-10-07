use anyhow::Result;

/// Link the old command names, then add devkit's ignore patterns to git's
/// global excludes file. The excludes step runs even when a link fails, so one
/// unclaimable name does not leave the machine half set up.
pub fn run(args: crate::links::InstallLinksArgs) -> Result<()> {
    let linked = crate::links::run(args);
    let (path, added) = match devkit_common::gitignore::ensure_ignored() {
        Ok(ensured) => ensured,
        Err(e) => {
            if let Err(link_err) = &linked {
                eprintln!("devkit install: {link_err:#}");
            }
            return Err(e);
        }
    };
    if added.is_empty() {
        println!("current   {}", path.display());
    }
    for pattern in added {
        println!("added     {pattern} to {}", path.display());
    }
    linked
}
