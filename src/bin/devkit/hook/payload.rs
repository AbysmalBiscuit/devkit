//! A hook payload as devkit reads it: pabal's model of Claude Code and Codex,
//! plus the Cursor spellings pabal does not model yet.

use std::path::Path;

use clap::ValueEnum;
use pabal::{AddContext, AnyHarness, AnyPayload, AnyView, Fields, Response, Tool};
use serde_json::{Value, json};

/// The harness a hook answers, and so the envelope it writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Harness {
    ClaudeCode,
    Codex,
    Cursor,
}

impl Harness {
    /// The harness a payload's own fields point to: `cursor_version` is
    /// Cursor's, and pabal tells the other two apart.
    fn infer(raw: &Value) -> Self {
        if has(raw, "cursor_version") {
            return Harness::Cursor;
        }
        match AnyHarness::infer(raw) {
            AnyHarness::ClaudeCode => Harness::ClaudeCode,
            AnyHarness::Codex => Harness::Codex,
        }
    }

    /// The name `--harness` takes, which is also what a record carries.
    pub fn name(self) -> &'static str {
        match self {
            Harness::ClaudeCode => "claude-code",
            Harness::Codex => "codex",
            Harness::Cursor => "cursor",
        }
    }

    /// How pabal parses the payload. A Cursor payload reads as Claude Code,
    /// the shape Cursor sends to hooks configured for Claude Code.
    fn wire(self) -> AnyHarness {
        match self {
            Harness::ClaudeCode | Harness::Cursor => AnyHarness::ClaudeCode,
            Harness::Codex => AnyHarness::Codex,
        }
    }

    /// The `PreToolUse` deny this harness reads. Cursor's reason goes in
    /// `agent_message`: `user_message` is shown to the human, and only the
    /// agent message reaches the agent, which is the whole point of handing
    /// back a command it can retry.
    pub fn deny(self, reason: &str) -> String {
        match self {
            Harness::ClaudeCode | Harness::Codex => {
                Response::deny_pre_tool_use(self.wire(), reason).to_string()
            }
            Harness::Cursor => json!({
                "permission": "deny",
                "agent_message": reason,
                "continue": true
            })
            .to_string(),
        }
    }
}

fn has(raw: &Value, key: &str) -> bool {
    raw.get(key).is_some_and(|v| !v.is_null())
}

fn text<'a>(raw: &'a Value, key: &str) -> Option<&'a str> {
    raw.get(key)?.as_str().filter(|s| !s.is_empty())
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
pub struct Payload {
    harness: Harness,
    inner: AnyPayload,
}

impl Payload {
    /// `None` when `raw` is not a JSON object. The harness a manifest declared
    /// beats inferring it from which fields a vendor happens to send.
    pub fn new(declared: Option<Harness>, raw: Value) -> Option<Self> {
        let harness = declared.unwrap_or_else(|| Harness::infer(&raw));
        let inner = AnyPayload::from_value(harness.wire(), raw).ok()?;
        Some(Self { harness, inner })
    }

    /// A payload with no fields, for a verb whose stdin carried none.
    pub fn empty(declared: Option<Harness>) -> Self {
        Self::new(declared, Value::Object(serde_json::Map::new()))
            .expect("an empty object is a payload")
    }

    pub fn harness(&self) -> Harness {
        self.harness
    }

    pub fn raw(&self) -> &Value {
        self.inner.raw()
    }

    /// Cursor wrote this, natively or through a hook configured for Claude
    /// Code, so its own spellings of the identity fields apply.
    fn cursor(&self) -> bool {
        self.harness == Harness::Cursor || has(self.raw(), "cursor_version")
    }

    fn cursor_text(&self, key: &str) -> Option<&str> {
        self.cursor().then(|| text(self.raw(), key)).flatten()
    }

    /// The `hook_event_name` as sent.
    pub fn event_name(&self) -> Option<String> {
        Some(self.inner.event_name()).filter(|name| !name.is_empty())
    }

    pub fn session_id(&self) -> Option<&str> {
        self.inner
            .session_id()
            .or_else(|| self.cursor_text("conversation_id"))
    }

    /// The subagent the payload speaks for, or `None` for its session.
    pub fn agent(&self) -> Option<&str> {
        self.inner
            .agent()
            .or_else(|| self.cursor_text("parent_conversation_id"))
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
        self.inner
            .agent_id()
            .or_else(|| self.cursor_text("parent_conversation_id"))
    }

    pub fn tool_use_id(&self) -> Option<&str> {
        self.inner
            .tool_use_id()
            .or_else(|| self.cursor_text("tool_call_id"))
    }

    pub fn cwd(&self) -> Option<&Path> {
        self.inner.cwd().or_else(|| {
            self.cursor()
                .then(|| text(self.raw().get("tool_input")?, "working_directory"))
                .flatten()
                .map(Path::new)
        })
    }

    pub fn tool_name(&self) -> Option<&str> {
        text(self.raw(), "tool_name")
    }

    /// The tool call. Cursor's shell hook carries its command at the top level
    /// and its generic tool hook calls the shell `Shell`, so on Cursor any
    /// call with a command is a shell command.
    pub fn tool(&self) -> Option<Tool<'_>> {
        let tool = self.inner.tool();
        if !self.cursor() {
            return tool;
        }
        match tool {
            Some(Tool::Shell { command, shell, .. }) => Some(Tool::Shell {
                command,
                cwd: self.cwd(),
                shell,
            }),
            Some(Tool::Edit(_)) => tool,
            _ => {
                let raw = self.raw();
                text(raw, "command")
                    .or_else(|| text(raw.get("tool_input")?, "command"))
                    .filter(|command| !command.trim().is_empty())
                    .map(|command| Tool::Shell {
                        command,
                        cwd: self.cwd(),
                        shell: None,
                    })
                    .or(tool)
            }
        }
    }

    /// A `PreToolUse` answer that adds `text` to the agent's context and
    /// leaves the call's outcome alone. `None` where the harness has no such
    /// channel, which leaves a warning a silent allow.
    pub fn pre_tool_use_context(&self, text: &str) -> Option<String> {
        if self.harness == Harness::Cursor {
            return None;
        }
        match self.inner.view() {
            AnyView::PreToolUse(pre) => Some(pre.add_context(text).to_string()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(declared: Option<Harness>, raw: Value) -> Payload {
        Payload::new(declared, raw).expect("an object is a payload")
    }

    #[test]
    fn cursor_version_names_cursor_and_the_rest_is_pabals() {
        let cursor = json!({"hook_event_name": "beforeShellExecution", "cursor_version": "1.7.0"});
        assert_eq!(Harness::infer(&cursor), Harness::Cursor);
        let codex = json!({"hook_event_name": "PreToolUse", "turn_id": "t1", "model": "gpt-5"});
        assert_eq!(Harness::infer(&codex), Harness::Codex);
        assert_eq!(
            Harness::infer(&json!({"hook_event_name": "PreToolUse"})),
            Harness::ClaudeCode
        );
    }

    #[test]
    fn a_declared_harness_beats_the_payloads_shape() {
        let p = payload(Some(Harness::Codex), json!({"cursor_version": "1.7.0"}));
        assert_eq!(p.harness(), Harness::Codex);
    }

    #[test]
    fn only_a_json_object_is_a_payload() {
        assert!(Payload::new(None, json!([])).is_none());
        assert_eq!(Payload::empty(None).harness(), Harness::ClaudeCode);
    }

    #[test]
    fn cursor_carries_its_command_at_the_top_level() {
        let p = payload(
            None,
            json!({
                "hook_event_name": "beforeShellExecution",
                "cursor_version": "1.7.0",
                "command": "vite dev",
                "cwd": "/repo"
            }),
        );
        assert_eq!(
            p.tool(),
            Some(Tool::Shell {
                command: "vite dev",
                cwd: Some(Path::new("/repo")),
                shell: None
            })
        );
    }

    #[test]
    fn cursor_spells_the_identity_fields_its_own_way() {
        let p = payload(
            None,
            json!({
                "hook_event_name": "preToolUse",
                "cursor_version": "1.7.0",
                "model": "claude-4.5-sonnet",
                "conversation_id": "c1",
                "parent_conversation_id": "p1",
                "tool_call_id": "u1",
                "tool_name": "Shell",
                "tool_input": {"command": "npm install", "working_directory": "/w"}
            }),
        );
        assert_eq!(p.harness(), Harness::Cursor);
        assert_eq!(p.session_id(), Some("c1"));
        assert_eq!(p.agent(), Some("p1"));
        assert_eq!(p.tool_use_id(), Some("u1"));
        assert_eq!(p.cwd(), Some(Path::new("/w")));
        assert!(matches!(
            p.tool(),
            Some(Tool::Shell {
                command: "npm install",
                ..
            })
        ));
    }

    /// `cwd` wins where a payload sends both, so Cursor's fallback cannot
    /// displace the field the other two send.
    #[test]
    fn an_explicit_cwd_beats_the_tool_inputs_working_directory() {
        let p = payload(
            Some(Harness::Cursor),
            json!({
                "hook_event_name": "PreToolUse", "tool_name": "Bash", "cwd": "/repo",
                "tool_input": {"command": "ls", "working_directory": "/elsewhere"}
            }),
        );
        assert_eq!(p.cwd(), Some(Path::new("/repo")));
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
    fn claude_code_ignores_cursors_spellings() {
        let p = payload(
            Some(Harness::ClaudeCode),
            json!({
                "hook_event_name": "PreToolUse",
                "conversation_id": "c1",
                "tool_name": "Shell",
                "tool_input": {"command": "ls"}
            }),
        );
        assert_eq!(p.session_id(), None);
        assert!(!matches!(p.tool(), Some(Tool::Shell { .. })));
    }

    #[test]
    fn each_harness_gets_its_own_deny_envelope() {
        let cc: Value = serde_json::from_str(&Harness::ClaudeCode.deny("use devrun")).unwrap();
        assert_eq!(cc["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            cc["hookSpecificOutput"]["permissionDecisionReason"],
            "use devrun"
        );
        let cur: Value = serde_json::from_str(&Harness::Cursor.deny("use devrun")).unwrap();
        assert_eq!(cur["permission"], "deny");
        assert_eq!(cur["agent_message"], "use devrun");
    }

    #[test]
    fn context_allows_without_a_permission_decision() {
        let p = payload(
            Some(Harness::Codex),
            json!({"hook_event_name": "PreToolUse"}),
        );
        let v: Value =
            serde_json::from_str(&p.pre_tool_use_context("not checked").unwrap()).unwrap();
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "not checked");
        assert!(v["hookSpecificOutput"].get("permissionDecision").is_none());
        let cursor = payload(
            Some(Harness::Cursor),
            json!({"hook_event_name": "PreToolUse"}),
        );
        assert!(cursor.pre_tool_use_context("not checked").is_none());
    }

    #[test]
    fn harness_names_are_the_manifest_spellings() {
        for h in Harness::value_variants() {
            let clap = h.to_possible_value().unwrap();
            assert_eq!(h.name(), clap.get_name());
        }
    }
}
