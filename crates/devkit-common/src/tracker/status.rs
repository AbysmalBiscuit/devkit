//! Moving an issue's tracker status, as `[issue.events]` configures.
//!
//! Kept apart from the read-only [`Tracker`](super::Tracker): the triage
//! facade and the MCP server hold trackers and never write to one.

use anyhow::Result;
use devkit_config::EventTransition;

/// Reads and writes one tracker's issue status.
#[ambassador::delegatable_trait]
pub trait StatusWriter {
    /// The issue's current status name, `None` when it has none.
    fn status(&self, id: &str) -> Result<Option<String>>;
    /// Move the issue to the status named `to`. An unknown name is an error
    /// that lists the names the tracker has.
    fn set_status(&self, id: &str, to: &str) -> Result<()>;
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// The status an issue at `current` moves to under `t`: `to`, or `None` when
/// the issue is already there or `current` matches no `from` entry. `*`
/// matches any status, the empty string matches no status, and names compare
/// case-insensitively after trimming.
pub fn target<'a>(t: &'a EventTransition, current: Option<&str>) -> Option<&'a str> {
    if current.is_some_and(|c| same(c, &t.to)) {
        return None;
    }
    let current = current.unwrap_or("");
    t.from
        .iter()
        .any(|f| f.trim() == "*" || same(f, current))
        .then_some(t.to.as_str())
}

/// The entry in `names` that `wanted` names, compared like [`target`].
pub fn find_name<'a>(names: impl IntoIterator<Item = &'a str>, wanted: &str) -> Option<&'a str> {
    names.into_iter().find(|n| same(n, wanted))
}

#[cfg(test)]
mod tests {
    use devkit_config::EventTransition;

    use super::*;

    fn t(from: &[&str], to: &str) -> EventTransition {
        EventTransition {
            from: from.iter().map(|s| s.to_string()).collect(),
            to: to.into(),
        }
    }

    #[test]
    fn target_applies_from_and_skips_when_already_there() {
        assert_eq!(
            target(&t(&["*"], "In progress"), Some("Todo")),
            Some("In progress")
        );
        assert_eq!(
            target(&t(&["Todo"], "In progress"), Some("In review")),
            None
        );
        assert_eq!(
            target(&t(&["*"], "In progress"), Some(" in PROGRESS ")),
            None
        );
        assert_eq!(
            target(&t(&["todo "], "In progress"), Some("Todo")),
            Some("In progress")
        );
    }

    #[test]
    fn no_status_matches_the_empty_string_and_star_only() {
        assert_eq!(
            target(&t(&["", "Todo"], "In progress"), None),
            Some("In progress")
        );
        assert_eq!(target(&t(&["*"], "In progress"), None), Some("In progress"));
        assert_eq!(target(&t(&["Todo"], "In progress"), None), None);
    }

    #[test]
    fn find_name_is_case_and_space_insensitive() {
        assert_eq!(
            find_name(["Todo", "In Progress"], " in progress"),
            Some("In Progress")
        );
        assert_eq!(find_name(["Todo"], "Done"), None);
    }
}
