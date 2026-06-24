use anyhow::{Context, Result};
use minijinja::{Environment, UndefinedBehavior};
use serde::Serialize;
use std::collections::BTreeMap;

/// Render a minijinja template with strict undefined handling. `variables`
/// supply constants merged underneath `ctx` — a context field of the same name
/// wins. `ctx` must serialize to a JSON object.
pub fn render(
    template: &str,
    ctx: &impl Serialize,
    variables: &BTreeMap<String, String>,
) -> Result<String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Preserve a template's trailing newline so verbatim file content (e.g. an
    // `.env.local` ending in `\n`) round-trips unchanged.
    env.set_keep_trailing_newline(true);
    env.add_template("t", template)
        .context("compiling template")?;
    let tmpl = env.get_template("t").expect("template just added");
    let value = merged_context(ctx, variables)?;
    tmpl.render(value).context("rendering template")
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
}
