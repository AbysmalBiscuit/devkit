//! Reading and writing an issue's status in a GitHub Projects v2 project.
//!
//! Split like `github.rs`: query builders and response parsers, tested over
//! recorded responses, and a networked [`GithubWriter`] over them.

use std::cell::RefCell;

use anyhow::{Context, Result, anyhow, bail};
use devkit_config::ProjectRef;
use serde_json::Value;

use super::status::{StatusWriter, find_name};
use crate::{
    cmd::gh_json,
    forge::Repo,
    github::{Api, TokenSource},
};

fn quoted(s: &str) -> Value {
    Value::from(s)
}

/// One query reading the issue's node id, its item in each project, and the
/// configured project's status field. The project is read through
/// `repositoryOwner`, which resolves a user or an organization alike.
pub fn status_query(slug: &str, issue: u64, project: &ProjectRef, field: &str) -> String {
    let (owner, name) = slug.split_once('/').unwrap_or((slug, ""));
    let project_owner = project.owner.as_deref().unwrap_or(owner);
    format!(
        r#"query {{
  repository(owner: {o}, name: {n}) {{
    issue(number: {issue}) {{
      id
      projectItems(first: 20, includeArchived: true) {{
        nodes {{
          id
          project {{ id number }}
          fieldValueByName(name: {f}) {{ ... on ProjectV2ItemFieldSingleSelectValue {{ name optionId }} }}
        }}
      }}
    }}
  }}
  repositoryOwner(login: {po}) {{
    ... on ProjectV2Owner {{
      projectV2(number: {p}) {{
        id
        field(name: {f}) {{ ... on ProjectV2SingleSelectField {{ id options {{ id name }} }} }}
      }}
    }}
  }}
}}"#,
        o = quoted(owner),
        n = quoted(name),
        f = quoted(field),
        po = quoted(project_owner),
        p = project.number,
    )
}

/// What [`status_query`] read: everything a status write needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRead {
    pub issue_id: String,
    pub project_id: String,
    /// The issue's item in the configured project; `None` when the issue is
    /// not in it.
    pub item_id: Option<String>,
    pub field_id: String,
    /// The item's status, `None` when it has no item or the field is unset.
    pub current: Option<String>,
    /// `(option id, name)` for each option of the field.
    pub options: Vec<(String, String)>,
}

impl StatusRead {
    /// The id of the option named `to`, matched case-insensitively.
    pub fn option_id(&self, to: &str, field: &str) -> Result<&str> {
        let name =
            find_name(self.options.iter().map(|(_, n)| n.as_str()), to).with_context(|| {
                let names: Vec<&str> = self.options.iter().map(|(_, n)| n.as_str()).collect();
                format!(
                    "no option `{to}` in `{field}` ([github] status_field); options: {}",
                    names.join(", ")
                )
            })?;
        Ok(self
            .options
            .iter()
            .find(|(_, n)| n == name)
            .map(|(id, _)| id.as_str())
            .expect("find_name returned one of these names"))
    }
}

fn str_at<'a>(v: &'a Value, pointer: &str) -> Option<&'a str> {
    v.pointer(pointer).and_then(Value::as_str)
}

/// The [`StatusRead`] in a [`status_query`] response. Only the item in the
/// configured project counts: an issue in several projects has an item, and a
/// status, in each.
pub fn parse_status(resp: &Value, field: &str) -> Result<StatusRead> {
    let project = resp
        .pointer("/data/repositoryOwner/projectV2")
        .filter(|p| !p.is_null())
        .context("no such Projects v2 project ([github] project), or the token cannot see it")?;
    let project_id = str_at(project, "/id").context("project without an id")?;
    let field_value = project
        .get("field")
        .filter(|f| !f.is_null())
        .with_context(|| format!("the project has no field `{field}` ([github] status_field)"))?;
    let field_id = str_at(field_value, "/id").with_context(|| {
        format!("`{field}` is not a single-select field ([github] status_field)")
    })?;
    let options = field_value["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            Some((
                o["id"].as_str()?.to_string(),
                o["name"].as_str()?.to_string(),
            ))
        })
        .collect();

    let issue = resp
        .pointer("/data/repository/issue")
        .filter(|i| !i.is_null())
        .context("the issue was not found in [github] issues_repo")?;
    let item = issue
        .pointer("/projectItems/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|n| str_at(n, "/project/id") == Some(project_id));
    Ok(StatusRead {
        issue_id: str_at(issue, "/id")
            .context("issue without an id")?
            .to_string(),
        project_id: project_id.to_string(),
        item_id: item.and_then(|i| str_at(i, "/id")).map(String::from),
        field_id: field_id.to_string(),
        current: item
            .and_then(|i| str_at(i, "/fieldValueByName/name"))
            .map(String::from),
        options,
    })
}

/// The error for a token without Projects access, naming the remedy for
/// where the token came from. `None` when `resp` reports no such error.
pub fn scope_error(resp: &Value, source: TokenSource) -> Option<anyhow::Error> {
    let insufficient = resp["errors"]
        .as_array()?
        .iter()
        .any(|e| e["type"].as_str() == Some("INSUFFICIENT_SCOPES"));
    insufficient.then(|| match source {
        TokenSource::Env(var) => anyhow!(
            "the GitHub token in {var} lacks the `project` scope; reissue {var} with `project`"
        ),
        TokenSource::Gh | TokenSource::None => {
            anyhow!("the GitHub token lacks the `project` scope; run `gh auth refresh -s project`")
        }
    })
}

/// Fail on a response's errors: a scope error with its remedy, or any error
/// but `NOT_FOUND`, which the parsers report as the config key at fault.
pub fn check(resp: &Value, source: TokenSource) -> Result<()> {
    if let Some(e) = scope_error(resp, source) {
        return Err(e);
    }
    let fatal = resp["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|e| e["type"].as_str() != Some("NOT_FOUND"));
    match fatal {
        Some(e) => bail!(
            "GitHub GraphQL error: {}",
            e["message"].as_str().unwrap_or("unknown error")
        ),
        None => Ok(()),
    }
}

/// Add the issue `content_id` to the project as an item.
pub fn add_item_mutation(project_id: &str, content_id: &str) -> String {
    format!(
        "mutation {{ addProjectV2ItemById(input: {{projectId: {p}, contentId: {c}}}) {{ item {{ id }} }} }}",
        p = quoted(project_id),
        c = quoted(content_id),
    )
}

/// The new item's id from an [`add_item_mutation`] response.
pub fn parse_added_item(resp: &Value) -> Result<String> {
    str_at(resp, "/data/addProjectV2ItemById/item/id")
        .map(String::from)
        .context("adding the issue to [github] project returned no item")
}

/// Set the item's single-select field to the option `option_id`.
pub fn set_field_mutation(
    project_id: &str,
    item_id: &str,
    field_id: &str,
    option_id: &str,
) -> String {
    format!(
        "mutation {{ updateProjectV2ItemFieldValue(input: {{projectId: {p}, itemId: {i}, fieldId: {f}, value: {{singleSelectOptionId: {o}}}}}) {{ projectV2Item {{ id }} }} }}",
        p = quoted(project_id),
        i = quoted(item_id),
        f = quoted(field_id),
        o = quoted(option_id),
    )
}

/// The status of issues in `repo`, kept in `project`'s single-select `field`.
pub struct GithubWriter {
    repo: Repo,
    api: Api,
    project: ProjectRef,
    field: String,
    /// The read [`StatusWriter::status`] made, which the
    /// [`StatusWriter::set_status`] after it reuses instead of reading again.
    last: RefCell<Option<(u64, StatusRead)>>,
}

impl GithubWriter {
    pub fn new(repo: Repo, project: ProjectRef, field: String) -> Self {
        let api = Api::new(&repo.host);
        GithubWriter {
            repo,
            api,
            project,
            field,
            last: RefCell::new(None),
        }
    }

    /// Send `query` with the host's token, or through `gh` when none
    /// resolves, the fallback `GithubTracker::details` takes.
    fn send(&self, query: &str) -> Result<Value> {
        let resp = match self.api.token() {
            Some(_) => self.api.graphql_value(query)?,
            None => gh_json(
                &[
                    "api",
                    "graphql",
                    "--hostname",
                    self.api.host(),
                    "-f",
                    &format!("query={query}"),
                ],
                ".",
            )?,
        };
        check(&resp, self.api.token_source())?;
        Ok(resp)
    }

    fn read(&self, n: u64) -> Result<StatusRead> {
        let resp = self.send(&status_query(
            &self.repo.slug,
            n,
            &self.project,
            &self.field,
        ))?;
        parse_status(&resp, &self.field)
    }
}

fn number(id: &str) -> Result<u64> {
    id.trim()
        .trim_start_matches('#')
        .parse()
        .with_context(|| format!("`{id}` is not a GitHub issue number"))
}

impl StatusWriter for GithubWriter {
    fn status(&self, id: &str) -> Result<Option<String>> {
        let n = number(id)?;
        let read = self.read(n)?;
        let current = read.current.clone();
        *self.last.borrow_mut() = Some((n, read));
        Ok(current)
    }

    fn set_status(&self, id: &str, to: &str) -> Result<()> {
        let n = number(id)?;
        let read = match self.last.borrow_mut().take() {
            Some((cached, read)) if cached == n => read,
            _ => self.read(n)?,
        };
        let option = read.option_id(to, &self.field)?;
        let item = match &read.item_id {
            Some(item) => item.clone(),
            None => {
                parse_added_item(&self.send(&add_item_mutation(&read.project_id, &read.issue_id))?)?
            }
        };
        self.send(&set_field_mutation(
            &read.project_id,
            &item,
            &read.field_id,
            option,
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/tracker/fixtures")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn reads_the_configured_projects_item_only() {
        let r = parse_status(&fixture("github_status_two_projects.json"), "Status").unwrap();
        assert_eq!(
            (r.item_id.as_deref(), r.current.as_deref()),
            (Some("PVTI_3"), Some("Todo"))
        );
        assert_eq!(
            (r.issue_id.as_str(), r.project_id.as_str()),
            ("I_65", "PVT_3")
        );
        assert_eq!(r.field_id, "PVTSSF_status");
        assert_eq!(r.options.len(), 3);
    }

    #[test]
    fn an_issue_outside_the_project_has_no_item_and_no_status() {
        let r = parse_status(&fixture("github_status_no_item.json"), "Status").unwrap();
        assert_eq!((r.item_id, r.current), (None, None));
    }

    #[test]
    fn a_field_that_is_not_single_select_names_the_key() {
        let e =
            parse_status(&fixture("github_status_not_single_select.json"), "Status").unwrap_err();
        assert!(e.to_string().contains("[github] status_field"), "{e}");
    }

    #[test]
    fn a_missing_project_names_the_key() {
        let e = parse_status(&fixture("github_status_no_project.json"), "Status").unwrap_err();
        assert!(e.to_string().contains("[github] project"), "{e}");
    }

    #[test]
    fn insufficient_scopes_names_the_remedy_for_each_token_source() {
        let v = fixture("github_status_insufficient_scopes.json");
        let gh = scope_error(&v, TokenSource::Gh).unwrap().to_string();
        assert!(
            gh.contains("lacks the `project` scope") && gh.contains("gh auth refresh -s project"),
            "{gh}"
        );
        let env = scope_error(&v, TokenSource::Env("GH_TOKEN"))
            .unwrap()
            .to_string();
        assert!(
            env.contains("reissue GH_TOKEN") && !env.contains("gh auth refresh"),
            "{env}"
        );
        assert!(
            scope_error(&fixture("github_status_two_projects.json"), TokenSource::Gh).is_none()
        );
    }

    #[test]
    fn a_scope_error_wins_over_the_missing_data() {
        let e = check(
            &fixture("github_status_insufficient_scopes.json"),
            TokenSource::Gh,
        )
        .unwrap_err();
        assert!(e.to_string().contains("gh auth refresh -s project"), "{e}");
    }

    #[test]
    fn the_query_reads_the_named_owners_project() {
        let q = status_query(
            "me/repo",
            65,
            &ProjectRef {
                owner: Some("org".into()),
                number: 7,
            },
            "Status",
        );
        assert!(q.contains("repositoryOwner(login: \"org\")"), "{q}");
        assert!(
            q.contains("projectV2(number: 7)") && q.contains("issue(number: 65)"),
            "{q}"
        );
        let bare = status_query(
            "me/repo",
            65,
            &ProjectRef {
                owner: None,
                number: 3,
            },
            "Stage",
        );
        assert!(
            bare.contains("repositoryOwner(login: \"me\")") && bare.contains("\"Stage\""),
            "{bare}"
        );
    }

    #[test]
    fn options_resolve_case_insensitively_and_an_unknown_one_lists_them() {
        let r = parse_status(&fixture("github_status_two_projects.json"), "Status").unwrap();
        assert_eq!(r.option_id(" in PROGRESS", "Status").unwrap(), "opt_doing");
        let e = r.option_id("Shipping", "Status").unwrap_err().to_string();
        assert!(
            e.contains("no option `Shipping` in `Status`") && e.contains("Todo, In progress, Done"),
            "{e}"
        );
    }

    #[test]
    fn mutations_carry_their_ids_and_the_added_item_parses() {
        let add = add_item_mutation("PVT_3", "I_65");
        assert!(
            add.contains("addProjectV2ItemById")
                && add.contains("\"PVT_3\"")
                && add.contains("\"I_65\"")
        );
        let set = set_field_mutation("PVT_3", "PVTI_3", "PVTSSF_status", "opt_doing");
        assert!(
            set.contains("updateProjectV2ItemFieldValue")
                && set.contains("singleSelectOptionId: \"opt_doing\"")
        );
        let added = serde_json::json!({ "data": { "addProjectV2ItemById": { "item": { "id": "PVTI_new" } } } });
        assert_eq!(parse_added_item(&added).unwrap(), "PVTI_new");
    }
}
