//! `devkit rules`: read the rule index the hooks inject from.
//!
//! The filters mirror `repo-rules-agent query` so the two agree on what a given
//! query means, with `--min-severity` added: the hook asks for a floor, and a
//! flag the hook uses is a flag a person can reproduce by hand.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use devkit_common::git::Checkout;
use devkit_rules::{index, model::RuleIndex, query, vocab};

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

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
    Prompt,
}

/// The index for this checkout, or the one named. Errors name the path tried,
/// because a `devkit rules` run is a person asking a question and silence would
/// read as "no rules" rather than "no index".
fn load_or_default(explicit: Option<PathBuf>) -> Result<(PathBuf, RuleIndex)> {
    let path = match explicit {
        Some(p) => p,
        None => {
            let cwd = std::env::current_dir().context("getting current dir")?;
            let checkout = Checkout::at(&cwd);
            let repo = checkout
                .main_worktree()
                .map(|p| p.to_path_buf())
                .unwrap_or(cwd);
            index::default_index_path(&repo)
        }
    };
    let loaded =
        index::load(&path).with_context(|| format!("no rules index at {}", path.display()))?;
    Ok((path, loaded))
}

pub fn run(cli: RulesCli) -> Result<()> {
    match cli.command {
        RulesCommand::Query(args) => query_cmd(args),
        RulesCommand::Stats(args) => stats_cmd(args),
        RulesCommand::Context(args) => context_cmd(args),
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
    let filter = query::Filter {
        task: parse_vocab(
            args.task.as_deref(),
            "code-review, code-generation, code-questions",
        )?,
        language: args.language.map(|l| vocab::canonical_language(&l)),
        scope: parse_vocab(args.scope.as_deref(), "repo, directory, file-pattern")?,
        severity: parse_vocab(args.severity.as_deref(), "must, should, can")?,
        min_severity: parse_vocab(args.min_severity.as_deref(), "must, should, can")?,
        paths: args.paths,
    };
    let mut rules = query::rank(&index, query::matching(&index, &filter), &args.topics);
    if let Some(limit) = args.limit.filter(|n| *n > 0) {
        rules.truncate(limit);
    }
    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&rules)?),
        Format::Prompt => print!("{}", devkit_rules::render::block(&rules, &[], usize::MAX)),
        Format::Table => {
            let mut table = devkit_common::ui::table(&["SEVERITY", "DIRECTORY", "TITLE", "SOURCE"]);
            for rule in &rules {
                let directory = if rule.directory.is_empty() {
                    "."
                } else {
                    &rule.directory
                };
                table.add_row([
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
    let path = match &project.rules.index {
        Some(p) => PathBuf::from(p),
        None => {
            let Some(repo) = checkout.main_worktree().or_else(|| checkout.root()) else {
                return Ok(());
            };
            index::default_index_path(repo)
        }
    };
    let Some(loaded) = index::load(&path) else {
        return Ok(());
    };
    let filter = query::Filter {
        task: Some(vocab::Task::CodeGeneration),
        scope: Some(vocab::Scope::Repo),
        severity: Some(vocab::Severity::Must),
        ..query::Filter::default()
    };
    let mut matched = query::rank(&loaded, query::matching(&loaded, &filter), &[]);
    matched.truncate(project.rules.per_event_limit);
    let mut text = devkit_rules::render::block(&matched, &[], project.rules.max_event_bytes);
    if text.is_empty() {
        return Ok(());
    }
    text.push_str(
        "\nThe rest of this repository's rules are reachable with \
         `devkit rules query --path <path>`.\n",
    );
    if args.additional_context {
        println!("{}", crate::brief::envelope(&text));
    } else {
        print!("{text}");
    }
    // A top-level session's holder is the bare session id. Without this the
    // first allowed write re-injects everything the session-start block just
    // showed the agent.
    if let Some(session) = crate::brief::session_id() {
        let ids: Vec<String> = matched.iter().map(|r| r.id.clone()).collect();
        crate::hook::rules::stamp_ids(&session, &ids);
    }
    Ok(())
}
