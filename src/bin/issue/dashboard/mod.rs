use crate::{prs, triage};
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
    let (rows, states, has_key, url_key) = triage::gather(&start, &[])?;
    triage::render(&rows, &states, has_key, url_key.as_deref());
    println!();
    // The PR tables are a secondary panel; if gh is unavailable the rest of the
    // dashboard (triage above, timelines below) must still render.
    if let Err(e) = prs::run(true, true, None, false) {
        eprintln!("(PR tables unavailable: {e})");
    }

    if args.no_plots {
        return Ok(());
    }

    use chrono::Utc;
    use std::collections::HashMap;
    let now: chrono::DateTime<Utc> = std::time::SystemTime::now().into();
    let width = devkit_common::ui::term_width();

    // --- Issues by status over time ---
    let use_cache = !args.no_cache;
    let pb = crate::spin::spinner("Loading Linear issue history…");
    let issues = data::issues(use_cache);
    pb.finish_and_clear();
    if issues.is_empty() {
        println!("\n(no Linear issues — set LINEAR_API_KEY for the issue timeline)");
    } else if let Some(first) = data::origin(&issues) {
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

        let mut meta: HashMap<String, (String, String)> = HashMap::new();
        let replays: Vec<_> = issues
            .iter()
            .map(|i| bucket::parse_issue(i, &mut meta))
            .collect();

        // Lifecycle stacking order: type rank, then name.
        let type_rank = |k: &str| match k {
            "triage" => 0,
            "backlog" => 1,
            "unstarted" => 2,
            "started" => 3,
            "completed" => 4,
            "canceled" => 5,
            _ => 99,
        };
        let mut names: Vec<String> = meta.keys().cloned().collect();
        names.sort_by(|a, b| {
            type_rank(&meta[a].0)
                .cmp(&type_rank(&meta[b].0))
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

        let title = format!("My Linear issues by status — per {b}, {}", args.mode);
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
    let open_now = issues
        .iter()
        .filter(|i| i.state.kind != "completed" && i.state.kind != "canceled")
        .count();
    if !issues.is_empty() {
        println!("\nTotal assigned: {}   open now: {open_now}", issues.len());
    }

    // --- PRs opened/merged + commits over time ---
    let pb = crate::spin::spinner("Loading PR and commit history…");
    let (opened, merged, add, del) = data::pr_timeline(args.all_roles, use_cache);
    let author = match args.author.clone() {
        Some(a) => a,
        None => capture_email(&start),
    };
    let monorepo = monorepo_dir(&args)?;
    let commits = data::commit_dates(&monorepo, &author);
    pb.finish_and_clear();

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
    devkit_common::cmd::git(&["config", "user.email"], start)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// The monorepo root where commits land, derived from the configured
/// `doppler_yaml` path (its parent directory) rather than a hardcoded layout —
/// `doppler.yaml` lives at the repo root.
fn monorepo_dir(args: &DashboardArgs) -> anyhow::Result<String> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = devkit_ports::load::load(
        args.config.as_deref().map(std::path::Path::new),
        std::path::Path::new(&start),
    )?;
    let yaml = devkit_ports::config::expand_tilde(&loaded.config.defaults.doppler_yaml);
    let dir = yaml
        .parent()
        .context("doppler_yaml has no parent directory to locate the monorepo")?;
    Ok(dir.to_string_lossy().into_owned())
}
