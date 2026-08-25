//! The tracker seam: one contract over Linear, GitHub Issues, or no tracker.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub use devkit_config::TrackerKind;

#[cfg(any(test, feature = "test-support"))]
pub mod fake;
pub mod github;
pub mod linear;
pub mod none;

/// Where an issue sits in its tracker's workflow. Linear's `state.type`
/// vocabulary, adopted as devkit's own because the status verdict, the triage
/// colours, and the dashboard bands were already written against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateKind {
    Triage,
    Backlog,
    Unstarted,
    Started,
    Completed,
    Canceled,
}

impl StateKind {
    /// Every kind but the two terminal ones. The finished verdict and the
    /// dashboard's "open now" count both key off this.
    pub fn is_open(self) -> bool {
        !matches!(self, StateKind::Completed | StateKind::Canceled)
    }
}

impl std::fmt::Display for StateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StateKind::Triage => "triage",
            StateKind::Backlog => "backlog",
            StateKind::Unstarted => "unstarted",
            StateKind::Started => "started",
            StateKind::Completed => "completed",
            StateKind::Canceled => "canceled",
        })
    }
}

impl std::str::FromStr for StateKind {
    type Err = std::convert::Infallible;

    /// Never fails: a tracker may add a workflow state devkit has no name for,
    /// and one unknown state must not fail an entire status run.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "triage" => StateKind::Triage,
            "backlog" => StateKind::Backlog,
            "started" => StateKind::Started,
            "completed" => StateKind::Completed,
            "canceled" => StateKind::Canceled,
            _ => StateKind::Unstarted,
        })
    }
}

/// One issue's workflow state, as devkit renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub kind: StateKind,
    /// The tracker's own label, e.g. "In Progress" or "Not planned".
    pub name: String,
    /// Hex colour, when the tracker supplies one.
    pub color: Option<String>,
}

/// An issue id parsed from CLI input, plus the title slug when the input
/// carried one (a pasted issue URL usually does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub id: String,
    pub slug: Option<String>,
}

/// A pull request linked to an issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRef {
    pub url: String,
    pub number: u64,
}

/// One issue assigned to the current user, with its state transitions. Drives
/// the dashboard's issues-over-time chart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignedIssue {
    pub identifier: String,
    pub created_at: String,
    pub state: State,
    /// `(when, from, to)` per recorded transition, unsorted.
    pub history: Vec<(String, Option<State>, Option<State>)>,
}

/// Everything the issue summary file renders. Every field is empty rather than
/// absent when the tracker has nothing there.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueDetails {
    pub id: String,
    pub title: String,
    pub url: String,
    pub description: String,
    pub state: String,
    pub assignee: String,
    pub priority: String,
    pub estimate: String,
    pub labels: Vec<String>,
    pub parent: String,
    pub project: String,
}

/// One issue tracker. Every method is read-only: `devkit-issue` is a triage
/// facade and never mutates a tracker.
pub trait Tracker: Send + Sync {
    fn kind(&self) -> TrackerKind;
    /// Configured and able to authenticate. False means callers should degrade
    /// rather than error.
    fn ready(&self) -> bool;
    /// Parse CLI input — a bare id or an issue URL — into an id and, when the
    /// input spelled one out, a title slug. Fails when the input names a
    /// repository or workspace this tracker is not scoped to; the caller
    /// surfaces the error rather than guessing.
    fn issue_ref(&self, input: &str) -> Result<IssueRef>;
    fn title(&self, id: &str) -> Result<Option<String>>;
    fn details(&self, id: &str) -> Result<Option<IssueDetails>>;
    /// Batched: one round trip for every id.
    fn states(&self, ids: &[String]) -> HashMap<String, State>;
    fn issue_pr(&self, id: &str) -> Result<Option<PrRef>>;
    /// Issues that a bare number might refer to, for disambiguation.
    fn candidates(&self, n: u64) -> Result<Vec<IssueRef>>;
    /// PR URL to the issue ids it references.
    fn issues_for_prs(&self, urls: &[String]) -> HashMap<String, Vec<String>>;
    fn assigned_history(&self, on_page: &mut dyn FnMut(usize)) -> Result<Vec<AssignedIssue>>;
    /// Earliest timestamp the dashboard's timeline should start from.
    fn timeline_origin(&self) -> Result<Option<String>>;
    fn issue_url(&self, id: &str) -> Option<String>;
    /// A one-line identity for `devkit doctor`.
    fn check(&self) -> Result<String>;
}

/// A tracker and how devkit arrived at it.
pub struct Resolved {
    pub tracker: Box<dyn Tracker>,
    /// Whether this tracker is the project's own answer rather than a stand-in
    /// devkit fell back to. It is the difference between "this project has no
    /// issue states to wait for" and "devkit found no tracker to ask" — two
    /// situations that both produce a `TrackerKind::None` and must not be
    /// treated alike.
    pub declared: bool,
    /// Why this tracker and not another, phrased for `devkit doctor`.
    pub reason: String,
}

/// The tracker for this project. An explicit `kind` always wins; otherwise a
/// resolvable Linear key, then a GitHub `origin` remote, then no tracker.
///
/// Detection is a floor, not a convenience: a globally exported
/// `LINEAR_API_KEY` resolves to Linear for every project, so a GitHub project on
/// such a machine must set `kind` explicitly. What detection buys is that every
/// config predating `[tracker]` keeps behaving exactly as it did.
pub fn resolve(kind: Option<TrackerKind>, cwd: &Path, repos: &crate::github::Repos) -> Resolved {
    resolve_with_key(kind, cwd, repos, crate::secrets::resolve("LINEAR_API_KEY"))
}

/// `resolve` with the Linear key supplied instead of read from the environment,
/// so detection can be exercised whatever the ambient `LINEAR_API_KEY` holds.
fn resolve_with_key(
    kind: Option<TrackerKind>,
    cwd: &Path,
    repos: &crate::github::Repos,
    key: Option<String>,
) -> Resolved {
    let declared = kind.is_some();
    match kind.unwrap_or_else(|| detect(cwd, key.as_deref())) {
        TrackerKind::Linear => Resolved {
            tracker: Box::new(linear::LinearTracker::new(key)),
            declared,
            reason: if declared {
                "[tracker] kind = \"linear\"".into()
            } else {
                "detected: LINEAR_API_KEY resolves".into()
            },
        },
        // The only place a `GithubTracker` is built, and the issues repository
        // it is handed has already resolved here. Its `ready` reports on the
        // token alone because of that; a second construction site that skipped
        // this check would have it claim readiness with no repository to ask.
        TrackerKind::Github => match repos.issues() {
            Ok(repo) => Resolved {
                tracker: Box::new(github::GithubTracker::new(repo.clone())),
                declared,
                reason: if declared {
                    "[tracker] kind = \"github\"".into()
                } else {
                    "detected: github.com `origin` remote".into()
                },
            },
            // No issues repository resolves, so there is nothing to ask. This
            // is devkit finding no answer, not the project declaring none.
            Err(e) => Resolved {
                tracker: Box::new(none::NoneTracker),
                declared: false,
                reason: format!("github selected but no issues repository: {e:#}"),
            },
        },
        TrackerKind::None => Resolved {
            tracker: Box::new(none::NoneTracker),
            declared,
            reason: if declared {
                "[tracker] kind = \"none\"".into()
            } else {
                "detected: no LINEAR_API_KEY and no GitHub origin remote".into()
            },
        },
    }
}

/// Detection order, used only when `[tracker] kind` is absent.
fn detect(cwd: &Path, linear_key: Option<&str>) -> TrackerKind {
    if linear_key.is_some() {
        return TrackerKind::Linear;
    }
    if crate::github::github_origin_slug(&cwd.to_string_lossy()).is_ok() {
        return TrackerKind::Github;
    }
    TrackerKind::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(issues: Option<&str>, prs: Option<&str>) -> devkit_config::GithubConfig {
        devkit_config::GithubConfig {
            issues_repo: issues.map(String::from),
            pr_repo: prs.map(String::from),
        }
    }

    /// `Repos` where neither key resolves: no `[github]` config and no origin.
    fn no_repos() -> crate::github::Repos {
        crate::github::Repos::from_parts(&cfg(None, None), None, None)
    }

    /// `Repos` whose keys both default to one origin slug — the shape a project
    /// with a github.com `origin` and no `[github]` config resolves to.
    fn repos_with(slug: &str) -> crate::github::Repos {
        crate::github::Repos::from_parts(&cfg(None, None), Some(slug.to_string()), None)
    }

    /// `detect` against a scratch repository whose `origin` is `url`.
    fn detect_with_remote(url: &str, linear_key: Option<&str>) -> TrackerKind {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().to_str().unwrap();
        crate::cmd::git(&["init", "-q"], at).unwrap();
        crate::cmd::git(&["remote", "add", "origin", url], at).unwrap();
        detect(dir.path(), linear_key)
    }

    #[test]
    fn state_kind_round_trips_through_its_wire_string() {
        for (k, s) in [
            (StateKind::Triage, "triage"),
            (StateKind::Backlog, "backlog"),
            (StateKind::Unstarted, "unstarted"),
            (StateKind::Started, "started"),
            (StateKind::Completed, "completed"),
            (StateKind::Canceled, "canceled"),
        ] {
            assert_eq!(k.to_string(), s);
            assert_eq!(s.parse::<StateKind>().unwrap(), k);
            assert_eq!(serde_json::to_value(k).unwrap(), serde_json::json!(s));
        }
    }

    #[test]
    fn an_unknown_state_string_parses_as_unstarted() {
        // A tracker may add a state devkit does not know. Treat it as open
        // rather than failing the whole status run.
        assert_eq!(
            "something-new".parse::<StateKind>().unwrap(),
            StateKind::Unstarted
        );
    }

    #[test]
    fn only_completed_and_canceled_are_closed() {
        assert!(StateKind::Triage.is_open());
        assert!(StateKind::Backlog.is_open());
        assert!(StateKind::Unstarted.is_open());
        assert!(StateKind::Started.is_open());
        assert!(!StateKind::Completed.is_open());
        assert!(!StateKind::Canceled.is_open());
    }

    #[test]
    fn state_kind_deserializes_from_its_wire_string() {
        for (s, k) in [
            ("triage", StateKind::Triage),
            ("backlog", StateKind::Backlog),
            ("unstarted", StateKind::Unstarted),
            ("started", StateKind::Started),
            ("completed", StateKind::Completed),
            ("canceled", StateKind::Canceled),
        ] {
            let value = serde_json::json!(s);
            assert_eq!(serde_json::from_value::<StateKind>(value).unwrap(), k);
        }
    }

    #[test]
    fn the_none_tracker_answers_empty_and_is_never_ready() {
        let t = resolve(Some(TrackerKind::None), Path::new("/nowhere"), &no_repos()).tracker;
        assert_eq!(t.kind(), TrackerKind::None);
        assert!(!t.ready());
        assert!(t.states(&["ENG-1".into()]).is_empty());
        assert!(t.title("ENG-1").unwrap().is_none());
        assert!(t.details("ENG-1").unwrap().is_none());
        assert!(t.issue_url("ENG-1").is_none());
        assert!(t.candidates(7).unwrap().is_empty());
        assert!(t.assigned_history(&mut |_| {}).unwrap().is_empty());
    }

    #[test]
    fn the_none_tracker_passes_an_id_through_unchanged() {
        let t = resolve(Some(TrackerKind::None), Path::new("/nowhere"), &no_repos()).tracker;
        let r = t.issue_ref("  eng-1  ").unwrap();
        assert_eq!(r.id, "eng-1");
        assert_eq!(r.slug, None);
    }

    /// Detection with nothing to go on — no Linear key, and a directory with
    /// no git remote — lands on `None`, which must not read as the project
    /// declaring it has no tracker. The key is passed in rather than read from
    /// the environment, so an ambient `LINEAR_API_KEY` cannot decide this.
    #[test]
    fn a_tracker_detection_fell_back_to_is_not_declared() {
        let r = resolve_with_key(
            None,
            Path::new("/nonexistent-devkit-tracker-probe"),
            &no_repos(),
            None,
        );
        assert_eq!(r.tracker.kind(), TrackerKind::None);
        assert!(!r.declared);
    }

    /// A github.com `origin` on a machine with no Linear key is the detection
    /// path most projects land on. It builds the real adapter, but `declared`
    /// stays false: devkit chose this tracker, the project did not name it.
    #[test]
    fn a_detected_github_origin_resolves_the_adapter_undeclared() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().to_str().unwrap();
        crate::cmd::git(&["init", "-q"], at).unwrap();
        crate::cmd::git(
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widget.git",
            ],
            at,
        )
        .unwrap();

        let r = resolve_with_key(None, dir.path(), &repos_with("acme/widget"), None);
        assert_eq!(r.tracker.kind(), TrackerKind::Github);
        assert!(!r.declared, "{}", r.reason);
        assert!(r.reason.contains("detected"), "{}", r.reason);
    }

    /// A github kind with no repository to ask degrades to the stand-in rather
    /// than to an adapter that would report readiness it cannot back. It is not
    /// declared: devkit found no answer here, the project did not give one.
    #[test]
    fn a_github_kind_without_an_issues_repository_falls_back_undeclared() {
        let r = resolve(Some(TrackerKind::Github), Path::new("."), &no_repos());
        assert_eq!(r.tracker.kind(), TrackerKind::None);
        assert!(!r.declared);
        assert!(r.reason.contains("no issues repository"), "{}", r.reason);
    }

    /// A project that names GitHub gets the GitHub adapter, and it is the
    /// project's own answer: `declared` is what keeps the issue-state gate and
    /// the "set `[tracker] kind`" hints off a project that already decided.
    #[test]
    fn a_declared_github_kind_builds_the_real_adapter() {
        let repos = repos_with("me/widget");
        let r = resolve(Some(TrackerKind::Github), Path::new("."), &repos);
        assert_eq!(r.tracker.kind(), TrackerKind::Github);
        assert!(r.declared);
        assert!(r.reason.contains("kind = \"github\""), "{}", r.reason);
    }

    /// A project with both keys configured has everything the adapter needs;
    /// gating readiness on an origin remote would leave every state gate closed
    /// for a project whose repositories live elsewhere. `ready` needs a token
    /// too, so on a machine without one the assertion is that nothing *else*
    /// closed the gate.
    #[test]
    fn a_configured_project_is_ready_without_a_github_origin() {
        let repos = crate::github::Repos::from_parts(
            &cfg(Some("org/planning"), Some("up/app")),
            None,
            None,
        );
        let r = resolve(Some(TrackerKind::Github), Path::new("."), &repos);
        assert_eq!(r.tracker.kind(), TrackerKind::Github);
        assert!(r.tracker.ready() || crate::github::token().is_none());
    }

    /// `slug_from_remote_url` parses any `host/owner/repo` shape, so detection
    /// has to check the host: a GitLab origin read as GitHub would point every
    /// tracker call at an unrelated github.com repository.
    #[test]
    fn a_gitlab_origin_no_longer_detects_as_github() {
        assert_eq!(
            detect_with_remote("https://gitlab.com/o/r.git", None),
            TrackerKind::None
        );
        assert_eq!(
            detect_with_remote("https://github.com/o/r.git", None),
            TrackerKind::Github
        );
    }

    #[test]
    fn a_named_none_is_declared() {
        let r = resolve(
            Some(TrackerKind::None),
            Path::new("/nonexistent-devkit-tracker-probe"),
            &no_repos(),
        );
        assert_eq!(r.tracker.kind(), TrackerKind::None);
        assert!(r.declared);
    }

    /// One directory, two explicit kinds, two different trackers: only the
    /// explicit argument can account for the difference, so detection cannot be
    /// deciding it. Nothing here reads the environment.
    #[test]
    fn the_same_directory_yields_whichever_kind_is_named() {
        let dir = Path::new("/nonexistent-devkit-tracker-probe");
        assert_eq!(
            resolve(Some(TrackerKind::Linear), dir, &no_repos())
                .tracker
                .kind(),
            TrackerKind::Linear
        );
        assert_eq!(
            resolve(Some(TrackerKind::None), dir, &no_repos())
                .tracker
                .kind(),
            TrackerKind::None
        );
    }

    #[test]
    fn issue_ref_can_refuse() {
        // An issue URL naming a repository the tracker is not scoped to must
        // fail rather than resolve to an id the caller would go on to use.
        let t = fake::FakeTracker::new().refusing("https://github.com/other/repo/issues/9");
        assert!(
            t.issue_ref("https://github.com/other/repo/issues/9")
                .is_err()
        );
    }
}
