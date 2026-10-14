//! Writing and reading back a Linear issue's declared ticket fields: the
//! `issueCreate` and `issueUpdate` inputs (team, labels, assignee, priority,
//! estimate, cycle, project, project milestone, due date, parent) and
//! blocked-by relations.
//!
//! Split like `linear_status.rs`: query and input builders and response
//! parsers, tested over recorded responses, and a [`LinearFieldWriter`] over
//! them whose transport a test can replace.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result, bail, ensure};
use devkit_config::{FieldDecl, FieldType, Person, Place};
use serde_json::{Map, Value as Json, json};
use uuid::Uuid;

use super::linear::{graphql_errors, parse_id};
use crate::fields::{Current, Resolved, Value};

/// The namespace [`create_id`] hashes in, so its ids collide with no other
/// derived uuid.
const CREATE_NAMESPACE: Uuid = Uuid::from_u128(0x6b1d_3c2e_8f4a_4e57_9a61_0d2c_7e5b_4f93);

/// The id `ticket create` passes to `issueCreate` for a ticket with this team,
/// title and body. The same three always give the same id, so a create retried
/// after a timeout collides with the ticket the first attempt filed instead of
/// filing a second.
///
/// Linear takes a client id in UUID v4 format, so the hash's bytes carry the
/// v4 version and variant bits.
pub fn create_id(team_id: &str, title: &str, body: &str) -> Uuid {
    let name = json!([team_id, title, body]).to_string();
    let hashed = Uuid::new_v5(&CREATE_NAMESPACE, name.as_bytes());
    uuid::Builder::from_random_bytes(hashed.into_bytes()).into_uuid()
}

/// What a Linear issue holds at each place devkit writes, from [`read_query`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LinearIssue {
    pub uuid: String,
    pub identifier: String,
    pub url: String,
    pub title: String,
    /// The team's id and key.
    pub team: Option<(String, String)>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    /// `None` for Linear's `0`, no priority.
    pub priority: Option<i64>,
    pub estimate: Option<f64>,
    pub cycle: Option<String>,
    pub project: Option<String>,
    pub project_milestone: Option<String>,
    pub due_date: Option<String>,
    /// The parent's identifier.
    pub parent: Option<String>,
    /// Each blocker's identifier and the id of the relation that records it.
    pub blocked_by: Vec<(String, String)>,
    /// Whether the issue is archived or in the trash.
    pub archived: bool,
}

/// The issue fields every read selects.
const ISSUE_FIELDS: &str = "id identifier url title team { id key } labels(first: 100) { nodes { id } } \
     assignee { id } priority estimate cycle { id } project { id } projectMilestone { id } dueDate \
     parent { identifier } inverseRelations(first: 100) { nodes { id type issue { identifier } } } \
     archivedAt";

/// Which issue a [`read_query`] reads.
#[derive(Debug, Clone, Copy)]
pub enum Lookup<'a> {
    /// A `TEAM-NUMBER` identifier.
    Identifier(&'a str),
    /// The issue's own id, as [`create_id`] gives it, archived or trashed
    /// issues included.
    Uuid(Uuid),
    /// No issue: only the references.
    None,
}

fn by_identifier(id: &str) -> Result<String> {
    let (team, num) = parse_id(id).with_context(|| format!("`{id}` is not a Linear issue id"))?;
    Ok(format!(
        "filter: {{ team: {{ key: {{ eq: \"{team}\" }} }}, number: {{ eq: {num} }} }}"
    ))
}

/// One query reading the issue `lookup` names and the ids of the issues
/// `refs` names, which parent and blocked-by writes need.
pub fn read_query(lookup: Lookup<'_>, refs: &BTreeSet<String>) -> Result<String> {
    let mut parts = Vec::new();
    match lookup {
        Lookup::Identifier(id) => parts.push(format!(
            "issue: issues({}) {{ nodes {{ {ISSUE_FIELDS} }} }}",
            by_identifier(id)?
        )),
        Lookup::Uuid(u) => parts.push(format!(
            "issue: issues(filter: {{ id: {{ eq: \"{u}\" }} }}, includeArchived: true) {{ nodes {{ {ISSUE_FIELDS} }} }}"
        )),
        Lookup::None => {}
    }
    for (i, r) in refs.iter().enumerate() {
        parts.push(format!(
            "ref{i}: issues({}) {{ nodes {{ id identifier }} }}",
            by_identifier(r)?
        ));
    }
    Ok(format!("query {{ {} }}", parts.join(" ")))
}

fn text(v: &Json, pointer: &str) -> Option<String> {
    v.pointer(pointer)
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// The issue a [`read_query`] response read, `None` when Linear has none.
pub fn parse_issue(resp: &Json) -> Result<Option<LinearIssue>> {
    let Some(n) = resp.pointer("/data/issue/nodes/0") else {
        return Ok(None);
    };
    Ok(Some(LinearIssue {
        uuid: text(n, "/id").context("issue without an id")?,
        identifier: text(n, "/identifier").context("issue without an identifier")?,
        url: text(n, "/url").unwrap_or_default(),
        title: n["title"].as_str().unwrap_or_default().to_string(),
        team: text(n, "/team/id").map(|id| (id, text(n, "/team/key").unwrap_or_default())),
        labels: n
            .pointer("/labels/nodes")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| text(l, "/id"))
            .collect(),
        assignee: text(n, "/assignee/id"),
        priority: n["priority"].as_f64().map(|p| p as i64).filter(|p| *p != 0),
        estimate: n["estimate"].as_f64(),
        cycle: text(n, "/cycle/id"),
        project: text(n, "/project/id"),
        project_milestone: text(n, "/projectMilestone/id"),
        due_date: text(n, "/dueDate"),
        parent: text(n, "/parent/identifier"),
        blocked_by: n
            .pointer("/inverseRelations/nodes")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter(|r| r["type"] == "blocks")
            .filter_map(|r| Some((text(r, "/issue/identifier")?, text(r, "/id")?)))
            .collect(),
        archived: text(n, "/archivedAt").is_some(),
    }))
}

/// Identifier to id for each reference a [`read_query`] response resolved.
pub fn parse_refs(resp: &Json) -> BTreeMap<String, String> {
    resp["data"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, _)| k.starts_with("ref"))
        .filter_map(|(_, v)| {
            let n = v.pointer("/nodes/0")?;
            Some((text(n, "/identifier")?, text(n, "/id")?))
        })
        .collect()
}

fn number_text(n: f64) -> String {
    format!("{n}")
}

impl LinearIssue {
    /// What `place` holds, as the ids devkit writes there.
    pub fn ids(&self, place: &Place) -> Vec<String> {
        let one = |v: &Option<String>| v.iter().cloned().collect();
        match place {
            Place::Team => self.team.iter().map(|(id, _)| id.clone()).collect(),
            Place::Labels => self.labels.clone(),
            Place::Assignee => one(&self.assignee),
            Place::Priority => self.priority.iter().map(i64::to_string).collect(),
            Place::Estimate => self.estimate.map(number_text).into_iter().collect(),
            Place::Cycle => one(&self.cycle),
            Place::Project => one(&self.project),
            Place::ProjectMilestone => one(&self.project_milestone),
            Place::DueDate => one(&self.due_date),
            Place::Parent => one(&self.parent),
            Place::BlockedBy => self.blocked_by.iter().map(|(id, _)| id.clone()).collect(),
            Place::Assignees
            | Place::Milestone
            | Place::IssueType
            | Place::ProjectField { .. }
            | Place::State => Vec::new(),
        }
    }

    /// The fields' present values by field name, as names: an enum value's
    /// declared name, a `user`'s `[people]` alias, else what Linear holds.
    pub fn current(
        &self,
        decls: &BTreeMap<String, FieldDecl>,
        people: &HashMap<String, Person>,
    ) -> Current {
        let mut out = BTreeMap::new();
        for (name, decl) in decls {
            let Some(place) = &decl.r#where else { continue };
            let names: Vec<String> = self
                .ids(place)
                .into_iter()
                .map(|id| match decl.r#type {
                    Some(FieldType::User) => alias_of(people, &id),
                    _ => value_name(decl, &id),
                })
                .collect();
            if !names.is_empty() {
                out.insert(name.clone(), names);
            }
        }
        Current(out)
    }

    /// The name of the issue's team as the team field declares it, which
    /// scopes team-bound values: the declared value with the team's id, else
    /// the team's key.
    pub fn team_name(&self, decls: &BTreeMap<String, FieldDecl>) -> Option<String> {
        let (id, key) = self.team.as_ref()?;
        let declared = decls
            .values()
            .filter(|d| d.r#where == Some(Place::Team))
            .flat_map(|d| &d.values)
            .find(|v| v.id == *id);
        Some(declared.map_or_else(|| key.clone(), |v| v.name.clone()))
    }
}

/// The `[people]` alias whose Linear id is `id`, else `id`.
fn alias_of(people: &HashMap<String, Person>, id: &str) -> String {
    people
        .iter()
        .filter(|(_, p)| p.linear.as_deref() == Some(id))
        .map(|(alias, _)| alias)
        .min()
        .map_or_else(|| id.to_string(), Clone::clone)
}

/// The declared name of the value whose id is `id`, else `id`.
fn value_name(decl: &FieldDecl, id: &str) -> String {
    decl.values
        .iter()
        .find(|v| v.id == id)
        .map_or_else(|| id.to_string(), |v| v.name.clone())
}

/// The ids a field's resolved values are written as.
pub fn written_ids(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .map(|v| match v {
            Value::Enum { id, .. } | Value::User { id, .. } => id.clone(),
            Value::Number(n) => number_text(*n),
            Value::Text(s) | Value::Date(s) | Value::Issue(s) => s.clone(),
        })
        .collect()
}

/// The issue identifiers the parent and blocked-by fields of `resolved` name.
pub fn issue_refs(decls: &BTreeMap<String, FieldDecl>, resolved: &Resolved) -> BTreeSet<String> {
    resolved
        .fields
        .iter()
        .filter(|(name, _)| {
            matches!(
                decls.get(*name).and_then(|d| d.r#where.as_ref()),
                Some(Place::Parent | Place::BlockedBy)
            )
        })
        .flat_map(|(_, values)| written_ids(values))
        .collect()
}

/// A whole number for an `Int` input, refusing a fraction rather than
/// rounding it.
fn whole(place: &Place, raw: &str) -> Result<i64> {
    raw.parse::<f64>()
        .ok()
        .filter(|n| n.fract() == 0.0 && n.is_finite())
        .map(|n| n as i64)
        .with_context(|| {
            format!(
                "Linear's {} takes a whole number, not `{raw}`",
                place.label()
            )
        })
}

/// The `issueCreate`/`issueUpdate` input key and value that write `want` at
/// `place`, an empty `want` clearing it. `None` for blocked-by, which is a
/// relation rather than an input.
fn input_entry(
    place: &Place,
    want: &[String],
    refs: &BTreeMap<String, String>,
) -> Result<Option<(&'static str, Json)>> {
    let first = want.first();
    let id = |key: &'static str| Ok(Some((key, first.map_or(Json::Null, |v| json!(v)))));
    match place {
        Place::Team => {
            let team = first.context("a Linear ticket's team cannot be cleared")?;
            Ok(Some(("teamId", json!(team))))
        }
        Place::Labels => Ok(Some(("labelIds", json!(want)))),
        Place::Assignee => id("assigneeId"),
        Place::Cycle => id("cycleId"),
        Place::Project => id("projectId"),
        Place::ProjectMilestone => id("projectMilestoneId"),
        Place::DueDate => id("dueDate"),
        Place::Priority => Ok(Some((
            "priority",
            json!(first.map(|p| whole(place, p)).transpose()?.unwrap_or(0)),
        ))),
        Place::Estimate => Ok(Some((
            "estimate",
            first
                .map(|e| whole(place, e))
                .transpose()?
                .map_or(Json::Null, |e| json!(e)),
        ))),
        Place::Parent => match first {
            Some(p) => {
                let id = refs
                    .get(p)
                    .with_context(|| format!("Linear has no issue {p}"))?;
                Ok(Some(("parentId", json!(id))))
            }
            None => Ok(Some(("parentId", Json::Null))),
        },
        Place::BlockedBy => Ok(None),
        Place::Assignees
        | Place::Milestone
        | Place::IssueType
        | Place::ProjectField { .. }
        | Place::State => bail!("`{}` is not a place on Linear issues", place.label()),
    }
}

/// The input entries writing every field of `resolved` and every `--none`
/// clear, blocked-by aside.
fn field_inputs(
    decls: &BTreeMap<String, FieldDecl>,
    resolved: &Resolved,
    refs: &BTreeMap<String, String>,
) -> Result<Map<String, Json>> {
    let mut out = Map::new();
    for (name, values) in resolved.written() {
        let Some(place) = decls.get(name).and_then(|d| d.r#where.as_ref()) else {
            continue;
        };
        if let Some((key, value)) =
            input_entry(place, &written_ids(values), refs).with_context(|| format!("`{name}`"))?
        {
            out.insert(key.to_string(), value);
        }
    }
    Ok(out)
}

/// The `IssueCreateInput` filing a ticket with client id `id`, `title`, the
/// description `body`, and every field of `resolved` the create takes. A
/// `--none` writes nothing on a create.
pub fn create_input(
    id: Uuid,
    title: &str,
    body: &str,
    decls: &BTreeMap<String, FieldDecl>,
    resolved: &Resolved,
    refs: &BTreeMap<String, String>,
) -> Result<Json> {
    let given = Resolved {
        fields: resolved.fields.clone(),
        nones: BTreeSet::new(),
    };
    let mut input = field_inputs(decls, &given, refs)?;
    ensure!(
        input.contains_key("teamId"),
        "a Linear ticket needs a team, and none was given"
    );
    input.insert("id".into(), json!(id.to_string()));
    input.insert("title".into(), json!(title));
    input.insert("description".into(), json!(body));
    Ok(Json::Object(input))
}

/// The `IssueUpdateInput` writing exactly the fields `resolved` names, and
/// the title and description when given.
pub fn update_input(
    title: Option<&str>,
    body: Option<&str>,
    decls: &BTreeMap<String, FieldDecl>,
    resolved: &Resolved,
    refs: &BTreeMap<String, String>,
) -> Result<Json> {
    let mut input = field_inputs(decls, resolved, refs)?;
    if let Some(t) = title {
        input.insert("title".into(), json!(t));
    }
    if let Some(b) = body {
        input.insert("description".into(), json!(b));
    }
    Ok(Json::Object(input))
}

pub const CREATE_MUTATION: &str =
    "mutation($input: IssueCreateInput!) { issueCreate(input: $input) { success } }";
pub const UPDATE_MUTATION: &str = "mutation($id: String!, $input: IssueUpdateInput!) { issueUpdate(id: $id, input: $input) { success } }";
pub const RELATE_MUTATION: &str = "mutation($input: IssueRelationCreateInput!) { issueRelationCreate(input: $input) { success } }";
pub const UNRELATE_MUTATION: &str =
    "mutation($id: String!) { issueRelationDelete(id: $id) { success } }";

/// The `issueRelationCreate` input recording that `blocker` blocks `issue`,
/// both issue ids.
pub fn blocks_input(blocker: &str, issue: &str) -> Json {
    json!({ "issueId": blocker, "relatedIssueId": issue, "type": "blocks" })
}

/// A ticket `issueCreate` filed, or that an earlier attempt with the same id
/// already had.
#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    pub issue: LinearIssue,
    /// Whether the ticket existed before this create: an earlier attempt
    /// filed it.
    pub existed: bool,
}

/// Posts a GraphQL request body to Linear, labelled for timing.
pub type Transport = dyn Fn(Json, &str) -> Result<Json> + Send + Sync;

/// The ticket fields of Linear issues.
pub struct LinearFieldWriter {
    post: Box<Transport>,
}

impl LinearFieldWriter {
    /// Through Linear's API with the key `key`.
    pub fn new(key: String) -> Self {
        Self::with_transport(move |body, detail| super::linear::send_graphql(body, &key, detail))
    }

    /// Through `post`, which a test answers from recorded responses.
    pub fn with_transport(
        post: impl Fn(Json, &str) -> Result<Json> + Send + Sync + 'static,
    ) -> Self {
        LinearFieldWriter {
            post: Box::new(post),
        }
    }

    fn call(&self, query: &str, variables: Json, detail: &str) -> Result<Json> {
        graphql_errors((self.post)(
            json!({ "query": query, "variables": variables }),
            detail,
        )?)
    }

    fn mutate(&self, query: &str, variables: Json, op: &str) -> Result<()> {
        let resp = self.call(query, variables, op)?;
        ensure!(
            resp.pointer(&format!("/data/{op}/success")) == Some(&Json::Bool(true)),
            "Linear did not confirm `{op}`"
        );
        Ok(())
    }

    /// The id of each issue `refs` names, refusing one Linear does not have.
    /// Makes no request when `refs` is empty.
    pub fn refs(&self, refs: &BTreeSet<String>) -> Result<BTreeMap<String, String>> {
        if refs.is_empty() {
            return Ok(BTreeMap::new());
        }
        let found =
            parse_refs(&self.call(&read_query(Lookup::None, refs)?, json!({}), "issue refs")?);
        let missing: Vec<&str> = refs
            .iter()
            .filter(|r| !found.contains_key(*r))
            .map(String::as_str)
            .collect();
        ensure!(
            missing.is_empty(),
            "Linear has no issue {}",
            missing.join(", ")
        );
        Ok(found)
    }

    fn lookup(&self, lookup: Lookup<'_>) -> Result<Option<LinearIssue>> {
        parse_issue(&self.call(
            &read_query(lookup, &BTreeSet::new())?,
            json!({}),
            "issue fields",
        )?)
    }

    /// Read what issue `identifier` holds at every place devkit writes.
    pub fn read(&self, identifier: &str) -> Result<LinearIssue> {
        self.lookup(Lookup::Identifier(identifier))?
            .with_context(|| format!("Linear has no issue {identifier}"))
    }

    /// Read issue `id` like [`Self::read`], by the id that survives a move
    /// to another team, which renames the identifier.
    pub fn read_by_id(&self, id: &str) -> Result<LinearIssue> {
        let parsed: Uuid = id
            .parse()
            .with_context(|| format!("`{id}` is not a Linear issue id"))?;
        self.lookup(Lookup::Uuid(parsed))?
            .with_context(|| format!("Linear has no issue {id}"))
    }

    /// File the ticket `input` describes, which carries its client id. When
    /// the create fails or goes unanswered, the id is looked up: a ticket
    /// that has it was filed by this or an earlier attempt and is returned,
    /// never filed twice.
    pub fn create(&self, input: &Json) -> Result<Created> {
        let id: Uuid = input["id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .context("a create input without a client id")?;
        let created = self.mutate(CREATE_MUTATION, json!({ "input": input }), "issueCreate");
        let found = self.lookup(Lookup::Uuid(id));
        match (created, found) {
            (Ok(()), Ok(Some(issue))) => Ok(Created {
                issue,
                existed: false,
            }),
            (Ok(()), Ok(None)) => bail!("Linear confirmed the create, but has no issue {id}"),
            (Err(_), Ok(Some(issue))) => Ok(Created {
                issue,
                existed: true,
            }),
            (Err(e), Ok(None)) if crate::http::is_unreachable(&e) => Err(e.context(format!(
                "Linear did not answer the create and has no ticket under its id {id}; rerun \
                 the same `ticket create`: it passes the same id, so it files at most one ticket"
            ))),
            (Err(e), Ok(None)) => Err(e.context("Linear did not file the ticket")),
            (created, Err(read)) => Err(read.context(format!(
                "Linear may have filed the ticket ({}), and reading it back failed; rerun the \
                 same `ticket create`: it passes the same id {id}, so Linear returns that \
                 ticket rather than filing a second",
                match created {
                    Ok(()) => "it confirmed the create".to_string(),
                    Err(e) => format!("the create failed: {e:#}"),
                }
            ))),
        }
    }

    /// Write `input` onto the issue with id `uuid`. An empty input makes no
    /// request.
    pub fn update(&self, uuid: &str, input: &Json) -> Result<()> {
        if input.as_object().is_none_or(Map::is_empty) {
            return Ok(());
        }
        self.mutate(
            UPDATE_MUTATION,
            json!({ "id": uuid, "input": input }),
            "issueUpdate",
        )
    }

    /// Make the issues blocking `issue` exactly the identifiers `want`,
    /// whose ids `refs` holds.
    pub fn set_blocked_by(
        &self,
        issue: &LinearIssue,
        want: &[String],
        refs: &BTreeMap<String, String>,
    ) -> Result<()> {
        for w in want {
            if issue.blocked_by.iter().any(|(id, _)| id == w) {
                continue;
            }
            let blocker = refs
                .get(w)
                .with_context(|| format!("Linear has no issue {w}"))?;
            self.mutate(
                RELATE_MUTATION,
                json!({ "input": blocks_input(blocker, &issue.uuid) }),
                "issueRelationCreate",
            )?;
        }
        for (id, relation) in &issue.blocked_by {
            if !want.contains(id) {
                self.mutate(
                    UNRELATE_MUTATION,
                    json!({ "id": relation }),
                    "issueRelationDelete",
                )?;
            }
        }
        Ok(())
    }

    /// Each field of `resolved` whose read-back `after` differs from what was
    /// written, with what it reads as. A list compares as a set, and a
    /// `--none` field must read empty.
    pub fn verify(
        decls: &BTreeMap<String, FieldDecl>,
        resolved: &Resolved,
        after: &LinearIssue,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (name, values) in resolved.written() {
            let Some(decl) = decls.get(name) else {
                continue;
            };
            let Some(place) = &decl.r#where else { continue };
            let want: BTreeSet<String> = written_ids(values).into_iter().collect();
            let have: BTreeSet<String> = after.ids(place).into_iter().collect();
            if want != have {
                let reads: Vec<String> = have.iter().map(|id| value_name(decl, id)).collect();
                out.push((
                    name.clone(),
                    if reads.is_empty() {
                        "nothing".to_string()
                    } else {
                        reads.join(", ")
                    },
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use devkit_config::{Config, TrackerKind};

    use super::*;
    use crate::{
        fields::{Given, resolve},
        tracker::fixture,
    };

    const DECLS: &str = r#"
[ticket.fields.linear.team]
type   = "enum"
where  = "team"
values = [{ name = "ENG", id = "team-eng" }]

[ticket.fields.linear.labels]
type   = "enum"
list   = true
where  = "labels"
values = [{ name = "bug", id = "lbl-bug" }, { name = "infra", id = "lbl-infra" }]

[ticket.fields.linear.assignee]
type  = "user"
where = "assignee"

[ticket.fields.linear.priority]
type   = "enum"
where  = "priority"
values = [{ name = "urgent", id = "1" }, { name = "high", id = "2" }]

[ticket.fields.linear.estimate]
type  = "number"
where = "estimate"

[ticket.fields.linear.cycle]
type   = "enum"
where  = "cycle"
values = [{ name = "c7", id = "cyc-7" }]

[ticket.fields.linear.project]
type   = "enum"
where  = "project"
values = [{ name = "devkit", id = "prj-devkit" }]

[ticket.fields.linear.milestone]
type   = "enum"
where  = "project_milestone"
values = [{ name = "beta", id = "ms-beta" }]

[ticket.fields.linear.due]
type  = "date"
where = "due_date"

[ticket.fields.linear.parent]
type  = "issue"
where = "parent"

[ticket.fields.linear.blocked_by]
type  = "issue"
list  = true
where = "blocked_by"

[people.ada]
linear = "user-ada"
"#;

    fn config() -> Config {
        Config::parse(DECLS).unwrap()
    }

    fn resolved(values: &[(&str, &str)], nones: &[&str]) -> Resolved {
        let cfg = config();
        let given = Given {
            values: values
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            nones: nones.iter().map(|n| n.to_string()).collect(),
        };
        resolve(
            &cfg.ticket.fields.linear,
            TrackerKind::Linear,
            &given,
            &cfg.people,
            Some("ENG"),
        )
        .unwrap()
    }

    /// A writer answering each request with the next of `answers`, and the
    /// request bodies it was sent.
    fn scripted(answers: Vec<Result<Json>>) -> (LinearFieldWriter, Arc<Mutex<Vec<Json>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let answers = Mutex::new(answers.into_iter());
        let log = sent.clone();
        let writer = LinearFieldWriter::with_transport(move |body, _| {
            log.lock().unwrap().push(body);
            answers
                .lock()
                .unwrap()
                .next()
                .unwrap_or_else(|| bail!("no answer scripted"))
        });
        (writer, sent)
    }

    #[test]
    fn create_id_is_stable_for_the_same_team_title_and_body() {
        let a = create_id("team-eng", "Fix it", "body");
        assert_eq!(a, create_id("team-eng", "Fix it", "body"));
        assert_ne!(a, create_id("team-ops", "Fix it", "body"));
        assert_ne!(a, create_id("team-eng", "Fix it!", "body"));
        assert_ne!(a, create_id("team-eng", "Fix it", "body!"));
        assert_ne!(
            create_id("a", "b\0c", ""),
            create_id("a", "b", "c\0"),
            "the parts are kept apart"
        );
        assert_eq!(a.get_version_num(), 4, "Linear takes a UUID v4");
        assert_eq!(a.get_variant(), uuid::Variant::RFC4122);
    }

    #[test]
    fn create_mutation_carries_the_client_id_and_every_create_input() {
        let r = resolved(
            &[
                ("team", "eng"),
                ("labels", "bug, infra"),
                ("assignee", "ada"),
                ("priority", "High"),
                ("estimate", "3"),
                ("cycle", "c7"),
                ("project", "devkit"),
                ("milestone", "beta"),
                ("due", "2026-11-01"),
                ("parent", "ENG-1"),
                ("blocked_by", "ENG-2"),
            ],
            &[],
        );
        let id = create_id("team-eng", "T", "B");
        let refs = BTreeMap::from([
            ("ENG-1".to_string(), "uuid-eng-1".to_string()),
            ("ENG-2".to_string(), "uuid-eng-2".to_string()),
        ]);
        let input = create_input(id, "T", "B", &config().ticket.fields.linear, &r, &refs).unwrap();
        assert_eq!(
            input,
            json!({
                "id": id.to_string(),
                "title": "T",
                "description": "B",
                "teamId": "team-eng",
                "labelIds": ["lbl-bug", "lbl-infra"],
                "assigneeId": "user-ada",
                "priority": 2,
                "estimate": 3,
                "cycleId": "cyc-7",
                "projectId": "prj-devkit",
                "projectMilestoneId": "ms-beta",
                "dueDate": "2026-11-01",
                "parentId": "uuid-eng-1",
            }),
            "blocked-by is a relation, written after the create"
        );

        let (writer, sent) = scripted(vec![
            Ok(json!({ "data": { "issueCreate": { "success": true } } })),
            Ok(fixture("linear_fields_read.json")),
        ]);
        let created = writer.create(&input).unwrap();
        assert!(!created.existed);
        assert_eq!(created.issue.identifier, "ENG-42");
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0]["query"], CREATE_MUTATION);
        assert_eq!(sent[0]["variables"]["input"], input);
        assert!(
            sent[1]["query"]
                .as_str()
                .unwrap()
                .contains(&format!("id: {{ eq: \"{id}\" }}")),
            "the read-back finds the ticket by its client id: {}",
            sent[1]["query"]
        );
    }

    #[test]
    fn a_fraction_for_a_whole_number_input_is_refused_before_any_request() {
        let r = resolved(&[("team", "ENG"), ("estimate", "2.5")], &[]);
        let e = create_input(
            create_id("t", "T", "B"),
            "T",
            "B",
            &config().ticket.fields.linear,
            &r,
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("whole number"), "{e:#}");
    }

    #[test]
    fn an_existing_id_is_read_back_not_duplicated() {
        let id = create_id("team-eng", "T", "B");
        let input = create_input(
            id,
            "T",
            "B",
            &config().ticket.fields.linear,
            &resolved(&[("team", "ENG")], &[]),
            &BTreeMap::new(),
        )
        .unwrap();
        let by_id = format!("id: {{ eq: \"{id}\" }}");
        let mut issue = fixture("linear_fields_read.json");
        issue["data"]["issue"]["nodes"][0]["id"] = json!(id.to_string());
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let filter = by_id.clone();
        let writer = LinearFieldWriter::with_transport(move |body, _| {
            log.lock().unwrap().push(body.clone());
            let query = body["query"].as_str().unwrap_or_default();
            Ok(if query.contains("issueCreate(") {
                fixture("linear_fields_create_conflict.json")
            } else if query.contains(&filter) {
                issue.clone()
            } else {
                json!({ "data": { "issue": { "nodes": [] } } })
            })
        });
        let created = writer.create(&input).unwrap();
        assert!(created.existed);
        assert_eq!(created.issue.identifier, "ENG-42");
        assert_eq!(created.issue.uuid, id.to_string());
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2, "one create, one read, no second create");
        assert!(
            sent[1]["query"].as_str().unwrap().contains(&by_id),
            "the read is by the client id: {}",
            sent[1]["query"]
        );
    }

    #[test]
    fn a_failed_create_with_no_ticket_under_its_id_is_an_error() {
        let input = create_input(
            create_id("team-eng", "T", "B"),
            "T",
            "B",
            &config().ticket.fields.linear,
            &resolved(&[("team", "ENG")], &[]),
            &BTreeMap::new(),
        )
        .unwrap();
        let (writer, _) = scripted(vec![
            Ok(json!({ "errors": [{ "message": "Entity not found: Team" }] })),
            Ok(json!({ "data": { "issue": { "nodes": [] } } })),
        ]);
        let e = writer.create(&input).unwrap_err();
        assert!(format!("{e:#}").contains("Entity not found: Team"), "{e:#}");

        let (writer, _) = scripted(vec![bail_err("timed out"), bail_err("timed out")]);
        let e = format!("{:#}", writer.create(&input).unwrap_err());
        assert!(
            e.contains("may have filed") && e.contains("rerun the same `ticket create`"),
            "{e}"
        );
    }

    fn bail_err(msg: &str) -> Result<Json> {
        Err(anyhow::anyhow!("{msg}"))
    }

    #[test]
    fn blocked_by_becomes_issue_relation_create_of_type_blocks() {
        let issue = parse_issue(&fixture("linear_fields_read.json"))
            .unwrap()
            .unwrap();
        assert_eq!(issue.blocked_by, [(
            "ENG-3".to_string(),
            "rel-3".to_string()
        )]);
        let refs = BTreeMap::from([("ENG-2".to_string(), "uuid-eng-2".to_string())]);
        let (writer, sent) = scripted(vec![
            Ok(json!({ "data": { "issueRelationCreate": { "success": true } } })),
            Ok(json!({ "data": { "issueRelationDelete": { "success": true } } })),
        ]);
        writer
            .set_blocked_by(&issue, &["ENG-2".to_string()], &refs)
            .unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(sent[0]["query"], RELATE_MUTATION);
        assert_eq!(
            sent[0]["variables"]["input"],
            json!({ "issueId": "uuid-eng-2", "relatedIssueId": issue.uuid, "type": "blocks" })
        );
        assert_eq!(sent[1]["query"], UNRELATE_MUTATION);
        assert_eq!(sent[1]["variables"]["id"], "rel-3");
    }

    #[test]
    fn parse_read_maps_ids_back_to_declared_names() {
        let cfg = config();
        let issue = parse_issue(&fixture("linear_fields_read.json"))
            .unwrap()
            .unwrap();
        let current = issue.current(&cfg.ticket.fields.linear, &cfg.people);
        let get = |k: &str| current.0.get(k).cloned().unwrap_or_default();
        assert_eq!(get("team"), ["ENG"]);
        assert_eq!(get("labels"), ["bug", "lbl-unknown"]);
        assert_eq!(get("assignee"), ["ada"]);
        assert_eq!(get("priority"), ["high"]);
        assert_eq!(get("estimate"), ["3"]);
        assert_eq!(get("cycle"), ["c7"]);
        assert_eq!(get("parent"), ["ENG-1"]);
        assert_eq!(get("blocked_by"), ["ENG-3"]);
        assert!(get("milestone").is_empty() && get("due").is_empty());
        assert_eq!(
            issue.team_name(&cfg.ticket.fields.linear).as_deref(),
            Some("ENG")
        );
    }

    #[test]
    fn update_input_writes_only_the_named_fields_and_clears_with_none() {
        let r = resolved(&[("priority", "urgent")], &["assignee", "labels"]);
        let input = update_input(
            None,
            None,
            &config().ticket.fields.linear,
            &r,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            input,
            json!({ "priority": 1, "assigneeId": null, "labelIds": [] })
        );
    }

    #[test]
    fn verify_names_a_field_that_reads_back_otherwise() {
        let cfg = config();
        let issue = parse_issue(&fixture("linear_fields_read.json"))
            .unwrap()
            .unwrap();
        let r = resolved(&[("priority", "urgent"), ("estimate", "3")], &["milestone"]);
        let off = LinearFieldWriter::verify(&cfg.ticket.fields.linear, &r, &issue);
        assert_eq!(off, [("priority".to_string(), "high".to_string())]);
    }
}
