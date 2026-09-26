//! `devkit rules`: read the rule index the hooks inject from, and change it by
//! hand.
//!
//! The filters mirror `repo-rules-agent query` so the two agree on what a given
//! query means, with `--min-severity` added: the hook asks for a floor, and a
//! flag the hook uses is a flag a person can reproduce by hand.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use devkit_common::git::Checkout;
use devkit_rules::{
    edit, index,
    model::RuleIndex,
    query,
    vocab::{self, Severity, Task},
};

#[derive(Args)]
pub struct RulesCli {
    #[command(subcommand)]
    pub command: RulesCommand,
}

#[derive(Subcommand)]
pub enum RulesCommand {
    /// Print the rules matching a filter.
    Query(QueryArgs),
    /// Summarize an index.
    Stats(StatsArgs),
    /// Print the session-start context block.
    Context(ContextArgs),
    /// Add a rule to the index and print its id.
    Add(AddArgs),
    /// Change a rule and pin it against a rebuild of the index.
    Edit(EditArgs),
    /// Remove a rule, leaving a pinned tombstone for an extracted one.
    ///
    /// Every reader skips the tombstone, and a rebuild that keeps pinned rules
    /// does not extract the rule again. A rule made with `add` is deleted
    /// outright.
    #[command(visible_aliases = ["rm", "delete"])]
    Remove(RemoveArgs),
}

#[derive(Args)]
pub struct QueryArgs {
    /// The index file. Defaults to the one built for this checkout.
    pub index_path: Option<PathBuf>,
    #[arg(long, short = 't')]
    pub task: Option<String>,
    #[arg(long = "lang", short = 'l')]
    pub language: Option<String>,
    #[arg(long, short = 's')]
    pub scope: Option<String>,
    /// Exact severity: must, should or can.
    #[arg(long)]
    pub severity: Option<String>,
    /// Least severe value still printed.
    #[arg(long)]
    pub min_severity: Option<String>,
    /// Keep repo-wide rules plus those governing this path. Repeatable.
    #[arg(long = "path", short = 'p')]
    pub paths: Vec<String>,
    /// Rank rules about this topic first. Repeatable.
    #[arg(long = "topic")]
    pub topics: Vec<String>,
    #[arg(long, short = 'n')]
    pub limit: Option<usize>,
    #[arg(long, short = 'f', value_enum, default_value_t = Format::Table)]
    pub format: Format,
}

#[derive(Args)]
pub struct StatsArgs {
    pub index_path: Option<PathBuf>,
}

#[derive(Args)]
pub struct ContextArgs {
    /// Emit inside the JSON envelope Codex and Cursor read, rather than plain.
    #[arg(long)]
    pub additional_context: bool,
}

#[derive(Args)]
pub struct AddArgs {
    #[arg(long)]
    pub title: String,
    #[command(flatten)]
    pub fields: FieldArgs,
}

#[derive(Args)]
pub struct EditArgs {
    /// The rule's id, from the ID column of `devkit rules query`.
    pub id: String,
    #[arg(long)]
    pub title: Option<String>,
    #[command(flatten)]
    pub fields: FieldArgs,
}

#[derive(Args)]
pub struct RemoveArgs {
    /// The rule's id, from the ID column of `devkit rules query`.
    pub id: String,
}

/// The rule fields `add` and `edit` share. On `edit`, a list flag replaces the
/// rule's whole list.
#[derive(Args)]
pub struct FieldArgs {
    #[arg(long)]
    pub description: Option<String>,
    #[arg(long)]
    pub category: Option<String>,
    /// must, should or can.
    #[arg(long, value_parser = parse_severity)]
    pub severity: Option<Severity>,
    /// code-review, code-generation or code-questions. Repeatable; none applies
    /// to every task.
    #[arg(long = "task", short = 't', value_parser = parse_task)]
    pub tasks: Vec<Task>,
    /// Repeatable; none applies to every language.
    #[arg(long = "lang", short = 'l')]
    pub languages: Vec<String>,
    /// Repeatable.
    #[arg(long = "topic")]
    pub topics: Vec<String>,
    /// Directory the rule governs, relative to the current directory. The
    /// repository root governs everything.
    #[arg(long)]
    pub directory: Option<PathBuf>,
}

fn parse_severity(raw: &str) -> Result<Severity, String> {
    raw.parse()
        .map_err(|_| "accepted: must, should, can".to_string())
}

fn parse_task(raw: &str) -> Result<Task, String> {
    raw.parse()
        .map_err(|_| "accepted: code-review, code-generation, code-questions".to_string())
}

impl FieldArgs {
    fn into_fields(self, title: Option<String>, here: &Here) -> Result<edit::Fields> {
        let directory = match self.directory {
            None => None,
            Some(dir) => {
                let rel =
                    devkit_rules::context::relativize_target(here.root(), &here.cwd.join(&dir))
                        .with_context(|| format!("{} is outside the repository", dir.display()))?;
                Some(if rel == "." { String::new() } else { rel })
            }
        };
        Ok(edit::Fields {
            title,
            description: self.description,
            category: self.category,
            severity: self.severity,
            tasks: non_empty(self.tasks),
            languages: non_empty(self.languages),
            topics: non_empty(self.topics),
            directory,
        })
    }
}

/// `None` for an empty list: a list flag nobody passed leaves the rule's list
/// alone.
fn non_empty<T>(values: Vec<T>) -> Option<Vec<T>> {
    (!values.is_empty()).then_some(values)
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
    Prompt,
}

/// The index path for this checkout: an explicit path (`--index`, or the
/// positional argument `query` and `stats` take) wins outright; otherwise the
/// project's own `[rules] index`; otherwise the path `repo-rules-agent` would
/// have written for the checkout's main worktree. Every `devkit rules`
/// subcommand resolves it this way, so a project that sets `[rules] index`
/// reads and edits the same file everywhere.
fn resolve_index_path(
    explicit: Option<PathBuf>,
    index: Option<&str>,
    checkout: &Checkout,
) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Some(index) = index {
        return PathBuf::from(index);
    }
    index::default_index_path(repo_of(checkout))
}

/// The repository an index describes: the main worktree, so every worktree
/// shares one index.
fn repo_of(checkout: &Checkout) -> &Path {
    checkout
        .main_worktree()
        .or_else(|| checkout.root())
        .unwrap_or_else(|| checkout.dir())
}

/// Where a `devkit rules` run stands: its directory, checkout and index path.
struct Here {
    cwd: PathBuf,
    checkout: Checkout,
    index: PathBuf,
}

impl Here {
    fn resolve(explicit: Option<PathBuf>) -> Result<Here> {
        let cwd = std::env::current_dir().context("getting current dir")?;
        let checkout = Checkout::at(&cwd);
        let configured = devkit_common::config::resolve_in(&checkout, None, &cwd)
            .ok()
            .and_then(|(project, _)| project.rules.index);
        let index = resolve_index_path(explicit, configured.as_deref(), &checkout);
        Ok(Here {
            cwd,
            checkout,
            index,
        })
    }

    fn root(&self) -> &Path {
        self.checkout.root().unwrap_or(&self.cwd)
    }
}

/// The index for this checkout, or the one named. Errors name the path tried,
/// because a `devkit rules` run is a person asking a question and silence would
/// read as "no rules" rather than "no index".
fn load_or_default(explicit: Option<PathBuf>) -> Result<(PathBuf, RuleIndex)> {
    let path = Here::resolve(explicit)?.index;
    let loaded =
        index::load(&path).with_context(|| format!("no rules index at {}", path.display()))?;
    Ok((path, loaded))
}

pub fn run(cli: RulesCli) -> Result<()> {
    match cli.command {
        RulesCommand::Query(args) => query_cmd(args),
        RulesCommand::Stats(args) => stats_cmd(args),
        RulesCommand::Context(args) => context_cmd(args),
        RulesCommand::Add(args) => {
            let here = Here::resolve(None)?;
            let fields = args.fields.into_fields(Some(args.title), &here)?;
            let repo = repo_of(&here.checkout).display().to_string();
            let id = edit::update(&here.index, |doc| doc.add(&repo, fields))?;
            println!("{id}");
            Ok(())
        }
        RulesCommand::Edit(args) => {
            let here = Here::resolve(None)?;
            let fields = args.fields.into_fields(args.title, &here)?;
            edit::update(&here.index, |doc| doc.edit(&args.id, fields))?;
            println!("edited {}", args.id);
            Ok(())
        }
        RulesCommand::Remove(args) => {
            let here = Here::resolve(None)?;
            edit::update(&here.index, |doc| doc.remove(&args.id))?;
            println!("removed {}", args.id);
            Ok(())
        }
    }
}

/// A vocabulary value off the command line, with the accepted values named on
/// failure. A person mistyping `--severity` gets the list, not a parse error.
fn parse_vocab<T: std::str::FromStr>(value: Option<&str>, accepted: &str) -> Result<Option<T>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    raw.parse()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("unknown value {raw:?}; accepted: {accepted}"))
}

fn query_cmd(args: QueryArgs) -> Result<()> {
    let (_, index) = load_or_default(args.index_path)?;
    let cwd = std::env::current_dir().context("getting current dir")?;
    let checkout = Checkout::at(&cwd);
    let root = checkout.root().unwrap_or(&cwd);
    let filter = query::Filter {
        task: parse_vocab(
            args.task.as_deref(),
            "code-review, code-generation, code-questions",
        )?,
        language: args.language.map(|l| vocab::canonical_language(&l)),
        scope: parse_vocab(args.scope.as_deref(), "repo, directory, file-pattern")?,
        severity: parse_vocab(args.severity.as_deref(), "must, should, can")?,
        min_severity: parse_vocab(args.min_severity.as_deref(), "must, should, can")?,
        paths: args
            .paths
            .iter()
            .filter_map(|path| devkit_rules::context::relativize_target(root, &cwd.join(path)))
            .collect(),
    };
    let matched = if !args.paths.is_empty() && filter.paths.is_empty() {
        Vec::new()
    } else {
        query::matching(&index, &filter)
    };
    let mut rules = query::rank(&index, matched, &args.topics);
    if let Some(limit) = args.limit.filter(|n| *n > 0) {
        rules.truncate(limit);
    }
    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&rules)?),
        Format::Prompt => print!("{}", devkit_rules::render::block(&rules, &[], usize::MAX)),
        Format::Table => {
            let mut table =
                devkit_common::ui::table(&["ID", "SEVERITY", "DIRECTORY", "TITLE", "SOURCE"]);
            for rule in &rules {
                let directory = if rule.directory.is_empty() {
                    "."
                } else {
                    &rule.directory
                };
                table.add_row([
                    &rule.id,
                    &rule.severity_raw,
                    directory,
                    &rule.title,
                    &rule.source_file,
                ]);
            }
            println!("{table}");
        }
    }
    Ok(())
}

fn stats_cmd(args: StatsArgs) -> Result<()> {
    use std::collections::BTreeMap;

    let (path, index) = load_or_default(args.index_path)?;
    println!("index  {}", path.display());
    println!("repo   {}", index.repo);
    println!(
        "\n{} rules across {} files\n",
        index.rules.len(),
        index.files.len()
    );

    let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for rule in &index.rules {
        *per_file.entry(rule.source_file.as_str()).or_default() += 1;
    }
    let mut table = devkit_common::ui::table(&["FILE", "RULES"]);
    for file in &index.files {
        let count = per_file.get(file.path.as_str()).copied().unwrap_or(0);
        table.add_row([file.path.clone(), count.to_string()]);
    }
    println!("{table}");

    let tally = |counts: BTreeMap<String, usize>| -> String {
        let mut rows: Vec<_> = counts.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        rows.iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let count_by = |f: &dyn Fn(&devkit_rules::model::Rule) -> Vec<String>| {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for rule in &index.rules {
            for key in f(rule) {
                *counts.entry(key).or_default() += 1;
            }
        }
        counts
    };
    println!(
        "\nby severity:  {}",
        tally(count_by(&|r| vec![r.severity_raw.clone()]))
    );
    println!("by task:      {}", tally(count_by(&|r| r.tasks.clone())));
    println!(
        "by language:  {}",
        tally(count_by(&|r| r.languages_canonical()))
    );
    println!(
        "by directory: {}",
        tally(count_by(&|r| {
            vec![if r.directory.is_empty() {
                "(repo root)".to_string()
            } else {
                r.directory.clone()
            }]
        }))
    );
    let topics = tally(count_by(&|r| r.topics_canonical()));
    if !topics.is_empty() {
        println!("by topic:     {topics}");
    }

    let failed: Vec<&devkit_rules::model::RuleFile> = index
        .files
        .iter()
        .filter(|f| !f.errors.is_empty())
        .collect();
    if !failed.is_empty() {
        println!("\n{} files had failed extractions:", failed.len());
        for file in failed {
            for error in &file.errors {
                println!("  {}: {error}", file.path);
            }
        }
    }
    Ok(())
}

/// The session-start block: what governs this repository as a whole, plus how
/// to reach the rest.
///
/// Silence on every failure, unlike `query` and `stats`. This runs from a
/// session hook in any repository, so "no index" is the common case and an
/// error would be noise in every session that has none.
fn context_cmd(args: ContextArgs) -> Result<()> {
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    let checkout = Checkout::at(&cwd);
    let Ok((project, provenance)) = devkit_common::config::resolve_in(&checkout, None, &cwd) else {
        return Ok(());
    };
    let _ = &provenance;
    if !project.rules.enabled {
        return Ok(());
    }
    let path = resolve_index_path(None, project.rules.index.as_deref(), &checkout);
    let Some(loaded) = index::load(&path) else {
        return Ok(());
    };
    // No task and no language: session start is the broad trigger, and a
    // rule tagged only `code-review` still governs the repository, unlike on
    // the write path.
    let filter = query::Filter {
        scope: Some(vocab::Scope::Repo),
        severity: Some(vocab::Severity::Must),
        ..query::Filter::default()
    };
    let mut matched = query::rank(&loaded, query::matching(&loaded, &filter), &[]);
    matched.truncate(project.rules.per_event_limit);

    let footer = "\nThe rest of this repository's rules are reachable with \
                  `devkit rules query --path <path>`.\n";
    let Some(budget) = project.rules.max_event_bytes.checked_sub(footer.len()) else {
        return Ok(());
    };
    let rendered = devkit_rules::render::fit(matched, Vec::new(), budget);
    let mut text = rendered.text;
    text.push_str(footer);
    if args.additional_context {
        println!("{}", crate::brief::envelope(&text));
    } else {
        print!("{text}");
    }
    // A top-level session's holder is the bare session id. Without this the
    // first allowed write re-injects everything the session-start block just
    // showed the agent.
    if let Some(session) = crate::brief::session_id() {
        let ids: Vec<String> = rendered.rules.iter().map(|r| r.id.clone()).collect();
        crate::hook::rules::stamp_ids(&session, &ids);
    }
    Ok(())
}
