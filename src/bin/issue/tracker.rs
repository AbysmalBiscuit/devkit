//! Which tracker this project's commands talk to.

use devkit_common::tracker::Tracker;
use devkit_ports::load;
use std::path::Path;

/// The tracker named by `[tracker] kind`, or the detected one when no config
/// resolves. A project without a `devkit.toml` — or with one that fails to
/// load — still gets its answer from detection: the tracker choice must never
/// be what fails a command that would otherwise work.
pub fn configured(config: Option<&str>, start: &str) -> Box<dyn Tracker> {
    let dir = Path::new(start);
    let kind = load::load(config.map(Path::new), dir)
        .ok()
        .and_then(|l| l.config.tracker.kind);
    devkit_common::tracker::resolve(kind, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::tracker::TrackerKind;

    fn write_config(path: &Path, kind: &str) {
        std::fs::write(
            path,
            format!(
                "[defaults]\n\
                 worktree_root = \"wts\"\n\
                 branch_prefix = \"lev/\"\n\
                 baseline_ref = \"origin/main\"\n\
                 baseline_path = \"baseline\"\n\
                 \n\
                 [tracker]\n\
                 kind = \"{kind}\"\n"
            ),
        )
        .unwrap();
    }

    /// Two configs, one directory: the kind follows whichever config was
    /// passed. Detection sees the same directory and the same environment both
    /// times, so only the config can account for the difference. An explicit
    /// path is the sole config layer, so neither the home config nor
    /// `$DEVKIT_CONFIG` takes part.
    #[test]
    fn the_configured_kind_wins_over_detection() {
        let dir = std::env::temp_dir().join(format!("devkit-tracker-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let start = dir.to_str().unwrap();

        for (named, kind) in [("linear", TrackerKind::Linear), ("none", TrackerKind::None)] {
            let path = dir.join(format!("{named}.toml"));
            write_config(&path, named);
            assert_eq!(
                configured(path.to_str(), start).kind(),
                kind,
                "config naming {named}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
