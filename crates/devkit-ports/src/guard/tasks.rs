//! Whether redirecting a typed command to `devrun task <name>` changes anything.

use super::norm::Doppler;
use devkit_config::TaskConfig;
use std::collections::BTreeMap;

/// Whether `devrun task <name>` launches a different process than the argv the
/// agent typed.
///
/// True when the task sets `app` (a different cwd plus that app's `static_env`),
/// sets `env`, references a port the registry has to supply, or carries a
/// different doppler wrapper than the typed command. A task with none of those
/// resolves to the identical process in the same directory, and blocking it
/// buys nothing.
///
/// `steps` is deliberately not among them: `run` and `steps` are mutually
/// exclusive, so a sequence task has no `run` for a typed command to match.
/// Nor is `require_live`, which `resolve_command` already requires to be
/// referenced through `ports[...]`, so the port test has fired first.
pub fn redirect_worth_it(
    task: &TaskConfig,
    vars: &BTreeMap<String, String>,
    typed: Option<&Doppler>,
    config: Option<&Doppler>,
) -> bool {
    if let Some(forced) = task.guard {
        return forced;
    }
    task.app.is_some() || !task.env.is_empty() || typed != config || references_a_port(task, vars)
}

/// Whether the task's `run` or `env` reads a port. Rendered against a recording
/// context rather than scanned for `{{`, so `ports["web"]` and a
/// `{% if port %}` guard both count and neither touches the registry.
fn references_a_port(task: &TaskConfig, vars: &BTreeMap<String, String>) -> bool {
    let mut templates: Vec<&str> = task.run.iter().map(String::as_str).collect();
    templates.extend(task.env.values().map(String::as_str));
    devkit_common::template::referenced_ports(&templates, vars)
        .map(|r| r.own_port || !r.apps.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devkit_config::TaskConfig;
    use std::collections::BTreeMap;

    fn task(run: &[&str]) -> TaskConfig {
        TaskConfig {
            run: run.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn worth(t: &TaskConfig) -> bool {
        redirect_worth_it(t, &BTreeMap::new(), None, None)
    }

    #[test]
    fn a_bare_run_is_not_worth_redirecting() {
        assert!(!worth(&task(&["bun", "test"])));
    }

    #[test]
    fn an_app_scoped_task_is_worth_redirecting() {
        let mut t = task(&["bun", "test"]);
        t.app = Some("web".into());
        assert!(worth(&t));
    }

    #[test]
    fn a_task_with_env_is_worth_redirecting() {
        let mut t = task(&["bun", "test"]);
        t.env.insert("CI".into(), "1".into());
        assert!(worth(&t));
    }

    #[test]
    fn a_port_template_is_worth_redirecting() {
        assert!(worth(&task(&["curl", "http://localhost:{{ port }}"])));
        assert!(worth(&task(&["curl", "{{ ports['api'] }}"])));
    }

    #[test]
    fn a_differing_doppler_config_is_worth_redirecting() {
        let typed = Doppler {
            config: Some("prd".into()),
            project: None,
        };
        let cfg = Doppler {
            config: Some("dev".into()),
            project: None,
        };
        assert!(redirect_worth_it(
            &task(&["bun", "test"]),
            &BTreeMap::new(),
            Some(&typed),
            Some(&cfg)
        ));
    }

    #[test]
    fn an_identical_doppler_config_is_not() {
        let d = Doppler {
            config: Some("dev".into()),
            project: None,
        };
        assert!(!redirect_worth_it(
            &task(&["bun", "test"]),
            &BTreeMap::new(),
            Some(&d),
            Some(&d)
        ));
    }

    #[test]
    fn a_missing_wrapper_on_one_side_is_a_difference() {
        let d = Doppler {
            config: Some("dev".into()),
            project: None,
        };
        assert!(redirect_worth_it(
            &task(&["bun", "test"]),
            &BTreeMap::new(),
            None,
            Some(&d)
        ));
    }

    #[test]
    fn the_guard_field_overrides_both_ways() {
        let mut bare = task(&["bun", "test"]);
        bare.guard = Some(true);
        assert!(worth(&bare));

        let mut scoped = task(&["bun", "test"]);
        scoped.app = Some("web".into());
        scoped.guard = Some(false);
        assert!(!worth(&scoped));
    }
}
