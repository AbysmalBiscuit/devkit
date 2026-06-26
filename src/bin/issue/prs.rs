use anyhow::Result;
use devkit_common::{paths, ui};
use devkit_issue::prs::{MinePrView, ReviewPrView};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// rendering ---------------------------------------------------------------------

/// Colour an ACTION value by whose turn it is: green = ready to land, red =
/// needs you, yellow = waiting on the author, dim = passive. Mirrors the verbs
/// produced by `mine_action`/`reviewer_state`.
fn paint_action(action: &str, s: &str) -> String {
    if action.starts_with("MERGE")
        || action.starts_with("rebase -> merge")
        || action.starts_with("done")
    {
        ui::green(s)
    } else if action.starts_with("address")
        || action.starts_with("fix")
        || action.starts_with("REVIEW NEEDED")
    {
        ui::red(s)
    } else if action.starts_with("awaiting author") {
        ui::yellow(s)
    } else {
        ui::dim(s)
    }
}

/// Render `cur` through `paint`. When it differs from the cached `prev`, prefix
/// the struck-through old value and a dim arrow so the change reads at a glance.
fn diff_cell(prev: Option<&str>, cur: &str, paint: impl Fn(&str) -> String) -> String {
    match prev {
        Some(p) if p != cur => format!("{}{}{}", ui::dim_strike(p), ui::dim(" → "), paint(cur)),
        _ => paint(cur),
    }
}

fn issue_cell(issue_id: &str, url_key: Option<&str>) -> String {
    if issue_id == "-" {
        return ui::dim("-");
    }
    let linked = match url_key {
        Some(k) => ui::link(
            issue_id,
            &format!("https://linear.app/{k}/issue/{issue_id}"),
        ),
        None => issue_id.to_string(),
    };
    ui::cyan(&linked)
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

fn mine_table(
    prs: &[MinePrView],
    url_key: Option<&str>,
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    println!("{}", ui::bold_cyan("MY OPEN PRs"));
    let mut cur = BTreeMap::new();
    if prs.is_empty() {
        println!("  {}", ui::dim("(none)"));
        return cur;
    }
    let mut t = ui::table(&["PR", "ISSUE", "REVIEW", "CHECK", "ACTION"]);
    for pr in prs {
        let review = pr.review_state.clone();
        let check = pr.check_state.clone();
        let action = pr.action.clone();
        let was = prev.get(&pr.number.to_string());
        let g = |k: &str| was.and_then(|m| m.get(k)).map(|s| s.as_str());
        t.add_row(vec![
            ui::link(&format!("#{}", pr.number), &pr.url),
            issue_cell(&pr.issue_id, url_key),
            diff_cell(g("review"), &review, |s| s.to_string()),
            diff_cell(g("check"), &check, |s| s.to_string()),
            diff_cell(g("action"), &action, |s| paint_action(&action, s)),
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
    rows: &[ReviewPrView],
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    println!("\n{}", ui::bold_cyan("PRs AWAITING MY REVIEW"));
    let mut cur = BTreeMap::new();
    if rows.is_empty() {
        println!("  {}", ui::dim("(none)"));
        return cur;
    }
    let mut t = ui::table(&["PR", "AUTHOR", "MY VOTE", "ACTION"]);
    for pr in rows {
        let vote = pr.my_vote.clone();
        let action = pr.action.clone();
        let was = prev.get(&pr.number.to_string());
        let g = |k: &str| was.and_then(|m| m.get(k)).map(|s| s.as_str());
        t.add_row(vec![
            ui::link(&format!("#{}", pr.number), &pr.url),
            pr.author.clone(),
            diff_cell(g("vote"), &vote, |s| s.to_string()),
            diff_cell(g("action"), &action, |s| paint_action(&action, s)),
        ]);
        cur.insert(
            pr.number.to_string(),
            BTreeMap::from([("vote".to_string(), vote), ("action".to_string(), action)]),
        );
    }
    println!("{t}");
    cur
}

// Entry point -------------------------------------------------------------------

pub fn run(
    mine: bool,
    reviews: bool,
    repo: Option<String>,
    no_cache: bool,
    config: Option<String>,
) -> Result<()> {
    let want_mine = mine || !reviews;
    let want_reviews = reviews || !mine;

    // Check-name globs to discount from the CHECK verdict. Absent or unreadable
    // config simply means no checks are ignored — triage still works repo-wide.
    let ignored_checks = devkit_ports::load::load(config.as_deref().map(Path::new), Path::new("."))
        .map(|l| l.config.defaults.ignored_checks)
        .unwrap_or_default();

    let steps = devkit_common::progress::Steps::new();
    let _b1 = steps.spinner("[1/2] Resolving Linear workspace…");
    let _b2 = steps.spinner("[2/2] Fetching PRs from GitHub…");

    let (url_key, report, repo_key) = std::thread::scope(|s| {
        let linear_t = s.spawn(devkit_common::linear::workspace_url_key);
        let ignored_checks = &ignored_checks;
        let github_t = s.spawn(move || -> Result<_> {
            let resolved = devkit_issue::prs::resolve_repo(repo.as_deref(), ".")?;
            let report =
                devkit_issue::prs::gather(".", mine, reviews, Some(&resolved), ignored_checks)?;
            let repo_key = if no_cache { None } else { Some(resolved) };
            Ok((report, repo_key))
        });
        let url_key = linear_t.join().expect("linear thread panicked");
        let (report, repo_key) = github_t.join().expect("github thread panicked")?;
        Ok::<_, anyhow::Error>((url_key, report, repo_key))
    })?;

    steps.clear();

    let path = repo_key.as_ref().map(|r| cache_path(r));
    let mut cache: Snap = path.as_deref().map(load_cache).unwrap_or_default();

    if want_mine {
        let prev = cache.get("mine").cloned().unwrap_or_default();
        let cur = mine_table(&report.mine, url_key.as_deref(), &prev);
        cache.insert("mine".to_string(), cur);
    }
    if want_reviews {
        let prev = cache.get("reviews").cloned().unwrap_or_default();
        let cur = reviews_table(&report.reviews, &prev);
        cache.insert("reviews".to_string(), cur);
    }

    if (want_mine && !report.mine.is_empty()) || (want_reviews && !report.reviews.is_empty()) {
        println!(
            "\n{} {} (REVIEW NEEDED · address changes · fix CI) · {} (MERGE · done) · {} (awaiting author fixes) · {}",
            ui::dim("ACTION colour:"),
            ui::red("needs you"),
            ui::green("ready to land"),
            ui::yellow("waiting on author"),
            ui::dim("passive (awaiting review · draft)"),
        );
        println!(
            "{}",
            ui::dim("old → new in a cell = value changed since the last run.")
        );
    }

    if let Some(p) = &path {
        save_cache(p, &cache)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_cell_shows_change() {
        // Tests are not a tty, so colour/strike helpers pass text through and the
        // change reads as a plain `old → new`.
        let plain = |s: &str| s.to_string();
        assert_eq!(diff_cell(Some("ok"), "fail", plain), "ok → fail");
        assert_eq!(diff_cell(Some("ok"), "ok", plain), "ok");
        assert_eq!(diff_cell(None, "ok", plain), "ok");
    }
}
