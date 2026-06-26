use anyhow::{Context, Result};
use devkit_common::cmd::{capture, gh_json, git};
use devkit_common::linear::{self, LinearIssueRef};
use devkit_common::progress::Steps;
use devkit_ports::config::expand_tilde;
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

/// How the raw `<PR_LINEAR_ID_URL>` input is classified before resolution.
#[derive(Debug, PartialEq, Eq)]
enum Ident {
    Pr(u64),
    Linear(String),
    Fuzzy(u64),
}

/// Classify the identifier by shape alone (no network, no key knowledge).
fn classify(input: &str) -> Result<Ident> {
    let s = input.trim();
    if s.contains("github.com") && s.contains("/pull/") {
        let n = linear::pr_number_from_url(s).context("no PR number in GitHub URL")?;
        return Ok(Ident::Pr(n));
    }
    if s.contains("linear.app") {
        let id = devkit_common::worktree::find_id(s).context("no issue id in Linear URL")?;
        return Ok(Ident::Linear(id.to_uppercase()));
    }
    if let Some(rest) = s.strip_prefix('#')
        && !rest.is_empty()
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(Ident::Pr(rest.parse().context("bad PR number")?));
    }
    if let Some((a, b)) = s.split_once('-')
        && !a.is_empty()
        && a.chars().all(|c| c.is_ascii_alphabetic())
        && !b.is_empty()
        && b.chars().all(|c| c.is_ascii_digit())
    {
        return Ok(Ident::Linear(s.to_uppercase()));
    }
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        return Ok(Ident::Fuzzy(s.parse().context("bad number")?));
    }
    anyhow::bail!("unrecognized PR/Linear identifier: {s}");
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

/// Lowercase, collapse non-alphanumerics to single dashes, trim dashes.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
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

/// Turn a chosen Linear issue into a `Resolved`, erroring if it has no PR.
fn resolve_linear(id: &str, title: Option<String>, key: &str) -> Result<Resolved> {
    let (pr, fetched_title) = linear::issue_pr(id, key)?;
    let pr = pr.with_context(|| format!("Linear issue {id} has no associated PR to check out"))?;
    Ok(Resolved {
        pr_number: pr.number,
        linear_id: Some(id.to_string()),
        linear_title: Some(title.unwrap_or(fetched_title)),
    })
}

/// Resolve the raw input to a concrete PR. Network + interactive.
fn resolve(target: &str, key: Option<&str>, repo: &str, steps: &Steps) -> Result<Resolved> {
    match classify(target)? {
        Ident::Pr(n) => Ok(Resolved {
            pr_number: n,
            linear_id: None,
            linear_title: None,
        }),
        Ident::Linear(id) => {
            let key = key.context("Linear id given but LINEAR_API_KEY is not set")?;
            steps.during(&format!("Resolving Linear issue {id}…"), || {
                resolve_linear(&id, None, key)
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
            let (exists, candidates) = steps.during(&format!("Resolving {n}…"), || {
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
                FuzzyDecision::UseLinear(r) => resolve_linear(&r.id, Some(r.title), key),
                FuzzyDecision::Prompt(cands) => match prompt_choice(exists, &cands, n)? {
                    None => Ok(Resolved {
                        pr_number: n,
                        linear_id: None,
                        linear_title: None,
                    }),
                    Some(r) => resolve_linear(&r.id, Some(r.title), key),
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
    let steps = Steps::new();
    let resolved = resolve(&args.target, key.as_deref(), monorepo_s, &steps)?;

    let meta: PrMeta = steps
        .during(&format!("Fetching PR #{}…", resolved.pr_number), || {
            gh_json(
                &[
                    "pr",
                    "view",
                    &resolved.pr_number.to_string(),
                    "--json",
                    "number,title,headRefName",
                ],
                monorepo_s,
            )
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

    steps.during("Fetching from origin…", || {
        git(&["fetch", "origin"], monorepo_s)
    })?;
    steps.during("Creating worktree…", || {
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
            .during(&format!("Checking out PR #{}…", meta.number), || {
                capture(
                    "gh",
                    &["pr", "checkout", &meta.number.to_string()],
                    Some(worktree_s),
                )
            })
            .with_context(|| format!("checking out PR #{}", meta.number))?;

        let issue = record_issue_id(resolved.linear_id.as_deref(), &meta.head_ref_name);
        crate::record::write(
            &worktree,
            &crate::record::IssueRecord {
                issue: issue.clone(),
                slug: slugify(&meta.title),
                apps: if args.setup {
                    args.apps.clone()
                } else {
                    vec![]
                },
            },
        )?;
        Ok(issue)
    })?;

    if args.setup {
        let setup_ctx = serde_json::json!({
            "prefix": cfg.defaults.branch_prefix,
            "issue": issue,
            "slug": slugify(&meta.title),
            "apps": args.apps,
        });
        steps.during("Preparing apps…", || {
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

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "pr": meta.number,
            "branch": meta.head_ref_name,
            "worktree": worktree_s,
        }))?
    );
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

    #[test]
    fn classify_hash_is_pr() {
        assert_eq!(classify("#3340").unwrap(), Ident::Pr(3340));
    }
    #[test]
    fn classify_github_url_is_pr() {
        assert_eq!(
            classify("https://github.com/o/r/pull/12").unwrap(),
            Ident::Pr(12)
        );
    }
    #[test]
    fn classify_prefix_is_linear() {
        assert_eq!(classify("eng-42").unwrap(), Ident::Linear("ENG-42".into()));
    }
    #[test]
    fn classify_linear_url_is_linear() {
        assert_eq!(
            classify("https://linear.app/acme/issue/ENG-42/fix").unwrap(),
            Ident::Linear("ENG-42".into())
        );
    }
    #[test]
    fn classify_bare_number_is_fuzzy() {
        assert_eq!(classify("3340").unwrap(), Ident::Fuzzy(3340));
    }
    #[test]
    fn classify_garbage_errors() {
        assert!(classify("not an id").is_err());
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
    fn slugify_cleans_titles() {
        assert_eq!(slugify("Fix the Login!! page"), "fix-the-login-page");
        assert_eq!(slugify("  Trailing  "), "trailing");
        assert_eq!(slugify("ALL_CAPS"), "all-caps");
    }

    #[test]
    fn slugify_empty_and_all_special() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn checkout_template_drops_linear_when_absent() {
        use devkit_ports::config::Templates;
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
