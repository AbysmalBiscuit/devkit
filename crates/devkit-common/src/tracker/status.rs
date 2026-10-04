//! Moving an issue's tracker status, as `[issue.events]` configures.
//!
//! Kept apart from the read-only [`Tracker`](super::Tracker): the triage
//! facade and the MCP server hold trackers and never write to one.

use anyhow::{Context, Result, bail};
use devkit_config::{EventTransition, GithubConfig, TrackerKind};

use super::{github_status::GithubWriter, linear_status::LinearWriter};
use crate::forge::Repos;

/// Reads and writes one tracker's issue status.
#[ambassador::delegatable_trait]
pub trait StatusWriter {
    /// The issue's current status name, `None` when it has none.
    fn status(&self, id: &str) -> Result<Option<String>>;
    /// Move the issue to the status named `to`. An unknown name is an error
    /// that lists the names the tracker has.
    fn set_status(&self, id: &str, to: &str) -> Result<()>;
}

/// The status writer for the resolved tracker.
#[derive(ambassador::Delegate)]
#[delegate(StatusWriter)]
pub enum Writer {
    Github(crate::tracker::github_status::GithubWriter),
    Linear(crate::tracker::linear_status::LinearWriter),
}

/// Whether two status names are the same: case-insensitive, after trimming.
pub fn same_status(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// The writer for a tracker of `kind`, or an error naming the key that would
/// supply one. Under Linear the `[github]` keys play no part.
pub fn writer_for(kind: TrackerKind, github: &GithubConfig, repos: &Repos) -> Result<Writer> {
    writer_with_key(kind, github, repos, || {
        crate::secrets::resolve("LINEAR_API_KEY")
    })
}

fn writer_with_key(
    kind: TrackerKind,
    github: &GithubConfig,
    repos: &Repos,
    linear_key: impl FnOnce() -> Option<String>,
) -> Result<Writer> {
    match kind {
        TrackerKind::Linear => {
            let key = linear_key()
                .context("no LINEAR_API_KEY in the environment or ~/.config/devkit/secrets.toml")?;
            Ok(Writer::Linear(LinearWriter::new(key)))
        }
        TrackerKind::Github => {
            let project = github.project.clone().context(
                "set [github] project to the Projects v2 project that holds the issues' status",
            )?;
            Ok(Writer::Github(GithubWriter::new(
                repos.issues()?.clone(),
                project,
                github.status_field().to_string(),
            )))
        }
        TrackerKind::None => {
            bail!("no tracker resolves to hold the status; set [tracker] kind")
        }
    }
}

/// What a transition does to an issue at some status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// Move the issue to this status.
    To(&'a str),
    /// The issue is already at `to`, whatever `from` says.
    Already,
    /// The issue's status matches no `from` entry.
    NotFrom,
}

/// What `t` does to an issue at `current`. `*` matches any status, the empty
/// string matches no status, and names compare case-insensitively after
/// trimming.
pub fn target<'a>(t: &'a EventTransition, current: Option<&str>) -> Target<'a> {
    if current.is_some_and(|c| same_status(c, &t.to)) {
        return Target::Already;
    }
    let current = current.unwrap_or("");
    if t.from
        .iter()
        .any(|f| f.trim() == "*" || same_status(f, current))
    {
        Target::To(&t.to)
    } else {
        Target::NotFrom
    }
}

/// The id paired with the name `wanted` names in `(id, name)` pairs,
/// compared like [`target`].
pub fn find_id<'a>(pairs: &'a [(String, String)], wanted: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(_, n)| same_status(n, wanted))
        .map(|(id, _)| id.as_str())
}

/// The names in `(id, name)` pairs, comma-separated, for an error that lists
/// what a tracker has.
pub fn names(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(_, n)| n.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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
            Target::To("In progress")
        );
        assert_eq!(
            target(&t(&["Todo"], "In progress"), Some("In review")),
            Target::NotFrom
        );
        assert_eq!(
            target(&t(&["*"], "In progress"), Some(" in PROGRESS ")),
            Target::Already
        );
        assert_eq!(
            target(&t(&["todo "], "In progress"), Some("Todo")),
            Target::To("In progress")
        );
    }

    #[test]
    fn no_status_matches_the_empty_string_and_star_only() {
        assert_eq!(
            target(&t(&["", "Todo"], "In progress"), None),
            Target::To("In progress")
        );
        assert_eq!(
            target(&t(&["*"], "In progress"), None),
            Target::To("In progress")
        );
        assert_eq!(target(&t(&["Todo"], "In progress"), None), Target::NotFrom);
    }

    #[test]
    fn find_id_is_case_and_space_insensitive() {
        let pairs = |names: &[&str]| -> Vec<(String, String)> {
            names
                .iter()
                .map(|n| (format!("id-{n}"), n.to_string()))
                .collect()
        };
        let board = pairs(&["Todo", "In Progress"]);
        assert_eq!(find_id(&board, " in progress"), Some("id-In Progress"));
        assert_eq!(find_id(&board, "Done"), None);
        assert_eq!(find_id(&pairs(&["ГОТОВО"]), "готово"), Some("id-ГОТОВО"));
        assert_eq!(names(&board), "Todo, In Progress");
    }

    fn github_with_project() -> GithubConfig {
        GithubConfig {
            issues_repo: Some("o/r".into()),
            project: Some(devkit_config::ProjectRef {
                owner: None,
                number: 3,
            }),
            ..Default::default()
        }
    }

    fn repos(github: &GithubConfig) -> Repos {
        Repos::from_parts(github, &Default::default(), None, None)
    }

    #[test]
    fn linear_ignores_the_github_keys() {
        let github = github_with_project();
        let w = writer_with_key(TrackerKind::Linear, &github, &repos(&github), || {
            Some("lin_key".into())
        });
        assert!(matches!(w, Ok(Writer::Linear(_))));
    }

    #[test]
    fn each_missing_writer_names_its_key() {
        let err = |kind, github: GithubConfig| {
            writer_with_key(kind, &github, &repos(&github), || None)
                .err()
                .expect("no writer")
                .to_string()
        };
        assert!(err(TrackerKind::Github, GithubConfig::default()).contains("[github] project"));
        assert!(err(TrackerKind::Linear, github_with_project()).contains("LINEAR_API_KEY"));
        assert!(err(TrackerKind::None, github_with_project()).contains("[tracker] kind"));
        let github = github_with_project();
        let w = writer_with_key(TrackerKind::Github, &github, &repos(&github), || None);
        assert!(matches!(w, Ok(Writer::Github(_))));
    }
}
