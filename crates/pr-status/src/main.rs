use anyhow::Result;
use clap::Parser;
use devkit_common::cmd::gh_json as gh_json_cwd;
use devkit_common::{paths, ui};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// gh JSON shapes ----------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct Check {
    conclusion: Option<String>,
    status: Option<String>,
    state: Option<String>,
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

#[derive(Deserialize)]
struct Review {
    author: Author,
    #[serde(default)]
    state: String,
    #[serde(rename = "submittedAt", default)]
    submitted_at: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewRequest {
    login: String,
}

#[derive(Deserialize)]
struct MinePr {
    number: u64,
    url: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "headRefName", default)]
    head_ref_name: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "reviewDecision", default)]
    review_decision: Option<String>,
    #[serde(default)]
    mergeable: String,
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Vec<Check>,
    #[serde(default)]
    reviews: Vec<Review>,
}

#[derive(Deserialize)]
struct ReviewPr {
    number: u64,
    url: String,
    author: Author,
    #[serde(rename = "latestReviews", default)]
    latest_reviews: Vec<Review>,
    #[serde(rename = "reviewRequests", default)]
    review_requests: Vec<ReviewRequest>,
}

// pure logic --------------------------------------------------------------------

const BOTS: [&str; 3] = ["greptile-apps", "linear-code", "coderabbitai"];
const FAIL: [&str; 4] = ["FAILURE", "ERROR", "TIMED_OUT", "CANCELLED"];
const RUNNING: [&str; 3] = ["IN_PROGRESS", "QUEUED", "PENDING"];

fn is_bot(login: &str) -> bool {
    BOTS.contains(&login) || login.ends_with("[bot]")
}

/// First `SWE-<digits>` (case-insensitive) in title+head, uppercased; else "-".
fn issue_of(title: &str, head: &str) -> String {
    let hay = format!("{title} {head}");
    let lower = hay.to_lowercase();
    let bytes = lower.as_bytes();
    let mut i = 0;
    while let Some(pos) = lower[i..].find("swe-") {
        let start = i + pos;
        let ds = start + 4;
        let mut j = ds;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > ds {
            return format!("SWE-{}", &hay[ds..j]);
        }
        i = start + 4;
    }
    "-".to_string()
}

fn checks_of(rollup: &[Check]) -> &'static str {
    if rollup.is_empty() {
        return "-";
    }
    let fail = rollup.iter().any(|c| {
        c.conclusion.as_deref().is_some_and(|x| FAIL.contains(&x))
            || matches!(c.state.as_deref(), Some("FAILURE") | Some("ERROR"))
    });
    if fail {
        return "fail";
    }
    let run = rollup.iter().any(|c| {
        c.status.as_deref().is_some_and(|x| RUNNING.contains(&x))
            || c.state.as_deref() == Some("PENDING")
    });
    if run {
        return "run";
    }
    "ok"
}

fn review_text(pr: &MinePr) -> &'static str {
    match pr.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => "changes",
        Some("APPROVED") => "approved",
        Some("REVIEW_REQUIRED") => "awaiting",
        _ => {
            if pr.reviews.is_empty() {
                "awaiting"
            } else {
                "commented"
            }
        }
    }
}

/// True when my latest review is newer than the latest non-bot reviewer's.
fn has_replied(pr: &MinePr, me: &str) -> bool {
    let mine = pr
        .reviews
        .iter()
        .filter(|r| r.author.login == me)
        .map(|r| r.submitted_at.as_str())
        .max()
        .unwrap_or("");
    let theirs = pr
        .reviews
        .iter()
        .filter(|r| r.author.login != me && !is_bot(&r.author.login))
        .map(|r| r.submitted_at.as_str())
        .max()
        .unwrap_or("");
    !mine.is_empty() && mine > theirs
}

fn mine_action(pr: &MinePr, me: &str) -> String {
    if pr.is_draft {
        return "draft".into();
    }
    let conflict = pr.mergeable == "CONFLICTING";
    match pr.review_decision.as_deref() {
        Some("CHANGES_REQUESTED") => {
            let base = if has_replied(pr, me) {
                "replied; await re-review"
            } else {
                "address changes"
            };
            format!("{base}{}", if conflict { " + rebase" } else { "" })
        }
        Some("APPROVED") => {
            if conflict {
                "rebase -> merge".into()
            } else if checks_of(&pr.status_check_rollup) == "fail" {
                "fix CI -> merge".into()
            } else {
                "MERGE".into()
            }
        }
        _ => format!(
            "awaiting review{}",
            if conflict { "; rebase" } else { "" }
        ),
    }
}

/// (my_vote, action) for a PR where I'm a reviewer.
fn reviewer_state(pr: &ReviewPr, me: &str) -> (String, String) {
    let vote = pr
        .latest_reviews
        .iter()
        .filter(|r| r.author.login == me)
        .map(|r| r.state.clone())
        .next_back()
        .unwrap_or_default();
    let requested = pr.review_requests.iter().any(|req| req.login == me);
    let vote_label = match vote.as_str() {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "changes",
        "COMMENTED" => "commented",
        _ => "-",
    }
    .to_string();
    let action = if requested {
        "REVIEW NEEDED"
    } else {
        match vote.as_str() {
            "APPROVED" => "done (approved)",
            "CHANGES_REQUESTED" => "awaiting author fixes",
            "COMMENTED" => "commented; decide",
            _ => "REVIEW NEEDED",
        }
    }
    .to_string();
    (vote_label, action)
}

fn diff_cell(prev: Option<&str>, cur: &str) -> String {
    match prev {
        Some(p) if p != cur => format!("{p} → {cur}"),
        _ => cur.to_string(),
    }
}

// diff cache --------------------------------------------------------------------

type Snap = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

fn cache_path(repo: &str) -> PathBuf {
    paths::cache_dir()
        .join("pr-status")
        .join(format!("{}.json", repo.replace('/', "_")))
}
fn load_cache(path: &Path) -> Snap {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
fn save_cache(path: &Path, data: &Snap) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(data)?)?;
    Ok(())
}

// gh wrappers -------------------------------------------------------------------

fn gh_json<T: serde::de::DeserializeOwned>(args: &[&str]) -> Result<T> {
    gh_json_cwd(args, ".")
}

#[derive(Deserialize)]
struct Me {
    login: String,
}
#[derive(Deserialize)]
struct RepoInfo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

fn resolve_repo(repo: Option<&str>) -> Result<String> {
    if let Some(r) = repo {
        return Ok(r.to_string());
    }
    let info: RepoInfo = gh_json(&["repo", "view", "--json", "nameWithOwner"])?;
    Ok(info.name_with_owner)
}

fn fetch_mine(repo: Option<&str>) -> Result<Vec<MinePr>> {
    let mut args: Vec<String> = vec!["pr".into(), "list".into()];
    if let Some(r) = repo {
        args.push("--repo".into());
        args.push(r.into());
    }
    for a in [
        "--author",
        "@me",
        "--state",
        "open",
        "--limit",
        "100",
        "--json",
        "number,url,title,headRefName,isDraft,reviewDecision,mergeable,statusCheckRollup,reviews",
    ] {
        args.push(a.into());
    }
    let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    gh_json_cwd(&refs, ".")
}

fn fetch_reviews(repo: Option<&str>, me: &str) -> Result<Vec<ReviewPr>> {
    let fields = "number,url,title,headRefName,author,latestReviews,reviewRequests";
    let mut seen: BTreeMap<u64, ReviewPr> = BTreeMap::new();
    for search in ["review-requested:@me", "reviewed-by:@me"] {
        let mut args: Vec<String> = vec!["pr".into(), "list".into()];
        if let Some(r) = repo {
            args.push("--repo".into());
            args.push(r.into());
        }
        for a in ["--state", "open", "--limit", "100", "--search", search, "--json", fields] {
            args.push(a.into());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let batch: Vec<ReviewPr> = gh_json_cwd(&refs, ".")?;
        for pr in batch {
            seen.entry(pr.number).or_insert(pr);
        }
    }
    Ok(seen
        .into_values()
        .filter(|pr| pr.author.login != me)
        .collect())
}

// rendering ---------------------------------------------------------------------

fn issue_cell(issue_id: &str, url_key: Option<&str>) -> String {
    if issue_id == "-" {
        return "-".to_string();
    }
    match url_key {
        Some(k) => ui::link(issue_id, &format!("https://linear.app/{k}/issue/{issue_id}")),
        None => issue_id.to_string(),
    }
}

fn mine_table(
    me: &str,
    prs: &[MinePr],
    url_key: Option<&str>,
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    println!("MY OPEN PRs");
    let mut cur = BTreeMap::new();
    if prs.is_empty() {
        println!("  (none)");
        return cur;
    }
    let mut t = ui::table(&["PR", "ISSUE", "REVIEW", "CHECK", "ACTION"]);
    for pr in prs {
        let review = review_text(pr).to_string();
        let check = checks_of(&pr.status_check_rollup).to_string();
        let action = mine_action(pr, me);
        let was = prev.get(&pr.number.to_string());
        let g = |k: &str| was.and_then(|m| m.get(k)).map(|s| s.as_str());
        t.add_row(vec![
            ui::link(&format!("#{}", pr.number), &pr.url),
            issue_cell(&issue_of(&pr.title, &pr.head_ref_name), url_key),
            diff_cell(g("review"), &review),
            diff_cell(g("check"), &check),
            diff_cell(g("action"), &action),
        ]);
        cur.insert(
            pr.number.to_string(),
            BTreeMap::from([
                ("review".to_string(), review),
                ("check".to_string(), check),
                ("action".to_string(), action),
            ]),
        );
    }
    println!("{t}");
    cur
}

fn reviews_table(
    me: &str,
    rows: &[ReviewPr],
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    println!("\nPRs AWAITING MY REVIEW");
    let mut cur = BTreeMap::new();
    if rows.is_empty() {
        println!("  (none)");
        return cur;
    }
    let mut sorted: Vec<&ReviewPr> = rows.iter().collect();
    sorted.sort_by_key(|p| p.number);
    let mut t = ui::table(&["PR", "AUTHOR", "MY VOTE", "ACTION"]);
    for pr in sorted {
        let (vote, action) = reviewer_state(pr, me);
        let was = prev.get(&pr.number.to_string());
        let g = |k: &str| was.and_then(|m| m.get(k)).map(|s| s.as_str());
        t.add_row(vec![
            ui::link(&format!("#{}", pr.number), &pr.url),
            pr.author.login.clone(),
            diff_cell(g("vote"), &vote),
            diff_cell(g("action"), &action),
        ]);
        cur.insert(
            pr.number.to_string(),
            BTreeMap::from([
                ("vote".to_string(), vote),
                ("action".to_string(), action),
            ]),
        );
    }
    println!("{t}");
    cur
}

// CLI ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(about = "At-a-glance triage of your GitHub PRs via gh")]
struct Cli {
    /// only the "my PRs" table
    #[arg(short = 'm', long)]
    mine: bool,
    /// only the "reviewing" table
    #[arg(short = 'r', long)]
    reviews: bool,
    /// target a specific repo (owner/repo)
    #[arg(short = 'R', long)]
    repo: Option<String>,
    /// don't read or write the diff cache
    #[arg(long = "no-cache")]
    no_cache: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let want_mine = cli.mine || !cli.reviews;
    let want_reviews = cli.reviews || !cli.mine;

    let me: Me = gh_json(&["api", "user"])?;
    let me = me.login;

    let repo_key = if cli.no_cache {
        None
    } else {
        Some(resolve_repo(cli.repo.as_deref())?)
    };
    let url_key = std::env::var("LINEAR_WORKSPACE").ok();

    let mine_prs = if want_mine {
        fetch_mine(cli.repo.as_deref())?
    } else {
        vec![]
    };
    let review_rows = if want_reviews {
        fetch_reviews(cli.repo.as_deref(), &me)?
    } else {
        vec![]
    };

    let path = repo_key.as_ref().map(|r| cache_path(r));
    let mut cache: Snap = path.as_deref().map(load_cache).unwrap_or_default();

    if want_mine {
        let prev = cache.get("mine").cloned().unwrap_or_default();
        let cur = mine_table(&me, &mine_prs, url_key.as_deref(), &prev);
        cache.insert("mine".to_string(), cur);
    }
    if want_reviews {
        let prev = cache.get("reviews").cloned().unwrap_or_default();
        let cur = reviews_table(&me, &review_rows, &prev);
        cache.insert("reviews".to_string(), cur);
    }

    if (want_mine && !mine_prs.is_empty()) || (want_reviews && !review_rows.is_empty()) {
        println!("\nACTION colour key: needs you (REVIEW NEEDED · address changes · fix CI) · ready to land (MERGE · done) · waiting on author · passive (awaiting review · draft)");
        println!("old → new in a cell = value changed since the last run.");
    }

    if let Some(p) = &path {
        save_cache(p, &cache)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mine(decision: Option<&str>, mergeable: &str, draft: bool, checks: Vec<Check>) -> MinePr {
        MinePr {
            number: 1,
            url: "u".into(),
            title: "t".into(),
            head_ref_name: "h".into(),
            is_draft: draft,
            review_decision: decision.map(String::from),
            mergeable: mergeable.into(),
            status_check_rollup: checks,
            reviews: vec![],
        }
    }
    fn check(conclusion: Option<&str>, status: Option<&str>) -> Check {
        Check {
            conclusion: conclusion.map(String::from),
            status: status.map(String::from),
            state: None,
        }
    }
    #[test]
    fn checks_fail_run_ok_empty() {
        assert_eq!(checks_of(&[]), "-");
        assert_eq!(checks_of(&[check(Some("SUCCESS"), Some("COMPLETED"))]), "ok");
        assert_eq!(checks_of(&[check(Some("FAILURE"), Some("COMPLETED"))]), "fail");
        assert_eq!(checks_of(&[check(None, Some("IN_PROGRESS"))]), "run");
    }
    #[test]
    fn approved_green_merges() {
        assert_eq!(
            mine_action(
                &mine(Some("APPROVED"), "MERGEABLE", false, vec![check(Some("SUCCESS"), None)]),
                "me"
            ),
            "MERGE"
        );
    }
    #[test]
    fn approved_with_failing_ci() {
        assert_eq!(
            mine_action(
                &mine(Some("APPROVED"), "MERGEABLE", false, vec![check(Some("FAILURE"), None)]),
                "me"
            ),
            "fix CI -> merge"
        );
    }
    #[test]
    fn changes_requested_action() {
        assert_eq!(
            mine_action(&mine(Some("CHANGES_REQUESTED"), "MERGEABLE", false, vec![]), "me"),
            "address changes"
        );
    }
    #[test]
    fn draft_action() {
        assert_eq!(mine_action(&mine(None, "MERGEABLE", true, vec![]), "me"), "draft");
    }
    #[test]
    fn review_text_variants() {
        assert_eq!(review_text(&mine(Some("APPROVED"), "x", false, vec![])), "approved");
        assert_eq!(review_text(&mine(Some("CHANGES_REQUESTED"), "x", false, vec![])), "changes");
        assert_eq!(review_text(&mine(None, "x", false, vec![])), "awaiting");
    }
    #[test]
    fn reviewer_state_requested_needs_review() {
        let pr = ReviewPr {
            number: 1,
            url: "u".into(),
            author: Author { login: "other".into() },
            latest_reviews: vec![],
            review_requests: vec![ReviewRequest { login: "me".into() }],
        };
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "-");
        assert_eq!(action, "REVIEW NEEDED");
    }
    #[test]
    fn reviewer_state_approved_done() {
        let pr = ReviewPr {
            number: 1,
            url: "u".into(),
            author: Author { login: "other".into() },
            latest_reviews: vec![Review {
                author: Author { login: "me".into() },
                state: "APPROVED".into(),
                submitted_at: "".into(),
            }],
            review_requests: vec![],
        };
        let (vote, action) = reviewer_state(&pr, "me");
        assert_eq!(vote, "approved");
        assert_eq!(action, "done (approved)");
    }
    #[test]
    fn issue_of_finds_swe() {
        assert_eq!(issue_of("Fix thing", "lev/swe-123-fix"), "SWE-123");
        assert_eq!(issue_of("no issue here", "main"), "-");
    }
    #[test]
    fn diff_cell_shows_change() {
        assert_eq!(diff_cell(Some("ok"), "fail"), "ok → fail");
        assert_eq!(diff_cell(Some("ok"), "ok"), "ok");
        assert_eq!(diff_cell(None, "ok"), "ok");
    }
}
