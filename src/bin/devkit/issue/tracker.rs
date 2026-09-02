//! Which tracker and GitHub repositories this project's commands talk to.

use devkit_common::github::Repos;
use devkit_common::tracker::Resolved;
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

/// `select` with the config itself and a health verdict attached. Both come
/// from one `devkit_common::config::resolve`, so `Health::Ok` alongside a
/// missing config cannot happen — `issue end` reads its preserve table from the
/// same result its gate approved. Resolving the config alone rather than
/// through `devkit_ports::load` is what keeps a `doppler.yaml` devkit cannot
/// parse from reading as a broken config: the tracker and repositories need
/// neither the doppler map nor the app catalog.
pub fn select_full(config: Option<&str>, start: &str, pr_override: Option<&str>) -> Selected {
    let dir = Path::new(start);
    let resolved = devkit_common::config::resolve(config.map(Path::new), dir);
    let health = devkit_config::Health::of(&resolved);
    let cfg = resolved.ok().map(|(c, _)| c);
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

    /// An explicit `--config` that does not parse is a fault, not a project
    /// without a config: the health verdict has to describe the very config the
    /// command will read, not whatever else happens to be discoverable from the
    /// same directory.
    #[test]
    fn a_broken_explicit_config_reports_broken_even_beside_a_valid_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[config]\n\
             root = true\n\
             [defaults]\n\
             worktree_root = \"wts\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n",
        )
        .unwrap();
        let explicit = dir.path().join("explicit.toml");
        std::fs::write(&explicit, "[defaults\n").unwrap();

        let sel = select_full(explicit.to_str(), dir.path().to_str().unwrap(), None);

        assert!(
            matches!(sel.health, devkit_config::Health::Broken(_)),
            "{:?}",
            sel.health
        );
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

    /// A `doppler.yaml` devkit cannot parse says nothing about the config, and
    /// must not cost `issue end` its preserve table. Doppler's single-project
    /// form writes `setup` as a mapping where the app catalog expects a list.
    #[test]
    fn an_unparseable_doppler_yaml_leaves_the_config_intact() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[config]\n\
             root = true\n\
             [defaults]\n\
             worktree_root = \"wts\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n\
             doppler_yaml = \"doppler.yaml\"\n\
             \n\
             [preserve.notes]\n\
             from = [\"notes/*.md\"]\n\
             to = \"/archive\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("doppler.yaml"),
            "setup:\n  project: api\n  config: dev\n  path: apps/api\n",
        )
        .unwrap();

        let sel = select_full(None, dir.path().to_str().unwrap(), None);

        assert_eq!(sel.health, devkit_config::Health::Ok);
        let cfg = sel.config.expect("config loaded");
        assert_eq!(cfg.preserve["notes"].to, "/archive");
    }
}
