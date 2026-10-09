//! `devkit rules`: read the rule index the hooks inject from, and change it by
//! hand.
//!
//! The filters mirror `repo-rules-agent query` so the two agree on what a given
//! query means, with `--min-severity` added: the hook asks for a floor, and a
//! flag the hook uses is a flag a person can reproduce by hand.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use devkit_common::{secrets, tls::Trust, vcs::Checkout};
use devkit_config::{RulesConfig, RulesPostgresConfig, RulesSupabaseConfig};
use devkit_rules::{
    edit,
    model::RuleIndex,
    postgres::Database,
    query, repo_config,
    source::{Refresh, RuleSource, Source, repo_of},
    vocab::{self, Scope, Severity, Task},
};
use devkit_supabase::{
    Api, Auth,
    auth::{Client, Credentials, SessionFile, Sessions},
};
use strum::VariantNames;

use crate::secret::{
    Resolved, Secret, SecretLookup, doppler_scope, global_ca_file, global_setting,
};

/// The `postgres` source's connection URL.
pub(crate) const DATABASE_VAR: &str = "DEVKIT_RULES_DATABASE_URL";

/// The `supabase` source's project URL, over `[rules.supabase] url`.
pub(crate) const SUPABASE_URL_VAR: &str = "DEVKIT_RULES_SUPABASE_URL";

/// The email and password the `supabase` source signs in with when it has
/// no session.
pub(crate) const SUPABASE_EMAIL_VAR: &str = "DEVKIT_RULES_SUPABASE_EMAIL";
pub(crate) const SUPABASE_PASSWORD_VAR: &str = "DEVKIT_RULES_SUPABASE_PASSWORD";

/// What names the rules API in errors.
const API_LABEL: &str = "rules API";

/// Who reads the rules, which sets how long the database may take and
/// whether Doppler is asked for its URL.
#[derive(Clone, Copy)]
pub(crate) enum Reader {
    /// A person at a command: Doppler is asked, and the database gets
    /// [`CLI_DATABASE_WAIT`], long enough to wait out another edit's lock.
    Cli,
    /// The session-start block a hook prints, which refreshes the cache: a
    /// cached Doppler URL stands in for Doppler until it ages out, an aged
    /// one stands in when Doppler gives none, and the database gets
    /// [`HOOK_DATABASE_WAIT`].
    Session,
    /// A hook reading the cache for a write: the cached Doppler URL alone
    /// names the source, so the hook never waits on Doppler to find its
    /// cache, and the database gets [`HOOK_DATABASE_WAIT`].
    Write,
}

/// How long a command waits on the rules database, connecting included.
const CLI_DATABASE_WAIT: Duration = Duration::from_secs(15);

/// How long a hook waits on the rules database, connecting included, before
/// it injects nothing.
const HOOK_DATABASE_WAIT: Duration = Duration::from_secs(1);

impl Reader {
    fn wait(self) -> Duration {
        match self {
            Reader::Cli => CLI_DATABASE_WAIT,
            Reader::Session | Reader::Write => HOOK_DATABASE_WAIT,
        }
    }

    /// When every remote request this reader makes must be done by.
    fn deadline(self) -> Option<Instant> {
        match self {
            Reader::Cli => None,
            Reader::Session | Reader::Write => Some(Instant::now() + HOOK_DATABASE_WAIT),
        }
    }

    fn lookup(self) -> SecretLookup {
        match self {
            Reader::Cli => SecretLookup::Doppler,
            Reader::Session => SecretLookup::CachedFirst,
            Reader::Write => SecretLookup::CachedOnly,
        }
    }
}

/// The rule source `settings` names for `checkout`, for `reader`. A hook's
/// remote finishes by one deadline, so a refresh's revision check, pull
/// pages, sign-in and retries share a single wait between them.
pub(crate) fn source(settings: &RulesConfig, checkout: &Checkout, reader: Reader) -> Source {
    let deadline = reader.deadline();
    Source::for_checkout(
        settings,
        checkout,
        |config| {
            let db = open_database(config, reader.wait(), reader.lookup()).0;
            if let Some(at) = deadline {
                db.finish_by(at);
            }
            db
        },
        |config| {
            let api = open_api(config, reader.wait(), reader.lookup()).0;
            if let Some(at) = deadline {
                api.finish_by(at);
            }
            api
        },
        &devkit_common::paths::state_dir(),
    )
}

/// The rules API's project URL: [`SUPABASE_URL_VAR`], else the global
/// config's `[rules.supabase] url`, and where it came from. A project's
/// `devkit.toml` cannot set it, so a checkout cannot send a session
/// elsewhere.
pub(crate) fn supabase_url() -> (Result<String>, secrets::Source) {
    if let Some(url) = std::env::var(SUPABASE_URL_VAR)
        .ok()
        .filter(|url| !url.trim().is_empty())
    {
        return (Ok(url), secrets::Source::Env);
    }
    match global_setting(&["rules", "supabase", "url"]) {
        Some(url) => (Ok(url), secrets::Source::File),
        None => (
            Err(anyhow::anyhow!(
                "{SUPABASE_URL_VAR} is not set, nor [rules.supabase] url in the global config"
            )),
            secrets::Source::Unset,
        ),
    }
}

/// The sign-in email and password and where Doppler's answers for each are
/// kept.
fn supabase_credentials() -> [Secret; 2] {
    let state = devkit_common::paths::state_dir();
    [
        Secret {
            var: SUPABASE_EMAIL_VAR,
            cache_dir: state.join("rules-supabase-email"),
        },
        Secret {
            var: SUPABASE_PASSWORD_VAR,
            cache_dir: state.join("rules-supabase-password"),
        },
    ]
}

/// The sign-in email and password, resolved as the database URL is, and
/// where the email resolved from. Doppler's fresh answers are kept for
/// hooks.
pub(crate) fn resolve_credentials(
    config: &RulesSupabaseConfig,
    lookup: SecretLookup,
) -> (Option<Credentials>, secrets::Source) {
    let scope = doppler_scope(
        config.doppler_project.as_deref(),
        config.doppler_config.as_deref(),
    );
    let [email, password] = supabase_credentials();
    let resolve = |secret: &Secret| {
        let resolved = resolve_or_kept(secret, scope.as_ref(), lookup);
        if let (secrets::Source::Doppler, Some(scope), Some(value), false) = (
            &resolved.source,
            &scope,
            &resolved.value,
            resolved.from_cache,
        ) {
            secret.remember(scope, value);
        }
        resolved
    };
    let (email, password) = (resolve(&email), resolve(&password));
    let credentials = email
        .value
        .zip(password.value)
        .map(|(email, password)| Credentials { email, password });
    (credentials, email.source)
}

/// The rules API `config` and [`SUPABASE_URL_VAR`] name, opened with
/// `wait`, and where its URL resolved from. With a publishable key, requests
/// go as the user signed in through `devkit auth supabase`, or, failing
/// that, through the email and password the secrets resolve to; without
/// one, they carry no credentials, for a proxy that attaches them.
pub(crate) fn open_api(
    config: &RulesSupabaseConfig,
    wait: Duration,
    lookup: SecretLookup,
) -> (Arc<Api>, secrets::Source) {
    let (url, url_source) = supabase_url();
    let url = match url {
        Ok(url) => url,
        Err(e) => return (Arc::new(Api::unusable(e, API_LABEL)), url_source),
    };
    let auth = match &config.publishable_key {
        None => Ok(Auth::None),
        Some(key) => Client::new(&url, key, wait).map(|client| {
            let config = config.clone();
            let credentials = move || resolve_credentials(&config, lookup).0;
            Auth::User(Arc::new(Sessions::new(
                client,
                SessionFile::for_url(&devkit_common::paths::state_dir(), &url),
                Box::new(credentials),
            )))
        }),
    };
    let api = match auth.and_then(|auth| Api::new(&url, "repo_rules_api", auth, wait, API_LABEL)) {
        Ok(api) => api,
        Err(e) => {
            return (
                Arc::new(Api::unusable(
                    format!("{SUPABASE_URL_VAR}: {e:#}"),
                    API_LABEL,
                )),
                url_source,
            );
        }
    };
    if let Some(scope) = doppler_scope(
        config.doppler_project.as_deref(),
        config.doppler_config.as_deref(),
    ) {
        api.on_rejected(move || {
            for secret in supabase_credentials() {
                if let Some(value) = secret.resolve(Some(&scope), SecretLookup::CachedOnly).value {
                    secret.forget(&scope, &value);
                }
            }
        });
    }
    (Arc::new(api), url_source)
}

/// Refreshes `source`'s cache when the remote's revision changed. A failure
/// is one line on stderr, and the cache stays as it was for the read after.
fn refresh_quietly(source: &Source) {
    if let Some(Err(e)) = source.refresh(Refresh::IfChanged) {
        let line = format!("{e:#}").replace(['\r', '\n'], " ");
        eprintln!("devkit: {line}");
    }
}

/// `secret` resolved as `lookup` says, falling back to the copy Doppler last
/// gave, however old, when nothing else gives a value: the cache is keyed by
/// the source, so an offline session still finds the cache it filled.
fn resolve_or_kept(
    secret: &Secret,
    scope: Option<&secrets::DopplerScope>,
    lookup: SecretLookup,
) -> Resolved {
    let resolved = secret.resolve(scope, lookup);
    match (&resolved.value, lookup) {
        (None, SecretLookup::CachedFirst) => secret.resolve(scope, SecretLookup::CachedOnly),
        _ => resolved,
    }
}

/// The rules database `config` and [`DATABASE_VAR`] name, opened with `wait`,
/// and where its URL resolved from. A URL that is missing or does not parse
/// gives a database every operation on fails, naming the variable and never
/// the URL. The CA file comes from the global config alone.
pub(crate) fn open_database(
    config: &RulesPostgresConfig,
    wait: Duration,
    lookup: SecretLookup,
) -> (Arc<Database>, secrets::Source) {
    let scope = doppler_scope(
        config.doppler_project.as_deref(),
        config.doppler_config.as_deref(),
    );
    let cache = Secret {
        var: DATABASE_VAR,
        cache_dir: devkit_common::paths::state_dir().join("rules-database-url"),
    };
    let resolved = resolve_or_kept(&cache, scope.as_ref(), lookup);
    let Some(url) = resolved.value else {
        return (
            Arc::new(Database::unusable(format!("{DATABASE_VAR} is not set"))),
            resolved.source,
        );
    };
    let trust = Trust {
        ca_file: global_ca_file("rules"),
    };
    let db = match Database::new(&url, wait, &trust, "rules database") {
        Ok(db) => db,
        Err(e) => {
            return (
                Arc::new(Database::unusable(format!("{DATABASE_VAR}: {e:#}"))),
                resolved.source,
            );
        }
    };
    if let (secrets::Source::Doppler, Some(scope)) = (&resolved.source, scope) {
        // Only a fresh answer resets the copy's age; reusing it must not.
        if !resolved.from_cache {
            cache.remember(&scope, &url);
        }
        // Only a refused login says the URL went stale; every other failure
        // keeps it, since the write hooks need it to find their cache.
        db.on_connect_failure(move |e| {
            if devkit_postgres::is_rejected_login(e) {
                cache.forget(&scope, &url);
            }
        });
    }
    (Arc::new(db), resolved.source)
}

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
    ///
    /// The rule keeps its id even when its title changes.
    Edit(EditArgs),
    /// Remove a rule, leaving a pinned tombstone for an extracted one.
    ///
    /// Every reader skips the tombstone, and a rebuild that keeps pinned rules
    /// does not extract the rule again. A rule made with `add` is deleted
    /// outright.
    #[command(visible_aliases = ["rm", "delete"])]
    Remove(RemoveArgs),
    /// Refresh the local cache of a remote rules source.
    ///
    /// Reads every rule from the `postgres` or `supabase` source into the
    /// cache the hooks read, replacing it whatever revision it holds, and
    /// prints the revision and how many rules it holds. Session start
    /// refreshes the cache on its own when the remote's revision changed,
    /// but never over a later revision; run this when the remote's revision
    /// went backwards, such as after its database was recreated.
    Pull,
}

#[derive(Args)]
pub struct QueryArgs {
    /// The index file, a SQLite store or a JSON index. Defaults to the one
    /// built for this checkout.
    pub index_path: Option<PathBuf>,
    #[arg(long, short = 't', value_parser = parse_vocab::<Task>)]
    pub task: Option<Task>,
    #[arg(long = "lang", short = 'l')]
    pub language: Option<String>,
    #[arg(long, short = 's', value_parser = parse_vocab::<Scope>)]
    pub scope: Option<Scope>,
    /// Exact severity: must, should or can.
    #[arg(long, value_parser = parse_vocab::<Severity>)]
    pub severity: Option<Severity>,
    /// Least severe value still printed.
    #[arg(long, value_parser = parse_vocab::<Severity>)]
    pub min_severity: Option<Severity>,
    /// Keep repo-wide rules plus those governing this path. Repeatable.
    #[arg(long = "path", short = 'p')]
    pub paths: Vec<String>,
    /// Rank rules about this topic first. Topics come from the repo's
    /// .agents/repo-rules-agent.toml. One the file does not define matches
    /// rule text only, and warns on stderr if the file defines any. Repeatable.
    #[arg(long = "topic")]
    pub topics: Vec<String>,
    /// Print at most this many rules, most useful first. 0 prints all.
    /// Defaults to query.limit in the repo's .agents/repo-rules-agent.toml,
    /// then to 50. A capped result says so on stderr.
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
    /// The harness whose hook runs this. The block then travels inside that
    /// event's JSON answer, which every harness reads context from except
    /// Claude Code at session start, where it prints bare.
    #[arg(long)]
    pub harness: Option<pabal::AnyHarness>,
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
    #[arg(long, value_parser = parse_vocab::<Severity>)]
    pub severity: Option<Severity>,
    /// code-review, code-generation or code-questions. Repeatable; none applies
    /// to every task.
    #[arg(long = "task", short = 't', value_parser = parse_vocab::<Task>)]
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

/// A vocabulary value off the command line, with the accepted values named on
/// failure. A person mistyping `--severity` gets the list, not a parse error.
fn parse_vocab<T: std::str::FromStr + VariantNames>(raw: &str) -> Result<T, String> {
    raw.parse()
        .map_err(|_| format!("accepted: {}", T::VARIANTS.join(", ")))
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

/// Where a `devkit rules` run stands: its directory, checkout and rule source.
struct Here {
    cwd: PathBuf,
    checkout: Checkout,
    source: Source,
}

impl Here {
    /// An explicit index file (the positional argument `query` and `stats`
    /// take) wins outright; otherwise the source `[rules]` names. Every
    /// `devkit rules` subcommand resolves it this way, so a project reads and
    /// edits the same rules everywhere.
    fn resolve(explicit: Option<PathBuf>) -> Result<Here> {
        let cwd = std::env::current_dir().context("getting current dir")?;
        let checkout = Checkout::at(&cwd);
        let source = match explicit {
            Some(path) => Source::at(path, repo_of(&checkout)),
            None => {
                let settings = devkit_common::config::resolve_in(&checkout, None, &cwd)
                    .map(|(project, _)| project.rules)
                    .unwrap_or_default();
                source(&settings, &checkout, Reader::Cli)
            }
        };
        Ok(Here {
            cwd,
            checkout,
            source,
        })
    }

    fn root(&self) -> &Path {
        self.checkout.root().unwrap_or(&self.cwd)
    }
}

/// The rules for this checkout, or those in the index named, and where they
/// came from. Errors name the location tried, because a `devkit rules` run is
/// a person asking a question and silence would read as "no rules" rather than
/// "no index".
fn load_or_default(explicit: Option<PathBuf>) -> Result<(String, RuleIndex)> {
    let source = Here::resolve(explicit)?.source;
    refresh_quietly(&source);
    let location = source.location();
    let loaded = source
        .read()?
        .with_context(|| format!("no rules index at {location}"))?;
    Ok((location, loaded))
}

/// The `[rules]` settings and the index the session hooks inject from, or
/// `None` when injection is off or no index loads. `devkit rules context` and
/// the brief's rules section both decide through this, so neither describes
/// rules the other would not deliver.
pub(crate) fn enabled_index(checkout: &Checkout, cwd: &Path) -> Option<(RulesConfig, RuleIndex)> {
    let rules = enabled_settings(checkout, cwd)?;
    let source = source(&rules, checkout, Reader::Session);
    refresh_quietly(&source);
    let loaded = source.load()?;
    Some((rules, loaded))
}

/// The `[rules]` settings when the session hooks have rules to inject from,
/// found without reading them: the brief's rules section decides through
/// this, and needs only to know the rules are there.
pub(crate) fn enabled_rules(checkout: &Checkout, cwd: &Path) -> Option<RulesConfig> {
    let rules = enabled_settings(checkout, cwd)?;
    source(&rules, checkout, Reader::Session)
        .present()
        .then_some(rules)
}

fn enabled_settings(checkout: &Checkout, cwd: &Path) -> Option<RulesConfig> {
    let (project, _) = devkit_common::config::resolve_in(checkout, None, cwd).ok()?;
    project.rules.enabled.then_some(project.rules)
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
            let id = here.source.add(&repo, fields)?;
            println!("{id}");
            Ok(())
        }
        RulesCommand::Edit(args) => {
            let here = Here::resolve(None)?;
            let fields = args.fields.into_fields(args.title, &here)?;
            here.source.edit(&args.id, fields)?;
            println!("edited {}", args.id);
            Ok(())
        }
        RulesCommand::Remove(args) => {
            let here = Here::resolve(None)?;
            here.source.remove(&args.id)?;
            println!("removed {}", args.id);
            Ok(())
        }
        RulesCommand::Pull => pull_cmd(),
    }
}

fn pull_cmd() -> Result<()> {
    let here = Here::resolve(None)?;
    let Some(refreshed) = here.source.refresh(Refresh::Pull) else {
        anyhow::bail!("rules source `file` has no cache to pull");
    };
    let refreshed = refreshed?;
    println!(
        "{}: revision {}, {} rules",
        here.source.location(),
        refreshed.revision,
        refreshed.rules
    );
    Ok(())
}

fn query_cmd(args: QueryArgs) -> Result<()> {
    let (_, index) = load_or_default(args.index_path)?;
    let repo_config = repo_config::load(Path::new(&index.repo))?;
    let cwd = std::env::current_dir().context("getting current dir")?;
    let checkout = Checkout::at(&cwd);
    let root = checkout.root().unwrap_or(&cwd);
    let filter = query::Filter {
        task: args.task,
        language: args.language.map(|l| vocab::canonical_language(&l)),
        scope: args.scope,
        severity: args.severity,
        min_severity: args.min_severity,
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
    warn_unknown_topics(&args.topics, &repo_config.topic_names());
    let limit = args
        .limit
        .or(repo_config.query.limit)
        .unwrap_or(repo_config::DEFAULT_QUERY_LIMIT);
    if limit > 0 && rules.len() > limit {
        eprintln!(
            "showing {limit} of {} rules; narrow the query or pass --limit 0 for all",
            rules.len()
        );
        rules.truncate(limit);
    }
    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&rules)?),
        Format::Prompt => print!(
            "{}",
            devkit_rules::render::block(
                devkit_rules::render::EDIT_HEADING,
                &rules,
                &[],
                usize::MAX
            )
        ),
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

/// A requested topic the repository does not define still ranks by rule text,
/// so it earns a warning rather than an error. A repository with no topics gets
/// none: every topic there is text-only.
fn warn_unknown_topics(requested: &[String], known: &[String]) {
    if known.is_empty() {
        return;
    }
    for topic in requested {
        if !known.contains(&vocab::vocabulary_key(topic)) {
            eprintln!(
                "warning: topic {topic:?} is not in the repo config ({}), so it matches rule text only",
                known.join(", ")
            );
        }
    }
}

fn stats_cmd(args: StatsArgs) -> Result<()> {
    use std::collections::BTreeMap;

    let (location, index) = load_or_default(args.index_path)?;
    println!("index  {location}");
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
    let hook = crate::brief::Hook::read(args.harness);
    if hook.is_fork() {
        return Ok(());
    }
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    let Some((settings, loaded)) = enabled_index(&Checkout::at(&cwd), &cwd) else {
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
    matched.truncate(settings.per_event_limit);

    let footer = "\nThe rest of this repository's rules are reachable with \
                  `devkit rules query --path <path>`.\n";
    let Some(budget) = settings.max_event_bytes.checked_sub(footer.len()) else {
        return Ok(());
    };
    let rendered = devkit_rules::render::fit(
        "Rules for all code in this repository",
        matched,
        Vec::new(),
        budget,
    );
    let mut text = rendered.text;
    text.push_str(footer);
    hook.emit(&text);
    // Without this the agent's first allowed write re-injects everything
    // this block just showed it.
    if let Some(holder) = hook.holder() {
        let ids: Vec<String> = rendered.rules.iter().map(|r| r.id.clone()).collect();
        crate::hook::rules::stamp_ids(&holder, &ids);
    }
    Ok(())
}
