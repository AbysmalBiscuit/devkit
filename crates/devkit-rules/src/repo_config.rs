//! The settings in `.agents/repo-rules-agent.toml` that change what a query
//! prints.
//!
//! The extractor owns the file and rejects unknown keys. devkit reads only the
//! keys `devkit rules query` uses, so a key the extractor adds later is ignored
//! here rather than breaking the query.

use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::vocab::vocabulary_key;

pub const REPO_CONFIG_PATH: &str = ".agents/repo-rules-agent.toml";

/// The extractor's own `query.limit` default, used when neither the command
/// line nor the repository sets one.
pub const DEFAULT_QUERY_LIMIT: usize = 50;

#[derive(Debug, Default, Deserialize)]
pub struct RepoConfig {
    #[serde(default)]
    pub vocabulary: VocabularyOptions,
    #[serde(default)]
    pub query: QueryOptions,
}

#[derive(Debug, Default, Deserialize)]
pub struct VocabularyOptions {
    /// Topic name to the description the extractor's model sees.
    #[serde(default)]
    pub topics: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct QueryOptions {
    /// Rules a query prints by default. 0 prints all.
    pub limit: Option<usize>,
}

impl RepoConfig {
    /// The repository's topics under the key rules store them by, sorted.
    pub fn topic_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .vocabulary
            .topics
            .keys()
            .map(|k| vocabulary_key(k))
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// The config under `repo_root`, or the defaults when the repository has none.
/// A file that exists and does not parse is an error, as it is for the
/// extractor.
pub fn load(repo_root: &Path) -> Result<RepoConfig> {
    let path = repo_root.join(REPO_CONFIG_PATH);
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RepoConfig::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    toml::from_str(&raw).with_context(|| format!("invalid repo config {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_without_the_file_gets_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = load(dir.path()).unwrap();
        assert!(config.query.limit.is_none());
        assert!(config.topic_names().is_empty());
    }

    #[test]
    fn topics_are_stored_under_their_vocabulary_key() {
        let config: RepoConfig = toml::from_str(
            "[vocabulary.topics]\nKysely = \"query builders\"\n\"Row Level\" = \"rls\"\n",
        )
        .unwrap();
        assert_eq!(config.topic_names(), ["kysely", "row_level"]);
    }

    #[test]
    fn sections_devkit_does_not_read_are_ignored() {
        let config: RepoConfig = toml::from_str(
            "[discovery]\nexclude = [\"docs\"]\n\n[vocabulary]\nlanguages = [\"nix\"]\n\n[query]\nlimit = 20\n",
        )
        .unwrap();
        assert_eq!(config.query.limit, Some(20));
    }
}
