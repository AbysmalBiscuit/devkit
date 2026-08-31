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
}

/// The config plus everything a `devrun` command needs alongside it: the
/// doppler project map and the app catalog built from it. A command needing
/// only the config resolves through `devkit_common::config` directly, which
/// this wraps.
pub fn load(explicit: Option<&Path>, start: &Path) -> Result<Loaded> {
    let (cfg, provenance) = devkit_common::config::resolve(explicit, start)?;
    let yaml_path = config::expand_tilde(&cfg.defaults.doppler_yaml);
    let p2p = match std::fs::read_to_string(&yaml_path) {
        Ok(y) => doppler::path_to_project(&y)?,
        Err(_) => HashMap::new(), // doppler.yaml optional; apps then need explicit path/project
    };
    let catalog = apps::catalog(&cfg, &p2p)?;
    Ok(Loaded {
        config: cfg,
        catalog,
        provenance,
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
