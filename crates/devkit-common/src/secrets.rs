//! The credential store: `~/.config/devkit/secrets.toml`.
//!
//! Tokens resolve env-first, then this file, so a shell export or a
//! Doppler-injected var always wins and behavior is unchanged when nothing is
//! stored. The file is written `0600` and lives beside `config.toml`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Secrets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear_workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slack_token: Option<String>,
}

impl Secrets {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            "linear_api_key" => self.linear_api_key.as_deref(),
            "linear_workspace" => self.linear_workspace.as_deref(),
            "slack_token" => self.slack_token.as_deref(),
            _ => None,
        }
    }

    fn set(&mut self, key: &str, value: String) -> Result<()> {
        let slot = match key {
            "linear_api_key" => &mut self.linear_api_key,
            "linear_workspace" => &mut self.linear_workspace,
            "slack_token" => &mut self.slack_token,
            other => anyhow::bail!("unknown secret key: {other}"),
        };
        *slot = Some(value);
        Ok(())
    }
}

/// Where a credential was resolved from.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    Env,
    File,
    Unset,
}

/// `$HOME/.config/devkit/secrets.toml` — beside `config.toml`, which is also
/// HOME-based (not `XDG_CONFIG_HOME`-based) so the two stay co-located.
pub fn secrets_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_default();
    home.join(".config/devkit/secrets.toml")
}

fn nonempty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

/// env wins over file; empty strings count as unset.
fn pick(env_val: Option<String>, file_val: Option<String>) -> Option<String> {
    nonempty(env_val).or_else(|| nonempty(file_val))
}

fn source_of(env_val: Option<String>, file_val: Option<String>) -> Source {
    if nonempty(env_val).is_some() {
        Source::Env
    } else if nonempty(file_val).is_some() {
        Source::File
    } else {
        Source::Unset
    }
}

fn load_from(path: &Path) -> Result<Secrets> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Secrets::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_to(path: &Path, s: &Secrets) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let body = toml::to_string_pretty(s).context("serializing secrets")?;
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    chmod_600(path)
}

#[cfg(unix)]
fn chmod_600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", path.display()))
}

#[cfg(not(unix))]
fn chmod_600(_path: &Path) -> Result<()> {
    Ok(())
}

/// Persist one credential at `path`, preserving the others. Creates the parent
/// dir and the file `0600`. Public for path-injected tests.
pub fn store_at(path: &Path, key: &str, value: &str) -> Result<()> {
    let mut s = load_from(path)?;
    s.set(key, value.to_string())?;
    write_to(path, &s)
}

/// Parse the secrets file; a missing file is an empty `Secrets`.
pub fn load() -> Result<Secrets> {
    load_from(&secrets_path())
}

/// Resolve a credential: `$<env_key>` → `secrets.toml[<lowercased key>]` → `None`.
pub fn resolve(env_key: &str) -> Option<String> {
    let env_val = std::env::var(env_key).ok();
    let file_val = load()
        .ok()
        .and_then(|s| s.get(&env_key.to_ascii_lowercase()).map(str::to_string));
    pick(env_val, file_val)
}

/// Where `env_key` currently resolves from.
pub fn source(env_key: &str) -> Source {
    let env_val = std::env::var(env_key).ok();
    let file_val = load()
        .ok()
        .and_then(|s| s.get(&env_key.to_ascii_lowercase()).map(str::to_string));
    source_of(env_val, file_val)
}

/// Persist one credential to the default path, preserving the others.
pub fn store(key: &str, value: &str) -> Result<()> {
    store_at(&secrets_path(), key, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("devkit-secrets-{}-{name}", std::process::id()))
            .join("secrets.toml")
    }

    #[test]
    fn missing_file_is_empty() {
        assert_eq!(load_from(&tmp("missing")).unwrap(), Secrets::default());
    }

    #[test]
    fn store_then_load_round_trips() {
        let p = tmp("round");
        let _ = std::fs::remove_file(&p);
        store_at(&p, "linear_api_key", "lin_123").unwrap();
        store_at(&p, "linear_workspace", "adaptyv").unwrap();
        let s = load_from(&p).unwrap();
        assert_eq!(s.linear_api_key.as_deref(), Some("lin_123"));
        assert_eq!(s.linear_workspace.as_deref(), Some("adaptyv"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn store_preserves_siblings() {
        let p = tmp("siblings");
        let _ = std::fs::remove_file(&p);
        store_at(&p, "linear_api_key", "k").unwrap();
        store_at(&p, "slack_token", "xoxb").unwrap();
        let s = load_from(&p).unwrap();
        assert_eq!(s.linear_api_key.as_deref(), Some("k"));
        assert_eq!(s.slack_token.as_deref(), Some("xoxb"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn env_wins_over_file() {
        assert_eq!(pick(Some("e".into()), Some("f".into())), Some("e".into()));
        assert_eq!(pick(None, Some("f".into())), Some("f".into()));
        assert_eq!(
            pick(Some(String::new()), Some("f".into())),
            Some("f".into())
        );
        assert_eq!(pick(None, None), None);
    }

    #[test]
    fn source_reflects_precedence() {
        assert_eq!(source_of(Some("e".into()), Some("f".into())), Source::Env);
        assert_eq!(
            source_of(Some(String::new()), Some("f".into())),
            Source::File
        );
        assert_eq!(source_of(None, None), Source::Unset);
    }

    #[test]
    fn unknown_key_rejected() {
        assert!(Secrets::default().set("nope", "x".into()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stored_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let p = tmp("perms");
        let _ = std::fs::remove_file(&p);
        store_at(&p, "slack_token", "xoxb").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(&p);
    }
}
