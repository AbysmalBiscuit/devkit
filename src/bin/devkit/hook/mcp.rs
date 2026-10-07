//! The MCP path of `pre-tool-use`: an issue write through a tracker's MCP tool
//! is allowed only when its title and body are text `devkit issue render`
//! produced in the same agent session, and a PR write through a forge's MCP
//! tool only when they are text `devkit issue pr render` produced.

use std::{collections::BTreeMap, io::Write, path::Path, sync::OnceLock};

use anyhow::Result;
use devkit_common::{caller::Caller, harness, required, vcs::Checkout};
use devkit_config::RenderedToolRule;
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
    "devkit issue guard: internal failure while checking an issue or PR write (fail-closed)";

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
    render: "devkit issue render",
    flags: "--title ... [--body ...]",
};

pub(super) const PR: Gate = Gate {
    kind: Kind::Pr,
    with_article: "a PR",
    noun: "PR",
    render: "devkit issue pr render",
    flags: "--pr-title ... [--pr-body ...]",
};

#[derive(Debug, PartialEq)]
pub(super) enum Verdict {
    Allow,
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
            "devkit issue guard: this `{tool}` call writes {}, and the payload carries no \
             session id to check its `{render}` receipts against.",
            gate.with_article
        ));
    };
    if !receipt::valid_session(session) {
        return Verdict::Deny(format!(
            "devkit issue guard: session id `{session}` is not usable as a directory name, so \
             no `{render}` receipt can match it."
        ));
    }
    let Some(root) = receipt::store_root(checkout) else {
        return Verdict::Deny(format!(
            "devkit issue guard: {} is not inside a git checkout, where `{render}` keeps its \
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
    decide(rule, server, tool, input, &ctx, &|field, text| {
        receipt::has(&root, gate.kind, session, field, text)
    })
    .unwrap_or_else(|e| Verdict::Deny(format!("devkit issue guard: {e:#}")))
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

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
}
