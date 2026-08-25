//! Which tracker and GitHub repositories this project's commands talk to.

use devkit_common::github::Repos;
use devkit_common::tracker::Resolved;
use devkit_ports::load;
use std::path::Path;

/// The tracker this project talks to and the GitHub repositories its commands
/// work against, resolved from `config` (or the layers discovered from `start`)
/// plus the `origin` remote. The two come back together because they come from
/// one config load: resolving the tracker needs the issues repository to build
/// a GitHub adapter.
///
/// A project without a `devkit.toml` — or with one that fails to load — still
/// gets its tracker from detection and its repositories from origin alone: the
/// tracker choice must never be what fails a command that would otherwise work.
/// `pr_override` is `issue prs --repo`.
pub fn select(config: Option<&str>, start: &str, pr_override: Option<&str>) -> (Resolved, Repos) {
    let dir = Path::new(start);
    let cfg = load::load(config.map(Path::new), dir)
        .ok()
        .map(|l| l.config);
    let (kind, github) = match cfg {
        Some(c) => (c.tracker.kind, c.github),
        None => (None, devkit_config::GithubConfig::default()),
    };
    let repos = Repos::resolve(&github, start, pr_override);
    (devkit_common::tracker::resolve(kind, dir, &repos), repos)
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
        let dir = devkit_testtmp::dir("devkit-tracker");
        let start = dir.to_str().unwrap();

        for (named, kind) in [("linear", TrackerKind::Linear), ("none", TrackerKind::None)] {
            let path = dir.join(format!("{named}.toml"));
            write_config(&path, named);
            assert_eq!(
                select(path.to_str(), start, None).0.tracker.kind(),
                kind,
                "config naming {named}"
            );
        }
    }
}
