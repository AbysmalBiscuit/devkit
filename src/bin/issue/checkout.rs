use crate::slug::slugify;
use anyhow::{Context, Result};
use devkit_common::cmd::{capture, gh_json, git};
use devkit_common::gitfetch;
use devkit_common::github;
use devkit_common::progress::Steps;
use devkit_common::tracker::linear::{self, LinearIssueRef};
use devkit_common::tracker::{IssueRef, Tracker};
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
    Pr(u64),
    Issue(IssueRef),
    Fuzzy(u64),
}

/// Classify the identifier by shape. The PR and bare-number rules are
/// tracker-independent; recognizing an issue id or issue URL is the tracker's.
fn classify(input: &str, t: &dyn Tracker) -> Result<Ident> {
    let s = input.trim();
    if s.contains("github.com") && s.contains("/pull/") {
        let n = github::pr_number_from_url(s).context("no PR number in GitHub URL")?;
        return Ok(Ident::Pr(n));
    }
    if let Some(rest) = s.strip_prefix('#')
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(Ident::Pr(rest.parse().context("bad PR number")?));
    }
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Ident::Fuzzy(s.parse().context("bad number")?));
    }
    if s.is_empty() || s.split_whitespace().count() != 1 {
        anyhow::bail!("unrecognized PR/issue identifier: {s}");
    }
    let r = t.issue_ref(s);
    // A tracker that cannot parse a URL hands the input straight back, and no
    // tracker's id contains a `/`, so a slash in the result means it failed.
    anyhow::ensure!(
        !r.id.is_empty() && !r.id.contains('/'),
        "unrecognized PR/issue identifier: {s}"
    );
    Ok(Ident::Issue(r))
}

/// The decision for a bare-number input after probing both sides.
#[derive(Debug, PartialEq, Eq)]
enum FuzzyDecision {
    UsePr,
    UseLinear(LinearIssueRef),
    Prompt(Vec<LinearIssueRef>),
    ErrorAmbiguous,
    ErrorNone,
}

fn decide_fuzzy(pr_exists: bool, candidates: &[LinearIssueRef], is_tty: bool) -> FuzzyDecision {
    match (pr_exists, candidates) {
        (false, []) => FuzzyDecision::ErrorNone,
        (true, []) => FuzzyDecision::UsePr,
        (false, [only]) => FuzzyDecision::UseLinear(only.clone()),
        _ if is_tty => FuzzyDecision::Prompt(candidates.to_vec()),
        _ => FuzzyDecision::ErrorAmbiguous,
    }
}

struct Resolved {
    pr_number: u64,
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
fn pr_exists(n: u64, repo: &str) -> Result<bool> {
    // Direct HTTP resolves existence from a 200/404; a clean 404 is `Ok(false)`.
    // Any HTTP failure (no token, transport) yields `None` → fall back to `gh`.
    if let Some(exists) = github::repo_slug(repo)
        .ok()
        .and_then(|slug| github::pr_exists(&slug, n).ok())
    {
        return Ok(exists);
    }
    match capture(
        "gh",
        &["pr", "view", &n.to_string(), "--json", "number"],
        Some(repo),
    ) {
        Ok(_) => Ok(true),
        Err(e) => {
            // `capture` embeds the command's stderr in its error message, so the
            // not-found signal is recoverable from the rendered error chain.
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
fn fetch_pr_meta(n: u64, cwd: &str) -> Result<PrMeta> {
    if let Some(m) = github::repo_slug(cwd)
        .ok()
        .and_then(|slug| github::pr_meta(&slug, n).ok())
    {
        return Ok(PrMeta {
            number: m.number,
            title: m.title,
            head_ref_name: m.head_ref_name,
        });
    }
    gh_json(
        &[
            "pr",
            "view",
            &n.to_string(),
            "--json",
            "number,title,headRefName",
        ],
        cwd,
    )
}

/// Turn a chosen issue into a `Resolved`, erroring if it has no PR. A title the
/// caller already holds is kept; otherwise the tracker is asked for one.
fn resolve_issue(id: &str, title: Option<String>, t: &dyn Tracker) -> Result<Resolved> {
    let pr = t
        .issue_pr(id)?
        .with_context(|| format!("issue {id} has no associated PR to check out"))?;
    let title = match title {
        Some(known) => Some(known),
        None => t.title(id)?,
    };
    Ok(Resolved {
        pr_number: pr.number,
        linear_id: Some(id.to_string()),
        linear_title: title,
    })
}

/// Resolve the raw input to a concrete PR. Network + interactive.
fn resolve(
    target: &str,
    key: Option<&str>,
    repo: &str,
    t: &dyn Tracker,
    steps: &Steps,
) -> Result<Resolved> {
    match classify(target, t)? {
        Ident::Pr(n) => Ok(Resolved {
            pr_number: n,
            linear_id: None,
            linear_title: None,
        }),
        Ident::Issue(r) => {
            anyhow::ensure!(t.ready(), "issue id given but no tracker is configured");
            steps.during_result(&format!("Resolving issue {}…", r.id), || {
                resolve_issue(&r.id, None, t)
            })
        }
        Ident::Fuzzy(n) => {
            // No Linear key → a bare number is a GitHub PR.
            let Some(key) = key else {
                return Ok(Resolved {
                    pr_number: n,
                    linear_id: None,
                    linear_title: None,
                });
            };
            // Probe both sides under a spinner; clear it before any prompt.
            let (exists, candidates) = steps.during_result(&format!("Resolving {n}…"), || {
                let exists = pr_exists(n, repo)?;
                let candidates = linear::issues_by_number(n, key)?;
                Ok::<_, anyhow::Error>((exists, candidates))
            })?;
            let is_tty = std::io::stdin().is_terminal();
            match decide_fuzzy(exists, &candidates, is_tty) {
                FuzzyDecision::ErrorNone => {
                    anyhow::bail!("no PR or Linear issue found for {n}")
                }
                FuzzyDecision::ErrorAmbiguous => anyhow::bail!(
                    "ambiguous {n} — rerun as #{n} (GitHub PR) or PREFIX-{n} (Linear)"
                ),
                FuzzyDecision::UsePr => Ok(Resolved {
                    pr_number: n,
                    linear_id: None,
                    linear_title: None,
                }),
                FuzzyDecision::UseLinear(r) => resolve_issue(&r.id, Some(r.title), t),
                FuzzyDecision::Prompt(cands) => match prompt_choice(exists, &cands, n)? {
                    None => Ok(Resolved {
                        pr_number: n,
                        linear_id: None,
                        linear_title: None,
                    }),
                    Some(r) => resolve_issue(&r.id, Some(r.title), t),
                },
            }
        }
    }
}

/// Print the options and read a choice. `Ok(None)` = the GitHub PR.
fn prompt_choice(
    pr_exists: bool,
    candidates: &[LinearIssueRef],
    n: u64,
) -> Result<Option<LinearIssueRef>> {
    println!("Multiple matches for {n}:");
    let mut options: Vec<Option<&LinearIssueRef>> = Vec::new();
    if pr_exists {
        options.push(None);
    }
    options.extend(candidates.iter().map(Some));
    for (i, opt) in options.iter().enumerate() {
        match opt {
            None => println!("  [{i}] GitHub PR #{n}"),
            Some(c) => println!("  [{i}] Linear {} — {}", c.id, c.title),
        }
    }
    print!("Choose [0]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    let idx: usize = line.trim().parse().unwrap_or(0);
    let chosen: Option<&LinearIssueRef> = *options.get(idx).context("choice out of range")?;
    Ok(chosen.cloned())
}

/// The id stored in `.devkit/issue.toml`: the Linear id if known, else the id
/// parsed from the PR head ref, else `UNKNOWN`.
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

    let key = devkit_common::secrets::resolve("LINEAR_API_KEY");
    let tracker = devkit_common::tracker::resolve(None, None, Path::new(&start));
    let steps = Steps::persistent();
    let resolved = resolve(
        &args.target,
        key.as_deref(),
        monorepo_s,
        tracker.as_ref(),
        &steps,
    )?;

    let meta: PrMeta = steps
        .during_result(&format!("Fetching PR #{}…", resolved.pr_number), || {
            fetch_pr_meta(resolved.pr_number, monorepo_s)
        })
        .with_context(|| format!("fetching PR #{}", resolved.pr_number))?;

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
                capture(
                    "gh",
                    &["pr", "checkout", &meta.number.to_string()],
                    Some(worktree_s),
                )
            })
            .with_context(|| format!("checking out PR #{}", meta.number))?;

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

    fn lref(id: &str, title: &str) -> LinearIssueRef {
        LinearIssueRef {
            id: id.into(),
            title: title.into(),
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
        assert_eq!(classify("#3340", &tracker()).unwrap(), Ident::Pr(3340));
    }
    #[test]
    fn classify_github_url_is_pr() {
        assert_eq!(
            classify("https://github.com/o/r/pull/12", &tracker()).unwrap(),
            Ident::Pr(12)
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
        assert_eq!(classify("3340", &tracker()).unwrap(), Ident::Fuzzy(3340));
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
            FuzzyDecision::UseLinear(lref("ENG-1", "a"))
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
