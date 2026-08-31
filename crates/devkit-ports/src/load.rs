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

pub fn load(explicit: Option<&Path>, start: &Path) -> Result<Loaded> {
    let main_checkout = devkit_common::git::main_checkout(start).ok().flatten();
    let checkout_root = devkit_common::git::checkout_root(start).ok();
    let (cfg, provenance) = config::resolve(
        explicit,
        start,
        main_checkout.as_deref(),
        checkout_root.as_deref(),
    )?;
    // The one door every subcommand's config passes through, so the shared
    // pool is sized here rather than at each caller.
    devkit_common::pool::configure(cfg.parallelism.threads);
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
    /// `load` is the one door every subcommand's config goes through, so the
    /// pool is configured there rather than at each of the eighteen call
    /// sites, where it could be forgotten. The width is pinned to verify
    /// `load` actually calls `pool::configure` with the configured thread count.
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
