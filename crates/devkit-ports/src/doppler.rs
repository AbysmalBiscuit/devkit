use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct DopplerFile { setup: Vec<Entry> }
#[derive(Deserialize)]
struct Entry { project: String, path: String }

/// Map repo-relative app path (e.g. "apps/api") → doppler project.
pub fn path_to_project(yaml: &str) -> Result<HashMap<String, String>> {
    let f: DopplerFile = serde_yaml_ng::from_str(yaml)?;
    Ok(f.setup.into_iter().map(|e| (e.path, e.project)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_path_to_project() {
        let y = "setup:\n  - project: api-foundry\n    config: dev_local\n    path: apps/api\n";
        let m = path_to_project(y).unwrap();
        assert_eq!(m.get("apps/api").unwrap(), "api-foundry");
    }
}
