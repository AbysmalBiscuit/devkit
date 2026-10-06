//! Where rules are read from and edited. `[rules] source` picks the member of
//! [`Source`], and every read and edit goes through [`RuleSource`].

use std::path::Path;

use ambassador::Delegate;
use anyhow::Result;
use devkit_common::vcs::Checkout;
use devkit_config::{RulesConfig, RulesPostgresConfig, RulesSource};

use crate::{
    edit::Fields,
    index::FileSource,
    model::RuleIndex,
    postgres::{Database, PostgresSource},
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
            let line = format!("{e:#}").replace(['\r', '\n'], " ");
            let _ =
                std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("devkit: {line}\n"));
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
}

/// Every rule source devkit can read.
#[derive(Delegate)]
#[delegate(RuleSource)]
pub enum Source {
    File(FileSource),
    Postgres(PostgresSource),
}

impl Source {
    /// The source `settings` names for `checkout`'s repository. `database`
    /// opens the database `[rules.postgres]` names, and is called only for
    /// the `postgres` source.
    pub fn for_checkout(
        settings: &RulesConfig,
        checkout: &Checkout,
        database: impl FnOnce(&RulesPostgresConfig) -> Database,
    ) -> Source {
        match settings.source {
            RulesSource::File => Source::File(FileSource::configured(
                settings.index.as_deref(),
                repo_of(checkout),
            )),
            RulesSource::Postgres => Source::Postgres(PostgresSource::new(
                database(&settings.postgres),
                settings.postgres.repository.as_deref(),
                repo_of(checkout),
            )),
        }
    }
}

/// The repository rules describe: the main worktree, so every worktree shares
/// one set.
pub fn repo_of(checkout: &Checkout) -> &Path {
    checkout
        .main_worktree()
        .or_else(|| checkout.root())
        .unwrap_or_else(|| checkout.dir())
}
