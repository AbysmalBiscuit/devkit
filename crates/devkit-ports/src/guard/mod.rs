//! Decide whether a shell command has a devkit equivalent worth redirecting to.

pub mod appname;
pub mod catalog;
pub mod norm;
pub mod sig;
pub mod tasks;

use std::collections::{BTreeMap, HashMap};

use devkit_command::{Analysis, Invocation, Value};
use devkit_common::caller::Caller;
use devkit_config::{AppMatch, CommandRule, Config, RuleAction, RunAction, RunArg, Severity};
use norm::basename;

use crate::apps::App;

/// The resolved half of the answer, absent when only the `[harness]` probe ran.
pub struct Project {
    pub config: Config,
    pub catalog: HashMap<String, App>,
    /// The hook's cwd relative to the project root, used as the last-resort
    /// app-name hint.
    pub cwd_rel: Option<String>,
    /// `[harness.app_match]`. It rides here rather than on `Config` because
    /// `Config` has no `harness` field, and because app naming is the only
    /// thing that reads it.
    pub app_match: AppMatch,
}

#[derive(Debug)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: Option<String>,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Verdict {
    pub blocks: Vec<Finding>,
    pub warnings: Vec<Finding>,
}

/// The guard's reading of one invocation: the program and arguments as
/// strings when every word is known.
struct Known {
    argv: Vec<String>,
    doppler: Option<norm::Doppler>,
}

fn known(inv: &Invocation) -> Option<Known> {
    let mut argv = vec![inv.program.known()?.to_string()];
    for arg in &inv.args {
        argv.push(arg.known()?.to_string());
    }
    Some(Known {
        argv,
        doppler: norm::doppler_of(inv),
    })
}

/// Ordered from weakest to strongest, so combining alternatives takes the
/// maximum and combining a sequence takes the minimum.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Match {
    No,
    Possible,
    Yes,
}

fn rule_match(inv: &Invocation, rule: &CommandRule) -> Match {
    let program = match inv.program.known() {
        Some(program) => basename(program),
        None => return Match::Possible,
    };
    if !rule.programs.iter().any(|p| basename(p) == program) {
        return Match::No;
    }
    args_match(&rule.args, &inv.semantic_args)
}

/// Whether `args` starts with `patterns`. A `"**"` entry stands for zero or
/// more whole arguments; every other entry matches one argument.
fn args_match(patterns: &[String], args: &[Value]) -> Match {
    let Some((pattern, patterns)) = patterns.split_first() else {
        return Match::Yes;
    };
    if pattern == "**" {
        return (0..=args.len())
            .map(|skipped| args_match(patterns, &args[skipped..]))
            .max()
            .unwrap_or(Match::No);
    }
    let Some((arg, args)) = args.split_first() else {
        return Match::No;
    };
    let here = match arg {
        Value::Known(actual) if wildcard_matches(pattern, actual) => Match::Yes,
        Value::Known(_) => return Match::No,
        Value::Unknown | Value::Ephemeral(_) => Match::Possible,
    };
    here.min(args_match(patterns, args))
}

/// Whether `text` matches `pattern`, where each `*` stands for any run of
/// characters, `/` included. A pattern without `*` matches exactly.
fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let head = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(head) else {
        return false;
    };
    let Some(tail) = parts.next_back() else {
        return rest.is_empty();
    };
    for part in parts {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(tail)
}

/// Decide over an analysis. Every invocation is checked, nested ones
/// included; findings keep invocation order, then rule name order.
pub fn decide(
    analysis: &Analysis,
    rules: &BTreeMap<String, CommandRule>,
    project: Option<&Project>,
) -> Verdict {
    let mut verdict = Verdict::default();
    for inv in &analysis.invocations {
        if inv
            .program
            .known()
            .map(basename)
            .is_some_and(devkit_common::shim::is_devkit_command)
        {
            continue;
        }
        let typed = inv.typed.join(" ");
        let mut undetermined = false;
        for (name, rule) in rules
            .iter()
            .filter(|(_, rule)| rule.enabled && !rule.programs.is_empty())
        {
            match rule_match(inv, rule) {
                Match::Yes => {
                    let finding = Finding {
                        rule: Some(name.clone()),
                        severity: rule.severity,
                        message: rule.reason.clone(),
                    };
                    match rule.action {
                        RuleAction::Block => verdict.blocks.push(finding),
                        RuleAction::Warn => verdict.warnings.push(finding),
                    }
                }
                Match::Possible => undetermined = true,
                Match::No => {}
            }
        }
        if undetermined {
            verdict.warnings.push(Finding {
                rule: None,
                severity: Severity::Warning,
                message: format!("`{typed}` could not be fully resolved, so devkit could not tell whether a `[harness.commands]` rule applies; it was allowed."),
            });
        }
        let Some(project) = project else { continue };
        match known(inv) {
            Some(known) => {
                let program = basename(&known.argv[0]).to_string();
                let normalized = norm_view(&known);
                if let Some(message) = project_hit(&inv.typed, &normalized, &program, project) {
                    verdict.blocks.push(Finding {
                        rule: None,
                        severity: Severity::Error,
                        message,
                    });
                }
            }
            None if !project.config.tasks.is_empty() || !project.catalog.is_empty() => {
                verdict.warnings.push(Finding {
                    rule: None,
                    severity: Severity::Info,
                    message: format!("`{typed}` could not be fully resolved, so devkit did not check it against the project's tasks and apps."),
                })
            }
            None => {}
        }
    }
    verdict
}

/// Kept for callers holding a bash command string: the first block, if any.
pub fn decide_with(
    command: &str,
    rules: &BTreeMap<String, CommandRule>,
    project: Option<&Project>,
) -> Decision {
    let analysis = devkit_command::analyze(command, &bash_context());
    match decide(&analysis, rules, project).blocks.into_iter().next() {
        Some(finding) => Decision::Deny {
            reason: finding.message,
        },
        None => Decision::Allow,
    }
}

fn bash_context() -> devkit_command::Context {
    devkit_command::Context {
        dialect: devkit_command::Dialect::Bash,
        cwd: None,
        path_style: if cfg!(windows) {
            devkit_command::PathStyle::Windows
        } else {
            devkit_command::PathStyle::Unix
        },
        limits: devkit_command::Limits::default(),
    }
}

/// A configured argv, unwrapped the same way as a typed command.
fn configured(argv: &[String]) -> Option<Known> {
    devkit_command::analyze_argv(argv, &bash_context())
        .invocations
        .first()
        .and_then(known)
}

/// A task's `run` unwrapped the same way as a typed command.
///
/// A split renders to an unknown number of words, so nothing after one sits at
/// a knowable offset: a scalar following a split gives up on the task rather
/// than matching a typed command at the wrong position. The words before the
/// first split also have to carry the signature on their own, which takes at
/// least two of them. `["git", { split }]` would otherwise claim every `git`.
fn configured_task(args: &[RunArg]) -> Option<Known> {
    let words = |args: &[RunArg]| -> Vec<String> {
        args.iter().map(|a| a.template().to_string()).collect()
    };
    let word_count_unknown = |a: &RunArg| match a {
        RunArg::Scalar(_) => false,
        RunArg::Action(RunAction::Split { .. }) => true,
    };
    let Some(first_split) = args.iter().position(word_count_unknown) else {
        return configured(&words(args));
    };
    if first_split < 2 || !args[first_split..].iter().all(word_count_unknown) {
        return None;
    }
    let mut argv: Vec<String> = words(&args[..first_split]);
    argv.push(sig::OPAQUE.into());
    configured(&argv)
}

struct Normalized {
    argv: Vec<String>,
    doppler: Option<norm::Doppler>,
}

fn norm_view(known: &Known) -> Normalized {
    Normalized {
        argv: known.argv.clone(),
        doppler: known.doppler.clone(),
    }
}

/// Sources 3 through 5, in order.
///
/// `typed` is the segment as lexed and `n` the same segment with its wrappers
/// stripped. Every match runs on `n`; every message quotes `typed`, so the
/// message names the words the agent typed rather than the shorter command
/// runner-prefix stripping leaves behind. Lexing has already resolved quoting,
/// so a quoted argument comes back unquoted.
fn project_hit(typed: &[String], n: &Normalized, prog: &str, p: &Project) -> Option<String> {
    // The catalog knows which verbs of the programs it covers start a server,
    // and a claim resting on the program word alone does not: a task
    // `run = ["vite"]` would otherwise deny `vite build`. A task that names the
    // verb itself still claims the command.
    let min_sig = if catalog::is_known_program(prog) && !catalog::is_dev_server(&n.argv) {
        2
    } else {
        1
    };

    if let Some(name) = best_task(n, p, min_sig) {
        // The guard only ever fires because a coding agent acted, and its
        // stdin is a harness pipe, so the TTY says nothing. Name the set the
        // agent will actually be asked for.
        let usage: String = crate::task::task_args(&p.config, &name, Caller::Agent)
            .unwrap_or_default()
            .iter()
            .filter(|a| a.required)
            .map(|a| format!(" --arg {0}=<{0}>", a.name))
            .collect();
        return Some(format!(
            "`{}` is the `{name}` task. Run `devrun task {name}{usage}` so it gets its app \
             directory, layered env and allocated ports.",
            typed.join(" ")
        ));
    }

    let hits = matching_apps(n, p);

    // The catalog outranks an app's `launch` prefix: it knows which verbs start
    // a server, and a launch signature does not. The launch match still
    // supplies the app name, so the candidate set narrows to it when there
    // was one.
    if catalog::is_known_program(prog) {
        if !catalog::is_dev_server(&n.argv) {
            return None;
        }
        if !hits.is_empty() {
            return Some(up_message(typed, &narrowed_apps(n, &hits, p)));
        }
        return Some(match searched_app(n, p) {
            Some(app) => catalog_message(typed, &app),
            None => format!(
                "`{}` starts a dev server. Start it with `devrun up <app>` so its port is \
                 registered and `devrun down`/`logs`/`status` can see it. \
                 `devkit config apps` lists the apps.",
                typed.join(" ")
            ),
        });
    }

    if hits.is_empty() {
        return None;
    }
    Some(up_message(typed, &narrowed_apps(n, &hits, p)))
}

/// The launch match narrowed to one app, or every app tied on that launch.
///
/// The match is what ties the command to these apps, so a lone one is named
/// without a hint. Several apps sharing a `launch` need the hint to pick one,
/// and when none resolves they are all named.
fn narrowed_apps(n: &Normalized, hits: &[String], p: &Project) -> Vec<String> {
    let candidates: Vec<&App> = hits.iter().filter_map(|name| p.catalog.get(name)).collect();
    let hint = appname::hint(&n.argv, p.cwd_rel.as_deref());
    match appname::resolve(
        hint.as_deref(),
        &candidates,
        &p.app_match,
        appname::Scope::Narrowed,
    ) {
        Some(app) => vec![app],
        None => hits.to_vec(),
    }
}

/// The app a whole-catalog search names.
///
/// Nothing tied the command to any app here, so the hint is the only route to a
/// name and an unresolved one names none — which is what sends the caller to
/// `devkit config apps` rather than to whichever app the project happens to
/// declare.
fn searched_app(n: &Normalized, p: &Project) -> Option<String> {
    let candidates: Vec<&App> = p.catalog.values().collect();
    let hint = appname::hint(&n.argv, p.cwd_rel.as_deref());
    appname::resolve(
        hint.as_deref(),
        &candidates,
        &p.app_match,
        appname::Scope::Catalog,
    )
}

/// A task whose signature the typed segment matches.
struct TaskHit<'a> {
    sig_len: usize,
    name: &'a str,
    /// The app the task is scoped to, which is what a hint resolves against.
    app: Option<&'a str>,
}

/// The task this segment retypes, if redirecting to it would change the
/// process.
///
/// Ranked by signature length, then by hint resolution among the tasks tied at
/// that length, then by name. Length first because a longer signature is the
/// more specific claim on the command; the hint next because a tie means
/// several tasks retype the same command and only the app the agent is working
/// in says which was meant; name last so a `HashMap`'s iteration order cannot
/// make the message name a different task from one call to the next.
///
/// `min_sig` is the shortest signature allowed to claim this segment; the
/// caller raises it to exclude a bare-program task from a command the catalog
/// has already read as something other than a server.
fn best_task(n: &Normalized, p: &Project, min_sig: usize) -> Option<String> {
    let mut hits: Vec<TaskHit<'_>> = p
        .config
        .tasks
        .iter()
        .filter_map(|(name, task)| {
            // Both sides normalize, or a task's own runner prefix and doppler
            // wrapper make it unmatchable against the stripped typed side.
            let cfg = configured_task(&task.run)?;
            let s = sig::signature(&cfg.argv)?;
            // The floor is a heuristic about bare-program tasks. A task that
            // states its own `guard` answer has already settled the question,
            // so it is never filtered out before that answer is read.
            if task.guard.is_none() && s.len() < min_sig {
                return None;
            }
            if !sig::matches(&s, &n.argv) {
                return None;
            }
            tasks::redirect_worth_it(
                task,
                &p.config.templates.defaults(),
                n.doppler.as_ref(),
                cfg.doppler.as_ref(),
            )
            .then_some(TaskHit {
                sig_len: s.len(),
                name: name.as_str(),
                app: task.app.as_deref(),
            })
        })
        .collect();
    hits.sort_by(|a, b| b.sig_len.cmp(&a.sig_len).then_with(|| a.name.cmp(b.name)));
    let best = hits.first()?.sig_len;
    let tied = &hits[..hits.iter().take_while(|h| h.sig_len == best).count()];

    // `Scope::Catalog`, because the rung is hint *resolution*: a lone candidate
    // named without one would let a single app-scoped task beat an appless one
    // on nothing.
    let mut candidates: Vec<&App> = Vec::new();
    for app in tied
        .iter()
        .filter_map(|h| h.app.and_then(|a| p.catalog.get(a)))
    {
        if !candidates.iter().any(|c| c.name == app.name) {
            candidates.push(app);
        }
    }
    if let Some(app) = appname::resolve(
        appname::hint(&n.argv, p.cwd_rel.as_deref()).as_deref(),
        &candidates,
        &p.app_match,
        appname::Scope::Catalog,
    ) && let Some(h) = tied.iter().find(|h| h.app == Some(app.as_str()))
    {
        return Some(h.name.to_string());
    }
    tied.first().map(|h| h.name.to_string())
}

/// Every app whose `launch` signature this segment matches, sorted by name.
///
/// Narrowing only: the longest signature wins and every app tied at that length
/// comes back, because which of a tie the command meant is the hint's question,
/// not this one's.
fn matching_apps(n: &Normalized, p: &Project) -> Vec<String> {
    let mut hits: Vec<(usize, &str)> = p
        .catalog
        .values()
        .filter_map(|app| {
            let launch = configured(&app.launch)?;
            let s = sig::signature(&launch.argv)?;
            sig::matches(&s, &n.argv).then_some((s.len(), app.name.as_str()))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    let Some(&(best, _)) = hits.first() else {
        return Vec::new();
    };
    hits.iter()
        .take_while(|(len, _)| *len == best)
        .map(|(_, name)| (*name).to_string())
        .collect()
}

/// For a command that matched an app's `launch`: the typed command *is* how
/// devkit starts that app, so the message says so.
fn up_message(typed: &[String], apps: &[String]) -> String {
    let ups = apps
        .iter()
        .map(|a| format!("`devrun up {a}`"))
        .collect::<Vec<_>>()
        .join(" or ");
    format!(
        "`{}` is how devkit launches this app. Run {ups} instead, so the port is allocated from \
         the registry and `devrun down`/`logs`/`status` can see the server.",
        typed.join(" ")
    )
}

/// For a command the catalog recognised and a hint attributed to an app: the
/// command is a dev server and this is the app it belongs to, but it is not
/// that app's `launch`, so the message claims only the redirect.
fn catalog_message(typed: &[String], app: &str) -> String {
    format!(
        "`{}` starts a dev server. `devrun up {app}` is the registered way to start this app's \
         server, so the port is allocated from the registry and `devrun down`/`logs`/`status` \
         can see it.",
        typed.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use devkit_config::{AppConfig, AppMatch, CommandRule, RuleAction, Severity, TaskConfig};

    use super::*;

    fn rules(programs: &[&str], reason: &str) -> BTreeMap<String, CommandRule> {
        BTreeMap::from([("test-rule".to_string(), CommandRule {
            programs: programs.iter().map(|s| s.to_string()).collect(),
            args: Vec::new(),
            reason: reason.into(),
            ..CommandRule::default()
        })])
    }

    fn rule(
        programs: &[&str],
        args: &[&str],
        edit: impl FnOnce(&mut CommandRule),
    ) -> BTreeMap<String, CommandRule> {
        let mut r = CommandRule {
            programs: programs.iter().map(|s| s.to_string()).collect(),
            args: args.iter().map(|s| s.to_string()).collect(),
            reason: "use issue".into(),
            ..CommandRule::default()
        };
        edit(&mut r);
        BTreeMap::from([("worktree-add".to_string(), r)])
    }

    fn verdict(command: &str, rules: &BTreeMap<String, CommandRule>) -> Verdict {
        let ctx = devkit_command::Context {
            dialect: devkit_command::Dialect::Bash,
            cwd: None,
            path_style: devkit_command::PathStyle::Unix,
            limits: devkit_command::Limits::default(),
        };
        decide(&devkit_command::analyze(command, &ctx), rules, None)
    }

    fn project(build: impl FnOnce(&mut Config)) -> Project {
        let mut config = Config::default();
        build(&mut config);
        let catalog = config
            .apps
            .iter()
            .map(|(name, a)| {
                (name.clone(), App {
                    name: name.clone(),
                    base_port: a.base_port,
                    path: a.path.clone().unwrap_or_else(|| format!("apps/{name}")),
                    launch: a.launch.clone(),
                    url: None,
                    url_env: None,
                    provides_url: false,
                    static_env: Default::default(),
                    prep_files: Vec::new(),
                    setup: Vec::new(),
                })
            })
            .collect();
        Project {
            config,
            catalog,
            cwd_rel: None,
            app_match: AppMatch::default(),
        }
    }

    fn denies(d: &Decision) -> bool {
        matches!(d, Decision::Deny { .. })
    }

    fn reason(d: &Decision) -> &str {
        match d {
            Decision::Deny { reason } => reason,
            Decision::Allow => panic!("expected a denial"),
        }
    }

    #[test]
    fn git_global_options_do_not_hide_a_worktree_rule() {
        let r = rule(&["git"], &["worktree", "add"], |_| {});
        assert_eq!(
            verdict("git -C /repo worktree add ../wt", &r).blocks.len(),
            1
        );
        assert!(verdict("git -C /repo worktree list", &r).blocks.is_empty());
    }

    #[test]
    fn a_warn_rule_warns_and_a_disabled_rule_is_silent() {
        let warn = rule(&["git"], &["worktree", "add"], |r| {
            r.action = RuleAction::Warn;
            r.severity = Severity::Warning;
        });
        let v = verdict("git worktree add x", &warn);
        assert!(v.blocks.is_empty());
        assert_eq!(v.warnings[0].severity, Severity::Warning);
        let off = rule(&["git"], &["worktree", "add"], |r| r.enabled = false);
        let v = verdict("git worktree add x", &off);
        assert!(v.blocks.is_empty() && v.warnings.is_empty());
    }

    #[test]
    fn an_undetermined_argument_is_a_possible_match_that_warns() {
        let r = rule(&["git"], &["worktree", "add"], |_| {});
        let v = verdict("git \"$verb\" add /tmp/wt", &r);
        assert!(v.blocks.is_empty());
        assert_eq!(
            v.warnings.len(),
            1,
            "{:?}",
            v.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rules_see_nested_commands_but_not_quoted_mentions() {
        let r = rule(&["git"], &["worktree", "add"], |_| {});
        assert_eq!(verdict("bash -c 'git worktree add x'", &r).blocks.len(), 1);
        assert!(verdict("echo 'git worktree add x'", &r).blocks.is_empty());
        assert!(
            verdict("git commit -m \"git worktree add x\"", &r)
                .blocks
                .is_empty()
        );
    }

    #[test]
    fn a_devkit_shim_is_never_gated() {
        let r = rules(&["node"], "use bun");
        assert!(!denies(&decide_with("devrun up web", &r, None)));
        assert!(!denies(&decide_with("devkit config apps", &r, None)));
    }

    /// `issue` and `devkit-mcp` are shims too, and both read like ordinary
    /// words a project might write a rule about.
    #[test]
    fn a_rule_naming_a_shim_does_not_fire() {
        let r = rules(&["issue", "devkit-mcp"], "unreachable");
        assert!(!denies(&decide_with("issue setup ENG-1", &r, None)));
        assert!(!denies(&decide_with("devkit-mcp", &r, None)));
    }

    #[test]
    fn a_user_rule_denies_with_its_reason() {
        let r = rules(&["node"], "This workspace is bun-only.");
        let d = decide_with("node server.js", &r, None);
        assert_eq!(reason(&d), "This workspace is bun-only.");
    }

    #[test]
    fn a_user_rule_with_no_args_matches_the_bare_program() {
        let r = rules(&["node"], "This workspace is bun-only.");
        assert!(denies(&decide_with("node", &r, None)));
    }

    #[test]
    fn a_user_rule_with_args_needs_them() {
        let mut r = rules(&["docker-compose"], "use docker compose");
        r.get_mut("test-rule").unwrap().args = vec!["up".into()];
        assert!(denies(&decide_with("docker-compose up -d", &r, None)));
        assert!(!denies(&decide_with("docker-compose logs", &r, None)));
        assert!(!denies(&decide_with("docker-compose", &r, None)));
    }

    #[test]
    fn a_star_in_rule_args_matches_any_run_of_characters() {
        let bin = rule(&["node"], &["*/nitro", "dev"], |_| {});
        for command in [
            "node node_modules/.bin/nitro dev --port 9200",
            "node ./node_modules/.bin/nitro dev",
            "node /abs/apps/api/node_modules/.bin/nitro dev",
        ] {
            assert!(denies(&decide_with(command, &bin, None)), "{command}");
        }
        for command in [
            "node node_modules/.bin/nitro build",
            "node scripts/nitro-helper.mjs dev",
            "node nitro dev",
        ] {
            assert!(!denies(&decide_with(command, &bin, None)), "{command}");
        }

        let cli = rule(&["node"], &["*/nitropack/*", "dev"], |_| {});
        assert!(denies(&decide_with(
            "node node_modules/nitropack/dist/cli/index.mjs dev",
            &cli,
            None
        )));
        assert!(!denies(&decide_with(
            "node node_modules/nitropack/dist/cli/index.mjs build",
            &cli,
            None
        )));
    }

    #[test]
    fn a_lone_star_still_needs_an_argument() {
        let any = rule(&["node"], &["*"], |_| {});
        assert!(denies(&decide_with("node server.js", &any, None)));
        assert!(!denies(&decide_with("node", &any, None)));
    }

    #[test]
    fn a_double_star_rule_arg_skips_any_number_of_arguments() {
        let server = rule(&["bun"], &["**", "nitro", "dev"], |_| {});
        for command in [
            "bun nitro dev",
            "bun --cwd . nitro dev --port 9200",
            "bun --filter=@adaptyv/api --smol nitro dev",
        ] {
            assert!(denies(&decide_with(command, &server, None)), "{command}");
        }
        for command in [
            "bun nitro build",
            "bun dev nitro",
            "bun --cwd . nitro",
            "bun",
        ] {
            assert!(!denies(&decide_with(command, &server, None)), "{command}");
        }

        let bin = rule(&["node"], &["**", "*/nitro", "dev"], |_| {});
        assert!(denies(&decide_with(
            "node --inspect node_modules/.bin/nitro dev",
            &bin,
            None
        )));

        let trailing = rule(&["git"], &["worktree", "**"], |_| {});
        assert!(denies(&decide_with("git worktree", &trailing, None)));
        assert!(!denies(&decide_with("git status", &trailing, None)));
    }

    #[test]
    fn a_double_star_rule_arg_warns_when_only_an_unresolved_argument_could_match() {
        let server = rule(&["bun"], &["**", "nitro", "dev"], |_| {});
        let skipped = verdict(r#"bun "$FLAG" nitro dev"#, &server);
        assert_eq!(skipped.blocks.len(), 1);
        let unresolved = verdict(r#"bun --cwd . "$NAME" dev"#, &server);
        assert!(unresolved.blocks.is_empty());
        assert_eq!(unresolved.warnings.len(), 1);
    }

    #[test]
    fn wildcard_matching() {
        for (pattern, text) in [
            ("dev", "dev"),
            ("*", ""),
            ("*", "anything/at/all"),
            ("*/nitro", "a/b/nitro"),
            ("node_modules/*", "node_modules/x"),
            ("*/nitropack/*", "node_modules/nitropack/dist/cli.mjs"),
            ("a*b*c", "abc"),
            ("a*b*c", "aXbYbZc"),
            ("**", "x"),
        ] {
            assert!(wildcard_matches(pattern, text), "{pattern} ~ {text}");
        }
        for (pattern, text) in [
            ("dev", "devx"),
            ("*/nitro", "nitro"),
            ("*/nitro", "a/nitro/b"),
            ("a*a", "a"),
            ("a*b*c", "acb"),
            ("x*", "yx"),
        ] {
            assert!(!wildcard_matches(pattern, text), "{pattern} !~ {text}");
        }
    }

    #[test]
    fn an_empty_programs_rule_matches_nothing() {
        let r = rules(&[], "unreachable");
        assert!(!denies(&decide_with("node server.js", &r, None)));
    }

    #[test]
    fn a_task_redirect_names_the_task() {
        let p = project(|c| {
            c.tasks.insert("check".into(), TaskConfig {
                run: vec!["bun".into(), "test".into()],
                app: Some("web".into()),
                ..Default::default()
            });
        });
        let d = decide_with("bun test", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun task check"), "{}", reason(&d));
    }

    #[test]
    fn a_redirect_names_an_arg_marked_for_agents_over_its_default() {
        let p = project(|c| {
            c.templates
                .variables
                .insert("msg".into(), devkit_config::VariableDecl::Table {
                    default: Some("wip".into()),
                    required: Some(devkit_config::Required::Agents),
                });
            c.tasks.insert(
                "commit".into(),
                toml::from_str("run = [\"git\", \"commit\", \"-m\", \"{{ msg }}\"]\nguard = true")
                    .unwrap(),
            );
        });
        let d = decide_with("git commit -m wip", &BTreeMap::new(), Some(&p));
        // The hint is built with Caller::Agent rather than detection, so it
        // names what the redirected command will actually be refused for. A
        // default would otherwise make this arg look optional here and then
        // fail the run the redirect sends the agent to.
        assert!(
            reason(&d).contains("devrun task commit --arg msg=<msg>"),
            "{}",
            reason(&d)
        );
    }

    #[test]
    fn a_trailing_split_keeps_the_static_task_signature() {
        for run in [
            r#"["git", "add", "--", { split = "{{ files }}", on = ";" }]"#,
            r#"["git", "add", { split = "{{ files }}", on = ";" }]"#,
        ] {
            let p = project(|c| {
                c.tasks.insert(
                    "stage".into(),
                    toml::from_str(&format!("run = {run}\nguard = true")).unwrap(),
                );
            });
            let d = decide_with("git add -- selected.txt", &BTreeMap::new(), Some(&p));
            assert!(
                reason(&d).contains("devrun task stage --arg files=<files>"),
                "{run}: {}",
                reason(&d)
            );
            assert!(!denies(&decide_with(
                "git diff",
                &BTreeMap::new(),
                Some(&p)
            )));
        }
    }

    #[test]
    fn splits_cannot_supply_a_guard_signature_or_wrapper_argument() {
        let split = r#"{ split = "{{ args }}", on = ";" }"#;
        for (run, typed) in [
            (
                format!(r#"["docker", "compose", {split}, "up"]"#),
                "docker compose down",
            ),
            (format!(r#"["sudo", "-u", {split}]"#), "sudo ls"),
            (
                format!(r#"["doppler", "run", "-c", {split}]"#),
                "doppler run -c dev -- git status",
            ),
            (format!(r#"["bun", "--", {split}]"#), "bun test"),
            (format!(r#"["git", {split}]"#), "git status"),
        ] {
            let p = project(|c| {
                c.tasks.insert(
                    "dynamic".into(),
                    toml::from_str(&format!("run = {run}\nguard = true")).unwrap(),
                );
            });
            let d = decide_with(typed, &BTreeMap::new(), Some(&p));
            assert!(!denies(&d), "{run}: {}", reason(&d));
        }
    }

    /// A bare-program task is normally held back from a command the catalog
    /// reads as something other than a server. An explicit `guard = true` is
    /// the project overruling that, and it has to reach the decision to do
    /// so.
    #[test]
    fn an_explicit_guard_outranks_the_bare_program_floor() {
        let forced = |guard| {
            project(move |c| {
                c.tasks.insert("bundle".into(), TaskConfig {
                    run: vec!["vite".into()],
                    guard,
                    ..Default::default()
                });
            })
        };
        let d = decide_with("vite build", &BTreeMap::new(), Some(&forced(Some(true))));
        assert!(reason(&d).contains("devrun task bundle"), "{}", reason(&d));
        assert!(!denies(&decide_with(
            "vite build",
            &BTreeMap::new(),
            Some(&forced(None))
        )));
        assert!(!denies(&decide_with(
            "vite build",
            &BTreeMap::new(),
            Some(&forced(Some(false)))
        )));
    }

    #[test]
    fn a_task_with_its_own_runner_prefix_still_matches() {
        let p = project(|c| {
            c.tasks.insert("lint".into(), TaskConfig {
                run: vec!["bun".into(), "run".into(), "lint".into()],
                app: Some("web".into()),
                ..Default::default()
            });
        });
        let d = decide_with("bun run lint", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun task lint"), "{}", reason(&d));
    }

    #[test]
    fn a_differing_doppler_config_denies_and_a_matching_one_does_not() {
        let p = project(|c| {
            c.tasks.insert("check".into(), TaskConfig {
                run: ["doppler", "run", "-c", "dev", "--", "bun", "test"]
                    .iter()
                    .map(|s| (*s).into())
                    .collect(),
                ..Default::default()
            });
        });
        let denied = decide_with("doppler run -c prd -- bun test", &BTreeMap::new(), Some(&p));
        assert!(
            reason(&denied).contains("devrun task check"),
            "{}",
            reason(&denied)
        );
        assert!(!denies(&decide_with(
            "doppler run -c dev -- bun test",
            &BTreeMap::new(),
            Some(&p)
        )));
    }

    #[test]
    fn an_identical_task_is_allowed_through() {
        let p = project(|c| {
            c.tasks.insert("check".into(), TaskConfig {
                run: vec!["bun".into(), "test".into()],
                ..Default::default()
            });
        });
        assert!(!denies(&decide_with(
            "bun test",
            &BTreeMap::new(),
            Some(&p)
        )));
    }

    #[test]
    fn the_longest_matching_task_wins() {
        let p = project(|c| {
            for (name, run) in [
                ("short", vec!["bun", "test"]),
                ("long", vec!["bun", "test", "unit"]),
            ] {
                c.tasks.insert(name.into(), TaskConfig {
                    run: run.iter().map(|s| (*s).into()).collect(),
                    app: Some("web".into()),
                    ..Default::default()
                });
            }
        });
        for _ in 0..20 {
            let d = decide_with("bun test unit", &BTreeMap::new(), Some(&p));
            assert!(reason(&d).contains("devrun task long"), "{}", reason(&d));
        }
    }

    /// Two tasks that retype the same command, each scoped to a different app.
    fn two_tasks_tied_on_one_command(cwd_rel: Option<&str>) -> Project {
        let mut p = project(|c| {
            for (task, app) in [("admin-test", "admin"), ("web-test", "web")] {
                c.apps.insert(app.into(), AppConfig {
                    base_port: 3000,
                    path: Some(format!("apps/{app}")),
                    ..Default::default()
                });
                c.tasks.insert(task.into(), TaskConfig {
                    run: vec!["bun".into(), "test".into()],
                    app: Some(app.into()),
                    ..Default::default()
                });
            }
        });
        p.cwd_rel = cwd_rel.map(str::to_string);
        p
    }

    #[test]
    fn a_tie_between_tasks_is_broken_by_the_hint() {
        let p = two_tasks_tied_on_one_command(Some("apps/web"));
        let d = decide_with("bun test", &BTreeMap::new(), Some(&p));
        assert!(
            reason(&d).contains("devrun task web-test"),
            "{}",
            reason(&d)
        );
    }

    #[test]
    fn a_tie_no_hint_resolves_falls_back_to_name_order() {
        let p = two_tasks_tied_on_one_command(None);
        let d = decide_with("bun test", &BTreeMap::new(), Some(&p));
        assert!(
            reason(&d).contains("devrun task admin-test"),
            "{}",
            reason(&d)
        );
    }

    #[test]
    fn a_catalog_search_names_an_app_without_calling_the_command_its_launch() {
        let mut p = project(|c| {
            c.apps.insert("web".into(), AppConfig {
                base_port: 3000,
                launch: vec![
                    "nitro".into(),
                    "dev".into(),
                    "--port".into(),
                    "{{ port }}".into(),
                ],
                path: Some("apps/web".into()),
                ..Default::default()
            });
        });
        p.cwd_rel = Some("apps/web".into());
        let d = decide_with("uvicorn app:app", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("`devrun up web`"), "{}", reason(&d));
        assert!(
            reason(&d).starts_with("`uvicorn app:app` starts a dev server."),
            "{}",
            reason(&d)
        );
        assert!(
            !reason(&d).contains("is how devkit launches"),
            "{}",
            reason(&d)
        );
    }

    #[test]
    fn an_app_launch_redirects_to_devrun_up() {
        let p = project(|c| {
            c.apps.insert("web".into(), AppConfig {
                base_port: 3000,
                launch: vec![
                    "nitro".into(),
                    "dev".into(),
                    "--port".into(),
                    "{{ port }}".into(),
                ],
                ..Default::default()
            });
        });
        let d = decide_with("nitro dev", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun up web"), "{}", reason(&d));
    }

    #[test]
    fn the_catalog_outranks_a_launch_prefix() {
        let p = project(|c| {
            c.apps.insert("web".into(), AppConfig {
                base_port: 3000,
                launch: vec!["vite".into(), "--port".into(), "{{ port }}".into()],
                ..Default::default()
            });
        });
        assert!(!denies(&decide_with(
            "vite build",
            &BTreeMap::new(),
            Some(&p)
        )));
        assert!(denies(&decide_with("vite", &BTreeMap::new(), Some(&p))));
    }

    fn vite_task(name: &str, run: &[&str]) -> Project {
        project(|c| {
            c.tasks.insert(name.into(), TaskConfig {
                run: run.iter().map(|s| (*s).into()).collect(),
                app: Some("storefront".into()),
                ..Default::default()
            });
        })
    }

    #[test]
    fn the_catalog_outranks_a_bare_program_task() {
        let p = vite_task("dev-sf", &["vite"]);
        assert!(!denies(&decide_with(
            "vite build",
            &BTreeMap::new(),
            Some(&p)
        )));
    }

    #[test]
    fn a_bare_program_task_still_claims_the_serving_invocation() {
        let p = vite_task("dev-sf", &["vite"]);
        let d = decide_with("vite", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun task dev-sf"), "{}", reason(&d));
    }

    #[test]
    fn a_task_naming_the_verb_claims_it_from_the_catalog() {
        let p = vite_task("bundle", &["vite", "build"]);
        let d = decide_with("vite build", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun task bundle"), "{}", reason(&d));
    }

    #[test]
    fn a_trailing_comment_is_not_a_command() {
        let p = project(|_| {});
        assert!(!denies(&decide_with(
            "cargo build   # TODO(next dev)",
            &BTreeMap::new(),
            Some(&p)
        )));
        assert!(!denies(&decide_with(
            "ls # build && next dev",
            &BTreeMap::new(),
            Some(&p)
        )));
        assert!(!denies(&decide_with(
            "ls # then run: cd x; uvicorn app",
            &BTreeMap::new(),
            Some(&p)
        )));
    }

    #[test]
    fn a_catalog_verb_that_does_not_serve_is_allowed_with_no_apps_at_all() {
        let p = project(|_| {});
        assert!(!denies(&decide_with(
            "next build",
            &BTreeMap::new(),
            Some(&p)
        )));
    }

    #[test]
    fn a_launch_prefix_outside_the_catalog_still_denies() {
        let p = project(|c| {
            c.apps.insert("api".into(), AppConfig {
                base_port: 4000,
                launch: vec![
                    "hypercorn".into(),
                    "app:app".into(),
                    "--bind".into(),
                    "0.0.0.0:{{ port }}".into(),
                ],
                ..Default::default()
            });
        });
        let d = decide_with("hypercorn app:app", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun up api"), "{}", reason(&d));
    }

    #[test]
    fn a_catalog_hit_with_no_app_names_the_listing_command() {
        let p = project(|_| {});
        let d = decide_with("uvicorn app:app", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devkit config apps"), "{}", reason(&d));
    }

    /// A whole-catalog search is a search even when the catalog holds one app.
    /// Nothing tied `uvicorn` to a nitro app, so naming it would send the agent
    /// at the wrong server.
    #[test]
    fn a_catalog_hit_matching_no_launch_names_no_app_in_a_one_app_project() {
        let p = project(|c| {
            c.apps.insert("web".into(), AppConfig {
                base_port: 3000,
                launch: vec![
                    "nitro".into(),
                    "dev".into(),
                    "--port".into(),
                    "{{ port }}".into(),
                ],
                ..Default::default()
            });
        });
        let d = decide_with("uvicorn app:app", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devkit config apps"), "{}", reason(&d));
        assert!(!reason(&d).contains("devrun up web"), "{}", reason(&d));
    }

    fn three_apps_sharing_one_launch(cwd_rel: Option<&str>) -> Project {
        let mut p = project(|c| {
            for name in ["admin", "storefront", "web"] {
                c.apps.insert(name.into(), AppConfig {
                    base_port: 3000,
                    launch: vec![
                        "bun".into(),
                        "run".into(),
                        "dev".into(),
                        "--".into(),
                        "--port".into(),
                        "{{ port }}".into(),
                    ],
                    path: Some(format!("apps/{name}")),
                    ..Default::default()
                });
            }
        });
        p.cwd_rel = cwd_rel.map(str::to_string);
        p
    }

    #[test]
    fn a_resolving_hint_picks_one_of_several_apps_sharing_a_launch() {
        let p = three_apps_sharing_one_launch(Some("apps/storefront/src"));
        let d = decide_with("bun run dev", &BTreeMap::new(), Some(&p));
        assert!(
            reason(&d).contains("`devrun up storefront`"),
            "{}",
            reason(&d)
        );
        assert!(!reason(&d).contains("devrun up web"), "{}", reason(&d));
    }

    #[test]
    fn several_apps_sharing_a_launch_are_all_named_when_no_hint_resolves() {
        let p = three_apps_sharing_one_launch(None);
        let d = decide_with("bun run dev", &BTreeMap::new(), Some(&p));
        for name in ["admin", "storefront", "web"] {
            assert!(
                reason(&d).contains(&format!("`devrun up {name}`")),
                "{}",
                reason(&d)
            );
        }
    }

    #[test]
    fn a_message_quotes_the_command_as_typed_not_as_normalized() {
        let p = three_apps_sharing_one_launch(Some("apps/web"));
        let d = decide_with("bun run dev", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).starts_with("`bun run dev` is"), "{}", reason(&d));

        let t = project(|c| {
            c.tasks.insert("lint".into(), TaskConfig {
                run: vec!["bun".into(), "run".into(), "lint".into()],
                app: Some("web".into()),
                ..Default::default()
            });
        });
        let d = decide_with("bun run lint", &BTreeMap::new(), Some(&t));
        assert!(
            reason(&d).starts_with("`bun run lint` is"),
            "{}",
            reason(&d)
        );

        let c = project(|_| {});
        let d = decide_with("uvx uvicorn app:app", &BTreeMap::new(), Some(&c));
        assert!(
            reason(&d).starts_with("`uvx uvicorn app:app` starts"),
            "{}",
            reason(&d)
        );
    }

    #[test]
    fn a_quoted_mention_is_not_a_launch() {
        let p = project(|_| {});
        let cmd = r#"git commit -m "fix: crash under uvicorn; retry""#;
        assert!(!denies(&decide_with(cmd, &BTreeMap::new(), Some(&p))));
    }

    #[test]
    fn a_heredoc_body_is_not_a_launch() {
        let p = project(|_| {});
        let cmd = "cat > notes.md <<EOF\nuvicorn app:app\nEOF";
        assert!(!denies(&decide_with(cmd, &BTreeMap::new(), Some(&p))));
    }

    #[test]
    fn a_later_segment_is_reached() {
        let r = rules(&["node"], "This workspace is bun-only.");
        let d = decide_with("git status && node server.js", &r, None);
        assert_eq!(reason(&d), "This workspace is bun-only.");
    }

    #[test]
    fn an_empty_command_is_allowed() {
        let r = rules(&["node"], "unreachable");
        assert!(!denies(&decide_with("", &r, None)));
        assert!(!denies(&decide_with("   ", &r, None)));
        assert!(!denies(&decide_with("FOO=bar", &r, None)));
    }
}
