//! Persistent caller roles and one-time suggestions, keyed by exact holder.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use devkit_common::store;
use serde::{Deserialize, Serialize};

use crate::{
    Holder,
    layout::{Facts, Layout},
};

#[derive(Default, Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    nudged: bool,
    /// Where the holder last selected its role, for its calls from outside
    /// any repository to resolve against.
    #[serde(default)]
    checkout: Option<PathBuf>,
    at: DateTime<Utc>,
}

impl Entry {
    fn new(at: DateTime<Utc>) -> Self {
        Self {
            role: None,
            agent_type: None,
            nudged: false,
            checkout: None,
            at,
        }
    }
}

impl store::Document for Doc {
    fn stamp_version(&mut self) {
        self.version = 1;
    }

    fn salvage(_: &str) -> Option<Self> {
        None
    }

    fn label() -> &'static str {
        "todo roles"
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

pub struct ResolvedRole {
    pub name: String,
    pub node: String,
    pub hold_pending: bool,
    pub warning: Option<String>,
}

struct Choice {
    name: String,
    assigned: bool,
    stale: Option<String>,
}

fn choose(
    layout: &Layout,
    holder: &Holder,
    entry: Option<&Entry>,
    agent_type: Option<&str>,
) -> Choice {
    let recorded = entry.and_then(|entry| entry.role.as_deref());
    if let Some(name) = recorded.filter(|name| layout.role(name).is_some()) {
        return Choice {
            name: name.into(),
            assigned: true,
            stale: None,
        };
    }
    let agent_type = agent_type.or_else(|| entry.and_then(|entry| entry.agent_type.as_deref()));
    if *holder != holder.session()
        && let Some((name, _)) = agent_type.and_then(|agent_type| {
            layout
                .roles()
                .iter()
                .find(|(_, role)| role.agent_types.iter().any(|t| t == agent_type))
        })
    {
        return Choice {
            name: name.clone(),
            assigned: true,
            stale: recorded.map(str::to_string),
        };
    }
    Choice {
        name: if *holder == holder.session() {
            "main"
        } else {
            "subagent"
        }
        .into(),
        assigned: false,
        stale: recorded.map(str::to_string),
    }
}

/// One locked JSON document alongside the todo native-tool state.
pub struct Roles {
    dir: PathBuf,
}

impl Roles {
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn read(&self) -> Result<Doc> {
        store::with_file_lock(&self.dir.join("roles.lock"), || {
            store::try_load(&self.dir.join("roles.json"))
        })
    }

    fn write<T>(&self, f: impl FnOnce(&mut Doc, DateTime<Utc>) -> Result<T>) -> Result<T> {
        store::with_lock_strict(
            &self.dir.join("roles.lock"),
            &self.dir.join("roles.json"),
            |doc: &mut Doc| {
                let now: DateTime<Utc> = std::time::SystemTime::now().into();
                doc.entries
                    .retain(|_, entry| entry.at >= now - chrono::Duration::days(30));
                f(doc, now)
            },
        )
    }

    pub fn resolve(
        &self,
        layout: &Layout,
        facts: &Facts,
        holder: &Holder,
        agent_type: Option<&str>,
    ) -> Result<ResolvedRole> {
        let doc = self.read()?;
        let choice = choose(
            layout,
            holder,
            doc.entries.get(&holder.to_string()),
            agent_type,
        );
        let role = layout
            .role(&choice.name)
            .expect("validated layouts contain built-in roles");
        let warning = choice.stale.map(|stale| {
            format!(
                "warning: recorded todo role {stale:?} no longer exists; using {:?}",
                choice.name
            )
        });
        Ok(ResolvedRole {
            node: layout.fill(&role.scope, facts)?,
            hold_pending: layout.hold_pending(&choice.name, facts)?,
            name: choice.name,
            warning,
        })
    }

    /// Records `name` as `holder`'s role, and `checkout`, when given, as the
    /// directory it selected the role in.
    pub fn record(
        &self,
        layout: &Layout,
        holder: &Holder,
        name: &str,
        checkout: Option<&Path>,
    ) -> Result<()> {
        if holder.is_human() {
            bail!("devkit todo role needs an agent session; terminal callers have no role record");
        }
        if layout.role(name).is_none() {
            bail!(
                "unknown todo role {name:?}; valid roles: {}",
                layout
                    .roles()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        self.write(|doc, now| {
            let entry = doc
                .entries
                .entry(holder.to_string())
                .or_insert_with(|| Entry::new(now));
            entry.role = Some(name.into());
            if let Some(checkout) = checkout {
                entry.checkout = Some(checkout.to_path_buf());
            }
            entry.at = now;
            Ok(())
        })
    }

    /// The directory [`Roles::record`] last recorded for `holder`.
    pub fn checkout(&self, holder: &Holder) -> Result<Option<PathBuf>> {
        Ok(self
            .read()?
            .entries
            .remove(&holder.to_string())
            .and_then(|entry| entry.checkout))
    }

    pub fn spawn(&self, layout: &Layout, holder: &Holder, agent_type: Option<&str>) -> Result<()> {
        self.write(|doc, now| {
            let entry = doc
                .entries
                .entry(holder.to_string())
                .or_insert_with(|| Entry::new(now));
            entry.agent_type = agent_type.map(str::to_string);
            if entry.role.is_none() {
                let choice = choose(layout, holder, Some(entry), agent_type);
                if choice.assigned {
                    entry.role = Some(choice.name);
                }
            }
            entry.at = now;
            Ok(())
        })
    }

    pub fn nudge(&self, layout: &Layout, facts: &Facts, holder: &Holder) -> Result<Option<String>> {
        self.write(|doc, now| {
            let entry = doc.entries.get(&holder.to_string());
            let current = choose(layout, holder, entry, None);
            if entry.is_some_and(|entry| entry.nudged) || current.assigned { return Ok(None); }
            let parent = holder.session();
            let parent_role = choose(layout, &parent, doc.entries.get(&parent.to_string()), None).name;
            let candidates: Vec<_> = layout.roles().iter().filter(|(name, role)| {
                if facts.subagent { role.parent.as_deref() == Some(parent_role.as_str()) }
                else { role.parent.is_none() && !matches!(name.as_str(), "main" | "subagent") }
            }).map(|(name, role)| {
                Ok(format!("{name} (writing to `{}`)", layout.fill(&role.scope, facts)?))
            }).collect::<Result<_>>()?;
            if candidates.is_empty() { return Ok(None); }
            let node = layout.fill(&layout.role(&current.name).expect("validated layouts contain built-in roles").scope, facts)?;
            let text = format!("Your todo role is the default `{}`, writing to `{node}`. Todo roles you can take: {}. Select the role matching your work with `devkit todo role <name>` before adding the rest of your plan.", current.name, candidates.join(", "));
            let entry = doc.entries.entry(holder.to_string()).or_insert_with(|| Entry::new(now));
            entry.nudged = true;
            entry.at = now;
            Ok(Some(text))
        })
    }
}
