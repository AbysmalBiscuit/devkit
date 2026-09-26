//! Payload to [`Record`]: the envelope every verb fills in, and the projection
//! of an [`Analysis`] the shell path carries.
//!
//! The mapping lives here rather than in `devkit-common` because it is the one
//! place that already depends on both crates. `devkit-common` does not depend
//! on `devkit-command`: that would compile six tree-sitter C grammars into
//! every library crate in the workspace, and put the IO-free analyser
//! underneath the IO crate. So the projection types are plain serde structs
//! over there, and `Analysis` gains no `Serialize`.

use devkit_command::{
    ANALYZER_VERSION, Analysis, Location, Target, Uncertainty, UncertaintyKind, Value,
};
use devkit_common::{
    git::Checkout,
    harness::Harness,
    harness_log::{
        self, AnalysisProjection, Counts, FrameEnd, Kind, Permission, Prompt, Record,
        SCHEMA_VERSION, SessionFrame, ShellPost, UnresolvedWrite, Worktree, WorktreeChange,
        now_rfc3339,
    },
};
use devkit_config::{Fidelity, PromptFidelity};
use serde_json::Value as Json;

use super::HookEvent;

/// A command over this is truncated with a flag in the record. A heredoc
/// carrying a whole file is not corpus signal.
pub const MAX_COMMAND_BYTES: usize = 128 * 1024;

impl HookEvent {
    /// The verb as a manifest spells it, which is what a record carries.
    ///
    /// Spelled out rather than read back from clap: a record's `event` is a
    /// field readers group on, so it is a contract of its own and a variant
    /// added without a spelling here is a compile error. A test below pins the
    /// two together.
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "pre-tool-use",
            HookEvent::PostToolUse => "post-tool-use",
            HookEvent::PostToolUseFailure => "post-tool-use-failure",
            HookEvent::SessionStart => "session-start",
            HookEvent::SessionEnd => "session-end",
            HookEvent::SubagentStart => "subagent-start",
            HookEvent::SubagentStop => "subagent-stop",
            HookEvent::PermissionRequest => "permission-request",
            HookEvent::PermissionDenied => "permission-denied",
            HookEvent::Stop => "stop",
            HookEvent::StopFailure => "stop-failure",
            HookEvent::PreCompact => "pre-compact",
            HookEvent::PostCompact => "post-compact",
            HookEvent::CwdChanged => "cwd-changed",
            HookEvent::WorktreeCreate => "worktree-create",
            HookEvent::WorktreeRemove => "worktree-remove",
            HookEvent::UserPromptSubmit => "user-prompt-submit",
        }
    }
}

fn harness_name(h: Harness) -> &'static str {
    match h {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::Cursor => "cursor",
    }
}

fn text(p: &Json, key: &str) -> Option<String> {
    p.get(key)
        .and_then(Json::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Where the hook fired: the payload's `cwd`, else Cursor's
/// `tool_input.working_directory`, else this process's own directory.
pub fn payload_cwd(payload: &Json) -> std::path::PathBuf {
    text(payload, "cwd")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            payload
                .get("tool_input")?
                .get("working_directory")?
                .as_str()
                .map(std::path::PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The envelope, filled in from whatever the payload carries. Every field a
/// harness did not send stays `None`. `cwd` and `project_root` come from the
/// caller's `checkout`, which the rest of the invocation already resolved, so
/// the record costs no git of its own.
pub fn envelope(
    payload: &Json,
    event: HookEvent,
    harness: Option<Harness>,
    checkout: &Checkout,
    kind: Kind,
) -> Box<Record> {
    Box::new(Record {
        schema_version: SCHEMA_VERSION,
        recorded_at: now_rfc3339(),
        devkit_version: env!("CARGO_PKG_VERSION").to_string(),
        analyzer_version: ANALYZER_VERSION,
        harness: harness
            .or_else(|| Some(devkit_common::harness::infer_harness(payload)))
            .map(|h| harness_name(h).to_string()),
        event: event.as_str().to_string(),
        vendor_event: text(payload, "hook_event_name"),
        session_id: text(payload, "session_id").or_else(|| text(payload, "conversation_id")),
        agent_id: text(payload, "agent_id").or_else(|| text(payload, "parent_conversation_id")),
        tool_use_id: text(payload, "tool_use_id").or_else(|| text(payload, "tool_call_id")),
        cwd: Some(checkout.dir().to_path_buf()),
        project_root: checkout.root().map(std::path::Path::to_path_buf),
        kind,
    })
}

/// The record a verb beyond `pre-tool-use` writes: a thin mapping from the
/// payload, with no analysis and no action. `None` when the verb records
/// nothing of its own.
pub fn record_only(payload: &Json, event: HookEvent, settings: &harness_log::Settings) -> Kind {
    match event {
        HookEvent::PostToolUse | HookEvent::PostToolUseFailure => {
            Kind::ShellPost(shell_post(payload, event))
        }
        HookEvent::SessionStart => Kind::Session(SessionFrame {
            end: FrameEnd::Start,
            subagent: false,
        }),
        HookEvent::SessionEnd => Kind::Session(SessionFrame {
            end: FrameEnd::End,
            subagent: false,
        }),
        HookEvent::SubagentStart => Kind::Session(SessionFrame {
            end: FrameEnd::Start,
            subagent: true,
        }),
        HookEvent::SubagentStop => Kind::Session(SessionFrame {
            end: FrameEnd::End,
            subagent: true,
        }),
        HookEvent::PermissionRequest | HookEvent::PermissionDenied => {
            Kind::Permission(Permission {
                blocked: event == HookEvent::PermissionDenied,
                tool_name: text(payload, "tool_name"),
                detail: text(payload, "permission_suggestions")
                    .or_else(|| text(payload, "reason"))
                    .or_else(|| text(payload, "message")),
            })
        }
        HookEvent::WorktreeCreate | HookEvent::WorktreeRemove => Kind::Worktree(Worktree {
            change: if event == HookEvent::WorktreeCreate {
                WorktreeChange::Create
            } else {
                WorktreeChange::Remove
            },
            path: text(payload, "worktree_path").or_else(|| text(payload, "path")),
        }),
        HookEvent::UserPromptSubmit => Kind::Prompt(prompt(payload, settings)),
        // Turn and context boundaries carry nothing beyond the envelope, which
        // already names the verb, the vendor event, the session and the cwd.
        HookEvent::Stop
        | HookEvent::StopFailure
        | HookEvent::PreCompact
        | HookEvent::PostCompact
        | HookEvent::CwdChanged => Kind::Lifecycle,
        // Its own path, which analyses and reaches a verdict.
        HookEvent::PreToolUse => Kind::Lifecycle,
    }
}

/// Each field is absent, not zero, where the harness does not supply it. Codex
/// forces that wording: its post payload exposes `tool_response` as output text
/// rather than a structured result, so no exit code reaches the hook. Byte
/// length and duration still land; the output itself never does.
fn shell_post(payload: &Json, event: HookEvent) -> ShellPost {
    let response = payload
        .get("tool_response")
        .or_else(|| payload.get("tool_output"));
    let num = |v: Option<&Json>, key: &str| v.and_then(|r| r.get(key)).and_then(Json::as_i64);
    let bytes = |key: &str| {
        response
            .and_then(|r| r.get(key))
            .and_then(Json::as_str)
            .map(str::len)
            // Codex hands back the model-facing output body as a bare string
            // rather than a structured result, so its length is all there is.
            .or_else(|| response.and_then(Json::as_str).map(str::len))
    };
    ShellPost {
        exit_code: num(response, "exit_code").or_else(|| num(response, "exitCode")),
        duration_ms: response
            .and_then(|r| r.get("duration_ms"))
            .and_then(Json::as_u64),
        // A failure verb is an error whatever the payload says, and a success
        // verb takes the payload's word for it.
        error: Some(event == HookEvent::PostToolUseFailure)
            .filter(|failed| *failed)
            .or_else(|| {
                response
                    .and_then(|r| r.get("error"))
                    .and_then(Json::as_bool)
            }),
        interrupted: response
            .and_then(|r| r.get("interrupted"))
            .and_then(Json::as_bool),
        stdout_bytes: bytes("stdout"),
        stderr_bytes: response
            .and_then(|r| r.get("stderr"))
            .and_then(Json::as_str)
            .map(str::len),
    }
}

/// Prompt text is gated by its own fidelity key, defaulting to `off`, where the
/// record says a prompt was submitted and carries none of it. A corpus can hold
/// full command text without holding what the human typed.
fn prompt(payload: &Json, settings: &harness_log::Settings) -> Prompt {
    let raw = text(payload, "prompt")
        .or_else(|| text(payload, "user_prompt"))
        .or_else(|| text(payload, "text"))
        .unwrap_or_default();
    let chars = Some(raw.chars().count());
    let text = match settings.prompt {
        PromptFidelity::Off => None,
        PromptFidelity::Hashed => Some(harness_log::redact::digest(&raw)),
        PromptFidelity::Redacted => Some(harness_log::redact::apply(&raw, Fidelity::Redacted).0),
        PromptFidelity::Full => Some(raw),
    };
    Prompt { text, chars }
}

/// Cut a command to the size cap, on a character boundary so the text stays
/// valid UTF-8. Returns the text and whether it was cut.
pub fn truncate(command: &str) -> (String, bool) {
    if command.len() <= MAX_COMMAND_BYTES {
        return (command.to_string(), false);
    }
    let mut end = MAX_COMMAND_BYTES;
    while end > 0 && !command.is_char_boundary(end) {
        end -= 1;
    }
    (command[..end].to_string(), true)
}

/// A summary of the analysis rather than the tree. The full invocation tree is
/// reconstructable by replaying the command through `analyze`, which is what
/// makes `ANALYZER_VERSION` load-bearing.
pub fn projection(analysis: &Analysis, analyze_micros: u64) -> AnalysisProjection {
    let resolved_writes: Vec<String> = analysis
        .file_effects
        .iter()
        .filter_map(|e| match &e.target {
            Target::Path(p) => Some(p.clone()),
            Target::Unresolved | Target::Ephemeral { .. } | Target::Within(_) => None,
        })
        .collect();
    // An unresolved write is not an `Uncertainty` — the analyser records it as
    // a `Target::Unresolved` effect — so its span reaches the projection here
    // or not at all. It is the span that ranks which construct to teach the
    // parser next, so it is kept rather than reduced to a count.
    let unresolved_writes: Vec<UnresolvedWrite> = analysis
        .file_effects
        .iter()
        .filter(|e| e.target == Target::Unresolved)
        .map(|e| UnresolvedWrite {
            op: e.op.name().to_string(),
            start: e.location.outer.start,
            end: e.location.outer.end,
        })
        .collect();
    let uncertainties: Vec<_> = analysis.uncertainties.iter().map(uncertainty).collect();
    AnalysisProjection {
        counts: Counts {
            invocations: analysis.invocations.len(),
            resolved_writes: resolved_writes.len(),
            unresolved_writes: unresolved_writes.len(),
            uncertainties: uncertainties.len(),
        },
        resolved_writes,
        unresolved_writes,
        tree_effects: analysis
            .tree_effects
            .iter()
            .map(|t| t.scope.clone())
            .collect(),
        script_files: analysis
            .script_files
            .iter()
            .map(|s| match s.script.known() {
                Some(path) => path.to_string(),
                None => "<unresolved>".to_string(),
            })
            .collect(),
        programs: analysis
            .invocations
            .iter()
            .map(|i| match &i.program {
                Value::Known(p) => p.clone(),
                Value::Unknown | Value::Within(_) => "<unknown>".to_string(),
                Value::Ephemeral(_) => "<ephemeral>".to_string(),
            })
            .collect(),
        uncertainties,
        analyze_micros,
    }
}

fn uncertainty(u: &Uncertainty) -> devkit_common::harness_log::Uncertainty {
    devkit_common::harness_log::Uncertainty {
        kind: kind_name(&u.kind),
        detail: (!u.detail.is_empty()).then(|| u.detail.clone()),
        start: span(&u.location).map(|r| r.start),
        end: span(&u.location).map(|r| r.end),
    }
}

/// The span into the command as the hook received it. An uncertainty found
/// inside an embedded script carries an inner span too; the outer one is what a
/// reader replaying the original command can index with.
fn span(l: &Location) -> Option<std::ops::Range<usize>> {
    (l.outer.end >= l.outer.start).then(|| l.outer.clone())
}

/// A stable name per uncertainty kind, so a cohort query groups on a string
/// rather than on a `Debug` rendering that a refactor could move.
fn kind_name(k: &UncertaintyKind) -> String {
    match k {
        UncertaintyKind::UnresolvedWrite => "unresolved_write".into(),
        UncertaintyKind::UnsupportedLanguage(lang) => format!("unsupported_language:{lang}"),
        UncertaintyKind::ParseError => "parse_error".into(),
        UncertaintyKind::LimitExhausted(limit) => format!("limit_exhausted:{}", limit.name()),
        UncertaintyKind::UnresolvedInvocation => "unresolved_invocation".into(),
    }
}

#[cfg(test)]
mod tests {
    use devkit_command::{Context, Dialect, Limits, PathStyle};

    use super::*;

    fn ctx() -> Context {
        Context {
            dialect: Dialect::Bash,
            cwd: Some("/repo".into()),
            path_style: PathStyle::Unix,
            limits: Limits::default(),
        }
    }

    #[test]
    fn the_projection_carries_every_uncertainty_and_the_counts() {
        let a = devkit_command::analyze("cat in.txt > out.txt", &ctx());
        let p = projection(&a, 42);
        assert_eq!(p.uncertainties.len(), a.uncertainties.len());
        assert_eq!(p.counts.uncertainties, a.uncertainties.len());
        assert_eq!(p.counts.invocations, a.invocations.len());
        assert_eq!(p.counts.resolved_writes, p.resolved_writes.len());
        assert!(p.resolved_writes.iter().any(|w| w.ends_with("out.txt")));
        assert!(p.programs.iter().any(|w| w == "cat"));
        assert_eq!(p.analyze_micros, 42);
    }

    /// An unresolved write is located, never named: the path is what could not
    /// be determined. The span is what points at the construct to teach the
    /// parser next.
    #[test]
    fn an_unresolved_write_is_counted_and_located_not_named() {
        let command = "echo x > \"$OUT\"";
        let a = devkit_command::analyze(command, &ctx());
        let p = projection(&a, 0);
        assert_eq!(p.counts.unresolved_writes, 1);
        assert!(p.resolved_writes.is_empty());
        let u = &p.unresolved_writes[0];
        assert_eq!(u.op, "overwrite");
        assert!(u.end > u.start && u.end <= command.len(), "{u:?}");
        assert!(command[u.start..u.end].contains("$OUT"), "{u:?}");
    }

    #[test]
    fn the_analyzer_version_is_not_the_crate_version() {
        assert_ne!(
            ANALYZER_VERSION.to_string(),
            env!("CARGO_PKG_VERSION"),
            "a release-please stamped version would call the corpus stale each release"
        );
    }

    /// A record's `event` and the verb a manifest types must stay the same
    /// string, or a reader's cohort query silently misses a whole verb.
    #[test]
    fn every_verb_names_itself_the_way_a_manifest_spells_it() {
        use clap::ValueEnum;
        for event in HookEvent::value_variants() {
            let clap_name = event
                .to_possible_value()
                .expect("every variant is selectable")
                .get_name()
                .to_string();
            assert_eq!(event.as_str(), clap_name, "{event:?}");
        }
        assert_eq!(HookEvent::PreToolUse.as_str(), "pre-tool-use");
    }

    /// The mapping is not injective, so a record names both. Codex sends `Stop`
    /// and `Interrupt` to the one `stop` verb, and a reader asking which vendor
    /// event produced a record cannot recover it from the verb alone.
    #[test]
    fn every_record_names_both_its_verb_and_its_vendor_event() {
        let payload = serde_json::json!({
            "hook_event_name": "Interrupt",
            "session_id": "s1",
            "turn_id": "t1"
        });
        let r = envelope(
            &payload,
            HookEvent::Stop,
            Some(Harness::Codex),
            &Checkout::at(&payload_cwd(&payload)),
            Kind::Lifecycle,
        );
        assert_eq!(r.event, "stop");
        assert_eq!(r.vendor_event.as_deref(), Some("Interrupt"));
        assert_eq!(r.harness.as_deref(), Some("codex"));
        assert_eq!(r.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn cursors_spellings_of_the_ids_reach_the_envelope() {
        let payload = serde_json::json!({
            "hook_event_name": "preToolUse",
            "cursor_version": "1.7.0",
            "conversation_id": "c1",
            "parent_conversation_id": "p1",
            "tool_use_id": "u1",
            "tool_input": { "working_directory": "/w" }
        });
        let r = envelope(
            &payload,
            HookEvent::PreToolUse,
            None,
            &Checkout::at(&payload_cwd(&payload)),
            Kind::Lifecycle,
        );
        assert_eq!(r.session_id.as_deref(), Some("c1"));
        assert_eq!(r.agent_id.as_deref(), Some("p1"));
        assert_eq!(r.tool_use_id.as_deref(), Some("u1"));
        assert_eq!(r.cwd.as_deref(), Some(std::path::Path::new("/w")));
        assert_eq!(
            r.harness.as_deref(),
            Some("cursor"),
            "inferred, not declared"
        );
    }

    #[test]
    fn a_command_over_the_cap_is_cut_on_a_character_boundary() {
        let (short, cut) = truncate("ls -la");
        assert_eq!(short, "ls -la");
        assert!(!cut);

        // Multi-byte, so a naive cut would land mid-character.
        let long = "é".repeat(MAX_COMMAND_BYTES);
        let (cut_text, was_cut) = truncate(&long);
        assert!(was_cut);
        assert!(cut_text.len() <= MAX_COMMAND_BYTES);
        assert!(long.starts_with(&cut_text));
    }
}
