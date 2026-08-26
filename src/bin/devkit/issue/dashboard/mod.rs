use crate::issue::{prs, triage};
use anyhow::{Context, Result};

mod bucket;
mod cache;
mod chart;
mod data;

pub struct DashboardArgs {
    pub bucket: String,
    pub chart: String,
    pub mode: String,
    pub all_roles: bool,
    pub author: Option<String>,
    pub no_plots: bool,
    pub no_cache: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

pub fn run(args: DashboardArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());

    // At-a-glance: worktree triage, then my PRs + PRs awaiting my review.
    let report = crate::issue::status::gather_live(&start, &[], args.config.as_deref())?;
    triage::render(&report, false);
    println!();
    // The PR tables are a secondary panel; if gh is unavailable the rest of the
    // dashboard (triage above, timelines below) must still render.
    if let Err(e) = prs::run(
        true,
        true,
        None,
        false,
        args.config.clone(),
        devkit_issue::prs::Fetch::default(),
    ) {
        eprintln!("(PR tables unavailable: {e})");
    }

    if args.no_plots {
        return Ok(());
    }

    use chrono::Utc;
    use std::collections::HashMap;
    let now: chrono::DateTime<Utc> = std::time::SystemTime::now().into();
    let width = devkit_common::ui::term_width();

    // The tracker this project talks to, plus its GitHub repositories (when
    // any resolve) — used both to fetch the timeline and to scope its cache
    // entries so two projects, or two viewers of one project, never share
    // a cache file (see `cache::CacheScope`).
    let (resolved, scope_repos) =
        crate::issue::tracker::select(args.config.as_deref(), &start, None);
    let tracker = resolved.tracker.as_ref();
    let scope_repo = scope_repos
        .issues()
        .or_else(|_| scope_repos.prs())
        .map(|r| r.slug.clone())
        .unwrap_or_default();
    let viewer = match tracker.kind() {
        devkit_common::tracker::TrackerKind::Linear => {
            devkit_common::secrets::resolve("LINEAR_API_KEY").unwrap_or_default()
        }
        devkit_common::tracker::TrackerKind::Github => devkit_common::github::token()
            .unwrap_or_default()
            .to_string(),
        devkit_common::tracker::TrackerKind::None => String::new(),
    };
    let scope = cache::CacheScope {
        tracker: tracker.kind(),
        repo: scope_repo,
        viewer,
    };

    // --- Issues by status over time ---
    let use_cache = !args.no_cache;
    let steps = devkit_common::progress::Steps::new();
    let pb = steps.spinner(&format!("Loading {} issue history…", tracker.kind()));
    let issues = data::issues(tracker, &scope, use_cache, |n| {
        pb.set_message(format!(
            "Loading {} issue history… {n} issues",
            tracker.kind()
        ));
    });
    steps.clear();
    if issues.is_empty() {
        let kind = tracker.kind();
        if let Some(why) = resolved.unbuilt_reason() {
            println!("\n(no issue timeline — {why})");
        } else if !tracker.ready() {
            let hint = match kind {
                devkit_common::tracker::TrackerKind::Linear => "set LINEAR_API_KEY",
                devkit_common::tracker::TrackerKind::Github => {
                    "set GH_TOKEN/GITHUB_TOKEN or run `gh auth login`"
                }
                devkit_common::tracker::TrackerKind::None => "set [tracker] kind",
            };
            println!("\n(no {kind} issues — {hint} for the issue timeline)");
        } else {
            println!("\n({kind} returned no assigned issues)");
        }
    } else if let Some(first) = data::origin(tracker, &issues) {
        let b = if args.bucket == "auto" {
            bucket::choose_bucket(first, now, width).to_string()
        } else {
            args.bucket.clone()
        };
        let starts = bucket::bucket_starts(first, now, &b);
        let ends: Vec<_> = (0..starts.len())
            .map(|i| std::cmp::min(*starts.get(i + 1).unwrap_or(&now), now))
            .collect();
        let labels: Vec<String> = starts.iter().map(|s| bucket::label_for(*s, &b)).collect();

        let mut meta: HashMap<String, (devkit_common::tracker::StateKind, String)> = HashMap::new();
        let replays: Vec<_> = issues
            .iter()
            .map(|i| bucket::parse_issue(i, &mut meta))
            .collect();

        // Lifecycle stacking order: type rank, then name.
        let mut names: Vec<String> = meta.keys().cloned().collect();
        names.sort_by(|a, b| {
            bucket::type_rank(meta[a].0)
                .cmp(&bucket::type_rank(meta[b].0))
                .then_with(|| a.cmp(b))
        });

        let mut series: Vec<Vec<u32>> = names.iter().map(|_| vec![0u32; starts.len()]).collect();
        for (si, name) in names.iter().enumerate() {
            for (bi, end) in ends.iter().enumerate() {
                series[si][bi] = replays
                    .iter()
                    .filter(|r| bucket::state_at(r, *end).as_deref() == Some(name.as_str()))
                    .count() as u32;
            }
        }
        // Drop statuses that never appear.
        let keep: Vec<usize> = (0..names.len())
            .filter(|&i| series[i].iter().any(|&v| v > 0))
            .collect();
        let names: Vec<String> = keep.iter().map(|&i| names[i].clone()).collect();
        let mut series: Vec<Vec<u32>> = keep.iter().map(|&i| series[i].clone()).collect();
        let colors: Vec<(u8, u8, u8)> = names.iter().map(|n| chart::hex_rgb(&meta[n].1)).collect();

        if args.mode == "proportional" {
            for bi in 0..starts.len() {
                let total: u32 = series.iter().map(|s| s[bi]).sum();
                if total > 0 {
                    for s in series.iter_mut() {
                        s[bi] = (s[bi] as f64 / total as f64 * 100.0).round() as u32;
                    }
                }
            }
        }

        let title = format!(
            "My {} issues by status — per {b}, {}",
            tracker.kind(),
            args.mode
        );
        if args.chart == "line" {
            chart::render_lines(&title, &series, &names, &colors);
        } else {
            chart::render_stacked_bars(
                &title,
                &labels,
                &series,
                &names,
                &colors,
                &starts,
                b == "day",
            );
        }
    }

    // Footer for issues.
    let open_now = issues.iter().filter(|i| i.state.kind.is_open()).count();
    if !issues.is_empty() {
        println!("\nTotal assigned: {}   open now: {open_now}", issues.len());
    }

    // --- PRs opened/merged + commits over time ---
    let author = match args.author.clone() {
        Some(a) => a,
        None => capture_email(&start),
    };
    let monorepo = monorepo_dir(&start)?;
    // The `[github]` config lives beside `start`'s `devkit.toml`, not
    // `monorepo` (the main checkout) — only the `origin` remote lookup runs
    // against `monorepo`.
    let loaded = devkit_ports::load::load(
        args.config.as_deref().map(std::path::Path::new),
        std::path::Path::new(&start),
    )?;
    let repos = devkit_common::github::Repos::resolve(&loaded.config.github, &monorepo, None);
    // Absent, not `?`: the PR-timeline section degrades to empty when no PR
    // repository resolves (a Linear project whose code is not on GitHub is
    // exactly the shape `[github]` exists to serve), while the issue charts
    // and commit history above and below it still render.
    let pr_repo = repos.prs().ok();
    let steps = devkit_common::progress::Steps::new();
    let _b1 = steps.spinner("[1/2] Loading PR history…");
    let _b2 = steps.spinner("[2/2] Loading commit history…");
    let (opened, merged, add, del, commits) = std::thread::scope(|s| {
        let pr_t = s.spawn(|| data::pr_timeline(args.all_roles, use_cache, pr_repo, &scope));
        let commit_t = s.spawn(|| data::commit_dates(&monorepo, &author));
        let (opened, merged, add, del) = pr_t.join().expect("pr timeline thread panicked");
        let commits = commit_t.join().expect("commit thread panicked");
        (opened, merged, add, del, commits)
    });
    steps.clear();

    let mut stamps: Vec<chrono::DateTime<Utc>> = Vec::new();
    stamps.extend(opened.iter().copied());
    stamps.extend(merged.iter().copied());
    stamps.extend(commits.iter().copied());
    if let Some(&first) = stamps.iter().min() {
        let b = if args.bucket == "auto" {
            bucket::choose_bucket(first, now, width).to_string()
        } else {
            args.bucket.clone()
        };
        let starts = bucket::bucket_starts(first, now, &b);
        let labels: Vec<String> = starts.iter().map(|s| bucket::label_for(*s, &b)).collect();
        let c_commits = bucket::tally(&starts, &commits);
        let c_opened = bucket::tally(&starts, &opened);
        let c_merged = bucket::tally(&starts, &merged);

        let cyan = (0u8, 200u8, 200u8);
        let orange = (255u8, 150u8, 0u8);
        let green = (0u8, 200u8, 0u8);
        if args.chart == "line" {
            chart::render_lines(
                &format!("Commits per {b}"),
                std::slice::from_ref(&c_commits),
                &["commits".into()],
                &[cyan],
            );
            chart::render_lines(
                &format!("PRs per {b}"),
                &[c_opened.clone(), c_merged.clone()],
                &["opened".into(), "merged".into()],
                &[orange, green],
            );
        } else {
            chart::render_stacked_bars(
                &format!("Commits per {b}"),
                &labels,
                std::slice::from_ref(&c_commits),
                &["commits".into()],
                &[cyan],
                &starts,
                b == "day",
            );
            chart::render_stacked_bars(
                &format!("PRs opened/merged per {b}"),
                &labels,
                &[c_opened.clone(), c_merged.clone()],
                &["opened".into(), "merged".into()],
                &[orange, green],
                &starts,
                b == "day",
            );
        }
    }
    println!(
        "\nPRs: {} opened, {} merged   Commits: {}   Lines: +{add} / -{del}",
        opened.len(),
        merged.len(),
        commits.len()
    );
    Ok(())
}

fn capture_email(start: &str) -> String {
    devkit_common::git::Git::at(std::path::Path::new(start))
        .args(["config", "user.email"])
        .output()
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The monorepo root where commits land: `start`'s main checkout, or `start`
/// itself when it already is the main checkout.
fn monorepo_dir(start: &str) -> anyhow::Result<String> {
    let start = std::path::Path::new(start);
    let main = devkit_common::git::main_checkout(start)?
        .map(Ok)
        .unwrap_or_else(|| devkit_common::git::checkout_root(start))?;
    main.to_str()
        .map(str::to_string)
        .context("monorepo path not UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `doppler_yaml` may be declared only in the main checkout and inherited
    /// by every linked worktree (`devkit-config`'s main-checkout layering), so
    /// `monorepo_dir` cannot infer the monorepo root from that config value —
    /// it has to ask git, which is what this pins.
    #[test]
    fn monorepo_dir_resolves_a_linked_worktree_to_its_main_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let g = |args: &[&str], cwd: &std::path::Path| {
            devkit_common::git::Git::fixture(cwd)
                .args(args.iter().copied())
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
        };
        g(&["init", "-q", "-b", "main"], &main);
        std::fs::write(main.join("f.txt"), "x\n").unwrap();
        g(&["add", "-A"], &main);
        g(&["commit", "-qm", "init"], &main);
        std::fs::write(
            main.join("devkit.toml"),
            "[defaults]\nworktree_root = \"wt\"\nbranch_prefix = \"x/\"\n\
             baseline_ref = \"origin/main\"\nbaseline_path = \"\"\n\
             doppler_yaml = \"doppler.yaml\"\n",
        )
        .unwrap();
        std::fs::write(main.join("doppler.yaml"), "").unwrap();

        let wt = dir.path().join("wt-side");
        g(
            &["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()],
            &main,
        );

        let resolved = monorepo_dir(wt.to_str().unwrap()).unwrap();

        assert_eq!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(&main).unwrap(),
            "resolves to the main checkout, not the linked worktree that has no devkit.toml of its own"
        );
        assert_ne!(
            std::fs::canonicalize(&resolved).unwrap(),
            std::fs::canonicalize(&wt).unwrap(),
            "non-vacuous: the worktree and the main checkout are genuinely different paths"
        );
    }
}
