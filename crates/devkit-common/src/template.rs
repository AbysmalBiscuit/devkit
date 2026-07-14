use anyhow::{Context, Result};
use minijinja::value::{Object, Value};
use minijinja::{Environment, UndefinedBehavior};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Render a compiled template against a prebuilt minijinja root value, with the
/// same strict-undefined and trailing-newline settings as [`render`].
fn render_value(template: &str, root: Value) -> Result<String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Preserve a template's trailing newline so verbatim file content (e.g. an
    // `.env.local` ending in `\n`) round-trips unchanged.
    env.set_keep_trailing_newline(true);
    env.add_template("t", template)
        .context("compiling template")?;
    let tmpl = env.get_template("t").expect("template just added");
    tmpl.render(root).context("rendering template")
}

/// Render a minijinja template with strict undefined handling. `variables`
/// supply constants merged underneath `ctx` — a context field of the same name
/// wins. `ctx` must serialize to a JSON object.
pub fn render(
    template: &str,
    ctx: &impl Serialize,
    variables: &BTreeMap<String, String>,
) -> Result<String> {
    let value = merged_context(ctx, variables)?;
    render_value(template, Value::from_serialize(&value))
}

/// Port references a set of launch/task templates makes: the app names looked
/// up via `ports[...]` and whether `port` (the app's own port) is used.
#[derive(Debug, Default, PartialEq)]
pub struct PortRefs {
    pub apps: BTreeSet<String>,
    pub own_port: bool,
}

/// Records every key looked up on `ports` during a discovery render, returning
/// a placeholder value so rendering proceeds.
#[derive(Debug, Default)]
struct PortsRecorder {
    apps: Mutex<BTreeSet<String>>,
}

impl Object for PortsRecorder {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let k = key.as_str()?;
        self.apps.lock().unwrap().insert(k.to_string());
        Some(Value::from(0u16))
    }
}

/// Discovery-render root: serves `port`/`ports` from recorders and everything
/// else from the user variables, so an unknown name still errors (strict).
#[derive(Debug)]
struct DiscoveryCtx {
    vars: BTreeMap<String, String>,
    ports: Arc<PortsRecorder>,
    own_port: Arc<AtomicBool>,
}

impl Object for DiscoveryCtx {
    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        match key.as_str()? {
            "port" => {
                self.own_port.store(true, Ordering::SeqCst);
                Some(Value::from(0u16))
            }
            "ports" => Some(Value::from_dyn_object(self.ports.clone())),
            k => self.vars.get(k).map(|v| Value::from(v.clone())),
        }
    }
}

/// Collect the port references across `templates` by rendering each against a
/// recording context. Never touches the port registry; `ports[...]` lookups
/// return a placeholder. A reference inside a branch not taken under
/// placeholder values goes unrecorded — keep port refs out of conditionals.
pub fn referenced_ports(
    templates: &[&str],
    variables: &BTreeMap<String, String>,
) -> Result<PortRefs> {
    let ports = Arc::new(PortsRecorder::default());
    let own_port = Arc::new(AtomicBool::new(false));
    for t in templates {
        let ctx = Arc::new(DiscoveryCtx {
            vars: variables.clone(),
            ports: ports.clone(),
            own_port: own_port.clone(),
        });
        render_value(t, Value::from_dyn_object(ctx))
            .with_context(|| format!("scanning template `{t}` for port references"))?;
    }
    Ok(PortRefs {
        apps: std::mem::take(&mut ports.apps.lock().unwrap()),
        own_port: own_port.load(Ordering::SeqCst),
    })
}

#[derive(Serialize)]
struct LaunchCtx<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    ports: &'a BTreeMap<String, u16>,
}

/// Render one launch/static_env/task string against the port context. `port`
/// is the app's own allocated port (absent for a task without an `app`);
/// `ports` maps app name → this worktree's allocated port. Errors if the
/// output still contains the retired `{port}` placeholder, which minijinja
/// would otherwise pass through as literal text.
pub fn render_launch(
    template: &str,
    port: Option<u16>,
    ports: &BTreeMap<String, u16>,
    variables: &BTreeMap<String, String>,
) -> Result<String> {
    let out = render(template, &LaunchCtx { port, ports }, variables)?;
    anyhow::ensure!(
        !out.contains("{port}"),
        "`{{port}}` is retired; use `{{{{ port }}}}` (minijinja) in launch/static_env/task templates"
    );
    Ok(out)
}

fn merged_context(
    ctx: &impl Serialize,
    variables: &BTreeMap<String, String>,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(ctx).context("serializing template context")?;
    if let Some(obj) = value.as_object_mut() {
        for (k, v) in variables {
            obj.entry(k.clone())
                .or_insert_with(|| serde_json::Value::String(v.clone()));
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn substitutes_named_fields() {
        let out = render("{{ a }}-{{ b }}", &json!({"a": "x", "b": "y"}), &novars()).unwrap();
        assert_eq!(out, "x-y");
    }

    #[test]
    fn supports_if_and_for() {
        let tmpl = "{% for a in apps %}{{ a }},{% endfor %}{% if flag %}!{% endif %}";
        let out = render(tmpl, &json!({"apps": ["w", "i"], "flag": true}), &novars()).unwrap();
        assert_eq!(out, "w,i,!");
    }

    #[test]
    fn strict_undefined_is_an_error() {
        assert!(render("{{ missing }}", &json!({}), &novars()).is_err());
    }

    #[test]
    fn variables_fill_unset_fields() {
        let mut vars = BTreeMap::new();
        vars.insert("team".to_string(), "platform".to_string());
        let out = render("{{ team }}", &json!({}), &vars).unwrap();
        assert_eq!(out, "platform");
    }

    #[test]
    fn context_wins_over_variable() {
        let mut vars = BTreeMap::new();
        vars.insert("slug".to_string(), "from-const".to_string());
        let out = render("{{ slug }}", &json!({"slug": "from-ctx"}), &vars).unwrap();
        assert_eq!(out, "from-ctx");
    }

    fn ports(pairs: &[(&str, u16)]) -> BTreeMap<String, u16> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn render_launch_substitutes_port_and_ports() {
        let out = render_launch(
            "http://localhost:{{ ports['api-prod'] }} own={{ port }}",
            Some(9200),
            &ports(&[("api-prod", 9101)]),
            &novars(),
        )
        .unwrap();
        assert_eq!(out, "http://localhost:9101 own=9200");
    }

    #[test]
    fn render_launch_unknown_ports_key_is_an_error() {
        assert!(render_launch("{{ ports['nope'] }}", None, &ports(&[]), &novars()).is_err());
    }

    #[test]
    fn render_launch_port_without_app_is_an_error() {
        assert!(render_launch("{{ port }}", None, &ports(&[]), &novars()).is_err());
    }

    #[test]
    fn render_launch_rejects_leftover_brace_port() {
        let err = render_launch("--port {port}", Some(9200), &ports(&[]), &novars())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("{{ port }}"),
            "error must show the migration hint: {err}"
        );
    }

    #[test]
    fn render_launch_uses_variables() {
        let mut vars = novars();
        vars.insert("cfg".into(), "dev_local".into());
        let out = render_launch("-c {{ cfg }}", None, &ports(&[]), &vars).unwrap();
        assert_eq!(out, "-c dev_local");
    }

    #[test]
    fn referenced_ports_collects_apps_and_own_port() {
        let refs = referenced_ports(
            &[
                "http://localhost:{{ ports['api-prod'] }}",
                "NITRO_PORT={{ port }}",
                "{{ ports[\"web\"] }}",
                "no refs here",
            ],
            &novars(),
        )
        .unwrap();
        assert_eq!(
            refs.apps,
            ["api-prod", "web"].iter().map(|s| s.to_string()).collect()
        );
        assert!(refs.own_port);
    }

    #[test]
    fn referenced_ports_empty_when_no_refs() {
        let refs = referenced_ports(&["plain", "-c dev"], &novars()).unwrap();
        assert!(refs.apps.is_empty());
        assert!(!refs.own_port);
    }

    #[test]
    fn referenced_ports_unknown_variable_is_an_error() {
        assert!(referenced_ports(&["{{ typo }}"], &novars()).is_err());
    }
}
