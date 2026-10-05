//! Filling scopes, caller visibility and the fence around store-wide scans.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use devkit_config::{RoleConfig, TodoConfig, todo::Template};

use crate::{
    Filter, Holder, NodeMatch, Todo,
    node::{Place, SessionRef, sanitize},
};

/// The caller's sanitized identity, collected once for a command or hook.
#[derive(Debug, Clone)]
pub struct Facts {
    pub values: BTreeMap<String, String>,
    pub subagent: bool,
}

impl Facts {
    pub fn new(place: &Place, session: Option<&SessionRef>, holder: &Holder) -> Self {
        let mut values = BTreeMap::new();
        match place {
            Place::Global => {}
            Place::Project { repo } => {
                values.insert("repo".into(), sanitize(repo));
            }
            Place::Workspace { repo, branch } => {
                values.insert("repo".into(), sanitize(repo));
                values.insert("branch".into(), sanitize(branch));
            }
        }
        if let Some(session) = session {
            values.insert("harness".into(), session.harness.prefix().into());
            values.insert("session".into(), sanitize(&session.id));
        }
        let subagent = !holder.is_human() && *holder != holder.session();
        if subagent && let Some((_, agent)) = holder.split_once('/') {
            values.insert("agent".into(), sanitize(agent));
        }
        values.retain(|_, value| !value.is_empty());
        Self { values, subagent }
    }
}

/// A validated scope tree and its roles, shared by CLI and hook routing.
pub struct Layout {
    config: TodoConfig,
    templates: BTreeMap<String, Template>,
}

impl Layout {
    pub fn new(config: &TodoConfig) -> Result<Self> {
        config.validate()?;
        let templates = config
            .scopes
            .iter()
            .map(|(name, scope)| Ok((name.clone(), Template::parse(&scope.node)?)))
            .collect::<Result<_>>()?;
        Ok(Self {
            config: config.clone(),
            templates,
        })
    }

    pub fn roles(&self) -> &BTreeMap<String, RoleConfig> {
        &self.config.roles
    }

    pub fn role(&self, name: &str) -> Option<&RoleConfig> {
        self.config.roles.get(name)
    }

    fn filled_scope<'a>(&'a self, scope: &'a str, facts: &Facts) -> Result<(&'a str, String)> {
        let mut here = scope;
        loop {
            let Some(config) = self.config.scopes.get(here) else {
                bail!(
                    "unknown todo scope {here:?}; valid scopes: {}",
                    self.config
                        .scopes
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            };
            if let Some(node) = self.templates[here].fill(&facts.values) {
                return Ok((here, node));
            }
            let Some(parent) = &config.parent else {
                bail!("todo root {here:?} cannot fill");
            };
            here = parent;
        }
    }

    pub fn fill(&self, scope: &str, facts: &Facts) -> Result<String> {
        self.filled_scope(scope, facts).map(|(_, node)| node)
    }

    pub fn visible(&self, facts: &Facts, role: &str) -> Result<Filter> {
        let Some(role) = self.role(role) else {
            bail!("unknown todo role {role:?}");
        };
        let (scope, own) = self.filled_scope(&role.scope, facts)?;
        let mut nodes = vec![NodeMatch::Exact(own.clone())];
        let mut parent = self.config.scopes[scope].parent.as_deref();
        while let Some(scope) = parent {
            let node = self.fill(scope, facts)?;
            if !nodes.contains(&NodeMatch::Exact(node.clone())) {
                nodes.push(NodeMatch::Exact(node));
            }
            parent = self.config.scopes[scope].parent.as_deref();
        }
        if let Some(session) = facts.values.get("session") {
            nodes.push(NodeMatch::SessionDescendants {
                node: own,
                session: session.clone(),
                templates: self.templates.values().cloned().collect(),
            });
        }
        Ok(Filter { nodes })
    }

    pub fn hold_pending(&self, role: &str, facts: &Facts) -> Result<bool> {
        let Some(role) = self.role(role) else {
            bail!("unknown todo role {role:?}");
        };
        match role.hold_pending {
            Some(explicit) => Ok(explicit),
            None => {
                let (scope, _) = self.filled_scope(&role.scope, facts)?;
                Ok(
                    self.templates[scope].contains(if facts.subagent {
                        "agent"
                    } else {
                        "session"
                    }),
                )
            }
        }
    }

    pub fn is_devkit(&self, project: &str, anchored_repos: &BTreeSet<String>) -> bool {
        self.templates.values().any(|template| {
            template.read(project).is_some_and(|values| {
                template.anchored()
                    || values
                        .get("repo")
                        .is_some_and(|repo| anchored_repos.contains(repo))
            })
        })
    }

    pub fn fence(&self, todos: Vec<Todo>) -> Vec<Todo> {
        let mut anchored_repos = BTreeSet::new();
        for todo in &todos {
            for template in self
                .templates
                .values()
                .filter(|template| template.anchored())
            {
                if let Some(repo) = template
                    .read(todo.node())
                    .and_then(|mut values| values.remove("repo"))
                {
                    anchored_repos.insert(repo);
                }
            }
        }
        todos
            .into_iter()
            .filter(|todo| self.is_devkit(todo.node(), &anchored_repos))
            .collect()
    }
}
