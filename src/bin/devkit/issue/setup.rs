use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

use anyhow::{Context, Result};
use devkit_common::{
    cmd::capture,
    gitfetch,
    progress::Steps,
    record::RecordOrigin,
    tracker::{IssueDetails, IssueRef, Resolved, Tracker},
    vcs::{NewWorktree, Vcs, VersionControl},
};
use devkit_config::{IssueEvent, PrepFile, expand_tilde};
use devkit_ports::load;

pub struct SetupArgs {
    /// `None` is work with no tracker issue; `slug` then names the worktree.
    pub issue: Option<String>,
    /// `None` asks the resolved tracker for the issue title and slugifies that.
    pub slug: Option<String>,
    pub apps: Vec<String>,
    pub dry_run: bool,
    /// Also write the issue summary file named by
    /// `templates.issue_summary_path`.
    pub summary: bool,
    /// Skip that file for this run even when `defaults.issue_summary` is set.
    pub no_summary: bool,
    pub no_gitignore: bool,
    /// Bind the checkout `dir` sits in instead of creating a worktree.
    pub here: bool,
    pub dir: Option<String>,
    pub config: Option<String>,
}

#[derive(serde::Serialize)]
struct Prepared {
    #[serde(skip_serializing_if = "Option::is_none")]
    issue: Option<String>,
    worktree: String,
    branch: String,
    /// The summary file's path, present only when one was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    /// The text the summary file gets, present only on `--dry-run --summary`,
    /// which writes no file to hold it.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_text: Option<String>,
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
        println!("{}", self.terminal_report());
        Ok(())
    }

    /// The terminal form. The summary text is the tracker's issue body, so each
    /// line is made printable; its line breaks stay real.
    fn terminal_report(&self) -> String {
        let mut rows: Vec<_> = self.issue.iter().map(|i| ("issue", i.clone())).collect();
        rows.push(("worktree", self.worktree.clone()));
        rows.push(("branch", self.branch.clone()));
        if let Some(s) = &self.summary {
            rows.push(("summary", s.clone()));
        }
        let mut out = devkit_common::ui::kv_table(&rows).to_string();
        if let Some(text) = &self.summary_text {
            let lines: Vec<_> = text.lines().map(devkit_common::ui::printable).collect();
            out.push_str("\n\n");
            out.push_str(&lines.join("\n"));
        }
        out
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

/// Per-app bootstrap shared by `setup` and `pr checkout --setup`: write each
/// app's prep files (rendered against `base_ctx` plus
/// `app`/`branch`/`worktree`), then run its setup commands in its directory.
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

/// Run each `hooks.after_worktree_create` command in the new worktree, in
/// order, with `env` added to each child's environment. The worktree already
/// exists and is usable by the time these run.
pub(crate) fn run_after_worktree_create(
    worktree: &Path,
    hooks: &[Vec<String>],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    env: &[(&str, &str)],
    steps: &Steps,
) {
    crate::issue::hooks::run_all(
        worktree,
        "after_worktree_create",
        hooks,
        ctx,
        vars,
        env,
        steps,
    );
}

/// How many files may pass before the transient line is redrawn. A large
/// include list fires one event per file, and every redraw allocates a message.
const INCLUDE_REDRAW_EVERY: usize = 64;

/// Draws an include backfill's events as sub-steps of one `Step`. Sub-step
/// numbers are a display concern, so the offset the discovery sub-step
/// introduces is applied here rather than in the event stream.
///
/// The event callback type is `Sync` and every method takes `&self`, so the
/// counters are atomic to match, whether or not a given caller drives them
/// from more than one thread.
struct IncludeRender<'a> {
    step: &'a devkit_common::progress::Step<'a>,
    discovery: bool,
    subs: usize,
    entry: std::sync::atomic::AtomicUsize,
    drawn: std::sync::atomic::AtomicUsize,
}

impl IncludeRender<'_> {
    fn on(&self, event: devkit_common::worktree::IncludeEvent<'_>) {
        use std::sync::atomic::Ordering::Relaxed;

        use devkit_common::worktree::IncludeEvent as E;
        match event {
            E::Found { files } => {
                if self.discovery && self.due(files) {
                    self.step.activity(&format!(
                        "[1/{}] discovering files... ({files} found)",
                        self.subs
                    ));
                }
            }
            E::ScanDone { files } => {
                if self.discovery {
                    self.step.substep(&format!(
                        "1/{} discovering files ({files} found)",
                        self.subs
                    ));
                }
            }
            E::EntryStart {
                pattern,
                index,
                files,
                ..
            } => {
                self.entry.store(self.number(index), Relaxed);
                self.drawn.store(0, Relaxed);
                self.step.activity(&format!(
                    "[{}/{}] {pattern} 0/{files}",
                    self.number(index),
                    self.subs
                ));
            }
            E::FileDone { pattern, done, of } => {
                if self.due(done) || done == of {
                    self.step.activity(&format!(
                        "[{}/{}] {pattern} {done}/{of}",
                        self.entry.load(Relaxed),
                        self.subs
                    ));
                }
            }
            E::EntryDone { pattern, index, .. } => {
                self.step
                    .substep(&format!("{}/{} {pattern}", self.number(index), self.subs));
            }
        }
    }

    /// Whether `count` has moved far enough since the last redraw to be worth
    /// another one. A `true` result records `count` as the new redraw cursor.
    /// A caller may still draw on a `false` result, e.g. the last file of a
    /// pattern, and such a draw does not move the cursor.
    fn due(&self, count: usize) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        if count.saturating_sub(self.drawn.load(Relaxed)) < INCLUDE_REDRAW_EVERY {
            return false;
        }
        self.drawn.store(count, Relaxed);
        true
    }

    /// A pattern's display number, offset past the discovery sub-step when
    /// there is one.
    fn number(&self, index: usize) -> usize {
        index + 1 + usize::from(self.discovery)
    }
}

/// Copy the configured `worktree_include` globs from the primary checkout into
/// a freshly created worktree under a step of its own, one sub-step per include
/// entry. Fail-open warnings print to stderr after the step settles, so a live
/// bar cannot tear them. A no-op that draws nothing when the include list is
/// empty.
pub fn backfill_includes(
    primary: &str,
    worktree: &std::path::Path,
    patterns: &[String],
    steps: &Steps,
) {
    if patterns.is_empty() {
        return;
    }
    let discovery = devkit_common::worktree::needs_discovery(patterns);
    let subs = patterns.len() + usize::from(discovery);

    let warnings = steps.during_step("Copying worktree includes...", |step| {
        let render = IncludeRender {
            step,
            discovery,
            subs,
            entry: std::sync::atomic::AtomicUsize::new(0),
            drawn: std::sync::atomic::AtomicUsize::new(0),
        };
        if discovery {
            step.activity(&format!("[1/{subs}] discovering files... (0 found)"));
        }
        let (copied, linked, warnings) = devkit_common::worktree::copy_includes_with(
            std::path::Path::new(primary),
            worktree,
            patterns,
            &|e| render.on(e),
        );
        let summary = super::sync::counts(copied, linked);
        step.detail(&if summary.is_empty() {
            "0 files".to_string()
        } else {
            summary.join(", ")
        });
        warnings
    });

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
    issue: Option<&IssueRef>,
    explicit: Option<String>,
    budget: usize,
    details: Option<&IssueDetails>,
) -> Result<String> {
    if let Some(s) = explicit {
        return Ok(s);
    }
    let issue = issue.context("pass an issue or --slug")?;
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

/// Render context both templates are measured against. The two slug values are
/// the ones being measured, so a caller passes whichever it already knows and
/// the empty string for the other.
fn probe_ctx(
    cfg: &devkit_config::Config,
    issue: &str,
    apps: &[String],
    slug: &str,
    short_slug: &str,
) -> serde_json::Value {
    serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": issue,
        "slug": slug,
        "short_slug": short_slug,
        "apps": apps,
    })
}

/// Characters the `branch` template leaves for the slug it renders. A template
/// whose fixed text fills `branch_max` falls back to `MIN_SLUG` rather than
/// failing, since an over-long branch is elided by the status table and
/// nothing worse.
///
/// A `branch` template that does not render `{{ slug }}` at all measures as
/// unconstrained, so the result is clamped to `branch_max`: `slug` also feeds
/// the worktree directory, the issue record, `prep_apps`, and
/// `after_worktree_create`, and none of those get a bound from this template.
fn branch_budget(
    cfg: &devkit_config::Config,
    vars: &BTreeMap<String, String>,
    issue: &str,
    apps: &[String],
) -> Result<usize> {
    let budget = crate::issue::slug::budget(
        cfg.templates.branch(),
        &probe_ctx(cfg, issue, apps, "", ""),
        vars,
        &["slug"],
        cfg.templates.branch_max(),
        "branch_max",
        Some(MIN_SLUG),
    )
    .context("measuring the `branch` template")?;
    Ok(budget.min(cfg.templates.branch_max()))
}

/// `slug` shortened again to whatever the `worktree_dir` template leaves for
/// `{{ short_slug }}`, and `slug` unchanged when that template does not render
/// it, which is the shipped default. Unlike the branch, this one fails rather
/// than overrunning: the directory name is charged against a filesystem path
/// limit.
fn short_slug(
    cfg: &devkit_config::Config,
    vars: &BTreeMap<String, String>,
    issue: &str,
    apps: &[String],
    slug: &str,
) -> Result<String> {
    let budget = crate::issue::slug::budget(
        cfg.templates.worktree_dir(),
        &probe_ctx(cfg, issue, apps, slug, ""),
        vars,
        &["short_slug"],
        cfg.templates.worktree_dir_max(),
        "worktree_dir_max",
        None,
    )
    .context("measuring the `worktree_dir` template")?;
    Ok(crate::issue::slug::cap(slug, budget))
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

/// The directory worktrees are placed under. Empty means devkit derived none
/// — a bare main worktree has no checkout to derive a sibling of — and joining
/// a name onto an empty root yields a bare relative path. `git -C <primary>
/// worktree add` resolves that against the primary checkout, so the worktree
/// and its branch would be created *inside* it. Refuse instead, naming the key
/// that settles it.
pub fn worktree_root(cfg: &devkit_config::Config) -> Result<std::path::PathBuf> {
    anyhow::ensure!(
        !cfg.defaults.worktree_root.is_empty(),
        "set `defaults.worktree_root`: devkit cannot derive one for a bare main worktree"
    );
    Ok(expand_tilde(&cfg.defaults.worktree_root))
}

/// `rev` as the local branch it names: `origin/main` and
/// `refs/remotes/origin/main` are both `main`, and so is `upstream/main` when
/// `upstream` is a remote of the checkout at `root`.
fn local_branch<'a>(vcs: &Vcs, root: &Path, rev: &'a str) -> &'a str {
    let rev = rev
        .strip_prefix("refs/remotes/")
        .or_else(|| rev.strip_prefix("refs/heads/"))
        .unwrap_or(rev);
    match rev.split_once('/') {
        Some((remote, branch))
            if remote == "origin"
                || vcs
                    .remote_url(root, remote)
                    .is_ok_and(|url| !url.is_empty()) =>
        {
            branch
        }
        _ => rev,
    }
}

/// Refuse to bind `branch` when it is the repository's default branch, which
/// `origin/HEAD` and `defaults.baseline_ref` each name, or when no branch is
/// checked out. The primary checkout's branch is shared by every session, so
/// binding it to one issue would hand that issue to all of them.
fn refuse_default_branch(
    cfg: &devkit_config::Config,
    vcs: &Vcs,
    root: &Path,
    branch: &str,
) -> Result<()> {
    anyhow::ensure!(
        branch != devkit_common::vcs::DETACHED,
        "`workspace setup --here` binds a branch, and HEAD is detached: check out a feature branch first"
    );
    let defaults: Vec<String> = [
        vcs.default_branch(root).ok(),
        Some(cfg.defaults.baseline_ref.clone()).filter(|r| !r.is_empty()),
    ]
    .into_iter()
    .flatten()
    .map(|r| local_branch(vcs, root, &r).to_string())
    .collect();
    anyhow::ensure!(
        !defaults.is_empty(),
        "cannot tell which branch is the default: set `defaults.baseline_ref`, \
         or run `git remote set-head origin -a` so origin/HEAD names one"
    );
    anyhow::ensure!(
        !defaults.iter().any(|d| d == branch),
        "refusing to bind the default branch `{branch}` to an issue: check out a feature branch first"
    );
    Ok(())
}

/// `workspace setup --here`: bind the checkout `start` sits in to `issue` on
/// the branch it already has. Writes the record and, when asked, the summary;
/// creates no branch or worktree and runs no worktree hooks. Running it again
/// for the same issue refreshes the record, and for another issue refuses.
fn bind_here(
    args: &SetupArgs,
    cfg: &devkit_config::Config,
    start: &str,
    t: &dyn Tracker,
    issue: &str,
    slug: String,
    details: Option<IssueDetails>,
) -> Result<Prepared> {
    let root = devkit_common::vcs::checkout_root(Path::new(start))?;
    let vcs = Vcs::at(&root);
    let branch = vcs.branch(&root)?;
    refuse_default_branch(cfg, &vcs, &root, &branch)?;
    // A `pr checkout` record marks a reviewer's checkout of someone else's
    // PR, whose work an issue binding would claim as this issue's.
    let refuse_bound = |bound: &devkit_common::record::IssueRecord| -> Result<()> {
        anyhow::ensure!(
            bound.origin != Some(RecordOrigin::Checkout),
            "{} is a review checkout `pr checkout` made, not work on an issue",
            root.display()
        );
        anyhow::ensure!(
            bound.issue == issue,
            "{} is already bound to issue `{}`",
            root.display(),
            bound.issue
        );
        Ok(())
    };
    // Checked again under the lock; refusing here keeps a refused run from
    // writing the summary.
    if let Some(bound) = devkit_common::record::read(&root) {
        refuse_bound(&bound)?;
    }
    let holder = root.to_string_lossy().into_owned();

    if args.dry_run {
        let out = dry_run_report(args, cfg, t, details.as_ref(), Planned {
            issue: Some(issue.to_string()),
            worktree: holder,
            branch,
            slug: &slug,
            apps: &[],
        })?;
        eprintln!("(dry-run: nothing written)");
        return Ok(out);
    }

    let summary = match &details {
        Some(d) => {
            let tracker_summary = read_tracker_summary(t, issue)?;
            let (path, written) = crate::issue::summary::write(
                cfg,
                d,
                tracker_summary.as_deref(),
                &holder,
                &branch,
                &slug,
                &[],
            )?;
            if !written {
                eprintln!("summary already exists, left untouched: {}", path.display());
            }
            Some(path.display().to_string())
        }
        None => None,
    };
    let setup_event = cfg.ticket.events.setup.is_some();
    let fires_setup = devkit_common::record::update(&root, |rec| {
        if let Some(bound) = rec.as_ref() {
            refuse_bound(bound)?;
        }
        let rec = rec.get_or_insert_with(|| devkit_common::record::IssueRecord {
            issue: issue.to_string(),
            slug: slug.clone(),
            origin: Some(RecordOrigin::Setup),
            events: Some(vec![]),
            ..Default::default()
        });
        rec.slug = slug.clone();
        rec.branch = Some(branch.clone());
        if summary.is_some() {
            rec.summary = summary.clone();
        }
        anyhow::Ok(setup_event && rec.claim(IssueEvent::Setup))
    })??;
    if !args.no_gitignore {
        ignore_devkit_files();
    }

    let out = Prepared {
        issue: Some(issue.to_string()),
        worktree: holder,
        branch,
        summary,
        summary_text: None,
    };
    out.report()?;
    if fires_setup {
        crate::issue::event::fire_inline(
            Path::new(start),
            args.config.as_deref().map(Path::new),
            IssueEvent::Setup,
            issue,
        );
    }
    Ok(out)
}

/// The tracker's own summary of `issue`, which a summary file holds verbatim
/// in place of the rendered template.
fn read_tracker_summary(t: &dyn Tracker, issue: &str) -> Result<Option<String>> {
    Steps::new()
        .during_result("Reading the issue summary\u{2026}", || t.summary(issue))
        .with_context(|| format!("fetching the summary for {issue}"))
}

/// What a dry run reports about the worktree it would set up.
struct Planned<'a> {
    issue: Option<String>,
    worktree: String,
    branch: String,
    slug: &'a str,
    apps: &'a [String],
}

/// Print and return what `--dry-run` would set up. With `--summary` that
/// includes the summary file's text, rendered exactly as a real run writes it,
/// which costs the tracker summary read a real run makes; without it, the
/// tracker is asked nothing more.
fn dry_run_report(
    args: &SetupArgs,
    cfg: &devkit_config::Config,
    t: &dyn Tracker,
    details: Option<&IssueDetails>,
    plan: Planned<'_>,
) -> Result<Prepared> {
    let Planned {
        issue,
        worktree,
        branch,
        slug,
        apps,
    } = plan;
    let summary = details
        .map(|d| crate::issue::summary::plan_path(cfg, d, &worktree, &branch, slug, apps))
        .transpose()?;
    let summary_text = match (details.filter(|_| args.summary), issue.as_deref()) {
        (Some(d), Some(id)) => {
            let tracker_summary = read_tracker_summary(t, id)?;
            Some(crate::issue::summary::text(
                cfg,
                d,
                tracker_summary.as_deref(),
                &worktree,
                &branch,
                slug,
                apps,
            )?)
        }
        _ => None,
    };
    let out = Prepared {
        issue,
        worktree,
        branch,
        summary: summary.map(|p| p.display().to_string()),
        summary_text,
    };
    out.report()?;
    Ok(out)
}

pub fn run(args: SetupArgs) -> Result<()> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    let loaded = load::load(args.config.as_deref().map(Path::new), Path::new(&start))?;
    let cfg = &loaded.config;
    let forge = devkit_common::forge::resolve(&cfg.forge, &cfg.github, &start, None);
    let resolved =
        devkit_common::tracker::resolve(cfg.tracker.kind, Path::new(&start), &forge.repos);
    setup(&args, cfg, &loaded.catalog, &resolved).map(drop)
}

/// `workspace setup` against an already loaded config and resolved tracker,
/// returning what it reported.
fn setup(
    args: &SetupArgs,
    cfg: &devkit_config::Config,
    catalog: &HashMap<String, devkit_ports::apps::App>,
    resolved: &Resolved,
) -> Result<Prepared> {
    let start = args.dir.clone().unwrap_or_else(|| ".".to_string());
    for a in &args.apps {
        anyhow::ensure!(catalog.contains_key(a), "unknown app `{a}`");
    }
    let t = resolved.tracker.as_ref();
    let issue_ref = args
        .issue
        .as_deref()
        .map(|i| parse_input(resolved, i))
        .transpose()?;
    anyhow::ensure!(
        issue_ref.is_some() || !args.summary,
        "--summary needs an issue: the summary is built from the tracker's issue"
    );
    // An empty id is how templates and the record spell "no tracker issue".
    let issue = issue_ref.as_ref().map(|r| r.id.clone()).unwrap_or_default();
    let vars = &cfg.templates.defaults();
    let budget = branch_budget(cfg, vars, &issue, &args.apps)?;
    let details = (issue_ref.is_some() && want_summary(args, cfg))
        .then(|| fetch_details(t, &issue))
        .transpose()?;
    let slug = resolve_slug(
        t,
        issue_ref.as_ref(),
        args.slug.clone(),
        budget,
        details.as_ref(),
    )?;
    if args.here {
        return bind_here(args, cfg, &start, t, &issue, slug, details);
    }
    let dir_slug = short_slug(cfg, vars, &issue, &args.apps, &slug)?;

    let wt_root = worktree_root(cfg)?;
    let ctx = serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": issue,
        "slug": slug,
        "short_slug": dir_slug,
        "apps": args.apps,
        "role": "issue",
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
    let holder = worktree.to_string_lossy().into_owned();

    if args.dry_run {
        let out = dry_run_report(args, cfg, t, details.as_ref(), Planned {
            issue: issue_ref.map(|r| r.id),
            worktree: holder,
            branch,
            slug: &slug,
            apps: &args.apps,
        })?;
        eprintln!("(dry-run: no worktree created)");
        return Ok(out);
    }

    // Resolved before anything is created, so a summary path that cannot be
    // rendered fails while there is no worktree to clean up.
    if let Some(d) = &details {
        crate::issue::summary::plan_path(cfg, d, &holder, &branch, &slug, &args.apps)?;
    }
    anyhow::ensure!(
        !worktree.exists(),
        "worktree path already exists: {}",
        worktree.display()
    );
    let tracker_summary = details
        .is_some()
        .then(|| read_tracker_summary(t, &issue))
        .transpose()?
        .flatten();
    let primary = devkit_common::vcs::primary_checkout(Path::new(&start))?;
    let primary_s = primary
        .to_str()
        .context("primary checkout path not UTF-8")?;
    let baseline_target = crate::baseline::target(cfg, &primary)?;
    let total = 2
        + usize::from(!args.apps.is_empty())
        + cfg.hooks.after_worktree_create.len()
        + usize::from(!cfg.defaults.worktree_include.is_empty());
    let steps = Steps::persistent_with_total(total);
    steps.during_result("Fetching from origin...", || {
        gitfetch::fetch("origin", primary_s)
    })?;
    let vcs = Vcs::at(&primary);
    if vcs.has_branch(&primary, &branch)? {
        anyhow::bail!("branch {branch} already exists — let /issue-setup decide how to proceed");
    }
    steps.during_result("Creating worktree...", || {
        vcs.create_worktree(&NewWorktree {
            main: &primary,
            path: &worktree,
            start: &baseline_target,
            branch: Some(&branch),
        })
    })?;

    // The summary lands before the record so the record can name it: that is
    // how `workspace end` knows which file to remove, whatever the path
    // template said at setup time.
    let summary_path = match &details {
        Some(d) => {
            let (path, written) = crate::issue::summary::write(
                cfg,
                d,
                tracker_summary.as_deref(),
                &holder,
                &branch,
                &slug,
                &args.apps,
            )?;
            if !written {
                eprintln!("summary already exists, left untouched: {}", path.display());
            }
            Some(path.display().to_string())
        }
        None => None,
    };
    // The setup event is claimed in the record's first write, so firing it
    // costs no second one.
    let fires_setup = !issue.is_empty() && cfg.ticket.events.setup.is_some();
    devkit_common::record::write(&worktree, &devkit_common::record::IssueRecord {
        issue: issue.clone(),
        slug: slug.clone(),
        branch: Some(branch.clone()),
        apps: args.apps.clone(),
        summary: summary_path.clone(),
        // `workspace setup` has no PR to record — there is none yet.
        pr: None,
        baseline: None,
        origin: Some(RecordOrigin::Setup),
        events: Some(if fires_setup {
            vec![IssueEvent::Setup]
        } else {
            vec![]
        }),
    })?;
    if !args.no_gitignore {
        ignore_devkit_files();
    }

    backfill_includes(primary_s, &worktree, &cfg.defaults.worktree_include, &steps);

    // Per-app bootstrap: write the app's configured prep files, then run its
    // setup commands in its directory. Everything project-specific — filenames,
    // file contents, installs, doppler wiring — lives in config, not here.
    if args.apps.is_empty() {
        prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)?;
    } else {
        steps.during_result("Preparing apps...", || {
            prep_apps(&worktree, &branch, &args.apps, catalog, &ctx, vars)
        })?;
    }

    let mut hook_ctx = ctx.clone();
    if let Some(obj) = hook_ctx.as_object_mut() {
        obj.insert("branch".into(), serde_json::Value::String(branch.clone()));
        obj.insert("worktree".into(), serde_json::Value::String(holder.clone()));
    }

    // Ports are not reserved here. A worktree's servers get their ports
    // dynamically from `devrun up`, which allocates against the live registry
    // at start time — so the numbers always reflect what is actually free
    // and no unused reservation can be reclaimed by another session in the
    // meantime.
    let out = Prepared {
        issue: issue_ref.map(|r| r.id),
        worktree: holder,
        branch,
        summary: summary_path,
        summary_text: None,
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
        &[],
        &steps,
    );
    if fires_setup {
        crate::issue::event::fire_inline(
            Path::new(&start),
            args.config.as_deref().map(Path::new),
            IssueEvent::Setup,
            &issue,
        );
    }
    Ok(out)
}

/// Add devkit's ignore patterns to the global excludes file. Reported on
/// stderr, since stdout carries the setup summary.
fn ignore_devkit_files() {
    match devkit_common::gitignore::ensure_ignored() {
        Ok((path, added)) => {
            for pattern in added {
                eprintln!("added {pattern} to {}", path.display());
            }
        }
        Err(e) => eprintln!("warning: could not update global gitignore: {e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use devkit_common::tracker::fake;
    use devkit_config::Templates;
    use serde_json::json;

    use super::*;

    /// An empty root joins to a bare relative path, which `git -C <primary>`
    /// resolves inside the primary checkout — the one placement that corrupts
    /// every other session's view of it.
    #[test]
    fn an_underivable_worktree_root_is_refused_by_name() {
        let cfg = devkit_config::Config::default();
        assert!(cfg.defaults.worktree_root.is_empty());
        let err = worktree_root(&cfg).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("defaults.worktree_root"), "{msg}");
    }

    #[test]
    fn a_configured_worktree_root_expands() {
        let mut cfg = devkit_config::Config::default();
        cfg.defaults.worktree_root = "/w/trees".into();
        assert_eq!(
            worktree_root(&cfg).unwrap(),
            std::path::PathBuf::from("/w/trees")
        );
    }

    #[test]
    fn setup_takes_its_slug_from_the_tracker() {
        let t = fake::FakeTracker::new().with_title("ENG-7", "Fix the export crash");
        let r = resolve_slug(
            &t,
            Some(&IssueRef {
                id: "ENG-7".into(),
                slug: None,
            }),
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

    fn cfg_with(prefix: &str, templates: Templates) -> devkit_config::Config {
        let mut cfg = devkit_config::Config::default();
        cfg.defaults.branch_prefix = prefix.into();
        cfg.templates = templates;
        cfg
    }

    /// One title, two limits: the branch keeps a slug worth reading while the
    /// worktree directory gets a shorter one, and rendering the real
    /// `worktree_dir` template with the derived `short_slug` confirms the
    /// directory name that reaches the filesystem actually fits the limit.
    #[test]
    fn short_slug_is_shorter_than_the_branch_slug() {
        let cfg = cfg_with("lev/", Templates {
            worktree_dir: Some("{{ short_slug }}".into()),
            ..Templates::default()
        });
        let budget = branch_budget(&cfg, &novars(), "142", &[]).unwrap();
        let slug = crate::issue::slug::cap("group-sync-includes-file-lists-in-the-output", budget);
        assert_eq!(slug, "group-sync-includes-file-lists-in-the");
        let dir_slug = short_slug(&cfg, &novars(), "142", &[], &slug).unwrap();
        assert_eq!(dir_slug, "group-sync-includes-file");

        let ctx = json!({
            "prefix": "lev/",
            "issue": "142",
            "slug": slug,
            "short_slug": dir_slug,
            "apps": Vec::<String>::new(),
        });
        let name =
            devkit_common::template::render(cfg.templates.worktree_dir(), &ctx, &novars()).unwrap();
        assert_eq!(name, dir_slug);
        assert!(name.chars().count() <= cfg.templates.worktree_dir_max());
    }

    /// The shipped `worktree_dir` renders `{{ slug }}`, so the directory limit
    /// finds nothing to constrain and the directory keeps matching the branch.
    #[test]
    fn the_default_worktree_dir_ignores_its_limit() {
        let cfg = cfg_with("lev/", Templates::default());
        let slug = "group-sync-includes-file-lists-in-the";
        assert_eq!(short_slug(&cfg, &novars(), "142", &[], slug).unwrap(), slug);
    }

    /// A directory limit its own template's fixed text already fills is an
    /// error rather than a silently longer directory. The limit exists because
    /// a path that overruns cannot be removed by every tool that meets it.
    #[test]
    fn a_worktree_dir_limit_the_template_cannot_meet_is_an_error() {
        let cfg = cfg_with("lev/", Templates {
            worktree_dir: Some("worktree-for-issue-{{ issue }}-{{ short_slug }}".into()),
            worktree_dir_max: Some(16),
            ..Templates::default()
        });
        let err = short_slug(&cfg, &novars(), "142", &[], "fix-the-export").unwrap_err();
        // `Debug` prints the whole cause chain; the limit is named in
        // `budget`'s own bail, below the context this call site adds.
        let err = format!("{err:?}");
        assert!(err.contains("worktree_dir_max = 16"), "{err}");
    }

    /// A `branch` template that does not render `{{ slug }}` must still bound
    /// it: `slug` also names the worktree directory under the shipped
    /// `worktree_dir` template, and a `short_slug` that renders nothing does
    /// not save it. An unbounded budget here would put an unbounded name on a
    /// filesystem path.
    #[test]
    fn a_branch_template_without_slug_still_bounds_the_budget() {
        let cfg = cfg_with("lev/", Templates {
            branch: Some("{{ prefix }}{{ issue }}".into()),
            ..Templates::default()
        });
        assert_eq!(
            branch_budget(&cfg, &novars(), "142", &[]).unwrap(),
            cfg.templates.branch_max()
        );
    }

    /// A `branch_prefix` long enough to eat the budget yields the shortest slug
    /// still worth reading, not an error: a git ref has no hard length limit.
    #[test]
    fn a_branch_limit_the_prefix_fills_falls_back_to_the_floor() {
        // 39 characters of prefix against a limit of 46 leaves 7, below the
        // floor.
        let cfg = cfg_with(
            "an-extremely-long-branch-prefix-indeed/",
            Templates::default(),
        );
        assert_eq!(
            branch_budget(&cfg, &novars(), "142", &[]).unwrap(),
            MIN_SLUG
        );
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        devkit_git::Git::fixture(dir)
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
    }

    /// Every path under `dir`, so a test can tell that a run created nothing.
    fn tree(dir: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(tree(&path));
            }
            out.push(path);
        }
        out.sort();
        out
    }

    /// A primary checkout at `app/` on `main`, pushed to a bare `origin.git`,
    /// with worktrees placed under `wts/` beside them.
    struct Project {
        root: tempfile::TempDir,
        cfg: devkit_config::Config,
    }

    impl Project {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let origin = root.path().join("origin.git");
            let app = root.path().join("app");
            std::fs::create_dir_all(&origin).unwrap();
            std::fs::create_dir_all(&app).unwrap();
            git(&origin, &["init", "-q", "--bare", "-b", "main"]);
            git(&app, &["init", "-q", "-b", "main"]);
            std::fs::write(app.join("f.txt"), "x\n").unwrap();
            git(&app, &["add", "-A"]);
            git(&app, &["commit", "-qm", "init"]);
            git(&app, &["remote", "add", "origin", origin.to_str().unwrap()]);
            git(&app, &["push", "-q", "origin", "main"]);
            let mut cfg = devkit_config::Config::default();
            cfg.defaults.worktree_root = root.path().join("wts").display().to_string();
            cfg.defaults.branch_prefix = "x/".into();
            cfg.defaults.baseline_ref = "origin/main".into();
            Self { root, cfg }
        }

        fn app(&self) -> std::path::PathBuf {
            self.root.path().join("app")
        }

        fn args(&self, issue: &str, summary: bool, dry_run: bool) -> SetupArgs {
            SetupArgs {
                issue: Some(issue.into()),
                slug: None,
                apps: vec![],
                dry_run,
                summary,
                no_summary: false,
                no_gitignore: true,
                here: false,
                dir: Some(self.app().display().to_string()),
                config: None,
            }
        }

        fn setup(&self, args: &SetupArgs, tracker: fake::FakeTracker) -> Prepared {
            let resolved = Resolved {
                tracker: Box::new(tracker),
                declared: true,
                reason: "test".into(),
            };
            setup(args, &self.cfg, &HashMap::new(), &resolved).unwrap()
        }
    }

    fn eng_7() -> IssueDetails {
        IssueDetails {
            id: "ENG-7".into(),
            title: "Fix the export crash".into(),
            url: "https://linear.app/acme/issue/ENG-7/fix-the-export-crash".into(),
            description: "Exporting a CSV panics.".into(),
            state: "Todo".into(),
            labels: vec!["export".into()],
            ..IssueDetails::default()
        }
    }

    fn linear() -> fake::FakeTracker {
        fake::FakeTracker::new()
            .with_title("ENG-7", "Fix the export crash")
            .with_details(eng_7())
    }

    /// A launcher dispatching a Linear issue takes the handoff text from a dry
    /// run, so it has to be the file a real `--summary` run writes, and the dry
    /// run must leave no worktree, branch or file behind.
    #[test]
    fn a_dry_run_summary_is_the_text_a_real_setup_writes() {
        let p = Project::new();
        let branches = git(&p.app(), &["branch", "--format=%(refname:short)"]);
        let before = tree(p.root.path());

        let dry = p.setup(&p.args("ENG-7", true, true), linear());

        let json = serde_json::to_value(&dry).unwrap();
        let text = json["summary_text"]
            .as_str()
            .expect("summary_text in the JSON");
        assert!(
            text.starts_with("# ENG-7: Fix the export crash\n"),
            "{text}"
        );
        assert_eq!(tree(p.root.path()), before, "the dry run created a file");
        assert_eq!(
            git(&p.app(), &["branch", "--format=%(refname:short)"]),
            branches,
            "the dry run created a branch"
        );

        let real = p.setup(&p.args("ENG-7", true, false), linear());

        assert_eq!(real.summary, dry.summary);
        let written = std::fs::read_to_string(real.summary.as_deref().unwrap()).unwrap();
        assert_eq!(text, written);
    }

    /// A tracker that keeps its own summary has it written verbatim, so the
    /// dry run reports that text rather than the rendered template.
    /// The summary text carries the issue body verbatim, so on a terminal its
    /// escape sequences are shown, not obeyed, while its lines stay lines.
    #[test]
    fn a_dry_run_summary_on_a_terminal_shows_escapes_as_text() {
        let p = Project::new();
        let tracker = fake::FakeTracker::new()
            .with_title("ENG-7", "Fix the export crash")
            .with_details(IssueDetails {
                description: "first \u{1b}]8;;https://evil\u{7}line\nsecond line".into(),
                ..eng_7()
            });

        let report = p
            .setup(&p.args("ENG-7", true, true), tracker)
            .terminal_report();

        assert!(!report.contains(['\u{1b}', '\u{7}']), "{report:?}");
        assert!(
            report.contains("first \\u{1b}]8;;https://evil\\u{7}line\nsecond line"),
            "{report:?}"
        );
    }

    #[test]
    fn a_dry_run_summary_carries_the_trackers_own_summary() {
        let p = Project::new();
        let tracker = || linear().with_summary("ENG-7", "## Plan\n\nFix it.\n");

        let dry = p.setup(&p.args("ENG-7", true, true), tracker());
        let real = p.setup(&p.args("ENG-7", true, false), tracker());

        assert_eq!(dry.summary_text.as_deref(), Some("## Plan\n\nFix it.\n"));
        let written = std::fs::read_to_string(real.summary.as_deref().unwrap()).unwrap();
        assert_eq!(dry.summary_text.unwrap(), written);
    }

    /// `--here` reports the same summary text on a dry run as it writes.
    #[test]
    fn a_dry_run_here_summary_is_the_text_setup_here_writes() {
        let p = Project::new();
        git(&p.app(), &["checkout", "-q", "-b", "feature"]);
        let here = |dry_run| SetupArgs {
            here: true,
            ..p.args("ENG-7", true, dry_run)
        };

        let dry = p.setup(&here(true), linear());
        assert!(
            devkit_common::record::read(&p.app()).is_none(),
            "the dry run bound the checkout"
        );
        let real = p.setup(&here(false), linear());

        let written = std::fs::read_to_string(real.summary.as_deref().unwrap()).unwrap();
        assert_eq!(dry.summary_text.expect("summary_text"), written);
    }

    /// Without `--summary` a dry run asks the tracker only for the title its
    /// slug needs, and reports no summary text.
    #[test]
    fn a_dry_run_without_summary_reads_no_more_of_the_tracker() {
        let p = Project::new();
        let tracker = linear();
        let calls = tracker.calls();

        let dry = p.setup(&p.args("ENG-7", false, true), tracker);

        let json = serde_json::to_value(&dry).unwrap();
        assert!(json.get("summary_text").is_none(), "{json}");
        assert!(json.get("summary").is_none(), "{json}");
        assert_eq!(*calls.lock().unwrap(), vec!["title ENG-7".to_string()]);
    }

    fn ctx() -> serde_json::Value {
        json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix", "apps": ["web"], "app": "web"})
    }

    /// The backfill is a step of the run like any other, so a caller's `[i/N]`
    /// numbering has to account for it.
    #[test]
    fn the_backfill_consumes_one_step() {
        let base = tempfile::tempdir().unwrap();
        let src = base.path().join("src");
        let dst = base.path().join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join(".tool-versions"), "node 20").unwrap();

        let steps = Steps::persistent_with_total(1);
        backfill_includes(
            src.to_str().unwrap(),
            &dst,
            &[".tool-versions".to_string()],
            &steps,
        );

        assert_eq!(steps.started(), 1);
        assert!(dst.join(".tool-versions").exists(), "the file was copied");
    }

    /// An empty include list is not work, so it must not draw a step or the run
    /// ends one short of its total.
    #[test]
    fn an_empty_include_list_consumes_no_step() {
        let base = tempfile::tempdir().unwrap();
        let steps = Steps::persistent_with_total(0);

        backfill_includes(base.path().to_str().unwrap(), base.path(), &[], &steps);

        assert_eq!(steps.started(), 0);
    }

    /// Without a discovery sub-step, a pattern's display number is its index
    /// in the configured list, one-based.
    #[test]
    fn number_without_discovery_starts_at_one() {
        let steps = Steps::persistent();
        steps.during_step("test", |step| {
            let render = IncludeRender {
                step,
                discovery: false,
                subs: 3,
                entry: std::sync::atomic::AtomicUsize::new(0),
                drawn: std::sync::atomic::AtomicUsize::new(0),
            };
            assert_eq!(render.subs, 3, "no discovery sub-step to add");
            assert_eq!(render.number(0), 1);
            assert_eq!(render.number(2), 3);
        });
    }

    /// A discovery sub-step takes slot 1, so every pattern's number and the
    /// sub-step total both shift by one.
    #[test]
    fn number_with_discovery_reserves_the_first_slot() {
        let steps = Steps::persistent();
        steps.during_step("test", |step| {
            let render = IncludeRender {
                step,
                discovery: true,
                subs: 4,
                entry: std::sync::atomic::AtomicUsize::new(0),
                drawn: std::sync::atomic::AtomicUsize::new(0),
            };
            assert_eq!(render.subs, 4, "discovery adds one sub-step");
            assert_eq!(render.number(0), 2, "the first pattern follows discovery");
            assert_eq!(render.number(2), 4);
        });
    }

    /// `due` throttles redraws to every `INCLUDE_REDRAW_EVERY` files, and a
    /// `true` result moves the recorded cursor to that count.
    #[test]
    fn due_throttles_and_records_the_redraw_cursor() {
        let steps = Steps::persistent();
        steps.during_step("test", |step| {
            let render = IncludeRender {
                step,
                discovery: false,
                subs: 1,
                entry: std::sync::atomic::AtomicUsize::new(0),
                drawn: std::sync::atomic::AtomicUsize::new(0),
            };
            assert!(!render.due(10), "under the threshold from a zero cursor");
            assert!(render.due(INCLUDE_REDRAW_EVERY), "reaches the threshold");
            assert!(
                !render.due(INCLUDE_REDRAW_EVERY + 10),
                "not far enough past the cursor `due` just recorded"
            );
            assert!(
                render.due(INCLUDE_REDRAW_EVERY * 2),
                "far enough past the recorded cursor"
            );
        });
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
        let out = devkit_common::template::render(t.branch(), &ctx, &t.defaults()).unwrap();
        assert_eq!(out, "lev/fix");
    }

    #[test]
    fn default_worktree_dir_renders_slug() {
        let t = Templates::default();
        let ctx = json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix"});
        let out = devkit_common::template::render(t.worktree_dir(), &ctx, &t.defaults()).unwrap();
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
