//! `[ticket.fields.<tracker>.<field>]`: the ticket fields a repository
//! declares, where each is written on its tracker, and how strongly it must be
//! filled.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use anyhow::{Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Severity, TrackerKind};

/// The names the issue templates' render contexts supply, which no `--arg`
/// and no ticket field can take.
pub const ISSUE_CONTEXT_KEYS: &[&str] = &["input", "ticket_title", "issue_title"];

/// The `when` condition key that tests who is filing rather than a field.
pub const CALLER_CONDITION: &str = "caller";

/// The ticket fields of each tracker, keyed by the field name agents pass as
/// `--arg <name>=<value>` to `ticket create` and `ticket edit`. Only the table
/// of the tracker the repository uses applies. A global config may set
/// `level`, `when`, `default` and `description` for a field across every
/// repository; `type`, `where` and `values` belong in the repository's
/// `devkit.toml`, and `values` is replaced whole by a higher layer.
///
/// ```
/// # use devkit_config::{Config, FieldLevel, FieldType, Place};
/// # let cfg = Config::parse(r#"
/// [ticket.fields.github.priority]
/// type        = "enum"
/// where       = { project_field = "PVTSSF_x", name = "Priority" }
/// values      = [{ name = "P0", id = "f75ad846" }, { name = "P1", id = "47fc9ee4" }]
/// default     = "P1"
/// level       = "error"
/// description = "P0 drop everything, P1 this cycle"
///
/// [ticket.fields.github.milestone]
/// type   = "enum"
/// where  = "milestone"
/// values = [{ name = "v1", id = "v1" }]
/// level  = "none"
/// when   = [{ if = { project = "*" }, level = "warning" }]
/// # "#).unwrap();
/// # let gh = &cfg.ticket.fields.github;
/// # assert_eq!(gh["priority"].r#type, Some(FieldType::Enum));
/// # assert_eq!(gh["milestone"].max_level(), FieldLevel::Warning);
/// # assert_eq!(gh["milestone"].r#where, Some(Place::Milestone));
/// ```
#[derive(Debug, Default, Clone, JsonSchema, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TicketFieldsConfig {
    /// Fields of GitHub issues: their labels, assignees, milestone, type,
    /// parent and blocked-by, membership of `[github] project`, and that
    /// project's item fields.
    pub github: BTreeMap<String, FieldDecl>,
    /// Fields of Linear issues: team, labels, assignee, priority, estimate,
    /// cycle, project, project milestone, due date, parent and blocked-by.
    pub linear: BTreeMap<String, FieldDecl>,
}

impl TicketFieldsConfig {
    /// The declarations of `kind`'s table; empty for no tracker.
    pub fn for_tracker(&self, kind: TrackerKind) -> &BTreeMap<String, FieldDecl> {
        static EMPTY: BTreeMap<String, FieldDecl> = BTreeMap::new();
        match kind {
            TrackerKind::Github => &self.github,
            TrackerKind::Linear => &self.linear,
            TrackerKind::None => &EMPTY,
        }
    }
}

/// One ticket field: what it holds, where on the tracker it is written, and
/// how strongly a ticket must carry it.
#[derive(Debug, Default, Clone, PartialEq, JsonSchema, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FieldDecl {
    /// What a value is: `string`, `number`, `date` (`YYYY-MM-DD`), `enum` (one
    /// of `values`), `user` (a `[people]` alias) or `issue` (a ticket
    /// reference). Required once the layers are merged.
    pub r#type: Option<FieldType>,
    /// Whether the field takes several values, passed comma-separated
    /// (`--arg labels=bug,enhancement`). A write sets exactly the values given.
    /// Only `labels`, `assignees` and `blocked_by` hold several values.
    pub list: bool,
    /// The place on the tracker the value is written. Each place belongs to at
    /// most one field, and the status (`[github] status_field`, Linear's
    /// workflow state) belongs to `[ticket.events]`. On GitHub, `project` and
    /// `project_field` need `[github] project`, `parent` and `blocked_by`
    /// hold `issue` fields, and a `project_field` holds no `user` or `issue`.
    /// Required once the layers are merged.
    pub r#where: Option<Place>,
    /// For `enum`: each value's `name`, which agents pass and match
    /// case-insensitively, and its tracker `id`, which devkit writes and agents
    /// may pass instead. A higher layer replaces the list whole.
    pub values: Vec<EnumValue>,
    /// How a ticket missing this field is treated: `error` refuses it,
    /// `warning` reports it after the write, `info` and `none` say nothing.
    /// `none` also hides the field from `ticket fields` unless a `when` entry
    /// can raise it. `none` when unset.
    pub level: FieldLevel,
    /// Conditional levels: the first entry whose `if` conditions all hold sets
    /// the level, and `level` is the fallback.
    pub when: Vec<WhenEntry>,
    /// The value `ticket create` writes when none is given. `ticket edit`
    /// never applies it.
    pub default: Option<String>,
    /// Shown to agents with the field, in `ticket fields` and in every message
    /// about it.
    pub description: Option<String>,
    /// Whether `--none <field>` may mark the field deliberately empty, which
    /// satisfies `error` on a create and clears the field on an edit. True
    /// when unset; a Linear `team` field never allows it.
    pub allow_none: Option<bool>,
}

impl FieldDecl {
    /// Whether `--none` may name this field.
    pub fn allows_none(&self) -> bool {
        self.r#where != Some(Place::Team) && self.allow_none.unwrap_or(true)
    }

    /// The highest level this field can take, over `level` and every `when`
    /// entry.
    pub fn max_level(&self) -> FieldLevel {
        self.when
            .iter()
            .map(|w| w.level)
            .fold(self.level, FieldLevel::max)
    }

    /// The enum values `raw` names: every value whose id it is exactly, with
    /// every value it is an alias of, else every value whose name or one of
    /// whose aliases it matches case-insensitively, which is several when
    /// values of different Linear teams share a name.
    pub fn enum_values(&self, raw: &str) -> Vec<&EnumValue> {
        let raw = raw.trim();
        self.by_id_or(raw, |v| {
            v.name.eq_ignore_ascii_case(raw) || v.has_alias(raw)
        })
    }

    /// The enum values `raw` names as the tracker spells them: every value
    /// whose id it is exactly, with every value it is an alias of, else every
    /// value whose id or one of whose aliases it matches case-insensitively.
    /// A value's `name` is devkit's alone.
    pub fn tracker_values(&self, raw: &str) -> Vec<&EnumValue> {
        let raw = raw.trim();
        self.by_id_or(raw, |v| v.id.eq_ignore_ascii_case(raw) || v.has_alias(raw))
    }

    /// The values `raw` is the exact id or an alias of when it is some
    /// value's exact id, else the values `loosely` matches.
    fn by_id_or(&self, raw: &str, loosely: impl Fn(&EnumValue) -> bool) -> Vec<&EnumValue> {
        let exact = self.values.iter().any(|v| v.id == raw);
        self.values
            .iter()
            .filter(|v| {
                if exact {
                    v.id == raw || v.has_alias(raw)
                } else {
                    loosely(v)
                }
            })
            .collect()
    }
}

/// What a field's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Number,
    Date,
    Enum,
    User,
    Issue,
}

impl FieldType {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::String => "string",
            FieldType::Number => "number",
            FieldType::Date => "date",
            FieldType::Enum => "enum",
            FieldType::User => "user",
            FieldType::Issue => "issue",
        }
    }
}

/// How strongly a ticket must carry a field: `devkit_config::Severity` with
/// `none` below it.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, JsonSchema, Deserialize, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum FieldLevel {
    #[default]
    None,
    Info,
    Warning,
    Error,
}

impl FieldLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldLevel::None => "none",
            FieldLevel::Info => "info",
            FieldLevel::Warning => "warning",
            FieldLevel::Error => "error",
        }
    }
}

impl From<Severity> for FieldLevel {
    fn from(s: Severity) -> Self {
        match s {
            Severity::Info => FieldLevel::Info,
            Severity::Warning => FieldLevel::Warning,
            Severity::Error => FieldLevel::Error,
        }
    }
}

/// A place on a tracker a field is written to.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(try_from = "PlaceSpec", into = "PlaceSpec")]
pub enum Place {
    /// GitHub and Linear: the issue's labels.
    Labels,
    /// GitHub: the issue's assignees.
    Assignees,
    /// Linear: the issue's assignee.
    Assignee,
    /// GitHub: the issue's milestone.
    Milestone,
    /// GitHub: the issue's type.
    IssueType,
    /// GitHub and Linear: the parent issue.
    Parent,
    /// GitHub and Linear: the issues blocking this one.
    BlockedBy,
    /// GitHub: membership of `[github] project`. Linear: the issue's project.
    Project,
    /// GitHub: a field of `[github] project`'s items, by id, with its name.
    ProjectField { id: String, name: String },
    /// Linear: the issue's team.
    Team,
    /// Linear: the issue's priority.
    Priority,
    /// Linear: the issue's estimate.
    Estimate,
    /// Linear: the issue's cycle.
    Cycle,
    /// Linear: the issue's project milestone.
    ProjectMilestone,
    /// Linear: the issue's due date.
    DueDate,
    /// Linear's workflow state. It parses only so the load can refuse it with
    /// a pointer to `[ticket.events]`.
    State,
}

impl Place {
    /// Whether `self` and `other` are one place: a project field by its id,
    /// whatever name each gives it.
    pub fn same_place(&self, other: &Place) -> bool {
        match (self, other) {
            (Place::ProjectField { id: x, .. }, Place::ProjectField { id: y, .. }) => x == y,
            _ => self == other,
        }
    }

    /// Whether `kind`'s tracker has this place.
    pub fn valid_for(&self, kind: TrackerKind) -> bool {
        use Place::*;
        match kind {
            TrackerKind::Github => matches!(
                self,
                Labels
                    | Assignees
                    | Milestone
                    | IssueType
                    | Parent
                    | BlockedBy
                    | Project
                    | ProjectField { .. }
            ),
            TrackerKind::Linear => matches!(
                self,
                Team | Labels
                    | Assignee
                    | Priority
                    | Estimate
                    | Cycle
                    | Project
                    | ProjectMilestone
                    | DueDate
                    | Parent
                    | BlockedBy
                    | State
            ),
            TrackerKind::None => false,
        }
    }

    /// Whether the place holds several values, so a `list` field may write it.
    pub fn takes_several(&self) -> bool {
        use Place::*;
        match self {
            Labels | Assignees | BlockedBy => true,
            Assignee
            | Milestone
            | IssueType
            | Parent
            | Project
            | ProjectField { .. }
            | Team
            | Priority
            | Estimate
            | Cycle
            | ProjectMilestone
            | DueDate
            | State => false,
        }
    }

    /// The place as `where` spells it.
    pub fn label(&self) -> String {
        match PlaceSpec::from(self.clone()) {
            PlaceSpec::Named(n) => n.as_str().to_string(),
            PlaceSpec::Field(f) => format!("project_field {} ({})", f.project_field, f.name),
        }
    }
}

/// `where` as written: a place name, or a project field table.
#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
#[serde(untagged)]
enum PlaceSpec {
    /// A place on the issue itself.
    Named(PlaceName),
    /// A field of `[github] project`'s items.
    Field(ProjectFieldSpec),
}

#[derive(Debug, Clone, Copy, JsonSchema, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PlaceName {
    Labels,
    Assignees,
    Assignee,
    Milestone,
    IssueType,
    Parent,
    BlockedBy,
    Project,
    Team,
    Priority,
    Estimate,
    Cycle,
    ProjectMilestone,
    DueDate,
    State,
}

impl PlaceName {
    fn as_str(self) -> &'static str {
        use PlaceName::*;
        match self {
            Labels => "labels",
            Assignees => "assignees",
            Assignee => "assignee",
            Milestone => "milestone",
            IssueType => "issue_type",
            Parent => "parent",
            BlockedBy => "blocked_by",
            Project => "project",
            Team => "team",
            Priority => "priority",
            Estimate => "estimate",
            Cycle => "cycle",
            ProjectMilestone => "project_milestone",
            DueDate => "due_date",
            State => "state",
        }
    }
}

/// `{ project_field = "<field id>", name = "<field name>" }`.
#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProjectFieldSpec {
    /// The project field's node id (`PVTSSF_...`).
    project_field: String,
    /// The field's name on the project, as it reads there.
    name: String,
}

impl TryFrom<PlaceSpec> for Place {
    type Error = String;

    fn try_from(spec: PlaceSpec) -> Result<Self, String> {
        use PlaceName as N;
        Ok(match spec {
            PlaceSpec::Field(f) => Place::ProjectField {
                id: f.project_field,
                name: f.name,
            },
            PlaceSpec::Named(n) => match n {
                N::Labels => Place::Labels,
                N::Assignees => Place::Assignees,
                N::Assignee => Place::Assignee,
                N::Milestone => Place::Milestone,
                N::IssueType => Place::IssueType,
                N::Parent => Place::Parent,
                N::BlockedBy => Place::BlockedBy,
                N::Project => Place::Project,
                N::Team => Place::Team,
                N::Priority => Place::Priority,
                N::Estimate => Place::Estimate,
                N::Cycle => Place::Cycle,
                N::ProjectMilestone => Place::ProjectMilestone,
                N::DueDate => Place::DueDate,
                N::State => Place::State,
            },
        })
    }
}

impl From<Place> for PlaceSpec {
    fn from(p: Place) -> Self {
        use PlaceName as N;
        PlaceSpec::Named(match p {
            Place::ProjectField { id, name } => {
                return PlaceSpec::Field(ProjectFieldSpec {
                    project_field: id,
                    name,
                });
            }
            Place::Labels => N::Labels,
            Place::Assignees => N::Assignees,
            Place::Assignee => N::Assignee,
            Place::Milestone => N::Milestone,
            Place::IssueType => N::IssueType,
            Place::Parent => N::Parent,
            Place::BlockedBy => N::BlockedBy,
            Place::Project => N::Project,
            Place::Team => N::Team,
            Place::Priority => N::Priority,
            Place::Estimate => N::Estimate,
            Place::Cycle => N::Cycle,
            Place::ProjectMilestone => N::ProjectMilestone,
            Place::DueDate => N::DueDate,
            Place::State => N::State,
        })
    }
}

/// One value of an `enum` field.
#[derive(Debug, Default, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnumValue {
    /// What agents pass to `ticket create` and `ticket edit`, matched
    /// case-insensitively. A tracker MCP call reaches the tracker unchanged,
    /// so it passes `id` or an alias instead.
    pub name: String,
    /// What devkit writes: a label's name, a milestone's title, a project
    /// option's id, a Linear id.
    pub id: String,
    /// The Linear teams the value is valid in, each by the team value's
    /// name, id or one of its aliases; any team when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teams: Vec<String>,
    /// Other spellings the tracker itself takes for the value, matched
    /// case-insensitively wherever `id` is, such as a Linear team's key and
    /// name or a label's name. `ticket fields discover` writes them; a
    /// hand-written value needs aliases for the spellings a tracker MCP
    /// takes (a GitHub issue type's name, a milestone's number, a Linear
    /// value's name), or an MCP call naming it that way is refused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

impl EnumValue {
    /// Whether `raw` is one of the value's aliases, ignoring case.
    pub fn has_alias(&self, raw: &str) -> bool {
        self.aliases.iter().any(|a| a.eq_ignore_ascii_case(raw))
    }
}

/// One conditional level.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WhenEntry {
    /// Conditions that must all hold, keyed by another declared field's name,
    /// or `caller` for who is filing (`"agent"` or `"human"`). A field's
    /// condition is `"*"` for set, `""` for unset, or one value or a list of
    /// values it must be one of.
    pub r#if: BTreeMap<String, Cond>,
    /// The level when every condition holds.
    pub level: FieldLevel,
}

/// A condition's accepted values.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Cond {
    One(String),
    Any(Vec<String>),
}

impl Cond {
    /// The accepted values as a slice.
    pub fn options(&self) -> &[String] {
        match self {
            Cond::One(v) => std::slice::from_ref(v),
            Cond::Any(v) => v,
        }
    }
}

/// What the load-time check needs besides the declarations.
pub(crate) struct FieldContext<'a> {
    pub status_field: &'a str,
    /// Whether `[github] project` is set.
    pub github_project: bool,
    pub template_variables: &'a std::collections::BTreeSet<String>,
    pub origin: &'a HashMap<String, PathBuf>,
}

/// Refuse a declaration no write could honour, naming the field. Unless
/// `in_use`, `kind` may be a tracker the repository does not use, whose
/// table a global config fills with levels alone, so a field lacking `type`
/// or `where` passes.
pub(crate) fn validate(
    kind: TrackerKind,
    decls: &BTreeMap<String, FieldDecl>,
    cx: &FieldContext<'_>,
    in_use: bool,
) -> Result<()> {
    let mut places: Vec<(&str, &Place)> = Vec::new();
    for (name, decl) in decls {
        let key = format!("[ticket.fields.{kind}.{name}]");
        if ISSUE_CONTEXT_KEYS.contains(&name.as_str()) || name == CALLER_CONDITION {
            bail!(
                "`{key}`{}: `{name}` is a name devkit supplies to the ticket templates or \
                 tests in `when`, so a field cannot take it. Rename the field.",
                note(cx, kind, name)
            );
        }
        if cx.template_variables.contains(name) {
            bail!(
                "`{key}`{} shares its name with `[templates.variables] {name}`; a field and a \
                 template variable are both passed as `--arg {name}=...`. Rename one.",
                note(cx, kind, name)
            );
        }
        for entry in &decl.when {
            for (cond, accepted) in &entry.r#if {
                if cond == CALLER_CONDITION {
                    if let Some(bad) = accepted
                        .options()
                        .iter()
                        .find(|v| !matches!(v.as_str(), "agent" | "human"))
                    {
                        bail!(
                            "`{key}`{}: `when` tests `caller = \"{bad}\"`; a caller is \
                             \"agent\" or \"human\".",
                            note(cx, kind, name)
                        );
                    }
                } else if !decls.contains_key(cond) {
                    bail!(
                        "`{key}`{}: `when` tests `{cond}`, which is not a declared \
                         `[ticket.fields.{kind}]` field.",
                        note(cx, kind, name)
                    );
                }
            }
        }
        let (Some(ty), Some(place)) = (decl.r#type, decl.r#where.as_ref()) else {
            if !in_use {
                continue;
            }
            let missing = match (decl.r#type, &decl.r#where) {
                (None, None) => "`type` and `where`",
                (None, Some(_)) => "`type`",
                _ => "`where`",
            };
            let level_from = cx
                .origin
                .get(&format!("ticket.fields.{kind}.{name}.level"))
                .map(|p| format!(" Its `level` is set in {}.", p.display()))
                .unwrap_or_default();
            bail!(
                "`{key}`{} has no {missing}, so devkit cannot write it.{level_from} Declare \
                 {missing} in the repository's devkit.toml, or remove the field.",
                note(cx, kind, name)
            );
        };
        let is_status = match place {
            Place::State => true,
            Place::ProjectField { name: f, .. } => f.trim().eq_ignore_ascii_case(cx.status_field),
            _ => false,
        };
        if is_status {
            bail!(
                "`{key}`{} is written to the ticket's status, which `[ticket.events]` moves. \
                 Configure status moves under `[ticket.events]` and remove the field.",
                note(cx, kind, name)
            );
        }
        if !place.valid_for(kind) {
            bail!(
                "`{key}`{}: `where = {}` is not a place on {kind} tickets.",
                note(cx, kind, name),
                place.label()
            );
        }
        if decl.list && !place.takes_several() {
            let prefix = format!("ticket.fields.{kind}.{name}.");
            let mut layers = cx.origin.iter().filter(|(k, _)| k.starts_with(&prefix));
            let list_from = cx
                .origin
                .get(&format!("{prefix}list"))
                .filter(|from| layers.any(|(_, p)| p != *from))
                .map(|from| format!(" `list` is set in {}.", from.display()))
                .unwrap_or_default();
            let several = match kind {
                TrackerKind::Github => "`labels`, `assignees` or `blocked_by`",
                _ => "`labels` or `blocked_by`",
            };
            bail!(
                "`{key}`{} has `list = true`, but `{}` holds one value, so a write would \
                 keep only the first.{list_from} Remove `list`, or write the field to \
                 {several}.",
                note(cx, kind, name),
                place.label()
            );
        }
        if in_use {
            let writable = match kind {
                TrackerKind::Github => github_writable(&key, ty, place, cx),
                TrackerKind::Linear => linear_writable(&key, ty, place),
                TrackerKind::None => Ok(()),
            };
            writable.map_err(|why| anyhow::anyhow!("`{key}`{} {why}", note(cx, kind, name)))?;
        }
        if let Some((other, _)) = places.iter().find(|(_, p)| p.same_place(place)) {
            bail!(
                "`{key}`{} and `[ticket.fields.{kind}.{other}]` are both written to `{}`; each \
                 place belongs to one field.",
                note(cx, kind, name),
                place.label()
            );
        }
        places.push((name, place));
    }
    Ok(())
}

/// Refuse an `enum` in `decls` with no `values` that can reach `error`,
/// which no ticket could satisfy, naming `ticket fields discover`, which
/// fills its values in.
pub(crate) fn reject_unfilled(
    kind: TrackerKind,
    decls: &BTreeMap<String, FieldDecl>,
    cx: &FieldContext<'_>,
) -> Result<()> {
    for (name, decl) in decls {
        if decl.r#type == Some(FieldType::Enum)
            && decl.values.is_empty()
            && decl.max_level() == FieldLevel::Error
        {
            let key = format!("[ticket.fields.{kind}.{name}]");
            bail!(
                "`{key}`{} is an `enum` with no `values` that can reach `error`, so no value \
                 could satisfy it. Run `devkit ticket fields discover` to list the tracker's \
                 values under `values` in `{key}`, or lower its level.",
                note(cx, kind, name)
            );
        }
    }
    Ok(())
}

/// Why Linear could not take a `ty` value at `place`, whose input takes ids
/// that only an `enum`'s values, or a `user`'s `[people]` entry, supply, or
/// issue identifiers that only an `issue` supplies.
fn linear_writable(key: &str, ty: FieldType, place: &Place) -> std::result::Result<(), String> {
    if matches!(place, Place::Parent | Place::BlockedBy) && ty != FieldType::Issue {
        return Err(format!(
            "has `type = \"{}\"`, but Linear's `{}` takes issue identifiers, which an \
             `issue` supplies. Change `type` in `{key}`.",
            ty.as_str(),
            place.label()
        ));
    }
    let wants = match place {
        Place::Assignee => "a `user` or `enum`",
        Place::Team | Place::Labels | Place::Cycle | Place::Project | Place::ProjectMilestone => {
            "an `enum`"
        }
        _ => return Ok(()),
    };
    let fits = ty == FieldType::Enum || (ty == FieldType::User && *place == Place::Assignee);
    if fits {
        return Ok(());
    }
    Err(format!(
        "has `type = \"{}\"`, but Linear's `{}` takes ids, which {wants} supplies. Change \
         `type` in `{key}`.",
        ty.as_str(),
        place.label()
    ))
}

/// Why GitHub could not take a `ty` value at `place`, which would otherwise
/// fail only after `ticket create` filed the issue.
fn github_writable(
    key: &str,
    ty: FieldType,
    place: &Place,
    cx: &FieldContext<'_>,
) -> std::result::Result<(), String> {
    let on_project = matches!(place, Place::Project | Place::ProjectField { .. });
    if on_project && !cx.github_project {
        return Err(format!(
            "is written to `{}`, which needs `[github] project`. Set `[github] project`, or \
             remove the field.",
            place.label()
        ));
    }
    let fits = match place {
        Place::ProjectField { .. } => !matches!(ty, FieldType::User | FieldType::Issue),
        Place::Parent | Place::BlockedBy => ty == FieldType::Issue,
        _ => true,
    };
    if fits {
        return Ok(());
    }
    let wants = match place {
        Place::ProjectField { .. } => "a `string`, `number`, `date` or `enum`",
        _ => "an `issue`",
    };
    Err(format!(
        "has `type = \"{}\"`, but `{}` holds {wants}. Change `type` in `{key}`.",
        ty.as_str(),
        place.label()
    ))
}

/// ` (declared in <layer>)` for the first layer provenance knows wrote a key
/// of the field.
fn note(cx: &FieldContext<'_>, kind: TrackerKind, name: &str) -> String {
    let prefix = format!("ticket.fields.{kind}.{name}.");
    let mut layers: Vec<&PathBuf> = cx
        .origin
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(_, p)| p)
        .collect();
    layers.sort();
    layers.dedup();
    match layers.as_slice() {
        [] => String::new(),
        ps => format!(
            " (declared in {})",
            ps.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    const ILLUSTRATION: &str = r#"
[ticket.fields.github.priority]
type        = "enum"
where       = { project_field = "PVTSSF_x", name = "Priority" }
values      = [{ name = "P0", id = "f75ad846" }, { name = "P1", id = "47fc9ee4" }, { name = "P2", id = "98236657" }]
default     = "P2"
level       = "error"
description = "P0 drop everything, P1 this cycle, P2 backlog"

[ticket.fields.github.milestone]
type   = "enum"
where  = "milestone"
values = [{ name = "v1", id = "v1" }]
level  = "none"
when   = [{ if = { project = "*" }, level = "warning" }]
"#;

    #[test]
    fn a_declaration_parses_every_shape() {
        let cfg = Config::parse(ILLUSTRATION).unwrap();
        let gh = &cfg.ticket.fields.github;
        let p = &gh["priority"];
        assert_eq!(p.values[1].name, "P1");
        assert_eq!(p.default.as_deref(), Some("P2"));
        assert_eq!(
            p.r#where,
            Some(Place::ProjectField {
                id: "PVTSSF_x".into(),
                name: "Priority".into()
            })
        );
        let m = &gh["milestone"];
        assert_eq!(m.when[0].level, FieldLevel::Warning);
        assert_eq!(m.when[0].r#if["project"], Cond::One("*".into()));
        assert_eq!(m.r#where, Some(Place::Milestone));
    }

    #[test]
    fn levels_order_none_below_error() {
        assert!(FieldLevel::None < FieldLevel::Info);
        assert!(FieldLevel::Info < FieldLevel::Warning);
        assert!(FieldLevel::Warning < FieldLevel::Error);
        assert_eq!(FieldLevel::from(Severity::Warning), FieldLevel::Warning);
    }

    #[test]
    fn max_level_takes_the_highest_when_entry() {
        let cfg = Config::parse(
            r#"
[ticket.fields.github.estimate]
type = "number"
where = { project_field = "PVTF_e", name = "Estimate" }
level = "none"
when = [{ if = { caller = "agent" }, level = "error" }, { if = { caller = "human" }, level = "info" }]
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.ticket.fields.github["estimate"].max_level(),
            FieldLevel::Error
        );
    }

    #[test]
    fn team_never_allows_none() {
        let cfg = Config::parse(
            "[ticket.fields.linear.team]\ntype = \"enum\"\nwhere = \"team\"\nallow_none = true\n",
        )
        .unwrap();
        assert!(!cfg.ticket.fields.linear["team"].allows_none());
        assert!(FieldDecl::default().allows_none());
    }

    #[test]
    fn an_unknown_place_is_a_parse_error() {
        assert!(
            Config::parse("[ticket.fields.github.x]\ntype = \"string\"\nwhere = \"nowhere\"\n")
                .is_err()
        );
    }
}
