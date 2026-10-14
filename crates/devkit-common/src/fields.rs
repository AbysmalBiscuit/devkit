//! Ticket fields: which a ticket must carry, whether the values given are
//! valid, and the message naming what to pass. Pure, so the CLI, the MCP hook
//! and `ticket fields` judge a ticket alike.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use devkit_config::{CALLER_CONDITION, FieldDecl, FieldLevel, FieldType, Person, TrackerKind};
use serde::Serialize;

use crate::caller::Caller;

/// The path a ticket write takes, which decides what counts as missing and
/// what a condition sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// `ticket create`: the values given, plus defaults.
    Create,
    /// `ticket edit`: the ticket's current fields with the given ones laid
    /// over them.
    Edit,
    /// A tracker MCP create: the call's input alone.
    McpCreate,
    /// A tracker MCP update: a partial write of the fields it carries.
    McpUpdate,
}

/// The field values a caller passed, as text, and the fields marked
/// deliberately empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Given {
    pub values: BTreeMap<String, String>,
    pub nones: BTreeSet<String>,
}

impl Given {
    /// `self` with each field's `default` for every field given neither a
    /// value nor `--none`.
    pub fn with_defaults(&self, decls: &BTreeMap<String, FieldDecl>) -> Given {
        let mut out = self.clone();
        for (name, decl) in decls {
            if let Some(d) = &decl.default
                && !out.values.contains_key(name)
                && !out.nones.contains(name)
            {
                out.values.insert(name.clone(), d.clone());
            }
        }
        out
    }
}

/// One validated value, ready to write.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Number(f64),
    /// `YYYY-MM-DD`.
    Date(String),
    /// A declared enum value: what agents pass and what devkit writes.
    Enum {
        name: String,
        id: String,
    },
    /// A `[people]` alias and its identity on the tracker.
    User {
        alias: String,
        id: String,
    },
    /// A ticket reference, checked against the tracker by the writer.
    Issue(String),
}

impl Value {
    /// The value as a condition and a person read it: an enum's name, a
    /// user's alias.
    pub fn name(&self) -> String {
        match self {
            Value::Text(s) | Value::Date(s) | Value::Issue(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Enum { name, .. } => name.clone(),
            Value::User { alias, .. } => alias.clone(),
        }
    }
}

/// Every given value, validated. A scalar is a one-element list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    pub fields: BTreeMap<String, Vec<Value>>,
    pub nones: BTreeSet<String>,
}

/// A ticket's present field values by field name, as names (an enum value's
/// name, a user's alias), for [`Surface::Edit`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Current(pub BTreeMap<String, Vec<String>>);

/// What is wrong with a ticket's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A field the ticket must carry has no value.
    Missing { field: String },
    /// A given value is not one the field takes.
    Invalid {
        field: String,
        value: String,
        reason: String,
    },
    /// An edit clears a field whose level reports it.
    Clear { field: String },
    /// `--none` names a field that may not be left empty.
    NoneNotAllowed { field: String },
}

impl Problem {
    pub fn field(&self) -> &str {
        match self {
            Problem::Missing { field }
            | Problem::Invalid { field, .. }
            | Problem::Clear { field }
            | Problem::NoneNotAllowed { field } => field,
        }
    }
}

/// What a write must refuse, and what it reports after writing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub refused: Vec<Problem>,
    pub warned: Vec<Problem>,
}

/// Whether one `when` condition holds over `seen`.
fn holds(
    key: &str,
    options: &[String],
    seen: &BTreeMap<String, Vec<String>>,
    caller: Caller,
) -> bool {
    if key == CALLER_CONDITION {
        let who = match caller {
            Caller::Agent => "agent",
            Caller::Human => "human",
        };
        return options.iter().any(|o| o == who);
    }
    let values = seen.get(key).map(Vec::as_slice).unwrap_or_default();
    options.iter().any(|o| match o.as_str() {
        "*" => !values.is_empty(),
        "" => values.is_empty(),
        o => values
            .iter()
            .any(|v| v.trim().eq_ignore_ascii_case(o.trim())),
    })
}

/// The level of `name` given the ticket's fields `seen`: the first `when`
/// entry whose conditions all hold, else its `level`.
pub fn level_of(
    decls: &BTreeMap<String, FieldDecl>,
    name: &str,
    seen: &BTreeMap<String, Vec<String>>,
    caller: Caller,
) -> FieldLevel {
    let Some(decl) = decls.get(name) else {
        return FieldLevel::None;
    };
    decl.when
        .iter()
        .find(|w| {
            w.r#if
                .iter()
                .all(|(k, c)| holds(k, c.options(), seen, caller))
        })
        .map_or(decl.level, |w| w.level)
}

/// The highest level `name` could take when only the fields in `known` are
/// known: an entry whose conditions name an unknown field counts as holding,
/// and the fallback `level` counts only when no fully known entry holds.
fn level_upper_bound(
    decl: &FieldDecl,
    known: &BTreeSet<&str>,
    seen: &BTreeMap<String, Vec<String>>,
    caller: Caller,
) -> FieldLevel {
    let mut level = FieldLevel::None;
    for w in &decl.when {
        let all_known = w
            .r#if
            .keys()
            .all(|k| k == CALLER_CONDITION || known.contains(k.as_str()));
        let could_hold = w.r#if.iter().all(|(k, c)| {
            (k != CALLER_CONDITION && !known.contains(k.as_str()))
                || holds(k, c.options(), seen, caller)
        });
        if could_hold {
            level = level.max(w.level);
            if all_known {
                return level;
            }
        }
    }
    level.max(decl.level)
}

/// The list items of `raw`: split on commas for a list field, trimmed, with
/// blanks and repeats dropped.
fn items(decl: &FieldDecl, raw: &str) -> Vec<String> {
    let pieces: Vec<&str> = if decl.list {
        raw.split(',').collect()
    } else {
        vec![raw]
    };
    let mut out: Vec<String> = Vec::new();
    for p in pieces.into_iter().map(str::trim).filter(|p| !p.is_empty()) {
        if !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

fn person_id(p: &Person, kind: TrackerKind) -> Option<&str> {
    match kind {
        TrackerKind::Github => p.github.as_deref(),
        TrackerKind::Linear => p.linear.as_deref(),
        TrackerKind::None => None,
    }
}

fn one(
    name: &str,
    decl: &FieldDecl,
    item: &str,
    kind: TrackerKind,
    people: &HashMap<String, Person>,
    team: Option<&str>,
) -> Result<Value, String> {
    let ty = decl.r#type.unwrap_or(FieldType::String);
    match ty {
        FieldType::String => Ok(Value::Text(item.to_string())),
        FieldType::Issue => Ok(Value::Issue(item.to_string())),
        FieldType::Number => item
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(Value::Number)
            .ok_or_else(|| "is not a number".to_string()),
        FieldType::Date => chrono::NaiveDate::parse_from_str(item, "%Y-%m-%d")
            .map(|_| Value::Date(item.to_string()))
            .map_err(|_| "is not a date (YYYY-MM-DD)".to_string()),
        FieldType::Enum => {
            let v = decl
                .enum_value(item)
                .ok_or_else(|| format!("is not a value of `{name}`"))?;
            if let Some(team) = team
                && !v.teams.is_empty()
                && !v.teams.iter().any(|t| t == team)
            {
                return Err(format!(
                    "is valid in team {} only, not in team {team}",
                    v.teams.join(", ")
                ));
            }
            Ok(Value::Enum {
                name: v.name.clone(),
                id: v.id.clone(),
            })
        }
        FieldType::User => {
            let p = people
                .get(item)
                .ok_or_else(|| "is not a `[people]` alias".to_string())?;
            let id = person_id(p, kind).ok_or_else(|| {
                format!(
                    "has no `{key}` identity; add `{key} = \"...\"` to `[people.{item}]`",
                    key = kind.as_str()
                )
            })?;
            Ok(Value::User {
                alias: item.to_string(),
                id: id.to_string(),
            })
        }
    }
}

/// Validate every given value and `--none` name, reporting every problem
/// rather than the first.
pub fn resolve(
    decls: &BTreeMap<String, FieldDecl>,
    kind: TrackerKind,
    given: &Given,
    people: &HashMap<String, Person>,
    team: Option<&str>,
) -> Result<Resolved, Vec<Problem>> {
    let mut problems = Vec::new();
    let mut out = Resolved::default();
    for (name, raw) in &given.values {
        let invalid = |reason: String| Problem::Invalid {
            field: name.clone(),
            value: raw.clone(),
            reason,
        };
        let Some(decl) = decls.get(name) else {
            problems.push(invalid(format!("`{name}` is not a declared ticket field")));
            continue;
        };
        if given.nones.contains(name) {
            problems.push(invalid(format!(
                "`{name}` is given both a value and `--none {name}`; pass one"
            )));
            continue;
        }
        let items = items(decl, raw);
        if items.is_empty() {
            problems.push(invalid(format!(
                "an empty value is not a value; pass `--none {name}` to leave `{name}` empty"
            )));
            continue;
        }
        let mut values = Vec::new();
        for item in &items {
            match one(name, decl, item, kind, people, team) {
                Ok(v) => values.push(v),
                Err(reason) => problems.push(Problem::Invalid {
                    field: name.clone(),
                    value: item.clone(),
                    reason,
                }),
            }
        }
        out.fields.insert(name.clone(), values);
    }
    for name in &given.nones {
        if !decls.contains_key(name) {
            problems.push(Problem::Invalid {
                field: name.clone(),
                value: String::new(),
                reason: format!("`--none {name}` names no declared ticket field"),
            });
        }
    }
    out.nones = given.nones.clone();
    if problems.is_empty() {
        Ok(out)
    } else {
        Err(problems)
    }
}

/// The team a ticket is filed in: the name of the value given for the
/// `team`-placed field, which scopes team-bound values.
pub fn team(decls: &BTreeMap<String, FieldDecl>, given: &Given) -> Option<String> {
    let (name, decl) = decls
        .iter()
        .find(|(_, d)| d.r#where == Some(devkit_config::Place::Team))?;
    let raw = given.values.get(name)?;
    Some(
        decl.enum_value(raw)
            .map_or_else(|| raw.trim().to_string(), |v| v.name.clone()),
    )
}

/// What a condition sees on `surface`.
fn seen_on(
    decls: &BTreeMap<String, FieldDecl>,
    surface: Surface,
    resolved: &Resolved,
    current: Option<&Current>,
) -> BTreeMap<String, Vec<String>> {
    let mut seen = match (surface, current) {
        (Surface::Edit, Some(c)) => c.0.clone(),
        _ => BTreeMap::new(),
    };
    if surface == Surface::Create {
        for (name, decl) in decls {
            if let Some(d) = &decl.default
                && !resolved.nones.contains(name)
            {
                seen.insert(name.clone(), vec![d.clone()]);
            }
        }
    }
    for (name, values) in &resolved.fields {
        seen.insert(name.clone(), values.iter().map(Value::name).collect());
    }
    for name in &resolved.nones {
        seen.remove(name);
    }
    seen
}

/// Judge a ticket's fields on `surface`: on a create, every field it must
/// carry; on an edit, every clear.
pub fn check(
    decls: &BTreeMap<String, FieldDecl>,
    surface: Surface,
    resolved: &Resolved,
    current: Option<&Current>,
    caller: Caller,
) -> Report {
    let seen = seen_on(decls, surface, resolved, current);
    let mut report = Report::default();
    for (name, decl) in decls {
        let field = || name.clone();
        let none = resolved.nones.contains(name);
        match surface {
            Surface::Create | Surface::McpCreate => {
                if none {
                    if !decl.allows_none() {
                        report
                            .refused
                            .push(Problem::NoneNotAllowed { field: field() });
                    }
                    continue;
                }
                let given = resolved.fields.contains_key(name);
                let defaulted = surface == Surface::Create && decl.default.is_some();
                if given || defaulted {
                    continue;
                }
                match level_of(decls, name, &seen, caller) {
                    FieldLevel::Error => report.refused.push(Problem::Missing { field: field() }),
                    FieldLevel::Warning => report.warned.push(Problem::Missing { field: field() }),
                    FieldLevel::Info | FieldLevel::None => {}
                }
            }
            Surface::Edit | Surface::McpUpdate => {
                if !none {
                    continue;
                }
                if !decl.allows_none() {
                    report
                        .refused
                        .push(Problem::NoneNotAllowed { field: field() });
                    continue;
                }
                let level = if surface == Surface::McpUpdate {
                    let known: BTreeSet<&str> = resolved
                        .fields
                        .keys()
                        .chain(&resolved.nones)
                        .map(String::as_str)
                        .collect();
                    level_upper_bound(decl, &known, &seen, caller)
                } else {
                    level_of(decls, name, &seen, caller)
                };
                if level == FieldLevel::Warning {
                    report.warned.push(Problem::Clear { field: field() });
                }
            }
        }
    }
    report
}

/// The values a field takes, as a person passes them.
fn valid_values(decl: &FieldDecl) -> Option<String> {
    match decl.r#type? {
        FieldType::Enum => Some(
            decl.values
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        FieldType::User => Some("a `[people]` alias".into()),
        FieldType::Number => Some("a number".into()),
        FieldType::Date => Some("a date, YYYY-MM-DD".into()),
        FieldType::Issue => Some("a ticket reference".into()),
        FieldType::String => None,
    }
}

/// One block per problem: the field, what is wrong, how to set it (`--arg`,
/// and `--none` when the field allows it), its description, its valid values
/// and its default.
pub fn message(decls: &BTreeMap<String, FieldDecl>, problems: &[Problem]) -> String {
    let mut out = Vec::new();
    for p in problems {
        let name = p.field();
        let decl = decls.get(name);
        let allows_none = decl.is_some_and(FieldDecl::allows_none);
        let set = if allows_none {
            format!("--arg {name}=... or --none {name}")
        } else {
            format!("--arg {name}=...")
        };
        let mut block = match p {
            Problem::Missing { .. } => format!("`{name}` is required: pass {set}"),
            Problem::Invalid { value, reason, .. } if value.is_empty() => {
                format!("`{name}`: {reason}")
            }
            Problem::Invalid { value, reason, .. } => format!("`{name}`: `{value}` {reason}"),
            Problem::Clear { .. } => format!("`{name}` was cleared; set it with --arg {name}=..."),
            Problem::NoneNotAllowed { .. } => {
                format!("`{name}` cannot be left empty (allow_none = false): pass --arg {name}=...")
            }
        };
        if let Some(decl) = decl {
            if let Some(d) = decl.description.as_deref() {
                block.push_str(&crate::required::description_line(name, d));
            }
            if let Some(v) = valid_values(decl) {
                block.push_str(&format!("\n  values: {v}"));
            }
            if let Some(d) = &decl.default {
                block.push_str(&format!("\n  default: {d}"));
            }
        }
        out.push(block);
    }
    out.join("\n")
}

/// A field as `ticket fields` shows it.
#[derive(Debug, Clone, Serialize)]
pub struct FieldView {
    pub name: String,
    pub r#type: Option<FieldType>,
    pub list: bool,
    /// The level for this caller and the values given.
    pub level: FieldLevel,
    pub when: Vec<devkit_config::WhenEntry>,
    pub values: Vec<String>,
    pub default: Option<String>,
    pub description: Option<String>,
    pub allows_none: bool,
}

/// Every field that can rise above `none`, at its level for `caller` and
/// the values in `given`.
pub fn listing(
    decls: &BTreeMap<String, FieldDecl>,
    caller: Caller,
    given: &Given,
) -> Vec<FieldView> {
    let seen: BTreeMap<String, Vec<String>> = given
        .values
        .iter()
        .filter(|(k, _)| !given.nones.contains(*k))
        .map(|(k, v)| {
            let items = decls
                .get(k)
                .map_or_else(|| vec![v.clone()], |d| items(d, v));
            (k.clone(), items)
        })
        .collect();
    decls
        .iter()
        .filter(|(_, d)| d.max_level() > FieldLevel::None)
        .map(|(name, d)| FieldView {
            name: name.clone(),
            r#type: d.r#type,
            list: d.list,
            level: level_of(decls, name, &seen, caller),
            when: d.when.clone(),
            values: d.values.iter().map(|v| v.name.clone()).collect(),
            default: d.default.clone(),
            description: d.description.clone(),
            allows_none: d.allows_none(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use devkit_config::Config;

    use super::*;

    fn decls(toml: &str) -> BTreeMap<String, FieldDecl> {
        Config::parse(toml).unwrap().ticket.fields.github
    }

    fn linear(toml: &str) -> BTreeMap<String, FieldDecl> {
        Config::parse(toml).unwrap().ticket.fields.linear
    }

    const PRIORITY: &str = r#"
[ticket.fields.github.priority]
type        = "enum"
where       = { project_field = "PVTSSF_x", name = "Priority" }
values      = [{ name = "P0", id = "f75ad846" }, { name = "P1", id = "47fc9ee4" }, { name = "P2", id = "98236657" }]
default     = "P2"
level       = "error"
description = "P0 drop everything, P1 this cycle, P2 backlog"

[ticket.fields.github.labels]
type   = "string"
list   = true
where  = "labels"
level  = "warning"
"#;

    const MILESTONE: &str = r#"
[ticket.fields.github.project]
type  = "string"
where = "project"

[ticket.fields.github.milestone]
type       = "enum"
where      = "milestone"
values     = [{ name = "v1", id = "v1" }]
level      = "none"
allow_none = false
when       = [{ if = { project = "*" }, level = "error" }]
"#;

    fn given(values: &[(&str, &str)], nones: &[&str]) -> Given {
        Given {
            values: values
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            nones: nones.iter().map(|n| n.to_string()).collect(),
        }
    }

    fn ok(d: &BTreeMap<String, FieldDecl>, g: &Given) -> Resolved {
        resolve(d, TrackerKind::Github, g, &HashMap::new(), None).unwrap()
    }

    fn missing(field: &str) -> Problem {
        Problem::Missing {
            field: field.into(),
        }
    }

    #[test]
    fn missing_error_field_is_refused_on_create_and_defaults_count_only_on_create() {
        let d = decls(PRIORITY);
        let g = given(&[("labels", "a")], &[]);
        let r = ok(&d, &g);
        let create = check(&d, Surface::Create, &r, None, Caller::Agent);
        assert!(create.refused.is_empty(), "{create:?}");
        let mcp = check(&d, Surface::McpCreate, &r, None, Caller::Agent);
        assert_eq!(mcp.refused, [missing("priority")]);
        let msg = message(&d, &mcp.refused);
        assert!(
            msg.contains("P2")
                && msg.contains("--arg priority=")
                && msg.contains("--none priority"),
            "{msg}"
        );
        assert!(msg.contains("P0 drop everything"), "{msg}");
        let mut no_default = d.clone();
        no_default.get_mut("priority").unwrap().default = None;
        let create = check(&no_default, Surface::Create, &r, None, Caller::Agent);
        assert_eq!(create.refused, [missing("priority")]);
    }

    #[test]
    fn warning_is_warned_not_refused() {
        let d = decls(PRIORITY);
        let r = ok(&d, &given(&[("priority", "P1")], &[]));
        let report = check(&d, Surface::Create, &r, None, Caller::Agent);
        assert!(report.refused.is_empty());
        assert_eq!(report.warned, [missing("labels")]);
    }

    #[test]
    fn edit_never_reports_missing_and_never_applies_defaults() {
        let d = decls(PRIORITY);
        let r = ok(&d, &given(&[("labels", "a")], &[]));
        assert!(!r.fields.contains_key("priority"));
        let report = check(
            &d,
            Surface::Edit,
            &r,
            Some(&Current::default()),
            Caller::Agent,
        );
        assert_eq!(report, Report::default());
    }

    #[test]
    fn a_clear_of_an_error_field_is_refused_unless_it_allows_none() {
        let mut d = decls(PRIORITY);
        let r = ok(&d, &given(&[], &["priority"]));
        let report = check(
            &d,
            Surface::Edit,
            &r,
            Some(&Current::default()),
            Caller::Agent,
        );
        assert_eq!(report, Report::default(), "allows none by default");
        d.get_mut("priority").unwrap().allow_none = Some(false);
        let report = check(
            &d,
            Surface::Edit,
            &r,
            Some(&Current::default()),
            Caller::Agent,
        );
        assert_eq!(report.refused, [Problem::NoneNotAllowed {
            field: "priority".into()
        }]);
        let r = ok(&d, &given(&[], &["labels"]));
        let report = check(
            &d,
            Surface::Edit,
            &r,
            Some(&Current::default()),
            Caller::Agent,
        );
        assert_eq!(report.warned, [Problem::Clear {
            field: "labels".into()
        }]);
    }

    #[test]
    fn edit_conditions_see_current_overlaid_with_given() {
        let mut d = decls(MILESTONE);
        let milestone = d.get_mut("milestone").unwrap();
        milestone.allow_none = None;
        milestone.when[0].level = FieldLevel::Warning;
        let warned = |current: &[(&str, &str)], g: &Given| {
            let current = Current(
                current
                    .iter()
                    .map(|(k, v)| (k.to_string(), vec![v.to_string()]))
                    .collect(),
            );
            check(&d, Surface::Edit, &ok(&d, g), Some(&current), Caller::Agent).warned
        };
        let clear = given(&[], &["milestone"]);
        let cleared = [Problem::Clear {
            field: "milestone".into(),
        }];
        assert_eq!(warned(&[("project", "devkit")], &clear), cleared);
        assert!(warned(&[], &clear).is_empty(), "no project, level none");
        let set_project = given(&[("project", "devkit")], &["milestone"]);
        assert_eq!(warned(&[], &set_project), cleared);
        let clear_both = given(&[], &["milestone", "project"]);
        assert!(warned(&[("project", "devkit")], &clear_both).is_empty());
    }

    #[test]
    fn allow_none_false_refuses_a_clear_at_every_level() {
        for level in ["error", "warning", "info", "none"] {
            let d = decls(&format!(
                "[ticket.fields.github.size]\ntype = \"string\"\nwhere = {{ project_field = \"F\", name = \"Size\" }}\n\
                 level = \"{level}\"\nallow_none = false\n"
            ));
            let r = ok(&d, &given(&[], &["size"]));
            for surface in [Surface::Edit, Surface::McpUpdate] {
                let report = check(&d, surface, &r, Some(&Current::default()), Caller::Agent);
                assert_eq!(
                    report.refused,
                    [Problem::NoneNotAllowed {
                        field: "size".into()
                    }],
                    "{level} on {surface:?}"
                );
            }
        }
    }

    #[test]
    fn mcp_update_judges_a_clear_at_the_highest_possible_level() {
        let mut d = decls(MILESTONE);
        d.get_mut("milestone").unwrap().allow_none = None;
        d.get_mut("milestone").unwrap().when[0].level = FieldLevel::Warning;
        let r = ok(&d, &given(&[], &["milestone"]));
        let report = check(&d, Surface::McpUpdate, &r, None, Caller::Agent);
        assert_eq!(report.warned, [Problem::Clear {
            field: "milestone".into()
        }]);
    }

    #[test]
    fn mcp_update_falls_back_only_when_no_known_entry_holds() {
        let d = decls(
            "[ticket.fields.github.size]\ntype = \"string\"\nlevel = \"warning\"\n\
             where = { project_field = \"F\", name = \"Size\" }\n\
             when = [{ if = { caller = \"agent\" }, level = \"none\" }]\n",
        );
        let r = ok(&d, &given(&[], &["size"]));
        let agent = check(&d, Surface::McpUpdate, &r, None, Caller::Agent);
        assert_eq!(agent, Report::default(), "the agent entry holds: none");
        let human = check(&d, Surface::McpUpdate, &r, None, Caller::Human);
        assert_eq!(human.warned, [Problem::Clear {
            field: "size".into()
        }]);
    }

    #[test]
    fn an_explicit_none_satisfies_error_on_create() {
        let mut d = decls(PRIORITY);
        d.get_mut("priority").unwrap().default = None;
        let r = ok(&d, &given(&[], &["priority", "labels"]));
        let report = check(&d, Surface::Create, &r, None, Caller::Agent);
        assert_eq!(report, Report::default());
    }

    #[test]
    fn invalid_values_are_refused_at_every_level() {
        let mut d = decls(MILESTONE);
        d.get_mut("milestone").unwrap().when.clear();
        assert_eq!(d["milestone"].max_level(), FieldLevel::None);
        let e = resolve(
            &d,
            TrackerKind::Github,
            &given(&[("milestone", "v9")], &[]),
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&e[..], [Problem::Invalid { field, value, .. }] if field == "milestone" && value == "v9"),
            "{e:?}"
        );
        let msg = message(&d, &e);
        assert!(msg.contains("v9") && msg.contains("values: v1"), "{msg}");

        let typed = decls(
            "[ticket.fields.github.estimate]\ntype = \"number\"\nwhere = { project_field = \"F\", name = \"Estimate\" }\n\
             [ticket.fields.github.due]\ntype = \"date\"\nwhere = { project_field = \"G\", name = \"Due\" }\n\
             [ticket.fields.github.owner]\ntype = \"user\"\nwhere = \"assignees\"\n",
        );
        let e = resolve(
            &typed,
            TrackerKind::Github,
            &given(
                &[
                    ("estimate", "lots"),
                    ("due", "2026-13-01"),
                    ("owner", "ghost"),
                ],
                &[],
            ),
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        let fields: Vec<&str> = e.iter().map(Problem::field).collect();
        assert_eq!(
            fields,
            ["due", "estimate", "owner"],
            "every problem, not the first"
        );
        assert!(
            resolve(
                &typed,
                TrackerKind::Github,
                &given(&[("estimate", "NaN")], &[]),
                &HashMap::new(),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn list_values_are_trimmed_and_deduplicated() {
        let d = decls(PRIORITY);
        let r = ok(&d, &given(&[("labels", " a , b,a")], &[]));
        assert_eq!(r.fields["labels"], [
            Value::Text("a".into()),
            Value::Text("b".into())
        ]);
    }

    #[test]
    fn enum_names_match_case_insensitively() {
        let d = decls(PRIORITY);
        let r = ok(&d, &given(&[("priority", "p1")], &[]));
        assert_eq!(r.fields["priority"], [Value::Enum {
            name: "P1".into(),
            id: "47fc9ee4".into()
        }]);
    }

    #[test]
    fn an_alias_without_the_tracker_identity_names_the_key() {
        let d = linear("[ticket.fields.linear.assignee]\ntype = \"user\"\nwhere = \"assignee\"\n");
        let people = Config::parse("[people.lev]\ngithub = \"LevValle\"\n")
            .unwrap()
            .people;
        let e = resolve(
            &d,
            TrackerKind::Linear,
            &given(&[("assignee", "lev")], &[]),
            &people,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&e[..], [Problem::Invalid { reason, .. }] if reason.contains("linear") && reason.contains("[people.lev]")),
            "{e:?}"
        );
        let r = resolve(
            &d,
            TrackerKind::Github,
            &given(&[("assignee", "lev")], &[]),
            &people,
            None,
        )
        .unwrap();
        assert_eq!(r.fields["assignee"], [Value::User {
            alias: "lev".into(),
            id: "LevValle".into()
        }]);
    }

    #[test]
    fn a_team_scoped_value_from_another_team_is_refused() {
        let d = linear(
            "[ticket.fields.linear.labels]\ntype = \"enum\"\nlist = true\nwhere = \"labels\"\n\
             values = [{ name = \"infra\", id = \"L1\", teams = [\"ENG\"] }]\n",
        );
        let g = given(&[("labels", "infra")], &[]);
        let e = resolve(&d, TrackerKind::Linear, &g, &HashMap::new(), Some("OPS")).unwrap_err();
        assert!(
            matches!(&e[..], [Problem::Invalid { reason, .. }] if reason.contains("ENG") && reason.contains("OPS")),
            "{e:?}"
        );
        assert!(resolve(&d, TrackerKind::Linear, &g, &HashMap::new(), Some("ENG")).is_ok());
    }

    #[test]
    fn an_empty_arg_points_at_none() {
        let d = decls(MILESTONE);
        let e = resolve(
            &d,
            TrackerKind::Github,
            &given(&[("milestone", "  ")], &[]),
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(
            matches!(&e[..], [Problem::Invalid { reason, .. }] if reason.contains("--none milestone")),
            "{e:?}"
        );
    }

    #[test]
    fn a_none_naming_no_field_is_refused() {
        let d = decls(MILESTONE);
        let e = resolve(
            &d,
            TrackerKind::Github,
            &given(&[], &["nope"]),
            &HashMap::new(),
            None,
        )
        .unwrap_err();
        assert!(message(&d, &e).contains("--none nope"), "{e:?}");
    }

    #[test]
    fn listing_hides_only_fields_that_can_never_rise_above_none() {
        let d = decls(&MILESTONE.replace("level = \"error\"", "level = \"warning\""));
        let names = |v: &[FieldView]| v.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        let bare = listing(&d, Caller::Agent, &Given::default());
        assert_eq!(
            names(&bare),
            ["milestone"],
            "project never rises above none"
        );
        assert_eq!(bare[0].level, FieldLevel::None);
        let with = listing(&d, Caller::Agent, &given(&[("project", "devkit")], &[]));
        assert_eq!(with[0].level, FieldLevel::Warning);
        assert!(!with[0].allows_none);
    }

    #[test]
    fn a_caller_condition_raises_the_level_for_agents_only() {
        let d = decls(
            "[ticket.fields.github.estimate]\ntype = \"number\"\nlevel = \"info\"\n\
             where = { project_field = \"F\", name = \"Estimate\" }\n\
             when = [{ if = { caller = \"agent\" }, level = \"error\" }]\n",
        );
        let seen = BTreeMap::new();
        assert_eq!(
            level_of(&d, "estimate", &seen, Caller::Agent),
            FieldLevel::Error
        );
        assert_eq!(
            level_of(&d, "estimate", &seen, Caller::Human),
            FieldLevel::Info
        );
    }
}
