use crate::{
    apps::{self, App},
    doppler,
};
use anyhow::Result;
use devkit_config::{self as config, Config, Provenance};
use std::collections::HashMap;
use std::path::Path;

pub struct Loaded {
    pub config: Config,
    pub catalog: HashMap<String, App>,
    pub provenance: Provenance,
    /// Apps the config named that the catalog could not resolve a path for.
    pub skipped: Vec<String>,
}

/// The config plus everything a `devrun` command needs alongside it: the
/// doppler project map and the app catalog built from it. A command needing
/// only the config resolves through `devkit_common::config` directly, which
/// this wraps.
///
/// Reports unresolvable apps on stderr. Use [`load_quiet`] when the config
/// being read is not the one the caller is working in.
pub fn load(explicit: Option<&Path>, start: &Path) -> Result<Loaded> {
    let loaded = load_quiet(explicit, start)?;
    for name in &loaded.skipped {
        eprintln!(
            "note: skipping app `{name}` — no path in config and none inferrable from doppler.yaml"
        );
    }
    Ok(loaded)
}

/// [`load`] without the stderr report. Reading another worktree's config is
/// not grounds for printing that config's gaps on this terminal, where they
/// read as faults in the project the caller is actually in.
pub fn load_quiet(explicit: Option<&Path>, start: &Path) -> Result<Loaded> {
    let (cfg, provenance) = devkit_common::config::resolve(explicit, start)?;
    let yaml_path = config::expand_tilde(&cfg.defaults.doppler_yaml);
    let p2p = match std::fs::read_to_string(&yaml_path) {
        Ok(y) => doppler::path_to_project(&y)?,
        Err(_) => HashMap::new(), // doppler.yaml optional; apps then need explicit path/project
    };
    let (catalog, skipped) = apps::catalog(&cfg, &p2p)?;
    Ok(Loaded {
        config: cfg,
        catalog,
        provenance,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    /// `load` resolves through `devkit_common::config`, the one door that
    /// sizes the shared pool. The width is pinned rather than the call read,
    /// so a `load` that stopped going through that door fails here.
    #[test]
    fn load_configures_the_shared_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("devkit.toml"),
            "[config]\nroot = true\n[parallelism]\nthreads = 3\n",
        )
        .unwrap();

        super::load(None, &project).unwrap();

        // DEVKIT_THREADS outranks the config, so the expectation follows the
        // same precedence `width` does. Without the `configure` call the width
        // falls back to its default and this fails.
        let expected = std::env::var("DEVKIT_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(3);
        assert_eq!(devkit_common::pool::width(), expected);
    }
}
