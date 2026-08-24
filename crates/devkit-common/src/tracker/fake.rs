//! A tracker that answers from a fixed map. Lets `devkit-issue` exercise the
//! whole gather path — discovery, state attachment, finished verdict — with no
//! network and no credentials.

use super::{AssignedIssue, IssueDetails, IssueRef, PrRef, State, Tracker, TrackerKind};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

pub struct FakeTracker {
    pub states: HashMap<String, State>,
    pub ready: bool,
    /// What `kind()` reports. Set it to a kind that ambient resolution could
    /// never pair with this `ready` flag to prove a report came from an
    /// injected tracker rather than a resolved one.
    pub kind: TrackerKind,
    /// Exact inputs `issue_ref` refuses, simulating a URL naming a repository
    /// or workspace this tracker is not scoped to.
    refuse: HashSet<String>,
    titles: HashMap<String, String>,
    by_number: HashMap<u64, Vec<String>>,
    links: HashMap<String, Vec<String>>,
}

impl FakeTracker {
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
            ready: true,
            kind: TrackerKind::Linear,
            refuse: HashSet::new(),
            titles: HashMap::new(),
            by_number: HashMap::new(),
            links: HashMap::new(),
        }
    }

    /// A ready Linear-kind tracker knowing exactly these issue states.
    pub fn with_states<const N: usize>(rows: [(&str, State); N]) -> Self {
        Self {
            states: rows.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            ..Self::new()
        }
    }

    /// Make `issue_ref(input)` fail, as if `input` named a repository or
    /// workspace this tracker is not scoped to.
    pub fn refusing(mut self, input: &str) -> Self {
        self.refuse.insert(input.to_string());
        self
    }

    pub fn with_kind(mut self, kind: TrackerKind) -> Self {
        self.kind = kind;
        self
    }

    /// `candidates(n)` answers with these ids.
    pub fn with_candidates(mut self, n: u64, ids: Vec<&str>) -> Self {
        self.by_number
            .insert(n, ids.into_iter().map(String::from).collect());
        self
    }

    /// `title(id)` answers with this title.
    pub fn with_title(mut self, id: &str, title: &str) -> Self {
        self.titles.insert(id.to_string(), title.to_string());
        self
    }

    /// `issues_for_prs` answers `pr_url` with these issue ids.
    pub fn with_links(mut self, pr_url: &str, ids: Vec<&str>) -> Self {
        self.links.insert(
            pr_url.to_string(),
            ids.into_iter().map(String::from).collect(),
        );
        self
    }
}

impl Default for FakeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker for FakeTracker {
    fn kind(&self) -> TrackerKind {
        self.kind
    }
    fn ready(&self) -> bool {
        self.ready
    }
    fn issue_ref(&self, input: &str) -> Result<IssueRef> {
        let trimmed = input.trim();
        anyhow::ensure!(
            !self.refuse.contains(trimmed),
            "fake tracker refuses {trimmed}"
        );
        Ok(IssueRef {
            id: trimmed.to_uppercase(),
            slug: None,
        })
    }
    fn title(&self, id: &str) -> Result<Option<String>> {
        if let Some(t) = self.titles.get(id) {
            return Ok(Some(t.clone()));
        }
        Ok(self.states.get(id).map(|s| s.name.clone()))
    }
    fn details(&self, _id: &str) -> Result<Option<IssueDetails>> {
        Ok(None)
    }
    fn states(&self, ids: &[String]) -> HashMap<String, State> {
        ids.iter()
            .filter_map(|i| self.states.get(i).map(|s| (i.clone(), s.clone())))
            .collect()
    }
    fn issue_pr(&self, _id: &str) -> Result<Option<PrRef>> {
        Ok(None)
    }
    fn candidates(&self, n: u64) -> Result<Vec<IssueRef>> {
        Ok(self
            .by_number
            .get(&n)
            .map(|ids| {
                ids.iter()
                    .map(|id| IssueRef {
                        id: id.clone(),
                        slug: None,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }
    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>> {
        urls.iter()
            .filter_map(|u| self.links.get(u).map(|ids| (u.clone(), ids.clone())))
            .collect()
    }
    fn assigned_history(&self, _on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>> {
        Ok(Vec::new())
    }
    fn timeline_origin(&self) -> Result<Option<String>> {
        Ok(None)
    }
    fn issue_url(&self, id: &str) -> Option<String> {
        Some(format!("https://example.test/issue/{id}"))
    }
    fn check(&self) -> Result<String> {
        Ok("fake tracker".to_string())
    }
}
