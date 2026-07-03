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

// snapshot cache -----------------------------------------------------------------
// One file per repo: the previous run's full rows (for the stale-while-
// revalidate render) plus the per-PR diff values (for `old → new` cells).

/// Per-section `pr-number -> field -> value` maps backing the `old → new` cells.
type DiffMap = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Snapshot {
    #[serde(default)]
    mine: Vec<MinePrView>,
    #[serde(default)]
    reviews: Vec<ReviewPrView>,
    #[serde(default)]
    diff: DiffMap,
}

fn cache_path(repo: &str) -> PathBuf {
    paths::cache_dir()
        .join("pr-status")
        .join(format!("{}.json", repo.replace('/', "_")))
}

/// Read the snapshot; any parse failure (including the pre-snapshot cache
/// format) reads as an empty snapshot rather than an error. Note the
/// mechanism: the whole-struct parse fails on any incompatibility and
/// `unwrap_or_default` catches it — the per-field `#[serde(default)]`s only
/// rescue *missing* fields, not shape mismatches.
fn load_snapshot(path: &Path) -> Snapshot {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_snapshot(path: &Path, snap: &Snapshot) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    // Write-then-rename so a concurrent or killed run never leaves a torn file.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(snap)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Assemble the snapshot to persist. A section's rows are replaced only when
/// that section was fetched this run; a partial run (`--mine` / `--reviews`)
/// keeps the other section's previously cached rows instead of wiping them
/// with the empty fresh fetch.
fn next_snapshot(
    report: &devkit_issue::prs::PrsReport,
    want_mine: bool,
    want_reviews: bool,
    prev_mine: Vec<MinePrView>,
    prev_reviews: Vec<ReviewPrView>,
    diff: DiffMap,
) -> Snapshot {
    Snapshot {
        mine: if want_mine {
            report.mine.clone()
        } else {
            prev_mine
        },
        reviews: if want_reviews {
            report.reviews.clone()
        } else {
            prev_reviews
        },
        diff,
    }
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
    let Snapshot {
        mine: prev_mine,
        reviews: prev_reviews,
        mut diff,
    } = path.as_deref().map(load_snapshot).unwrap_or_default();

    if want_mine {
        let prev = diff.get("mine").cloned().unwrap_or_default();
        let cur = mine_table(&report.mine, url_key.as_deref(), &prev);
        diff.insert("mine".to_string(), cur);
    }
    if want_reviews {
        let prev = diff.get("reviews").cloned().unwrap_or_default();
        let cur = reviews_table(&report.reviews, &prev);
        diff.insert("reviews".to_string(), cur);
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
        save_snapshot(
            p,
            &next_snapshot(
                &report,
                want_mine,
                want_reviews,
                prev_mine,
                prev_reviews,
                diff,
            ),
        )?;
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

    #[test]
    fn snapshot_round_trips() {
        let snap = Snapshot {
            mine: vec![devkit_issue::prs::MinePrView {
                number: 1,
                url: "https://x/1".into(),
                issue_id: "ENG-1".into(),
                review_state: "approved".into(),
                check_state: "ok".into(),
                action: "MERGE".into(),
            }],
            reviews: vec![],
            diff: DiffMap::new(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mine.len(), 1);
        assert_eq!(back.mine[0].action, "MERGE");
    }

    // A partial run fetches only one section; the saved snapshot must keep the
    // other section's previously cached rows instead of overwriting them with
    // the empty fresh fetch.
    #[test]
    fn next_snapshot_preserves_unrequested_sections() {
        let prev_mine = vec![devkit_issue::prs::MinePrView {
            number: 7,
            url: "https://x/7".into(),
            issue_id: "ENG-7".into(),
            review_state: "pending".into(),
            check_state: "ok".into(),
            action: "awaiting review".into(),
        }];
        let report = devkit_issue::prs::PrsReport {
            mine: vec![],
            reviews: vec![devkit_issue::prs::ReviewPrView {
                number: 9,
                url: "https://x/9".into(),
                author: "alice".into(),
                my_vote: "-".into(),
                action: "REVIEW NEEDED".into(),
            }],
        };
        // Reviews-only run: cached mine rows survive, reviews are replaced.
        let snap = next_snapshot(
            &report,
            false,
            true,
            prev_mine.clone(),
            vec![],
            DiffMap::new(),
        );
        assert_eq!(snap.mine.len(), 1);
        assert_eq!(snap.mine[0].number, 7);
        assert_eq!(snap.reviews.len(), 1);
        assert_eq!(snap.reviews[0].number, 9);
        // Full run: mine is replaced by the (empty) fresh fetch.
        let snap = next_snapshot(&report, true, true, prev_mine, vec![], DiffMap::new());
        assert!(snap.mine.is_empty());
        assert_eq!(snap.reviews.len(), 1);
    }

    // The pre-snapshot cache format (top-level {"mine": {"12": {...}}} maps)
    // must read as an empty snapshot, not an error — first run after upgrade
    // simply has no stale table.
    #[test]
    fn old_format_reads_as_empty() {
        let old = r#"{"mine":{"12":{"review":"approved","check":"ok","action":"MERGE"}}}"#;
        let dir = std::env::temp_dir().join(format!("devkit-prs-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("repo.json");
        std::fs::write(&p, old).unwrap();
        let snap = load_snapshot(&p);
        assert!(snap.mine.is_empty());
        assert!(snap.reviews.is_empty());
        assert!(snap.diff.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
