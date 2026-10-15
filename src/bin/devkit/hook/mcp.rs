//! The MCP path of `pre-tool-use`: an issue write through a tracker's MCP tool
//! is allowed only when its title and body are text `devkit ticket render`
//! produced in the same agent session, and a PR write through a forge's MCP
//! tool only when they are text `devkit pr render` produced.

use std::{
    collections::{BTreeMap, HashMap},
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::Result;
use devkit_common::{
    caller::Caller,
    fields::{self, Given, Naming, Problem, Raw, Resolved, Surface},
    harness, required,
    vcs::Checkout,
};
use devkit_config::{Config, FieldDecl, FieldType, Person, RenderedToolRule, TrackerKind};
use pabal::Tool;
use serde_json::Value;

use super::{
    payload::{self, Payload},
    print_envelope,
};
use crate::issue::{
    pr,
    receipt::{self, Field, Kind},
    render,
};

const PANIC_REASON: &str =
    "devkit render guard: internal failure while checking an issue or PR write (fail-closed)";

/// What a matched rule writes, and the render whose receipts vouch for it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Gate {
    kind: Kind,
    with_article: &'static str,
    noun: &'static str,
    render: &'static str,
    flags: &'static str,
}

pub(super) const ISSUE: Gate = Gate {
    kind: Kind::Issue,
    with_article: "an issue",
    noun: "issue",
    render: "devkit ticket render",
    flags: "--title ... [--body ...]",
};

pub(super) const PR: Gate = Gate {
    kind: Kind::Pr,
    with_article: "a PR",
    noun: "PR",
    render: "devkit pr render",
    flags: "--pr-title ... [--pr-body ...]",
};

#[derive(Debug, PartialEq)]
pub(super) enum Verdict {
    Allow,
    /// Allow, with a report the agent should read.
    AllowWith(String),
    Deny(String),
}

/// What the decision needs beyond the call itself, resolved by the caller so
/// `decide` stays free of IO.
pub(super) struct Context<'a> {
    /// What the matched rule writes.
    pub gate: Gate,
    /// The required `--arg`s the gate's templates ask of an agent, appended to
    /// a missing-receipt denial. Called only for that denial, since building
    /// it loads the config a second time.
    pub hint: &'a dyn Fn() -> String,
    /// Whether this session has rendered anything, which turns "not rendered"
    /// into "differs from what was rendered".
    pub session_seen: bool,
}

/// Guard an MCP call. Never returns an error and writes no log record. Until
/// a rule matches, every failure allows, since a project with no rule for
/// this tool has nothing to enforce; after a match every failure denies,
/// panics included, because an enforcement rule that fails open enforces
/// nothing.
pub(super) fn guard(payload: &Payload, checkout: &Checkout, cwd: &Path) -> Result<()> {
    let matched: OnceLock<()> = OnceLock::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        respond(payload, &matched, checkout, cwd)
    }));
    match outcome {
        Ok(Verdict::Deny(reason)) => print_envelope(&payload::deny(payload.harness(), &reason)),
        Ok(Verdict::Allow) => {}
        Ok(Verdict::AllowWith(report)) => {
            if let Some(answer) = payload.pre_tool_use_context(&report) {
                print_envelope(&answer);
            }
        }
        Err(_) if matched.get().is_some() => {
            print_envelope(&payload::deny(payload.harness(), PANIC_REASON))
        }
        Err(_) => {}
    }
    let _ = std::io::stdout().flush();
    Ok(())
}

fn respond(payload: &Payload, matched: &OnceLock<()>, checkout: &Checkout, cwd: &Path) -> Verdict {
    let Some(Tool::Mcp {
        server,
        tool,
        input,
    }) = payload.tool()
    else {
        return Verdict::Allow;
    };
    let (rules, _) = harness::resolve_rules_in(checkout, cwd);
    let Some((rule, gate)) = [(&rules.issue_tools, ISSUE), (&rules.pr_tools, PR)]
        .into_iter()
        .find_map(|(table, gate)| {
            table
                .values()
                .find(|r| r.enabled && r.matches(server, tool))
                .map(|r| (r, gate))
        })
    else {
        return Verdict::Allow;
    };
    let _ = matched.set(());
    let render = gate.render;

    let Some(session) = payload.session_id() else {
        return Verdict::Deny(format!(
            "devkit render guard: this `{tool}` call writes {}, and the payload carries no \
             session id to check its `{render}` receipts against.",
            gate.with_article
        ));
    };
    if !receipt::valid_session(session) {
        return Verdict::Deny(format!(
            "devkit render guard: session id `{session}` is not usable as a directory name, so \
             no `{render}` receipt can match it."
        ));
    }
    let Some(root) = receipt::store_root(checkout) else {
        return Verdict::Deny(format!(
            "devkit render guard: {} is not inside a git checkout, where `{render}` keeps its \
             receipts.",
            cwd.display()
        ));
    };
    let ctx = Context {
        gate,
        hint: &|| required_hint(checkout, cwd, gate.kind),
        session_seen: receipt::session_dir(&root, gate.kind, session).is_dir(),
    };
    let server = server.unwrap_or(tool);
    let rendered = decide(rule, server, tool, input, &ctx, &|field, text| {
        receipt::has(&root, gate.kind, session, field, text)
    })
    .unwrap_or_else(|e| Verdict::Deny(format!("devkit render guard: {e:#}")));
    if rendered != Verdict::Allow || gate.kind != Kind::Issue || rule.tracker.is_none() {
        return rendered;
    }
    match devkit_common::config::resolve_in(checkout, None, cwd) {
        Ok((cfg, prov)) => field_verdict(rule, tool, input, &cfg, cwd, &prov.origin, Caller::Agent),
        Err(e) => Verdict::Deny(format!(
            "devkit ticket fields: this `{tool}` call writes a ticket, and devkit.toml, which \
             declares the ticket fields it must carry, does not load: {e:#}"
        )),
    }
}

/// The `--arg`s the templates of `kind` require of an agent, as a sentence to
/// append to a denial, or empty when none are required or the config cannot
/// say.
fn required_hint(checkout: &Checkout, cwd: &Path, kind: Kind) -> String {
    let Ok((cfg, _)) = devkit_common::config::resolve_in(checkout, None, cwd) else {
        return String::new();
    };
    let missing = match kind {
        Kind::Issue => render::missing(
            &cfg,
            render::Title::Input(""),
            true,
            &BTreeMap::new(),
            Caller::Agent,
        ),
        Kind::Pr => pr::render::missing(&cfg, &BTreeMap::new(), Caller::Agent),
    };
    let missing = match missing {
        Ok(m) if !m.is_empty() => m,
        _ => return String::new(),
    };
    format!(
        " Pass: {}{}",
        missing
            .iter()
            .map(required::Missing::hint)
            .collect::<Vec<_>>()
            .join(" "),
        missing
            .iter()
            .filter_map(|m| Some(required::description_line(
                &m.name,
                m.description.as_deref()?
            )))
            .collect::<String>()
    )
}

/// The decision for a call `rule` matched. A create checks both the title
/// and the body, a missing one as empty text; an update checks only the ones
/// it carries, and refuses a `body_patch` edit outright.
pub(super) fn decide(
    rule: &RenderedToolRule,
    server: &str,
    tool: &str,
    input: &Value,
    ctx: &Context<'_>,
    receipt: &dyn Fn(Field, &str) -> Result<bool>,
) -> Result<Verdict> {
    let Gate {
        with_article,
        noun,
        render,
        flags,
        ..
    } = ctx.gate;
    if !input.is_object() {
        return Ok(Verdict::Deny(format!(
            "This `{server}` `{tool}` call's input is not a JSON object, so its `{}` and `{}` \
             cannot be checked against `{render}`.",
            rule.title, rule.body
        )));
    }
    let create = rule.is_create(input);
    if !create
        && let Some(patch) = rule
            .body_patch
            .iter()
            .find(|k| input.get(k.as_str()).is_some_and(|v| !v.is_null()))
    {
        return Ok(Verdict::Deny(format!(
            "This call edits the {noun} body in place with `{patch}`. Render the whole new body \
             with `{render}` and pass it as `{}`.",
            rule.body
        )));
    }
    for (field, key) in [(Field::Title, &rule.title), (Field::Body, &rule.body)] {
        let (text, absent) = match input.get(key.as_str()) {
            None | Some(Value::Null) if create => ("", true),
            None | Some(Value::Null) => continue,
            Some(Value::String(s)) => (s.as_str(), false),
            Some(_) => {
                return Ok(Verdict::Deny(format!(
                    "`{key}` is not a string, so `{render}` cannot have produced it."
                )));
            }
        };
        if !receipt(field, text)? {
            let output = match field {
                Field::Title => "title",
                Field::Body => "body",
            };
            let opening = if absent {
                format!(
                    "This call creates {with_article} with no `{key}`. Pass the `{output}` \
                     output of `{render}` as `{key}`."
                )
            } else if ctx.session_seen {
                format!(
                    "This call's `{key}` differs from what `{render}` produced in this session."
                )
            } else {
                format!(
                    "This `{server}` `{tool}` call writes {with_article} `{key}` that `{render}` \
                     did not produce in this session."
                )
            };
            return Ok(Verdict::Deny(format!(
                "{opening} Run `{render} {flags}` and pass its `title` output unchanged as `{}` \
                 and its `body` output as `{}`.{}",
                rule.title,
                rule.body,
                (ctx.hint)()
            )));
        }
    }
    Ok(Verdict::Allow)
}

/// Hold an issue write `rule` matched to the repository's ticket field
/// levels, from config, the call's input and `provenance`, the layer each
/// config key came from. A rule naming no `tracker`, a repository using
/// another tracker or declaring no fields, and a GitHub call to a repository
/// other than the issues repository are allowed here. A create carries every
/// field its level requires; an update is judged on the fields it carries. A
/// value is refused at every level.
pub(super) fn field_verdict(
    rule: &RenderedToolRule,
    tool: &str,
    input: &Value,
    cfg: &Config,
    cwd: &Path,
    provenance: &HashMap<String, PathBuf>,
    caller: Caller,
) -> Verdict {
    let Some(kind) = rule.tracker else {
        return Verdict::Allow;
    };
    let decls = cfg.ticket.fields.for_tracker(kind);
    let other_tracker = cfg.tracker.kind.is_some_and(|k| k != kind);
    if decls.is_empty()
        || other_tracker
        || !names_issues_repo(kind, cfg, input, || origin_slug(cwd))
    {
        return Verdict::Allow;
    }
    if let Err(e) = devkit_config::check_ticket_fields(cfg, kind, provenance) {
        return Verdict::Deny(format!("devkit ticket fields: {e:#}"));
    }
    let (given, users, mut problems) = given(rule, decls, kind, &cfg.people, input);
    let team = fields::team(decls, &given);
    let resolved = match fields::resolve(
        decls,
        kind,
        &given,
        &cfg.people,
        team.as_deref(),
        Naming::Verbatim,
    ) {
        Ok(mut r) => {
            r.fields.extend(users);
            r
        }
        Err(more) => {
            problems.extend(more);
            Resolved::default()
        }
    };
    if !problems.is_empty() {
        return Verdict::Deny(field_denial(rule, tool, kind, decls, &problems));
    }
    let surface = if rule.is_create(input) {
        Surface::McpCreate
    } else {
        Surface::McpUpdate
    };
    let report = fields::check(decls, surface, &resolved, None, caller);
    if !report.refused.is_empty() {
        return Verdict::Deny(field_denial(rule, tool, kind, decls, &report.refused));
    }
    if !report.warned.is_empty() {
        return Verdict::AllowWith(format!(
            "devkit ticket fields: this `{tool}` call leaves out warning-level fields of \
             [ticket.fields.{kind}].\n{}",
            blocks(rule, tool, decls, &report.warned)
        ));
    }
    Verdict::Allow
}

/// Whether a call to `kind` writes the repository's own tickets: on GitHub,
/// a call naming an `owner`/`repo` other than the issues repository, which
/// is `[github] issues_repo` or else the slug `origin_slug` reads, does not.
/// With neither, every call is checked.
fn names_issues_repo(
    kind: TrackerKind,
    cfg: &Config,
    input: &Value,
    origin_slug: impl FnOnce() -> Option<String>,
) -> bool {
    if kind != TrackerKind::Github {
        return true;
    }
    let part = |k: &str| input.get(k).and_then(Value::as_str);
    let (Some(owner), Some(name)) = (part("owner"), part("repo")) else {
        return true;
    };
    match cfg.github.issues_repo.clone().or_else(origin_slug) {
        Some(slug) => slug.eq_ignore_ascii_case(&format!("{owner}/{name}")),
        None => true,
    }
}

/// The owner/repo of the `origin` remote at `cwd`, read from the URL alone.
/// The pre-tool-use hook runs under a deadline that resolving an ssh alias
/// through `ssh -G` could outlast, so the remote's host is not checked.
fn origin_slug(cwd: &Path) -> Option<String> {
    let url = devkit_common::forge::remote::origin_url(&cwd.to_string_lossy()).ok()?;
    devkit_common::forge::remote::slug_from_remote_url(&url)
}

/// The call's mapped field values, the items of every key a field maps to
/// together, with a field whose keys carry only `null`, or `[]` for a list
/// field, as an explicit none; each `user` value already resolved, since the
/// call names a user by tracker identity where `--arg` takes an alias; and a
/// problem for each value of a JSON type the field does not take or a user
/// no `[people]` entry has as its identity.
fn given(
    rule: &RenderedToolRule,
    decls: &BTreeMap<String, FieldDecl>,
    kind: TrackerKind,
    people: &HashMap<String, Person>,
    input: &Value,
) -> (Given, BTreeMap<String, Vec<fields::Value>>, Vec<Problem>) {
    let mut out = Given::default();
    let mut users = BTreeMap::new();
    let mut problems = Vec::new();
    for (field, keys) in &rule.fields {
        let Some(decl) = decls.get(field) else {
            continue;
        };
        let ty = decl.r#type.unwrap_or(FieldType::String);
        let scalar = |v: &Value| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) if matches!(ty, FieldType::Number | FieldType::Enum) => {
                Some(n.to_string())
            }
            _ => None,
        };
        let (mut items, mut none) = (Vec::new(), false);
        for key in keys.keys() {
            let Some(value) = input.get(key.as_str()) else {
                continue;
            };
            let carried = match value {
                Value::Null => Some(Vec::new()),
                Value::Array(items) if decl.list => items.iter().map(scalar).collect(),
                Value::Array(_) => None,
                v => scalar(v).map(|s| vec![s]),
            };
            match carried {
                Some(carried) if carried.is_empty() => none = true,
                Some(carried) => items.extend(carried),
                None => problems.push(Problem::Invalid {
                    field: field.clone(),
                    value: value.to_string(),
                    reason: format!(
                        "is not a JSON value `{field}` takes: {} under `{key}`",
                        json_shape(ty, decl.list)
                    ),
                }),
            }
        }
        if items.is_empty() {
            if none {
                out.nones.insert(field.clone());
            }
            continue;
        }
        if ty != FieldType::User {
            out.values.insert(field.clone(), Raw::Items(items));
            continue;
        }
        let mut found = Vec::new();
        for item in items {
            match user(people, kind, &item) {
                Ok(v) => found.push(v),
                Err(reason) => problems.push(Problem::Invalid {
                    field: field.clone(),
                    value: item,
                    reason,
                }),
            }
        }
        users.insert(field.clone(), found);
    }
    (out, users, problems)
}

/// The JSON a field's input key takes.
fn json_shape(ty: FieldType, list: bool) -> String {
    let one = match ty {
        FieldType::Number | FieldType::Enum => "a number or a string",
        _ => "a string",
    };
    if list {
        format!("{one}, an array of them, or null or [] for none")
    } else {
        format!("{one}, or null")
    }
}

/// A user as a tracker MCP takes it: the identity on `kind` of a `[people]`
/// entry, ignoring case. Linear resolves `me`, an email address and a
/// display name itself, so anything but a user id passes there unchecked. A
/// bare `[people]` alias is refused naming the identity to send, since the
/// tracker would read it as a user of its own.
fn user(
    people: &HashMap<String, Person>,
    kind: TrackerKind,
    who: &str,
) -> Result<fields::Value, String> {
    let who = who.trim();
    let known = people.iter().find_map(|(alias, p)| {
        let id = fields::person_id(p, kind)?;
        id.eq_ignore_ascii_case(who).then(|| fields::Value::User {
            alias: alias.clone(),
            id: id.to_string(),
        })
    });
    if let Some(v) = known {
        return Ok(v);
    }
    let alias = people
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(who));
    if kind == TrackerKind::Linear && alias.is_none() && !is_uuid(who) {
        return Ok(fields::Value::User {
            alias: who.to_string(),
            id: who.to_string(),
        });
    }
    let identity = alias.map(|(alias, p)| (alias, fields::person_id(p, kind)));
    Err(match identity {
        Some((_, Some(id))) => format!(
            "is a `[people]` alias, and this call takes the user's {kind} identity: pass `{id}`"
        ),
        Some((alias, None)) => format!(
            "is a `[people]` alias with no `{kind}` identity; add `{kind} = \"...\"` to \
             `[people.{alias}]`"
        ),
        None => format!("is not the {kind} identity of any `[people]` entry"),
    })
}

/// Whether `s` is a UUID, the shape of a Linear id.
fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
}

fn field_denial(
    rule: &RenderedToolRule,
    tool: &str,
    kind: TrackerKind,
    decls: &BTreeMap<String, FieldDecl>,
    problems: &[Problem],
) -> String {
    format!(
        "devkit ticket fields: this `{tool}` call does not satisfy [ticket.fields.{kind}].\n{}\n\
         `devkit ticket create` and `devkit ticket edit` set every declared field, each as \
         `--arg <field>=...`; `devkit ticket fields` lists them.",
        blocks(rule, tool, decls, problems)
    )
}

/// The shared message for each problem, followed for a missing field by the
/// input key that sets it in this call, or by `ticket create` when the tool
/// has none.
fn blocks(
    rule: &RenderedToolRule,
    tool: &str,
    decls: &BTreeMap<String, FieldDecl>,
    problems: &[Problem],
) -> String {
    problems
        .iter()
        .map(|p| {
            let mut block = fields::message_for(decls, std::slice::from_ref(p), Naming::Verbatim);
            if let Problem::Missing { field } = p {
                let none = match decls.get(field).filter(|d| d.allows_none()) {
                    Some(d) if d.list => ", or `null` or `[]` to leave it empty",
                    Some(_) => ", or `null` to leave it empty",
                    None => "",
                };
                block.push_str(&match rule.fields.get(field) {
                    Some(keys) => format!(
                        "\n  pass {} in this call{none}",
                        keys.keys()
                            .iter()
                            .map(|k| format!("`{k}`"))
                            .collect::<Vec<_>>()
                            .join(" or ")
                    ),
                    None => format!(
                        "\n  `{tool}` cannot set `{field}`: file the ticket with \
                         `devkit ticket create --arg {field}=...` instead"
                    ),
                });
            }
            block
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use devkit_config::InputKeys;
    use serde_json::json;

    use super::*;

    fn linear() -> RenderedToolRule {
        toml::from_str(
            "servers = [\"*linear*\"]\ntools = [\"save_issue\"]\nabsent = [\"id\"]\n\
             title = \"title\"\nbody = \"description\"\nbody_patch = [\"patch\"]\n",
        )
        .unwrap()
    }

    fn github() -> RenderedToolRule {
        toml::from_str(
            "servers = [\"*github*\"]\ntools = [\"issue_write\"]\n\
             equals = { method = \"create\" }\ntitle = \"title\"\nbody = \"body\"\n",
        )
        .unwrap()
    }

    fn run(rule: &RenderedToolRule, input: Value, receipted: &[(Field, &str)]) -> Verdict {
        let set: HashSet<(Field, String)> =
            receipted.iter().map(|(f, t)| (*f, t.to_string())).collect();
        let ctx = Context {
            gate: ISSUE,
            hint: &String::new,
            session_seen: false,
        };
        decide(rule, "srv", "tool", &input, &ctx, &|f, t| {
            Ok(set.contains(&(f, t.to_string())))
        })
        .unwrap()
    }

    fn denied_mentioning(v: &Verdict, word: &str) -> bool {
        matches!(v, Verdict::Deny(r) if r.contains(word))
    }

    #[test]
    fn a_create_needs_both_fields() {
        let v = run(&linear(), json!({"title": "T", "description": "B"}), &[(
            Field::Title,
            "T",
        )]);
        assert!(denied_mentioning(&v, "description"), "{v:?}");
    }

    #[test]
    fn a_create_without_description_checks_the_empty_body() {
        let v = run(&linear(), json!({"title": "T"}), &[
            (Field::Title, "T"),
            (Field::Body, ""),
        ]);
        assert_eq!(v, Verdict::Allow);
    }

    #[test]
    fn an_update_touching_neither_field_is_allowed() {
        let v = run(&linear(), json!({"id": "ENG-1", "state": "Done"}), &[]);
        assert_eq!(v, Verdict::Allow);
    }

    #[test]
    fn an_update_rewriting_the_body_needs_its_receipt() {
        let input = json!({"id": "ENG-1", "description": "x"});
        assert!(matches!(
            run(&linear(), input.clone(), &[]),
            Verdict::Deny(_)
        ));
        assert_eq!(run(&linear(), input, &[(Field::Body, "x")]), Verdict::Allow);
    }

    #[test]
    fn a_patch_is_denied() {
        let v = run(&linear(), json!({"id": "ENG-1", "patch": []}), &[]);
        assert!(denied_mentioning(&v, "patch"), "{v:?}");
    }

    #[test]
    fn a_non_string_field_is_denied() {
        let v = run(&linear(), json!({"title": 5}), &[(Field::Body, "")]);
        assert!(denied_mentioning(&v, "`title` is not a string"), "{v:?}");
    }

    #[test]
    fn a_non_object_input_is_denied_as_such() {
        for rule in [linear(), github()] {
            let v = run(&rule, json!("T"), &[]);
            assert!(denied_mentioning(&v, "not a JSON object"), "{v:?}");
        }
    }

    #[test]
    fn github_update_without_fields_is_allowed() {
        let v = run(
            &github(),
            json!({"method": "update", "issue_number": 1, "state": "closed"}),
            &[],
        );
        assert_eq!(v, Verdict::Allow);
    }

    #[test]
    fn a_rendered_session_reports_the_field_as_differing() {
        let ctx = Context {
            gate: ISSUE,
            hint: &String::new,
            session_seen: true,
        };
        let v = decide(
            &linear(),
            "srv",
            "tool",
            &json!({"title": "T", "description": "B"}),
            &ctx,
            &|_, _| Ok(false),
        )
        .unwrap();
        assert!(denied_mentioning(&v, "differs"), "{v:?}");
    }

    const GITHUB_FIELDS: &str = r#"
[github]
issues_repo = "acme/app"
project = 7

[ticket.fields.github.priority]
type        = "enum"
where       = { project_field = "PVTSSF_p", name = "Priority" }
values      = [{ name = "P0", id = "f75ad846" }, { name = "P1", id = "47fc9ee4" }]
default     = "P1"
level       = "error"
description = "P0 drop everything, P1 this cycle"

[ticket.fields.github.labels]
type   = "string"
list   = true
where  = "labels"
level  = "error"

[ticket.fields.github.milestone]
type   = "enum"
where  = "milestone"
values = [{ name = "v1", id = "v1" }]
level  = "warning"
description = "the release it ships in"
"#;

    const LINEAR_FIELDS: &str = r#"
[ticket.fields.linear.team]
type   = "enum"
where  = "team"
values = [
  { name = "ENG", id = "t-eng", aliases = ["ENG", "Engineering"] },
  { name = "OPS", id = "t-ops", aliases = ["OPS", "Operations"] },
]
level  = "error"

[ticket.fields.linear.labels]
type   = "enum"
list   = true
where  = "labels"
values = [{ name = "infra", id = "L1", teams = ["ENG"] }, { name = "bug", id = "L2" }]
level  = "error"

[ticket.fields.linear.estimate]
type  = "number"
where = "estimate"
level = "info"

[ticket.fields.linear.priority]
type   = "enum"
where  = "priority"
values = [{ name = "Urgent", id = "1" }, { name = "Low", id = "4" }]
level  = "info"
"#;

    fn with_fields(
        mut rule: RenderedToolRule,
        tracker: TrackerKind,
        map: &[(&str, &str)],
    ) -> RenderedToolRule {
        rule.tracker = Some(tracker);
        rule.fields = map
            .iter()
            .map(|(f, k)| (f.to_string(), InputKeys::One(k.to_string())))
            .collect();
        rule
    }

    fn github_rule() -> RenderedToolRule {
        with_fields(github(), TrackerKind::Github, &[
            ("labels", "labels"),
            ("milestone", "milestone"),
        ])
    }

    fn linear_rule() -> RenderedToolRule {
        with_fields(linear(), TrackerKind::Linear, &[
            ("team", "team"),
            ("labels", "labels"),
            ("estimate", "estimate"),
            ("priority", "priority"),
        ])
    }

    fn fields(rule: &RenderedToolRule, toml: &str, input: Value) -> Verdict {
        fields_in(tempfile::tempdir().unwrap().path(), rule, toml, input)
    }

    fn fields_in(dir: &Path, rule: &RenderedToolRule, toml: &str, input: Value) -> Verdict {
        let cfg = devkit_config::Config::parse(toml).unwrap();
        field_verdict(
            rule,
            "issue_write",
            &input,
            &cfg,
            dir,
            &HashMap::new(),
            Caller::Agent,
        )
    }

    fn gh_create(extra: Value) -> Value {
        let mut v =
            json!({"method": "create", "owner": "acme", "repo": "app", "title": "T", "body": "B"});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    #[test]
    fn create_missing_an_error_field_denies_naming_it_and_the_ticket_create_flag() {
        let v = fields(
            &github_rule(),
            GITHUB_FIELDS,
            gh_create(json!({"milestone": "v1"})),
        );
        let Verdict::Deny(reason) = v else {
            panic!("{v:?}")
        };
        assert!(reason.contains("`labels`"), "{reason}");
        assert!(reason.contains("pass `labels` in this call"), "{reason}");
        assert!(reason.contains("--arg labels="), "{reason}");
        assert!(reason.contains("devkit ticket create"), "{reason}");
    }

    #[test]
    fn a_field_with_no_mapped_key_is_always_missing_on_create() {
        let input = gh_create(json!({"labels": ["bug"], "milestone": "v1", "priority": "P0"}));
        let v = fields(&github_rule(), GITHUB_FIELDS, input);
        let Verdict::Deny(reason) = v else {
            panic!("{v:?}")
        };
        assert!(reason.contains("`priority`"), "{reason}");
        assert!(
            reason.contains("`issue_write` cannot set `priority`")
                && reason.contains("devkit ticket create")
                && reason.contains("--arg priority="),
            "{reason}"
        );
        assert!(
            reason.contains("default: P1"),
            "a default is named, not applied: {reason}"
        );
        assert!(!reason.contains("`labels`"), "{reason}");
    }

    #[test]
    fn warning_allows_with_context() {
        let decls = GITHUB_FIELDS.replace("level       = \"error\"", "level       = \"info\"");
        let v = fields(
            &github_rule(),
            &decls,
            gh_create(json!({"labels": ["bug"]})),
        );
        let Verdict::AllowWith(report) = v else {
            panic!("{v:?}")
        };
        assert!(report.contains("`milestone`"), "{report}");
        assert!(report.contains("the release it ships in"), "{report}");
        assert_eq!(
            fields(
                &github_rule(),
                &decls,
                gh_create(json!({"labels": ["bug"], "milestone": "V1"}))
            ),
            Verdict::Allow
        );
    }

    #[test]
    fn a_call_to_another_repo_gets_no_field_check() {
        let mut input = gh_create(json!({}));
        input["repo"] = json!("other");
        assert_eq!(fields(&github_rule(), GITHUB_FIELDS, input), Verdict::Allow);
        let elsewhere = GITHUB_FIELDS.replace("[github]", "[tracker]\nkind = \"linear\"\n[github]");
        assert_eq!(
            fields(&github_rule(), &elsewhere, gh_create(json!({}))),
            Verdict::Allow,
            "[tracker] kind names another tracker"
        );
        let mut same = gh_create(json!({}));
        same["owner"] = json!("ACME");
        assert!(matches!(
            fields(&github_rule(), GITHUB_FIELDS, same),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn a_configured_issues_repo_never_reads_the_origin() {
        let cfg = devkit_config::Config::parse(GITHUB_FIELDS).unwrap();
        let never = || -> Option<String> { panic!("the origin was read") };
        let call = |repo: &str| json!({"owner": "acme", "repo": repo});
        assert!(names_issues_repo(
            TrackerKind::Github,
            &cfg,
            &call("app"),
            never
        ));
        assert!(!names_issues_repo(
            TrackerKind::Github,
            &cfg,
            &call("other"),
            never
        ));
    }

    #[test]
    fn without_issues_repo_the_origin_names_the_repo_checked() {
        let toml = GITHUB_FIELDS.replace("issues_repo = \"acme/app\"\n", "");
        let dir = tempfile::tempdir().unwrap();
        let mut other = gh_create(json!({}));
        other["repo"] = json!("other");
        assert!(
            matches!(
                fields_in(dir.path(), &github_rule(), &toml, other.clone()),
                Verdict::Deny(_)
            ),
            "with no issues repository to compare against, every call is checked"
        );
        let git = |args: &[&str]| {
            devkit_git::Git::fixture(dir.path())
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@github.com:acme/app.git"]);
        assert_eq!(
            fields_in(dir.path(), &github_rule(), &toml, other),
            Verdict::Allow
        );
        assert!(matches!(
            fields_in(dir.path(), &github_rule(), &toml, gh_create(json!({}))),
            Verdict::Deny(_)
        ));
    }

    #[test]
    fn a_rule_without_tracker_gets_no_field_check() {
        assert_eq!(
            fields(&github(), GITHUB_FIELDS, gh_create(json!({}))),
            Verdict::Allow
        );
    }

    #[test]
    fn null_is_explicit_none() {
        let input = json!({"title": "T", "team": "ENG", "labels": null});
        assert_eq!(fields(&linear_rule(), LINEAR_FIELDS, input), Verdict::Allow);
        let no_none = LINEAR_FIELDS.replace(
            "where  = \"labels\"",
            "where  = \"labels\"\nallow_none = false",
        );
        let input = json!({"title": "T", "team": "ENG", "labels": null});
        let v = fields(&linear_rule(), &no_none, input);
        assert!(denied_mentioning(&v, "cannot be left empty"), "{v:?}");
        let v = fields(
            &linear_rule(),
            LINEAR_FIELDS,
            json!({"title": "T", "team": null, "labels": ["L2"]}),
        );
        assert!(
            denied_mentioning(&v, "`team`"),
            "a team never allows none: {v:?}"
        );
    }

    #[test]
    fn an_empty_array_is_explicit_none_for_a_list_field_only() {
        let input = json!({"title": "T", "team": "ENG", "labels": []});
        assert_eq!(fields(&linear_rule(), LINEAR_FIELDS, input), Verdict::Allow);
        let v = fields(
            &linear_rule(),
            LINEAR_FIELDS,
            json!({"title": "T", "team": "ENG"}),
        );
        assert!(denied_mentioning(&v, "`null` or `[]`"), "{v:?}");
        let update = json!({"id": "ENG-1", "labels": []});
        let no_none = LINEAR_FIELDS.replace(
            "where  = \"labels\"",
            "where  = \"labels\"\nallow_none = false",
        );
        let v = fields(&linear_rule(), &no_none, update);
        assert!(denied_mentioning(&v, "cannot be left empty"), "{v:?}");
        let input = json!({"title": "T", "team": "ENG", "labels": ["L2"], "priority": []});
        let v = fields(&linear_rule(), LINEAR_FIELDS, input);
        assert!(
            denied_mentioning(&v, "`priority`") && denied_mentioning(&v, "JSON"),
            "{v:?}"
        );
    }

    #[test]
    fn a_linear_label_valid_in_any_team_is_accepted() {
        let input = json!({"title": "T", "team": "OPS", "labels": ["L2"]});
        assert_eq!(fields(&linear_rule(), LINEAR_FIELDS, input), Verdict::Allow);
        let input = json!({"title": "T", "team": "OPS", "labels": ["nope"]});
        let v = fields(&linear_rule(), LINEAR_FIELDS, input);
        assert!(denied_mentioning(&v, "`nope`"), "{v:?}");
    }

    #[test]
    fn a_value_of_the_wrong_json_type_is_refused() {
        for (key, value) in [
            ("team", json!(["ENG"])),
            ("labels", json!([["L2"]])),
            ("labels", json!({"name": "L2"})),
            ("estimate", json!(true)),
        ] {
            let mut input = json!({"title": "T", "team": "ENG", "labels": ["L2"]});
            input[key] = value.clone();
            let v = fields(&linear_rule(), LINEAR_FIELDS, input);
            assert!(
                denied_mentioning(&v, &format!("`{key}`")) && denied_mentioning(&v, "JSON"),
                "{key} = {value}: {v:?}"
            );
        }
        let input = json!({"title": "T", "team": "ENG", "labels": "L2", "estimate": 3});
        assert_eq!(
            fields(&linear_rule(), LINEAR_FIELDS, input),
            Verdict::Allow,
            "a string for a list is one value, a number for a number field"
        );
    }

    #[test]
    fn a_number_names_an_enum_value_by_its_numeric_id() {
        let input = json!({"title": "T", "team": "ENG", "labels": ["L2"], "priority": 1});
        assert_eq!(fields(&linear_rule(), LINEAR_FIELDS, input), Verdict::Allow);
        let input = json!({"title": "T", "team": "ENG", "labels": ["L2"], "priority": 9});
        let v = fields(&linear_rule(), LINEAR_FIELDS, input);
        assert!(denied_mentioning(&v, "`9`"), "{v:?}");
    }

    #[test]
    fn an_update_checks_only_what_it_carries() {
        let update = json!({"id": "ENG-1", "estimate": "lots"});
        let v = fields(&linear_rule(), LINEAR_FIELDS, update);
        assert!(denied_mentioning(&v, "not a number"), "{v:?}");
        let update = json!({"id": "ENG-1", "estimate": 2});
        assert_eq!(
            fields(&linear_rule(), LINEAR_FIELDS, update),
            Verdict::Allow
        );
    }

    /// [`LINEAR_FIELDS`] with a `Bug` label in each team besides `infra`.
    fn bug_in_each_team() -> String {
        LINEAR_FIELDS.replace(
            r#"{ name = "bug", id = "L2" }"#,
            r#"{ name = "Bug", id = "L2", teams = ["ENG"] }, { name = "Bug", id = "L3", teams = ["OPS"] }"#,
        )
    }

    #[test]
    fn a_create_scopes_team_bound_values_to_its_team() {
        let input = json!({"title": "T", "team": "OPS", "labels": ["L1"]});
        let v = fields(&linear_rule(), LINEAR_FIELDS, input);
        assert!(
            denied_mentioning(&v, "`L1`") && denied_mentioning(&v, "OPS"),
            "{v:?}"
        );
        let input = json!({"title": "T", "team": "OPS", "labels": ["L3"]});
        assert_eq!(
            fields(&linear_rule(), &bug_in_each_team(), input),
            Verdict::Allow
        );
    }

    #[test]
    fn a_name_several_teams_share_is_refused_naming_each_id() {
        let update = json!({"id": "ENG-1", "labels": ["Bug"]});
        let v = fields(&linear_rule(), &bug_in_each_team(), update);
        assert!(denied_mentioning(&v, "pass `L2` or `L3`"), "{v:?}");
    }

    #[test]
    fn a_denial_lists_each_id_with_its_name_and_teams() {
        let v = fields(
            &linear_rule(),
            &bug_in_each_team(),
            json!({"title": "T", "team": "ENG"}),
        );
        assert!(
            denied_mentioning(
                &v,
                "L1 (infra; teams ENG), L2 (Bug; teams ENG), L3 (Bug; teams OPS)"
            ),
            "{v:?}"
        );
    }

    #[test]
    fn a_user_is_passed_as_its_tracker_identity() {
        let toml = "[people.lev]\ngithub = \"LevValle\"\nlinear = \"u-lev\"\n\
                    [people.ada]\nlinear = \"u-ada\"\n\
                    [ticket.fields.github.assignees]\ntype = \"user\"\nlist = true\n\
                    where = \"assignees\"\nlevel = \"error\"\n\
                    [ticket.fields.linear.assignee]\ntype = \"user\"\n\
                    where = \"assignee\"\nlevel = \"error\"\n";
        let rule = with_fields(github(), TrackerKind::Github, &[("assignees", "assignees")]);
        let gh = |who: &str| fields(&rule, toml, gh_create(json!({"assignees": [who]})));
        assert_eq!(gh("levvalle"), Verdict::Allow);
        let v = gh("lev");
        assert!(
            denied_mentioning(&v, "`[people]` alias") && denied_mentioning(&v, "pass `LevValle`"),
            "an alias is not a login: {v:?}"
        );
        let v = gh("ghost");
        assert!(
            denied_mentioning(&v, "ghost") && denied_mentioning(&v, "identity"),
            "{v:?}"
        );
        assert!(
            denied_mentioning(&gh("me"), "`me`"),
            "`me` passes on Linear only"
        );

        let rule = with_fields(linear(), TrackerKind::Linear, &[("assignee", "assignee")]);
        let lin = |who: &str| fields(&rule, toml, json!({"title": "T", "assignee": who}));
        for who in ["u-ada", "U-LEV", "me", "ada@example.com"] {
            assert_eq!(lin(who), Verdict::Allow, "{who}");
        }
        let v = lin("ada");
        assert!(denied_mentioning(&v, "pass `u-ada`"), "{v:?}");
    }

    #[test]
    fn a_linear_user_passes_by_the_display_name_linear_resolves() {
        let toml = "[people.ada]\nlinear = \"0b7c4a52-9d1e-4f3a-8c6b-2e5d7f9a1c30\"\n\
                    [ticket.fields.linear.assignee]\ntype = \"user\"\n\
                    where = \"assignee\"\nlevel = \"error\"\n";
        let rule = with_fields(linear(), TrackerKind::Linear, &[("assignee", "assignee")]);
        let lin = |who: &str| fields(&rule, toml, json!({"title": "T", "assignee": who}));
        for who in ["Grace Hopper", "grace"] {
            assert_eq!(lin(who), Verdict::Allow, "{who}");
        }
        let v = lin("Ada");
        assert!(
            denied_mentioning(&v, "pass `0b7c4a52-9d1e-4f3a-8c6b-2e5d7f9a1c30`"),
            "a `[people]` alias still names the identity to pass: {v:?}"
        );
        let v = lin("6f1e2d3c-4b5a-4978-8a6b-5c4d3e2f1a00");
        assert!(
            denied_mentioning(&v, "6f1e2d3c-4b5a-4978-8a6b-5c4d3e2f1a00"),
            "a user id no `[people]` entry records: {v:?}"
        );
    }

    #[test]
    fn a_linear_team_passes_as_its_name_key_or_id() {
        let call = |team: &str| {
            let input = json!({"title": "T", "team": team, "labels": ["L2"]});
            fields(&linear_rule(), LINEAR_FIELDS, input)
        };
        for team in ["Engineering", "ENG", "t-eng"] {
            assert_eq!(call(team), Verdict::Allow, "{team}");
        }
        assert!(matches!(call("  "), Verdict::Deny(_)), "a blank team");
        let v = call("Platform");
        assert!(
            denied_mentioning(&v, "`Platform`")
                && denied_mentioning(&v, "t-eng (ENG; or Engineering)"),
            "{v:?}"
        );
    }

    #[test]
    fn every_input_key_a_field_maps_to_is_checked() {
        let rule: RenderedToolRule = toml::from_str(
            "servers = [\"*linear*\"]\ntools = [\"save_issue\"]\nabsent = [\"id\"]\n\
             title = \"title\"\nbody = \"description\"\ntracker = \"linear\"\n\
             fields = { team = \"team\", labels = [\"labels\", \"addLabels\"] }\n",
        )
        .unwrap();
        let check = |input: Value| fields(&rule, LINEAR_FIELDS, input);
        assert_eq!(
            check(json!({"title": "T", "team": "ENG", "addLabels": ["L2"]})),
            Verdict::Allow,
            "either key sets the field on a create"
        );
        let v = check(json!({"id": "ENG-1", "addLabels": ["nope"]}));
        assert!(denied_mentioning(&v, "`nope`"), "{v:?}");
        let v = check(json!({"id": "ENG-1", "labels": ["L2"], "addLabels": ["nope"]}));
        assert!(denied_mentioning(&v, "`nope`"), "{v:?}");
        assert_eq!(
            check(json!({"id": "ENG-1", "removeLabels": ["nope"]})),
            Verdict::Allow,
            "removing a value cannot break a level"
        );
        let v = check(json!({"title": "T", "team": "ENG"}));
        assert!(
            denied_mentioning(&v, "pass `labels` or `addLabels` in this call"),
            "{v:?}"
        );
    }

    #[test]
    fn an_enum_value_is_passed_as_its_tracker_id() {
        let toml = "[ticket.fields.github.labels]\ntype = \"enum\"\nlist = true\n\
                    where = \"labels\"\nlevel = \"error\"\n\
                    values = [{ name = \"urgent\", id = \"Bug\" }, { name = \"docs\", id = \"docs\" }]\n";
        let rule = with_fields(github(), TrackerKind::Github, &[("labels", "labels")]);
        let gh = |label: &str| fields(&rule, toml, gh_create(json!({"labels": [label]})));
        for label in ["Bug", "bug", "docs"] {
            assert_eq!(gh(label), Verdict::Allow, "{label}");
        }
        let v = gh("urgent");
        assert!(
            denied_mentioning(&v, "`urgent`") && denied_mentioning(&v, "pass `Bug`"),
            "a local name reaches the tracker as a label of its own: {v:?}"
        );
        let v = gh("nope");
        assert!(
            denied_mentioning(&v, "values: Bug (urgent), docs"),
            "the denial lists what the call takes: {v:?}"
        );
    }

    #[test]
    fn a_list_item_holding_a_comma_is_one_value() {
        let toml = LINEAR_FIELDS.replace(
            r#"{ name = "bug", id = "L2" }"#,
            r#"{ name = "needs, triage", id = "needs, triage" }"#,
        );
        let input = json!({"title": "T", "team": "ENG", "labels": ["needs, triage"]});
        assert_eq!(fields(&linear_rule(), &toml, input), Verdict::Allow);
        let input = json!({"title": "T", "team": "ENG", "labels": ["needs"]});
        let v = fields(&linear_rule(), &toml, input);
        assert!(denied_mentioning(&v, "`needs`"), "{v:?}");
    }
}
