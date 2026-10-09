//! `repo-rules-agent`'s store over a Supabase project's Data API: the
//! `repo_rules_api` functions, behind row-level security on the signed-in
//! user. devkit holds no copy of their SQL and codes against the contract
//! the rules design spec records.

use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use devkit_supabase::{Api, Refused};
use serde_json::{Map, Value, json};

use crate::{
    edit::{Fields, apply, rule_id},
    model::{Rule, RuleFile, RuleIndex},
    postgres::{MANUAL_SOURCE, is_uuid},
    remote::RemoteRules,
    source::RuleSource,
};

/// The page size `query_rules` defaults to.
const PAGE: u32 = 100;

/// How many times a pull starts over when the rules change under it.
const PULL_ATTEMPTS: usize = 3;

/// The fields `put_rule` takes, all of them on every call.
const EDITABLE: [&str; 8] = [
    "title",
    "description",
    "category",
    "scope",
    "severity",
    "directory",
    "tasks",
    "languages",
];

/// The rule source over one repository in the rules API.
pub struct SupabaseSource {
    api: Arc<Api>,
    /// The repository's UUID, lowercased, or why there is none.
    repository: Result<String, String>,
    /// The local checkout the rules describe, reported as the index's `repo`.
    repo: String,
    page: u32,
}

/// A live rule and the key the store knows it by.
struct Keyed {
    key: String,
    rule: Rule,
}

/// What a function answered, once its refusals are read.
enum Answer {
    Done(Value),
    Conflict,
}

impl SupabaseSource {
    /// The repository `repository` names in `api`, describing the checkout
    /// at `repo`.
    pub fn new(api: Arc<Api>, repository: Option<&str>, repo: &Path) -> SupabaseSource {
        let repository = match repository.map(str::trim) {
            None | Some("") => Err("[rules.supabase] repository is not set".to_string()),
            Some(id) if is_uuid(id) => Ok(id.to_ascii_lowercase()),
            Some(id) => Err(format!("[rules.supabase] repository {id:?} is not a UUID")),
        };
        SupabaseSource {
            api,
            repository,
            repo: repo.display().to_string(),
            page: PAGE,
        }
    }

    /// This source, asking for `n` rules a page.
    #[doc(hidden)]
    pub fn with_page_size(self, n: u32) -> SupabaseSource {
        SupabaseSource { page: n, ..self }
    }

    /// The repository's UUID, lowercased, when the config names a valid one.
    pub fn repository_id(&self) -> Option<&str> {
        self.repository.as_deref().ok()
    }

    pub fn api(&self) -> &Api {
        &self.api
    }

    fn repository(&self) -> Result<&str> {
        self.repository
            .as_deref()
            .map_err(|reason| anyhow!("{reason}"))
    }

    /// Calls `function` and reads its answer: a refusal or a missing
    /// repository is an error, and a revision conflict is
    /// [`Answer::Conflict`].
    fn call(&self, function: &str, args: Value) -> Result<Answer> {
        let repo = self.repository()?;
        let answer: Value = self
            .api
            .call(function, &args)
            .and_then(|resp| self.api.body(resp))
            .map_err(|e| self.refusal(e))?;
        match answer.get("error").and_then(Value::as_str) {
            None => Ok(Answer::Done(answer)),
            Some("conflict") => Ok(Answer::Conflict),
            Some("not_found") => bail!(
                "repository {repo} is not in the rules API, or you have no access to it; \
                 check repository_members"
            ),
            Some(other) => bail!(
                "{} {} answered {function} with error {other:?}",
                self.api.label(),
                self.api.url()
            ),
        }
    }

    /// `e` with the store's own refusal codes read.
    fn refusal(&self, e: anyhow::Error) -> anyhow::Error {
        let Some(refused) = e.downcast_ref::<Refused>() else {
            return e;
        };
        let repo = self.repository.as_deref().unwrap_or("?");
        match refused.code.as_deref() {
            Some("42501") => e.context(format!("your role cannot edit repository {repo}")),
            Some("22023") => anyhow!("{}", refused.message),
            _ => e,
        }
    }

    /// The live rules with their keys, and the revision they were read at.
    fn pull_keyed(&self) -> Result<(i64, Vec<Keyed>)> {
        let repo = self.repository()?;
        for _ in 0..PULL_ATTEMPTS {
            let mut args = json!({"p_repo_id": repo, "p_limit": self.page});
            let mut revision = None;
            let mut rules = Vec::new();
            let finished = loop {
                let Answer::Done(page) = self.call("query_rules", args.clone())? else {
                    break false;
                };
                let page_revision = page
                    .get("revision")
                    .and_then(Value::as_i64)
                    .with_context(|| self.unrecognised("query_rules"))?;
                revision.get_or_insert(page_revision);
                let payloads = page
                    .get("rules")
                    .and_then(Value::as_array)
                    .with_context(|| self.unrecognised("query_rules"))?;
                for payload in payloads {
                    rules.push(keyed(payload).with_context(|| self.unrecognised("query_rules"))?);
                }
                match page.get("continuation") {
                    None | Some(Value::Null) => break true,
                    Some(next) => {
                        for (arg, field) in [
                            ("p_after_position", "after_position"),
                            ("p_after_rule_key", "after_rule_key"),
                            ("p_expected_revision", "expected_revision"),
                        ] {
                            let value = next
                                .get(field)
                                .with_context(|| self.unrecognised("query_rules"))?;
                            args[arg] = value.clone();
                        }
                    }
                }
            };
            if finished {
                return Ok((revision.unwrap_or_default(), rules));
            }
        }
        bail!("rules changed during the pull {PULL_ATTEMPTS} times; try again")
    }

    fn unrecognised(&self, function: &str) -> String {
        format!(
            "{} {} answered an unrecognised {function} shape",
            self.api.label(),
            self.api.url()
        )
    }

    fn index(&self, rules: Vec<Keyed>) -> RuleIndex {
        let rules: Vec<Rule> = rules.into_iter().map(|k| k.rule).collect();
        let mut files: Vec<RuleFile> = Vec::new();
        for rule in &rules {
            if rule.source_file != MANUAL_SOURCE
                && !files.iter().any(|f| f.path == rule.source_file)
            {
                files.push(RuleFile {
                    path: rule.source_file.clone(),
                    tier: 0,
                    applies_to: String::new(),
                    errors: Vec::new(),
                    content: None,
                });
            }
        }
        RuleIndex {
            repo: self.repo.clone(),
            files,
            rules,
        }
    }

    /// Runs `change` against freshly pulled rules: the call it makes, and
    /// once more on fresh rules when the revision moved under it.
    fn change(
        &self,
        change: impl Fn(i64, &[Keyed]) -> Result<(&'static str, Value)>,
    ) -> Result<Value> {
        for _ in 0..2 {
            let (revision, rules) = self.pull_keyed()?;
            let (function, args) = change(revision, &rules)?;
            if let Answer::Done(answer) = self.call(function, args)? {
                return Ok(answer);
            }
        }
        bail!(
            "repository {} changed during the edit twice (a revision conflict); try again",
            self.repository()?
        )
    }

    /// The one live rule `id` names.
    fn one<'a>(&self, rules: &'a [Keyed], id: &str) -> Result<&'a Keyed> {
        let repo = self.repository()?;
        let found: Vec<&Keyed> = rules.iter().filter(|k| k.rule.id == id).collect();
        match found.as_slice() {
            [] => bail!("no rule {id} in repository {repo}"),
            [one] => Ok(one),
            many => bail!(
                "{} rules in repository {repo} share the id {id} (rule keys {}); \
                 edit them through repo-rules-agent",
                many.len(),
                many.iter()
                    .map(|k| k.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

/// A `query_rules` payload as a rule and its key.
fn keyed(payload: &Value) -> Result<Keyed> {
    let object = payload
        .as_object()
        .context("a rule that is not an object")?;
    let text = |field: &str| -> Result<String> {
        match object.get(field) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => bail!("a rule's {field} is not a string"),
        }
    };
    let list = |field: &str| -> Result<Vec<String>> {
        match object.get(field) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(value) => serde_json::from_value(value.clone())
                .with_context(|| format!("a rule's {field} is not a list of strings")),
        }
    };
    let flag = |field: &str| object.get(field).and_then(Value::as_bool).unwrap_or(false);
    let key = text("rule_key")?;
    if key.is_empty() {
        bail!("a rule with no rule_key");
    }
    Ok(Keyed {
        key,
        rule: Rule {
            id: text("id")?,
            title: text("title")?,
            description: text("description")?,
            category: text("category")?,
            tasks: list("tasks")?,
            languages: list("languages")?,
            topics: list("topics")?,
            scope_raw: text("scope")?,
            severity_raw: text("severity")?,
            source_file: text("source_file")?,
            directory: text("directory")?,
            pinned: flag("pinned"),
            removed: flag("removed"),
        },
    })
}

/// The eight fields `put_rule` takes, from `rule`.
fn payload(rule: &Rule) -> Value {
    let mut payload = Map::new();
    for field in EDITABLE {
        let value = match field {
            "title" => json!(rule.title),
            "description" => json!(rule.description),
            "category" => json!(rule.category),
            "scope" => json!(rule.scope_raw),
            "severity" => json!(rule.severity_raw),
            "directory" => json!(rule.directory),
            "tasks" => json!(rule.tasks),
            "languages" => json!(rule.languages),
            _ => unreachable!("every editable field is listed"),
        };
        payload.insert(field.to_string(), value);
    }
    Value::Object(payload)
}

/// Refuses `fields` that set topics, which `put_rule` keeps among the
/// fields it does not let a caller change.
fn refuse_topics(fields: &Fields) -> Result<()> {
    if fields.topics.is_some() {
        bail!("topics change through repo-rules-agent on the supabase source");
    }
    Ok(())
}

impl RemoteRules for SupabaseSource {
    fn revision(&self) -> Result<i64> {
        let repo = self.repository()?;
        let Answer::Done(stats) = self.call("stats_rules", json!({"p_repo_id": repo}))? else {
            bail!(self.unrecognised("stats_rules"));
        };
        stats
            .get("revision")
            .and_then(Value::as_i64)
            .with_context(|| self.unrecognised("stats_rules"))
    }

    fn pull(&self) -> Result<(i64, RuleIndex)> {
        let (revision, rules) = self.pull_keyed()?;
        Ok((revision, self.index(rules)))
    }

    fn checkout(&self) -> &str {
        &self.repo
    }

    fn identity(&self) -> String {
        self.api.identity()
    }
}

impl RuleSource for SupabaseSource {
    fn read(&self) -> Result<Option<RuleIndex>> {
        self.pull().map(|(_, index)| Some(index))
    }

    fn add(&self, _repo: &str, fields: Fields) -> Result<String> {
        refuse_topics(&fields)?;
        let repo = self.repository()?;
        let title = fields.title.clone().unwrap_or_default();
        let id = rule_id(MANUAL_SOURCE, &title);
        let mut rule = Rule::default_extracted();
        rule.id = id.clone();
        rule.source_file = MANUAL_SOURCE.to_string();
        let rule = apply(rule, fields)?;
        self.change(|revision, rules| {
            if rules.iter().any(|k| k.rule.id == id) {
                bail!("rule {id} already exists; change it with `devkit rules edit {id}`");
            }
            Ok((
                "put_rule",
                json!({
                    "p_repo_id": repo,
                    "p_rule_key": Value::Null,
                    "p_expected_revision": revision,
                    "p_payload": payload(&rule),
                }),
            ))
        })?;
        Ok(id)
    }

    fn edit(&self, id: &str, fields: Fields) -> Result<()> {
        refuse_topics(&fields)?;
        if fields.is_empty() {
            bail!("nothing to change: pass at least one field to set");
        }
        let repo = self.repository()?;
        self.change(|revision, rules| {
            let target = self.one(rules, id)?;
            let rule = apply(target.rule.clone(), fields.clone())?;
            Ok((
                "put_rule",
                json!({
                    "p_repo_id": repo,
                    "p_rule_key": target.key,
                    "p_expected_revision": revision,
                    "p_payload": payload(&rule),
                }),
            ))
        })?;
        Ok(())
    }

    fn remove(&self, id: &str) -> Result<()> {
        let repo = self.repository()?;
        self.change(|revision, rules| {
            let target = self.one(rules, id)?;
            Ok((
                "remove_rule",
                json!({
                    "p_repo_id": repo,
                    "p_rule_key": target.key,
                    "p_expected_revision": revision,
                }),
            ))
        })?;
        Ok(())
    }

    fn location(&self) -> String {
        match &self.repository {
            Ok(repo) => format!("{}, repository {repo}", self.api.url()),
            Err(_) => format!("{}, no repository", self.api.url()),
        }
    }

    fn kind(&self) -> &'static str {
        "supabase"
    }
}
