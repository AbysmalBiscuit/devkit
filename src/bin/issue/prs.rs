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

/// The ISSUE column cell: every id as a Linear link (plain text without a
/// workspace url key), space-joined; dim `-` when no id resolved.
fn issue_cell(issue_ids: &[String], url_key: Option<&str>) -> String {
    if issue_ids.is_empty() {
        return ui::dim("-");
    }
    let cells: Vec<String> = issue_ids
        .iter()
        .map(|id| {
            let linked = match url_key {
                Some(k) => ui::link(id, &format!("https://linear.app/{k}/issue/{id}")),
                None => id.to_string(),
            };
            ui::cyan(&linked)
        })
        .collect();
    cells.join(" ")
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

/// Build the "MY OPEN PRs" table body plus the diff map for the next
/// snapshot. Pure — no printing — so both the final render and the stale
/// (last-run) render can share it; the stale path passes an empty `prev`.
fn mine_table_build(
    prs: &[MinePrView],
    url_key: Option<&str>,
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> (String, BTreeMap<String, BTreeMap<String, String>>) {
    let mut cur = BTreeMap::new();
    if prs.is_empty() {
        return (format!("  {}", ui::dim("(none)")), cur);
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
            issue_cell(&pr.issue_ids, url_key),
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
    (t.to_string(), cur)
}

/// The colour legend and diff hint shown under the tables when any fetched
/// section has rows.
fn legend_lines() -> [String; 2] {
    [
        format!(
            "{} {} (REVIEW NEEDED · address changes · fix CI) · {} (MERGE · MERGE (unreviewed) · done) · {} (awaiting author fixes) · {}",
            ui::dim("ACTION colour:"),
            ui::red("needs you"),
            ui::green("ready to land"),
            ui::yellow("waiting on author"),
            ui::dim("passive (awaiting review · draft)"),
        ),
        ui::dim("old → new in a cell = value changed since the last run."),
    ]
}

/// Build every stdout line of the final render, in print order, plus the
/// updated diff map for the next snapshot. Pure — and the layout contract
/// for the stale block: [`stale_body`] must produce the same line count for
/// the same rows, or the swap from stale to fresh shifts the screen by the
/// difference instead of replacing the block in place.
fn final_lines(
    report: &devkit_issue::prs::PrsReport,
    url_key: Option<&str>,
    mut diff: DiffMap,
    want_mine: bool,
    want_reviews: bool,
) -> (Vec<String>, DiffMap) {
    let mut out = Vec::new();
    if want_mine {
        let prev = diff.get("mine").cloned().unwrap_or_default();
        out.push(ui::bold_cyan("MY OPEN PRs"));
        let (body, cur) = mine_table_build(&report.mine, url_key, &prev);
        out.extend(body.lines().map(String::from));
        diff.insert("mine".to_string(), cur);
    }
    if want_reviews {
        let prev = diff.get("reviews").cloned().unwrap_or_default();
        out.push(String::new());
        out.push(ui::bold_cyan("PRs AWAITING MY REVIEW"));
        let (body, cur) = reviews_table_build(&report.reviews, url_key, &prev);
        out.extend(body.lines().map(String::from));
        diff.insert("reviews".to_string(), cur);
    }
    if (want_mine && !report.mine.is_empty()) || (want_reviews && !report.reviews.is_empty()) {
        let [legend, hint] = legend_lines();
        out.push(String::new());
        out.push(legend);
        out.push(hint);
    }
    (out, diff)
}

/// Build the "PRs AWAITING MY REVIEW" table body plus the diff map for the
/// next snapshot. Pure — see [`mine_table_build`].
fn reviews_table_build(
    rows: &[ReviewPrView],
    url_key: Option<&str>,
    prev: &BTreeMap<String, BTreeMap<String, String>>,
) -> (String, BTreeMap<String, BTreeMap<String, String>>) {
    let mut cur = BTreeMap::new();
    if rows.is_empty() {
        return (format!("  {}", ui::dim("(none)")), cur);
    }
    let mut t = ui::table(&["PR", "ISSUE", "AUTHOR", "MY VOTE", "ACTION"]);
    for pr in rows {
        let vote = pr.my_vote.clone();
        let action = pr.action.clone();
        let was = prev.get(&pr.number.to_string());
        let g = |k: &str| was.and_then(|m| m.get(k)).map(|s| s.as_str());
        t.add_row(vec![
            ui::link(&format!("#{}", pr.number), &pr.url),
            issue_cell(&pr.issue_ids, url_key),
            pr.author.clone(),
            diff_cell(g("vote"), &vote, |s| s.to_string()),
            diff_cell(g("action"), &action, |s| paint_action(&action, s)),
        ]);
        cur.insert(
            pr.number.to_string(),
            BTreeMap::from([("vote".to_string(), vote), ("action".to_string(), action)]),
        );
    }
    (t.to_string(), cur)
}

/// The stale-while-revalidate block: last run's tables, every line dimmed.
/// Pure, and built once per run — the whole block is static while the fetch
/// spinner below it animates. The block draws on stderr, so the dim keys off
/// that stream. `dim_all` keeps painted cells dim past their own SGR resets,
/// and the titles use plain dim (not bold cyan) so terminals don't have to
/// resolve bold-over-faint.
///
/// Layout contract: line-for-line parallel to [`final_lines`] — same line
/// count for the same rows — so the fresh render lands exactly where the
/// block was and the screen does not shift at the swap.
fn stale_body(
    prev_mine: &[MinePrView],
    prev_reviews: &[ReviewPrView],
    want_mine: bool,
    want_reviews: bool,
) -> Vec<String> {
    let paint = ui::Paint::on(ui::Stream::Stderr);
    let mut out = Vec::new();
    let empty = BTreeMap::new();
    if want_mine {
        out.push(paint.dim("MY OPEN PRs"));
        let (body, _) = mine_table_build(prev_mine, None, &empty);
        out.extend(body.lines().map(|l| paint.dim_all(l)));
    }
    if want_reviews {
        out.push(String::new());
        out.push(paint.dim("PRs AWAITING MY REVIEW"));
        let (body, _) = reviews_table_build(prev_reviews, None, &empty);
        out.extend(body.lines().map(|l| paint.dim_all(l)));
    }
    if (want_mine && !prev_mine.is_empty()) || (want_reviews && !prev_reviews.is_empty()) {
        let [legend, hint] = legend_lines();
        out.push(String::new());
        out.push(paint.dim_all(&legend));
        out.push(paint.dim_all(&hint));
    }
    out
}

/// Fetch the PR report and the Linear workspace URL key concurrently.
fn fetch_report(
    resolved: &str,
    mine: bool,
    reviews: bool,
    ignored_checks: &[String],
    resolve_pr_links: bool,
) -> Result<(Option<String>, devkit_issue::prs::PrsReport)> {
    enum Update {
        Fetched(Result<devkit_issue::prs::PrsReport>),
        Workspace(Option<String>),
    }
    std::thread::scope(|s| {
        let (tx, rx) = std::sync::mpsc::channel::<Update>();
        {
            let tx = tx.clone();
            s.spawn(move || {
                let _ = tx.send(Update::Workspace(devkit_common::linear::workspace_url_key()));
            });
        }
        {
            let tx = tx.clone();
            s.spawn(move || {
                let _ = tx.send(Update::Fetched(devkit_issue::prs::gather(
                    ".",
                    mine,
                    reviews,
                    Some(resolved),
                    ignored_checks,
                    resolve_pr_links,
                )));
            });
        }
        drop(tx);
        let mut url_key: Option<String> = None;
        let mut report: Option<devkit_issue::prs::PrsReport> = None;
        // The explicit got_ws flag matters: a `None` workspace key is a
        // legitimate answer (no Linear configured), so `url_key.is_none()`
        // cannot mean "still waiting".
        let mut got_ws = false;
        while !(got_ws && report.is_some()) {
            match rx.recv() {
                Ok(Update::Fetched(res)) => report = Some(res?),
                Ok(Update::Workspace(ws)) => {
                    got_ws = true;
                    url_key = ws;
                }
                // All senders gone: fall through to the report check.
                Err(_) => break,
            }
        }
        match report {
            Some(r) => Ok((url_key, r)),
            None => anyhow::bail!("PR fetch ended without a result"),
        }
    })
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

    // Check-name globs to discount from the CHECK verdict, and the Linear
    // PR-link opt-in. Absent or unreadable config means no checks are ignored
    // and no Linear lookup — triage still works repo-wide.
    let loaded = devkit_ports::load::load(config.as_deref().map(Path::new), Path::new(".")).ok();
    let ignored_checks = loaded
        .as_ref()
        .map(|l| l.config.defaults.ignored_checks.clone())
        .unwrap_or_default();
    let resolve_pr_links = loaded
        .as_ref()
        .is_some_and(|l| l.config.linear.resolve_pr_links);

    // Resolve the repo up front: the snapshot cache is keyed by it and the
    // stale table must render before the fetch starts.
    let resolved = devkit_issue::prs::resolve_repo(repo.as_deref(), ".")?;
    let repo_key = if no_cache {
        None
    } else {
        Some(resolved.clone())
    };
    let path = repo_key.as_ref().map(|r| cache_path(r));
    let Snapshot {
        mine: prev_mine,
        reviews: prev_reviews,
        diff,
    } = path.as_deref().map(load_snapshot).unwrap_or_default();

    // Stale-while-revalidate: last run's rows render immediately, dimmed, and
    // the fresh render replaces them in place when it lands. The fetch spinner
    // animates below the block — or alone when there is no usable snapshot —
    // matching the status-lines-under-the-table layout of the live triage
    // table.
    let mut live = devkit_common::livetable::LiveLines::new();
    let have_stale =
        (want_mine && !prev_mine.is_empty()) || (want_reviews && !prev_reviews.is_empty());
    let spin_msg = if have_stale {
        live.set_lines(&stale_body(
            &prev_mine,
            &prev_reviews,
            want_mine,
            want_reviews,
        ));
        format!(
            "Fetching PRs from GitHub… {}",
            ui::Paint::on(ui::Stream::Stderr).dim("(table is as of the last run)")
        )
    } else {
        "Fetching PRs from GitHub…".to_string()
    };
    let _fetch_spin = live.spinner(&spin_msg);

    let fetched = fetch_report(&resolved, mine, reviews, &ignored_checks, resolve_pr_links);
    // Clear the stale block (and finish the spinner) before any fetch error
    // renders, so the anyhow report is not printed under a half-drawn region.
    live.clear();
    let (url_key, report) = fetched?;

    let (lines, diff) = final_lines(&report, url_key.as_deref(), diff, want_mine, want_reviews);
    for l in &lines {
        println!("{l}");
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

    fn mine_view(n: u64, action: &str) -> MinePrView {
        MinePrView {
            number: n,
            url: format!("https://x/{n}"),
            issue_ids: vec![],
            review_state: "none".into(),
            check_state: "ok".into(),
            action: action.into(),
        }
    }

    #[test]
    fn mine_table_build_renders_and_collects() {
        let (body, cur) = mine_table_build(&[mine_view(12, "MERGE")], None, &BTreeMap::new());
        assert!(body.contains("#12"), "{body}");
        assert!(body.contains("MERGE"), "{body}");
        assert_eq!(cur["12"]["action"], "MERGE");
    }

    fn review_view(n: u64, action: &str) -> ReviewPrView {
        ReviewPrView {
            number: n,
            url: format!("https://x/{n}"),
            issue_ids: vec!["ENG-9".into()],
            author: "alice".into(),
            my_vote: "-".into(),
            action: action.into(),
        }
    }

    #[test]
    fn reviews_table_build_renders_issue_column() {
        let (body, _) = reviews_table_build(
            &[review_view(9, "REVIEW NEEDED")],
            Some("acme"),
            &BTreeMap::new(),
        );
        assert!(body.contains("ISSUE"), "header missing ISSUE: {body}");
        assert!(body.contains("ENG-9"), "row missing issue id: {body}");
    }

    #[test]
    fn stale_block_has_rows_and_legend() {
        let lines = stale_body(&[mine_view(12, "MERGE")], &[], true, true);
        assert!(lines.iter().any(|l| l.contains("#12")));
        assert!(lines.iter().any(|l| l.contains("(none)"))); // empty reviews
        assert!(lines.iter().any(|l| l.contains("ACTION colour:")));
    }

    // The layout contract behind the in-place swap: for the same rows, the
    // stale block and the final render must have the same line count, or the
    // screen shifts by the difference when the fresh tables land.
    #[test]
    fn stale_block_aligns_with_final_render() {
        let mine = vec![mine_view(12, "MERGE"), mine_view(13, "fix CI")];
        let reviews = vec![review_view(9, "REVIEW NEEDED")];
        let report = devkit_issue::prs::PrsReport {
            mine: mine.clone(),
            reviews: reviews.clone(),
        };
        for (want_mine, want_reviews) in [(true, true), (true, false), (false, true)] {
            let (fresh, _) = final_lines(&report, None, DiffMap::new(), want_mine, want_reviews);
            let stale = stale_body(&mine, &reviews, want_mine, want_reviews);
            assert_eq!(
                stale.len(),
                fresh.len(),
                "stale/fresh line counts diverge for mine={want_mine} reviews={want_reviews}"
            );
        }
        // Empty sections render a `(none)` line on both sides.
        let report = devkit_issue::prs::PrsReport {
            mine: vec![],
            reviews: vec![],
        };
        let (fresh, _) = final_lines(&report, None, DiffMap::new(), true, true);
        assert_eq!(stale_body(&[], &[], true, true).len(), fresh.len());
    }

    #[test]
    fn issue_cell_joins_multiple_ids() {
        assert_eq!(issue_cell(&[], None), ui::dim("-"));
        let cell = issue_cell(&["ENG-1".into(), "SWE-6".into()], None);
        assert!(cell.contains("ENG-1") && cell.contains("SWE-6"), "{cell}");
    }

    // A pre-issue_ids snapshot (rows carry `issue_id`) still whole-struct
    // parses; the missing `issue_ids` reads as empty for one run.
    #[test]
    fn old_issue_id_snapshot_reads_with_empty_ids() {
        let old = r#"{"mine":[{"number":1,"url":"u","issue_id":"ENG-1","review_state":"approved","check_state":"ok","action":"MERGE"}],"reviews":[],"diff":{}}"#;
        let snap: Snapshot = serde_json::from_str(old).unwrap();
        assert_eq!(snap.mine.len(), 1);
        assert!(snap.mine[0].issue_ids.is_empty());
    }

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
                issue_ids: vec!["ENG-1".into()],
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
            issue_ids: vec!["ENG-7".into()],
            review_state: "pending".into(),
            check_state: "ok".into(),
            action: "awaiting review".into(),
        }];
        let report = devkit_issue::prs::PrsReport {
            mine: vec![],
            reviews: vec![devkit_issue::prs::ReviewPrView {
                number: 9,
                url: "https://x/9".into(),
                issue_ids: vec!["ENG-9".into()],
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
