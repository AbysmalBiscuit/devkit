//! Where rules are read from and edited. `[rules] source` picks the member of
//! [`Source`], and every read and edit goes through [`RuleSource`].
#![expect(
    dead_code,
    reason = "ambassador's Delegate derive emits a private helper trait that `Source` never uses"
)]

use std::path::Path;

use ambassador::Delegate;
use anyhow::Result;
use devkit_common::vcs::Checkout;
use devkit_config::{RulesConfig, RulesSource};

use crate::{edit::Fields, index::FileSource, model::RuleIndex};

/// A store of rules.
#[ambassador::delegatable_trait]
pub trait RuleSource {
    /// The live rules, or `None` when there are none to read. A store that
    /// exists and cannot be read says so on stderr before answering `None`.
    fn load(&self) -> Option<RuleIndex>;
    /// Add a rule and return its id. `repo` names the repository when the
    /// store is new.
    fn add(&self, repo: &str, fields: Fields) -> Result<String>;
    /// Change the rule `id` and pin it.
    fn edit(&self, id: &str, fields: Fields) -> Result<()>;
    /// Remove the rule `id`.
    fn remove(&self, id: &str) -> Result<()>;
    /// Where the rules live, for messages and `devkit doctor`.
    fn location(&self) -> String;
}

/// Every rule source devkit can read.
#[derive(Delegate)]
#[delegate(RuleSource)]
pub enum Source {
    File(FileSource),
}

impl Source {
    /// The source `settings` names for `checkout`'s repository.
    pub fn for_checkout(settings: &RulesConfig, checkout: &Checkout) -> Source {
        match settings.source {
            RulesSource::File => Source::File(FileSource::configured(
                settings.index.as_deref(),
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
