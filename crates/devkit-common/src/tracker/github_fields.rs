//! Writing and reading back an issue's declared ticket fields on GitHub: its
//! labels, assignees, milestone, type, parent and blockers, and its item in
//! `[github] project` with that item's fields.
//!
//! Split like `github_status.rs`: query builders and response parsers, tested
//! over recorded responses, and a networked [`GithubFieldWriter`] over them.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use devkit_config::{FieldDecl, FieldType, Place, ProjectRef};
use serde_json::Value as Json;

use super::github_status::{
    add_item_mutation, parse_added_item, scope_error, scope_remedy, set_field_mutation,
};
use crate::{
    fields::{Current, Resolved, Value},
    forge::Repo,
    github::{Api, TokenSource},
};

/// The id an issue's membership of `[github] project` reads as.
const MEMBER: &str = "member";

/// One query reading what the declared places hold on issue `number`, the
/// node ids of the issues in `refs`, and the project's id.
pub fn read_query(
    slug: &str,
    number: u64,
    decls: &BTreeMap<String, FieldDecl>,
    project: Option<&ProjectRef>,
    refs: &BTreeSet<u64>,
) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    let places: Vec<&Place> = decls.values().filter_map(|d| d.r#where.as_ref()).collect();
    let has = |p: &Place| places.contains(&p);
    let mut issue = vec!["id".to_string()];
    if has(&Place::Labels) {
        issue.push("labels(first: 100) { nodes { name } }".into());
    }
    if has(&Place::Assignees) {
        issue.push("assignees(first: 100) { nodes { login } }".into());
    }
    if has(&Place::Milestone) {
        issue.push("milestone { title }".into());
    }
    if has(&Place::IssueType) {
        issue.push("issueType { id name }".into());
    }
    if has(&Place::Parent) {
        issue.push("parent { id number }".into());
    }
    if has(&Place::BlockedBy) {
        issue.push("blockedBy(first: 100) { nodes { id number } }".into());
    }
    let on_project = places
        .iter()
        .any(|p| matches!(p, Place::Project | Place::ProjectField { .. }));
    if on_project {
        let common = "field { ... on ProjectV2FieldCommon { id } }";
        issue.push(format!(
            "projectItems(first: 100, includeArchived: true) {{ nodes {{ id project {{ id number }} \
             fieldValues(first: 100) {{ nodes {{ \
             ... on ProjectV2ItemFieldSingleSelectValue {{ optionId {common} }} \
             ... on ProjectV2ItemFieldTextValue {{ text {common} }} \
             ... on ProjectV2ItemFieldNumberValue {{ number {common} }} \
             ... on ProjectV2ItemFieldDateValue {{ date {common} }} }} }} }} }}"
        ));
    }
    let refs: String = refs
        .iter()
        .map(|n| format!("\n    ref{n}: issue(number: {n}) {{ id }}"))
        .collect();
    let board = match (on_project, project) {
        (true, Some(p)) => format!(
            "\n  repositoryOwner(login: {po}) {{ ... on ProjectV2Owner {{ projectV2(number: {n}) {{ id }} }} }}",
            po = Json::from(p.owner.as_deref().unwrap_or(owner)),
            n = p.number,
        ),
        _ => String::new(),
    };
    format!(
        "query {{\n  repository(owner: {o}, name: {n}) {{\n    issue(number: {number}) {{ {fields} }}{refs}\n  }}{board}\n}}",
        o = Json::from(owner),
        n = Json::from(name),
        fields = issue.join(" "),
    )
}

/// What [`read_query`] read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueFields {
    pub issue_id: String,
    pub project_id: Option<String>,
    /// The issue's item in `[github] project`, when it has one.
    pub item_id: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub milestone: Option<String>,
    pub issue_type: Option<String>,
    /// The parent's number and node id.
    pub parent: Option<(u64, String)>,
    /// Each blocker's number and node id.
    pub blocked_by: Vec<(u64, String)>,
    /// The item's field values by field id: an option id, text, number or
    /// date.
    pub project_values: BTreeMap<String, String>,
    /// Node ids of the issues the query was asked to resolve.
    pub refs: BTreeMap<u64, String>,
}

fn strs(v: &Json, list: &str, key: &str) -> Vec<String> {
    v.pointer(list)
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|n| n[key].as_str().map(String::from))
        .collect()
}

fn number_text(n: f64) -> String {
    format!("{n}")
}

/// The [`IssueFields`] in a [`read_query`] response.
pub fn parse_read(resp: &Json) -> Result<IssueFields> {
    let issue = resp
        .pointer("/data/repository/issue")
        .filter(|i| !i.is_null())
        .context("the issue was not found in [github] issues_repo")?;
    let project_id = resp
        .pointer("/data/repositoryOwner/projectV2/id")
        .and_then(Json::as_str)
        .map(String::from);
    let item = issue
        .pointer("/projectItems/nodes")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .find(|n| {
            project_id.is_some()
                && n.pointer("/project/id").and_then(Json::as_str) == project_id.as_deref()
        });
    let mut project_values = BTreeMap::new();
    for v in item
        .and_then(|i| i.pointer("/fieldValues/nodes"))
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        let Some(field) = v.pointer("/field/id").and_then(Json::as_str) else {
            continue;
        };
        let value = match (&v["optionId"], &v["text"], &v["number"], &v["date"]) {
            (Json::String(o), ..) => o.clone(),
            (_, Json::String(t), ..) => t.clone(),
            (_, _, Json::Number(n), _) => number_text(n.as_f64().unwrap_or_default()),
            (.., Json::String(d)) => d.clone(),
            _ => continue,
        };
        project_values.insert(field.to_string(), value);
    }
    let pair = |n: &Json| Some((n["number"].as_u64()?, n["id"].as_str()?.to_string()));
    let refs = resp
        .pointer("/data/repository")
        .and_then(Json::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| {
            let n = k.strip_prefix("ref")?.parse().ok()?;
            Some((n, v["id"].as_str()?.to_string()))
        })
        .collect();
    Ok(IssueFields {
        issue_id: issue["id"]
            .as_str()
            .context("issue without an id")?
            .to_string(),
        project_id,
        item_id: item.and_then(|i| i["id"].as_str()).map(String::from),
        labels: strs(issue, "/labels/nodes", "name"),
        assignees: strs(issue, "/assignees/nodes", "login"),
        milestone: issue
            .pointer("/milestone/title")
            .and_then(Json::as_str)
            .map(String::from),
        issue_type: issue
            .pointer("/issueType/id")
            .and_then(Json::as_str)
            .map(String::from),
        parent: pair(&issue["parent"]),
        blocked_by: issue
            .pointer("/blockedBy/nodes")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(pair)
            .collect(),
        project_values,
        refs,
    })
}

impl IssueFields {
    /// What `place` holds, as the ids devkit writes there.
    pub fn ids(&self, place: &Place) -> Vec<String> {
        match place {
            Place::Labels => self.labels.clone(),
            Place::Assignees => self.assignees.clone(),
            Place::Milestone => self.milestone.iter().cloned().collect(),
            Place::IssueType => self.issue_type.iter().cloned().collect(),
            Place::Parent => self.parent.iter().map(|(n, _)| n.to_string()).collect(),
            Place::BlockedBy => self.blocked_by.iter().map(|(n, _)| n.to_string()).collect(),
            Place::Project => self.item_id.iter().map(|_| MEMBER.to_string()).collect(),
            Place::ProjectField { id, .. } => {
                self.project_values.get(id).cloned().into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    /// The fields' present values by field name, as names: an enum value's
    /// declared name where its id is declared, else what GitHub holds.
    pub fn current(&self, decls: &BTreeMap<String, FieldDecl>) -> Current {
        let mut out = BTreeMap::new();
        for (name, decl) in decls {
            let Some(place) = &decl.r#where else { continue };
            let names: Vec<String> = self
                .ids(place)
                .into_iter()
                .map(|id| decl.value_name(&id))
                .collect();
            if !names.is_empty() {
                out.insert(name.clone(), names);
            }
        }
        Current(out)
    }
}

/// The ids a field's resolved values are written as.
pub fn written_ids(place: &Place, values: &[Value]) -> Vec<String> {
    values
        .iter()
        .map(|v| match (place, v) {
            (Place::Project, _) => MEMBER.to_string(),
            (_, Value::Enum { id, .. } | Value::User { id, .. }) => id.clone(),
            (_, Value::Number(n)) => number_text(*n),
            (_, Value::Text(s) | Value::Date(s) | Value::Issue(s)) => s.clone(),
        })
        .collect()
}

/// Set the issue's type, or clear it with `None`.
pub fn issue_type_mutation(issue_id: &str, type_id: Option<&str>) -> String {
    format!(
        "mutation {{ updateIssueIssueType(input: {{issueId: {i}, issueTypeId: {t}}}) {{ issue {{ id }} }} }}",
        i = Json::from(issue_id),
        t = type_id.map_or(Json::Null, Json::from),
    )
}

/// Make `child` a sub-issue of `parent`, moving it from any other parent.
pub fn add_sub_issue_mutation(parent: &str, child: &str) -> String {
    format!(
        "mutation {{ addSubIssue(input: {{issueId: {p}, subIssueId: {c}, replaceParent: true}}) {{ issue {{ id }} }} }}",
        p = Json::from(parent),
        c = Json::from(child),
    )
}

/// Take `child` out of `parent`'s sub-issues.
pub fn remove_sub_issue_mutation(parent: &str, child: &str) -> String {
    format!(
        "mutation {{ removeSubIssue(input: {{issueId: {p}, subIssueId: {c}}}) {{ issue {{ id }} }} }}",
        p = Json::from(parent),
        c = Json::from(child),
    )
}

/// Mark `issue` blocked by `blocker`, or with `add` false, no longer.
pub fn blocked_by_mutation(issue: &str, blocker: &str, add: bool) -> String {
    format!(
        "mutation {{ {m}(input: {{issueId: {i}, blockingIssueId: {b}}}) {{ issue {{ id }} }} }}",
        m = if add {
            "addBlockedBy"
        } else {
            "removeBlockedBy"
        },
        i = Json::from(issue),
        b = Json::from(blocker),
    )
}

/// Set a project item's text, number or date field.
pub fn set_value_mutation(
    project: &str,
    item: &str,
    field: &str,
    ty: FieldType,
    raw: &str,
) -> Result<String> {
    let value = match ty {
        FieldType::Number => format!(
            "number: {}",
            raw.parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .map(number_text)
                .with_context(|| format!("`{raw}` is not a number"))?
        ),
        FieldType::Date => format!("date: {}", Json::from(raw)),
        FieldType::String => format!("text: {}", Json::from(raw)),
        other => bail!("a project field cannot hold a `{}` value", other.as_str()),
    };
    Ok(format!(
        "mutation {{ updateProjectV2ItemFieldValue(input: {{projectId: {p}, itemId: {i}, fieldId: {f}, value: {{{value}}}}}) {{ projectV2Item {{ id }} }} }}",
        p = Json::from(project),
        i = Json::from(item),
        f = Json::from(field),
    ))
}

/// Clear a project item's field.
pub fn clear_value_mutation(project: &str, item: &str, field: &str) -> String {
    format!(
        "mutation {{ clearProjectV2ItemFieldValue(input: {{projectId: {p}, itemId: {i}, fieldId: {f}}}) {{ projectV2Item {{ id }} }} }}",
        p = Json::from(project),
        i = Json::from(item),
        f = Json::from(field),
    )
}

/// Remove the issue's item from the project.
pub fn delete_item_mutation(project: &str, item: &str) -> String {
    format!(
        "mutation {{ deleteProjectV2Item(input: {{projectId: {p}, itemId: {i}}}) {{ deletedItemId }} }}",
        p = Json::from(project),
        i = Json::from(item),
    )
}

/// Fail on any error in a mutation's response, a scope error with its
/// remedy: a mutation that names an id GitHub cannot resolve did not take.
fn check_mutation(resp: &Json, source: TokenSource) -> Result<()> {
    if let Some(e) = scope_error(resp, source) {
        return Err(e);
    }
    match resp["errors"].as_array().and_then(|e| e.first()) {
        Some(e) => bail!(
            "GitHub refused the write: {}",
            e["message"].as_str().unwrap_or("unknown error")
        ),
        None => Ok(()),
    }
}

/// The scope remedy for a failure `gh` reported as missing scopes.
fn with_scope_remedy(e: anyhow::Error, source: TokenSource) -> anyhow::Error {
    let e = match crate::cmd::failed_stderr(&e) {
        Some(stderr) => anyhow::anyhow!("`gh api graphql` failed: {}", stderr.trim()),
        None => e,
    };
    let text = format!("{e:#}");
    if !(text.contains("INSUFFICIENT_SCOPES") || text.contains("required scopes")) {
        return e;
    }
    e.context(scope_remedy(source))
}

/// The ticket fields of issues in `repo`, with project fields on `project`.
pub struct GithubFieldWriter {
    repo: Repo,
    api: Api,
    project: Option<ProjectRef>,
    cwd: String,
}

/// The places `gh issue create` writes itself.
pub fn on_create(place: &Place) -> bool {
    matches!(place, Place::Labels | Place::Assignees | Place::Milestone)
}

impl GithubFieldWriter {
    pub fn new(repo: Repo, project: Option<ProjectRef>, cwd: &str) -> Self {
        let api = Api::new(&repo.host);
        GithubFieldWriter {
            repo,
            api,
            project,
            cwd: cwd.to_string(),
        }
    }

    fn query(&self, query: &str) -> Result<Json> {
        let source = self.api.token_source();
        let resp = self
            .api
            .graphql_or_gh(query, Api::graphql_value)
            .map_err(|e| with_scope_remedy(e, source))?;
        super::github_status::check(&resp, source)?;
        Ok(resp)
    }

    fn mutate(&self, query: &str) -> Result<Json> {
        let source = self.api.token_source();
        let resp = self
            .api
            .graphql_or_gh(query, Api::graphql_value)
            .map_err(|e| with_scope_remedy(e, source))?;
        check_mutation(&resp, source)?;
        Ok(resp)
    }

    /// Read what issue `number`'s declared places hold, resolving the node
    /// ids of the issues `resolved` names as parent or blockers.
    pub fn read(
        &self,
        number: u64,
        decls: &BTreeMap<String, FieldDecl>,
        resolved: Option<&Resolved>,
    ) -> Result<IssueFields> {
        let mut refs = BTreeSet::new();
        for (name, values) in resolved.map(|r| &r.fields).into_iter().flatten() {
            if matches!(
                decls.get(name).and_then(|d| d.r#where.as_ref()),
                Some(Place::Parent | Place::BlockedBy)
            ) {
                refs.extend(values.iter().filter_map(|v| v.name().parse::<u64>().ok()));
            }
        }
        let resp = self.query(&read_query(
            &self.repo.slug,
            number,
            decls,
            self.project.as_ref(),
            &refs,
        ))?;
        parse_read(&resp)
    }

    /// Write every field in `resolved` and every `--none` clear onto issue
    /// `number`, which held `before`, skipping the fields `skip` names.
    /// Returns each field that did not take, with why.
    pub fn apply(
        &self,
        number: u64,
        decls: &BTreeMap<String, FieldDecl>,
        resolved: &Resolved,
        before: &IssueFields,
        skip: &BTreeSet<String>,
    ) -> Vec<(String, anyhow::Error)> {
        let mut failed = Vec::new();
        let mut item = before.item_id.clone();
        let names: BTreeSet<&String> = resolved.fields.keys().chain(&resolved.nones).collect();
        for name in names.into_iter().filter(|n| !skip.contains(*n)) {
            let Some(decl) = decls.get(name) else {
                continue;
            };
            let Some(place) = &decl.r#where else { continue };
            let values = resolved
                .fields
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let want = written_ids(place, values);
            if let Err(e) = self.write(number, decl, place, &want, before, &mut item) {
                failed.push((name.clone(), e));
            }
        }
        failed
    }

    fn ensure_item(
        &self,
        before: &IssueFields,
        item: &mut Option<String>,
    ) -> Result<(String, String)> {
        let project = before
            .project_id
            .clone()
            .context("no Projects v2 project to write to: set [github] project")?;
        if let Some(i) = item {
            return Ok((project, i.clone()));
        }
        let added =
            parse_added_item(&self.mutate(&add_item_mutation(&project, &before.issue_id))?)?;
        *item = Some(added.clone());
        Ok((project, added))
    }

    fn gh_edit(&self, number: u64, args: &[&str]) -> Result<()> {
        let n = number.to_string();
        let mut v = vec!["issue", "edit", n.as_str()];
        v.extend_from_slice(args);
        crate::cmd::gh_capture(&v, &self.repo, &self.cwd).map(|_| ())
    }

    fn node(&self, before: &IssueFields, number: &str) -> Result<String> {
        let n: u64 = number
            .parse()
            .with_context(|| format!("`{number}` is not an issue number"))?;
        before
            .refs
            .get(&n)
            .cloned()
            .with_context(|| format!("issue #{n} was not found in [github] issues_repo"))
    }

    fn write(
        &self,
        number: u64,
        decl: &FieldDecl,
        place: &Place,
        want: &[String],
        before: &IssueFields,
        item: &mut Option<String>,
    ) -> Result<()> {
        let have = before.ids(place);
        let same = |a: &str, b: &str| match place {
            Place::Labels | Place::Assignees => a.eq_ignore_ascii_case(b),
            _ => a == b,
        };
        let added: Vec<&String> = want
            .iter()
            .filter(|w| !have.iter().any(|h| same(h, w)))
            .collect();
        let removed: Vec<&String> = have
            .iter()
            .filter(|h| !want.iter().any(|w| same(h, w)))
            .collect();
        match place {
            Place::Labels | Place::Assignees => {
                let (add, remove) = if *place == Place::Labels {
                    ("--add-label", "--remove-label")
                } else {
                    ("--add-assignee", "--remove-assignee")
                };
                let mut args: Vec<&str> = Vec::new();
                for a in &added {
                    args.extend([add, a.as_str()]);
                }
                for r in &removed {
                    args.extend([remove, r.as_str()]);
                }
                if !args.is_empty() {
                    self.gh_edit(number, &args)?;
                }
            }
            Place::Milestone => match want.first() {
                Some(m) if have.first() != Some(m) => self.gh_edit(number, &["--milestone", m])?,
                Some(_) => {}
                None if !have.is_empty() => self.gh_edit(number, &["--remove-milestone"])?,
                None => {}
            },
            Place::IssueType => {
                if have.first() != want.first() {
                    self.mutate(&issue_type_mutation(
                        &before.issue_id,
                        want.first().map(String::as_str),
                    ))?;
                }
            }
            Place::Parent => match (want.first(), &before.parent) {
                (Some(w), Some((n, _))) if *w == n.to_string() => {}
                (Some(w), _) => {
                    let parent = self.node(before, w)?;
                    self.mutate(&add_sub_issue_mutation(&parent, &before.issue_id))?;
                }
                (None, Some((_, parent))) => {
                    self.mutate(&remove_sub_issue_mutation(parent, &before.issue_id))?;
                }
                (None, None) => {}
            },
            Place::BlockedBy => {
                for a in &added {
                    let blocker = self.node(before, a)?;
                    self.mutate(&blocked_by_mutation(&before.issue_id, &blocker, true))?;
                }
                for r in &removed {
                    let blocker = before
                        .blocked_by
                        .iter()
                        .find(|(n, _)| n.to_string() == **r)
                        .map(|(_, id)| id.clone())
                        .context("a blocker without a node id")?;
                    self.mutate(&blocked_by_mutation(&before.issue_id, &blocker, false))?;
                }
            }
            Place::Project => match (want.is_empty(), item.clone()) {
                (false, _) => {
                    self.ensure_item(before, item)?;
                }
                (true, Some(i)) => {
                    let project = before.project_id.clone().context("no project id")?;
                    self.mutate(&delete_item_mutation(&project, &i))?;
                    *item = None;
                }
                (true, None) => {}
            },
            Place::ProjectField { id: field, .. } => {
                if have.first() == want.first() && item.is_some() {
                    return Ok(());
                }
                match want.first() {
                    Some(v) => {
                        let (project, item) = self.ensure_item(before, item)?;
                        let q = match decl.r#type {
                            Some(FieldType::Enum) => set_field_mutation(&project, &item, field, v),
                            Some(ty) => set_value_mutation(&project, &item, field, ty, v)?,
                            None => bail!("the field has no `type`"),
                        };
                        self.mutate(&q)?;
                    }
                    None => {
                        if let Some(i) = item.clone() {
                            let project = before.project_id.clone().context("no project id")?;
                            self.mutate(&clear_value_mutation(&project, &i, field))?;
                        }
                    }
                }
            }
            other => bail!("`{}` is not a place on GitHub issues", other.label()),
        }
        Ok(())
    }

    /// Each field of `resolved` whose read-back `after` differs from what was
    /// written, with what it reads as. A list compares as a set, and a
    /// `--none` field must read empty.
    pub fn verify(
        decls: &BTreeMap<String, FieldDecl>,
        resolved: &Resolved,
        after: &IssueFields,
        include_nones: bool,
    ) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let nones = resolved.nones.iter().filter(|_| include_nones);
        for name in resolved.fields.keys().chain(nones) {
            let Some(decl) = decls.get(name) else {
                continue;
            };
            let Some(place) = &decl.r#where else { continue };
            let values = resolved
                .fields
                .get(name)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let want: BTreeSet<String> = written_ids(place, values).into_iter().collect();
            let have: BTreeSet<String> = after.ids(place).into_iter().collect();
            let same = match place {
                Place::Labels | Place::Assignees => {
                    want.iter()
                        .map(|w| w.to_ascii_lowercase())
                        .collect::<BTreeSet<_>>()
                        == have.iter().map(|h| h.to_ascii_lowercase()).collect()
                }
                _ => want == have,
            };
            if !same {
                let reads: Vec<String> = have.iter().map(|id| decl.value_name(id)).collect();
                let reads = if reads.is_empty() {
                    "nothing".to_string()
                } else {
                    reads.join(", ")
                };
                out.push((name.clone(), reads));
            }
        }
        out
    }
}

/// Why a write may have failed on a stale declared id.
pub fn stale_hint(field: &str, decl: &FieldDecl, e: &anyhow::Error) -> Option<String> {
    let text = format!("{e:#}");
    let names_an_id = decl.r#type == Some(FieldType::Enum)
        && (text.contains("Could not resolve")
            || text.contains("NOT_FOUND")
            || text.contains("not found"));
    names_an_id.then(|| {
        format!(
            "the declared id may be stale: check `values` under [ticket.fields.github.{field}] against the tracker"
        )
    })
}

#[cfg(test)]
mod tests {
    use devkit_config::Config;

    use super::*;
    use crate::tracker::fixture;

    fn decls() -> BTreeMap<String, FieldDecl> {
        Config::parse(
            r#"
[ticket.fields.github.labels]
type = "string"
list = true
where = "labels"
[ticket.fields.github.assignee]
type = "user"
where = "assignees"
[ticket.fields.github.milestone]
type = "string"
where = "milestone"
[ticket.fields.github.type]
type = "enum"
where = "issue_type"
values = [{ name = "Bug", id = "IT_bug" }]
[ticket.fields.github.parent]
type = "issue"
where = "parent"
[ticket.fields.github.blocked_by]
type = "issue"
list = true
where = "blocked_by"
[ticket.fields.github.priority]
type = "enum"
where = { project_field = "PVTSSF_p", name = "Priority" }
values = [{ name = "P0", id = "opt_p0" }, { name = "P1", id = "opt_p1" }]
[ticket.fields.github.estimate]
type = "number"
where = { project_field = "PVTF_e", name = "Estimate" }
"#,
        )
        .unwrap()
        .ticket
        .fields
        .github
    }

    #[test]
    fn read_query_reads_labels_assignees_milestone_type_parent_blockers_and_the_project_item() {
        let project = ProjectRef {
            owner: Some("org".into()),
            number: 3,
        };
        let q = read_query("o/r", 42, &decls(), Some(&project), &BTreeSet::from([12]));
        for part in [
            "repository(owner: \"o\", name: \"r\")",
            "issue(number: 42)",
            "labels(first: 100)",
            "assignees(first: 100)",
            "milestone { title }",
            "issueType { id name }",
            "parent { id number }",
            "blockedBy(first: 100)",
            "projectItems(first: 100",
            "ProjectV2ItemFieldSingleSelectValue { optionId",
            "ref12: issue(number: 12) { id }",
            "repositoryOwner(login: \"org\")",
            "projectV2(number: 3)",
        ] {
            assert!(q.contains(part), "missing {part}: {q}");
        }
        let bare = read_query(
            "o/r",
            42,
            &BTreeMap::new(),
            Some(&project),
            &BTreeSet::new(),
        );
        assert!(
            !bare.contains("labels")
                && !bare.contains("projectItems")
                && !bare.contains("repositoryOwner"),
            "only what is declared: {bare}"
        );
    }

    #[test]
    fn parse_read_maps_project_field_options_back_to_value_ids() {
        let r = parse_read(&fixture("github_fields_read.json")).unwrap();
        assert_eq!(r.issue_id, "I_42");
        assert_eq!(
            r.item_id.as_deref(),
            Some("PVTI_3"),
            "the configured project's item"
        );
        assert_eq!(r.project_values["PVTSSF_p"], "opt_p1");
        assert_eq!(r.project_values["PVTF_e"], "3");
        assert_eq!(r.labels, ["bug", "enhancement"]);
        assert_eq!(r.parent, Some((12, "I_12".into())));
        assert_eq!(r.blocked_by, [(13, "I_13".into())]);
        assert_eq!(r.refs[&14], "I_14");
        let d = decls();
        let current = r.current(&d);
        assert_eq!(current.0["priority"], ["P1"]);
        assert_eq!(current.0["type"], ["Bug"]);
        assert_eq!(current.0["milestone"], ["v1.0"]);
    }

    #[test]
    fn blocked_by_and_parent_mutations_carry_node_ids() {
        let add = blocked_by_mutation("I_42", "I_13", true);
        assert!(
            add.starts_with("mutation")
                && add.contains(
                    "addBlockedBy(input: {issueId: \"I_42\", blockingIssueId: \"I_13\"})"
                ),
            "{add}"
        );
        assert!(blocked_by_mutation("I_42", "I_13", false).contains("removeBlockedBy("));
        let parent = add_sub_issue_mutation("I_12", "I_42");
        assert!(
            parent.contains(
                "addSubIssue(input: {issueId: \"I_12\", subIssueId: \"I_42\", replaceParent: true})"
            ),
            "{parent}"
        );
        assert!(remove_sub_issue_mutation("I_12", "I_42").contains("removeSubIssue("));
        assert!(issue_type_mutation("I_42", None).contains("issueTypeId: null"));
        let n = set_value_mutation("P", "I", "F", FieldType::Number, "3").unwrap();
        assert!(n.contains("value: {number: 3}"), "{n}");
        assert!(set_value_mutation("P", "I", "F", FieldType::User, "x").is_err());
    }

    #[test]
    fn verify_compares_lists_as_sets_and_names_what_reads_back() {
        let d = decls();
        let resolved = Resolved {
            fields: BTreeMap::from([
                ("labels".to_string(), vec![
                    Value::Text("enhancement".into()),
                    Value::Text("bug".into()),
                ]),
                ("priority".to_string(), vec![Value::Enum {
                    name: "P0".into(),
                    id: "opt_p0".into(),
                }]),
            ]),
            nones: BTreeSet::from(["milestone".to_string()]),
        };
        let after = parse_read(&fixture("github_fields_read.json")).unwrap();
        let bad = GithubFieldWriter::verify(&d, &resolved, &after, true);
        assert_eq!(bad, [
            ("priority".to_string(), "P1".to_string()),
            ("milestone".to_string(), "v1.0".to_string())
        ]);
        assert_eq!(
            GithubFieldWriter::verify(&d, &resolved, &after, false).len(),
            1
        );
    }
}
