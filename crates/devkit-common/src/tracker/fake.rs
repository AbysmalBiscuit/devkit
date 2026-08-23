//! A tracker that answers from a fixed map. Lets `devkit-issue` exercise the
//! whole gather path — discovery, state attachment, finished verdict — with no
//! network and no credentials.

use super::{AssignedIssue, IssueDetails, IssueRef, PrRef, State, Tracker, TrackerKind};
use anyhow::Result;
use std::collections::HashMap;

pub struct FakeTracker {
    pub states: HashMap<String, State>,
    pub ready: bool,
}

impl FakeTracker {
    /// A ready tracker knowing exactly these issue states.
    pub fn ready<const N: usize>(rows: [(&str, State); N]) -> Self {
        Self {
            states: rows.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            ready: true,
        }
    }
}

impl Tracker for FakeTracker {
    fn kind(&self) -> TrackerKind {
        TrackerKind::Linear
    }
    fn ready(&self) -> bool {
        self.ready
    }
    fn issue_ref(&self, input: &str) -> IssueRef {
        IssueRef {
            id: input.trim().to_uppercase(),
            slug: None,
        }
    }
    fn title(&self, id: &str) -> Result<Option<String>> {
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
    fn candidates(&self, _n: u64) -> Result<Vec<IssueRef>> {
        Ok(Vec::new())
    }
    fn issues_for_prs(&self, _urls: &[String]) -> HashMap<String, Vec<String>> {
        HashMap::new()
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
