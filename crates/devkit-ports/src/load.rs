use crate::{
    apps::{self, App},
    config::{self, Config, Provenance},
    doppler,
};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

pub struct Loaded {
    pub config: Config,
    pub catalog: HashMap<String, App>,
    pub provenance: Provenance,
}

pub fn load(explicit: Option<&Path>, start: &Path) -> Result<Loaded> {
    let (cfg, provenance) = config::resolve(explicit, start)?;
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
