//! Decide whether a shell command has a devkit equivalent worth redirecting to.

pub mod appname;
pub mod catalog;
pub mod lex;
pub mod norm;
pub mod sig;
pub mod tasks;

use crate::apps::App;
use devkit_config::{AppMatch, CommandRule, Config};
use norm::{Normalized, basename};
use std::collections::{BTreeMap, HashMap};

/// The devkit commands a user types directly. Never gate them: the guard's
/// whole purpose is to route work to them.
///
/// `devkit` plus every entry of `SHIMS` in `src/bin/devkit/shim.rs`. That list
/// lives in the binary and this crate cannot see it, so a shim added there has
/// to be added here by hand.
const SHIMS: [&str; 7] = [
    "devkit",
    "devrun",
    "lockm",
    "portm",
    "docm",
    "issue",
    "devkit-mcp",
];

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

/// Decide over already-loaded inputs. The IO-free half of the guard, so every
/// matching rule is unit-testable without a project on disk.
///
/// Segments are evaluated in order and the first denial decides the whole
/// command.
pub fn decide_with(
    command: &str,
    rules: &BTreeMap<String, CommandRule>,
    project: Option<&Project>,
) -> Decision {
    for words in lex::segments(command) {
        let Some(n) = norm::normalize(&words) else {
            continue;
        };
        let Some(prog) = n.argv.first().map(|w| basename(w).to_string()) else {
            continue;
        };
        if SHIMS.contains(&prog.as_str()) {
            continue;
        }
        if let Some(reason) = rule_hit(&n, &prog, rules) {
            return Decision::Deny { reason };
        }
        if let Some(p) = project
            && let Some(reason) = project_hit(&words, &n, &prog, p)
        {
            return Decision::Deny { reason };
        }
    }
    Decision::Allow
}

/// Source 2: a `[harness.commands.*]` rule. An empty `programs` matches
/// nothing, which is how a child layer exempts a subtree.
fn rule_hit(n: &Normalized, prog: &str, rules: &BTreeMap<String, CommandRule>) -> Option<String> {
    rules.values().find_map(|rule| {
        let named = rule.programs.iter().any(|p| basename(p) == prog);
        let args_match = n
            .argv
            .get(1..)
            .is_some_and(|typed| typed.starts_with(&rule.args));
        (named && args_match).then(|| rule.reason.clone())
    })
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
        return Some(format!(
            "`{}` is the `{name}` task. Run `devrun task {name}` so it gets its app directory, \
             layered env and allocated ports.",
            typed.join(" ")
        ));
    }

    let hits = matching_apps(n, p);

    // The catalog outranks an app's `launch` prefix: it knows which verbs start
    // a server, and a launch signature does not. The launch match still supplies
    // the app name, so the candidate set narrows to it when there was one.
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
            let cfg = norm::normalize(&task.run)?;
            let s = sig::signature(&cfg.argv)?;
            if s.len() < min_sig || !sig::matches(&s, &n.argv) {
                return None;
            }
            tasks::redirect_worth_it(
                task,
                &p.config.templates.variables,
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
            let launch = norm::normalize(&app.launch)?;
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
    use super::*;
    use devkit_config::{AppConfig, AppMatch, CommandRule, TaskConfig};

    fn rules(programs: &[&str], reason: &str) -> BTreeMap<String, CommandRule> {
        BTreeMap::from([(
            "test-rule".to_string(),
            CommandRule {
                programs: programs.iter().map(|s| s.to_string()).collect(),
                args: Vec::new(),
                reason: reason.into(),
            },
        )])
    }

    fn project(build: impl FnOnce(&mut Config)) -> Project {
        let mut config = Config::default();
        build(&mut config);
        let catalog = config
            .apps
            .iter()
            .map(|(name, a)| {
                (
                    name.clone(),
                    App {
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
                    },
                )
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
    fn an_empty_programs_rule_matches_nothing() {
        let r = rules(&[], "unreachable");
        assert!(!denies(&decide_with("node server.js", &r, None)));
    }

    #[test]
    fn a_task_redirect_names_the_task() {
        let p = project(|c| {
            c.tasks.insert(
                "check".into(),
                TaskConfig {
                    run: vec!["bun".into(), "test".into()],
                    app: Some("web".into()),
                    ..Default::default()
                },
            );
        });
        let d = decide_with("bun test", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun task check"), "{}", reason(&d));
    }

    #[test]
    fn a_task_with_its_own_runner_prefix_still_matches() {
        let p = project(|c| {
            c.tasks.insert(
                "lint".into(),
                TaskConfig {
                    run: vec!["bun".into(), "run".into(), "lint".into()],
                    app: Some("web".into()),
                    ..Default::default()
                },
            );
        });
        let d = decide_with("bun run lint", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun task lint"), "{}", reason(&d));
    }

    #[test]
    fn a_differing_doppler_config_denies_and_a_matching_one_does_not() {
        let p = project(|c| {
            c.tasks.insert(
                "check".into(),
                TaskConfig {
                    run: ["doppler", "run", "-c", "dev", "--", "bun", "test"]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                    ..Default::default()
                },
            );
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
            c.tasks.insert(
                "check".into(),
                TaskConfig {
                    run: vec!["bun".into(), "test".into()],
                    ..Default::default()
                },
            );
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
                c.tasks.insert(
                    name.into(),
                    TaskConfig {
                        run: run.iter().map(|s| s.to_string()).collect(),
                        app: Some("web".into()),
                        ..Default::default()
                    },
                );
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
                c.apps.insert(
                    app.into(),
                    AppConfig {
                        base_port: 3000,
                        path: Some(format!("apps/{app}")),
                        ..Default::default()
                    },
                );
                c.tasks.insert(
                    task.into(),
                    TaskConfig {
                        run: vec!["bun".into(), "test".into()],
                        app: Some(app.into()),
                        ..Default::default()
                    },
                );
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
            c.apps.insert(
                "web".into(),
                AppConfig {
                    base_port: 3000,
                    launch: vec![
                        "nitro".into(),
                        "dev".into(),
                        "--port".into(),
                        "{{ port }}".into(),
                    ],
                    path: Some("apps/web".into()),
                    ..Default::default()
                },
            );
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
            c.apps.insert(
                "web".into(),
                AppConfig {
                    base_port: 3000,
                    launch: vec![
                        "nitro".into(),
                        "dev".into(),
                        "--port".into(),
                        "{{ port }}".into(),
                    ],
                    ..Default::default()
                },
            );
        });
        let d = decide_with("nitro dev", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devrun up web"), "{}", reason(&d));
    }

    #[test]
    fn the_catalog_outranks_a_launch_prefix() {
        let p = project(|c| {
            c.apps.insert(
                "web".into(),
                AppConfig {
                    base_port: 3000,
                    launch: vec!["vite".into(), "--port".into(), "{{ port }}".into()],
                    ..Default::default()
                },
            );
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
            c.tasks.insert(
                name.into(),
                TaskConfig {
                    run: run.iter().map(|s| s.to_string()).collect(),
                    app: Some("storefront".into()),
                    ..Default::default()
                },
            );
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
            c.apps.insert(
                "api".into(),
                AppConfig {
                    base_port: 4000,
                    launch: vec![
                        "hypercorn".into(),
                        "app:app".into(),
                        "--bind".into(),
                        "0.0.0.0:{{ port }}".into(),
                    ],
                    ..Default::default()
                },
            );
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
            c.apps.insert(
                "web".into(),
                AppConfig {
                    base_port: 3000,
                    launch: vec![
                        "nitro".into(),
                        "dev".into(),
                        "--port".into(),
                        "{{ port }}".into(),
                    ],
                    ..Default::default()
                },
            );
        });
        let d = decide_with("uvicorn app:app", &BTreeMap::new(), Some(&p));
        assert!(reason(&d).contains("devkit config apps"), "{}", reason(&d));
        assert!(!reason(&d).contains("devrun up web"), "{}", reason(&d));
    }

    fn three_apps_sharing_one_launch(cwd_rel: Option<&str>) -> Project {
        let mut p = project(|c| {
            for name in ["admin", "storefront", "web"] {
                c.apps.insert(
                    name.into(),
                    AppConfig {
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
                    },
                );
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
            c.tasks.insert(
                "lint".into(),
                TaskConfig {
                    run: vec!["bun".into(), "run".into(), "lint".into()],
                    app: Some("web".into()),
                    ..Default::default()
                },
            );
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
