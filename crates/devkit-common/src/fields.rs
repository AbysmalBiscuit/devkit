//! Ticket fields: which a ticket must carry, whether the values given are
//! valid, and the message naming what to pass. Pure, so the CLI, the MCP hook
//! and `ticket fields` judge a ticket alike.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use devkit_config::{
    CALLER_CONDITION, EnumValue, FieldDecl, FieldLevel, FieldType, Person, Place, TrackerKind,
};
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

/// The field values a caller passed, and the fields marked deliberately
/// empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Given {
    pub values: BTreeMap<String, Raw>,
    pub nones: BTreeSet<String>,
}

/// One field's value as a caller passed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Raw {
    /// `--arg` text, which a list field splits on commas.
    Text(String),
    /// Items passed one by one, as an MCP call's array carries them: none is
    /// split, so an item may hold a comma.
    Items(Vec<String>),
}

impl Raw {
    /// The value as a refusal quotes it.
    pub fn shown(&self) -> String {
        match self {
            Raw::Text(s) => s.clone(),
            Raw::Items(items) => items.join(", "),
        }
    }
}

impl From<String> for Raw {
    fn from(s: String) -> Self {
        Raw::Text(s)
    }
}

impl From<&str> for Raw {
    fn from(s: &str) -> Self {
        Raw::Text(s.to_string())
    }
}

impl Given {
    /// `self` with each field's [`default_of`] for every field given no
    /// value.
    pub fn with_defaults(&self, decls: &BTreeMap<String, FieldDecl>) -> Given {
        let mut out = self.clone();
        for (name, decl) in decls {
            if let Some(d) = default_of(decls, name, decl, &self.nones)
                && !out.values.contains_key(name)
            {
                out.values.insert(name.clone(), d.as_str().into());
            }
        }
        out
    }
}

/// The field holding project membership when `nones` keeps the ticket out
/// of the project.
fn outside_project<'a>(
    decls: &'a BTreeMap<String, FieldDecl>,
    nones: &BTreeSet<String>,
) -> Option<&'a str> {
    decls
        .iter()
        .find(|(name, d)| d.r#where == Some(Place::Project) && nones.contains(*name))
        .map(|(name, _)| name.as_str())
}

/// The default a create gives field `name`: none for a field marked
/// `--none`, nor for a project field of a ticket kept out of the project.
fn default_of<'a>(
    decls: &BTreeMap<String, FieldDecl>,
    name: &str,
    decl: &'a FieldDecl,
    nones: &BTreeSet<String>,
) -> Option<&'a String> {
    let in_project = || {
        !matches!(decl.r#where, Some(Place::ProjectField { .. }))
            || outside_project(decls, nones).is_none()
    };
    decl.default
        .as_ref()
        .filter(|_| !nones.contains(name) && in_project())
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

impl Resolved {
    /// Each field a write sets, with its values: a `--none` clear has none.
    pub fn written(&self) -> impl Iterator<Item = (&String, &[Value])> {
        self.fields
            .iter()
            .map(|(name, values)| (name, values.as_slice()))
            .chain(self.nones.iter().map(|name| (name, &[][..])))
    }
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
    /// A value is given for a field of the project while `--none <project>`
    /// keeps the ticket out of it.
    OutsideProject { field: String, project: String },
}

impl Problem {
    pub fn field(&self) -> &str {
        match self {
            Problem::Missing { field }
            | Problem::Invalid { field, .. }
            | Problem::Clear { field }
            | Problem::NoneNotAllowed { field }
            | Problem::OutsideProject { field, .. } => field,
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

/// The list items of `raw`: text split on commas for a list field, each
/// trimmed, with blanks and repeats dropped.
fn items(decl: &FieldDecl, raw: &Raw) -> Vec<String> {
    let pieces: Vec<&str> = match raw {
        Raw::Text(text) if decl.list => text.split(',').collect(),
        Raw::Text(text) => vec![text],
        Raw::Items(items) => items.iter().map(String::as_str).collect(),
    };
    let mut out: Vec<String> = Vec::new();
    for p in pieces.into_iter().map(str::trim).filter(|p| !p.is_empty()) {
        if !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// `p`'s identity on `kind`, when `[people]` records one.
pub fn person_id(p: &Person, kind: TrackerKind) -> Option<&str> {
    match kind {
        TrackerKind::Github => p.github.as_deref(),
        TrackerKind::Linear => p.linear.as_deref(),
        TrackerKind::None => None,
    }
}

/// Who turns an enum value's name into the id the tracker takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Naming {
    /// devkit writes the id, so a name several values share must be
    /// narrowed to one by the ticket's team, or given as an id.
    Devkit,
    /// The tracker resolves the name itself, within the issue's team, so a
    /// name several team-bound values share passes while the team is
    /// unknown.
    Tracker,
    /// The value reaches the tracker as given, so only what the tracker
    /// itself takes names a value: its id or an alias, never devkit's name.
    Verbatim,
}

/// `value` as a person tells it from another value of the same name: its
/// id, and the teams it is valid in.
fn described(value: &EnumValue) -> String {
    if value.teams.is_empty() {
        format!("{} (id {})", value.name, value.id)
    } else {
        format!(
            "{} (id {}, teams {})",
            value.name,
            value.id,
            value.teams.join(", ")
        )
    }
}

/// `value` as a tracker takes it: its id, with its name, teams and aliases
/// beside it.
fn as_tracker_takes(value: &EnumValue) -> String {
    let mut about = Vec::new();
    if !value.name.eq_ignore_ascii_case(&value.id) {
        about.push(value.name.clone());
    }
    if !value.teams.is_empty() {
        about.push(format!("teams {}", value.teams.join(", ")));
    }
    let others: Vec<&str> = value
        .aliases
        .iter()
        .map(String::as_str)
        .filter(|a| !a.eq_ignore_ascii_case(&value.name))
        .collect();
    if !others.is_empty() {
        about.push(format!("or {}", others.join(", ")));
    }
    if about.is_empty() {
        value.id.clone()
    } else {
        format!("{} ({})", value.id, about.join("; "))
    }
}

/// The ticket's team as team-bound values may record it: by any spelling of
/// the declared team value it names, case-insensitively.
struct Team<'a> {
    given: &'a str,
    spellings: Vec<&'a str>,
}

impl<'a> Team<'a> {
    fn of(decls: &'a BTreeMap<String, FieldDecl>, given: &'a str) -> Self {
        let mut spellings = vec![given];
        for value in decls
            .values()
            .filter(|d| d.r#where == Some(Place::Team))
            .flat_map(|d| d.enum_values(given))
        {
            spellings.push(&value.name);
            spellings.push(&value.id);
            spellings.extend(value.aliases.iter().map(String::as_str));
        }
        Team { given, spellings }
    }

    fn admits(&self, value: &EnumValue) -> bool {
        value.teams.is_empty()
            || value
                .teams
                .iter()
                .any(|t| self.spellings.iter().any(|s| s.eq_ignore_ascii_case(t)))
    }
}

/// The value of `decl` that `item` names in `team`. When the team is known,
/// a name only one of its values carries resolves to that value, whatever
/// the naming, and a name several of them share is refused.
fn enum_one<'a>(
    name: &str,
    decl: &'a FieldDecl,
    item: &str,
    team: Option<&Team>,
    naming: Naming,
) -> Result<&'a EnumValue, String> {
    let named = match naming {
        Naming::Verbatim => decl.tracker_values(item),
        Naming::Devkit | Naming::Tracker => decl.enum_values(item),
    };
    if named.is_empty() && naming == Naming::Verbatim {
        let by_name = decl.enum_values(item);
        if !by_name.is_empty() {
            let ids: Vec<String> = by_name.iter().map(|v| format!("`{}`", v.id)).collect();
            return Err(format!(
                "is devkit's name for a value, and this call reaches the tracker unchanged: \
                 pass {}",
                ids.join(" or ")
            ));
        }
    }
    let fits: Vec<&EnumValue> = named
        .iter()
        .copied()
        .filter(|v| team.is_none_or(|t| t.admits(v)))
        .collect();
    match (fits.as_slice(), naming, team) {
        ([v], ..) => Ok(v),
        ([v, ..], Naming::Tracker | Naming::Verbatim, None)
            if fits.iter().all(|v| !v.teams.is_empty()) =>
        {
            Ok(v)
        }
        ([], ..) => match team.filter(|_| !named.is_empty()) {
            Some(team) => {
                let mut teams: Vec<&str> = named
                    .iter()
                    .flat_map(|v| &v.teams)
                    .map(String::as_str)
                    .collect();
                teams.sort_unstable();
                teams.dedup();
                Err(format!(
                    "is valid in team {} only, not in team {}",
                    teams.join(", "),
                    team.given
                ))
            }
            None => Err(format!("is not a value of `{name}`")),
        },
        (several, ..) => Err(format!(
            "names several values of `{name}`; pass the id of the one meant: {}",
            several
                .iter()
                .map(|v| described(v))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn one(
    name: &str,
    decl: &FieldDecl,
    item: &str,
    kind: TrackerKind,
    people: &HashMap<String, Person>,
    team: Option<&Team>,
    naming: Naming,
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
            let v = enum_one(name, decl, item, team, naming)?;
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
/// rather than the first. `team` is the ticket's team, when known, which
/// scopes team-bound enum values.
pub fn resolve(
    decls: &BTreeMap<String, FieldDecl>,
    kind: TrackerKind,
    given: &Given,
    people: &HashMap<String, Person>,
    team: Option<&str>,
    naming: Naming,
) -> Result<Resolved, Vec<Problem>> {
    let team = team.map(|t| Team::of(decls, t));
    let mut problems = Vec::new();
    let mut out = Resolved::default();
    for (name, raw) in &given.values {
        let invalid = |reason: String| Problem::Invalid {
            field: name.clone(),
            value: raw.shown(),
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
            problems.push(invalid(if decl.allows_none() {
                format!(
                    "an empty value is not a value; pass `--none {name}` to leave `{name}` empty"
                )
            } else {
                format!("an empty value is not a value, and `{name}` cannot be left empty")
            }));
            continue;
        }
        let mut values = Vec::new();
        for item in &items {
            match one(name, decl, item, kind, people, team.as_ref(), naming) {
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
    let raw = items(decl, given.values.get(name)?).into_iter().next()?;
    Some(
        decl.enum_values(&raw)
            .first()
            .map_or_else(|| raw.clone(), |v| v.name.clone()),
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
            if let Some(d) = default_of(decls, name, decl, &resolved.nones) {
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
/// carry; on an edit, every clear; on both, a project field given a value
/// while the ticket is kept out of the project.
pub fn check(
    decls: &BTreeMap<String, FieldDecl>,
    surface: Surface,
    resolved: &Resolved,
    current: Option<&Current>,
    caller: Caller,
) -> Report {
    let seen = seen_on(decls, surface, resolved, current);
    let mut report = Report::default();
    let outside = outside_project(decls, &resolved.nones);
    for (name, decl) in decls {
        let field = || name.clone();
        let none = resolved.nones.contains(name);
        if let Some(project) = outside
            && matches!(decl.r#where, Some(Place::ProjectField { .. }))
            && resolved.fields.contains_key(name)
        {
            report.refused.push(Problem::OutsideProject {
                field: field(),
                project: project.to_string(),
            });
        }
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

/// Each enum value of `decl` as a person passes it: its name, with its id
/// and teams when another value shares the name.
fn value_names(decl: &FieldDecl) -> Vec<String> {
    decl.values
        .iter()
        .map(|v| {
            let shared = decl
                .values
                .iter()
                .filter(|o| o.name.eq_ignore_ascii_case(&v.name))
                .count()
                > 1;
            if shared { described(v) } else { v.name.clone() }
        })
        .collect()
}

/// The values a field takes, as a caller of `naming` passes them.
fn valid_values(decl: &FieldDecl, naming: Naming) -> Option<String> {
    match decl.r#type? {
        FieldType::Enum if naming == Naming::Verbatim => Some(
            decl.values
                .iter()
                .map(as_tracker_takes)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        FieldType::Enum => Some(value_names(decl).join(", ")),
        FieldType::User => Some("a `[people]` alias".into()),
        FieldType::Number => Some("a number".into()),
        FieldType::Date => Some("a date, YYYY-MM-DD".into()),
        FieldType::Issue => Some("a ticket reference".into()),
        FieldType::String => None,
    }
}

/// Why a write to `kind`'s tracker may have failed on a stale declared id.
pub fn stale_hint(
    kind: TrackerKind,
    field: &str,
    decl: &FieldDecl,
    e: &anyhow::Error,
) -> Option<String> {
    let text = format!("{e:#}");
    let names_an_id = decl.r#type == Some(FieldType::Enum)
        && (text.contains("Could not resolve")
            || text.contains("NOT_FOUND")
            || text.contains("not found"));
    names_an_id.then(|| {
        format!(
            "the declared id may be stale: check `values` under [ticket.fields.{kind}.{field}] against the tracker"
        )
    })
}

/// One block per problem: the field, what is wrong, how to set it (`--arg`,
/// and `--none` when the field allows it), its description, its valid values
/// and its default.
pub fn message(decls: &BTreeMap<String, FieldDecl>, problems: &[Problem]) -> String {
    message_for(decls, problems, Naming::Devkit)
}

/// [`message`], listing each enum's values as a caller of `naming` passes
/// them.
pub fn message_for(
    decls: &BTreeMap<String, FieldDecl>,
    problems: &[Problem],
    naming: Naming,
) -> String {
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
            Problem::OutsideProject { project, .. } => format!(
                "`{name}` is a field of the project, but `--none {project}` keeps the ticket out \
                 of it: drop `--arg {name}=...` or `--none {project}`"
            ),
        };
        if let Some(decl) = decl {
            if let Some(d) = decl.description.as_deref() {
                block.push_str(&crate::required::description_line(name, d));
            }
            if let Some(v) = valid_values(decl, naming) {
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
                .map_or_else(|| vec![v.shown()], |d| items(d, v));
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
            values: value_names(d),
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
                .map(|(k, v)| (k.to_string(), (*v).into()))
                .collect(),
            nones: nones.iter().map(|n| n.to_string()).collect(),
        }
    }

    fn ok(d: &BTreeMap<String, FieldDecl>, g: &Given) -> Resolved {
        resolve(
            d,
            TrackerKind::Github,
            g,
            &HashMap::new(),
            None,
            Naming::Devkit,
        )
        .unwrap()
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
            Naming::Devkit,
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
            Naming::Devkit,
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
                None,
                Naming::Devkit,
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
            Naming::Devkit,
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
            Naming::Devkit,
        )
        .unwrap();
        assert_eq!(r.fields["assignee"], [Value::User {
            alias: "lev".into(),
            id: "LevValle".into()
        }]);
    }

    #[test]
    fn an_exact_id_resolves_before_a_name() {
        let d = decls(
            "[ticket.fields.github.size]\ntype = \"enum\"\n\
             where = { project_field = \"F\", name = \"Size\" }\n\
             values = [{ name = \"2\", id = \"o-two\" }, { name = \"small\", id = \"2\" }]\n",
        );
        let r = ok(&d, &given(&[("size", "2")], &[]));
        assert_eq!(r.fields["size"], [Value::Enum {
            name: "small".into(),
            id: "2".into()
        }]);
    }

    #[test]
    fn a_team_scoped_value_from_another_team_is_refused() {
        let d = linear(
            "[ticket.fields.linear.labels]\ntype = \"enum\"\nlist = true\nwhere = \"labels\"\n\
             values = [{ name = \"infra\", id = \"L1\", teams = [\"ENG\"] }]\n",
        );
        let g = given(&[("labels", "infra")], &[]);
        let e = resolve(
            &d,
            TrackerKind::Linear,
            &g,
            &HashMap::new(),
            Some("OPS"),
            Naming::Devkit,
        )
        .unwrap_err();
        assert!(
            matches!(&e[..], [Problem::Invalid { reason, .. }] if reason.contains("ENG") && reason.contains("OPS")),
            "{e:?}"
        );
        assert!(
            resolve(
                &d,
                TrackerKind::Linear,
                &g,
                &HashMap::new(),
                Some("ENG"),
                Naming::Devkit
            )
            .is_ok()
        );
    }

    #[test]
    fn an_empty_arg_points_at_none_when_the_field_allows_it() {
        let mut d = decls(MILESTONE);
        let empty = |d: &BTreeMap<String, FieldDecl>| {
            resolve(
                d,
                TrackerKind::Github,
                &given(&[("milestone", "  ")], &[]),
                &HashMap::new(),
                None,
                Naming::Devkit,
            )
            .unwrap_err()
        };
        let e = empty(&d);
        assert!(
            matches!(&e[..], [Problem::Invalid { reason, .. }] if !reason.contains("--none") && reason.contains("cannot be left empty")),
            "allow_none = false refuses --none, so the refusal does not offer it: {e:?}"
        );
        d.get_mut("milestone").unwrap().allow_none = None;
        let e = empty(&d);
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
            Naming::Devkit,
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
