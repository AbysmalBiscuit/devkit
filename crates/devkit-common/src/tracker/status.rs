//! Moving an issue's tracker status, as `[ticket.events]` configures.
//!
//! Kept apart from the read-only [`Tracker`](super::Tracker): the triage
//! facade and the MCP server hold trackers and never write to one.

use anyhow::{Context, Result, bail};
use devkit_config::{EventTransition, GithubConfig, IssueEvent, TrackerKind};
use serde_json::Value;

use super::{github_status::GithubWriter, linear_status::LinearWriter};
use crate::forge::Repos;

/// Moves one tracker's issue status.
#[ambassador::delegatable_trait]
pub trait StatusWriter {
    /// Read the issue's status and move it as `event`'s transition `t`
    /// allows, in one read and at most the writes the move needs. A `to` the
    /// tracker lacks is an error naming `[ticket.events.<event>] to` and
    /// listing the names the tracker has.
    fn move_status(&self, id: &str, event: IssueEvent, t: &EventTransition) -> Result<Outcome>;
}

/// What [`StatusWriter::move_status`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Moved from `from` (`None` for no status) to `to`.
    Moved { from: Option<String>, to: String },
    /// Already at `to`; nothing was written.
    Already(String),
    /// At a status `from` does not list (`None` for no status); nothing was
    /// written.
    NotFrom(Option<String>),
}

/// Apply `event`'s transition `t` to an issue at `current`, calling `write`
/// with the status to move to only when it moves.
pub fn apply(
    event: IssueEvent,
    t: &EventTransition,
    current: Option<String>,
    write: impl FnOnce(&str) -> Result<()>,
) -> Result<Outcome> {
    Ok(match target(t, current.as_deref()) {
        Target::To(to) => {
            write(to).with_context(|| format!("[ticket.events.{event}] to"))?;
            Outcome::Moved {
                from: current,
                to: to.to_string(),
            }
        }
        Target::Already => Outcome::Already(current.unwrap_or_default()),
        Target::NotFrom => Outcome::NotFrom(current),
    })
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

/// The statuses a tracker offers, each an `(id, name)` pair: a GitHub
/// field's options, or a Linear team's workflow states.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Statuses(Vec<(String, String)>);

impl Statuses {
    /// The statuses in a GraphQL list of `{ id name }` nodes, the shape both
    /// trackers report them in. A node missing either field is skipped.
    pub fn from_nodes(nodes: &Value) -> Self {
        nodes
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| {
                Some((
                    n["id"].as_str()?.to_string(),
                    n["name"].as_str()?.to_string(),
                ))
            })
            .collect()
    }

    /// The id of the status `wanted` names, compared like [`target`].
    pub fn id(&self, wanted: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(_, n)| same_status(n, wanted))
            .map(|(id, _)| id.as_str())
    }

    /// The names, comma-separated, for an error that lists what a tracker has.
    pub fn names(&self) -> String {
        self.0
            .iter()
            .map(|(_, n)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl FromIterator<(String, String)> for Statuses {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(pairs: I) -> Self {
        Statuses(pairs.into_iter().collect())
    }
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
    fn status_ids_resolve_case_and_space_insensitively() {
        let statuses = |names: &[&str]| -> Statuses {
            names
                .iter()
                .map(|n| (format!("id-{n}"), n.to_string()))
                .collect()
        };
        let board = statuses(&["Todo", "In Progress"]);
        assert_eq!(board.id(" in progress"), Some("id-In Progress"));
        assert_eq!(board.id("Done"), None);
        assert_eq!(statuses(&["ГОТОВО"]).id("готово"), Some("id-ГОТОВО"));
        assert_eq!(board.names(), "Todo, In Progress");
    }

    #[test]
    fn apply_writes_only_a_move_and_names_the_key_when_it_fails() {
        let t = t(&["Todo"], "In progress");
        let mut wrote = Vec::new();
        let moved = apply(IssueEvent::Start, &t, Some("Todo".into()), |to| {
            wrote.push(to.to_string());
            Ok(())
        });
        assert_eq!(moved.unwrap(), Outcome::Moved {
            from: Some("Todo".into()),
            to: "In progress".into()
        });
        assert_eq!(wrote, ["In progress"]);

        let never = |_: &str| -> Result<()> { panic!("no write expected") };
        assert_eq!(
            apply(IssueEvent::Start, &t, Some("in progress".into()), never).unwrap(),
            Outcome::Already("in progress".into())
        );
        assert_eq!(
            apply(IssueEvent::Start, &t, None, never).unwrap(),
            Outcome::NotFrom(None)
        );

        let failed = apply(IssueEvent::PrOpen, &t, Some("Todo".into()), |_| {
            bail!("no option")
        })
        .unwrap_err();
        assert!(format!("{failed:#}").starts_with("[ticket.events.pr_open] to: no option"));
    }

    #[test]
    fn statuses_parse_from_id_and_name_nodes() {
        let nodes = serde_json::json!([
            { "id": "a", "name": "Todo" },
            { "id": "b" },
            { "id": "c", "name": "Done" },
        ]);
        let parsed = Statuses::from_nodes(&nodes);
        assert_eq!(parsed.names(), "Todo, Done");
        assert_eq!(parsed.id("done"), Some("c"));
        assert_eq!(
            Statuses::from_nodes(&serde_json::Value::Null),
            Statuses::default()
        );
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
