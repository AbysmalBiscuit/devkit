//! Which tracker and GitHub repositories this project's commands talk to.

use devkit_common::github::Repos;
use devkit_common::tracker::Resolved;
use devkit_ports::load;
use std::path::Path;

/// Everything one config load yields for an `issue` command: the tracker and
/// repositories `select` returns, plus the config itself and how its load went.
/// `issue end` needs the last two — its preserve entries live in the config, and
/// acting on an empty table because the config is broken would remove a worktree
/// having archived nothing.
pub struct Selected {
    pub tracker: Resolved,
    pub repos: Repos,
    pub config: Option<devkit_config::Config>,
    pub health: devkit_config::Health,
}

/// `select` with the config load's result attached. `health` is classified
/// separately from `load`, because `load` also builds the doppler map and app
/// catalog: a broken `doppler.yaml` is not a broken config, and must not make
/// `issue end` refuse.
pub fn select_full(config: Option<&str>, start: &str, pr_override: Option<&str>) -> Selected {
    let dir = Path::new(start);
    let main = devkit_common::git::main_checkout(dir).ok().flatten();
    let health = devkit_config::health(dir, main.as_deref());
    let cfg = load::load(config.map(Path::new), dir)
        .ok()
        .map(|l| l.config);
    let (kind, github) = match &cfg {
        Some(c) => (c.tracker.kind, c.github.clone()),
        None => (None, devkit_config::GithubConfig::default()),
    };
    let repos = Repos::resolve(&github, start, pr_override);
    let tracker = devkit_common::tracker::resolve(kind, dir, &repos);
    Selected {
        tracker,
        repos,
        config: cfg,
        health,
    }
}

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
    let sel = select_full(config, start, pr_override);
    (sel.tracker, sel.repos)
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
        let dir = tempfile::tempdir().unwrap();
        let start = dir.path().to_str().unwrap();

        for (named, kind) in [("linear", TrackerKind::Linear), ("none", TrackerKind::None)] {
            let path = dir.path().join(format!("{named}.toml"));
            write_config(&path, named);
            assert_eq!(
                select(path.to_str(), start, None).0.tracker.kind(),
                kind,
                "config naming {named}"
            );
        }
    }

    /// A config that does not parse must be distinguishable from no config at all:
    /// `issue end` refuses on the first and proceeds on the second.
    #[test]
    fn a_broken_config_reports_broken_and_yields_no_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("devkit.toml"), "[defaults\n").unwrap();

        let sel = select_full(None, dir.path().to_str().unwrap(), None);

        assert!(matches!(sel.health, devkit_config::Health::Broken(_)));
        assert!(sel.config.is_none());
    }

    /// The loaded config comes back so `issue end` can read its preserve table
    /// without a second load.
    #[test]
    fn a_valid_config_comes_back_with_its_preserve_table() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[config]\n\
             root = true\n\
             [defaults]\n\
             worktree_root = \"wts\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n\
             baseline_path = \"base\"\n\
             doppler_yaml = \"doppler.yaml\"\n\
             \n\
             [preserve.notes]\n\
             from = [\"notes/*.md\"]\n\
             to = \"/archive\"\n",
        )
        .unwrap();

        let sel = select_full(None, dir.path().to_str().unwrap(), None);

        let cfg = sel.config.expect("config loaded");
        assert_eq!(cfg.preserve["notes"].to, "/archive");
    }
}
