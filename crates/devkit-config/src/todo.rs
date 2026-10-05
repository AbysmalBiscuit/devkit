//! Configurable todo node templates and workflow roles.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A named todo node template in the scope tree.
///
/// ```
/// # use devkit_config::Config;
/// # let custom = Config::parse(r#"
/// [todo.scopes.review]
/// node = "{repo}.{branch}.review-{session}"
/// parent = "workspace"
/// # "#).unwrap();
/// # assert_eq!(custom.todo.scopes["review"].parent.as_deref(), Some("workspace"));
/// # let config = Config::parse("").unwrap();
/// # assert_eq!(config.todo.scopes["global"].node, "global");
/// # assert_eq!(config.todo.scopes["repo"].node, "{repo}");
/// # assert_eq!(config.todo.scopes["workspace"].node, "{repo}.{branch}");
/// # assert_eq!(config.todo.scopes["session"].node, "{repo}.{branch}.{harness}-{session}");
/// # assert_eq!(config.todo.scopes["agent"].node, "{repo}.{branch}.{harness}-{session}.{agent}");
/// # for template in [
/// #     "{unknown}",
/// #     "{repo}.{repo}",
/// #     "{repo}.{branch}-{session}",
/// #     "{branch}.{session}",
/// #     "{repo}..jobs",
/// #     "{repo}.{branch",
/// #     "{repo}.branch}",
/// # ] {
/// #     let text = format!("[todo.scopes.custom]\nnode = {template:?}\nparent = 'workspace'");
/// #     assert!(Config::parse(&text).is_err(), "{template}");
/// # }
/// # for text in [
/// #     "[todo.scopes.workspace]\nnode = '{repo}.{branch}'\nparent = 'missing'",
/// #     "[todo.scopes.repo]\nnode = '{repo}'\nparent = 'workspace'",
/// #     "[todo.scopes.custom]\nnode = 'extra'",
/// #     "[todo.scopes.global]\nnode = '{repo}'",
/// # ] {
/// #     assert!(Config::parse(text).is_err(), "{text}");
/// # }
/// ```
#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeConfig {
    /// Segments joined by dots. Placeholders are `repo`, `branch`,
    /// `harness`, `session` and `agent`.
    pub node: String,
    /// The scope to use when a caller lacks a template value. The one root
    /// has no parent and its node contains no placeholders.
    #[serde(default)]
    pub parent: Option<String>,
}

/// A caller's workflow role, independent of its authority over claims.
///
/// ```
/// # use devkit_config::Config;
/// # let config = Config::parse(r#"
/// [todo.roles.manager]
/// scope = "workspace"
/// [todo.roles.implementer]
/// scope = "agent"
/// parent = "manager"
/// agent_types = ["implementer"]
/// hold_pending = true
/// # "#).unwrap();
/// # assert_eq!(config.todo.roles["main"].scope, "session");
/// # assert_eq!(config.todo.roles["subagent"].scope, "session");
/// # assert_eq!(config.todo.roles["implementer"].parent.as_deref(), Some("manager"));
/// # for text in [
/// #     "[todo.roles.worker]\nscope = 'missing'",
/// #     "[todo.roles.worker]\nscope = 'agent'\nparent = 'missing'",
/// #     "[todo.roles.main]\nscope = 'session'\nparent = 'subagent'\n[todo.roles.subagent]\nscope = 'session'\nparent = 'main'",
/// #     "[todo.roles.main]\nscope = 'session'\nagent_types = ['worker']\n[todo.roles.subagent]\nscope = 'session'\nagent_types = ['worker']",
/// # ] {
/// #     assert!(Config::parse(text).is_err(), "{text}");
/// # }
/// ```
#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoleConfig {
    /// The scope this role writes to, walking up when caller facts are missing.
    pub scope: String,
    /// The role it works under. Selects suggested roles, never claim authority.
    #[serde(default)]
    pub parent: Option<String>,
    /// Subagent types assigned this role by the subagent-start hook.
    #[serde(default)]
    pub agent_types: Vec<String>,
    /// Hold a stop on pending todos at the role's node. When absent, true
    /// only when the template includes the caller's deepest identity:
    /// `agent` for a subagent and `session` for a main agent.
    #[serde(default)]
    pub hold_pending: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Value(String),
}

/// A parsed, readable todo node template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    segments: Vec<Vec<Part>>,
}

impl Template {
    pub fn parse(text: &str) -> Result<Self> {
        let mut seen = BTreeSet::new();
        let mut segments = Vec::new();
        for segment in text.split('.') {
            if segment.is_empty() {
                bail!("todo template {text:?} has an empty segment");
            }
            let mut parts = Vec::new();
            let mut rest = segment;
            let mut open = 0;
            while !rest.is_empty() {
                let end = rest.find(['{', '}']).unwrap_or(rest.len());
                if end > 0 {
                    parts.push(Part::Literal(rest[..end].into()));
                    rest = &rest[end..];
                }
                if rest.is_empty() {
                    break;
                }
                let Some(value) = rest.strip_prefix('{').and_then(|s| s.split_once('}')) else {
                    bail!("todo template {text:?} has an unmatched brace");
                };
                let (name, after) = value;
                if !matches!(name, "repo" | "branch" | "harness" | "session" | "agent") {
                    bail!("todo template {text:?} has unknown placeholder {{{name}}}");
                }
                if !seen.insert(name.to_string()) {
                    bail!("todo template {text:?} has placeholder {{{name}}} used twice");
                }
                if name != "harness" {
                    open += 1;
                    if open > 1 {
                        bail!(
                            "todo template {text:?} has more than one open placeholder in a segment"
                        );
                    }
                }
                parts.push(Part::Value(name.into()));
                rest = after;
            }
            segments.push(parts);
        }
        let template = Self { segments };
        if !template.anchored() && !template.contains("repo") {
            bail!("todo template {text:?} made only of open placeholders must include {{repo}}");
        }
        Ok(template)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.segments
            .iter()
            .flatten()
            .any(|p| matches!(p, Part::Value(n) if n == name))
    }

    pub fn anchored(&self) -> bool {
        self.segments.iter().flatten().any(|p| match p {
            Part::Literal(_) => true,
            Part::Value(name) => name == "harness",
        })
    }

    pub fn fill(&self, values: &BTreeMap<String, String>) -> Option<String> {
        self.segments
            .iter()
            .map(|parts| {
                parts
                    .iter()
                    .map(|part| match part {
                        Part::Literal(text) => Some(text.clone()),
                        Part::Value(name) => values
                            .get(name)
                            .filter(|value| {
                                !value.is_empty()
                                    && !value.contains(['.', '/', '\\'])
                                    && (name != "harness"
                                        || matches!(value.as_str(), "claude" | "codex"))
                            })
                            .cloned(),
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(|pieces| pieces.concat())
            })
            .collect::<Option<Vec<_>>>()
            .map(|segments| segments.join("."))
    }

    pub fn read(&self, project: &str) -> Option<BTreeMap<String, String>> {
        let segments: Vec<_> = project.split('.').collect();
        if segments.len() != self.segments.len() {
            return None;
        }
        let mut values = BTreeMap::new();
        for (parts, node) in self.segments.iter().zip(segments) {
            let segment_values = ["claude", "codex"].into_iter().find_map(|harness| {
                let mut prefix = String::new();
                let mut suffix = String::new();
                let mut open = None;
                let mut has_harness = false;
                for part in parts {
                    let target = if open.is_some() {
                        &mut suffix
                    } else {
                        &mut prefix
                    };
                    match part {
                        Part::Literal(text) => target.push_str(text),
                        Part::Value(name) if name == "harness" => {
                            has_harness = true;
                            target.push_str(harness);
                        }
                        Part::Value(name) => open = Some(name),
                    }
                }
                let mut found = BTreeMap::new();
                match open {
                    Some(name) => {
                        let value = node.strip_prefix(&prefix)?.strip_suffix(&suffix)?;
                        if value.is_empty() || value.contains(['/', '\\']) {
                            return None;
                        }
                        found.insert(name.clone(), value.into());
                    }
                    None if node != prefix => return None,
                    None => {}
                }
                if has_harness {
                    found.insert("harness".into(), harness.into());
                }
                Some(found)
            })?;
            values.extend(segment_values);
        }
        Some(values)
    }
}

pub(crate) fn validate(config: &crate::TodoConfig) -> Result<()> {
    let mut roots = Vec::new();
    for (name, scope) in &config.scopes {
        let template =
            Template::parse(&scope.node).with_context(|| format!("[todo.scopes.{name}] node"))?;
        match &scope.parent {
            Some(parent) if !config.scopes.contains_key(parent) => {
                bail!("[todo.scopes.{name}] missing parent {parent:?}")
            }
            Some(_) => {}
            None => {
                if template
                    .segments
                    .iter()
                    .flatten()
                    .any(|p| matches!(p, Part::Value(_)))
                {
                    bail!("[todo.scopes.{name}] root must have no placeholders");
                }
                roots.push(name);
            }
        }
        let mut visited = BTreeSet::new();
        let mut here = Some(name.as_str());
        while let Some(current) = here {
            if !visited.insert(current) {
                bail!("[todo.scopes.{name}] parent cycle through {current:?}");
            }
            here = config.scopes.get(current).and_then(|s| s.parent.as_deref());
        }
    }
    if roots.len() != 1 {
        bail!("[todo.scopes] must form one tree with one root");
    }
    let mut types = BTreeMap::new();
    for (name, role) in &config.roles {
        if !config.scopes.contains_key(&role.scope) {
            bail!("[todo.roles.{name}] missing scope {:?}", role.scope);
        }
        if let Some(parent) = &role.parent
            && !config.roles.contains_key(parent)
        {
            bail!("[todo.roles.{name}] missing parent {parent:?}");
        }
        let mut visited = BTreeSet::new();
        let mut here = Some(name.as_str());
        while let Some(current) = here {
            if !visited.insert(current) {
                bail!("[todo.roles.{name}] parent cycle through {current:?}");
            }
            here = config.roles.get(current).and_then(|r| r.parent.as_deref());
        }
        for agent_type in &role.agent_types {
            if let Some(other) = types.insert(agent_type, name)
                && other != name
            {
                bail!("agent_type {agent_type:?} is claimed by roles {other:?} and {name:?}");
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Input {
    #[serde(deserialize_with = "scopes")]
    scopes: BTreeMap<String, ScopeConfig>,
    #[serde(deserialize_with = "roles")]
    roles: BTreeMap<String, RoleConfig>,
    backend: crate::TodoBackend,
    project: Option<serde::de::IgnoredAny>,
    hold_stop: bool,
    taskchampion: crate::TaskchampionConfig,
    postgres: crate::PostgresConfig,
}

impl Default for Input {
    fn default() -> Self {
        let config = crate::TodoConfig::default();
        Self {
            scopes: config.scopes,
            roles: config.roles,
            backend: config.backend,
            project: None,
            hold_stop: config.hold_stop,
            taskchampion: config.taskchampion,
            postgres: config.postgres,
        }
    }
}

impl TryFrom<Input> for crate::TodoConfig {
    type Error = String;

    fn try_from(input: Input) -> Result<Self, Self::Error> {
        if input.project.is_some() {
            return Err(
                "[todo] project was removed; configure node templates in [todo.scopes] instead"
                    .into(),
            );
        }
        let config = Self {
            scopes: input.scopes,
            roles: input.roles,
            backend: input.backend,
            hold_stop: input.hold_stop,
            taskchampion: input.taskchampion,
            postgres: input.postgres,
        };
        validate(&config).map_err(|error| format!("{error:#}"))?;
        Ok(config)
    }
}

pub(crate) fn default_scopes() -> BTreeMap<String, ScopeConfig> {
    [
        ("global", "global", None),
        ("repo", "{repo}", Some("global")),
        ("workspace", "{repo}.{branch}", Some("repo")),
        (
            "session",
            "{repo}.{branch}.{harness}-{session}",
            Some("workspace"),
        ),
        (
            "agent",
            "{repo}.{branch}.{harness}-{session}.{agent}",
            Some("session"),
        ),
    ]
    .into_iter()
    .map(|(name, node, parent)| {
        (name.into(), ScopeConfig {
            node: node.into(),
            parent: parent.map(str::to_string),
        })
    })
    .collect()
}

pub(crate) fn default_roles() -> BTreeMap<String, RoleConfig> {
    ["main", "subagent"]
        .into_iter()
        .map(|name| {
            (name.into(), RoleConfig {
                scope: "session".into(),
                parent: None,
                agent_types: Vec::new(),
                hold_pending: None,
            })
        })
        .collect()
}

pub(crate) fn scopes<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, ScopeConfig>, D::Error> {
    let mut scopes = default_scopes();
    scopes.extend(BTreeMap::<String, ScopeConfig>::deserialize(d)?);
    Ok(scopes)
}

pub(crate) fn roles<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, RoleConfig>, D::Error> {
    let mut roles = default_roles();
    roles.extend(BTreeMap::<String, RoleConfig>::deserialize(d)?);
    Ok(roles)
}
