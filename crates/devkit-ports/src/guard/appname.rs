//! Work out which app a dev-server command belongs to.
//!
//! The catalog knows a command starts a server without knowing whose it is, and
//! several apps can share one `launch`. Both problems resolve the same way.

use crate::apps::App;
use devkit_config::AppMatch;

/// Flags whose value names a workspace member.
const DIR_FLAGS: [&str; 5] = ["--filter", "-F", "--dir", "-C", "--cwd"];

/// The best guess at which app a segment refers to, before it is matched
/// against the catalog: a workspace path in the command, else a directory
/// flag's value, else the hook's cwd relative to the project root.
pub fn hint(argv: &[String], cwd_rel: Option<&str>) -> Option<String> {
    if let Some(name) = argv.iter().find_map(|w| workspace_member(w)) {
        return Some(name);
    }
    let mut it = argv.iter();
    while let Some(w) = it.next() {
        if let Some((flag, inline)) = w.split_once('=')
            && DIR_FLAGS.contains(&flag)
        {
            return Some(inline.to_string());
        }
        if DIR_FLAGS.contains(&w.as_str())
            && let Some(v) = it.next()
        {
            return Some(v.clone());
        }
    }
    cwd_rel.map(str::to_string)
}

/// The member name in an `apps/<name>` or `packages/<name>` path.
fn workspace_member(word: &str) -> Option<String> {
    let norm = word.replace('\\', "/");
    let mut parts = norm.split('/').peekable();
    while let Some(p) = parts.next() {
        if (p == "apps" || p == "packages")
            && let Some(name) = parts.peek()
            && !name.is_empty()
        {
            return Some((*name).to_string());
        }
    }
    None
}

/// Which of `candidates` the hint names.
///
/// A single candidate needs no hint: the caller already narrowed to it, and
/// asking a fuzzy matcher to confirm a set of one only adds a way to fail.
/// Otherwise an exact name or path wins, then a hint that is a path *under* an
/// app's path (a cwd of `apps/web/src` names `web`), then frizbee rescues a
/// near-miss such as `lab-tools` against an app declared `lab_tools`.
///
/// The fuzzy rung is the only guesswork here, and `cfg` is what a project tunes
/// it with. `frizbee::Config::default()` allows zero typos, which filters the
/// `lab-tools` case outright, so `cfg.max_typos` is always passed rather than
/// left to the library.
pub fn resolve(hint: Option<&str>, candidates: &[&App], cfg: &AppMatch) -> Option<String> {
    match candidates {
        [] => return None,
        [only] => return Some(only.name.clone()),
        _ => {}
    }
    let needle = hint?.trim_matches('/');

    for app in candidates {
        if app.name == needle || app.path.trim_matches('/') == needle {
            return Some(app.name.clone());
        }
    }
    for app in candidates {
        let path = app.path.trim_matches('/');
        if !path.is_empty() && needle.starts_with(&format!("{path}/")) {
            return Some(app.name.clone());
        }
    }

    if !cfg.fuzzy {
        return None;
    }
    let haystack: Vec<&str> = candidates.iter().map(|a| a.name.as_str()).collect();
    let config = frizbee::Config::default().max_typos(Some(cfg.max_typos));
    let mut matcher = frizbee::Matcher::new(needle, &config);
    // `match_list` already returns descending score.
    matcher
        .match_list(&haystack)
        .first()
        .filter(|m| m.score >= cfg.min_score)
        .map(|m| haystack[m.index as usize].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::App;
    use devkit_config::AppMatch;

    fn app(name: &str, path: &str) -> App {
        App {
            name: name.into(),
            base_port: 3000,
            path: path.into(),
            launch: Vec::new(),
            url: None,
            url_env: None,
            provides_url: false,
            static_env: Default::default(),
            prep_files: Vec::new(),
            setup: Vec::new(),
        }
    }

    fn words(cmd: &str) -> Vec<String> {
        cmd.split_whitespace().map(str::to_string).collect()
    }

    /// Two apps, so `resolve` cannot short-circuit on a lone candidate.
    fn pair() -> [App; 2] {
        [app("lab_tools", "apps/lab_tools"), app("web", "apps/web")]
    }

    #[test]
    fn a_workspace_path_in_the_command_is_the_hint() {
        assert_eq!(
            hint(&words("vite --root apps/web"), None).as_deref(),
            Some("web")
        );
        assert_eq!(
            hint(&words("vite packages/ui/src"), None).as_deref(),
            Some("ui")
        );
    }

    #[test]
    fn a_filter_flag_is_the_hint_when_no_path_is_present() {
        assert_eq!(
            hint(&words("bun run --filter admin dev"), None).as_deref(),
            Some("admin")
        );
        assert_eq!(
            hint(&words("vite -C tools"), None).as_deref(),
            Some("tools")
        );
    }

    #[test]
    fn the_cwd_is_the_last_resort() {
        assert_eq!(
            hint(&words("vite"), Some("apps/web")).as_deref(),
            Some("apps/web")
        );
        assert_eq!(hint(&words("vite"), None), None);
    }

    #[test]
    fn an_exact_name_wins() {
        let apps = [app("web", "apps/web"), app("website", "apps/website")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(
            resolve(Some("web"), &refs, &AppMatch::default()).as_deref(),
            Some("web")
        );
    }

    #[test]
    fn a_path_hint_resolves_to_the_owning_app() {
        let apps = [app("web", "apps/web"), app("admin", "apps/admin")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(
            resolve(Some("apps/web"), &refs, &AppMatch::default()).as_deref(),
            Some("web")
        );
    }

    #[test]
    fn a_cwd_inside_an_app_resolves_to_it() {
        let apps = [app("web", "apps/web"), app("admin", "apps/admin")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(
            resolve(Some("apps/web/src/routes"), &refs, &AppMatch::default()).as_deref(),
            Some("web")
        );
    }

    #[test]
    fn a_single_candidate_is_named_without_a_hint() {
        let apps = [app("web", "apps/web")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(
            resolve(None, &refs, &AppMatch::default()).as_deref(),
            Some("web")
        );
    }

    #[test]
    fn a_near_miss_is_rescued_by_fuzzy_matching() {
        let apps = pair();
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(
            resolve(Some("lab-tools"), &refs, &AppMatch::default()).as_deref(),
            Some("lab_tools")
        );
    }

    #[test]
    fn an_unrelated_hint_names_no_app() {
        let apps = [app("web", "apps/web"), app("admin", "apps/admin")];
        let refs: Vec<&App> = apps.iter().collect();
        assert_eq!(resolve(Some("zzzzzzzz"), &refs, &AppMatch::default()), None);
        assert_eq!(resolve(None, &refs, &AppMatch::default()), None);
    }

    #[test]
    fn fuzzy_false_stops_after_exact_and_path_matching() {
        let apps = pair();
        let refs: Vec<&App> = apps.iter().collect();
        let strict = AppMatch {
            fuzzy: false,
            ..Default::default()
        };
        assert_eq!(resolve(Some("lab-tools"), &refs, &strict), None);
        assert_eq!(
            resolve(Some("apps/web"), &refs, &strict).as_deref(),
            Some("web")
        );
    }

    #[test]
    fn a_raised_min_score_rejects_what_the_default_accepts() {
        let apps = pair();
        let refs: Vec<&App> = apps.iter().collect();
        let picky = AppMatch {
            min_score: u16::MAX,
            ..Default::default()
        };
        assert_eq!(resolve(Some("lab-tools"), &refs, &picky), None);
    }

    /// Pins the reason devkit does not inherit `frizbee::Config::default()`:
    /// a substitution is one typo, and a zero budget filters it.
    #[test]
    fn a_zero_typo_budget_filters_the_case_the_default_rescues() {
        let apps = pair();
        let refs: Vec<&App> = apps.iter().collect();
        let zero = AppMatch {
            max_typos: 0,
            ..Default::default()
        };
        assert_eq!(resolve(Some("lab-tools"), &refs, &zero), None);
    }
}
