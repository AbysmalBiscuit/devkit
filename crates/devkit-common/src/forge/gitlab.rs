//! GitLab as a forge: gitlab.com, or a self-managed instance through its host.
//!
//! All of it goes over REST v4. A merge request's `iid` is its PR number.

use std::{collections::HashMap, path::Path, sync::OnceLock};

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::{
    CheckRun, CheckState, Checks, Forge, ForgeKind, HeadLookup, NewPr, OpenPr, OpenPrPage, PrBrief,
    PrLocator, PrTimeline, Repo, Review, ReviewDecision, ReviewState, Reviewers, Role, Section,
    locate_on,
    rest::{Method, Rest, encode},
};

const TOKEN_ENV: &str = "GITLAB_TOKEN";

/// GitLab's largest page.
const MAX_PER_PAGE: u32 = 100;

pub struct GitlabForge {
    host: String,
    rest: Rest,
    has_token: bool,
    /// The token owner's username, read once per process.
    viewer: OnceLock<String>,
}

impl GitlabForge {
    pub fn new(host: &str) -> GitlabForge {
        GitlabForge::with_api(
            host,
            &format!("https://{host}/api/v4"),
            crate::secrets::resolve(TOKEN_ENV),
        )
    }

    /// A forge answering for `host` whose API root is `base_url`.
    pub fn with_api(host: &str, base_url: &str, token: Option<String>) -> GitlabForge {
        GitlabForge {
            host: host.to_string(),
            has_token: token.is_some(),
            rest: Rest::new(base_url, token.map(|t| ("PRIVATE-TOKEN", t))),
            viewer: OnceLock::new(),
        }
    }
}

// --- wire shapes
// ---------------------------------------------------------------

#[derive(Deserialize, Debug, Clone)]
struct User {
    #[serde(default)]
    id: u64,
    username: String,
}

#[derive(Deserialize, Debug)]
struct Pipeline {
    id: u64,
    project_id: u64,
    #[serde(default)]
    status: String,
}

/// One merge request, from the list or the single-MR endpoint. Fields only
/// the single-MR endpoint carries (`head_pipeline`) are `None` on a list.
#[derive(Deserialize, Debug)]
struct Mr {
    iid: u64,
    #[serde(default)]
    state: String,
    #[serde(default)]
    web_url: String,
    #[serde(default)]
    title: String,
    source_branch: String,
    sha: Option<String>,
    draft: Option<bool>,
    work_in_progress: Option<bool>,
    author: Option<User>,
    has_conflicts: Option<bool>,
    detailed_merge_status: Option<String>,
    head_pipeline: Option<Pipeline>,
    reviewers: Option<Vec<User>>,
    created_at: Option<String>,
    merged_at: Option<String>,
}

impl Mr {
    fn is_draft(&self) -> bool {
        self.draft.or(self.work_in_progress).unwrap_or(false)
    }

    fn author(&self) -> Option<String> {
        self.author.as_ref().map(|a| a.username.clone())
    }
}

#[derive(Deserialize, Debug)]
struct Approver {
    user: User,
    approved_at: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct Approvals {
    approvals_required: Option<u64>,
    approvals_left: Option<u64>,
    #[serde(default)]
    approved_by: Vec<Approver>,
}

/// One entry of `GET .../merge_requests/:iid/reviewers`. `state` is the
/// reviewer's review state (`unreviewed`, `reviewed`, `requested_changes`,
/// `approved`, ...), not the account state the MR object's `reviewers` carry.
#[derive(Deserialize, Debug)]
struct ReviewerEntry {
    user: User,
    #[serde(default)]
    state: String,
}

#[derive(Deserialize, Debug)]
struct Job {
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    allow_failure: bool,
    started_at: Option<String>,
}

fn parse<T: DeserializeOwned>(v: Value, what: &str) -> Result<T> {
    serde_json::from_value(v).with_context(|| format!("unexpected GitLab {what} shape"))
}

// --- paths -------------------------------------------------------------------

fn project(slug: &str) -> String {
    format!("/projects/{}", encode(slug))
}

fn mr_path(repo: &Repo, n: u64) -> String {
    format!("{}/merge_requests/{n}", project(&repo.slug))
}

/// The list filter selecting the viewer's MRs in `section`.
fn section_filter(section: Section, me: &str) -> String {
    let me = encode(me);
    match section {
        Section::Mine => format!("author_username={me}"),
        Section::ReviewRequested => format!("reviewer_username={me}"),
        Section::ReviewedBy => format!("approved_by_usernames%5B%5D={me}"),
    }
}

fn role_filter(role: Role, me: &str) -> String {
    let me = encode(me);
    match role {
        Role::Author => format!("author_username={me}"),
        Role::Reviewer => format!("reviewer_username={me}"),
    }
}

// --- parsing -----------------------------------------------------------------

/// GitLab's MR state in devkit's vocabulary. `locked` is a merge in progress,
/// which no longer takes changes.
fn pr_state(s: &str) -> String {
    match s {
        "merged" => "MERGED".into(),
        "closed" | "locked" => "CLOSED".into(),
        "opened" | "reopened" => "OPEN".into(),
        other => other.to_uppercase(),
    }
}

impl From<Mr> for PrBrief {
    fn from(m: Mr) -> PrBrief {
        PrBrief {
            number: m.iid,
            state: pr_state(&m.state),
            is_draft: m.is_draft(),
            author_login: m.author(),
            url: m.web_url,
            title: m.title,
            head_ref_name: m.source_branch,
            head_ref_oid: m.sha.unwrap_or_default(),
            head_repo_owner: None,
        }
    }
}

/// A single-MR response as a brief. `None` is reserved for a 404: a body that
/// came back and would not parse is an error, since "there is no such PR" is
/// what closes a worktree's finished verdict.
fn brief_of_response(body: Option<Value>, n: u64, slug: &str) -> Result<Option<PrBrief>> {
    match body {
        None => Ok(None),
        Some(v) => Ok(Some(
            parse::<Mr>(v, "merge request")
                .with_context(|| format!("reading MR !{n} in {slug}"))?
                .into(),
        )),
    }
}

/// A source-branch list as a lookup. One entry that will not parse makes the
/// whole answer unavailable rather than silently shorter.
fn head_lookup(body: Value, branch: &str) -> HeadLookup {
    match parse::<Vec<Mr>>(body, "merge request list") {
        Ok(mrs) => HeadLookup::of(mrs.into_iter().map(Into::into).collect()),
        Err(e) => HeadLookup::Unavailable(format!("MRs for branch `{branch}`: {e:#}")),
    }
}

const DRAFT_PREFIXES: [&str; 3] = ["[draft]", "(draft)", "draft:"];

/// `title` with every leading draft marker removed. GitLab reads a title as a
/// draft when it starts with `[Draft]`, `(Draft)` or `Draft:` in any case, and
/// markers can stack, so one strip may leave another behind.
fn strip_draft(title: &str) -> &str {
    let mut rest = title;
    loop {
        let lower = rest.to_ascii_lowercase();
        match DRAFT_PREFIXES.iter().find(|p| lower.starts_with(**p)) {
            Some(p) => rest = rest[p.len()..].trim_start(),
            None => return rest,
        }
    }
}

/// The title GitLab reads as a draft when `draft`, since it has no draft flag
/// of its own.
fn draft_title(title: &str, draft: bool) -> String {
    if draft && strip_draft(title) == title {
        format!("Draft: {title}")
    } else {
        title.to_string()
    }
}

/// One person's standing on an MR: `None` is a pending review request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Standing {
    login: String,
    state: Option<ReviewState>,
    /// When the standing was reached, where GitLab records it (approvals).
    at: String,
}

/// Every approver and reviewer of an MR, each once. An approval outranks the
/// reviewer entry for the same person: anyone with approval rights can
/// approve without having been asked.
fn standings(approvals: &Approvals, reviewers: &[ReviewerEntry]) -> Vec<Standing> {
    let mut out: Vec<Standing> = approvals
        .approved_by
        .iter()
        .map(|a| Standing {
            login: a.user.username.clone(),
            state: Some(ReviewState::Approved),
            at: a.approved_at.clone().unwrap_or_default(),
        })
        .collect();
    for r in reviewers {
        if out.iter().any(|s| s.login == r.user.username) {
            continue;
        }
        let state = match r.state.as_str() {
            "approved" => Some(ReviewState::Approved),
            "requested_changes" => Some(ReviewState::ChangesRequested),
            "reviewed" => Some(ReviewState::Commented),
            _ => None,
        };
        out.push(Standing {
            login: r.user.username.clone(),
            state,
            at: String::new(),
        });
    }
    out
}

fn reviewers_of(standings: &[Standing]) -> Reviewers {
    let (submitted, requested): (Vec<_>, Vec<_>) =
        standings.iter().partition(|s| s.state.is_some());
    Reviewers {
        requested: requested.into_iter().map(|s| s.login.clone()).collect(),
        submitted: submitted.into_iter().map(|s| s.login.clone()).collect(),
    }
}

/// The MR's overall verdict. Approval rules only exist where the project
/// requires approvals, so with none required there is no verdict, the same
/// as GitHub's empty `reviewDecision` on a repository requiring no review.
fn review_decision(mr: &Mr, approvals: &Approvals) -> Option<ReviewDecision> {
    if mr.detailed_merge_status.as_deref() == Some("requested_changes") {
        return Some(ReviewDecision::ChangesRequested);
    }
    match (approvals.approvals_required, approvals.approvals_left) {
        (Some(req), Some(0)) if req > 0 => Some(ReviewDecision::Approved),
        (Some(req), Some(_)) if req > 0 => Some(ReviewDecision::ReviewRequired),
        _ => None,
    }
}

fn pipeline_state(status: &str) -> CheckState {
    match status {
        "success" | "skipped" => CheckState::Passed,
        "failed" | "canceled" => CheckState::Failed,
        _ => CheckState::Running,
    }
}

/// A job that is allowed to fail, or a manual job nobody has to run, does not
/// hold the pipeline back.
fn job_state(j: &Job) -> CheckState {
    match j.status.as_str() {
        "success" | "skipped" => CheckState::Passed,
        "failed" if j.allow_failure => CheckState::Passed,
        "failed" | "canceled" | "canceling" => CheckState::Failed,
        "manual" if j.allow_failure => CheckState::Passed,
        _ => CheckState::Running,
    }
}

fn checks_of(mr: &Mr, jobs: Vec<Job>) -> Option<Checks> {
    let pipeline = mr.head_pipeline.as_ref()?;
    Some(Checks {
        overall: pipeline_state(&pipeline.status),
        runs: jobs
            .into_iter()
            .map(|j| CheckRun {
                state: job_state(&j),
                name: j.name,
                started_at: j.started_at,
            })
            .collect(),
    })
}

/// One open MR in the triage shape, from its single-MR body and review data.
fn open_pr_of(
    mr: Mr,
    approvals: &Approvals,
    reviewers: &[ReviewerEntry],
    jobs: Vec<Job>,
) -> OpenPr {
    let standings = standings(approvals, reviewers);
    OpenPr {
        review_decision: review_decision(&mr, approvals),
        checks: checks_of(&mr, jobs),
        conflicting: mr.has_conflicts.unwrap_or(false),
        is_draft: mr.is_draft(),
        author: mr.author().unwrap_or_default(),
        reviews: standings
            .iter()
            .filter_map(|s| {
                Some(Review {
                    author: s.login.clone(),
                    state: s.state?,
                    submitted_at: s.at.clone(),
                })
            })
            .collect(),
        review_requests: standings
            .iter()
            .filter(|s| s.state.is_none())
            .map(|s| s.login.clone())
            .collect(),
        number: mr.iid,
        url: mr.web_url,
        title: mr.title,
        head_ref_name: mr.source_branch,
    }
}

impl From<Mr> for PrTimeline {
    fn from(m: Mr) -> PrTimeline {
        PrTimeline {
            created_at: m.created_at,
            merged_at: m.merged_at,
            additions: 0,
            deletions: 0,
        }
    }
}

// --- requests ----------------------------------------------------------------

impl GitlabForge {
    fn viewer(&self) -> Result<String> {
        if let Some(v) = self.viewer.get() {
            return Ok(v.clone());
        }
        let user: User = parse(
            self.rest.get("/user").context("reading the GitLab user")?,
            "user",
        )?;
        Ok(self.viewer.get_or_init(|| user.username).clone())
    }

    fn mr(&self, repo: &Repo, n: u64) -> Result<Mr> {
        let v = self
            .rest
            .get(&mr_path(repo, n))
            .with_context(|| format!("reading MR !{n} in {}", repo.slug))?;
        parse(v, "merge request")
    }

    /// Each login's user id. An unknown login is an error naming it, since
    /// silently dropping a requested reviewer is a request nobody sees.
    fn user_ids(&self, logins: &[String]) -> Result<Vec<u64>> {
        logins
            .iter()
            .map(|login| {
                let users: Vec<User> = parse(
                    self.rest
                        .get(&format!("/users?username={}", encode(login)))
                        .with_context(|| format!("looking up GitLab user `{login}`"))?,
                    "user list",
                )?;
                users
                    .into_iter()
                    .find(|u| u.username.eq_ignore_ascii_case(login))
                    .map(|u| u.id)
                    .with_context(|| format!("no GitLab user `{login}` on {}", self.host))
            })
            .collect()
    }

    fn approvals(&self, repo: &Repo, n: u64) -> Result<Approvals> {
        let v = self
            .rest
            .get(&format!("{}/approvals", mr_path(repo, n)))
            .with_context(|| format!("reading approvals of MR !{n} in {}", repo.slug))?;
        parse(v, "approvals")
    }

    fn reviewer_entries(&self, repo: &Repo, n: u64) -> Result<Vec<ReviewerEntry>> {
        let v = self
            .rest
            .get(&format!(
                "{}/reviewers?per_page={MAX_PER_PAGE}",
                mr_path(repo, n)
            ))
            .with_context(|| format!("reading reviewers of MR !{n} in {}", repo.slug))?;
        parse(v, "reviewer list")
    }

    /// The jobs of `pipeline`, which lives in the project that ran it: a fork
    /// MR's pipeline can run in the fork. Empty when they cannot be read,
    /// which leaves the pipeline's own status as the verdict.
    fn jobs(&self, pipeline: &Pipeline) -> Vec<Job> {
        let path = format!(
            "/projects/{}/pipelines/{}/jobs?per_page={MAX_PER_PAGE}",
            pipeline.project_id, pipeline.id
        );
        self.rest
            .get(&path)
            .and_then(|v| parse(v, "job list"))
            .unwrap_or_default()
    }

    fn open_pr(&self, repo: &Repo, n: u64) -> Result<(OpenPr, Approvals)> {
        let mr = self.mr(repo, n)?;
        let approvals = self.approvals(repo, n)?;
        let reviewers = self.reviewer_entries(repo, n)?;
        let jobs = mr
            .head_pipeline
            .as_ref()
            .map(|p| self.jobs(p))
            .unwrap_or_default();
        Ok((open_pr_of(mr, &approvals, &reviewers, jobs), approvals))
    }

    /// Open the MR, from a fork when `origin` is one: GitLab creates a fork's
    /// MR on the fork, naming the upstream as its target.
    fn create_from(&self, repo: &Repo, pr: &NewPr<'_>, origin: Option<&str>) -> Result<String> {
        let mut body = json!({
            "source_branch": pr.head,
            "target_branch": pr.base,
            "title": draft_title(pr.title, pr.draft),
            "description": pr.body,
        });
        let source = match origin {
            Some(fork) if !fork.eq_ignore_ascii_case(&repo.slug) => {
                let upstream = self
                    .rest
                    .get(&project(&repo.slug))
                    .with_context(|| format!("reading project {}", repo.slug))?;
                body["target_project_id"] = upstream
                    .get("id")
                    .cloned()
                    .with_context(|| format!("no id for project {}", repo.slug))?;
                fork
            }
            _ => repo.slug.as_str(),
        };
        let created = self
            .rest
            .send(
                Method::POST,
                &format!("{}/merge_requests", project(source)),
                &body,
            )
            .with_context(|| format!("opening an MR from {} into {}", pr.head, pr.base))?;
        created["web_url"]
            .as_str()
            .map(str::to_string)
            .context("no web_url in GitLab's new merge request")
    }
}

impl Forge for GitlabForge {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Gitlab
    }

    fn host(&self) -> &str {
        &self.host
    }

    fn ready(&self) -> bool {
        self.has_token
    }

    fn check(&self) -> Result<String> {
        anyhow::ensure!(self.has_token, "no GitLab token (set {TOKEN_ENV})");
        Ok(format!("gitlab: {} ({})", self.viewer()?, self.host))
    }

    fn locate(&self, url: &str) -> Option<PrLocator> {
        locate_on(url, &self.host, "/-/merge_requests/")
    }

    fn pr(&self, repo: &Repo, n: u64) -> Result<Option<PrBrief>> {
        let body = self
            .rest
            .get_opt(&mr_path(repo, n))
            .with_context(|| format!("reading MR !{n} in {}", repo.slug))?;
        brief_of_response(body, n, &repo.slug)
    }

    /// `NoMatch` only when GitLab answered: `issue end` deletes a branch on
    /// that answer.
    fn pr_by_head(&self, repo: &Repo, branch: &str) -> HeadLookup {
        let path = format!(
            "{}/merge_requests?source_branch={}&state=all&per_page={MAX_PER_PAGE}",
            project(&repo.slug),
            encode(branch)
        );
        match self.rest.get(&path) {
            Ok(v) => head_lookup(v, branch),
            Err(e) => HeadLookup::Unavailable(format!("{e:#}")),
        }
    }

    fn prs_by_head(&self, repo: &Repo, branches: &[String]) -> HashMap<String, HeadLookup> {
        crate::pool::install(|| {
            branches
                .par_iter()
                .map(|b| (b.clone(), self.pr_by_head(repo, b)))
                .collect()
        })
    }

    fn create(&self, repo: &Repo, pr: &NewPr<'_>, cwd: &Path) -> Result<String> {
        let origin = super::remote::origin_slug(&cwd.to_string_lossy(), &self.host).ok();
        self.create_from(repo, pr, origin.as_deref())
    }

    fn mark_ready(&self, repo: &Repo, n: u64) -> Result<()> {
        let mr = self.mr(repo, n)?;
        if !mr.is_draft() {
            return Ok(());
        }
        let updated: Mr = parse(
            self.rest
                .send(
                    Method::PUT,
                    &mr_path(repo, n),
                    &json!({ "title": strip_draft(&mr.title) }),
                )
                .with_context(|| format!("marking MR !{n} in {} ready", repo.slug))?,
            "merge request",
        )?;
        anyhow::ensure!(
            !updated.is_draft(),
            "MR !{n} in {} is still a draft after removing the draft prefix from its title",
            repo.slug
        );
        Ok(())
    }

    /// `reviewer_ids` replaces the whole set, so the current reviewers are read
    /// and sent back with the new ones.
    fn add_reviewers(&self, repo: &Repo, n: u64, logins: &[String]) -> Result<()> {
        if logins.is_empty() {
            return Ok(());
        }
        let mut ids: Vec<u64> = self
            .mr(repo, n)?
            .reviewers
            .unwrap_or_default()
            .iter()
            .map(|u| u.id)
            .collect();
        let existing = ids.len();
        for id in self.user_ids(logins)? {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if ids.len() == existing {
            return Ok(());
        }
        self.rest
            .send(
                Method::PUT,
                &mr_path(repo, n),
                &json!({ "reviewer_ids": ids }),
            )
            .with_context(|| format!("requesting reviewers on MR !{n} in {}", repo.slug))?;
        Ok(())
    }

    fn reviewers(&self, repo: &Repo, n: u64) -> Result<Reviewers> {
        let approvals = self.approvals(repo, n)?;
        let entries = self.reviewer_entries(repo, n)?;
        Ok(reviewers_of(&standings(&approvals, &entries)))
    }

    /// The head ref lives on the target project, so a fork's MR checks out
    /// with no remote for the fork. GitLab deletes it 14 days after the MR
    /// closes or merges.
    fn checkout(&self, _repo: &Repo, pr: &PrBrief, dir: &Path) -> Result<()> {
        super::checkout_ref(
            dir,
            &format!("refs/merge-requests/{}/head", pr.number),
            &pr.head_ref_name,
        )
    }

    /// One list page, then per MR the single-MR read (pipeline, fresh
    /// conflict state), its approvals, reviewers and pipeline jobs, fetched in
    /// parallel. The cursor is the page number.
    fn open_prs(
        &self,
        repo: &Repo,
        section: Section,
        page_size: u32,
        cursor: Option<&str>,
    ) -> Result<OpenPrPage> {
        let viewer = self.viewer()?;
        let page: u32 = match cursor {
            Some(c) => c
                .parse()
                .with_context(|| format!("not a GitLab page number: `{c}`"))?,
            None => 1,
        };
        let path = format!(
            "{}/merge_requests?state=opened&{}&per_page={}&page={page}",
            project(&repo.slug),
            section_filter(section, &viewer),
            page_size.clamp(1, MAX_PER_PAGE)
        );
        let listed = self
            .rest
            .get_page(&path)
            .with_context(|| format!("listing open MRs in {}", repo.slug))?;
        let iids: Vec<u64> = parse::<Vec<Mr>>(listed.body, "merge request list")?
            .into_iter()
            .map(|m| m.iid)
            .collect();
        let found: Vec<(OpenPr, Approvals)> = crate::pool::install(|| {
            iids.par_iter()
                .map(|n| self.open_pr(repo, *n))
                .collect::<Result<Vec<_>>>()
        })?;
        // A GitLab build without the approval filter ignores it and lists
        // every open MR, so each is checked against its own approvers.
        let prs = found
            .into_iter()
            .filter(|(_, a)| {
                section != Section::ReviewedBy
                    || a.approved_by.iter().any(|x| x.user.username == viewer)
            })
            .map(|(pr, _)| pr)
            .collect();
        Ok(OpenPrPage {
            viewer,
            prs,
            next: listed.next.map(|n| n.to_string()),
        })
    }

    /// The viewer's MRs in `role`, newest first. Line counts stay zero: REST
    /// reports none without reading every diff.
    fn timeline(&self, repo: &Repo, role: Role, max: usize) -> Result<Vec<PrTimeline>> {
        if max == 0 {
            return Ok(Vec::new());
        }
        let filter = role_filter(role, &self.viewer()?);
        let per_page = max.min(MAX_PER_PAGE as usize);
        let mut out: Vec<PrTimeline> = Vec::new();
        let mut page = 1;
        loop {
            let path = format!(
                "{}/merge_requests?state=all&{filter}&order_by=created_at&sort=desc\
                 &per_page={per_page}&page={page}",
                project(&repo.slug)
            );
            let listed = self
                .rest
                .get_page(&path)
                .with_context(|| format!("listing MRs in {}", repo.slug))?;
            let mrs: Vec<Mr> = parse(listed.body, "merge request list")?;
            out.extend(mrs.into_iter().map(PrTimeline::from));
            match listed.next {
                Some(n) if out.len() < max => page = n,
                _ => break,
            }
        }
        out.truncate(max);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::stub::{self, Route};

    fn repo() -> Repo {
        Repo {
            host: "gitlab.test".into(),
            slug: "g/sub/app".into(),
        }
    }

    const P: &str = "/api/v4/projects/g%2Fsub%2Fapp";

    fn forge(s: &stub::Stub) -> GitlabForge {
        GitlabForge::with_api(
            "gitlab.test",
            &format!("{}/api/v4", s.url()),
            Some("t0k".into()),
        )
    }

    fn mr_json(iid: u64, extra: Value) -> String {
        let mut v = json!({
            "iid": iid, "state": "opened", "title": format!("MR {iid}"),
            "web_url": format!("https://gitlab.test/g/sub/app/-/merge_requests/{iid}"),
            "source_branch": format!("feat/{iid}"), "sha": format!("sha{iid}"),
            "draft": false, "author": {"id": 1, "username": "me"},
            "reviewers": [], "has_conflicts": false
        });
        for (k, x) in extra.as_object().into_iter().flatten() {
            v[k] = x.clone();
        }
        v.to_string()
    }

    fn body_of(req: &stub::Request) -> Value {
        serde_json::from_str(&req.body).unwrap()
    }

    // --- parsing ---

    #[test]
    fn mr_states_map_onto_devkits_vocabulary() {
        assert_eq!(pr_state("opened"), "OPEN");
        assert_eq!(pr_state("merged"), "MERGED");
        assert_eq!(pr_state("closed"), "CLOSED");
        assert_eq!(pr_state("locked"), "CLOSED");
    }

    #[test]
    fn a_single_mr_maps_onto_the_brief() {
        let v: Value = serde_json::from_str(&mr_json(
            7,
            json!({"state": "merged", "draft": null, "work_in_progress": true}),
        ))
        .unwrap();
        let b = brief_of_response(Some(v), 7, "g/app").unwrap().unwrap();
        assert_eq!(b.number, 7);
        assert_eq!(b.state, "MERGED");
        assert_eq!(b.head_ref_name, "feat/7");
        assert_eq!(b.head_ref_oid, "sha7");
        assert_eq!(b.author_login.as_deref(), Some("me"));
        assert!(b.is_draft, "falls back to work_in_progress");
        assert_eq!(b.url, "https://gitlab.test/g/sub/app/-/merge_requests/7");
    }

    #[test]
    fn a_body_that_will_not_parse_is_not_an_absent_mr() {
        assert!(brief_of_response(None, 7, "g/app").unwrap().is_none());
        let err = brief_of_response(Some(json!({"state": "opened"})), 7, "g/app")
            .unwrap_err()
            .to_string();
        assert!(err.contains("!7") && err.contains("g/app"), "{err}");
    }

    #[test]
    fn a_head_list_with_an_unparseable_entry_is_unavailable_not_shorter() {
        let ok: Value = serde_json::from_str(&format!("[{}]", mr_json(3, json!({})))).unwrap();
        assert!(matches!(head_lookup(ok, "b"), HeadLookup::Unique(p) if p.number == 3));
        assert!(matches!(head_lookup(json!([]), "b"), HeadLookup::NoMatch));
        let bad = json!([{"iid": 3}]);
        assert!(matches!(head_lookup(bad, "b"), HeadLookup::Unavailable(r) if r.contains("`b`")));
    }

    #[test]
    fn every_stacked_draft_prefix_is_stripped_in_any_case() {
        assert_eq!(strip_draft("Draft: [DRAFT] (draft) Fix it"), "Fix it");
        assert_eq!(strip_draft("draft:Fix"), "Fix");
        assert_eq!(
            strip_draft("Fix the draft: parser"),
            "Fix the draft: parser"
        );
    }

    #[test]
    fn a_draft_title_carries_one_prefix() {
        assert_eq!(draft_title("Fix", true), "Draft: Fix");
        assert_eq!(draft_title("[Draft] Fix", true), "[Draft] Fix");
        assert_eq!(draft_title("Fix", false), "Fix");
    }

    fn reviewer(login: &str, state: &str) -> ReviewerEntry {
        ReviewerEntry {
            user: User {
                id: 0,
                username: login.into(),
            },
            state: state.into(),
        }
    }

    fn approvals(required: u64, left: u64, by: &[&str]) -> Approvals {
        Approvals {
            approvals_required: Some(required),
            approvals_left: Some(left),
            approved_by: by
                .iter()
                .map(|l| Approver {
                    user: User {
                        id: 0,
                        username: (*l).into(),
                    },
                    approved_at: Some("2026-01-02T00:00:00Z".into()),
                })
                .collect(),
        }
    }

    #[test]
    fn an_approval_and_a_reviewer_entry_are_one_standing() {
        let s = standings(&approvals(1, 0, &["ann"]), &[
            reviewer("ann", "approved"),
            reviewer("bob", "requested_changes"),
            reviewer("cat", "reviewed"),
            reviewer("dan", "unreviewed"),
            reviewer("eve", "review_started"),
        ]);
        let got: Vec<_> = s.iter().map(|s| (s.login.as_str(), s.state)).collect();
        assert_eq!(got, vec![
            ("ann", Some(ReviewState::Approved)),
            ("bob", Some(ReviewState::ChangesRequested)),
            ("cat", Some(ReviewState::Commented)),
            ("dan", None),
            ("eve", None),
        ]);
        assert_eq!(s[0].at, "2026-01-02T00:00:00Z");
        let r = reviewers_of(&s);
        assert_eq!(r.requested, vec!["dan", "eve"]);
        assert_eq!(r.submitted, vec!["ann", "bob", "cat"]);
    }

    fn mr_of(extra: Value) -> Mr {
        serde_json::from_str(&mr_json(1, extra)).unwrap()
    }

    #[test]
    fn the_review_decision_needs_approval_rules_or_a_change_request() {
        let plain = mr_of(json!({}));
        assert_eq!(review_decision(&plain, &approvals(0, 0, &[])), None);
        assert_eq!(review_decision(&plain, &Approvals::default()), None);
        assert_eq!(
            review_decision(&plain, &approvals(2, 1, &["a"])),
            Some(ReviewDecision::ReviewRequired)
        );
        assert_eq!(
            review_decision(&plain, &approvals(1, 0, &["a"])),
            Some(ReviewDecision::Approved)
        );
        let blocked = mr_of(json!({"detailed_merge_status": "requested_changes"}));
        assert_eq!(
            review_decision(&blocked, &approvals(1, 0, &["a"])),
            Some(ReviewDecision::ChangesRequested)
        );
    }

    #[test]
    fn checks_come_from_the_head_pipeline_and_its_jobs() {
        assert_eq!(
            checks_of(&mr_of(json!({"head_pipeline": null})), vec![]),
            None
        );
        for (status, want) in [
            ("success", CheckState::Passed),
            ("failed", CheckState::Failed),
            ("canceled", CheckState::Failed),
            ("running", CheckState::Running),
            ("pending", CheckState::Running),
        ] {
            let mr = mr_of(json!({"head_pipeline": {"id": 9, "project_id": 4, "status": status}}));
            assert_eq!(checks_of(&mr, vec![]).unwrap().overall, want, "{status}");
        }
        let jobs: Vec<Job> = serde_json::from_value(json!([
            {"name": "lint", "status": "failed", "allow_failure": true, "started_at": "t"},
            {"name": "test", "status": "failed", "allow_failure": false},
            {"name": "deploy", "status": "manual", "allow_failure": true},
            {"name": "gate", "status": "manual", "allow_failure": false},
        ]))
        .unwrap();
        let mr = mr_of(json!({"head_pipeline": {"id": 9, "project_id": 4, "status": "failed"}}));
        let runs = checks_of(&mr, jobs).unwrap().runs;
        let got: Vec<_> = runs.iter().map(|r| (r.name.as_str(), r.state)).collect();
        assert_eq!(got, vec![
            ("lint", CheckState::Passed),
            ("test", CheckState::Failed),
            ("deploy", CheckState::Passed),
            ("gate", CheckState::Running),
        ]);
        assert_eq!(runs[0].started_at.as_deref(), Some("t"));
    }

    // --- wire ---

    #[test]
    fn check_without_a_token_names_the_variable_and_sends_nothing() {
        let s = stub::serve(vec![]);
        let f = GitlabForge::with_api("gitlab.test", &format!("{}/api/v4", s.url()), None);
        assert!(!f.ready());
        let err = f.check().unwrap_err().to_string();
        assert!(err.contains("GITLAB_TOKEN"), "{err}");
        assert!(s.requests().is_empty());
    }

    #[test]
    fn check_reads_the_token_owner_once() {
        let s = stub::serve(vec![Route::new(
            "GET",
            "/api/v4/user",
            200,
            r#"{"id":1,"username":"lev"}"#,
        )]);
        let f = forge(&s);
        assert!(f.ready());
        assert_eq!(f.check().unwrap(), "gitlab: lev (gitlab.test)");
        f.check().unwrap();
        let reqs = s.requests();
        assert_eq!(reqs.len(), 1, "the viewer is cached");
        assert_eq!(reqs[0].header("PRIVATE-TOKEN"), Some("t0k"));
    }

    #[test]
    fn a_merge_request_url_is_located_on_its_host_only() {
        let f = GitlabForge::with_api("gitlab.test", "http://unused", None);
        let loc = f
            .locate("https://gitlab.test/g/sub/app/-/merge_requests/12/diffs")
            .unwrap();
        assert_eq!(loc.repo.as_deref(), Some("g/sub/app"));
        assert_eq!(loc.number, 12);
        assert!(
            f.locate("https://gitlab.com/g/app/-/merge_requests/12")
                .is_none()
        );
    }

    #[test]
    fn pr_reads_one_mr_by_iid_and_a_404_is_absent() {
        let s = stub::serve(vec![Route::new(
            "GET",
            &format!("{P}/merge_requests/7"),
            200,
            &mr_json(7, json!({})),
        )]);
        let f = forge(&s);
        assert_eq!(f.pr(&repo(), 7).unwrap().unwrap().number, 7);
        assert!(f.pr(&repo(), 8).unwrap().is_none());
        let reqs = s.requests();
        assert_eq!(reqs[0].path, format!("{P}/merge_requests/7"));
        assert_eq!(reqs[0].header("PRIVATE-TOKEN"), Some("t0k"));
    }

    #[test]
    fn a_head_lookup_asks_for_the_branch_in_every_state() {
        let s = stub::serve(vec![Route::new(
            "GET",
            &format!("{P}/merge_requests?"),
            200,
            &format!("[{},{}]", mr_json(1, json!({})), mr_json(2, json!({}))),
        )]);
        let got = forge(&s).pr_by_head(&repo(), "feat/x y");
        assert!(matches!(got, HeadLookup::Ambiguous(c) if c.len() == 2));
        assert_eq!(
            s.requests()[0].path,
            format!("{P}/merge_requests?source_branch=feat%2Fx%20y&state=all&per_page=100")
        );
    }

    /// A lookup GitLab did not answer is not "no MR": `issue end` deletes a
    /// branch on the latter.
    #[test]
    fn a_head_lookup_that_gets_no_answer_is_unavailable() {
        let s = stub::serve(vec![Route::new("GET", P, 500, "{}")]);
        let f = forge(&s);
        assert!(matches!(
            f.pr_by_head(&repo(), "b"),
            HeadLookup::Unavailable(_)
        ));
        let many = f.prs_by_head(&repo(), &["a".into(), "b".into()]);
        assert!(
            many.values()
                .all(|l| matches!(l, HeadLookup::Unavailable(_)))
        );
        assert_eq!(many.len(), 2);
        let gone = stub::serve(vec![]);
        assert!(matches!(
            forge(&gone).pr_by_head(&repo(), "b"),
            HeadLookup::Unavailable(_)
        ));
    }

    fn new_pr(draft: bool) -> NewPr<'static> {
        NewPr {
            base: "main",
            head: "feat/x",
            title: "Fix",
            body: "Body",
            draft,
            attachments: &[],
        }
    }

    #[test]
    fn create_posts_a_draft_titled_mr() {
        let s = stub::serve(vec![Route::new(
            "POST",
            &format!("{P}/merge_requests"),
            201,
            r#"{"iid":3,"web_url":"https://gitlab.test/g/sub/app/-/merge_requests/3"}"#,
        )]);
        let url = forge(&s)
            .create_from(&repo(), &new_pr(true), Some("g/sub/app"))
            .unwrap();
        assert_eq!(url, "https://gitlab.test/g/sub/app/-/merge_requests/3");
        let post = s
            .requests()
            .into_iter()
            .find(|r| r.method == "POST")
            .unwrap();
        assert_eq!(post.path, format!("{P}/merge_requests"));
        assert_eq!(
            body_of(&post),
            json!({
                "source_branch": "feat/x", "target_branch": "main",
                "title": "Draft: Fix", "description": "Body"
            })
        );
    }

    #[test]
    fn create_from_a_fork_posts_on_the_fork_targeting_upstream() {
        let s = stub::serve(vec![
            Route::new("GET", P, 200, r#"{"id":77}"#),
            Route::new(
                "POST",
                "/api/v4/projects/me%2Fapp/merge_requests",
                201,
                r#"{"web_url":"u"}"#,
            ),
        ]);
        forge(&s)
            .create_from(&repo(), &new_pr(false), Some("me/app"))
            .unwrap();
        let post = s
            .requests()
            .into_iter()
            .find(|r| r.method == "POST")
            .unwrap();
        let body = body_of(&post);
        assert_eq!(body["target_project_id"], 77);
        assert_eq!(body["title"], "Fix");
    }

    #[test]
    fn mark_ready_puts_the_title_without_its_draft_prefixes() {
        let s = stub::serve(vec![
            Route::new(
                "GET",
                &format!("{P}/merge_requests/4"),
                200,
                &mr_json(4, json!({"title": "Draft: [Draft] Fix", "draft": true})),
            ),
            Route::new(
                "PUT",
                &format!("{P}/merge_requests/4"),
                200,
                &mr_json(4, json!({"title": "Fix"})),
            ),
        ]);
        forge(&s).mark_ready(&repo(), 4).unwrap();
        let put = s
            .requests()
            .into_iter()
            .find(|r| r.method == "PUT")
            .unwrap();
        assert_eq!(body_of(&put), json!({"title": "Fix"}));
    }

    #[test]
    fn mark_ready_on_a_ready_mr_sends_no_update() {
        let s = stub::serve(vec![Route::new(
            "GET",
            &format!("{P}/merge_requests/4"),
            200,
            &mr_json(4, json!({})),
        )]);
        forge(&s).mark_ready(&repo(), 4).unwrap();
        assert!(s.requests().iter().all(|r| r.method == "GET"));
    }

    #[test]
    fn add_reviewers_sends_the_existing_set_with_the_new_ids() {
        let s = stub::serve(vec![
            Route::new(
                "GET",
                &format!("{P}/merge_requests/4"),
                200,
                &mr_json(4, json!({"reviewers": [{"id": 2, "username": "old"}]})),
            ),
            Route::new(
                "GET",
                "/api/v4/users?username=new",
                200,
                r#"[{"id":9,"username":"new"}]"#,
            ),
            Route::new(
                "GET",
                "/api/v4/users?username=old",
                200,
                r#"[{"id":2,"username":"old"}]"#,
            ),
            Route::new("PUT", &format!("{P}/merge_requests/4"), 200, "{}"),
        ]);
        forge(&s)
            .add_reviewers(&repo(), 4, &["new".into(), "old".into()])
            .unwrap();
        let put = s
            .requests()
            .into_iter()
            .find(|r| r.method == "PUT")
            .unwrap();
        assert_eq!(body_of(&put), json!({"reviewer_ids": [2, 9]}));
    }

    #[test]
    fn reviewers_split_pending_requests_from_submitted_reviews() {
        let s = stub::serve(vec![
            Route::new(
                "GET",
                &format!("{P}/merge_requests/4/approvals"),
                200,
                r#"{"approvals_required":0,"approvals_left":0,
                    "approved_by":[{"user":{"id":3,"username":"approver"}}]}"#,
            ),
            Route::new(
                "GET",
                &format!("{P}/merge_requests/4/reviewers"),
                200,
                r#"[{"user":{"id":1,"username":"asked"},"state":"unreviewed"},
                    {"user":{"id":2,"username":"critic"},"state":"requested_changes"}]"#,
            ),
        ]);
        let r = forge(&s).reviewers(&repo(), 4).unwrap();
        assert_eq!(r.requested, vec!["asked"]);
        assert_eq!(r.submitted, vec!["approver", "critic"]);
    }

    fn open_pr_routes(section_prefix: &str, approved_by: &str) -> Vec<Route> {
        vec![
            Route::new("GET", "/api/v4/user", 200, r#"{"id":1,"username":"me"}"#),
            Route::new(
                "GET",
                section_prefix,
                200,
                &format!("[{}]", mr_json(5, json!({}))),
            )
            .header("X-Next-Page", "3"),
            Route::new(
                "GET",
                &format!("{P}/merge_requests/5/approvals"),
                200,
                &format!(
                    r#"{{"approvals_required":1,"approvals_left":1,"approved_by":{approved_by}}}"#
                ),
            ),
            Route::new(
                "GET",
                &format!("{P}/merge_requests/5/reviewers"),
                200,
                r#"[{"user":{"id":2,"username":"rev"},"state":"unreviewed"}]"#,
            ),
            Route::new(
                "GET",
                "/api/v4/projects/40/pipelines/9/jobs",
                200,
                r#"[{"name":"test","status":"running"}]"#,
            ),
            Route::new(
                "GET",
                &format!("{P}/merge_requests/5"),
                200,
                &mr_json(
                    5,
                    json!({"has_conflicts": true,
                           "head_pipeline": {"id": 9, "project_id": 40, "status": "running"}}),
                ),
            ),
        ]
    }

    #[test]
    fn open_prs_lists_one_page_of_the_viewers_section_and_enriches_each_mr() {
        let list = format!("{P}/merge_requests?state=opened&reviewer_username=me");
        let s = stub::serve(open_pr_routes(&list, "[]"));
        let page = forge(&s)
            .open_prs(&repo(), Section::ReviewRequested, 25, Some("2"))
            .unwrap();
        assert_eq!(page.viewer, "me");
        assert_eq!(page.next.as_deref(), Some("3"));
        let pr = &page.prs[0];
        assert_eq!(pr.number, 5);
        assert!(pr.conflicting);
        assert_eq!(pr.review_decision, Some(ReviewDecision::ReviewRequired));
        assert_eq!(pr.review_requests, vec!["rev"]);
        let checks = pr.checks.as_ref().unwrap();
        assert_eq!(checks.overall, CheckState::Running);
        assert_eq!(checks.runs[0].name, "test");
        let paths: Vec<String> = s.requests().into_iter().map(|r| r.path).collect();
        assert!(
            paths.contains(&format!("{list}&per_page=25&page=2")),
            "{paths:?}"
        );
    }

    /// A GitLab build without the `approved_by_usernames` filter ignores it and
    /// lists every open MR, so an MR the viewer has not approved is dropped.
    #[test]
    fn the_reviewed_by_section_keeps_only_mrs_the_viewer_approved() {
        let list = format!("{P}/merge_requests?state=opened&approved_by_usernames%5B%5D=me");
        let s = stub::serve(open_pr_routes(&list, "[]"));
        let page = forge(&s)
            .open_prs(&repo(), Section::ReviewedBy, 25, None)
            .unwrap();
        assert!(page.prs.is_empty());
        assert!(
            s.requests()
                .iter()
                .any(|r| r.path == format!("{list}&per_page=25&page=1"))
        );

        let s = stub::serve(open_pr_routes(
            &list,
            r#"[{"user":{"id":1,"username":"me"},"approved_at":"2026-01-01T00:00:00Z"}]"#,
        ));
        let page = forge(&s)
            .open_prs(&repo(), Section::ReviewedBy, 25, None)
            .unwrap();
        assert_eq!(page.prs[0].reviews[0].author, "me");
        assert_eq!(page.prs[0].reviews[0].state, ReviewState::Approved);
    }

    #[test]
    fn the_timeline_pages_until_max_newest_first() {
        let list = format!("{P}/merge_requests?state=all&author_username=me");
        let mr = |iid: u64| {
            mr_json(
                iid,
                json!({"created_at": format!("2026-01-0{iid}T00:00:00Z"),
                       "merged_at": if iid == 1 { json!("2026-02-01T00:00:00Z") } else { Value::Null }}),
            )
        };
        let s = stub::serve(vec![
            Route::new("GET", "/api/v4/user", 200, r#"{"id":1,"username":"me"}"#),
            Route::new(
                "GET",
                &format!("{list}&order_by=created_at&sort=desc&per_page=3&page=2"),
                200,
                &format!("[{}]", mr(3)),
            ),
            Route::new("GET", &list, 200, &format!("[{},{}]", mr(1), mr(2)))
                .header("X-Next-Page", "2"),
        ]);
        let got = forge(&s).timeline(&repo(), Role::Author, 3).unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].merged_at.as_deref(), Some("2026-02-01T00:00:00Z"));
        assert_eq!(got[2].created_at.as_deref(), Some("2026-01-03T00:00:00Z"));
        assert_eq!((got[0].additions, got[0].deletions), (0, 0));
        let paths: Vec<String> = s.requests().into_iter().map(|r| r.path).collect();
        assert!(
            paths.contains(&format!(
                "{list}&order_by=created_at&sort=desc&per_page=3&page=1"
            )),
            "{paths:?}"
        );
    }
}
