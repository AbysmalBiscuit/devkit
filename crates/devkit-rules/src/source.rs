//! Where rules are read from and edited. `[rules] source` picks the member of
//! [`Source`], and every read and edit goes through [`RuleSource`].

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use ambassador::Delegate;
use anyhow::Result;
use devkit_common::vcs::Checkout;
use devkit_config::{RulesConfig, RulesPostgresConfig, RulesSource, RulesSupabaseConfig};
use devkit_supabase::Api;

use crate::{
    cache::{CacheKey, RuleCache, Write},
    edit::Fields,
    index::{FileSource, cache_dir},
    model::RuleIndex,
    postgres::{Database, PostgresSource},
    remote::{Remote, RemoteRules},
    sqlite::{self, SqliteSource},
    supabase::SupabaseSource,
};

/// A store of rules.
#[ambassador::delegatable_trait]
pub trait RuleSource {
    /// The live rules, `None` when there is no store to read, or an error
    /// saying why a store that exists cannot be read.
    fn read(&self) -> Result<Option<RuleIndex>>;
    /// [`RuleSource::read`] for a caller that must not fail: an error is one
    /// line on stderr and reads as no rules.
    fn load(&self) -> Option<RuleIndex> {
        self.read().unwrap_or_else(|e| {
            report(&e);
            None
        })
    }
    /// Add a rule and return its id. `repo` names the repository when the
    /// store is new.
    fn add(&self, repo: &str, fields: Fields) -> Result<String>;
    /// Change the rule `id` and pin it.
    fn edit(&self, id: &str, fields: Fields) -> Result<()>;
    /// Remove the rule `id`.
    fn remove(&self, id: &str) -> Result<()>;
    /// Where the rules live, for messages and `devkit doctor`. Never carries
    /// a credential.
    fn location(&self) -> String;
    /// What kind of store this is, for `devkit doctor`.
    fn kind(&self) -> &'static str;
}

/// Every rule store devkit can read.
#[derive(Delegate)]
#[delegate(RuleSource)]
pub enum Source {
    /// The legacy JSON index.
    Json(FileSource),
    /// The SQLite store the extractor writes by default.
    Sqlite(SqliteSource),
    /// A remote store, such as the shared Postgres store, read through its
    /// local cache.
    Cached(CachedSource),
}

impl Source {
    /// The source `settings` names for `checkout`'s repository. `database`
    /// opens the database `[rules.postgres]` names, called only for the
    /// `postgres` source, and `api` the Data API `[rules.supabase]` names,
    /// called only for the `supabase` source. Their caches live under
    /// `state_dir`.
    pub fn for_checkout(
        settings: &RulesConfig,
        checkout: &Checkout,
        database: impl FnOnce(&RulesPostgresConfig) -> Arc<Database>,
        api: impl FnOnce(&RulesSupabaseConfig) -> Arc<Api>,
        state_dir: &Path,
    ) -> Source {
        let repo = repo_of(checkout);
        match settings.source {
            RulesSource::File => match settings.index.as_deref() {
                Some(path) => Source::at(PathBuf::from(path), repo),
                None => Source::cached(repo),
            },
            RulesSource::Postgres => Source::Cached(CachedSource::at_state_dir(
                state_dir,
                Remote::Postgres(PostgresSource::new(
                    database(&settings.postgres),
                    settings.postgres.repository.as_deref(),
                    repo,
                )),
            )),
            RulesSource::Supabase => Source::Cached(CachedSource::at_state_dir(
                state_dir,
                Remote::Supabase(SupabaseSource::new(
                    api(&settings.supabase),
                    settings.supabase.repository.as_deref(),
                    repo,
                )),
            )),
        }
    }

    /// The store in the file at `path`, whichever kind it is.
    pub fn at(path: PathBuf, repo: &Path) -> Source {
        if sqlite::is_store(&path) {
            Source::Sqlite(SqliteSource::at(path, repo))
        } else {
            Source::Json(FileSource::at(path))
        }
    }

    /// The store the extractor keeps for `repo`: its SQLite store when there
    /// is one, as the extractor's own `cache list` prefers it, else the JSON
    /// index, which devkit leaves to the extractor to create.
    fn cached(repo: &Path) -> Source {
        let dir = cache_dir(repo);
        let store = dir.join("index.sqlite");
        if store.is_file() {
            Source::Sqlite(SqliteSource::at(store, repo))
        } else {
            Source::Json(FileSource::cached(dir.join("index.json")))
        }
    }

    /// Whether there are rules to read, found as cheaply as this source
    /// allows: a file source loads its rules, a cached source finds its
    /// cache or else checks that the remote answers and holds the
    /// repository. A failure is
    /// one line on stderr and reads as no rules, as with [`RuleSource::load`].
    pub fn present(&self) -> bool {
        match self {
            Source::Json(_) | Source::Sqlite(_) => self.load().is_some(),
            Source::Cached(source) => {
                source.cache.meta().is_some()
                    || source.remote.revision().map_err(|e| report(&e)).is_ok()
            }
        }
    }

    /// Refreshes a cached source's cache, as [`CachedSource::refresh`] does;
    /// `None` for a source with no cache.
    pub fn refresh(&self, how: Refresh) -> Option<Result<Refreshed>> {
        match self {
            Source::Json(_) | Source::Sqlite(_) => None,
            Source::Cached(source) => Some(source.refresh(how)),
        }
    }
}

/// When a refresh pulls, and what its pull may replace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    /// Pull only when there is no cache or the remote's revision differs
    /// from the cached one, keeping a cache at a later revision: session
    /// start and the commands that read rules.
    IfChanged,
    /// Always pull, keeping a cache at a later revision: after an edit.
    AfterEdit,
    /// Always pull and replace the cache, whatever revision it holds:
    /// `devkit rules pull`, the recovery for a remote whose revision went
    /// backwards.
    Pull,
}

/// What a refresh found.
#[derive(Clone, Debug)]
pub struct Refreshed {
    /// The remote's revision, which the cache now holds.
    pub revision: i64,
    /// How many rules the cache holds.
    pub rules: usize,
    /// Whether the rules were read afresh, rather than found current.
    pub pulled: bool,
}

/// A remote source read through its local cache. Reads come from the cache
/// alone once it exists, and from the remote, without filling the cache,
/// until then; [`CachedSource::refresh`] fills it. Edits go to the remote
/// and then refresh the cache, so the next read sees them.
pub struct CachedSource {
    cache: RuleCache,
    remote: Remote,
}

impl CachedSource {
    pub fn new(cache: RuleCache, remote: Remote) -> CachedSource {
        CachedSource { cache, remote }
    }

    /// `remote` read through its cache under `state_dir`, keyed by the
    /// remote's kind, repository and identity.
    pub fn at_state_dir(state_dir: &Path, remote: Remote) -> CachedSource {
        let key = CacheKey {
            kind: remote.kind(),
            repository: remote.repository_id().unwrap_or("unset").to_string(),
            source: remote.identity(),
        };
        CachedSource::new(RuleCache::at_state_dir(state_dir, key), remote)
    }

    pub fn cache(&self) -> &RuleCache {
        &self.cache
    }

    pub fn remote(&self) -> &Remote {
        &self.remote
    }

    /// Pulls the remote's rules into the cache as `how` says. A failure
    /// leaves the cache as it was.
    pub fn refresh(&self, how: Refresh) -> Result<Refreshed> {
        let revision = self.remote.revision()?;
        if how == Refresh::IfChanged
            && self.cache.meta().is_some_and(|m| m.revision == revision)
            && let Some(index) = self.cache.read()?
        {
            return Ok(Refreshed {
                revision,
                rules: index.rules.len(),
                pulled: false,
            });
        }
        let (revision, index) = self.remote.pull()?;
        let mode = match how {
            Refresh::Pull => Write::Replace,
            Refresh::IfChanged | Refresh::AfterEdit => Write::KeepNewer,
        };
        self.cache.write(mode, revision, &index)?;
        Ok(Refreshed {
            revision,
            rules: index.rules.len(),
            pulled: true,
        })
    }

    /// Refreshes the cache after an edit, which has already succeeded, so a
    /// failure is one line on stderr.
    fn refresh_after_edit(&self) {
        if let Err(e) = self.refresh(Refresh::AfterEdit) {
            report(&e.context("the edit is made, but refreshing the rules cache failed"));
        }
    }
}

impl RuleSource for CachedSource {
    fn read(&self) -> Result<Option<RuleIndex>> {
        match self.cache.read()? {
            // Every clone of the repository shares the cache, which holds
            // the checkout of whichever pulled last.
            Some(index) => Ok(Some(RuleIndex {
                repo: self.remote.checkout().to_string(),
                ..index
            })),
            None => self.remote.pull().map(|(_, index)| Some(index)),
        }
    }

    fn add(&self, repo: &str, fields: Fields) -> Result<String> {
        let id = self.remote.add(repo, fields)?;
        self.refresh_after_edit();
        Ok(id)
    }

    fn edit(&self, id: &str, fields: Fields) -> Result<()> {
        self.remote.edit(id, fields)?;
        self.refresh_after_edit();
        Ok(())
    }

    fn remove(&self, id: &str) -> Result<()> {
        self.remote.remove(id)?;
        self.refresh_after_edit();
        Ok(())
    }

    fn location(&self) -> String {
        format!(
            "{} (cache {})",
            self.remote.location(),
            self.cache.path().display()
        )
    }

    fn kind(&self) -> &'static str {
        self.remote.kind()
    }
}

/// An error as the one line a caller that must not fail prints.
fn report(e: &anyhow::Error) {
    let line = format!("{e:#}").replace(['\r', '\n'], " ");
    let _ = std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("devkit: {line}\n"));
}

/// The repository rules describe: the main worktree, so every worktree shares
/// one set.
pub fn repo_of(checkout: &Checkout) -> &Path {
    checkout
        .main_worktree()
        .or_else(|| checkout.root())
        .unwrap_or_else(|| checkout.dir())
}
