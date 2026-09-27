//! The forge a project gets when it has none, or devkit could not find one.
//! Every read reports why, and every write refuses.

use std::path::Path;

use anyhow::Result;

use super::{
    Forge, ForgeKind, HeadLookup, NewPr, OpenPrPage, PrBrief, PrLocator, PrTimeline, Repo,
    Reviewers, Role, Section,
};

pub struct NoForge {
    reason: String,
}

impl NoForge {
    pub fn new(reason: String) -> NoForge {
        NoForge { reason }
    }

    fn refuse<T>(&self) -> Result<T> {
        anyhow::bail!(
            "this needs a pull-request forge and the project has none ({}). Set [forge] kind \
             (and host, for a self-hosted one) in devkit.toml",
            self.reason
        )
    }
}

impl Forge for NoForge {
    fn kind(&self) -> ForgeKind {
        ForgeKind::None
    }

    fn host(&self) -> &str {
        ""
    }

    fn ready(&self) -> bool {
        false
    }

    fn check(&self) -> Result<String> {
        self.refuse()
    }

    fn locate(&self, _url: &str) -> Option<PrLocator> {
        None
    }

    fn pr(&self, _repo: &Repo, _n: u64) -> Result<Option<PrBrief>> {
        self.refuse()
    }

    fn pr_by_head(&self, _repo: &Repo, _branch: &str) -> HeadLookup {
        HeadLookup::Unavailable(format!("no forge: {}", self.reason))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_write_names_the_key_that_supplies_a_forge() {
        let f = NoForge::new("detected: `origin` is on no forge".into());
        let repo = Repo {
            host: String::new(),
            slug: "o/r".into(),
        };
        let err = f.mark_ready(&repo, 1).unwrap_err().to_string();
        assert!(err.contains("[forge] kind"), "{err}");
        assert!(err.contains("origin"), "{err}");
        assert!(matches!(
            f.pr_by_head(&repo, "b"),
            HeadLookup::Unavailable(r) if r.contains("no forge")
        ));
    }
}
