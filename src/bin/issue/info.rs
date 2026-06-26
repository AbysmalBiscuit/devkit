use crate::triage::render;
use anyhow::Result;
use devkit_common::cmd::git;
use devkit_common::progress::Steps;
use devkit_issue::status::{self as st, IssueWorktree, StatusReport};
use std::path::Path;

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

pub fn run(start: &str, selector: Option<&str>, json: bool, cache_only: bool) -> Result<()> {
    // `issue info` reports a single worktree, so discover once (one cheap
    // `git worktree list`) and enrich only the target — never run the
    // per-worktree dirty/PR/Linear work for every worktree the way a full
    // `status` gather does.
    let d = st::discover(start, &[])?;
    let top = current_top(start);
    let has_key = devkit_common::secrets::resolve("LINEAR_API_KEY").is_some();

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

    let mut linear_workspace = None;
    let steps = Steps::new();
    if cache_only {
        if let Some(pr) = crate::info_cache::read(Path::new(&row.worktree)) {
            apply_cached_pr(&mut row, pr);
        } else if discovered {
            // Offline verdict from local signal only — PR stays NO_PR and Linear
            // stays unknown. The main-clone row keeps its empty verdict.
            let reason = st::reason_not_finished(&row, has_key, false);
            row.finished = reason.is_none();
            row.reason_not_finished = reason;
        }
    } else if discovered {
        // Live: one `gh pr list` plus a single-id Linear lookup, scoped to this
        // row — not the whole worktree set.
        steps
            .during("Fetching PR status…", || st::fetch_prs(&d))?
            .apply_best(&mut row);
        if row.issue_id != "UNKNOWN" {
            let linear = steps.during("Fetching Linear status…", || {
                devkit_common::linear::states(
                    std::slice::from_ref(&row.issue_id),
                    devkit_common::secrets::resolve("LINEAR_API_KEY").as_deref(),
                )
            });
            if let Some(s) = linear.get(&row.issue_id) {
                row.linear_kind = Some(s.kind.clone());
                row.linear_name = Some(s.name.clone());
            }
        }
        let reason = st::reason_not_finished(&row, has_key, false);
        row.finished = reason.is_none();
        row.reason_not_finished = reason;
        linear_workspace = steps.during(
            "Resolving Linear workspace…",
            devkit_common::linear::workspace_url_key,
        );
        if let (Some(number), Some(url)) = (row.pr_number, row.pr_url.clone()) {
            // pr_number and pr_url are set together, so both-Some is the normal
            // PR case; a PR-less row simply leaves the cache untouched.
            let _ = crate::info_cache::write(
                Path::new(&row.worktree),
                &crate::info_cache::CachedPr {
                    number,
                    state: row.pr_state.clone(),
                    url,
                },
            );
        }
    } else {
        // Live, but the target is the main clone (no associated PR/Linear): only
        // the workspace link is worth resolving for rendering.
        linear_workspace = steps.during(
            "Resolving Linear workspace…",
            devkit_common::linear::workspace_url_key,
        );
    }

    if json {
        println!("{}", serde_json::to_string(&row)?);
    } else {
        let one = StatusReport {
            finished_count: usize::from(row.finished),
            has_linear_key: has_key,
            linear_workspace,
            worktrees: vec![row],
        };
        render(&one, cache_only);
    }
    Ok(())
}

/// Build a row for the worktree at `top` straight from git, for the current-dir
/// case where discovery did not list it (notably the main clone). PR and Linear
/// stay empty — the main clone has neither — while the cache-only path still
/// overlays a cached PR if one happens to exist.
fn local_row(top: &str) -> Result<IssueWorktree> {
    let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"], top)?
        .trim()
        .to_string();
    let branch = if branch == "HEAD" {
        "DETACHED".to_string()
    } else {
        branch
    };
    let issue_id = devkit_common::worktree::issue_id_of(&branch, Path::new(top));
    Ok(IssueWorktree {
        worktree: top.to_string(),
        branch,
        issue_id,
        dirty: st::dirty_of(top),
        pr_number: None,
        pr_state: "NO_PR".to_string(),
        pr_url: None,
        linear_kind: None,
        linear_name: None,
        finished: false,
        reason_not_finished: None,
    })
}

/// Overlay a cached PR onto an offline row. The PR fields come from the cache;
/// the finished verdict is cleared because it cannot be computed without a
/// Linear fetch, and the row's `NO_PR` verdict would otherwise contradict the
/// cached PR.
fn apply_cached_pr(row: &mut IssueWorktree, pr: crate::info_cache::CachedPr) {
    row.pr_number = Some(pr.number);
    row.pr_state = pr.state;
    row.pr_url = Some(pr.url);
    row.finished = false;
    row.reason_not_finished = None;
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
            pr_number: None,
            pr_state: "NO_PR".into(),
            pr_url: None,
            linear_kind: None,
            linear_name: None,
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn local_row_reads_branch_id_and_dirty() {
        use std::process::Command;
        let base = std::env::temp_dir().join(format!("devkit-localrow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&base)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["init", "-q", "-b", "lev/eng-9-foo"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(base.join("f"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "init"]);

        let top = base.to_str().unwrap();
        let r = local_row(top).unwrap();
        assert_eq!(r.issue_id, "ENG-9");
        assert_eq!(r.branch, "lev/eng-9-foo");
        assert_eq!(r.pr_number, None);
        assert!(!r.dirty);

        std::fs::write(base.join("g"), "y").unwrap();
        assert!(local_row(top).unwrap().dirty);

        let _ = std::fs::remove_dir_all(&base);
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
    fn cache_overlay_sets_pr_and_clears_verdict() {
        let mut r = row("/a", "lev/eng-1-x", "ENG-1");
        r.reason_not_finished = Some("no PR, Linear unknown".into());
        apply_cached_pr(
            &mut r,
            crate::info_cache::CachedPr {
                number: 123,
                state: "OPEN".into(),
                url: "https://x/pr/123".into(),
            },
        );
        assert_eq!(r.pr_number, Some(123));
        assert_eq!(r.pr_state, "OPEN");
        assert_eq!(r.pr_url.as_deref(), Some("https://x/pr/123"));
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
}
