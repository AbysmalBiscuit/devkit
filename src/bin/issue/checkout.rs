use crate::slug::slugify;
use anyhow::{Context, Result};
use devkit_common::cmd::{gh_capture, gh_json_in, git};
use devkit_common::gitfetch;
use devkit_common::github;
use devkit_common::progress::Steps;
use devkit_common::tracker::{IssueRef, Tracker, TrackerKind};
use devkit_config::expand_tilde;
use devkit_ports::load;
use std::io::{IsTerminal, Write};
use std::path::Path;

pub struct CheckoutArgs {
    pub target: String,
    pub worktree_path: Option<String>,
    pub setup: bool,
    pub apps: Vec<String>,
    pub dir: Option<String>,
    pub config: Option<String>,
}

/// How the raw `<PR_ISSUE_ID_URL>` input is classified before resolution.
#[derive(Debug, PartialEq, Eq)]
enum Ident {
    Pr(github::PrLocator),
    Issue(IssueRef),
    Fuzzy(github::PrLocator),
}

/// Classify the identifier by shape. The PR and bare-number rules are
/// tracker-independent; recognizing an issue id or issue URL is the tracker's.
fn classify(input: &str, t: &dyn Tracker) -> Result<Ident> {
    let s = input.trim();
    if s.contains("github.com") && s.contains("/pull/") {
        let loc = github::PrLocator::from_url(s).context("no PR number in GitHub URL")?;
        return Ok(Ident::Pr(loc));
    }
    if let Some(rest) = s.strip_prefix('#')
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(Ident::Pr(github::PrLocator {
            repo: None,
            number: rest.parse().context("bad PR number")?,
        }));
    }
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Ident::Fuzzy(github::PrLocator {
            repo: None,
            number: s.parse().context("bad number")?,
        }));
    }
    if s.is_empty() || s.split_whitespace().count() != 1 {
        anyhow::bail!("unrecognized PR/issue identifier: {s}");
    }
    // A tracker that cannot parse this input says so, so there is nothing to
    // infer from the shape of what it returned.
    let r = t
        .issue_ref(s)
        .with_context(|| format!("unrecognized PR/issue identifier: {s}"))?;
    anyhow::ensure!(!r.id.is_empty(), "unrecognized PR/issue identifier: {s}");
    Ok(Ident::Issue(r))
}

/// The decision for a bare-number input after probing both sides.
#[derive(Debug, PartialEq, Eq)]
enum FuzzyDecision {
    UsePr,
    UseTracker(IssueRef),
    Prompt(Vec<IssueRef>),
    ErrorAmbiguous,
    ErrorNone,
}

fn decide_fuzzy(pr_exists: bool, candidates: &[IssueRef], is_tty: bool) -> FuzzyDecision {
    match (pr_exists, candidates) {
        (false, []) => FuzzyDecision::ErrorNone,
        (true, []) => FuzzyDecision::UsePr,
        (false, [only]) => FuzzyDecision::UseTracker(only.clone()),
        _ if is_tty => FuzzyDecision::Prompt(candidates.to_vec()),
        _ => FuzzyDecision::ErrorAmbiguous,
    }
}

/// `decide_fuzzy` with the candidates asked from the tracker itself, so the
/// project's declared kind decides what a bare number means rather than
/// whatever `LINEAR_API_KEY` happens to be exported in the shell.
/// `candidates` degrades to empty on error, reported to stderr — the id still
/// resolves via the PR side, or reports "not found", rather than aborting the
/// whole checkout.
fn decide_fuzzy_via(t: &dyn Tracker, n: u64, pr_exists: bool, is_tty: bool) -> FuzzyDecision {
    let candidates = t.candidates(n).unwrap_or_else(|e| {
        eprintln!("warning: tracker lookup for {n} failed, treating it as no issue match: {e:#}");
        Vec::new()
    });
    decide_fuzzy(pr_exists, &candidates, is_tty)
}

struct Resolved {
    loc: github::PrLocator,
    linear_id: Option<String>,
    linear_title: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrMeta {
    number: u64,
    title: String,
    head_ref_name: String,
}

/// Whether GitHub PR `n` exists in `repo`. A clean "not found" from `gh pr view`
/// is `Ok(false)`; a real tool failure (gh missing, unauthenticated, network
/// down, bad cwd) propagates as `Err` rather than masquerading as absence.
fn pr_exists(n: u64, cwd: &str, repo: &github::Repo) -> Result<bool> {
    // Direct HTTP resolves existence from a 200/404; a clean 404 is `Ok(false)`.
    // Any HTTP failure (no token, transport) yields `None` → fall back to `gh`.
    if let Ok(exists) = github::pr_exists(&repo.slug, n) {
        return Ok(exists);
    }
    match gh_capture(
        &["pr", "view", &n.to_string(), "--json", "number"],
        repo,
        cwd,
    ) {
        Ok(_) => Ok(true),
        Err(e) => {
            // `gh_capture` embeds the command's stderr in its error message, so
            // the not-found signal is recoverable from the rendered error chain.
            let msg = format!("{e:#}").to_lowercase();
            if msg.contains("no pull requests found")
                || msg.contains("could not resolve to a pullrequest")
            {
                Ok(false)
            } else {
                Err(e).with_context(|| format!("probing whether PR #{n} exists"))
            }
        }
    }
}

/// PR number/title/head-branch, over direct HTTP when a token is available and
/// falling back to `gh pr view` otherwise.
fn fetch_pr_meta(n: u64, cwd: &str, repo: &github::Repo) -> Result<PrMeta> {
    if let Ok(m) = github::pr_meta(&repo.slug, n) {
        return Ok(PrMeta {
            number: m.number,
            title: m.title,
            head_ref_name: m.head_ref_name,
        });
    }
    gh_json_in(
        &[
            "pr",
            "view",
            &n.to_string(),
            "--json",
            "number,title,headRefName",
        ],
        repo,
        cwd,
    )
}

/// Turn a chosen issue into a `Resolved`, erroring if it has no PR. A title the
/// caller already holds is kept; otherwise the tracker is asked for one. That
/// lookup is best-effort: the title only decorates the worktree name, so a
/// tracker hiccup must not fail a checkout whose PR is already resolved.
///
/// The locator comes from the PR's URL so a PR outside `pr_repo` — which a
/// split `[github]` config makes reachable — is fetched from the repository
/// holding it. A URL that does not parse leaves the number to resolve against
/// `pr_repo`.
fn resolve_issue(id: &str, title: Option<String>, t: &dyn Tracker) -> Result<Resolved> {
    let pr = t
        .issue_pr(id)?
        .with_context(|| format!("issue {id} has no associated PR to check out"))?;
    Ok(Resolved {
        loc: github::PrLocator::from_url(&pr.url).unwrap_or(github::PrLocator {
            repo: None,
            number: pr.number,
        }),
        linear_id: Some(id.to_string()),
        linear_title: title.or_else(|| t.title(id).ok().flatten()),
    })
}

/// Resolve the raw input to a concrete PR. Network + interactive.
fn resolve(
    target: &str,
    cwd: &str,
    repos: &github::Repos,
    t: &dyn Tracker,
    steps: &Steps,
) -> Result<Resolved> {
    match classify(target, t)? {
        Ident::Pr(loc) => Ok(Resolved {
            loc,
            linear_id: None,
            linear_title: None,
        }),
        Ident::Issue(r) => {
            anyhow::ensure!(t.ready(), "issue id given but no tracker is configured");
            // A pasted issue URL spells the title slug out, and the worktree
            // template slugifies whatever it is given, so the slug stands in for
            // the title and spares a lookup.
            steps.during_result(&format!("Resolving issue {}…", r.id), || {
                resolve_issue(&r.id, r.slug.clone(), t)
            })
        }
        Ident::Fuzzy(loc) => {
            let n = loc.number;
            let repo = loc.resolve(repos)?;
            // Probe both sides under a spinner; clear it before any prompt.
            let (exists, decision) = steps.during_result(&format!("Resolving {n}…"), || {
                let exists = pr_exists(n, cwd, &repo)?;
                let is_tty = std::io::stdin().is_terminal();
                Ok::<_, anyhow::Error>((exists, decide_fuzzy_via(t, n, exists, is_tty)))
            })?;
            match decision {
                FuzzyDecision::ErrorNone => {
                    anyhow::bail!("no PR or issue found for {n}")
                }
                FuzzyDecision::ErrorAmbiguous => {
                    anyhow::bail!("ambiguous {n} — rerun as #{n} (PR) or with the full issue id")
                }
                FuzzyDecision::UsePr => Ok(Resolved {
                    loc,
                    linear_id: None,
                    linear_title: None,
                }),
                FuzzyDecision::UseTracker(r) => resolve_issue(&r.id, r.slug.clone(), t),
                FuzzyDecision::Prompt(cands) => match prompt_choice(exists, &cands, n, t.kind())? {
                    None => Ok(Resolved {
                        loc,
                        linear_id: None,
                        linear_title: None,
                    }),
                    Some(r) => resolve_issue(&r.id, r.slug.clone(), t),
                },
            }
        }
    }
}

/// Print the options and read a choice. `Ok(None)` = the GitHub PR.
fn prompt_choice(
    pr_exists: bool,
    candidates: &[IssueRef],
    n: u64,
    kind: TrackerKind,
) -> Result<Option<IssueRef>> {
    println!("Multiple matches for {n}:");
    let mut options: Vec<Option<&IssueRef>> = Vec::new();
    if pr_exists {
        options.push(None);
    }
    options.extend(candidates.iter().map(Some));
    for (i, opt) in options.iter().enumerate() {
        match opt {
            None => println!("  [{i}] GitHub PR #{n}"),
            Some(c) => println!("  [{i}] {} {}", kind.as_str(), c.id),
        }
    }
    print!("Choose [0]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    let idx: usize = line.trim().parse().unwrap_or(0);
    let chosen: Option<&IssueRef> = *options.get(idx).context("choice out of range")?;
    Ok(chosen.cloned())
}

/// The id stored in `.devkit/issue.toml`: the tracker's id when the input
/// resolved to one, else the id parsed from the PR head ref, else `UNKNOWN`.
fn record_issue_id(linear_id: Option<&str>, head_ref: &str) -> String {
    linear_id.map(str::to_string).unwrap_or_else(|| {
        devkit_common::worktree::find_id(head_ref)
            .map(|s| s.to_uppercase())
            .unwrap_or_else(|| "UNKNOWN".into())
    })
}

/// Run `f`; on error, remove the just-created worktree at `worktree` (in
/// `monorepo`) before propagating, so a failed checkout or record write never
/// leaves an orphan worktree with no `.devkit/issue.toml`. Without the record,
/// the worktree is invisible to `issue status`/`issue end` yet blocks a re-run
/// at the path-exists guard. A failure of the removal itself is ignored — the
/// original error is what propagates.
fn with_cleanup<T>(worktree: &Path, monorepo: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match f() {
        Ok(v) => Ok(v),
        Err(e) => {
            let _ = git(
                &[
                    "worktree",
                    "remove",
                    "--force",
                    worktree.to_str().unwrap_or_default(),
                ],
                monorepo,
            );
            Err(e)
        }
    }
}

pub fn run(args: CheckoutArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let cfg = &loaded.config;
    let catalog = &loaded.catalog;
    for a in &args.apps {
        anyhow::ensure!(catalog.contains_key(a), "unknown app `{a}`");
    }

    let wt_root = expand_tilde(&cfg.defaults.worktree_root);
    let monorepo = wt_root.join("monorepo");
    let monorepo_s = monorepo.to_str().context("monorepo path not UTF-8")?;

    let repos = github::Repos::resolve(&cfg.github, monorepo_s, None);
    let tracker =
        devkit_common::tracker::resolve(cfg.tracker.kind, Path::new(monorepo_s), &repos).tracker;
    let steps = Steps::persistent();
    let resolved = resolve(&args.target, monorepo_s, &repos, tracker.as_ref(), &steps)?;
    let pr_repo = resolved.loc.resolve(&repos)?;

    let meta: PrMeta = steps
        .during_result(&format!("Fetching PR #{}…", resolved.loc.number), || {
            fetch_pr_meta(resolved.loc.number, monorepo_s, &pr_repo)
        })
        .with_context(|| format!("fetching PR #{}", resolved.loc.number))?;

    let ctx = serde_json::json!({
        "pr_number": meta.number,
        "pr_title": slugify(&meta.title),
        "linear_id": resolved.linear_id.clone().unwrap_or_default(),
        "linear_title": resolved.linear_title.as_deref().map(slugify).unwrap_or_default(),
    });
    let wt_name = devkit_common::template::render(
        cfg.templates.checkout_worktree_dir(),
        &ctx,
        &cfg.templates.variables,
    )
    .context("rendering `checkout_worktree_dir` template")?
    .trim()
    .to_string();
    let worktree = match &args.worktree_path {
        Some(p) => expand_tilde(p),
        None => wt_root.join(&wt_name),
    };

    anyhow::ensure!(
        !worktree.exists(),
        "worktree path already exists: {}",
        worktree.display()
    );
    let worktree_s = worktree.to_str().context("worktree path not UTF-8")?;

    steps.during_result("Fetching from origin…", || {
        gitfetch::fetch("origin", monorepo_s)
    })?;
    steps.during_result("Creating worktree…", || {
        git(
            &[
                "worktree",
                "add",
                "--detach",
                worktree_s,
                &cfg.defaults.baseline_ref,
            ],
            monorepo_s,
        )
    })?;

    // Once the worktree exists, any failure through record::write leaves an
    // orphan with no record — invisible to status/end, and blocked from
    // re-creation. Clean it up atomically so the user ends up with a recorded
    // worktree or with nothing.
    let issue = with_cleanup(&worktree, monorepo_s, || {
        steps
            .during_result(&format!("Checking out PR #{}…", meta.number), || {
                gh_capture(
                    &["pr", "checkout", &meta.number.to_string()],
                    &pr_repo,
                    worktree_s,
                )
            })
            .with_context(|| format!("checking out PR #{}", meta.number))?;

        // The worktree is built *from* the PR, so it has no head to compare
        // until the checkout lands — validated immediately after, before the
        // record is written, rather than pre-gated.
        let head = git(&["rev-parse", "HEAD"], worktree_s)?.trim().to_string();
        let checked_out = github::pr_meta_full(&pr_repo, meta.number)
            .with_context(|| format!("verifying PR #{}", meta.number))?;
        crate::review::finish::assert_belongs(&checked_out, &head)?;

        let issue = record_issue_id(resolved.linear_id.as_deref(), &meta.head_ref_name);
        devkit_common::record::write(
            &worktree,
            &devkit_common::record::IssueRecord {
                issue: issue.clone(),
                slug: slugify(&meta.title),
                apps: if args.setup {
                    args.apps.clone()
                } else {
                    vec![]
                },
                // A PR checkout reviews someone else's work; there is no issue
                // to scaffold notes for.
                summary: None,
                pr: Some(github::PrLocator {
                    repo: Some(pr_repo.slug.clone()),
                    number: meta.number,
                }),
            },
        )?;
        Ok(issue)
    })?;

    crate::setup::backfill_includes(monorepo_s, &worktree, &cfg.defaults.worktree_include);

    if args.setup {
        let setup_ctx = serde_json::json!({
            "prefix": cfg.defaults.branch_prefix,
            "issue": issue,
            "slug": slugify(&meta.title),
            "apps": args.apps,
        });
        steps.during_result("Preparing apps…", || {
            crate::setup::prep_apps(
                &worktree,
                &meta.head_ref_name,
                &args.apps,
                catalog,
                &setup_ctx,
                &cfg.templates.variables,
            )
        })?;
    }

    let hook_ctx = serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": issue,
        "slug": slugify(&meta.title),
        "apps": args.apps,
        "branch": meta.head_ref_name,
        "worktree": worktree_s,
    });
    crate::setup::run_after_worktree_create(
        &worktree,
        &cfg.hooks.after_worktree_create,
        &hook_ctx,
        &cfg.templates.variables,
    );

    report(meta.number, &meta.head_ref_name, worktree_s)?;
    Ok(())
}

/// What `checkout-pr` prints: a labelled table for a reader, the JSON a caller
/// parses for anything else.
fn report(pr: u64, branch: &str, worktree: &str) -> Result<()> {
    if !devkit_common::ui::stdout_is_tty() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "pr": pr,
                "branch": branch,
                "worktree": worktree,
            }))?
        );
        return Ok(());
    }
    let rows = [
        ("pr", format!("#{pr}")),
        ("worktree", worktree.to_string()),
        ("branch", branch.to_string()),
    ];
    println!("{}", devkit_common::ui::kv_table(&rows));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_cmd(args: &[&str], cwd: &std::path::Path) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed");
    }

    /// Verify that `with_cleanup` removes the worktree on failure and returns
    /// the original error, and leaves the worktree intact on success.
    #[test]
    fn with_cleanup_removes_worktree_on_error_and_preserves_on_success() {
        let base = std::env::temp_dir().join(format!("devkit-co-wc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();

        git_cmd(&["init", "-q", "-b", "main"], &repo);
        git_cmd(&["config", "user.email", "t@t"], &repo);
        git_cmd(&["config", "user.name", "t"], &repo);
        std::fs::write(repo.join("f"), "x").unwrap();
        git_cmd(&["add", "."], &repo);
        git_cmd(&["commit", "-qm", "init"], &repo);

        // Error path: closure fails → worktree must be removed.
        let wt_err = base.join("wt-err");
        git_cmd(
            &[
                "worktree",
                "add",
                "--detach",
                wt_err.to_str().unwrap(),
                "HEAD",
            ],
            &repo,
        );
        assert!(wt_err.exists(), "worktree must exist before with_cleanup");

        let repo_s = repo.to_str().unwrap();
        let result = with_cleanup(&wt_err, repo_s, || -> Result<()> { anyhow::bail!("boom") });
        assert!(result.is_err());
        assert!(
            format!("{:#}", result.unwrap_err()).contains("boom"),
            "original error must propagate"
        );
        assert!(
            !wt_err.exists(),
            "worktree must be removed after a failed closure"
        );

        // Success path: closure succeeds → worktree must remain intact.
        let wt_ok = base.join("wt-ok");
        git_cmd(
            &[
                "worktree",
                "add",
                "--detach",
                wt_ok.to_str().unwrap(),
                "HEAD",
            ],
            &repo,
        );
        assert!(wt_ok.exists(), "worktree must exist before with_cleanup");

        let ok_result = with_cleanup(&wt_ok, repo_s, || -> Result<()> { Ok(()) });
        assert!(ok_result.is_ok(), "success must propagate");
        assert!(
            wt_ok.exists(),
            "worktree must remain after a successful closure"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    fn lref(id: &str, slug: &str) -> IssueRef {
        IssueRef {
            id: id.into(),
            slug: Some(slug.into()),
        }
    }

    fn tracker() -> devkit_common::tracker::linear::LinearTracker {
        devkit_common::tracker::linear::LinearTracker::new(Some("k".into()))
    }

    fn iref(id: &str, slug: Option<&str>) -> Ident {
        Ident::Issue(IssueRef {
            id: id.into(),
            slug: slug.map(str::to_string),
        })
    }

    #[test]
    fn classify_hash_is_pr() {
        assert_eq!(
            classify("#3340", &tracker()).unwrap(),
            Ident::Pr(github::PrLocator {
                repo: None,
                number: 3340
            })
        );
    }
    #[test]
    fn classify_github_url_is_pr() {
        assert_eq!(
            classify("https://github.com/o/r/pull/12", &tracker()).unwrap(),
            Ident::Pr(github::PrLocator {
                repo: Some("o/r".into()),
                number: 12
            })
        );
    }
    #[test]
    fn classify_prefix_is_an_issue() {
        assert_eq!(
            classify("eng-42", &tracker()).unwrap(),
            iref("ENG-42", None)
        );
    }
    /// The id shape is the tracker's to define, so an id with no dash-digits
    /// run reaches it instead of being rejected on sight.
    #[test]
    fn classify_defers_the_id_shape_to_the_tracker() {
        assert_eq!(classify("eng42", &tracker()).unwrap(), iref("ENG42", None));
    }
    #[test]
    fn classify_issue_url_is_an_issue() {
        assert_eq!(
            classify("https://linear.app/acme/issue/ENG-42/fix", &tracker()).unwrap(),
            iref("ENG-42", Some("fix"))
        );
    }
    /// A workspace whose name ends in `-<digits>` reads as an issue id, so the
    /// id has to come from the path position rather than the first match.
    #[test]
    fn classify_issue_url_ignores_a_workspace_named_like_an_id() {
        assert_eq!(
            classify("https://linear.app/acme-2/issue/ENG-42/fix", &tracker()).unwrap(),
            iref("ENG-42", Some("fix"))
        );
    }
    #[test]
    fn classify_issue_url_without_an_issue_segment_errors() {
        assert!(classify("https://linear.app/acme/team/ENG/active", &tracker()).is_err());
    }
    #[test]
    fn classify_bare_number_is_fuzzy() {
        assert_eq!(
            classify("3340", &tracker()).unwrap(),
            Ident::Fuzzy(github::PrLocator {
                repo: None,
                number: 3340
            })
        );
    }
    #[test]
    fn a_pasted_pr_url_keeps_its_repository() {
        // With one resolved repository the loss was invisible. With issues_repo
        // and pr_repo configured separately, pasting other/repo/pull/42 resolved
        // pr_repo#42 — a different pull request that happens to share a number —
        // and built a worktree from it without a word.
        let Ident::Pr(loc) = classify("https://github.com/other/repo/pull/42", &tracker()).unwrap()
        else {
            panic!("expected a PR")
        };
        assert_eq!(loc.repo.as_deref(), Some("other/repo"));
        assert_eq!(loc.number, 42);
    }
    #[test]
    fn a_bare_number_or_hash_defaults_to_pr_repo() {
        for input in ["#42", "42"] {
            let (Ident::Pr(loc) | Ident::Fuzzy(loc)) = classify(input, &tracker()).unwrap() else {
                panic!("expected a PR-shaped ident for {input}")
            };
            assert_eq!(loc.repo, None, "{input}");
            assert_eq!(loc.number, 42, "{input}");
        }
    }
    #[test]
    fn classify_garbage_errors() {
        assert!(classify("not an id", &tracker()).is_err());
    }

    #[test]
    fn fuzzy_none_errors() {
        assert_eq!(decide_fuzzy(false, &[], true), FuzzyDecision::ErrorNone);
    }
    #[test]
    fn fuzzy_pr_only() {
        assert_eq!(decide_fuzzy(true, &[], true), FuzzyDecision::UsePr);
    }
    #[test]
    fn fuzzy_single_linear() {
        assert_eq!(
            decide_fuzzy(false, &[lref("ENG-1", "a")], true),
            FuzzyDecision::UseTracker(lref("ENG-1", "a"))
        );
    }
    #[test]
    fn fuzzy_both_tty_prompts() {
        let cands = vec![lref("ENG-1", "a")];
        assert_eq!(
            decide_fuzzy(true, &cands, true),
            FuzzyDecision::Prompt(cands.clone())
        );
    }
    #[test]
    fn fuzzy_multi_linear_no_tty_is_ambiguous() {
        let cands = vec![lref("ENG-1", "a"), lref("OPS-1", "b")];
        assert_eq!(
            decide_fuzzy(false, &cands, false),
            FuzzyDecision::ErrorAmbiguous
        );
    }
    #[test]
    fn fuzzy_multi_linear_tty_prompts() {
        let cands = vec![lref("ENG-1", "a"), lref("OPS-1", "b")];
        assert_eq!(
            decide_fuzzy(false, &cands, true),
            FuzzyDecision::Prompt(cands.clone())
        );
    }
    #[test]
    fn fuzzy_both_no_tty_is_ambiguous() {
        assert_eq!(
            decide_fuzzy(true, &[lref("ENG-1", "a")], false),
            FuzzyDecision::ErrorAmbiguous
        );
    }

    #[test]
    fn a_linked_pr_is_checked_out_from_the_repository_its_url_names() {
        // With issues_repo and pr_repo configured separately, the tracker's
        // issue can name a PR outside pr_repo. Dropping the URL for the bare
        // number checked out pr_repo's PR of the same number instead.
        use devkit_common::tracker::fake;
        let t =
            fake::FakeTracker::new().with_pr("ENG-42", "https://github.com/other/repo/pull/7", 7);
        let r = resolve_issue("ENG-42", None, &t).unwrap();
        assert_eq!(r.loc.repo.as_deref(), Some("other/repo"));
        assert_eq!(r.loc.number, 7);
    }

    #[test]
    fn a_linked_pr_whose_url_does_not_parse_falls_back_to_its_number() {
        use devkit_common::tracker::fake;
        let t = fake::FakeTracker::new().with_pr("ENG-42", "not-a-pr-url", 7);
        let r = resolve_issue("ENG-42", None, &t).unwrap();
        assert_eq!(r.loc.repo, None);
        assert_eq!(r.loc.number, 7);
    }

    #[test]
    fn a_bare_number_asks_the_tracker_not_the_environment() {
        // The exported LINEAR_API_KEY of one project decided what a number meant in
        // another: the arm read the ambient key directly, so declaring
        // kind = "github" did not stop it.
        use devkit_common::tracker::fake;
        let gh = fake::FakeTracker::new().with_kind(TrackerKind::Github); // candidates() empty
        assert_eq!(
            decide_fuzzy_via(&gh, 42, /* pr_exists */ true, false),
            FuzzyDecision::UsePr
        );

        let lin = fake::FakeTracker::new().with_candidates(42, vec!["ENG-42"]);
        assert!(matches!(
            decide_fuzzy_via(&lin, 42, true, true),
            FuzzyDecision::Prompt(_)
        ));
    }

    #[test]
    fn record_issue_id_prefers_linear_then_head_ref() {
        assert_eq!(record_issue_id(Some("ENG-42"), "lev/eng-9-x"), "ENG-42");
        assert_eq!(record_issue_id(None, "lev/eng-9-fix"), "ENG-9");
        assert_eq!(record_issue_id(None, "no-id-here"), "UNKNOWN");
    }

    #[test]
    fn checkout_template_drops_linear_when_absent() {
        use devkit_config::Templates;
        let t = Templates::default();
        let pr_only = serde_json::json!({
            "pr_number": 3340, "pr_title": "fix-login", "linear_id": "", "linear_title": ""
        });
        assert_eq!(
            devkit_common::template::render(t.checkout_worktree_dir(), &pr_only, &t.variables)
                .unwrap(),
            "3340-fix-login"
        );
        let with_linear = serde_json::json!({
            "pr_number": 3340, "pr_title": "fix-login", "linear_id": "ENG-42", "linear_title": "x"
        });
        assert_eq!(
            devkit_common::template::render(t.checkout_worktree_dir(), &with_linear, &t.variables)
                .unwrap(),
            "3340-fix-login_[ENG-42]"
        );
    }
}
