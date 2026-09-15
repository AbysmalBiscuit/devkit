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
    harness::Harness,
    harness_log::{
        AnalysisProjection, Counts, Kind, Record, SCHEMA_VERSION, UnresolvedWrite, now_rfc3339,
    },
};
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

/// The envelope, filled in from whatever the payload carries. Every field a
/// harness did not send stays `None`: nothing here is guessed from the hook
/// process's own environment except `cwd`, which a record is useless without.
pub fn envelope(
    payload: &Json,
    event: HookEvent,
    harness: Option<Harness>,
    kind: Kind,
) -> Box<Record> {
    let cwd = text(payload, "cwd")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            payload
                .get("tool_input")?
                .get("working_directory")?
                .as_str()
                .map(std::path::PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok());
    let project_root = cwd
        .as_deref()
        .and_then(|c| devkit_common::git::checkout_root(c).ok());
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
        cwd,
        project_root,
        kind,
    })
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
            Target::Unresolved | Target::Ephemeral { .. } => None,
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
            op: format!("{:?}", e.op).to_lowercase(),
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
                Value::Unknown => "<unknown>".to_string(),
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
        UncertaintyKind::LimitExhausted(limit) => format!("limit_exhausted:{limit:?}"),
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
        let r = envelope(&payload, HookEvent::PreToolUse, None, Kind::Lifecycle);
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
