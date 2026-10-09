//! The rule sources that live off this machine, which devkit reads through
//! a [`crate::cache::RuleCache`].

use ambassador::Delegate;
use anyhow::Result;

use crate::{
    edit::Fields,
    model::RuleIndex,
    postgres::PostgresSource,
    source::{RuleSource, ambassador_impl_RuleSource},
};

/// A rule source off this machine, which reports the revision its rules are
/// at, so a cache can tell when it is stale.
#[ambassador::delegatable_trait]
pub trait RemoteRules: RuleSource {
    /// The revision the remote's rules are at now; every edit changes it.
    fn revision(&self) -> Result<i64>;
    /// The live rules and the revision they were read at.
    fn pull(&self) -> Result<(i64, RuleIndex)>;
}

/// Every remote rule source.
#[derive(Delegate)]
#[delegate(RuleSource)]
#[delegate(RemoteRules)]
pub enum Remote {
    /// `repo-rules-agent`'s shared Postgres store.
    Postgres(PostgresSource),
    /// An in-memory remote for tests.
    #[cfg(feature = "test-remote")]
    Fake(FakeRemote),
}

impl RemoteRules for PostgresSource {
    fn revision(&self) -> Result<i64> {
        PostgresSource::revision(self)
    }

    fn pull(&self) -> Result<(i64, RuleIndex)> {
        PostgresSource::pull(self)
    }
}

#[cfg(feature = "test-remote")]
pub use fake::FakeRemote;

#[cfg(feature = "test-remote")]
mod fake {
    use std::cell::{Cell, RefCell};

    use anyhow::{Result, bail};

    use super::RemoteRules;
    use crate::{edit::Fields, model::RuleIndex, source::RuleSource};

    /// A remote holding `index` at `revision`. Setting `fail` fails every
    /// read; `pulls` counts the pulls. An edit renames a rule and bumps the
    /// revision.
    pub struct FakeRemote {
        pub revision: Cell<i64>,
        pub index: RefCell<RuleIndex>,
        pub fail: Cell<bool>,
        pub pulls: Cell<usize>,
    }

    impl FakeRemote {
        fn check(&self) -> Result<()> {
            if self.fail.get() {
                bail!("the fake remote is down");
            }
            Ok(())
        }
    }

    impl RuleSource for FakeRemote {
        fn read(&self) -> Result<Option<RuleIndex>> {
            self.pull().map(|(_, index)| Some(index))
        }

        fn add(&self, _repo: &str, _fields: Fields) -> Result<String> {
            bail!("the fake remote takes no new rules")
        }

        fn edit(&self, id: &str, fields: Fields) -> Result<()> {
            self.check()?;
            let mut index = self.index.borrow_mut();
            let Some(rule) = index.rules.iter_mut().find(|r| r.id == id) else {
                bail!("no rule {id}");
            };
            if let Some(title) = fields.title {
                rule.title = title;
            }
            self.revision.set(self.revision.get() + 1);
            Ok(())
        }

        fn remove(&self, _id: &str) -> Result<()> {
            bail!("the fake remote removes nothing")
        }

        fn location(&self) -> String {
            "fake remote".to_string()
        }

        fn kind(&self) -> &'static str {
            "fake"
        }
    }

    impl RemoteRules for FakeRemote {
        fn revision(&self) -> Result<i64> {
            self.check()?;
            Ok(self.revision.get())
        }

        fn pull(&self) -> Result<(i64, RuleIndex)> {
            self.check()?;
            self.pulls.set(self.pulls.get() + 1);
            Ok((self.revision.get(), self.index.borrow().clone()))
        }
    }
}
