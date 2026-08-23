//! The tracker for a project that has none. Every answer is empty —
//! expressed once here instead of as a `has_key` branch at each caller.

use super::{AssignedIssue, IssueDetails, IssueRef, PrRef, State, Tracker, TrackerKind};
use anyhow::Result;
use std::collections::HashMap;

pub struct NoneTracker;

impl Tracker for NoneTracker {
    fn kind(&self) -> TrackerKind {
        TrackerKind::None
    }
    fn ready(&self) -> bool {
        false
    }
    fn issue_ref(&self, input: &str) -> IssueRef {
        IssueRef {
            id: input.trim().to_string(),
            slug: None,
        }
    }
    fn title(&self, _id: &str) -> Result<Option<String>> {
        Ok(None)
    }
    fn details(&self, _id: &str) -> Result<Option<IssueDetails>> {
        Ok(None)
    }
    fn states(&self, _ids: &[String]) -> HashMap<String, State> {
        HashMap::new()
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
    fn issue_url(&self, _id: &str) -> Option<String> {
        None
    }
    fn check(&self) -> Result<String> {
        Ok("no tracker configured".to_string())
    }
}
