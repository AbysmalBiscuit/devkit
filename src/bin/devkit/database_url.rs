//! A database's connection URL: from the environment, then a Doppler scope,
//! then the secrets file, with the URL Doppler last gave kept so hooks need
//! not ask it again. The todo and rules databases each resolve theirs here.

use std::{path::PathBuf, time::Duration};

use devkit_common::secrets::{self, DopplerScope, Source};
use devkit_config::expand_tilde;
use devkit_todo::activity::segment;
use serde::{Deserialize, Serialize};

/// How [`DatabaseUrl::resolve`] finds the URL when Doppler holds it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlLookup {
    /// Ask Doppler, as a command does.
    Doppler,
    /// Reuse a cached URL younger than [`URL_CACHE_TTL`], else ask Doppler.
    CachedFirst,
    /// Reuse a cached URL however old, and never ask Doppler.
    CachedOnly,
}

/// How long a hook reuses the URL Doppler last gave before asking again.
const URL_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// The Doppler scope a `doppler_project` and `doppler_config` name, `None`
/// without a project.
pub(crate) fn doppler_scope(project: Option<&str>, config: Option<&str>) -> Option<DopplerScope> {
    project.map(|project| DopplerScope {
        project: project.to_string(),
        config: config.map(str::to_string),
    })
}

/// One database's URL: the variable that names it and the directory Doppler's
/// answers for it are kept in.
pub(crate) struct DatabaseUrl {
    pub(crate) var: &'static str,
    pub(crate) cache_dir: PathBuf,
}

/// A URL [`DatabaseUrl::resolve`] found, and where.
pub(crate) struct Resolved {
    pub(crate) url: Option<String>,
    pub(crate) source: Source,
    /// Whether the URL is the cached copy rather than a fresh answer.
    pub(crate) from_cache: bool,
}

/// The database URL Doppler last gave for one scope, kept so hooks skip
/// Doppler.
#[derive(Serialize, Deserialize)]
struct CachedUrl {
    project: String,
    config: Option<String>,
    url: String,
}

impl DatabaseUrl {
    /// The URL for `scope`, from the environment, the cache as `lookup`
    /// allows, Doppler, then the secrets file.
    pub(crate) fn resolve(&self, scope: Option<&DopplerScope>, lookup: UrlLookup) -> Resolved {
        let from_env = std::env::var(self.var).is_ok_and(|url| !url.trim().is_empty());
        let cached = match (scope, lookup) {
            (Some(scope), UrlLookup::CachedFirst) if !from_env => {
                self.cached(scope, Some(URL_CACHE_TTL))
            }
            (Some(scope), UrlLookup::CachedOnly) if !from_env => self.cached(scope, None),
            _ => None,
        };
        if let Some(url) = cached {
            return Resolved {
                url: Some(url),
                source: Source::Doppler,
                from_cache: true,
            };
        }
        let ask = scope.filter(|_| lookup != UrlLookup::CachedOnly);
        let [(url, source)] = secrets::resolve_many(&[self.var], ask);
        Resolved {
            url,
            source,
            from_cache: false,
        }
    }

    /// Where the URL Doppler gave for `scope` is kept: one file per project
    /// and config, so projects on one machine never displace each other's
    /// URL. The file's modification time is the URL's age.
    fn path(&self, scope: &DopplerScope) -> PathBuf {
        let mut name = segment(&scope.project);
        if let Some(config) = &scope.config {
            name.push('+');
            name.push_str(&segment(config));
        }
        self.cache_dir.join(name + ".json")
    }

    /// The cached URL for `scope`, when one is younger than `max_age` or no
    /// `max_age` applies. A copy dated ahead of now, as after the clock was
    /// set back, counts as just written.
    fn cached(&self, scope: &DopplerScope, max_age: Option<Duration>) -> Option<String> {
        let path = self.path(scope);
        if let Some(max_age) = max_age {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            let age = modified.elapsed().unwrap_or(Duration::ZERO);
            if age > max_age {
                return None;
            }
        }
        let cached: CachedUrl = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
        (cached.project == scope.project && cached.config == scope.config).then_some(cached.url)
    }

    /// Keeps `url` for `scope`, readable by its owner alone. Best-effort: a
    /// cache that cannot be written leaves hooks asking Doppler.
    pub(crate) fn remember(&self, scope: &DopplerScope, url: &str) {
        let cached = CachedUrl {
            project: scope.project.clone(),
            config: scope.config.clone(),
            url: url.to_string(),
        };
        let path = self.path(scope);
        let Ok(body) = serde_json::to_vec(&cached) else {
            return;
        };
        let tmp = path.with_extension(format!("json.{}", std::process::id()));
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                std::io::Write::write_all(&mut options.open(&tmp)?, &body)
            })
            .and_then(|()| std::fs::rename(&tmp, &path));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Drops the copy kept for `scope` if it still holds `url`, so a call
    /// that failed with an old URL leaves alone one another call has since
    /// refreshed.
    pub(crate) fn forget(&self, scope: &DopplerScope, url: &str) {
        let path = self.path(scope);
        let held = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<CachedUrl>(&text).ok());
        if held.is_some_and(|held| held.url == url) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// `[<table>.postgres] ca_file` as the global config sets it. A project
/// layer's is ignored: a checkout must not add a CA that a database
/// connection trusts, as it must not move the harness log.
pub(crate) fn global_ca_file(table: &str) -> Option<PathBuf> {
    global_setting(&[table, "postgres", "ca_file"]).map(|ca_file| expand_tilde(&ca_file))
}

/// The string at `path` in the global config, whatever a project layer sets
/// there.
pub(crate) fn global_setting(path: &[&str]) -> Option<String> {
    let body = std::fs::read_to_string(devkit_common::harness::global_config_path()?).ok()?;
    let config: toml::Value = toml::from_str(&body).ok()?;
    let value = path.iter().try_fold(&config, |value, key| value.get(key))?;
    value.as_str().map(str::to_string)
}
