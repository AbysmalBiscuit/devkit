use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::ServerCtx;

/// One registered action. `schema` returns its argument JSON Schema; `handler`
/// validates + executes it. The registry is the single source of truth for both
/// `devkit_describe` and `devkit_call`.
pub struct Action {
    pub name: &'static str,
    pub summary: &'static str,
    pub schema: fn() -> Value,
    pub handler: fn(&ServerCtx, Value) -> Result<Value>,
}

/// All registered actions. Adding a binary's actions is one `extend` line.
pub fn actions() -> Vec<Action> {
    let mut v = Vec::new();
    v.extend(crate::ports::actions());
    v.extend(crate::locks::actions());
    v.extend(crate::devrun::actions());
    v.extend(crate::issue::actions());
    v
}

pub fn find(name: &str) -> Option<Action> {
    actions().into_iter().find(|a| a.name == name)
}

/// `devkit_describe`: no `action` -> list `{action, summary}`; with `action` ->
/// that action's argument schema.
pub fn describe(args: Value) -> Result<Value> {
    match args.get("action").and_then(|v| v.as_str()) {
        None => {
            let list: Vec<Value> = actions()
                .iter()
                .map(|a| serde_json::json!({ "action": a.name, "summary": a.summary }))
                .collect();
            Ok(Value::Array(list))
        }
        Some(name) => {
            let a = find(name).ok_or_else(|| anyhow!("unknown action: {name}"))?;
            Ok((a.schema)())
        }
    }
}

/// `devkit_call`: look up the action, hand it its `args` object.
pub fn call(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let name = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing required field: action"))?;
    let a = find(name).ok_or_else(|| anyhow!("unknown action: {name}"))?;
    let action_args = args
        .get("args")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    (a.handler)(ctx, action_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_lists_the_ports_actions() {
        let list = describe(Value::Null).unwrap();
        let names: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"ports.status"));
        assert!(names.contains(&"ports.alloc"));
        assert!(names.contains(&"ports.release"));
        assert!(names.contains(&"ports.prune"));
        assert!(names.contains(&"ports.strays"));
    }

    #[test]
    fn describe_returns_a_schema_for_each_action() {
        for a in actions() {
            let schema = describe(serde_json::json!({ "action": a.name })).unwrap();
            assert_eq!(schema["type"], "object", "{} schema", a.name);
        }
    }

    #[test]
    fn describe_unknown_action_errors() {
        assert!(describe(serde_json::json!({ "action": "nope.nope" })).is_err());
    }

    #[test]
    fn call_unknown_action_errors() {
        let ctx = ServerCtx {
            default_holder: "t".to_string(),
        };
        assert!(call(&ctx, serde_json::json!({ "action": "nope.nope" })).is_err());
    }
}
