use crate::triage::render;
use anyhow::Result;
use devkit_common::cmd::git;
use devkit_common::livetable::{Cell, LiveTable};
use devkit_common::tracker::{Resolved, State};
use devkit_issue::status::{self as st, IssueWorktree, PrStatus, StatusReport, TrackerInfo};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;

/// One source's report to the live-table event loop: the row's PRs, its
/// tracker state, or the tracker's issue-link base.
enum Update {
    Prs(Result<st::Prs>),
    States(HashMap<String, State>),
    LinkBase(Option<String>),
}

/// Index of the worktree the command targets: the one matching `selector`, or —
/// when `selector` is `None` — the one whose path equals `current_top`.
fn pick_index(
    rows: &[IssueWorktree],
    selector: Option<&str>,
    current_top: Option<&str>,
) -> Option<usize> {
    match selector {
        Some(sel) => rows.iter().position(|r| crate::select::matches(r, sel)),
        None => {
            let top = current_top?;
            rows.iter().position(|r| same_path(&r.worktree, top))
        }
    }
}

/// Path equality that tolerates symlinks/normalization by canonicalizing both
/// sides; falls back to a string compare when a path cannot be canonicalized.
fn same_path(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// The current worktree's root (`git rev-parse --show-toplevel`), trimmed.
fn current_top(start: &str) -> Option<String> {
    git(&["rev-parse", "--show-toplevel"], start)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn run(
    start: &str,
    selector: Option<&str>,
    json: bool,
    cache_only: bool,
    config: Option<&str>,
) -> Result<()> {
    // `issue info` reports a single worktree, so discover once (one cheap
    // `git worktree list`) and narrow what is per-worktree: the dirty check
    // and the tracker state run for the target alone. The PR lookup stays
    // whole-set — `st::fetch_prs` aliases every discovered branch into one
    // batch, so narrowing it would not save a round trip.
    let d = st::discover(start, &[])?;
    let top = current_top(start);
    let (resolved, repos) = crate::tracker::select(config, start, None);
    let tracker = resolved.tracker.as_ref();
    let mut info = TrackerInfo::of(&resolved);

    let (mut row, discovered) = match pick_index(d.rows(), selector, top.as_deref()) {
        Some(i) => {
            let mut r = d.rows()[i].clone();
            r.dirty = st::dirty_of(&r.worktree);
            (r, true)
        }
        // No selector and the current worktree isn't in the triage rows (the
        // main clone is omitted from those): report on it directly anyway.
        None => match (selector, top.as_deref()) {
            (None, Some(top)) => (local_row(top)?, false),
            (Some(sel), _) => anyhow::bail!("no worktree matches '{sel}'"),
            (None, None) => anyhow::bail!("not in a git worktree"),
        },
    };

    let recorded_pr = devkit_common::record::read(Path::new(&row.worktree)).and_then(|r| r.pr);
    let cached_pr = seedable_cached_pr(
        crate::info_cache::read(Path::new(&row.worktree)),
        recorded_pr.as_ref(),
    );

    if cache_only {
        if let Some(pr) = cached_pr {
            apply_cached_pr(&mut row, pr);
        } else if discovered {
            // Offline verdict from local signal only — PR stays NO_PR and the
            // issue state stays unknown. The main-clone row keeps its empty
            // verdict.
            let reason = st::reason_not_finished(&row, &info, false);
            row.finished = reason.is_none();
            row.reason_not_finished = reason;
        }
    } else if discovered {
        // Seed the row from any cached PR before the live fetch lands, so the
        // live table's first paint shows a number instead of a spinner;
        // `live_enrich` reconciles it against the live lookup once that
        // arrives.
        if let Some(pr) = cached_pr {
            apply_cached_pr(&mut row, pr);
        }
        let repo = repos.prs()?;
        info.link_base = live_enrich(&mut row, &d, &resolved, !json, repo)?;

        if let PrStatus::Unique { number, state, url } = &row.pr {
            let _ = crate::info_cache::write(
                Path::new(&row.worktree),
                &crate::info_cache::CachedPr {
                    number: *number,
                    state: state.clone(),
                    url: url.clone(),
                },
            );
        }
    } else {
        // Live, but the target is the main clone (no associated PR/issue): only
        // the issue-link base is worth resolving for rendering.
        let steps = devkit_common::progress::Steps::new();
        info.link_base = steps.during("Resolving issue links…", || tracker.issue_url(""));
    }

    if json {
        println!("{}", serde_json::to_string(&row)?);
    } else {
        let one = StatusReport {
            finished_count: usize::from(row.finished),
            tracker: info,
            worktrees: vec![row],
        };
        render(&one, cache_only);
    }
    Ok(())
}

/// Enrich one discovered row live: one `gh pr list`, a single-id tracker
/// lookup, and the tracker's issue-link base — all concurrent, filling a
/// one-row live table as each lands. `render: false` keeps the concurrent
/// fetches but draws nothing (`--json` must not animate). Returns the resolved
/// link base.
fn live_enrich(
    row: &mut IssueWorktree,
    d: &st::Discovered,
    resolved: &Resolved,
    render: bool,
    repo: &devkit_common::github::Repo,
) -> Result<Option<String>> {
    let t = resolved.tracker.as_ref();
    let mut lt = if render {
        LiveTable::new("ISSUE WORKTREES", &crate::triage::HEADERS, 1)
    } else {
        LiveTable::hidden("ISSUE WORKTREES", &crate::triage::HEADERS, 1)
    };
    lt.set(0, 0, Cell::Ready(crate::triage::issue_cell(row, None)));
    lt.set(0, 1, Cell::Ready(crate::triage::branch_cell(&row.branch)));
    lt.set(0, 2, Cell::Ready(crate::triage::tree_cell(row.dirty)));
    let want_state = row.issue_id != "UNKNOWN";
    if !want_state {
        // No tracker fetch reports for an UNKNOWN id, so render the same dim
        // cell the final table shows instead of a spinner that never resolves.
        lt.set(0, 4, Cell::Ready(crate::triage::state_cell(row, t.ready())));
    }
    lt.redraw();

    let mut link_base = None;
    // Whether the row already carries a cached PR, seeded before this call —
    // the live update below reconciles against it instead of blindly
    // replacing it, so a live lookup that still agrees it's unique doesn't
    // discard the cache's answer.
    let had_cache = matches!(row.pr, PrStatus::Unique { .. });
    // The verdict never reads the link base, so it can be computed the moment
    // the PR and state land — before the link base has arrived.
    let verdict_tracker = TrackerInfo::of(resolved);
    let looped: Result<()> = std::thread::scope(|s| {
        let (tx, rx) = mpsc::channel::<Update>();
        {
            let tx = tx.clone();
            s.spawn(move || {
                let _ = tx.send(Update::Prs(st::fetch_prs(d, repo)));
            });
        }
        if want_state {
            let tx = tx.clone();
            let id = row.issue_id.clone();
            s.spawn(move || {
                let _ = tx.send(Update::States(t.states(std::slice::from_ref(&id))));
            });
        }
        {
            let tx = tx.clone();
            s.spawn(move || {
                let _ = tx.send(Update::LinkBase(t.issue_url("")));
            });
        }
        drop(tx);

        let mut got_prs = false;
        let mut got_state = !want_state;
        let mut got_link_base = false;
        lt.drive(&rx, |lt, msg| {
            match msg {
                Update::Prs(res) => {
                    let prs = res?;
                    if had_cache {
                        let mut live = row.clone();
                        prs.apply(&mut live);
                        reconcile_cache(row, &live.pr);
                    } else {
                        prs.apply(row);
                    }
                    got_prs = true;
                    lt.set(0, 3, Cell::Ready(crate::triage::pr_cell(row)));
                }
                Update::States(states) => {
                    if let Some(s) = states.get(&row.issue_id) {
                        row.state = Some(s.clone());
                    }
                    got_state = true;
                    lt.set(0, 4, Cell::Ready(crate::triage::state_cell(row, t.ready())));
                }
                Update::LinkBase(base) => {
                    got_link_base = true;
                    link_base = base;
                }
            }
            if got_prs && got_state {
                let reason = st::reason_not_finished(row, &verdict_tracker, false);
                row.finished = reason.is_none();
                row.reason_not_finished = reason;
                lt.set(0, 5, Cell::Ready(crate::triage::verdict_cell(row, false)));
            }
            Ok(got_prs && got_state && got_link_base)
        })
    });
    // Clear the live block before any error renders, so the anyhow report is
    // not printed under a half-drawn region.
    lt.finish();
    looped?;
    Ok(link_base)
}

/// Build a row for the worktree at `top` straight from git, for the current-dir
/// case where discovery did not list it (notably the main clone). PR and issue
/// state stay empty — the main clone has neither — while the cache-only path
/// still overlays a cached PR if one happens to exist.
fn local_row(top: &str) -> Result<IssueWorktree> {
    let branch = devkit_common::git::branch(Path::new(top))?;
    let issue_id = devkit_common::worktree::issue_id_of(Path::new(top), &branch);
    Ok(IssueWorktree {
        worktree: top.to_string(),
        branch,
        issue_id,
        dirty: st::dirty_of(top),
        pr: PrStatus::None,
        state: None,
        finished: false,
        reason_not_finished: None,
    })
}

/// The cached PR a row may be seeded with. A worktree record that binds a
/// different pull request discards the cache: that cache was written before the
/// binding, from a branch lookup the record has since superseded, and a live
/// answer of "the recorded PR does not resolve" would otherwise leave the
/// superseded PR on the row — with its own state driving the verdict.
fn seedable_cached_pr(
    cached: Option<crate::info_cache::CachedPr>,
    recorded: Option<&devkit_common::github::PrLocator>,
) -> Option<crate::info_cache::CachedPr> {
    let cached = cached?;
    match recorded {
        Some(loc) if loc.number != cached.number => None,
        Some(_) | None => Some(cached),
    }
}

/// Overlay a cached PR onto an offline row. The verdict is cleared because it
/// cannot be computed without a tracker fetch, and a `NO_PR` verdict would
/// contradict the cached PR.
fn apply_cached_pr(row: &mut IssueWorktree, pr: crate::info_cache::CachedPr) {
    row.pr = PrStatus::Unique {
        number: pr.number,
        state: pr.state,
        url: pr.url,
    };
    row.finished = false;
    row.reason_not_finished = None;
}

/// Reconcile a cache-seeded row against the live lookup. The live answer wins;
/// the cached PR survives only when the lookup could not be made at all, where
/// it is the better of the two available answers.
fn reconcile_cache(row: &mut IssueWorktree, live: &PrStatus) {
    if !matches!(live, PrStatus::Unknown { .. }) {
        row.pr = live.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(worktree: &str, branch: &str, id: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: worktree.into(),
            branch: branch.into(),
            issue_id: id.into(),
            dirty: false,
            pr: PrStatus::None,
            state: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn local_row_reads_branch_id_and_dirty() {
        use std::process::Command;
        let base = tempfile::tempdir().unwrap();
        // Git ignores the developer's real global/system config, so this
        // fixture commit can't inherit ambient settings like `commit.gpgsign`.
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&base)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_SYSTEM", "/dev/null")
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["init", "-q", "-b", "lev/eng-9-foo"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(base.path().join("f"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let top = base.path().to_str().unwrap();
        let r = local_row(top).unwrap();
        assert_eq!(r.issue_id, "ENG-9");
        assert_eq!(r.branch, "lev/eng-9-foo");
        assert_eq!(r.pr.number(), None);
        assert!(!r.dirty);

        std::fs::write(base.path().join("g"), "y").unwrap();
        assert!(local_row(top).unwrap().dirty);
    }

    #[test]
    fn selector_picks_by_id() {
        let rows = vec![
            row("/a", "lev/eng-1-x", "ENG-1"),
            row("/b", "lev/eng-2-y", "ENG-2"),
        ];
        assert_eq!(pick_index(&rows, Some("eng-2"), None), Some(1));
    }

    #[test]
    fn no_selector_picks_current_top() {
        let rows = vec![
            row("/a", "lev/eng-1-x", "ENG-1"),
            row("/b", "lev/eng-2-y", "ENG-2"),
        ];
        assert_eq!(pick_index(&rows, None, Some("/b")), Some(1));
    }

    #[test]
    fn a_cache_the_record_contradicts_never_seeds_the_row() {
        let cached = crate::info_cache::CachedPr {
            number: 7,
            state: "MERGED".into(),
            url: "https://github.com/o/r/pull/7".into(),
        };
        let bound = |n: u64| devkit_common::github::PrLocator {
            repo: None,
            number: n,
        };

        assert_eq!(
            seedable_cached_pr(Some(cached.clone()), Some(&bound(9))),
            None,
            "a cache naming another PR must not seed a bound row"
        );
        assert_eq!(
            seedable_cached_pr(Some(cached.clone()), Some(&bound(7))),
            Some(cached.clone())
        );
        assert_eq!(seedable_cached_pr(Some(cached.clone()), None), Some(cached));
        assert_eq!(seedable_cached_pr(None, Some(&bound(7))), None);
    }

    #[test]
    fn cache_overlay_sets_pr_and_clears_verdict() {
        let mut r = row("/a", "lev/eng-1-x", "ENG-1");
        r.reason_not_finished = Some("no PR, tracker state unknown".into());
        apply_cached_pr(
            &mut r,
            crate::info_cache::CachedPr {
                number: 123,
                state: "OPEN".into(),
                url: "https://x/pr/123".into(),
            },
        );
        assert_eq!(r.pr.number(), Some(123));
        assert_eq!(r.pr.state_label(), "OPEN");
        assert_eq!(r.pr.url(), Some("https://x/pr/123"));
        assert!(!r.finished);
        assert_eq!(r.reason_not_finished, None);
    }

    #[test]
    fn no_match_is_none() {
        let rows = vec![row("/a", "lev/eng-1-x", "ENG-1")];
        assert_eq!(pick_index(&rows, Some("eng-9"), None), None);
        assert_eq!(pick_index(&rows, None, Some("/elsewhere")), None);
        assert_eq!(pick_index(&rows, None, None), None);
    }

    #[test]
    fn a_cached_unique_pr_yields_to_a_live_ambiguous_lookup() {
        let mut r = row("/a", "lev/eng-1-x", "ENG-1");
        apply_cached_pr(
            &mut r,
            crate::info_cache::CachedPr {
                number: 7,
                state: "OPEN".into(),
                url: "https://github.com/o/r/pull/7".into(),
            },
        );
        assert!(matches!(r.pr, PrStatus::Unique { number: 7, .. }));

        let live = PrStatus::Ambiguous {
            candidates: vec![
                devkit_common::tracker::PrRef {
                    number: 7,
                    url: "https://github.com/o/r/pull/7".into(),
                },
                devkit_common::tracker::PrRef {
                    number: 9,
                    url: "https://github.com/o/r/pull/9".into(),
                },
            ],
        };
        reconcile_cache(&mut r, &live);

        assert!(
            matches!(r.pr, PrStatus::Ambiguous { .. }),
            "a live ambiguous lookup must clear the cached unique PR, got {:?}",
            r.pr
        );
    }

    #[test]
    fn a_cached_unique_pr_survives_a_live_unique_lookup() {
        let mut r = row("/a", "lev/eng-1-x", "ENG-1");
        apply_cached_pr(
            &mut r,
            crate::info_cache::CachedPr {
                number: 7,
                state: "OPEN".into(),
                url: "https://github.com/o/r/pull/7".into(),
            },
        );
        let live = PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "https://github.com/o/r/pull/7".into(),
        };
        reconcile_cache(&mut r, &live);
        assert!(matches!(r.pr, PrStatus::Unique { number: 7, .. }));
    }

    #[test]
    fn a_cached_unique_pr_survives_an_unavailable_live_lookup() {
        let mut r = row("/a", "lev/eng-1-x", "ENG-1");
        apply_cached_pr(
            &mut r,
            crate::info_cache::CachedPr {
                number: 7,
                state: "OPEN".into(),
                url: "https://github.com/o/r/pull/7".into(),
            },
        );
        let live = PrStatus::Unknown {
            reason: "GitHub unreachable".into(),
        };
        reconcile_cache(&mut r, &live);
        assert!(
            matches!(r.pr, PrStatus::Unique { number: 7, .. }),
            "an unreachable live lookup must not discard the cached PR, got {:?}",
            r.pr
        );
    }
}
