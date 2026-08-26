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
    let (cfg, provenance) = config::resolve(explicit, start, main_checkout.as_deref())?;
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
