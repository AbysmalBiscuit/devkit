//! Which tracker and forge a project's `ticket`, `workspace` and `pr` commands
//! talk to, from one config load.

use std::path::Path;

use super::Resolved;
use crate::forge;

/// Everything one config load yields for a `ticket`, `workspace` or `pr`
/// command: the tracker, the forge and its repositories, the config itself and
/// how its load went. `workspace end` needs the last two, since its preserve
/// entries live in the config, and acting on an empty table because the config
/// is broken would remove a worktree having archived nothing.
pub struct Selected {
    pub tracker: Resolved,
    pub forge: forge::Resolved,
    pub config: Option<devkit_config::Config>,
    pub health: devkit_config::Health,
}

/// Resolve `config` (or the layers found from `start`) and the `origin` remote
/// into a tracker and forge. A missing or broken config falls back to
/// detection, so the tracker choice never fails a command. `pr_override` is
/// `pr list --repo`.
///
/// Only `devkit.toml` is loaded, not `devkit_ports::load`, so an unparseable
/// `doppler.yaml` cannot discard the declared tracker.
pub fn select(config: Option<&Path>, start: &str, pr_override: Option<&str>) -> Selected {
    let dir = Path::new(start);
    let resolved = crate::config::resolve(config, dir);
    let health = devkit_config::Health::of(&resolved);
    let cfg = resolved.ok().map(|(c, _)| c);
    let (kind, forge_cfg, github) = match &cfg {
        Some(c) => (c.tracker.kind, c.forge.clone(), c.github.clone()),
        None => (None, Default::default(), Default::default()),
    };
    let forge = forge::resolve(&forge_cfg, &github, start, pr_override);
    let tracker = super::resolve(kind, dir, &forge.repos);
    Selected {
        tracker,
        forge,
        config: cfg,
        health,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::TrackerKind;

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
                select(Some(&path), start, None).tracker.tracker.kind(),
                kind,
                "config naming {named}"
            );
        }
    }

    /// A config that does not parse must be distinguishable from no config at
    /// all: `workspace end` refuses on the first and proceeds on the second.
    #[test]
    fn a_broken_config_reports_broken_and_yields_no_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("devkit.toml"), "[defaults\n").unwrap();

        let sel = select(None, dir.path().to_str().unwrap(), None);

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

        let sel = select(Some(&explicit), dir.path().to_str().unwrap(), None);

        assert!(
            matches!(sel.health, devkit_config::Health::Broken(_)),
            "{:?}",
            sel.health
        );
        assert!(sel.config.is_none());
    }

    /// The loaded config comes back so `workspace end` can read its preserve
    /// table without a second load.
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

        let sel = select(None, dir.path().to_str().unwrap(), None);

        let cfg = sel.config.expect("config loaded");
        assert_eq!(cfg.preserve["notes"].to, "/archive");
    }

    /// A `doppler.yaml` devkit cannot parse says nothing about the config, and
    /// must not cost `workspace end` its preserve table. Doppler's
    /// single-project form writes `setup` as a mapping where the app
    /// catalog expects a list.
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

        let sel = select(None, dir.path().to_str().unwrap(), None);

        assert_eq!(sel.health, devkit_config::Health::Ok);
        let cfg = sel.config.expect("config loaded");
        assert_eq!(cfg.preserve["notes"].to, "/archive");
    }
}
