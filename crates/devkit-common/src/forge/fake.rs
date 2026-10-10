//! A forge that answers from a fixed list of pull requests. Lets `workspace
//! end` and the status gather run their PR gate with no network and no
//! credentials.

use std::path::Path;

use anyhow::Result;

use super::{
    Forge, ForgeKind, HeadLookup, NewPr, OpenPrPage, PrBrief, PrLocator, PrTimeline, Repo,
    Reviewers, Role, Section,
};

/// Reports itself as GitHub, so callers take the forge path rather than the
/// no-forge one. Every write refuses.
#[derive(Default)]
pub struct FakeForge {
    prs: Vec<PrBrief>,
}

impl FakeForge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer lookups with this PR, by number and by its head branch.
    pub fn with_pr(mut self, pr: PrBrief) -> Self {
        self.prs.push(pr);
        self
    }

    fn refuse<T>(&self) -> Result<T> {
        anyhow::bail!("the fake forge answers reads only")
    }
}

impl Forge for FakeForge {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Github
    }

    fn host(&self) -> &str {
        "github.com"
    }

    fn ready(&self) -> bool {
        true
    }

    fn check(&self) -> Result<String> {
        Ok("fake".into())
    }

    fn locate(&self, _url: &str) -> Option<PrLocator> {
        None
    }

    fn pr(&self, _repo: &Repo, n: u64) -> Result<Option<PrBrief>> {
        Ok(self.prs.iter().find(|p| p.number == n).cloned())
    }

    fn pr_by_head(&self, _repo: &Repo, branch: &str) -> HeadLookup {
        HeadLookup::of(
            self.prs
                .iter()
                .filter(|p| p.head_ref_name == branch)
                .cloned()
                .collect(),
        )
    }

    fn create(&self, _repo: &Repo, _pr: &NewPr<'_>, _cwd: &Path) -> Result<String> {
        self.refuse()
    }

    fn mark_ready(&self, _repo: &Repo, _n: u64) -> Result<()> {
        self.refuse()
    }

    fn add_reviewers(&self, _repo: &Repo, _n: u64, _logins: &[String]) -> Result<()> {
        self.refuse()
    }

    fn reviewers(&self, _repo: &Repo, _n: u64) -> Result<Reviewers> {
        self.refuse()
    }

    fn head_ref(&self, n: u64) -> Option<String> {
        Some(format!("refs/pull/{n}/head"))
    }

    fn checkout(&self, _repo: &Repo, _pr: &PrBrief, _dir: &Path) -> Result<()> {
        self.refuse()
    }

    fn open_prs(
        &self,
        _repo: &Repo,
        _section: Section,
        _page_size: u32,
        _cursor: Option<&str>,
    ) -> Result<OpenPrPage> {
        self.refuse()
    }

    fn timeline(&self, _repo: &Repo, _role: Role, _max: usize) -> Result<Vec<PrTimeline>> {
        self.refuse()
    }
}
