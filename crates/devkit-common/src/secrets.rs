//! The credential store: `~/.config/devkit/secrets.toml`.
//!
//! Tokens resolve env-first, then this file, so a shell export or a
//! Doppler-injected var always wins and behavior is unchanged when nothing is
//! stored. [`resolve_many`] can also ask Doppler between the two. The file is
//! written `0600` and lives beside `config.toml`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Secrets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linear_workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slack_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devkit_todo_sync_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devkit_todo_sync_client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devkit_todo_sync_secret: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devkit_todo_database_url: Option<String>,
}

impl Secrets {
    fn get(&self, key: &str) -> Option<&str> {
        match key {
            "linear_api_key" => self.linear_api_key.as_deref(),
            "linear_workspace" => self.linear_workspace.as_deref(),
            "slack_token" => self.slack_token.as_deref(),
            "devkit_todo_sync_url" => self.devkit_todo_sync_url.as_deref(),
            "devkit_todo_sync_client_id" => self.devkit_todo_sync_client_id.as_deref(),
            "devkit_todo_sync_secret" => self.devkit_todo_sync_secret.as_deref(),
            "devkit_todo_database_url" => self.devkit_todo_database_url.as_deref(),
            _ => None,
        }
    }

    fn set(&mut self, key: &str, value: String) -> Result<()> {
        let slot = match key {
            "linear_api_key" => &mut self.linear_api_key,
            "linear_workspace" => &mut self.linear_workspace,
            "slack_token" => &mut self.slack_token,
            "devkit_todo_sync_url" => &mut self.devkit_todo_sync_url,
            "devkit_todo_sync_client_id" => &mut self.devkit_todo_sync_client_id,
            "devkit_todo_sync_secret" => &mut self.devkit_todo_sync_secret,
            "devkit_todo_database_url" => &mut self.devkit_todo_database_url,
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
    Doppler,
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

/// The default-path secrets file, parsed once per process. Read paths
/// (`resolve`/`source`) hit this instead of re-reading and re-parsing the file
/// on every lookup — a single command resolves several credentials. Environment
/// variables are still read live per call, so a shell export always wins;
/// `store` writes through `load_from`, so nothing reads a value it just wrote.
fn cached() -> &'static Secrets {
    static CACHE: std::sync::OnceLock<Secrets> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| load().unwrap_or_default())
}

/// Resolve a credential: `$<env_key>` -> `secrets.toml[<lowercased key>]` ->
/// `None`.
pub fn resolve(env_key: &str) -> Option<String> {
    let env_val = std::env::var(env_key).ok();
    let file_val = cached()
        .get(&env_key.to_ascii_lowercase())
        .map(str::to_string);
    pick(env_val, file_val)
}

/// Where `env_key` currently resolves from.
pub fn source(env_key: &str) -> Source {
    let env_val = std::env::var(env_key).ok();
    let file_val = cached()
        .get(&env_key.to_ascii_lowercase())
        .map(str::to_string);
    source_of(env_val, file_val)
}

/// The Doppler project, and optionally config, that [`resolve_many`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DopplerScope {
    pub project: String,
    /// Doppler's own default config when `None`.
    pub config: Option<String>,
}

/// Each name resolved env -> Doppler (only with `doppler`, one call for every
/// name the environment lacks) -> secrets file, in the order given. Values are
/// trimmed, and a value that trims to empty counts as unset. A Doppler failure
/// of any kind falls through to the file without a message, so nothing Doppler
/// prints can reach a log.
pub fn resolve_many<const N: usize>(
    names: &[&str; N],
    doppler: Option<&DopplerScope>,
) -> [(Option<String>, Source); N] {
    resolve_many_with(
        names,
        doppler,
        "doppler",
        |k| std::env::var(k).ok(),
        cached(),
    )
}

/// Doppler's own retries and per-attempt timeout are cut to one 5 s attempt
/// (`--attempts 1 --timeout 5s`); this bound only covers a wedged process.
const DOPPLER_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

fn resolve_many_with<const N: usize>(
    names: &[&str; N],
    doppler: Option<&DopplerScope>,
    program: &str,
    env: impl Fn(&str) -> Option<String>,
    file: &Secrets,
) -> [(Option<String>, Source); N] {
    let trimmed = |v: Option<String>| v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let from_env = names.map(|n| (n, trimmed(env(n))));
    let missing: Vec<&str> = from_env
        .iter()
        .filter(|(_, v)| v.is_none())
        .map(|(n, _)| *n)
        .collect();
    let from_doppler = match doppler {
        Some(scope) if !missing.is_empty() => doppler_get(program, scope, &missing),
        _ => serde_json::Map::new(),
    };
    from_env.map(|(name, env_val)| {
        if env_val.is_some() {
            return (env_val, Source::Env);
        }
        let computed = from_doppler
            .get(name)
            .and_then(|v| v.get("computed"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        if let Some(v) = trimmed(computed) {
            return (Some(v), Source::Doppler);
        }
        match trimmed(file.get(&name.to_ascii_lowercase()).map(str::to_string)) {
            Some(v) => (Some(v), Source::File),
            None => (None, Source::Unset),
        }
    })
}

/// `doppler secrets get <names> --json`, parsed; empty on any failure.
fn doppler_get(
    program: &str,
    scope: &DopplerScope,
    names: &[&str],
) -> serde_json::Map<String, serde_json::Value> {
    let mut args = vec!["secrets", "get"];
    args.extend_from_slice(names);
    args.extend_from_slice(&[
        "--json",
        "--no-exit-on-missing-secret",
        "--attempts",
        "1",
        "--timeout",
        "5s",
        "--project",
        &scope.project,
    ]);
    if let Some(config) = &scope.config {
        args.extend_from_slice(&["--config", config]);
    }
    crate::cmd::capture_bounded(program, &args, DOPPLER_BOUND)
        .and_then(|out| serde_json::from_str(&out).ok())
        .unwrap_or_default()
}

/// Persist one credential to the default path, preserving the others.
pub fn store(key: &str, value: &str) -> Result<()> {
    store_at(&secrets_path(), key, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        (dir, path)
    }

    #[test]
    fn missing_file_is_empty() {
        let (_guard, p) = tmp();
        assert_eq!(load_from(&p).unwrap(), Secrets::default());
    }

    #[test]
    fn store_then_load_round_trips() {
        let (_guard, p) = tmp();
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
        let (_guard, p) = tmp();
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

    #[cfg(unix)]
    mod doppler {
        use super::super::*;

        struct Fake {
            _dir: tempfile::TempDir,
            program: String,
            calls: PathBuf,
        }

        /// A `doppler` stand-in that logs each call's argv and then runs
        /// `body`.
        fn fake(body: &str) -> Fake {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let calls = dir.path().join("calls");
            let program = dir.path().join("doppler");
            std::fs::write(
                &program,
                format!("#!/bin/sh\necho \"$@\" >> '{}'\n{body}\n", calls.display()),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            Fake {
                program: program.to_string_lossy().into_owned(),
                _dir: dir,
                calls,
            }
        }

        fn call_count(f: &Fake) -> usize {
            std::fs::read_to_string(&f.calls)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        }

        fn scope() -> DopplerScope {
            DopplerScope {
                project: "devkit".into(),
                config: Some("dev".into()),
            }
        }

        fn env_with<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
            move |k| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| (*v).to_string())
            }
        }

        #[test]
        fn env_beats_doppler_beats_file() {
            let f = fake(
                r#"echo '{"A":{"computed":"from-doppler-a"},"B":{"computed":"from-doppler-b"},"C":{"computed":null}}'"#,
            );
            let file = Secrets {
                devkit_todo_sync_secret: Some("from-file".into()),
                ..Secrets::default()
            };
            let got = resolve_many_with(
                &["A", "B", "DEVKIT_TODO_SYNC_SECRET"],
                Some(&scope()),
                &f.program,
                env_with(&[("A", "from-env")]),
                &file,
            );
            assert_eq!(got, [
                (Some("from-env".into()), Source::Env),
                (Some("from-doppler-b".into()), Source::Doppler),
                (Some("from-file".into()), Source::File),
            ]);
            let log = std::fs::read_to_string(&f.calls).unwrap();
            assert!(log.contains("--project devkit --config dev"), "{log}");
            assert!(log.contains("--no-exit-on-missing-secret"), "{log}");
        }

        #[test]
        fn doppler_runs_once_for_every_missing_name() {
            let f = fake(r#"echo '{"A":{"computed":"a"},"B":{"computed":"b"}}'"#);
            let got = resolve_many_with(
                &["A", "B"],
                Some(&scope()),
                &f.program,
                env_with(&[]),
                &Secrets::default(),
            );
            assert_eq!(got[0], (Some("a".into()), Source::Doppler));
            assert_eq!(got[1], (Some("b".into()), Source::Doppler));
            assert_eq!(call_count(&f), 1);
        }

        #[test]
        fn doppler_is_not_run_when_the_env_has_every_name() {
            let f = fake("exit 1");
            resolve_many_with(
                &["A"],
                Some(&scope()),
                &f.program,
                env_with(&[("A", "a")]),
                &Secrets::default(),
            );
            assert_eq!(call_count(&f), 0);
        }

        #[test]
        fn no_scope_never_runs_doppler() {
            let f = fake("echo loud >&2; exit 1");
            let got =
                resolve_many_with(&["A"], None, &f.program, env_with(&[]), &Secrets::default());
            assert_eq!(got, [(None, Source::Unset)]);
            assert_eq!(call_count(&f), 0);
        }

        #[test]
        fn a_failing_doppler_falls_through_silently() {
            let f = fake("echo secret-value-xyz >&2; echo secret-value-xyz; exit 1");
            let file = Secrets {
                devkit_todo_sync_url: Some("https://file".into()),
                ..Secrets::default()
            };
            let got = resolve_many_with(
                &["DEVKIT_TODO_SYNC_URL", "DEVKIT_TODO_SYNC_SECRET"],
                Some(&scope()),
                &f.program,
                env_with(&[]),
                &file,
            );
            assert_eq!(got, [
                (Some("https://file".into()), Source::File),
                (None, Source::Unset),
            ]);
            assert!(!format!("{got:?}").contains("secret-value-xyz"));
        }

        #[test]
        fn values_are_trimmed() {
            let f = fake(r#"printf '{"B":{"computed":"  b\\n"}}'"#);
            let file = Secrets {
                devkit_todo_sync_url: Some(" c\n".into()),
                ..Secrets::default()
            };
            let got = resolve_many_with(
                &["A", "B", "DEVKIT_TODO_SYNC_URL", "D"],
                Some(&scope()),
                &f.program,
                env_with(&[("A", "  abc\n"), ("D", " \n")]),
                &file,
            );
            assert_eq!(got, [
                (Some("abc".into()), Source::Env),
                (Some("b".into()), Source::Doppler),
                (Some("c".into()), Source::File),
                (None, Source::Unset),
            ]);
        }
    }

    #[test]
    fn the_todo_sync_keys_round_trip_through_the_file() {
        let (_guard, p) = tmp();
        for key in [
            "devkit_todo_sync_url",
            "devkit_todo_sync_client_id",
            "devkit_todo_sync_secret",
        ] {
            store_at(&p, key, &format!("v-{key}")).unwrap();
        }
        let s = load_from(&p).unwrap();
        for key in [
            "devkit_todo_sync_url",
            "devkit_todo_sync_client_id",
            "devkit_todo_sync_secret",
        ] {
            assert_eq!(s.get(key), Some(format!("v-{key}").as_str()));
        }
    }

    #[test]
    fn unknown_key_rejected() {
        assert!(Secrets::default().set("nope", "x".into()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn stored_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_guard, p) = tmp();
        let _ = std::fs::remove_file(&p);
        store_at(&p, "slack_token", "xoxb").unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(&p);
    }
}
