use anyhow::{Context, Result};
use devkit_common::cmd::capture;
use devkit_common::git::Git;
use devkit_common::gitfetch;
use devkit_common::progress::Steps;
use devkit_common::tracker::{IssueDetails, IssueRef, Resolved, Tracker};
use devkit_config::{PrepFile, expand_tilde};
use devkit_ports::load;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub struct SetupArgs {
    pub issue: String,
    /// `None` asks the resolved tracker for the issue title and slugifies that.
    pub slug: Option<String>,
    pub apps: Vec<String>,
    pub dry_run: bool,
    /// Also write the issue summary file named by `templates.issue_summary_path`.
    pub summary: bool,
    /// Skip that file for this run even when `defaults.issue_summary` is set.
    pub no_summary: bool,
    pub no_gitignore: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(serde::Serialize)]
struct Prepared {
    issue: String,
    worktree: String,
    branch: String,
    /// The summary file's path, present only when one was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

impl Prepared {
    /// A labelled table for a reader, the JSON a caller parses for anything
    /// else. Both carry the same fields; only a terminal gets the one whose
    /// paths can be double-clicked out of the line.
    fn report(&self) -> Result<()> {
        if !devkit_common::ui::stdout_is_tty() {
            println!("{}", serde_json::to_string_pretty(self)?);
            return Ok(());
        }
        let mut rows = vec![
            ("issue", self.issue.clone()),
            ("worktree", self.worktree.clone()),
            ("branch", self.branch.clone()),
        ];
        if let Some(s) = &self.summary {
            rows.push(("summary", s.clone()));
        }
        println!("{}", devkit_common::ui::kv_table(&rows));
        Ok(())
    }
}

/// Whether this run writes a summary file: each flag decides outright, and with
/// neither `defaults.issue_summary` does.
fn want_summary(args: &SetupArgs, cfg: &devkit_config::Config) -> bool {
    if args.summary {
        return true;
    }
    if args.no_summary {
        return false;
    }
    cfg.defaults.issue_summary
}

/// Write each prep file into `app_dir`. `content` is rendered as a minijinja
/// template against `ctx`/`vars` (strict undefined) before writing; parent
/// directories are created; an existing file is left untouched unless the entry
/// opts into `overwrite`. Only files that will be written are rendered.
fn write_prep_files(
    app_dir: &Path,
    files: &[PrepFile],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    for pf in files {
        let target = app_dir.join(&pf.path);
        if pf.overwrite || !target.exists() {
            let rendered = devkit_common::template::render(&pf.content, ctx, vars)
                .with_context(|| format!("rendering prep file `{}`", pf.path))?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent dir for prep file `{}`", pf.path))?;
            }
            std::fs::write(&target, &rendered)
                .with_context(|| format!("writing prep file `{}`", pf.path))?;
        }
    }
    Ok(())
}

/// Per-app bootstrap shared by `setup` and `checkout-pr --setup`: write each
/// app's prep files (rendered against `base_ctx` plus `app`/`branch`/`worktree`),
/// then run its setup commands in its directory.
pub(crate) fn prep_apps(
    worktree: &Path,
    branch: &str,
    apps: &[String],
    catalog: &HashMap<String, devkit_ports::apps::App>,
    base_ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    for a in apps {
        let app = &catalog[a];
        let app_dir = worktree.join(&app.path);
        std::fs::create_dir_all(&app_dir).ok();

        let mut file_ctx = base_ctx.clone();
        if let Some(obj) = file_ctx.as_object_mut() {
            obj.insert("app".into(), serde_json::Value::String(a.clone()));
            obj.insert(
                "branch".into(),
                serde_json::Value::String(branch.to_string()),
            );
            obj.insert(
                "worktree".into(),
                serde_json::Value::String(worktree.to_string_lossy().into_owned()),
            );
        }
        write_prep_files(&app_dir, &app.prep_files, &file_ctx, vars)
            .with_context(|| format!("preparing files for app `{a}`"))?;

        for cmd in &app.setup {
            let (prog, rest) = cmd.split_first().context("empty setup command")?;
            capture(
                prog,
                &rest.iter().map(String::as_str).collect::<Vec<_>>(),
                app_dir.to_str(),
            )
            .with_context(|| format!("running setup `{}` for app `{a}`", cmd.join(" ")))?;
        }
    }
    Ok(())
}

/// Width a hook's command is elided to in its progress step: sized for an
/// 80-column terminal alongside the mark, the `[i/n]` counter, and the
/// elapsed time.
const HOOK_LABEL_MAX: usize = 56;

/// Render one hook's argv against `ctx`/`vars`.
fn render_hook(
    hook: &[String],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    hook.iter()
        .map(|part| {
            devkit_common::template::render(part, ctx, vars)
                .with_context(|| format!("rendering hook argument `{part}`"))
        })
        .collect()
}

/// Run an already-rendered hook argv in `worktree`.
fn run_rendered(worktree: &Path, argv: &[String]) -> Result<()> {
    let (prog, rest) = argv.split_first().context("empty hook command")?;
    capture(
        prog,
        &rest.iter().map(String::as_str).collect::<Vec<_>>(),
        worktree.to_str(),
    )?;
    Ok(())
}

/// The progress-step label for a hook command.
fn hook_label(argv: &[String]) -> String {
    format!(
        "Hook: {}",
        devkit_common::ui::truncate(
            &argv.join(" ").replace(['\n', '\r', '\t'], " "),
            HOOK_LABEL_MAX
        )
    )
}

/// Run each `hooks.after_worktree_create` command in the new worktree, in
/// order, one progress step each. Fail-open: the worktree already exists and
/// is usable by the time these run, so a hook that fails warns on stderr and
/// the rest still run.
pub(crate) fn run_after_worktree_create(
    worktree: &Path,
    hooks: &[Vec<String>],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    steps: &Steps,
) {
    for hook in hooks {
        let rendered = render_hook(hook, ctx, vars);
        // A hook that cannot render still draws its step, labelled from the
        // template source so the offending argument is visible. The step
        // counter only advances inside `during_result`, so skipping the step
        // would leave the run ending short of its total.
        let label = hook_label(rendered.as_deref().unwrap_or(hook));
        if let Err(e) = steps.during_result(&label, || run_rendered(worktree, &rendered?)) {
            eprintln!(
                "warning: after_worktree_create hook `{}` failed: {e:#}",
                hook.join(" ")
            );
        }
    }
}

/// Copy the configured `worktree_include` globs from the primary checkout into
/// a freshly created worktree, printing each fail-open warning to stderr. A
/// no-op when the include list is empty.
pub fn backfill_includes(monorepo: &str, worktree: &std::path::Path, patterns: &[String]) {
    if patterns.is_empty() {
        return;
    }
    let (_copied, warnings) =
        devkit_common::worktree::copy_includes(std::path::Path::new(monorepo), worktree, patterns);
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

/// The explicit `--slug`, else the slug a pasted issue URL already carries,
/// else the issue's tracker title slugified. Only the last needs the network,
/// and `--summary` has already paid for it — `details` carries that title, so
/// the two never cost two round trips.
///
/// A derived slug is capped to `budget`; an explicit one is taken verbatim,
/// since a slug you typed is a decision, not a suggestion.
fn resolve_slug(
    t: &dyn Tracker,
    issue: &IssueRef,
    explicit: Option<String>,
    budget: usize,
    details: Option<&IssueDetails>,
) -> Result<String> {
    if let Some(s) = explicit {
        return Ok(s);
    }
    if let Some(s) = &issue.slug {
        return Ok(crate::issue::slug::cap(s, budget));
    }
    let title = match details {
        Some(d) => d.title.clone(),
        None => Steps::new()
            .during_result("Reading the issue title\u{2026}", || t.title(&issue.id))
            .with_context(|| format!("fetching the title for {}", issue.id))?
            .with_context(|| format!("no issue {} \u{2014} pass --slug", issue.id))?,
    };
    let slug = crate::issue::slug::cap(&crate::issue::slug::from_title(&issue.id, &title)?, budget);
    eprintln!("slug from {}: {slug}", t.kind().as_str());
    Ok(slug)
}

/// Every tracker fact the summary file needs, fetched before anything is
/// created. A summary with holes is worse than a clear failure, so an unknown
/// issue or an unreachable API stops `setup` here — while there is still no
/// worktree and no branch to clean up.
fn fetch_details(t: &dyn Tracker, issue: &str) -> Result<IssueDetails> {
    Steps::new()
        .during_result("Reading the issue\u{2026}", || t.details(issue))
        .with_context(|| format!("fetching issue {issue}"))?
        .with_context(|| format!("no issue {issue}"))
}

/// A slug this short has stopped being a reminder, so a `branch_prefix` long
/// enough to eat the whole budget overflows the column instead.
const MIN_SLUG: usize = 12;

/// How many characters a derived slug may use before the rendered branch
/// outgrows `ui::BRANCH_DISPLAY_MAX`.
///
/// Measured, not assumed: the branch template renders once with a
/// one-character slug, and whatever else it produced is the fixed cost. A longer
/// `branch_prefix`, a longer issue id, or a template that spells out more comes
/// out of the slug rather than overflowing the column.
fn slug_budget(
    cfg: &devkit_config::Config,
    vars: &BTreeMap<String, String>,
    issue: &str,
    apps: &[String],
) -> Result<usize> {
    let probe = serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": issue,
        "slug": "x",
        "apps": apps,
    });
    let fixed = devkit_common::template::render(cfg.templates.branch(), &probe, vars)
        .context("rendering `branch` template")?
        .trim()
        .chars()
        .count()
        .saturating_sub(1);
    Ok(devkit_common::ui::BRANCH_DISPLAY_MAX
        .saturating_sub(fixed)
        .max(MIN_SLUG))
}

/// A declared tracker owns parsing completely. An undeclared one keeps
/// today's permissive linear.app parse, which needs no key and would
/// otherwise be lost for a project that configured no tracker.
fn parse_input(resolved: &Resolved, input: &str) -> Result<IssueRef> {
    if resolved.declared {
        resolved.tracker.issue_ref(input)
    } else {
        Ok(crate::issue::slug::parse_issue_ref(input))
    }
}

pub fn run(args: SetupArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let cfg = &loaded.config;
    let catalog = &loaded.catalog;

    for a in &args.apps {
        anyhow::ensure!(catalog.contains_key(a), "unknown app `{a}`");
    }

    let repos = devkit_common::github::Repos::resolve(&cfg.github, &start, None);
    let resolved = devkit_common::tracker::resolve(cfg.tracker.kind, Path::new(&start), &repos);
    let t = resolved.tracker.as_ref();
    let issue_ref = parse_input(&resolved, &args.issue)?;
    let issue = issue_ref.id.clone();
    let vars = &cfg.templates.variables;
    let budget = slug_budget(cfg, vars, &issue, &args.apps)?;
    let details = want_summary(&args, cfg)
        .then(|| fetch_details(t, &issue))
        .transpose()?;
    let slug = resolve_slug(t, &issue_ref, args.slug.clone(), budget, details.as_ref())?;

    let wt_root = expand_tilde(&cfg.defaults.worktree_root);
    let ctx = serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": issue,
        "slug": slug,
        "apps": args.apps,
    });
    let branch = devkit_common::template::render(cfg.templates.branch(), &ctx, vars)
        .context("rendering `branch` template")?
        .trim()
        .to_string();
    let wt_name = devkit_common::template::render(cfg.templates.worktree_dir(), &ctx, vars)
        .context("rendering `worktree_dir` template")?
        .trim()
        .to_string();
    let worktree = wt_root.join(&wt_name);
    let monorepo = wt_root.join("monorepo");
    let holder = worktree.to_string_lossy().into_owned();

    let summary_path = details
        .as_ref()
        .map(|d| {
            crate::issue::summary::plan_path(cfg, d, &wt_root, &holder, &branch, &slug, &args.apps)
        })
        .transpose()?;

    if args.dry_run {
        let out = Prepared {
            issue: issue.clone(),
            worktree: holder,
            branch,
            summary: summary_path.map(|p| p.display().to_string()),
        };
        out.report()?;
        eprintln!("(dry-run: no worktree created)");
        return Ok(());
    }

    anyhow::ensure!(
        !worktree.exists(),
        "worktree path already exists: {}",
        worktree.display()
    );
    let monorepo_s = monorepo.to_str().context("monorepo path not UTF-8")?;
    let total = 2 + usize::from(!args.apps.is_empty()) + cfg.hooks.after_worktree_create.len();
    let steps = Steps::persistent_with_total(total);
    steps.during_result("Fetching from origin…", || {
        gitfetch::fetch("origin", monorepo_s)
    })?;
    if Git::at(Path::new(monorepo_s))
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .success()?
    {
        anyhow::bail!("branch {branch} already exists — let /issue-setup decide how to proceed");
    }
    steps.during_result("Creating worktree…", || {
        Git::at(Path::new(monorepo_s))
            .args([
                "worktree",
                "add",
                "-b",
                &branch,
                worktree.to_str().unwrap(),
                &cfg.defaults.baseline_ref,
            ])
            .timeout(devkit_common::git::SLOW_TIMEOUT)
            .output()
    })?;

    // The summary lands before the record so the record can name it: that is
    // how `issue end` knows which file to remove, whatever the path template
    // said at setup time.
    let summary_path = match &details {
        Some(d) => {
            let (path, written) = crate::issue::summary::write(
                cfg, d, &wt_root, &holder, &branch, &slug, &args.apps,
            )?;
            if !written {
                eprintln!("summary already exists, left untouched: {}", path.display());
            }
            Some(path.display().to_string())
        }
        None => None,
    };
    devkit_common::record::write(
        &worktree,
        &devkit_common::record::IssueRecord {
            issue: issue.clone(),
            slug: slug.clone(),
            apps: args.apps.clone(),
            summary: summary_path.clone(),
            // `issue setup` has no PR to record — there is none yet.
            pr: None,
        },
    )?;
    if !args.no_gitignore
        && let Err(e) = devkit_common::gitignore::ensure_devkit_ignored()
    {
        eprintln!("warning: could not update global gitignore: {e:#}");
    }

    backfill_includes(monorepo_s, &worktree, &cfg.defaults.worktree_include);

    // Per-app bootstrap: write the app's configured prep files, then run its
    // setup commands in its directory. Everything project-specific — filenames,
    // file contents, installs, doppler wiring — lives in config, not here.
    if args.apps.is_empty() {
        prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)?;
    } else {
        steps.during_result("Preparing apps…", || {
            prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)
        })?;
    }

    let mut hook_ctx = ctx.clone();
    if let Some(obj) = hook_ctx.as_object_mut() {
        obj.insert("branch".into(), serde_json::Value::String(branch.clone()));
        obj.insert("worktree".into(), serde_json::Value::String(holder.clone()));
    }

    // Ports are not reserved here. A worktree's servers get their ports
    // dynamically from `devrun up`, which allocates against the live registry at
    // start time — so the numbers always reflect what is actually free and no
    // unused reservation can be reclaimed by another session in the meantime.
    let out = Prepared {
        issue: issue.clone(),
        worktree: holder,
        branch,
        summary: summary_path,
    };
    // The worktree, its record, its includes and its apps are all in place by
    // now, so the table is not a premature claim. `suspend` hides the live bars
    // for the write: they draw on stderr and the table prints on stdout, and a
    // redraw would tear it.
    steps.suspend(|| out.report())?;

    run_after_worktree_create(
        &worktree,
        &cfg.hooks.after_worktree_create,
        &hook_ctx,
        vars,
        &steps,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_common::tracker::fake;
    use devkit_config::Templates;
    use serde_json::json;

    #[test]
    fn setup_takes_its_slug_from_the_tracker() {
        let t = fake::FakeTracker::new().with_title("ENG-7", "Fix the export crash");
        let r = resolve_slug(
            &t,
            &IssueRef {
                id: "ENG-7".into(),
                slug: None,
            },
            None,
            40,
            None,
        )
        .unwrap();
        assert_eq!(r, "fix-the-export-crash");
    }

    #[test]
    fn a_declared_trackers_refusal_propagates() {
        let refused = "https://github.com/other/repo/issues/9";
        let resolved = Resolved {
            tracker: Box::new(fake::FakeTracker::new().refusing(refused)),
            declared: true,
            reason: "test".into(),
        };
        assert!(parse_input(&resolved, refused).is_err());
    }

    #[test]
    fn an_undeclared_project_still_reads_a_linear_url_without_a_key() {
        // A tracker that would refuse or need a key is never asked when the
        // project declared none — the permissive linear.app parse stands in.
        let resolved = Resolved {
            tracker: Box::new(fake::FakeTracker::new()),
            declared: false,
            reason: "test".into(),
        };
        let parsed = parse_input(
            &resolved,
            "https://linear.app/acme/issue/ENG-1234/fix-bli-export",
        )
        .unwrap();
        assert_eq!(parsed.id, "ENG-1234");
        assert_eq!(parsed.slug.as_deref(), Some("fix-bli-export"));
    }

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn ctx() -> serde_json::Value {
        json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix", "apps": ["web"], "app": "web"})
    }

    #[test]
    fn render_hook_expands_each_argument() {
        let hook = vec![
            "git".to_string(),
            "init".to_string(),
            "{{ slug }}-wt".to_string(),
        ];
        let argv = render_hook(&hook, &ctx(), &novars()).unwrap();
        assert_eq!(argv, vec!["git", "init", "fix-wt"]);
    }

    #[test]
    fn render_hook_reports_the_argument_it_could_not_render() {
        let hook = vec!["git".to_string(), "{{ nope }}".to_string()];
        let err = render_hook(&hook, &ctx(), &novars()).unwrap_err();
        assert!(
            format!("{err:#}").contains("{{ nope }}"),
            "the error names the offending argument: {err:#}"
        );
    }

    #[test]
    fn hook_label_keeps_a_short_command_whole() {
        let argv = vec!["bun".to_string(), "install".to_string()];
        assert_eq!(hook_label(&argv), "Hook: bun install");
    }

    #[test]
    fn hook_label_elides_a_long_command() {
        let argv = vec!["bash".to_string(), "-c".to_string(), "x".repeat(200)];
        let label = hook_label(&argv);
        assert!(label.starts_with("Hook: bash -c "), "label was {label}");
        assert!(label.ends_with('…'), "label was {label}");
        assert_eq!(
            label.chars().count(),
            "Hook: ".chars().count() + HOOK_LABEL_MAX
        );
    }

    #[test]
    fn hook_label_collapses_embedded_newlines() {
        let argv = vec![
            "bash".to_string(),
            "-c".to_string(),
            "echo one\necho two".to_string(),
        ];
        let label = hook_label(&argv);
        assert!(!label.contains('\n'), "label was {label:?}");
        assert_eq!(label, "Hook: bash -c echo one\necho two".replace('\n', " "));
    }

    #[test]
    fn hook_renders_args_and_runs_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![vec![
            "git".to_string(),
            "init".to_string(),
            "{{ slug }}-wt".to_string(),
        ]];
        run_after_worktree_create(dir.path(), &hooks, &ctx(), &novars(), &Steps::persistent());
        assert!(dir.path().join("fix-wt").exists());
    }

    #[test]
    fn failing_hook_does_not_stop_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec!["devkit-no-such-program-xyz".to_string()],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        run_after_worktree_create(dir.path(), &hooks, &ctx(), &novars(), &Steps::persistent());
        assert!(dir.path().join("after").exists());
    }

    #[test]
    fn unrenderable_hook_does_not_stop_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec![
                "git".to_string(),
                "init".to_string(),
                "{{ nope }}".to_string(),
            ],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        run_after_worktree_create(dir.path(), &hooks, &ctx(), &novars(), &Steps::persistent());
        assert!(dir.path().join("after").exists());
    }

    #[test]
    fn every_hook_consumes_a_step_even_when_it_cannot_render() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec![
                "git".to_string(),
                "init".to_string(),
                "{{ nope }}".to_string(),
            ],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        let steps = Steps::persistent_with_total(hooks.len());
        run_after_worktree_create(dir.path(), &hooks, &ctx(), &novars(), &steps);
        assert_eq!(
            steps.started(),
            2,
            "an unrenderable hook must still consume its step"
        );
        assert!(dir.path().join("after").exists(), "the next hook still ran");
    }

    #[test]
    fn renders_issue_context() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "ISSUE={{ issue }}\n".into(),
            overwrite: false,
        }];
        write_prep_files(dir.path(), &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".env.local")).unwrap(),
            "ISSUE=eng-1\n"
        );
    }

    #[test]
    fn default_branch_renders_prefix_and_slug() {
        let t = Templates::default();
        let ctx = json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix"});
        let out = devkit_common::template::render(t.branch(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "lev/fix");
    }

    #[test]
    fn default_worktree_dir_renders_slug() {
        let t = Templates::default();
        let ctx = json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix"});
        let out = devkit_common::template::render(t.worktree_dir(), &ctx, &t.variables).unwrap();
        assert_eq!(out, "fix");
    }

    #[test]
    fn writes_content_verbatim_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![PrepFile {
            path: "config/local.json".into(),
            content: "{\"mode\":\"local\"}\n".into(),
            overwrite: false,
        }];
        write_prep_files(dir.path(), &files, &ctx(), &novars()).unwrap();
        let got = std::fs::read_to_string(dir.path().join("config/local.json")).unwrap();
        assert_eq!(got, "{\"mode\":\"local\"}\n");
    }

    #[test]
    fn write_if_absent_preserves_existing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.local"), "ORIGINAL\n").unwrap();
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "REPLACED\n".into(),
            overwrite: false,
        }];
        write_prep_files(dir.path(), &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".env.local")).unwrap(),
            "ORIGINAL\n"
        );
    }

    #[test]
    fn overwrite_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.local"), "ORIGINAL\n").unwrap();
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "REPLACED\n".into(),
            overwrite: true,
        }];
        write_prep_files(dir.path(), &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".env.local")).unwrap(),
            "REPLACED\n"
        );
    }

    #[test]
    fn renders_app_name() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![PrepFile {
            path: "app.txt".into(),
            content: "{{ app }}".into(),
            overwrite: false,
        }];
        write_prep_files(dir.path(), &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("app.txt")).unwrap(),
            "web"
        );
    }

    #[test]
    fn unknown_var_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "{{ nope }}".into(),
            overwrite: false,
        }];
        assert!(write_prep_files(dir.path(), &files, &ctx(), &novars()).is_err());
    }

    #[test]
    fn skipped_existing_file_is_not_rendered() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env.local"), "ORIGINAL\n").unwrap();
        // A malformed template on an existing, non-overwrite file must not be
        // rendered (and so must not error) — the file is left untouched.
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "{{ nope }}".into(),
            overwrite: false,
        }];
        write_prep_files(dir.path(), &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".env.local")).unwrap(),
            "ORIGINAL\n"
        );
    }
}
