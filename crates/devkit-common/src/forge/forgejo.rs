//! Forgejo as a forge: Codeberg, a self-hosted Forgejo, or Gitea, which
//! shares its v1 REST API.
//!
//! Everything goes over the API with a token from `FORGEJO_TOKEN`, falling
//! back to `GITEA_TOKEN`. Forgejo keeps no draft flag: a draft is a PR whose
//! title starts with a work-in-progress prefix, so opening a draft and marking
//! one ready both edit the title.

use std::{collections::HashMap, path::Path, sync::OnceLock};

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    CheckRun, CheckState, Checks, Forge, ForgeKind, HeadLookup, NewPr, OpenPr, OpenPrPage, PrBrief,
    PrLocator, PrTimeline, Repo, Review, ReviewState, Reviewers, Role, Section, locate_on, remote,
    rest::{Method, Rest, encode},
};

/// Written in front of a draft's title. Forgejo's default
/// `WORK_IN_PROGRESS_PREFIXES` is `WIP:,[WIP]`, matched case-insensitively.
const WIP_WRITE: &str = "WIP: ";
const WIP_PREFIXES: [&str; 2] = ["WIP:", "[WIP]"];

/// Forgejo's default `MAX_RESPONSE_ITEMS`, the largest page it serves.
const PAGE: u32 = 50;

/// The pull list pages a head lookup reads before giving up. Only Forgejo 16
/// and later honor `?head=`; older Forgejo and Gitea ignore it and return
/// every PR, so the branch is matched client-side.
const HEAD_SCAN_PAGES: u32 = 20;

/// The review list pages read for one PR. The list carries no `Link` header,
/// so paging stops on an empty page or here.
const REVIEW_PAGES: u32 = 20;

const TOKEN_HINT: &str = "set FORGEJO_TOKEN (or GITEA_TOKEN)";

pub struct ForgejoForge {
    host: String,
    rest: Rest,
    has_token: bool,
    viewer: OnceLock<String>,
}

impl ForgejoForge {
    pub fn new(host: &str) -> ForgejoForge {
        let token = crate::secrets::resolve("FORGEJO_TOKEN")
            .or_else(|| crate::secrets::resolve("GITEA_TOKEN"));
        ForgejoForge::with_api(host, &format!("https://{host}/api/v1"), token)
    }

    /// A forge for `host` whose API lives at `base_url`, such as a test stub.
    pub fn with_api(host: &str, base_url: &str, token: Option<String>) -> ForgejoForge {
        let token = token.filter(|t| !t.is_empty());
        ForgejoForge {
            host: host.to_string(),
            has_token: token.is_some(),
            rest: Rest::new(
                base_url,
                token.map(|t| ("Authorization", format!("token {t}"))),
            ),
            viewer: OnceLock::new(),
        }
    }

    fn authed(&self) -> Result<()> {
        anyhow::ensure!(
            self.has_token,
            "no Forgejo token for {} ({TOKEN_HINT})",
            self.host
        );
        Ok(())
    }

    /// The authenticated user's login, read once.
    fn viewer(&self) -> Result<&str> {
        if let Some(v) = self.viewer.get() {
            return Ok(v);
        }
        let user = self.rest.get("/user")?;
        let login = user["login"]
            .as_str()
            .filter(|l| !l.is_empty())
            .context("no login in the Forgejo /user response")?
            .to_string();
        Ok(self.viewer.get_or_init(|| login))
    }

    /// Every PR in `repo` whose head is one of `branches`, keyed by branch,
    /// from one scan of the pull list. `Err` is why the scan could not
    /// finish, which a caller must not read as "no PR".
    fn scan_heads(
        &self,
        repo: &Repo,
        branches: &[String],
    ) -> std::result::Result<HashMap<String, Vec<PrBrief>>, String> {
        let mut found: HashMap<String, Vec<PrBrief>> =
            branches.iter().map(|b| (b.clone(), Vec::new())).collect();
        let head = match branches {
            [one] => format!("&head={}", encode(one)),
            _ => String::new(),
        };
        let mut page = 1;
        for _ in 0..HEAD_SCAN_PAGES {
            let path = format!(
                "/repos/{}/pulls?state=all&limit={PAGE}&page={page}{head}",
                repo.slug
            );
            let resp = self.rest.get_page(&path).map_err(|e| format!("{e:#}"))?;
            for pr in parse_pull_list(&resp.body).map_err(|e| format!("{e:#}"))? {
                if let Some(list) = found.get_mut(&pr.head_ref_name) {
                    list.push(pr);
                }
            }
            match resp.next {
                Some(n) => page = n,
                None => return Ok(found),
            }
        }
        Err(format!(
            "read {HEAD_SCAN_PAGES} pages of pull requests in {} without reaching the end, so \
             the branch's PR could be further back",
            repo.slug
        ))
    }

    fn review_rows(&self, repo: &Repo, n: u64) -> Result<Vec<ReviewRow>> {
        let mut rows = Vec::new();
        for page in 1..=REVIEW_PAGES {
            let body = self.rest.get(&format!(
                "/repos/{}/pulls/{n}/reviews?page={page}&limit={PAGE}",
                repo.slug
            ))?;
            let batch = parse_review_rows(&body)
                .with_context(|| format!("reading the reviews of #{n} in {}", repo.slug))?;
            if batch.is_empty() {
                break;
            }
            rows.extend(batch);
        }
        Ok(rows)
    }

    fn open_pr(&self, repo: &Repo, n: u64) -> Result<OpenPr> {
        let pull = self.rest.get(&format!("/repos/{}/pulls/{n}", repo.slug))?;
        let reviews = self.review_rows(repo, n)?;
        let sha = str_at(&pull, "/head/sha");
        let status = if sha.is_empty() {
            None
        } else {
            Some(self.rest.get(&format!(
                "/repos/{}/commits/{sha}/status?limit={PAGE}",
                repo.slug
            ))?)
        };
        open_pr_of(&pull, &reviews, status.as_ref())
            .with_context(|| format!("reading #{n} in {}", repo.slug))
    }

    /// One page of the viewer's PRs in `repo` from the cross-repository issue
    /// search, which has no repository filter: it is scoped to the owner and
    /// the rest is dropped here.
    fn search(
        &self,
        repo: &Repo,
        state: &str,
        flag: &str,
        page: u32,
        limit: u32,
    ) -> Result<(Vec<Value>, Option<u32>)> {
        let owner = repo.slug.split('/').next().unwrap_or(&repo.slug);
        let resp = self.rest.get_page(&format!(
            "/repos/issues/search?type=pulls&state={state}&owner={}&{flag}=true&page={page}&limit={limit}",
            encode(owner)
        ))?;
        Ok((in_repo(&resp.body, &repo.slug)?, resp.next))
    }
}

// --- parsing -----------------------------------------------------------------

fn str_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

fn opt_str(v: &Value, pointer: &str) -> Option<String> {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// The WIP prefix `title` starts with, as it is spelled there.
fn wip_prefix(title: &str) -> Option<&str> {
    WIP_PREFIXES.iter().find_map(|p| {
        let head = title.get(..p.len())?;
        head.eq_ignore_ascii_case(p).then_some(head)
    })
}

/// `title` with every leading WIP prefix and the whitespace after it removed.
fn strip_wip(mut title: &str) -> &str {
    while let Some(p) = wip_prefix(title) {
        title = title[p.len()..].trim_start();
    }
    title
}

/// The instance's prefix list is configurable and unpublished, so the
/// server's `draft` wins and the default prefixes are only a fallback.
fn is_draft(pull: &Value) -> bool {
    pull["draft"]
        .as_bool()
        .unwrap_or_else(|| wip_prefix(str_at(pull, "/title")).is_some())
}

/// Forgejo reports a merged PR as `closed` with `merged: true`.
fn pr_state(pull: &Value) -> String {
    if pull["merged"].as_bool() == Some(true) {
        return "MERGED".into();
    }
    match str_at(pull, "/state") {
        "open" => "OPEN".into(),
        "closed" => "CLOSED".into(),
        other => other.to_uppercase(),
    }
}

/// One `PullRequest`. The head branch is `head.label`: `head.ref` becomes
/// `refs/pull/<n>/head` once the branch is deleted.
fn parse_pull(pull: &Value) -> Option<PrBrief> {
    Some(PrBrief {
        number: pull["number"].as_u64()?,
        state: pr_state(pull),
        url: str_at(pull, "/html_url").to_string(),
        title: str_at(pull, "/title").to_string(),
        head_ref_name: str_at(pull, "/head/label").to_string(),
        head_ref_oid: str_at(pull, "/head/sha").to_string(),
        head_repo_owner: opt_str(pull, "/head/repo/owner/login"),
        is_draft: is_draft(pull),
        author_login: opt_str(pull, "/user/login"),
    })
}

/// A body that came back and would not parse is an error, not an absent PR:
/// "there is no such PR" is what closes a worktree's finished verdict.
fn brief_of_response(body: Option<Value>, n: u64, slug: &str) -> Result<Option<PrBrief>> {
    body.map(|v| parse_pull(&v).with_context(|| format!("unexpected PR shape for #{n} in {slug}")))
        .transpose()
}

/// A page of the pull list. One unreadable entry fails the page, since
/// skipping it could hide the very PR a head lookup is looking for.
fn parse_pull_list(v: &Value) -> Result<Vec<PrBrief>> {
    v.as_array()
        .context("the pull list is not an array")?
        .iter()
        .map(|p| parse_pull(p).context("unexpected PR shape in the pull list"))
        .collect()
}

/// The issue-search hits that are PRs in `slug`, which the search cannot
/// filter on itself.
fn in_repo(v: &Value, slug: &str) -> Result<Vec<Value>> {
    Ok(v.as_array()
        .context("the issue search result is not an array")?
        .iter()
        .filter(|i| str_at(i, "/repository/full_name").eq_ignore_ascii_case(slug))
        .cloned()
        .collect())
}

fn timeline_of(hit: &Value) -> PrTimeline {
    PrTimeline {
        created_at: opt_str(hit, "/created_at"),
        merged_at: opt_str(hit, "/pull_request/merged_at"),
        additions: hit["additions"].as_i64().unwrap_or(0),
        deletions: hit["deletions"].as_i64().unwrap_or(0),
    }
}

/// The `head` a create request takes: the bare branch, or `owner:branch`
/// when `origin` is a fork of `slug` under another owner.
fn head_spec(slug: &str, branch: &str, origin: Option<&str>) -> String {
    let owner = |s: &str| s.split('/').next().unwrap_or(s).to_ascii_lowercase();
    match origin {
        Some(o) if owner(o) != owner(slug) => {
            format!("{}:{branch}", o.split('/').next().unwrap_or(o))
        }
        _ => branch.to_string(),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Login {
    login: String,
}

/// One row of a PR's review list. Review requests are rows too, with state
/// `REQUEST_REVIEW`; a team request has no `user`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewRow {
    user: Option<Login>,
    state: String,
    dismissed: bool,
    submitted_at: Option<String>,
}

impl ReviewRow {
    fn login(&self) -> Option<&str> {
        self.user
            .as_ref()
            .map(|u| u.login.as_str())
            .filter(|l| !l.is_empty())
    }
}

fn parse_review_rows(v: &Value) -> Result<Vec<ReviewRow>> {
    if v.is_null() {
        return Ok(Vec::new());
    }
    Vec::<ReviewRow>::deserialize(v).context("unexpected review list shape")
}

/// Logins whose request is still open. The PR's `requested_reviewers` keeps
/// a user after they approve, so this follows Forgejo's own rule instead: a
/// request is open while it is the user's latest non-dismissed approval,
/// change request, or request row. A comment leaves it open.
fn pending_reviewers(rows: &[ReviewRow]) -> Vec<String> {
    let mut latest: Vec<(&str, &str)> = Vec::new();
    for r in rows.iter().filter(|r| {
        !r.dismissed
            && matches!(
                r.state.as_str(),
                "APPROVED" | "REQUEST_CHANGES" | "REQUEST_REVIEW"
            )
    }) {
        let Some(login) = r.login() else { continue };
        match latest.iter_mut().find(|(l, _)| *l == login) {
            Some(slot) => slot.1 = &r.state,
            None => latest.push((login, &r.state)),
        }
    }
    latest
        .into_iter()
        .filter(|(_, s)| *s == "REQUEST_REVIEW")
        .map(|(l, _)| l.to_string())
        .collect()
}

fn submitted_reviewers(rows: &[ReviewRow]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in rows {
        if !matches!(r.state.as_str(), "APPROVED" | "REQUEST_CHANGES" | "COMMENT") {
            continue;
        }
        if let Some(l) = r.login()
            && !out.iter().any(|o| o == l)
        {
            out.push(l.to_string());
        }
    }
    out
}

/// The rows that are reviews rather than requests.
fn reviews_of(rows: &[ReviewRow]) -> Vec<Review> {
    rows.iter()
        .filter_map(|r| {
            let state = match r.state.as_str() {
                "APPROVED" | "REQUEST_CHANGES" if r.dismissed => ReviewState::Dismissed,
                "APPROVED" => ReviewState::Approved,
                "REQUEST_CHANGES" => ReviewState::ChangesRequested,
                "COMMENT" => ReviewState::Commented,
                "PENDING" => ReviewState::Pending,
                _ => return None,
            };
            Some(Review {
                author: r.login()?.to_string(),
                state,
                submitted_at: match state {
                    ReviewState::Pending => String::new(),
                    _ => r.submitted_at.clone().unwrap_or_default(),
                },
            })
        })
        .collect()
}

/// A commit status as a check state. `warning` does not block a merge, and
/// `skipped` (Forgejo 16) is a check that chose not to run.
fn status_state(s: &str) -> CheckState {
    match s {
        "success" | "warning" | "skipped" => CheckState::Passed,
        "failure" | "error" => CheckState::Failed,
        _ => CheckState::Running,
    }
}

/// A combined status. A commit with no statuses comes back as `state: ""`
/// and `statuses: null`, which is no checks rather than a pending one.
fn parse_checks(v: &Value) -> Option<Checks> {
    let statuses = v["statuses"].as_array().filter(|s| !s.is_empty())?;
    Some(Checks {
        overall: status_state(str_at(v, "/state")),
        runs: statuses
            .iter()
            .map(|s| CheckRun {
                name: str_at(s, "/context").to_string(),
                state: status_state(str_at(s, "/status")),
                started_at: opt_str(s, "/created_at"),
            })
            .collect(),
    })
}

/// A pull, its review rows and its head's combined status as one open PR.
/// `mergeable` is false for a draft and while the conflict check runs, so
/// only a non-draft's `false` is read as a conflict.
fn open_pr_of(pull: &Value, rows: &[ReviewRow], status: Option<&Value>) -> Result<OpenPr> {
    let brief = parse_pull(pull).context("unexpected PR shape")?;
    Ok(OpenPr {
        number: brief.number,
        url: brief.url,
        title: brief.title,
        head_ref_name: brief.head_ref_name,
        is_draft: brief.is_draft,
        review_decision: None,
        conflicting: !brief.is_draft && pull["mergeable"].as_bool() == Some(false),
        author: brief.author_login.unwrap_or_default(),
        checks: status.and_then(parse_checks),
        reviews: reviews_of(rows),
        review_requests: pending_reviewers(rows),
    })
}

fn section_flag(section: Section) -> &'static str {
    match section {
        Section::Mine => "created",
        Section::ReviewRequested => "review_requested",
        Section::ReviewedBy => "reviewed",
    }
}

impl Forge for ForgejoForge {
    fn kind(&self) -> ForgeKind {
        ForgeKind::Forgejo
    }

    fn host(&self) -> &str {
        &self.host
    }

    fn ready(&self) -> bool {
        self.has_token
    }

    fn check(&self) -> Result<String> {
        self.authed()?;
        Ok(format!("forgejo: {} ({})", self.viewer()?, self.host))
    }

    fn locate(&self, url: &str) -> Option<PrLocator> {
        locate_on(url, &self.host, "/pulls/")
    }

    fn pr(&self, repo: &Repo, n: u64) -> Result<Option<PrBrief>> {
        let body = self
            .rest
            .get_opt(&format!("/repos/{}/pulls/{n}", repo.slug))?;
        brief_of_response(body, n, &repo.slug)
    }

    fn pr_by_head(&self, repo: &Repo, branch: &str) -> HeadLookup {
        self.prs_by_head(repo, &[branch.to_string()])
            .remove(branch)
            .unwrap_or_else(|| HeadLookup::Unavailable(format!("no lookup for `{branch}`")))
    }

    fn prs_by_head(&self, repo: &Repo, branches: &[String]) -> HashMap<String, HeadLookup> {
        if branches.is_empty() {
            return HashMap::new();
        }
        match self.scan_heads(repo, branches) {
            Ok(found) => found
                .into_iter()
                .map(|(b, prs)| (b, HeadLookup::of(prs)))
                .collect(),
            Err(why) => branches
                .iter()
                .map(|b| (b.clone(), HeadLookup::Unavailable(why.clone())))
                .collect(),
        }
    }

    fn create(&self, repo: &Repo, pr: &NewPr<'_>, cwd: &Path) -> Result<String> {
        self.authed()?;
        let title = if pr.draft && wip_prefix(pr.title).is_none() {
            format!("{WIP_WRITE}{}", pr.title)
        } else {
            pr.title.to_string()
        };
        let origin = remote::origin_slug(&cwd.to_string_lossy(), &self.host).ok();
        let body = json!({
            "head": head_spec(&repo.slug, pr.head, origin.as_deref()),
            "base": pr.base,
            "title": title,
            "body": pr.body,
        });
        let created = self
            .rest
            .send(Method::POST, &format!("/repos/{}/pulls", repo.slug), &body)
            .with_context(|| format!("opening a pull request from {}", pr.head))?;
        created["html_url"]
            .as_str()
            .map(str::to_string)
            .context("no html_url in the created pull request")
    }

    fn mark_ready(&self, repo: &Repo, n: u64) -> Result<()> {
        self.authed()?;
        let path = format!("/repos/{}/pulls/{n}", repo.slug);
        let pull = self.rest.get(&path)?;
        let title = str_at(&pull, "/title");
        let ready = strip_wip(title);
        if ready.len() == title.len() {
            anyhow::ensure!(
                !is_draft(&pull),
                "#{n} in {} is a draft by a title prefix devkit does not know; remove it from \
                 the title on {}",
                repo.slug,
                self.host
            );
            return Ok(());
        }
        // An empty title in an edit means "leave the title alone".
        anyhow::ensure!(
            !ready.is_empty(),
            "#{n} in {}'s title is only a draft prefix; give it a title first",
            repo.slug
        );
        self.rest
            .send(Method::PATCH, &path, &json!({ "title": ready }))
            .with_context(|| format!("marking #{n} in {} ready", repo.slug))?;
        Ok(())
    }

    fn add_reviewers(&self, repo: &Repo, n: u64, logins: &[String]) -> Result<()> {
        if logins.is_empty() {
            return Ok(());
        }
        self.authed()?;
        self.rest
            .send(
                Method::POST,
                &format!("/repos/{}/pulls/{n}/requested_reviewers", repo.slug),
                &json!({ "reviewers": logins }),
            )
            .with_context(|| format!("requesting reviewers on #{n} in {}", repo.slug))?;
        Ok(())
    }

    fn reviewers(&self, repo: &Repo, n: u64) -> Result<Reviewers> {
        let rows = self.review_rows(repo, n)?;
        Ok(Reviewers {
            requested: pending_reviewers(&rows),
            submitted: submitted_reviewers(&rows),
        })
    }

    fn head_ref(&self, n: u64) -> Option<String> {
        Some(format!("refs/pull/{n}/head"))
    }

    fn checkout(&self, _repo: &Repo, pr: &PrBrief, dir: &Path) -> Result<()> {
        super::checkout_head(self, pr, dir)
    }

    fn open_prs(
        &self,
        repo: &Repo,
        section: Section,
        page_size: u32,
        cursor: Option<&str>,
    ) -> Result<OpenPrPage> {
        self.authed()?;
        let page = match cursor {
            Some(c) => c
                .parse()
                .with_context(|| format!("`{c}` is not a Forgejo page number"))?,
            None => 1,
        };
        let viewer = self.viewer()?.to_string();
        let (hits, next) = self.search(
            repo,
            "open",
            section_flag(section),
            page,
            page_size.clamp(1, PAGE),
        )?;
        let numbers: Vec<u64> = hits.iter().filter_map(|h| h["number"].as_u64()).collect();
        let prs = crate::pool::install(|| {
            numbers
                .par_iter()
                .map(|n| self.open_pr(repo, *n))
                .collect::<Result<Vec<_>>>()
        })?;
        Ok(OpenPrPage {
            viewer,
            prs,
            next: next.map(|n| n.to_string()),
        })
    }

    /// The issue search carries no line counts, so they stay zero.
    fn timeline(&self, repo: &Repo, role: Role, max: usize) -> Result<Vec<PrTimeline>> {
        self.authed()?;
        let flag = match role {
            Role::Author => "created",
            Role::Reviewer => "reviewed",
        };
        let mut out = Vec::new();
        let mut page = 1;
        while out.len() < max {
            let (hits, next) = self.search(repo, "all", flag, page, PAGE)?;
            out.extend(hits.iter().map(timeline_of));
            match next {
                Some(n) => page = n,
                None => break,
            }
        }
        out.truncate(max);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::stub::{self, Route, Stub};

    fn repo() -> Repo {
        Repo {
            host: "codeberg.org".into(),
            slug: "o/r".into(),
        }
    }

    fn forge(s: &Stub) -> ForgejoForge {
        ForgejoForge::with_api(
            "codeberg.org",
            &format!("{}/api/v1", s.url()),
            Some("t0k".into()),
        )
    }

    fn body(r: &stub::Request) -> Value {
        serde_json::from_str(&r.body).unwrap()
    }

    /// A merged PR whose branch was deleted, trimmed from codeberg.org's
    /// `forgejo/forgejo#14563`.
    const MERGED_PULL: &str = r#"{
        "number": 14563, "state": "closed", "merged": true,
        "merged_at": "2026-09-27T11:32:56+02:00",
        "html_url": "https://codeberg.org/forgejo/forgejo/pulls/14563",
        "title": "[v17.0/forgejo] fix: something", "draft": false, "mergeable": true,
        "user": { "login": "forgejo-backport-action" },
        "head": {
            "label": "bp-v17.0/forgejo-e0d8002", "ref": "refs/pull/14563/head",
            "sha": "6943c9f0aa", "repo": { "full_name": "forgejo/forgejo", "owner": { "login": "forgejo" } }
        },
        "base": { "label": "v17.0/forgejo", "ref": "v17.0/forgejo" }
    }"#;

    fn pull(n: u64, label: &str, title: &str) -> Value {
        json!({
            "number": n, "state": "open", "merged": false,
            "html_url": format!("https://codeberg.org/o/r/pulls/{n}"),
            "title": title, "user": { "login": "me" }, "mergeable": true,
            "head": { "label": label, "ref": label, "sha": format!("sha{n}"),
                      "repo": { "owner": { "login": "fork" } } }
        })
    }

    #[test]
    fn a_pull_reads_its_branch_from_the_label_not_the_ref() {
        let b = parse_pull(&serde_json::from_str(MERGED_PULL).unwrap()).unwrap();
        assert_eq!(b.number, 14563);
        assert_eq!(b.state, "MERGED");
        assert_eq!(b.head_ref_name, "bp-v17.0/forgejo-e0d8002");
        assert_eq!(b.head_ref_oid, "6943c9f0aa");
        assert_eq!(b.head_repo_owner.as_deref(), Some("forgejo"));
        assert_eq!(b.author_login.as_deref(), Some("forgejo-backport-action"));
        assert!(!b.is_draft);
    }

    #[test]
    fn open_and_closed_map_to_their_upper_case_states() {
        assert_eq!(pr_state(&json!({"state": "open", "merged": false})), "OPEN");
        assert_eq!(
            pr_state(&json!({"state": "closed", "merged": false})),
            "CLOSED"
        );
    }

    #[test]
    fn a_draft_is_the_server_flag_or_else_a_wip_title() {
        assert!(is_draft(&json!({"title": "x", "draft": true})));
        assert!(!is_draft(&json!({"title": "DRAFT: x", "draft": false})));
        assert!(is_draft(&json!({"title": "wip: x"})));
        assert!(is_draft(&json!({"title": "[Wip] x"})));
        assert!(!is_draft(&json!({"title": "Draft: x"})));
    }

    #[test]
    fn stripping_removes_every_leading_wip_prefix_and_its_space() {
        assert_eq!(strip_wip("WIP: Fix it"), "Fix it");
        assert_eq!(strip_wip("[wip]  wip:Fix it"), "Fix it");
        assert_eq!(strip_wip("Fix WIP: it"), "Fix WIP: it");
        assert_eq!(strip_wip("WIP:"), "");
    }

    #[test]
    fn a_body_that_will_not_parse_is_not_an_absent_pr() {
        assert!(brief_of_response(None, 7, "o/r").unwrap().is_none());
        let err = brief_of_response(Some(json!({"state": "open"})), 7, "o/r")
            .unwrap_err()
            .to_string();
        assert!(err.contains('7') && err.contains("o/r"), "{err}");
    }

    #[test]
    fn a_fork_origin_qualifies_the_head_with_its_owner() {
        assert_eq!(head_spec("up/app", "feat/x", Some("me/app")), "me:feat/x");
        assert_eq!(head_spec("up/app", "feat/x", Some("Up/app")), "feat/x");
        assert_eq!(head_spec("up/app", "feat/x", None), "feat/x");
    }

    fn rows(v: Value) -> Vec<ReviewRow> {
        parse_review_rows(&v).unwrap()
    }

    fn row(login: &str, state: &str) -> Value {
        json!({"user": {"login": login}, "state": state, "dismissed": false,
               "submitted_at": "2026-09-27T10:00:00+02:00"})
    }

    #[test]
    fn a_request_stays_pending_until_the_user_approves_or_requests_changes() {
        let r = rows(json!([
            row("alice", "REQUEST_REVIEW"),
            row("bob", "REQUEST_REVIEW"),
            row("carol", "REQUEST_REVIEW"),
            row("alice", "APPROVED"),
            row("bob", "COMMENT"),
            row("carol", "REQUEST_CHANGES"),
            row("carol", "REQUEST_REVIEW"),
            {"user": null, "team": {"name": "core"}, "state": "REQUEST_REVIEW"}
        ]));
        assert_eq!(pending_reviewers(&r), vec!["bob", "carol"]);
    }

    #[test]
    fn a_dismissed_approval_does_not_close_a_request() {
        let mut approved = row("alice", "APPROVED");
        approved["dismissed"] = json!(true);
        let r = rows(json!([row("alice", "REQUEST_REVIEW"), approved]));
        assert_eq!(pending_reviewers(&r), vec!["alice"]);
    }

    #[test]
    fn submitted_reviewers_are_each_login_once_and_skip_requests_and_drafts() {
        let r = rows(json!([
            row("alice", "COMMENT"),
            row("alice", "APPROVED"),
            row("bob", "REQUEST_REVIEW"),
            row("carol", "PENDING"),
            row("dave", "REQUEST_CHANGES")
        ]));
        assert_eq!(submitted_reviewers(&r), vec!["alice", "dave"]);
    }

    #[test]
    fn review_rows_map_onto_review_states_and_requests_are_left_out() {
        let mut dismissed = row("dave", "REQUEST_CHANGES");
        dismissed["dismissed"] = json!(true);
        let r = rows(json!([
            row("alice", "APPROVED"),
            row("bob", "REQUEST_REVIEW"),
            row("carol", "PENDING"),
            dismissed,
            row("erin", "COMMENT")
        ]));
        let got = reviews_of(&r);
        assert_eq!(
            got.iter()
                .map(|r| (r.author.as_str(), r.state))
                .collect::<Vec<_>>(),
            vec![
                ("alice", ReviewState::Approved),
                ("carol", ReviewState::Pending),
                ("dave", ReviewState::Dismissed),
                ("erin", ReviewState::Commented),
            ]
        );
        assert_eq!(
            got[1].submitted_at, "",
            "a pending review has no submission time"
        );
        assert!(!got[0].submitted_at.is_empty());
    }

    #[test]
    fn a_combined_status_maps_each_context_to_a_check() {
        let v = json!({
            "state": "failure", "sha": "abc", "total_count": 3,
            "statuses": [
                {"context": "ci/build", "status": "success", "created_at": "2026-09-27T10:00:00+02:00"},
                {"context": "ci/test", "status": "error"},
                {"context": "ci/lint", "status": "pending"}
            ]
        });
        let c = parse_checks(&v).unwrap();
        assert_eq!(c.overall, CheckState::Failed);
        let runs: Vec<(&str, CheckState)> =
            c.runs.iter().map(|r| (r.name.as_str(), r.state)).collect();
        assert_eq!(runs, vec![
            ("ci/build", CheckState::Passed),
            ("ci/test", CheckState::Failed),
            ("ci/lint", CheckState::Running),
        ]);
        assert_eq!(status_state("skipped"), CheckState::Passed);
    }

    #[test]
    fn a_commit_with_no_statuses_has_no_checks() {
        let v = json!({"state": "", "sha": "", "total_count": 0, "statuses": null});
        assert!(parse_checks(&v).is_none());
    }

    #[test]
    fn only_a_non_draft_that_will_not_merge_is_conflicting() {
        let mut p = pull(7, "feat/x", "Fix");
        p["mergeable"] = json!(false);
        let pr = open_pr_of(&p, &[], None).unwrap();
        assert!(pr.conflicting);
        assert!(pr.checks.is_none());
        assert_eq!(pr.review_decision, None);

        let mut d = pull(8, "feat/y", "WIP: Fix");
        d["mergeable"] = json!(false);
        let pr = open_pr_of(&d, &[], None).unwrap();
        assert!(pr.is_draft);
        assert!(!pr.conflicting);
    }

    #[test]
    fn search_hits_outside_the_repository_are_dropped() {
        let v = json!([
            {"number": 1, "repository": {"full_name": "O/R"}},
            {"number": 2, "repository": {"full_name": "o/other"}}
        ]);
        let hits = in_repo(&v, "o/r").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["number"], 1);
    }

    // --- over the wire -------------------------------------------------------

    #[test]
    fn check_without_a_token_names_the_variable_and_sends_nothing() {
        let s = stub::serve(vec![]);
        let f = ForgejoForge::with_api("codeberg.org", &format!("{}/api/v1", s.url()), None);
        assert!(!f.ready());
        let err = f.check().unwrap_err().to_string();
        assert!(err.contains("FORGEJO_TOKEN"), "{err}");
        assert!(s.requests().is_empty());
    }

    #[test]
    fn check_reports_the_login_and_sends_the_token_header() {
        let s = stub::serve(vec![Route::new(
            "GET",
            "/api/v1/user",
            200,
            r#"{"login":"me"}"#,
        )]);
        let f = forge(&s);
        assert!(f.ready());
        assert_eq!(f.check().unwrap(), "forgejo: me (codeberg.org)");
        assert_eq!(s.requests()[0].header("Authorization"), Some("token t0k"));
    }

    #[test]
    fn a_missing_pr_is_none_and_a_present_one_parses() {
        let s = stub::serve(vec![Route::new(
            "GET",
            "/api/v1/repos/o/r/pulls/14563",
            200,
            MERGED_PULL,
        )]);
        let f = forge(&s);
        assert!(f.pr(&repo(), 9).unwrap().is_none());
        assert_eq!(f.pr(&repo(), 14563).unwrap().unwrap().state, "MERGED");
    }

    #[test]
    fn a_head_lookup_pages_the_pull_list_and_matches_the_label() {
        let page1 = json!([pull(1, "feat/x", "a"), pull(2, "other", "b")]).to_string();
        let page2 = json!([pull(3, "feat/x", "c")]).to_string();
        let s = stub::serve(vec![
            Route::new(
                "GET",
                "/api/v1/repos/o/r/pulls?state=all&limit=50&page=2",
                200,
                &page2,
            ),
            Route::new(
                "GET",
                "/api/v1/repos/o/r/pulls?state=all&limit=50&page=1",
                200,
                &page1,
            )
            .header(
                "Link",
                r#"<http://x/api/v1/repos/o/r/pulls?page=2>; rel="next""#,
            ),
        ]);
        let got = forge(&s).pr_by_head(&repo(), "feat/x");
        let HeadLookup::Ambiguous(prs) = got else {
            panic!("expected Ambiguous, got {got:?}")
        };
        assert_eq!(prs.iter().map(|p| p.number).collect::<Vec<_>>(), vec![1, 3]);
        let reqs = s.requests();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[0].path.ends_with("&head=feat%2Fx"), "{}", reqs[0].path);
    }

    #[test]
    fn many_heads_share_one_scan_of_the_pull_list() {
        let page = json!([pull(1, "a", "a"), pull(2, "b", "b")]).to_string();
        let s = stub::serve(vec![Route::new(
            "GET",
            "/api/v1/repos/o/r/pulls?",
            200,
            &page,
        )]);
        let got = forge(&s).prs_by_head(&repo(), &["a".into(), "b".into(), "c".into()]);
        assert!(matches!(&got["a"], HeadLookup::Unique(p) if p.number == 1));
        assert!(matches!(&got["b"], HeadLookup::Unique(p) if p.number == 2));
        assert!(matches!(got["c"], HeadLookup::NoMatch));
        assert_eq!(s.requests().len(), 1);
        assert!(!s.requests()[0].path.contains("head="));
    }

    /// `issue end` deletes a branch on `NoMatch`, so a list that never ends
    /// is an unanswered lookup, not an empty one.
    #[test]
    fn a_head_lookup_that_hits_the_page_cap_is_unavailable() {
        let s = stub::serve(vec![
            Route::new("GET", "/api/v1/repos/o/r/pulls?", 200, "[]").header(
                "Link",
                r#"<http://x/api/v1/repos/o/r/pulls?page=2>; rel="next""#,
            ),
        ]);
        let got = forge(&s).pr_by_head(&repo(), "feat/x");
        assert!(
            matches!(&got, HeadLookup::Unavailable(r) if r.contains("pages")),
            "{got:?}"
        );
        assert_eq!(s.requests().len(), HEAD_SCAN_PAGES as usize);
    }

    #[test]
    fn a_failed_head_lookup_is_unavailable() {
        let s = stub::serve(vec![Route::new(
            "GET",
            "/api/v1/repos/o/r/pulls?",
            500,
            "{}",
        )]);
        assert!(matches!(
            forge(&s).pr_by_head(&repo(), "feat/x"),
            HeadLookup::Unavailable(_)
        ));
    }

    #[test]
    fn create_opens_a_wip_titled_pr() {
        let s = stub::serve(vec![Route::new(
            "POST",
            "/api/v1/repos/o/r/pulls",
            201,
            r#"{"number":12,"html_url":"https://codeberg.org/o/r/pulls/12"}"#,
        )]);
        let dir = tempfile::tempdir().unwrap();
        let url = forge(&s)
            .create(
                &repo(),
                &NewPr {
                    base: "main",
                    head: "feat/x",
                    title: "Fix it",
                    body: "why",
                    draft: true,
                    attachments: &[],
                },
                dir.path(),
            )
            .unwrap();
        assert_eq!(url, "https://codeberg.org/o/r/pulls/12");
        let reqs = s.requests();
        assert_eq!(
            body(&reqs[0]),
            json!({"head": "feat/x", "base": "main", "title": "WIP: Fix it", "body": "why"})
        );
        assert_eq!(reqs.len(), 1, "{reqs:#?}");
    }

    #[test]
    fn mark_ready_patches_the_title_without_its_prefix() {
        let s = stub::serve(vec![
            Route::new(
                "GET",
                "/api/v1/repos/o/r/pulls/5",
                200,
                &pull(5, "feat/x", "[wip] Fix it").to_string(),
            ),
            Route::new("PATCH", "/api/v1/repos/o/r/pulls/5", 201, "{}"),
        ]);
        forge(&s).mark_ready(&repo(), 5).unwrap();
        let patch = s
            .requests()
            .into_iter()
            .find(|r| r.method == "PATCH")
            .unwrap();
        assert_eq!(body(&patch), json!({"title": "Fix it"}));
    }

    #[test]
    fn mark_ready_on_a_pr_that_is_not_a_draft_sends_no_edit() {
        let s = stub::serve(vec![Route::new(
            "GET",
            "/api/v1/repos/o/r/pulls/5",
            200,
            &pull(5, "feat/x", "Fix it").to_string(),
        )]);
        forge(&s).mark_ready(&repo(), 5).unwrap();
        assert!(s.requests().iter().all(|r| r.method == "GET"));
    }

    #[test]
    fn reviewers_page_the_review_list_until_an_empty_page() {
        let page1 = json!([row("alice", "REQUEST_REVIEW"), row("bob", "APPROVED")]).to_string();
        let s = stub::serve(vec![
            Route::new(
                "GET",
                "/api/v1/repos/o/r/pulls/5/reviews?page=1&",
                200,
                &page1,
            ),
            Route::new(
                "GET",
                "/api/v1/repos/o/r/pulls/5/reviews?page=2&",
                200,
                "[]",
            ),
        ]);
        let got = forge(&s).reviewers(&repo(), 5).unwrap();
        assert_eq!(got.requested, vec!["alice"]);
        assert_eq!(got.submitted, vec!["bob"]);
        assert_eq!(s.requests().len(), 2);
    }

    #[test]
    fn open_prs_search_the_owner_and_read_each_pr_in_the_repository() {
        let hits = json!([
            {"number": 7, "repository": {"full_name": "o/r"}},
            {"number": 9, "repository": {"full_name": "o/elsewhere"}}
        ])
        .to_string();
        let mut p = pull(7, "feat/x", "Fix");
        p["head"]["sha"] = json!("abc");
        let reviews = json!([row("alice", "REQUEST_CHANGES")]).to_string();
        let status =
            json!({"state": "success", "statuses": [{"context": "ci", "status": "success"}]})
                .to_string();
        let s = stub::serve(vec![
            Route::new("GET", "/api/v1/user", 200, r#"{"login":"me"}"#),
            Route::new("GET", "/api/v1/repos/issues/search", 200, &hits).header(
                "Link",
                r#"<http://x/api/v1/repos/issues/search?page=3>; rel="next""#,
            ),
            Route::new(
                "GET",
                "/api/v1/repos/o/r/pulls/7/reviews?page=1&",
                200,
                &reviews,
            ),
            Route::new("GET", "/api/v1/repos/o/r/pulls/7/reviews", 200, "[]"),
            Route::new("GET", "/api/v1/repos/o/r/pulls/7", 200, &p.to_string()),
            Route::new("GET", "/api/v1/repos/o/r/commits/abc/status", 200, &status),
        ]);
        let page = forge(&s)
            .open_prs(&repo(), Section::ReviewRequested, 25, Some("2"))
            .unwrap();
        assert_eq!(page.viewer, "me");
        assert_eq!(page.next.as_deref(), Some("3"));
        assert_eq!(page.prs.len(), 1);
        let pr = &page.prs[0];
        assert_eq!(pr.head_ref_name, "feat/x");
        assert_eq!(pr.reviews[0].state, ReviewState::ChangesRequested);
        assert_eq!(pr.checks.as_ref().unwrap().overall, CheckState::Passed);

        let reqs = s.requests();
        let search = reqs.iter().find(|r| r.path.contains("/search")).unwrap();
        for part in [
            "type=pulls",
            "state=open",
            "owner=o",
            "review_requested=true",
            "page=2",
            "limit=25",
        ] {
            assert!(
                search.path.contains(part),
                "{part} missing: {}",
                search.path
            );
        }
        assert!(reqs.iter().all(|r| !r.path.contains("/pulls/9")));
    }

    #[test]
    fn a_timeline_reads_every_state_and_the_merge_time() {
        let hits = json!([
            {"number": 1, "repository": {"full_name": "o/r"},
             "created_at": "2026-09-01T10:00:00+02:00",
             "pull_request": {"merged": true, "merged_at": "2026-09-02T10:00:00+02:00"}},
            {"number": 2, "repository": {"full_name": "o/r"},
             "created_at": "2026-09-03T10:00:00+02:00",
             "pull_request": {"merged": false, "merged_at": null}}
        ])
        .to_string();
        let s = stub::serve(vec![Route::new(
            "GET",
            "/api/v1/repos/issues/search",
            200,
            &hits,
        )]);
        let got = forge(&s).timeline(&repo(), Role::Reviewer, 10).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].merged_at.as_deref(),
            Some("2026-09-02T10:00:00+02:00")
        );
        assert_eq!(got[1].merged_at, None);
        assert_eq!(got[1].additions, 0);
        let path = &s.requests()[0].path;
        assert!(
            path.contains("state=all") && path.contains("reviewed=true"),
            "{path}"
        );
    }

    #[test]
    fn a_pr_url_is_located_on_the_forge_host_only() {
        let f = ForgejoForge::with_api("git.acme.test", "http://unused", None);
        let loc = f.locate("https://git.acme.test/o/r/pulls/7/files").unwrap();
        assert_eq!(loc.repo.as_deref(), Some("o/r"));
        assert_eq!(loc.number, 7);
        assert!(f.locate("https://codeberg.org/o/r/pulls/7").is_none());
    }
}
