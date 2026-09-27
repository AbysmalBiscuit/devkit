//! The forge seam: one contract over the hosts that keep a project's pull
//! requests (GitHub, GitLab, Forgejo), or none.
//!
//! GitHub's pull-request vocabulary is devkit's own: a PR's `state` is `OPEN`,
//! `CLOSED` or `MERGED`, and GitLab merge requests are PRs here too. Every
//! backend maps its answers onto these types.

use std::{collections::HashMap, path::Path};

use anyhow::Result;
pub use devkit_config::ForgeKind;
use devkit_config::{ForgeConfig, GithubConfig};
use serde::{Deserialize, Serialize};

use crate::vcs::{Vcs, VersionControl};

pub mod forgejo;
pub mod github;
pub mod gitlab;
pub mod none;
pub mod remote;
pub mod rest;

/// One resolved repository on one forge host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub host: String,
    /// `owner/repo`, or a nested `group/sub/repo` on GitLab.
    pub slug: String,
}

impl Repo {
    /// `host/slug`, the spelling `gh --repo` takes. The host is explicit
    /// because `--repo o/r` leaves `GH_HOST` free to select another host,
    /// which would send a token to a host it was not issued for.
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.host, self.slug)
    }
}

/// A pull request, identified. `repo: None` means the input was a bare number
/// or `#42` and defaults to the PR repository; a URL fills it in and that
/// repository wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrLocator {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub number: u64,
}

impl PrLocator {
    /// The repository this locator names, or the PR repository when it names
    /// none.
    pub fn resolve(&self, repos: &Repos) -> Result<Repo> {
        match &self.repo {
            Some(slug) => overridden_repo(&repos.forge_host, slug),
            None => repos.prs().cloned(),
        }
    }

    /// The repository this locator names, or `default` when it names none,
    /// for a caller that already holds the one repository a bare number would
    /// resolve to and has no `Repos` to ask.
    pub fn resolve_or(&self, default: &Repo) -> Result<Repo> {
        match &self.repo {
            Some(slug) => overridden_repo(&default.host, slug),
            None => Ok(default.clone()),
        }
    }
}

/// A repository named explicitly by a locator, as opposed to one defaulted
/// from `[forge] repo` or an origin remote.
fn overridden_repo(host: &str, slug: &str) -> Result<Repo> {
    remote::validate_slug(slug)?;
    Ok(Repo {
        host: host.to_string(),
        slug: slug.to_string(),
    })
}

/// Parse `scheme://<host>/<repo><marker><n>` into a locator, when the URL is
/// on `host`. Each forge spells the marker its own way: `/pull/` on GitHub,
/// `/-/merge_requests/` on GitLab, `/pulls/` on Forgejo.
pub fn locate_on(url: &str, host: &str, marker: &str) -> Option<PrLocator> {
    let (_, rest) = url.trim().split_once("://")?;
    let (authority, path) = rest.split_once('/')?;
    let url_host = authority.rsplit('@').next()?.split(':').next()?;
    if !remote::same_host(url_host, host) {
        return None;
    }
    let (repo, tail) = path.split_once(marker)?;
    let number = tail.split(['/', '?', '#']).next()?.parse().ok()?;
    remote::validate_slug(repo).ok()?;
    Some(PrLocator {
        repo: Some(repo.to_string()),
        number,
    })
}

/// The PR number in a pull-request URL from any forge devkit knows, whatever
/// its host: for a caller holding a URL from elsewhere, such as a Linear
/// attachment.
pub fn pr_number_from_url(url: &str) -> Option<u64> {
    ["/pull/", "/pulls/", "/merge_requests/"]
        .into_iter()
        .find_map(|marker| {
            let tail = url.split(marker).nth(1)?;
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        })
}

/// Check out the PR head a forge publishes at `remote_ref` on `origin` (such
/// as `refs/pull/7/head`) as local branch `branch` in the worktree at `dir`,
/// for a forge with no CLI that does it. The ref lives on the base repository,
/// so this works for a PR from a fork the local clone has no remote for.
pub fn checkout_ref(dir: &Path, remote_ref: &str, branch: &str) -> Result<()> {
    Vcs::at(dir).checkout_remote_ref(dir, "origin", remote_ref, branch)
}

/// The repositories one command works against, resolved once and threaded to
/// every operation. Each key resolves independently and is required only where
/// it is used, so a Linear project with a fork workflow sets `[forge] repo`
/// alone and is never asked for an `issues_repo` it will not read.
#[derive(Debug, Clone)]
pub struct Repos {
    issues: std::result::Result<Repo, String>,
    prs: std::result::Result<Repo, String>,
    forge_host: String,
    github_host: String,
}

impl Repos {
    /// The GitHub Issues repository, or the error explaining which key would
    /// supply it.
    pub fn issues(&self) -> Result<&Repo> {
        self.issues.as_ref().map_err(|e| anyhow::anyhow!(e.clone()))
    }

    /// The pull-request repository, or the error explaining which key would
    /// supply it.
    pub fn prs(&self) -> Result<&Repo> {
        self.prs.as_ref().map_err(|e| anyhow::anyhow!(e.clone()))
    }

    /// The host the GitHub tracker talks to: the forge's own when the forge
    /// is GitHub, since GitHub Enterprise keeps its issues beside its PRs, and
    /// github.com otherwise.
    pub fn github_host(&self) -> &str {
        &self.github_host
    }

    /// Resolution with the origin slug supplied rather than read, and both
    /// hosts github.com, so resolution is testable without a git remote.
    #[doc(hidden)]
    pub fn from_parts(
        github: &GithubConfig,
        forge: &ForgeConfig,
        origin: Option<String>,
        pr_override: Option<&str>,
    ) -> Repos {
        let hosts = Hosts {
            forge: "github.com".into(),
            github: "github.com".into(),
        };
        let origin: std::result::Result<String, String> =
            origin.ok_or_else(|| "no origin".to_string());
        build(github, forge, pr_override, &hosts, &|_| {
            origin.clone().map_err(|_| String::new())
        })
    }
}

struct Hosts {
    forge: String,
    github: String,
}

/// The one precedence chain resolution runs: config, then override (`prs`
/// only), then the origin default, each key independent. `origin(host)` is
/// the origin slug on `host`, or why there is none; an empty reason means
/// there was nothing worth reporting and the key's own message is used.
fn build(
    github: &GithubConfig,
    forge: &ForgeConfig,
    pr_override: Option<&str>,
    hosts: &Hosts,
    origin: &dyn Fn(&str) -> std::result::Result<String, String>,
) -> Repos {
    let from_origin = |host: &str, missing: String| {
        origin(host).map_err(|why| if why.is_empty() { missing } else { why })
    };
    let issues = match &github.issues_repo {
        Some(slug) => Ok(slug.clone()),
        None => from_origin(
            &hosts.github,
            format!(
                "no GitHub repository for issues: set [github] issues_repo or give the \
                 project an `origin` remote on {}",
                hosts.github
            ),
        ),
    };
    let prs = match (pr_override, forge.repo.as_deref(), &github.pr_repo) {
        (Some(slug), ..) | (None, Some(slug), _) => Ok(slug.to_string()),
        (None, None, Some(legacy)) => Err(format!(
            "`[github] pr_repo` moved to `[forge] repo`: replace it with \
             `[forge]\\nrepo = \"{legacy}\"`"
        )),
        (None, None, None) => from_origin(
            &hosts.forge,
            format!(
                "no repository for pull requests: set [forge] repo or give the project an \
                 `origin` remote on {}",
                hosts.forge
            ),
        ),
    };
    Repos {
        issues: issues.and_then(|slug| checked(&hosts.github, slug)),
        prs: prs.and_then(|slug| checked(&hosts.forge, slug)),
        forge_host: hosts.forge.clone(),
        github_host: hosts.github.clone(),
    }
}

/// Validate a resolved slug at the point of resolution, so a bad value is
/// reported against the key that carries it rather than failing later inside a
/// URL or a cache filename.
fn checked(host: &str, slug: String) -> std::result::Result<Repo, String> {
    match remote::validate_slug(&slug) {
        Ok(()) => Ok(Repo {
            host: host.to_string(),
            slug,
        }),
        Err(e) => Err(format!("{e:#}")),
    }
}

/// A pull request reduced to the fields devkit reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrBrief {
    pub number: u64,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub url: String,
    pub title: String,
    pub head_ref_name: String,
    /// The commit at the PR's head. This is what ties a PR to a worktree: a
    /// branch name does not, since two forks routinely propose the same name.
    pub head_ref_oid: String,
    /// The fork the head branch lives in, when the forge reported one.
    pub head_repo_owner: Option<String>,
    /// Whether the PR is a draft. A draft's `state` is `OPEN`, so this is the
    /// only thing separating "still being written" from "waiting on a
    /// reviewer".
    pub is_draft: bool,
    /// Who opened the PR. `None` when the forge omitted it, or for a deleted
    /// account. The reviewer gate reads this: an author does not review their
    /// own PR.
    pub author_login: Option<String>,
}

/// One pull request from a batched read: `Ok(None)` is a repository or pull
/// request that does not resolve, `Err` one that came back unparseable.
pub type PrLookup = Result<Option<PrBrief>>;

/// What a head-branch lookup found. An `Option` cannot distinguish "the
/// transport answered and there is no such PR" from "the transport failed", and
/// every caller collapsed both into a fallback that guessed.
#[derive(Debug, Clone)]
pub enum HeadLookup {
    Unique(PrBrief),
    NoMatch,
    Ambiguous(Vec<PrBrief>),
    Unavailable(String),
}

impl HeadLookup {
    /// The lookup for a list of every PR whose head is the branch.
    pub fn of(mut found: Vec<PrBrief>) -> HeadLookup {
        match found.len() {
            0 => HeadLookup::NoMatch,
            1 => HeadLookup::Unique(found.remove(0)),
            _ => HeadLookup::Ambiguous(found),
        }
    }
}

/// A pull request to open.
#[derive(Debug, Clone)]
pub struct NewPr<'a> {
    pub base: &'a str,
    /// The branch carrying the commits, already pushed to `origin`.
    pub head: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub draft: bool,
    /// Logins to request as reviewers.
    pub reviewers: &'a [String],
}

/// A PR's reviewers. A forge drops a login from `requested` once they review,
/// so "is anyone reviewing this" needs both lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reviewers {
    /// Logins with a review request pending.
    pub requested: Vec<String>,
    /// Logins that have submitted a review, each once.
    pub submitted: Vec<String>,
}

/// Which of the viewer's pull requests a timeline covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Author,
    Reviewer,
}

/// Open/merge timestamps and line counts of one PR, for timeline charts.
/// A forge that does not report line counts leaves them at zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrTimeline {
    pub created_at: Option<String>,
    pub merged_at: Option<String>,
    pub additions: i64,
    pub deletions: i64,
}

/// One of the three open-PR searches `issue prs` is built from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Mine,
    ReviewRequested,
    ReviewedBy,
}

/// The forge's overall review verdict on a PR, when it keeps one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
}

/// One submitted review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub author: String,
    pub state: ReviewState,
    /// RFC 3339, so lexicographic order is chronological. Empty for a review
    /// still pending.
    pub submitted_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    Running,
    Failed,
}

/// One check on a PR's head commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub name: String,
    pub state: CheckState,
    /// When this attempt began, to pick the latest among re-runs of one name.
    /// RFC 3339.
    pub started_at: Option<String>,
}

/// Every check on a PR's head commit, and the forge's own summary of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checks {
    pub overall: CheckState,
    pub runs: Vec<CheckRun>,
}

/// An open pull request with what `issue prs` needs to say what to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPr {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub head_ref_name: String,
    pub is_draft: bool,
    pub review_decision: Option<ReviewDecision>,
    /// Whether the PR cannot merge without a rebase.
    pub conflicting: bool,
    pub author: String,
    /// `None` when the head commit carries no checks at all.
    pub checks: Option<Checks>,
    pub reviews: Vec<Review>,
    /// Logins with a review request pending.
    pub review_requests: Vec<String>,
}

/// One page of one open-PR search.
#[derive(Debug, Clone, Default)]
pub struct OpenPrPage {
    /// The authenticated user's login.
    pub viewer: String,
    pub prs: Vec<OpenPr>,
    /// Passed back as `cursor` to fetch the next page; `None` on the last.
    pub next: Option<String>,
}

/// One forge. Reads fail soft where the type allows it (`HeadLookup`,
/// `PrLookup`), and every write is an error when it cannot be made.
pub trait Forge: Send + Sync {
    fn kind(&self) -> ForgeKind;
    /// The host this forge answers for. Empty when there is no forge.
    fn host(&self) -> &str;
    /// Whether a credential resolves. False means reads degrade to a CLI
    /// fallback where the forge has one, or fail.
    fn ready(&self) -> bool;
    /// A one-line identity for `devkit doctor`.
    fn check(&self) -> Result<String>;
    /// Parse a pasted PR URL on this forge's host.
    fn locate(&self, url: &str) -> Option<PrLocator>;
    /// PR `n` in `repo`, or `None` when no such PR exists.
    fn pr(&self, repo: &Repo, n: u64) -> Result<Option<PrBrief>>;
    /// Many PRs at once, in `targets` order. `Err` is the whole request
    /// failing.
    fn prs(&self, targets: &[(Repo, u64)]) -> Result<Vec<PrLookup>> {
        Ok(targets.iter().map(|(r, n)| self.pr(r, *n)).collect())
    }
    /// PRs whose head branch is `branch`, in any fork and any state.
    fn pr_by_head(&self, repo: &Repo, branch: &str) -> HeadLookup;
    /// [`Forge::pr_by_head`] for many branches, keyed by branch.
    fn prs_by_head(&self, repo: &Repo, branches: &[String]) -> HashMap<String, HeadLookup> {
        branches
            .iter()
            .map(|b| (b.clone(), self.pr_by_head(repo, b)))
            .collect()
    }
    /// Open a PR from `pr.head`, run in the checkout at `cwd`. Returns its URL.
    fn create(&self, repo: &Repo, pr: &NewPr<'_>, cwd: &Path) -> Result<String>;
    /// Take PR `n` out of draft.
    fn mark_ready(&self, repo: &Repo, n: u64) -> Result<()>;
    /// Request `logins` as reviewers on PR `n`, keeping those already asked.
    fn add_reviewers(&self, repo: &Repo, n: u64, logins: &[String]) -> Result<()>;
    /// Who is tied to PR `n` as a reviewer.
    fn reviewers(&self, repo: &Repo, n: u64) -> Result<Reviewers>;
    /// Check `pr`'s head out as a local branch in the worktree at `dir`.
    fn checkout(&self, repo: &Repo, pr: &PrBrief, dir: &Path) -> Result<()>;
    /// One page of the viewer's open PRs in `section`.
    fn open_prs(
        &self,
        repo: &Repo,
        section: Section,
        page_size: u32,
        cursor: Option<&str>,
    ) -> Result<OpenPrPage>;
    /// Up to `max` of the viewer's PRs in `role`, in any state.
    fn timeline(&self, repo: &Repo, role: Role, max: usize) -> Result<Vec<PrTimeline>>;
}

/// A forge and how devkit arrived at it.
pub struct Resolved {
    pub forge: Box<dyn Forge>,
    /// Whether this forge is the project's own answer rather than detection's.
    /// It separates "this project has no pull requests" from "devkit found no
    /// forge": both are `ForgeKind::None`, and only the first lifts the PR
    /// gate on the finished verdict.
    pub declared: bool,
    /// Why this forge and not another, phrased for `devkit doctor`.
    pub reason: String,
    pub repos: Repos,
}

/// Prefix of every `Resolved::reason` that came from detection rather than
/// from a `[forge] kind` the project set.
pub const DETECTED: &str = "detected: ";

/// The public host of each forge kind, and the one detection recognizes.
fn default_host(kind: ForgeKind) -> &'static str {
    match kind {
        ForgeKind::Github => "github.com",
        ForgeKind::Gitlab => "gitlab.com",
        ForgeKind::Forgejo => "codeberg.org",
        ForgeKind::None => "",
    }
}

/// The forge a public host belongs to.
fn kind_of_host(host: &str) -> Option<ForgeKind> {
    [ForgeKind::Github, ForgeKind::Gitlab, ForgeKind::Forgejo]
        .into_iter()
        .find(|k| remote::same_host(host, default_host(*k)))
}

/// Which forge, on which host, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    kind: ForgeKind,
    host: String,
    declared: bool,
    reason: String,
}

/// Choose the forge. An explicit `kind` wins; otherwise the `origin` host
/// decides, with `reached` mapping the remote to the host ssh would connect
/// to. Pure, so every rule is testable without a remote or an ssh config.
fn choose(
    cfg: &ForgeConfig,
    origin: Option<&str>,
    reached: &dyn Fn(&str) -> Option<String>,
) -> Choice {
    if let Some(kind) = cfg.kind {
        let host = cfg
            .host
            .clone()
            .unwrap_or_else(|| default_host(kind).to_string());
        let reason = match &cfg.host {
            Some(h) => format!("[forge] kind = \"{kind}\", host = \"{h}\""),
            None => format!("[forge] kind = \"{kind}\""),
        };
        return Choice {
            kind,
            host,
            declared: true,
            reason,
        };
    }
    let undetected = |reason: String| Choice {
        kind: ForgeKind::None,
        host: String::new(),
        declared: false,
        reason,
    };
    if let Some(h) = &cfg.host {
        return undetected(format!(
            "[forge] host = \"{h}\" names no kind; set [forge] kind as well"
        ));
    }
    let Some(url) = origin else {
        return undetected(format!(
            "{DETECTED}no `origin` remote; set [forge] kind to name this project's forge"
        ));
    };
    let literal = remote::remote_host(url).and_then(kind_of_host);
    let found = literal
        .map(|k| (k, default_host(k).to_string()))
        .or_else(|| {
            let host = reached(url)?;
            kind_of_host(&host).map(|k| (k, default_host(k).to_string()))
        });
    match found {
        Some((kind, host)) => Choice {
            kind,
            reason: format!("{DETECTED}`origin` is on {host}"),
            host,
            declared: false,
        },
        None => undetected(format!(
            "{DETECTED}`origin` ({}) is on no forge devkit recognizes; set [forge] kind and \
             host for a self-hosted one",
            url.trim()
        )),
    }
}

/// The forge for this project, with the repositories its commands work
/// against. `pr_override` is `issue prs --repo`, one invocation's override of
/// `[forge] repo`.
pub fn resolve(
    cfg: &ForgeConfig,
    github: &GithubConfig,
    cwd: &str,
    pr_override: Option<&str>,
) -> Resolved {
    // Only detection reads `origin` here, so a project that names its forge
    // and repositories never runs git to resolve them.
    let origin = cfg
        .kind
        .is_none()
        .then(|| remote::origin_url(cwd).ok())
        .flatten();
    let choice = choose(cfg, origin.as_deref(), &remote::remote_reached_host);
    let hosts = Hosts {
        github: if choice.kind == ForgeKind::Github {
            choice.host.clone()
        } else {
            default_host(ForgeKind::Github).to_string()
        },
        forge: choice.host.clone(),
    };
    let no_forge = format!("no forge: {}", choice.reason);
    // The origin lookup runs only for a key config left open, so a project
    // whose repositories are all configured never pays for it or fails on it.
    let origin_slug = |host: &str| -> std::result::Result<String, String> {
        if host.is_empty() {
            return Err(no_forge.clone());
        }
        remote::origin_slug(cwd, host).map_err(|e| format!("{e:#}"))
    };
    let repos = build(github, cfg, pr_override, &hosts, &origin_slug);
    let forge: Box<dyn Forge> = match choice.kind {
        ForgeKind::Github => Box::new(github::GithubForge::new(&choice.host)),
        ForgeKind::Gitlab => Box::new(gitlab::GitlabForge::new(&choice.host)),
        ForgeKind::Forgejo => Box::new(forgejo::ForgejoForge::new(&choice.host)),
        ForgeKind::None => Box::new(none::NoForge::new(choice.reason.clone())),
    };
    Resolved {
        forge,
        declared: choice.declared,
        reason: choice.reason,
        repos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(origin: Option<&str>, reached: Option<&str>) -> Choice {
        let reached = reached.map(str::to_string);
        choose(&ForgeConfig::default(), origin, &|_| reached.clone())
    }

    #[test]
    fn a_public_origin_host_detects_its_forge() {
        for (url, kind, host) in [
            ("git@github.com:o/r.git", ForgeKind::Github, "github.com"),
            (
                "https://www.github.com/o/r",
                ForgeKind::Github,
                "github.com",
            ),
            (
                "https://gitlab.com/g/s/r.git",
                ForgeKind::Gitlab,
                "gitlab.com",
            ),
            (
                "git@codeberg.org:o/r.git",
                ForgeKind::Forgejo,
                "codeberg.org",
            ),
        ] {
            let c = detect(Some(url), None);
            assert_eq!((c.kind, c.host.as_str()), (kind, host), "{url}");
            assert!(!c.declared);
            assert!(c.reason.starts_with(DETECTED), "{}", c.reason);
        }
    }

    #[test]
    fn an_ssh_alias_detects_the_forge_its_config_names() {
        let c = detect(Some("work:o/r.git"), Some("gitlab.com"));
        assert_eq!(c.kind, ForgeKind::Gitlab);
    }

    /// A self-hosted instance could be any forge, so its host alone decides
    /// nothing. Detection reports it and holds the PR gate closed.
    #[test]
    fn an_unknown_host_detects_no_forge_undeclared() {
        let c = detect(Some("git@git.acme.test:o/r.git"), None);
        assert_eq!(c.kind, ForgeKind::None);
        assert!(!c.declared);
        assert!(c.reason.contains("[forge] kind"), "{}", c.reason);

        let c = detect(None, None);
        assert_eq!(c.kind, ForgeKind::None);
        assert!(c.reason.contains("no `origin`"), "{}", c.reason);
    }

    #[test]
    fn a_declared_kind_takes_its_public_host_or_the_configured_one() {
        let cfg = ForgeConfig {
            kind: Some(ForgeKind::Github),
            host: Some("ghe.acme.test".into()),
            repo: None,
        };
        let c = choose(&cfg, Some("git@gitlab.com:o/r.git"), &|_| None);
        assert_eq!(c.kind, ForgeKind::Github);
        assert_eq!(c.host, "ghe.acme.test");
        assert!(c.declared);

        let cfg = ForgeConfig {
            kind: Some(ForgeKind::Forgejo),
            ..ForgeConfig::default()
        };
        assert_eq!(choose(&cfg, None, &|_| None).host, "codeberg.org");
    }

    #[test]
    fn a_declared_none_is_declared() {
        let cfg = ForgeConfig {
            kind: Some(ForgeKind::None),
            ..ForgeConfig::default()
        };
        let c = choose(&cfg, Some("git@github.com:o/r.git"), &|_| None);
        assert_eq!(c.kind, ForgeKind::None);
        assert!(c.declared);
    }

    #[test]
    fn a_host_without_a_kind_is_not_guessed() {
        let cfg = ForgeConfig {
            host: Some("git.acme.test".into()),
            ..ForgeConfig::default()
        };
        let c = choose(&cfg, Some("git@git.acme.test:o/r.git"), &|_| None);
        assert_eq!(c.kind, ForgeKind::None);
        assert!(!c.declared);
        assert!(c.reason.contains("kind"), "{}", c.reason);
    }

    fn github(issues: Option<&str>, legacy_prs: Option<&str>) -> GithubConfig {
        GithubConfig {
            issues_repo: issues.map(str::to_string),
            pr_repo: legacy_prs.map(str::to_string),
        }
    }

    fn forge(repo: Option<&str>) -> ForgeConfig {
        ForgeConfig {
            repo: repo.map(str::to_string),
            ..ForgeConfig::default()
        }
    }

    #[test]
    fn repos_resolve_each_key_independently() {
        // Both configured: no origin is consulted at all, so a project whose
        // code lives elsewhere still resolves.
        let r = Repos::from_parts(
            &github(Some("org/planning"), None),
            &forge(Some("up/app")),
            None,
            None,
        );
        assert_eq!(r.issues().unwrap().slug, "org/planning");
        assert_eq!(r.prs().unwrap().slug, "up/app");

        // Only the PR repository configured and no origin: the PR paths work,
        // and only an operation needing the issues repository fails, naming
        // the key.
        let r = Repos::from_parts(&github(None, None), &forge(Some("up/app")), None, None);
        assert_eq!(r.prs().unwrap().slug, "up/app");
        let err = r.issues().unwrap_err().to_string();
        assert!(err.contains("issues_repo"), "{err}");

        // Neither configured, origin available: both default to it.
        let r = Repos::from_parts(
            &github(None, None),
            &forge(None),
            Some("me/fork".into()),
            None,
        );
        assert_eq!(r.issues().unwrap().slug, "me/fork");
        assert_eq!(r.prs().unwrap().slug, "me/fork");

        // A per-invocation override beats the configured repository.
        let r = Repos::from_parts(
            &github(None, None),
            &forge(Some("up/app")),
            None,
            Some("other/x"),
        );
        assert_eq!(r.prs().unwrap().slug, "other/x");
    }

    /// A config written before `[forge]` existed still loads, and the PR
    /// repository it names is refused with the key that replaced it rather
    /// than silently defaulted from `origin`.
    #[test]
    fn a_legacy_pr_repo_names_its_new_home() {
        let r = Repos::from_parts(
            &github(None, Some("up/app")),
            &forge(None),
            Some("me/fork".into()),
            None,
        );
        let err = r.prs().unwrap_err().to_string();
        assert!(err.contains("[forge] repo"), "{err}");
        assert!(err.contains("up/app"), "{err}");
    }

    #[test]
    fn repos_reject_a_configured_slug_that_is_not_owner_repo() {
        let r = Repos::from_parts(
            &github(Some("../../etc/passwd"), None),
            &forge(None),
            None,
            None,
        );
        let err = r.issues().unwrap_err().to_string();
        assert!(err.contains("owner/repo"), "{err}");
    }

    #[test]
    fn a_locator_with_a_repository_outranks_the_configured_one() {
        let repos = Repos::from_parts(&github(None, None), &forge(Some("up/app")), None, None);
        let pasted = PrLocator {
            repo: Some("fork/app".into()),
            number: 42,
        };
        assert_eq!(pasted.resolve(&repos).unwrap().slug, "fork/app");
        let bare = PrLocator {
            repo: None,
            number: 42,
        };
        assert_eq!(bare.resolve(&repos).unwrap().slug, "up/app");
    }

    #[test]
    fn resolve_or_falls_back_to_the_given_default() {
        let default = Repo {
            host: "ghe.acme.test".into(),
            slug: "me/fork".into(),
        };
        let bare = PrLocator {
            repo: None,
            number: 9,
        };
        assert_eq!(bare.resolve_or(&default).unwrap(), default);
        let pasted = PrLocator {
            repo: Some("other/app".into()),
            number: 9,
        };
        let got = pasted.resolve_or(&default).unwrap();
        assert_eq!(got.slug, "other/app");
        assert_eq!(got.host, "ghe.acme.test", "a pasted slug stays on the host");
    }

    /// A locator's slug is parsed out of untrusted pasted text, so it faces the
    /// same shape check a configured one does before reaching a `--repo`
    /// argument or a cache path.
    #[test]
    fn a_locator_repository_that_is_not_owner_repo_is_rejected() {
        let repos = Repos::from_parts(&github(None, None), &forge(Some("up/app")), None, None);
        let loc = PrLocator {
            repo: Some("../../etc/passwd".into()),
            number: 42,
        };
        let err = loc.resolve(&repos).unwrap_err().to_string();
        assert!(err.contains("owner/repo"), "{err}");
    }

    #[test]
    fn a_repo_qualifies_itself_with_its_host() {
        let r = Repo {
            host: "ghe.acme.test".into(),
            slug: "o/r".into(),
        };
        assert_eq!(r.qualified(), "ghe.acme.test/o/r");
    }

    #[test]
    fn a_pr_url_is_located_only_on_its_own_host() {
        let loc = locate_on(
            "https://github.com/o/r/pull/9?x=1#y",
            "github.com",
            "/pull/",
        )
        .unwrap();
        assert_eq!(loc.repo.as_deref(), Some("o/r"));
        assert_eq!(loc.number, 9);

        let mr = locate_on(
            "https://gitlab.com/g/sub/app/-/merge_requests/12/diffs",
            "gitlab.com",
            "/-/merge_requests/",
        )
        .unwrap();
        assert_eq!(mr.repo.as_deref(), Some("g/sub/app"));
        assert_eq!(mr.number, 12);

        assert!(locate_on("https://evil.test/o/r/pull/9", "github.com", "/pull/").is_none());
        assert!(locate_on("https://github.com/o/r/issues/9", "github.com", "/pull/").is_none());
    }

    #[test]
    fn a_pr_number_is_read_from_any_forges_url() {
        assert_eq!(
            pr_number_from_url("https://github.com/org/repo/pull/3340"),
            Some(3340)
        );
        assert_eq!(
            pr_number_from_url("https://gitlab.com/g/r/-/merge_requests/7"),
            Some(7)
        );
        assert_eq!(
            pr_number_from_url("https://codeberg.org/o/r/pulls/5"),
            Some(5)
        );
        assert_eq!(
            pr_number_from_url("https://github.com/org/repo/issues/9"),
            None
        );
    }

    /// A forge publishes a PR's head under a ref of its own on the base
    /// repository, whatever fork it came from; checking it out makes a local
    /// branch named for the PR's head at that commit.
    #[test]
    fn a_published_pr_ref_checks_out_as_the_heads_branch() {
        let git = |dir: &Path, args: &[&str]| {
            devkit_git::Git::fixture(dir)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        let base = tempfile::tempdir().unwrap();
        let upstream = base.path().join("upstream");
        std::fs::create_dir(&upstream).unwrap();
        git(&upstream, &["init", "-q", "-b", "main"]);
        git(&upstream, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&upstream, &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "pr work",
        ]);
        let pr_head = git(&upstream, &["rev-parse", "HEAD"]).trim().to_string();
        git(&upstream, &["update-ref", "refs/pull/7/head", &pr_head]);
        git(&upstream, &["reset", "-q", "--hard", "HEAD~1"]);

        let clone = base.path().join("clone");
        git(base.path(), &[
            "clone",
            "-q",
            &upstream.to_string_lossy(),
            &clone.to_string_lossy(),
        ]);
        checkout_ref(&clone, "refs/pull/7/head", "contributor/fix").unwrap();

        assert_eq!(git(&clone, &["rev-parse", "HEAD"]).trim(), pr_head);
        assert_eq!(
            git(&clone, &["branch", "--show-current"]).trim(),
            "contributor/fix"
        );
    }

    #[test]
    fn a_head_list_becomes_a_lookup() {
        let pr = |n| PrBrief {
            number: n,
            state: "OPEN".into(),
            url: String::new(),
            title: String::new(),
            head_ref_name: "b".into(),
            head_ref_oid: String::new(),
            head_repo_owner: None,
            is_draft: false,
            author_login: None,
        };
        assert!(matches!(HeadLookup::of(vec![]), HeadLookup::NoMatch));
        assert!(matches!(HeadLookup::of(vec![pr(1)]), HeadLookup::Unique(p) if p.number == 1));
        assert!(matches!(
            HeadLookup::of(vec![pr(1), pr(2)]),
            HeadLookup::Ambiguous(c) if c.len() == 2
        ));
    }
}
