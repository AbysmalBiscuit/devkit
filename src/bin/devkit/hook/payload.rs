//! A hook payload as devkit reads it: pabal's model of each harness, with
//! the harness a manifest declared.

use std::path::Path;

use pabal::{AddContext, AnyHarness, AnyPayload, AnyView, Fields, Response, Tool};
use serde_json::Value;

use super::HookEvent;

/// The harness that answers a payload. A declared harness beats inferring
/// it from which fields a vendor happens to send, except that Cursor runs
/// hooks configured for Claude Code and sends them its own payload.
fn resolve(declared: Option<AnyHarness>, raw: &Value) -> AnyHarness {
    let inferred = AnyHarness::infer(raw);
    match declared {
        Some(AnyHarness::ClaudeCode) if inferred == AnyHarness::Cursor => inferred,
        Some(declared) => declared,
        None => inferred,
    }
}

/// The `PreToolUse` deny `harness` reads.
pub fn deny(harness: AnyHarness, reason: &str) -> String {
    Response::deny_pre_tool_use(harness, reason).to_string()
}

/// The id a payload's locks and fired rules are held under: its session, or
/// `session/agent` for a subagent. Only a [`Payload`] makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder(String);

impl std::ops::Deref for Holder {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

/// A payload with no session has no holder, so nothing can be claimed for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingSession;

impl std::fmt::Display for MissingSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("write payload carries no session_id")
    }
}

/// A parsed hook payload and the harness that answers it.
#[derive(Debug, Clone)]
pub struct Payload(AnyPayload);

impl Payload {
    /// `None` when `raw` is not a JSON object. `event` names the event for a
    /// harness that leaves it out of the payload; a `hook_event_name` the
    /// payload sends wins.
    pub fn new(declared: Option<AnyHarness>, event: HookEvent, raw: Value) -> Option<Self> {
        let harness = resolve(declared, &raw);
        let inner = match harness {
            // pabal takes a named event only alongside stdin text.
            AnyHarness::Antigravity => {
                AnyPayload::parse_named(harness, event.into(), &raw.to_string())
            }
            AnyHarness::ClaudeCode | AnyHarness::Codex | AnyHarness::Cursor => {
                AnyPayload::from_value(harness, raw)
            }
        };
        inner.ok().map(Self)
    }

    /// A payload with no fields, for a verb whose stdin carried none.
    pub fn empty(declared: Option<AnyHarness>, event: HookEvent) -> Self {
        Self::new(declared, event, Value::Object(serde_json::Map::new()))
            .expect("an empty object is a payload")
    }

    pub fn harness(&self) -> AnyHarness {
        self.0.harness()
    }

    pub fn raw(&self) -> &Value {
        self.0.raw()
    }

    /// The `hook_event_name` as sent, or as the verb named it.
    pub fn event_name(&self) -> Option<String> {
        Some(self.0.event_name()).filter(|name| !name.is_empty())
    }

    pub fn session_id(&self) -> Option<&str> {
        self.0.session_id()
    }

    /// The subagent the payload speaks for, or `None` for its session.
    pub fn agent(&self) -> Option<&str> {
        self.0.agent()
    }

    /// Who this payload's writes are held by. The payload exposes no ancestry
    /// deeper than session and subagent, so a holder has at most two levels.
    pub fn holder(&self) -> Result<Holder, MissingSession> {
        let session = self.session_id().ok_or(MissingSession)?;
        Ok(Holder(match self.agent().filter(|a| !a.is_empty()) {
            Some(agent) => format!("{session}/{agent}"),
            None => session.to_string(),
        }))
    }

    /// The session's own holder, which covers every subagent's by prefix.
    pub fn session_holder(&self) -> Option<Holder> {
        self.session_id().map(|session| Holder(session.to_string()))
    }

    /// The holder of the subagent this payload speaks for. `None` for the
    /// session and for a fork, whose holder is the session's.
    pub fn subagent_holder(&self) -> Option<Holder> {
        self.agent()?;
        self.holder().ok()
    }

    /// The raw `agent_id`, which a Claude Code fork carries as well as a
    /// subagent.
    pub fn agent_id(&self) -> Option<&str> {
        self.0.agent_id()
    }

    pub fn tool_use_id(&self) -> Option<&str> {
        self.0.tool_use_id()
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.0.cwd()
    }

    /// The tool's name as the harness spells it, `toolCall.name` being
    /// Antigravity's.
    pub fn tool_name(&self) -> Option<&str> {
        let raw = self.raw();
        text(raw, "tool_name").or_else(|| text(raw.get("toolCall")?, "name"))
    }

    pub fn tool(&self) -> Option<Tool<'_>> {
        self.0.tool()
    }

    /// The answer that adds `text` to the agent's context on an event whose
    /// hook output the harness reads as JSON. `None` on any other event, and
    /// where the harness has no such channel.
    pub fn context_answer(&self, text: &str) -> Option<String> {
        let response = match self.0.view() {
            AnyView::SessionStart(v) => Some(v.add_context(text)),
            AnyView::SubagentStart(v) => v.add_context(text),
            AnyView::UserPromptSubmit(v) => v.add_context(text),
            _ => None,
        };
        response.map(|r| r.to_string())
    }

    /// A `PreToolUse` answer that adds `text` to the agent's context and
    /// leaves the call's outcome alone. `None` where the harness has no such
    /// channel, which leaves a warning a silent allow.
    pub fn pre_tool_use_context(&self, text: &str) -> Option<String> {
        match self.0.view() {
            AnyView::PreToolUse(pre) => pre.add_context(text).map(|r| r.to_string()),
            _ => None,
        }
    }
}

fn text<'a>(raw: &'a Value, key: &str) -> Option<&'a str> {
    raw.get(key)?.as_str().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn payload(declared: Option<AnyHarness>, raw: Value) -> Payload {
        Payload::new(declared, HookEvent::PreToolUse, raw).expect("an object is a payload")
    }

    #[test]
    fn a_declared_harness_beats_the_payloads_shape() {
        let p = payload(Some(AnyHarness::Codex), json!({"cursor_version": "1.7.0"}));
        assert_eq!(p.harness(), AnyHarness::Codex);
    }

    #[test]
    fn a_cursor_payload_to_a_claude_code_hook_answers_as_cursor() {
        let camel = json!({"hook_event_name": "preToolUse", "cursor_version": "2026.09.26"});
        assert_eq!(
            payload(Some(AnyHarness::ClaudeCode), camel).harness(),
            AnyHarness::Cursor
        );
        let pascal = json!({"hook_event_name": "SessionStart", "cursor_version": "2026.08.11"});
        assert_eq!(
            payload(Some(AnyHarness::ClaudeCode), pascal).harness(),
            AnyHarness::ClaudeCode
        );
    }

    #[test]
    fn only_a_json_object_is_a_payload() {
        assert!(Payload::new(None, HookEvent::PreToolUse, json!([])).is_none());
        let empty = Payload::empty(None, HookEvent::Stop);
        assert_eq!(empty.harness(), AnyHarness::ClaudeCode);
        assert_eq!(empty.event_name(), None);
    }

    #[test]
    fn the_verb_names_an_antigravity_event() {
        let p = Payload::new(
            Some(AnyHarness::Antigravity),
            HookEvent::PreToolUse,
            json!({
                "conversationId": "c1",
                "toolCall": {"name": "run_command", "args": {"CommandLine": "ls", "Cwd": "/w"}}
            }),
        )
        .unwrap();
        assert_eq!(p.event_name().as_deref(), Some("PreToolUse"));
        assert_eq!(p.session_id(), Some("c1"));
        assert_eq!(p.tool_name(), Some("run_command"));
        assert!(matches!(
            p.tool(),
            Some(Tool::Shell { command: "ls", cwd: Some(cwd), .. }) if cwd == Path::new("/w")
        ));
    }

    #[test]
    fn the_holder_is_the_session_or_session_slash_subagent() {
        let session = payload(None, json!({"session_id": "S"}));
        assert_eq!(session.holder().as_deref(), Ok("S"));
        assert_eq!(session.subagent_holder(), None);

        let sub = payload(
            None,
            json!({"session_id": "S", "agent_id": "a1", "agent_type": "general-purpose"}),
        );
        assert_eq!(sub.holder().as_deref(), Ok("S/a1"));
        assert_eq!(sub.session_holder().as_deref(), Some("S"));
        assert_eq!(sub.subagent_holder().as_deref(), Some("S/a1"));
    }

    /// A fork can end without a `SubagentStop` to release it, so it holds as
    /// its session and has no subagent holder to release.
    #[test]
    fn a_fork_holds_as_its_session() {
        let fork = payload(None, json!({"session_id": "S", "agent_id": "afork"}));
        assert_eq!(fork.holder().as_deref(), Ok("S"));
        assert_eq!(fork.subagent_holder(), None);
    }

    #[test]
    fn a_payload_without_a_session_has_no_holder() {
        let p = payload(
            None,
            json!({"agent_id": "a1", "agent_type": "general-purpose"}),
        );
        assert_eq!(p.holder(), Err(MissingSession));
        assert_eq!(p.session_holder(), None);
        assert_eq!(p.subagent_holder(), None);
    }

    #[test]
    fn each_harness_gets_its_own_deny_envelope() {
        let deny = |h| -> Value {
            serde_json::from_str(&Response::deny_pre_tool_use(h, "use devrun").to_string()).unwrap()
        };
        let cc = deny(AnyHarness::ClaudeCode);
        assert_eq!(cc["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            cc["hookSpecificOutput"]["permissionDecisionReason"],
            "use devrun"
        );
        let cur = deny(AnyHarness::Cursor);
        assert_eq!(cur["permission"], "deny");
        assert_eq!(cur["agent_message"], "use devrun");
        let ag = deny(AnyHarness::Antigravity);
        assert_eq!(
            (&ag["decision"], &ag["reason"]),
            (&json!("deny"), &json!("use devrun"))
        );
    }

    #[test]
    fn context_allows_without_a_permission_decision() {
        let p = payload(
            Some(AnyHarness::Codex),
            json!({"hook_event_name": "PreToolUse"}),
        );
        let v: Value =
            serde_json::from_str(&p.pre_tool_use_context("not checked").unwrap()).unwrap();
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "not checked");
        assert!(v["hookSpecificOutput"].get("permissionDecision").is_none());
        let shell = payload(
            Some(AnyHarness::Cursor),
            json!({"hook_event_name": "beforeShellExecution"}),
        );
        assert!(shell.pre_tool_use_context("not checked").is_none());
        let antigravity = payload(Some(AnyHarness::Antigravity), json!({}));
        assert!(antigravity.pre_tool_use_context("not checked").is_none());
    }
}
