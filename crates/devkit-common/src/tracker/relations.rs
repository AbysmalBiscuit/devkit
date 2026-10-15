//! The ticket links a todo push writes, each read first: a GitHub sub-issue
//! or blocked-by link, a Linear `parentId` or `blocks` relation.
//!
//! A Linear pair holds one relation, and `issueRelationCreate` silently
//! replaces it; a resolved `blocks` still reads as `blocks`. So any other
//! relation on the pair is reported, for the push to treat as a conflict.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde_json::{Value as Json, json};

use super::{
    TrackerKind, github_fields, github_status,
    linear::{graphql_errors, parse_id, send_graphql},
    linear_fields::{RELATE_MUTATION, Transport, UNRELATE_MUTATION, UPDATE_MUTATION, blocks_input},
};
use crate::{
    forge::{Repo, Repos},
    github::Api,
};

/// The ticket links of the tracker `kind` names.
pub enum Relations {
    Github(GithubRelations),
    Linear(LinearRelations),
}

/// The relation writer for a tracker of `kind`, or an error naming what would
/// supply one.
pub fn relations_for(kind: TrackerKind, repos: &Repos) -> Result<Relations> {
    match kind {
        TrackerKind::Github => Ok(Relations::Github(GithubRelations::new(
            repos.issues()?.clone(),
        ))),
        TrackerKind::Linear => {
            let key = crate::secrets::resolve("LINEAR_API_KEY")
                .context("no LINEAR_API_KEY in the environment or ~/.config/devkit/secrets.toml")?;
            Ok(Relations::Linear(LinearRelations::new(key)))
        }
        TrackerKind::None => bail!("no tracker holds ticket links; set [tracker] kind"),
    }
}

/// A ticket's parent as the tracker holds it, and the ids a parent write
/// names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentRead {
    /// The parent's key, `None` when the ticket has none.
    pub current: Option<String>,
    /// The tracker's id for each ticket key read: the child, the target
    /// parent and the current one.
    ids: BTreeMap<String, String>,
}

/// What the tracker holds between a ticket and a would-be blocker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairRead {
    /// Whether the blocker blocks the ticket.
    pub blocks: bool,
    /// Any other relation the pair holds, which a Linear write would
    /// replace: its type, or the reversed `blocks`.
    pub other: Option<String>,
    blocked_id: String,
    blocker_id: String,
    /// The id of the `blocks` relation, on Linear.
    relation: Option<String>,
}

impl ParentRead {
    fn id(&self, key: &str) -> Result<&str> {
        self.ids
            .get(key)
            .map(String::as_str)
            .with_context(|| format!("the tracker has no ticket {key} it can read"))
    }
}

impl Relations {
    /// Reads `child`'s parent, and the ids a write making `target` its parent
    /// needs.
    pub fn read_parent(&self, child: &str, target: Option<&str>) -> Result<ParentRead> {
        match self {
            Relations::Github(g) => g.read_parent(child, target),
            Relations::Linear(l) => l.read_parent(child, target),
        }
    }

    /// Makes `child` a sub-issue of `to`, or of nothing, from the parent
    /// `read` found. On GitHub a child moving between parents leaves the old
    /// one first, so `addSubIssue` sends `replaceParent: false` and fails
    /// rather than take the child from a parent set since the read; with
    /// `replace` it sends `true` and takes it from whatever parent it has.
    pub fn set_parent(
        &self,
        read: &ParentRead,
        child: &str,
        to: Option<&str>,
        replace: bool,
    ) -> Result<()> {
        match self {
            Relations::Github(g) => g.set_parent(read, child, to, replace),
            Relations::Linear(l) => l.set_parent(read, child, to),
        }
    }

    /// Reads what links `blocked` and `blocker` on the tracker.
    pub fn read_pair(&self, blocked: &str, blocker: &str) -> Result<PairRead> {
        match self {
            Relations::Github(g) => g.read_pair(blocked, blocker),
            Relations::Linear(l) => l.read_pair(blocked, blocker),
        }
    }

    /// Adds, or with `add` false removes, the link recording that the
    /// blocker `read` names blocks its ticket.
    pub fn set_blocked(&self, read: &PairRead, add: bool) -> Result<()> {
        match self {
            Relations::Github(g) => g.set_blocked(read, add),
            Relations::Linear(l) => l.set_blocked(read, add),
        }
    }
}

/// Sends a GitHub GraphQL query or mutation.
type GithubSend = dyn Fn(&str) -> Result<Json> + Send + Sync;

/// Sub-issues and blocked-by links of the issues in one repository.
pub struct GithubRelations {
    repo: Repo,
    send: Box<GithubSend>,
}

impl GithubRelations {
    pub fn new(repo: Repo) -> Self {
        let api = Api::new(&repo.host);
        Self::with_transport(repo, move |query| {
            let source = api.token_source();
            let resp = api.graphql_or_gh(query, Api::graphql_value)?;
            github_fields::check_mutation(&resp, source)?;
            Ok(resp)
        })
    }

    /// Through `send`, which a test answers from recorded responses.
    pub fn with_transport(
        repo: Repo,
        send: impl Fn(&str) -> Result<Json> + Send + Sync + 'static,
    ) -> Self {
        GithubRelations {
            repo,
            send: Box::new(send),
        }
    }

    fn number(&self, key: &str) -> Result<u64> {
        github_status::number(key, &self.repo.slug)
    }

    /// One query reading each of `numbers` as `i<number>`, with `fields`
    /// added to the first.
    fn issues_query(&self, numbers: &[u64], fields: &str) -> String {
        let (owner, name) = self
            .repo
            .slug
            .split_once('/')
            .unwrap_or((&self.repo.slug, ""));
        let issues: Vec<String> = numbers
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let more = if i == 0 { fields } else { "" };
                format!("i{n}: issue(number: {n}) {{ id {more} }}")
            })
            .collect();
        format!(
            "query {{ repository(owner: {o}, name: {r}) {{ {} }} }}",
            issues.join(" "),
            o = Json::from(owner),
            r = Json::from(name),
        )
    }

    fn issue<'a>(&self, resp: &'a Json, number: u64) -> Result<&'a Json> {
        resp.pointer(&format!("/data/repository/i{number}"))
            .filter(|v| !v.is_null())
            .with_context(|| {
                format!(
                    "{}#{number} was not found, or the token cannot see it",
                    self.repo.slug
                )
            })
    }

    /// The key of the issue `v` names, an issue of this repository.
    fn key_of(&self, v: &Json) -> Option<String> {
        let repo = v.pointer("/repository/nameWithOwner")?.as_str()?;
        let number = v["number"].as_u64()?;
        Some(super::github_tickets::ticket_key(repo, number))
    }

    fn read_parent(&self, child: &str, target: Option<&str>) -> Result<ParentRead> {
        let child_n = self.number(child)?;
        let mut numbers = vec![child_n];
        if let Some(t) = target {
            numbers.push(self.number(t)?);
        }
        let resp = (self.send)(&self.issues_query(
            &numbers,
            "parent { id number repository { nameWithOwner } }",
        ))?;
        let issue = self.issue(&resp, child_n)?;
        let mut ids = BTreeMap::new();
        ids.insert(child.to_string(), id_of(issue)?);
        if let (Some(t), Some(n)) = (target, numbers.get(1)) {
            ids.insert(t.to_string(), id_of(self.issue(&resp, *n)?)?);
        }
        let parent = issue.get("parent").filter(|p| !p.is_null());
        let current = parent.and_then(|p| self.key_of(p));
        if let (Some(key), Some(p)) = (&current, parent) {
            ids.insert(key.clone(), id_of(p)?);
        }
        Ok(ParentRead { current, ids })
    }

    fn set_parent(
        &self,
        read: &ParentRead,
        child: &str,
        to: Option<&str>,
        replace: bool,
    ) -> Result<()> {
        let child_id = read.id(child)?;
        if let Some(from) = read.current.as_deref().filter(|_| to.is_none() || !replace)
            && Some(from) != to
        {
            (self.send)(&github_fields::remove_sub_issue_mutation(
                read.id(from)?,
                child_id,
            ))
            .with_context(|| format!("removing {child} from {from}'s sub-issues"))?;
        }
        if let Some(to) = to.filter(|t| read.current.as_deref() != Some(*t)) {
            (self.send)(&github_fields::add_sub_issue_mutation(
                read.id(to)?,
                child_id,
                replace,
            ))
            .with_context(|| format!("making {child} a sub-issue of {to}"))?;
        }
        Ok(())
    }

    fn read_pair(&self, blocked: &str, blocker: &str) -> Result<PairRead> {
        let (blocked_n, blocker_n) = (self.number(blocked)?, self.number(blocker)?);
        let resp = (self.send)(&self.issues_query(
            &[blocked_n, blocker_n],
            "blockedBy(first: 100) { nodes { number repository { nameWithOwner } } }",
        ))?;
        let issue = self.issue(&resp, blocked_n)?;
        let blocker_key = super::github_tickets::ticket_key(&self.repo.slug, blocker_n);
        let blocks = issue
            .pointer("/blockedBy/nodes")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|b| self.key_of(b))
            .any(|k| k == blocker_key);
        Ok(PairRead {
            blocks,
            other: None,
            blocked_id: id_of(issue)?,
            blocker_id: id_of(self.issue(&resp, blocker_n)?)?,
            relation: None,
        })
    }

    fn set_blocked(&self, read: &PairRead, add: bool) -> Result<()> {
        (self.send)(&github_fields::blocked_by_mutation(
            &read.blocked_id,
            &read.blocker_id,
            add,
        ))
        .map(drop)
    }
}

fn id_of(v: &Json) -> Result<String> {
    v["id"]
        .as_str()
        .map(String::from)
        .context("an issue without a node id")
}

/// Parents and `blocks` relations of Linear issues.
pub struct LinearRelations {
    post: Box<Transport>,
}

/// The `TEAM-NUMBER` form `issue(id:)` takes.
fn linear_id(key: &str) -> Result<String> {
    let (team, n) = parse_id(key).with_context(|| format!("`{key}` is not a Linear issue id"))?;
    Ok(format!("{team}-{n}"))
}

impl LinearRelations {
    pub fn new(key: String) -> Self {
        Self::with_transport(move |body, detail| send_graphql(body, &key, detail))
    }

    /// Through `post`, which a test answers from recorded responses.
    pub fn with_transport(
        post: impl Fn(Json, &str) -> Result<Json> + Send + Sync + 'static,
    ) -> Self {
        LinearRelations {
            post: Box::new(post),
        }
    }

    fn query(&self, query: &str, detail: &str) -> Result<Json> {
        graphql_errors((self.post)(
            json!({ "query": query, "variables": {} }),
            detail,
        )?)
    }

    fn mutate(&self, query: &str, variables: Json, op: &str) -> Result<()> {
        let resp = graphql_errors((self.post)(
            json!({ "query": query, "variables": variables }),
            op,
        )?)?;
        anyhow::ensure!(
            resp.pointer(&format!("/data/{op}/success")) == Some(&Json::Bool(true)),
            "Linear did not confirm `{op}`"
        );
        Ok(())
    }

    fn read_parent(&self, child: &str, target: Option<&str>) -> Result<ParentRead> {
        let mut parts = vec![format!(
            "c: issue(id: \"{}\") {{ id identifier parent {{ id identifier }} }}",
            linear_id(child)?
        )];
        if let Some(t) = target {
            parts.push(format!(
                "t: issue(id: \"{}\") {{ id identifier }}",
                linear_id(t)?
            ));
        }
        let resp = self.query(&format!("query {{ {} }}", parts.join(" ")), "issue parent")?;
        let issue = linear_issue(&resp, "c", child)?;
        let mut ids = BTreeMap::new();
        ids.insert(child.to_string(), id_of(issue)?);
        if let Some(t) = target {
            ids.insert(t.to_string(), id_of(linear_issue(&resp, "t", t)?)?);
        }
        let current = issue
            .pointer("/parent/identifier")
            .and_then(Json::as_str)
            .map(String::from);
        Ok(ParentRead { current, ids })
    }

    fn set_parent(&self, read: &ParentRead, child: &str, to: Option<&str>) -> Result<()> {
        let parent = to.map(|t| read.id(t)).transpose()?;
        self.mutate(
            UPDATE_MUTATION,
            json!({ "id": read.id(child)?, "input": { "parentId": parent } }),
            "issueUpdate",
        )
    }

    fn read_pair(&self, blocked: &str, blocker: &str) -> Result<PairRead> {
        let query = format!(
            "query {{ b: issue(id: \"{}\") {{ id identifier \
             relations(first: 250) {{ nodes {{ id type relatedIssue {{ identifier }} }} }} \
             inverseRelations(first: 250) {{ nodes {{ id type issue {{ identifier }} }} }} }} \
             r: issue(id: \"{}\") {{ id identifier }} }}",
            linear_id(blocked)?,
            linear_id(blocker)?
        );
        let resp = self.query(&query, "issue relations")?;
        let issue = linear_issue(&resp, "b", blocked)?;
        let other_issue = linear_issue(&resp, "r", blocker)?;
        let blocker_now = other_issue["identifier"].as_str().unwrap_or(blocker);
        let blocked_now = issue["identifier"].as_str().unwrap_or(blocked);
        let nodes = |at: &str| -> Vec<Json> {
            issue
                .pointer(at)
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let names = |r: &Json, at: &str| r.pointer(at).and_then(Json::as_str) == Some(blocker_now);
        let incoming: Vec<Json> = nodes("/inverseRelations/nodes")
            .into_iter()
            .filter(|r| names(r, "/issue/identifier"))
            .collect();
        let outgoing: Vec<Json> = nodes("/relations/nodes")
            .into_iter()
            .filter(|r| names(r, "/relatedIssue/identifier"))
            .collect();
        let relation = incoming
            .iter()
            .find(|r| r["type"] == "blocks")
            .and_then(|r| r["id"].as_str())
            .map(String::from);
        let other = incoming
            .iter()
            .filter(|r| r["type"] != "blocks")
            .map(|r| r["type"].as_str().unwrap_or("a relation").to_string())
            .chain(outgoing.iter().map(|r| match r["type"].as_str() {
                Some("blocks") => format!("{blocked_now} blocks {blocker_now}"),
                Some(other) => other.to_string(),
                None => "a relation".to_string(),
            }))
            .next();
        Ok(PairRead {
            blocks: relation.is_some(),
            other,
            blocked_id: id_of(issue)?,
            blocker_id: id_of(other_issue)?,
            relation,
        })
    }

    fn set_blocked(&self, read: &PairRead, add: bool) -> Result<()> {
        if add {
            return self.mutate(
                RELATE_MUTATION,
                json!({ "input": blocks_input(&read.blocker_id, &read.blocked_id) }),
                "issueRelationCreate",
            );
        }
        match &read.relation {
            Some(id) => self.mutate(
                UNRELATE_MUTATION,
                json!({ "id": id }),
                "issueRelationDelete",
            ),
            None => Ok(()),
        }
    }
}

/// The issue a Linear read aliased `alias`, or an error naming `key`.
fn linear_issue<'a>(resp: &'a Json, alias: &str, key: &str) -> Result<&'a Json> {
    resp.pointer(&format!("/data/{alias}"))
        .filter(|v| !v.is_null())
        .with_context(|| format!("Linear has no issue {key}"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A GitHub writer for `o/r` answering every request with `answer`, and
    /// the requests it was sent.
    fn github(answer: Json) -> (GithubRelations, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let repo = Repo {
            host: "github.com".into(),
            slug: "o/r".into(),
        };
        let writer = GithubRelations::with_transport(repo, move |query| {
            log.lock().unwrap().push(query.to_string());
            Ok(answer.clone())
        });
        (writer, sent)
    }

    fn mutations(sent: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        sent.lock()
            .unwrap()
            .iter()
            .filter(|q| q.starts_with("mutation"))
            .cloned()
            .collect()
    }

    /// Issue 67 under `parent`, and issues 65 and 66, as a read answers.
    fn issues_with_parent(parent: Option<u64>) -> Json {
        let parent = parent.map(|n| {
            json!({ "id": format!("I_{n}"), "number": n, "repository": { "nameWithOwner": "o/r" } })
        });
        json!({ "data": { "repository": {
            "i67": { "id": "I_67", "parent": parent },
            "i65": { "id": "I_65" },
            "i66": { "id": "I_66" },
        }}})
    }

    #[test]
    fn add_sub_issue_sends_replace_parent_false() {
        let (writer, sent) = github(issues_with_parent(None));
        let read = writer.read_parent("o/r#67", Some("o/r#65")).unwrap();
        assert_eq!(read.current, None);
        writer
            .set_parent(&read, "o/r#67", Some("o/r#65"), false)
            .unwrap();
        assert_eq!(mutations(&sent), [
            "mutation { addSubIssue(input: {issueId: \"I_65\", subIssueId: \"I_67\", \
             replaceParent: false}) { issue { id } } }"
        ]);

        let (writer, sent) = github(issues_with_parent(Some(66)));
        let read = writer.read_parent("o/r#67", Some("o/r#65")).unwrap();
        assert_eq!(read.current.as_deref(), Some("o/r#66"));
        writer
            .set_parent(&read, "o/r#67", Some("o/r#65"), true)
            .unwrap();
        let sent = mutations(&sent);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains("replaceParent: true"), "{sent:?}");
    }

    #[test]
    fn a_move_between_parents_leaves_the_old_one_first() {
        let (writer, sent) = github(issues_with_parent(Some(66)));
        let read = writer.read_parent("o/r#67", Some("o/r#65")).unwrap();
        writer
            .set_parent(&read, "o/r#67", Some("o/r#65"), false)
            .unwrap();
        let sent = mutations(&sent);
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(
            sent[0].contains("removeSubIssue(input: {issueId: \"I_66\", subIssueId: \"I_67\"})"),
            "{sent:?}"
        );
        assert!(
            sent[1].contains("issueId: \"I_65\", subIssueId: \"I_67\", replaceParent: false"),
            "{sent:?}"
        );
    }

    #[test]
    fn remove_sub_issue_names_both_ids() {
        let (writer, sent) = github(issues_with_parent(Some(65)));
        let read = writer.read_parent("o/r#67", None).unwrap();
        assert_eq!(read.current.as_deref(), Some("o/r#65"));
        writer.set_parent(&read, "o/r#67", None, false).unwrap();
        assert_eq!(mutations(&sent), [
            "mutation { removeSubIssue(input: {issueId: \"I_65\", subIssueId: \"I_67\"}) { issue \
             { id } } }"
        ]);
    }

    #[test]
    fn blocked_by_add_and_remove_name_the_blocker() {
        let answer = json!({ "data": { "repository": {
            "i65": { "id": "I_65", "blockedBy": { "nodes": [
                { "number": 66, "repository": { "nameWithOwner": "O/R" } }
            ]}},
            "i66": { "id": "I_66" },
        }}});
        let (writer, sent) = github(answer);
        let read = writer.read_pair("o/r#65", "o/r#66").unwrap();
        assert!(read.blocks && read.other.is_none(), "{read:?}");
        writer.set_blocked(&read, true).unwrap();
        writer.set_blocked(&read, false).unwrap();
        assert_eq!(mutations(&sent), [
            "mutation { addBlockedBy(input: {issueId: \"I_65\", blockingIssueId: \"I_66\"}) { \
             issue { id } } }",
            "mutation { removeBlockedBy(input: {issueId: \"I_65\", blockingIssueId: \"I_66\"}) \
             { issue { id } } }",
        ]);
    }

    /// A Linear writer answering each request with the first of `answers`
    /// whose needle its query holds, and the bodies it was sent.
    fn linear(answers: Vec<(&'static str, Json)>) -> (LinearRelations, Arc<Mutex<Vec<Json>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let writer = LinearRelations::with_transport(move |body, _| {
            let query = body["query"].as_str().unwrap_or_default().to_string();
            log.lock().unwrap().push(body);
            answers
                .iter()
                .find(|(needle, _)| query.contains(needle))
                .map(|(_, answer)| answer.clone())
                .with_context(|| format!("no answer for {query}"))
        });
        (writer, sent)
    }

    /// ENG-2, whose relations are `outgoing` and `incoming`, and ENG-1.
    fn pair(outgoing: Json, incoming: Json) -> Json {
        json!({ "data": {
            "b": { "id": "uuid-2", "identifier": "ENG-2",
                   "relations": { "nodes": outgoing },
                   "inverseRelations": { "nodes": incoming } },
            "r": { "id": "uuid-1", "identifier": "ENG-1" },
        }})
    }

    fn confirmed(op: &str) -> Json {
        json!({ "data": { op: { "success": true } } })
    }

    #[test]
    fn linear_blocks_runs_from_blocker_to_blocked() {
        let (writer, sent) = linear(vec![
            ("relations(first", pair(json!([]), json!([]))),
            ("issueRelationCreate", confirmed("issueRelationCreate")),
        ]);
        let read = writer.read_pair("ENG-2", "ENG-1").unwrap();
        assert!(!read.blocks && read.other.is_none(), "{read:?}");
        writer.set_blocked(&read, true).unwrap();
        let sent = sent.lock().unwrap();
        assert_eq!(
            sent[1]["variables"]["input"],
            json!({ "issueId": "uuid-1", "relatedIssueId": "uuid-2", "type": "blocks" })
        );
    }

    #[test]
    fn linear_unblock_deletes_the_blocks_relation() {
        let incoming =
            json!([{ "id": "rel-9", "type": "blocks", "issue": { "identifier": "ENG-1" } }]);
        let (writer, sent) = linear(vec![
            ("relations(first", pair(json!([]), incoming)),
            ("issueRelationDelete", confirmed("issueRelationDelete")),
        ]);
        let read = writer.read_pair("ENG-2", "ENG-1").unwrap();
        assert!(read.blocks && read.other.is_none(), "{read:?}");
        writer.set_blocked(&read, false).unwrap();
        assert_eq!(
            sent.lock().unwrap()[1]["variables"],
            json!({ "id": "rel-9" })
        );
    }

    #[test]
    fn linear_parent_null_removes_it() {
        let read = json!({ "data": {
            "c": { "id": "uuid-2", "identifier": "ENG-2", "parent": { "id": "uuid-1", "identifier": "ENG-1" } },
        }});
        let (writer, sent) = linear(vec![
            ("parent {", read),
            ("issueUpdate", confirmed("issueUpdate")),
        ]);
        let read = writer.read_parent("ENG-2", None).unwrap();
        assert_eq!(read.current.as_deref(), Some("ENG-1"));
        writer.set_parent(&read, "ENG-2", None).unwrap();
        assert_eq!(
            sent.lock().unwrap()[1]["variables"],
            json!({ "id": "uuid-2", "input": { "parentId": null } })
        );
    }

    #[test]
    fn linear_parent_names_the_new_parents_id() {
        let read = json!({ "data": {
            "c": { "id": "uuid-2", "identifier": "ENG-2", "parent": null },
            "t": { "id": "uuid-1", "identifier": "ENG-1" },
        }});
        let (writer, sent) = linear(vec![
            ("parent {", read),
            ("issueUpdate", confirmed("issueUpdate")),
        ]);
        let read = writer.read_parent("ENG-2", Some("ENG-1")).unwrap();
        assert_eq!(read.current, None);
        writer.set_parent(&read, "ENG-2", Some("ENG-1")).unwrap();
        assert_eq!(
            sent.lock().unwrap()[1]["variables"],
            json!({ "id": "uuid-2", "input": { "parentId": "uuid-1" } })
        );
    }

    #[test]
    fn linear_other_relation_finds_related_in_either_direction() {
        let related =
            |at: &str| json!([{ "id": "rel-1", "type": "related", at: { "identifier": "ENG-1" } }]);
        for (outgoing, incoming) in [
            (related("relatedIssue"), json!([])),
            (json!([]), related("issue")),
        ] {
            let (writer, _) = linear(vec![("relations(first", pair(outgoing, incoming))]);
            let read = writer.read_pair("ENG-2", "ENG-1").unwrap();
            assert_eq!(read.other.as_deref(), Some("related"));
            assert!(!read.blocks);
        }
    }

    #[test]
    fn linear_other_relation_counts_a_reversed_blocks() {
        let reversed =
            json!([{ "id": "rel-1", "type": "blocks", "relatedIssue": { "identifier": "ENG-1" } }]);
        let (writer, _) = linear(vec![("relations(first", pair(reversed, json!([])))]);
        let read = writer.read_pair("ENG-2", "ENG-1").unwrap();
        assert_eq!(read.other.as_deref(), Some("ENG-2 blocks ENG-1"));
        assert!(!read.blocks);
    }

    #[test]
    fn linear_resolved_blocks_is_not_another_relation() {
        let blocks =
            json!([{ "id": "rel-1", "type": "blocks", "issue": { "identifier": "ENG-1" } }]);
        let (writer, _) = linear(vec![("relations(first", pair(json!([]), blocks))]);
        let read = writer.read_pair("ENG-2", "ENG-1").unwrap();
        assert!(read.blocks);
        assert_eq!(read.other, None);
    }

    #[test]
    fn linear_relations_of_other_issues_do_not_count() {
        let elsewhere = json!([{ "id": "rel-1", "type": "related", "relatedIssue": { "identifier": "ENG-3" } }]);
        let (writer, _) = linear(vec![("relations(first", pair(elsewhere, json!([])))]);
        let read = writer.read_pair("ENG-2", "ENG-1").unwrap();
        assert_eq!(read.other, None);
    }
}
