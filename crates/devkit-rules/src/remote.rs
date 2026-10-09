//! The rule sources that live off this machine, which devkit reads through
//! a [`crate::cache::RuleCache`].

use ambassador::Delegate;
use anyhow::Result;

use crate::{
    edit::Fields,
    model::RuleIndex,
    postgres::PostgresSource,
    source::{RuleSource, ambassador_impl_RuleSource},
    supabase::SupabaseSource,
};

/// A rule source off this machine, which reports the revision its rules are
/// at, so a cache can tell when it is stale.
#[ambassador::delegatable_trait]
pub trait RemoteRules: RuleSource {
    /// The revision the remote's rules are at now; every edit changes it.
    fn revision(&self) -> Result<i64>;
    /// The live rules and the revision they were read at.
    fn pull(&self) -> Result<(i64, RuleIndex)>;
    /// The repository's UUID, lowercased, when the config names a valid
    /// one.
    fn repository_id(&self) -> Option<&str>;
    /// The local checkout the rules describe, which [`RuleIndex::repo`]
    /// reports so a query reads that checkout's `.repo-rules` config.
    fn checkout(&self) -> &str;
    /// Where the rules live, with no credential in it, which a cache keeps
    /// to tell one remote from another.
    fn identity(&self) -> String;
}

/// Every remote rule source.
#[derive(Delegate)]
#[delegate(RuleSource)]
#[delegate(RemoteRules)]
pub enum Remote {
    /// `repo-rules-agent`'s shared Postgres store.
    Postgres(PostgresSource),
    /// The same store over a Supabase project's Data API.
    Supabase(SupabaseSource),
    /// An in-memory remote for tests.
    #[cfg(feature = "test-remote")]
    Fake(FakeRemote),
}

/// The repository UUID `id` names, lowercased, or why the `section` that
/// sets it names none.
pub(crate) fn parse_repository(section: &str, id: Option<&str>) -> Result<String, String> {
    match id.map(str::trim) {
        None | Some("") => Err(format!("{section} repository is not set")),
        Some(id) if is_uuid(id) => Ok(id.to_ascii_lowercase()),
        Some(id) => Err(format!("{section} repository {id:?} is not a UUID")),
    }
}

/// Whether `id` spells a UUID in its hyphenated form.
fn is_uuid(id: &str) -> bool {
    const HYPHENS: [usize; 4] = [8, 13, 18, 23];
    id.len() == 36
        && id.char_indices().all(|(i, c)| match HYPHENS.contains(&i) {
            true => c == '-',
            false => c.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_must_be_a_uuid() {
        assert!(is_uuid("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a10"));
        assert!(is_uuid("0B6F6C1E-8F0E-4A43-9D55-3C0D2B1F9A10"));
        assert!(!is_uuid("0b6f6c1e8f0e4a439d553c0d2b1f9a10"));
        assert!(!is_uuid("0b6f6c1e-8f0e-4a43-9d55-3c0d2b1f9a1g"));
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

    /// A remote holding `index` at `revision`, describing the clone at
    /// `checkout`. Setting `fail` fails every read; `pulls` counts the
    /// pulls. An edit renames a rule and bumps the revision.
    pub struct FakeRemote {
        pub checkout: String,
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
            let mut index = self.index.borrow().clone();
            index.repo = self.checkout.clone();
            Ok((self.revision.get(), index))
        }

        fn repository_id(&self) -> Option<&str> {
            None
        }

        fn checkout(&self) -> &str {
            &self.checkout
        }

        fn identity(&self) -> String {
            "fake".to_string()
        }
    }
}
