//! Claude Code hook glue: holder derivation, payload parsing, decision envelope,
//! and per-checkout activation. Agent-specific shapes live here; the registry
//! decision logic stays in `model`/`store`.

use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Tool names whose writes the harness governs. The Claude Code names carry one
/// target in `tool_input.file_path`; Codex's `apply_patch` carries a whole patch
/// envelope in `tool_input.command` and may name several.
const WRITE_TOOLS: [&str; 5] = ["Edit", "MultiEdit", "Write", "NotebookEdit", "apply_patch"];

/// Codex's `apply_patch` tool name, whose payload needs the envelope parser.
const APPLY_PATCH: &str = "apply_patch";

/// Envelope headers that name a file, in the order `apply_patch` writes them.
/// `Move to` is a rename's destination, so a rename claims both ends.
const PATCH_VERBS: [&str; 4] = ["Add File", "Update File", "Delete File", "Move to"];

/// Two-level holder id: top-level agents are `session_id`; sub-agents are
/// `session_id/agent_id`. The Claude Code payload exposes no deeper ancestry.
pub fn holder_from_fields(session_id: &str, agent_id: Option<&str>) -> String {
    match agent_id {
        Some(a) if !a.is_empty() => format!("{session_id}/{a}"),
        _ => session_id.to_string(),
    }
}

#[derive(Debug)]
pub enum HookEvent {
    Write {
        tool_name: String,
        file_paths: Vec<String>,
        holder: String,
    },
    ReleaseSubagent {
        holder: String,
    },
    ReleaseSession {
        holder: String,
    },
    Ignore,
}

fn str_field<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Every file an `apply_patch` envelope writes, in the order it names them. Paths
/// are taken verbatim — they are relative to the invoking session's cwd, which the
/// caller supplies; this parser does not resolve them.
pub fn apply_patch_paths(command: &str) -> Vec<String> {
    command
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("*** ")?;
            let (verb, path) = rest.split_once(": ")?;
            PATCH_VERBS
                .contains(&verb)
                .then(|| path.trim())
                .filter(|p| !p.is_empty())
                .map(str::to_string)
        })
        .collect()
}

/// Classify a hook payload. `event` is the subcommand arg
/// (`pretooluse` | `subagent-stop` | `session-end`).
pub fn parse_event(event: &str, p: &Value) -> HookEvent {
    let Some(session) = str_field(p, "session_id") else {
        return HookEvent::Ignore;
    };
    let agent = str_field(p, "agent_id");
    let holder = holder_from_fields(session, agent);
    match event {
        "pretooluse" => {
            let tool = str_field(p, "tool_name").unwrap_or("");
            if !WRITE_TOOLS.contains(&tool) {
                return HookEvent::Ignore;
            }
            let input = p.get("tool_input");
            let file_paths = if tool == APPLY_PATCH {
                input
                    .and_then(|ti| str_field(ti, "command"))
                    .map(apply_patch_paths)
                    .unwrap_or_default()
            } else {
                input
                    .and_then(|ti| str_field(ti, "file_path"))
                    .map(|fp| vec![fp.to_string()])
                    .unwrap_or_default()
            };
            if file_paths.is_empty() {
                return HookEvent::Ignore;
            }
            HookEvent::Write {
                tool_name: tool.to_string(),
                file_paths,
                holder,
            }
        }
        "subagent-stop" => HookEvent::ReleaseSubagent { holder },
        "session-end" => HookEvent::ReleaseSession { holder },
        _ => HookEvent::Ignore,
    }
}

/// The current PreToolUse deny envelope. `reason` is surfaced to the agent.
pub fn deny_json(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
}

/// The `[harness]` table of a checkout's `devkit.toml`.
#[derive(Deserialize, Default, schemars::JsonSchema)]
pub struct HarnessSection {
    /// Refuse writes to paths this checkout has not claimed with `lockm`.
    #[serde(default)]
    pub enforce_writes: bool,
}

#[derive(Deserialize, Default)]
struct HarnessProbe {
    #[serde(default)]
    harness: HarnessSection,
}

/// Read `enforce_writes` from a devkit-config TOML body. Parses leniently — only
/// the `[harness]` table is consulted, so a full project config and a bare
/// `[harness]`-only file both work; unparseable input reads as off.
fn harness_flag_in(body: &str) -> bool {
    toml::from_str::<HarnessProbe>(body)
        .map(|p| p.harness.enforce_writes)
        .unwrap_or(false)
}

/// True iff `<root>/devkit.toml` opts this checkout into write enforcement.
pub fn harness_enabled(root: &Path) -> bool {
    std::fs::read_to_string(root.join("devkit.toml"))
        .map(|b| harness_flag_in(&b))
        .unwrap_or(false)
}

/// Parse the `DEVKIT_ENFORCE_WRITES` override into an explicit on/off, or `None`
/// when unset/blank/unrecognized — in which case callers fall back to the
/// file-based opt-ins. Case- and whitespace-insensitive.
fn parse_env_override(val: Option<&str>) -> Option<bool> {
    match val.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => Some(true),
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => Some(false),
        _ => None,
    }
}

/// The global devkit config file: `$DEVKIT_CONFIG`, else `~/.config/devkit/config.toml`.
/// Mirrors the `~/.config/devkit/config.toml` base layer the resolver loads, so the
/// harness reads the same global config the other binaries do.
fn global_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/devkit/config.toml"))
}

/// True iff the global devkit config opts every checkout into write enforcement.
fn global_harness_enabled() -> bool {
    global_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|b| harness_flag_in(&b))
        .unwrap_or(false)
}

/// Combine the enforcement opt-in sources. The env override is an explicit
/// on/off master switch; without it, enforcement is on when either the checkout's
/// own `devkit.toml` or the global config sets `[harness] enforce_writes = true`.
fn resolve_enforcement(env: Option<bool>, checkout: bool, global: bool) -> bool {
    match env {
        Some(v) => v,
        None => checkout || global,
    }
}

/// Whether write enforcement is active for the checkout rooted at `root`, across
/// all opt-in sources: the `DEVKIT_ENFORCE_WRITES` env var (explicit override),
/// the checkout's `devkit.toml`, and the global devkit config — see
/// [`resolve_enforcement`] for precedence.
pub fn enforcement_enabled(root: &Path) -> bool {
    resolve_enforcement(
        parse_env_override(std::env::var("DEVKIT_ENFORCE_WRITES").ok().as_deref()),
        harness_enabled(root),
        global_harness_enabled(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn holder_top_level_is_session() {
        assert_eq!(holder_from_fields("S", None), "S");
    }

    #[test]
    fn holder_subagent_is_session_slash_agent() {
        assert_eq!(holder_from_fields("S", Some("a1")), "S/a1");
    }

    #[test]
    fn parse_write_event_pulls_file_and_holder() {
        let p = json!({
            "session_id": "S",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/src/a.rs" }
        });
        match parse_event("pretooluse", &p) {
            HookEvent::Write {
                tool_name,
                file_paths,
                holder,
            } => {
                assert_eq!(tool_name, "Edit");
                assert_eq!(file_paths, vec!["/repo/src/a.rs"]);
                assert_eq!(holder, "S");
            }
            other => panic!("expected Write, got {other:?}"),
        }
    }

    #[test]
    fn parse_write_event_subagent_holder() {
        let p = json!({
            "session_id": "S", "agent_id": "a1",
            "tool_name": "Write", "tool_input": { "file_path": "/repo/x" }
        });
        match parse_event("pretooluse", &p) {
            HookEvent::Write { holder, .. } => assert_eq!(holder, "S/a1"),
            other => panic!("expected Write, got {other:?}"),
        }
    }

    #[test]
    fn parse_write_event_ignores_non_write_tool() {
        let p =
            json!({ "session_id": "S", "tool_name": "Bash", "tool_input": { "command": "ls" } });
        assert!(matches!(parse_event("pretooluse", &p), HookEvent::Ignore));
    }

    #[test]
    fn parse_write_event_ignores_missing_file_path() {
        let p = json!({ "session_id": "S", "tool_name": "Edit", "tool_input": {} });
        assert!(matches!(parse_event("pretooluse", &p), HookEvent::Ignore));
    }

    #[test]
    fn apply_patch_paths_covers_every_envelope_verb() {
        let patch = "*** Begin Patch\n\
                     *** Add File: src/new.rs\n\
                     +fn main() {}\n\
                     *** Update File: src/old.rs\n\
                     @@\n\
                     -a\n\
                     +b\n\
                     *** Delete File: src/gone.rs\n\
                     *** End Patch\n";
        assert_eq!(
            apply_patch_paths(patch),
            vec!["src/new.rs", "src/old.rs", "src/gone.rs"]
        );
    }

    #[test]
    fn apply_patch_paths_includes_both_ends_of_a_rename() {
        let patch = "*** Begin Patch\n\
                     *** Update File: src/from.rs\n\
                     *** Move to: src/to.rs\n\
                     *** End Patch\n";
        assert_eq!(apply_patch_paths(patch), vec!["src/from.rs", "src/to.rs"]);
    }

    #[test]
    fn parse_apply_patch_event_pulls_every_target() {
        let p = json!({
            "session_id": "S",
            "tool_name": "apply_patch",
            "tool_input": {
                "command": "*** Begin Patch\n*** Update File: a.rs\n*** Add File: b.rs\n*** End Patch\n"
            }
        });
        match parse_event("pretooluse", &p) {
            HookEvent::Write {
                tool_name,
                file_paths,
                holder,
            } => {
                assert_eq!(tool_name, "apply_patch");
                assert_eq!(file_paths, vec!["a.rs", "b.rs"]);
                assert_eq!(holder, "S");
            }
            other => panic!("expected Write, got {other:?}"),
        }
    }

    #[test]
    fn parse_apply_patch_event_ignores_a_patch_naming_no_file() {
        let p = json!({
            "session_id": "S",
            "tool_name": "apply_patch",
            "tool_input": { "command": "*** Begin Patch\n*** End Patch\n" }
        });
        assert!(matches!(parse_event("pretooluse", &p), HookEvent::Ignore));
    }

    #[test]
    fn parse_subagent_stop_releases_subagent_holder() {
        let p = json!({ "session_id": "S", "agent_id": "a1" });
        match parse_event("subagent-stop", &p) {
            HookEvent::ReleaseSubagent { holder } => assert_eq!(holder, "S/a1"),
            other => panic!("expected ReleaseSubagent, got {other:?}"),
        }
    }

    #[test]
    fn parse_session_end_releases_session_prefix() {
        let p = json!({ "session_id": "S" });
        match parse_event("session-end", &p) {
            HookEvent::ReleaseSession { holder } => assert_eq!(holder, "S"),
            other => panic!("expected ReleaseSession, got {other:?}"),
        }
    }

    #[test]
    fn deny_json_has_pretooluse_envelope() {
        let v = deny_json("blocked by S/a1");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "blocked by S/a1"
        );
    }

    #[test]
    fn parse_event_ignores_missing_session_id() {
        // Write payload with no session_id → Ignore (cannot establish a holder)
        let p = json!({ "tool_name": "Edit", "tool_input": { "file_path": "/repo/x" } });
        assert!(matches!(parse_event("pretooluse", &p), HookEvent::Ignore));
        // Empty session_id is treated as absent
        let p2 = json!({ "session_id": "", "tool_name": "Write", "tool_input": { "file_path": "/repo/x" } });
        assert!(matches!(parse_event("pretooluse", &p2), HookEvent::Ignore));
        // A release event without session_id is also ignored
        let p3 = json!({ "agent_id": "a1" });
        assert!(matches!(
            parse_event("subagent-stop", &p3),
            HookEvent::Ignore
        ));
    }

    #[test]
    fn harness_enabled_reads_flag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[harness]\nenforce_writes = true\n",
        )
        .unwrap();
        assert!(harness_enabled(dir.path()));
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[harness]\nenforce_writes = false\n",
        )
        .unwrap();
        assert!(!harness_enabled(dir.path()));
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[defaults]\nworktree_root = \"x\"\n",
        )
        .unwrap();
        assert!(!harness_enabled(dir.path())); // missing section → off, despite unrelated keys
        let _ = std::fs::remove_file(dir.path().join("devkit.toml"));
        assert!(!harness_enabled(dir.path())); // no devkit.toml → off
    }

    #[test]
    fn env_override_parses_truthy_falsy_and_unknown() {
        for on in ["1", "true", "TRUE", "yes", "On", "  true  "] {
            assert_eq!(parse_env_override(Some(on)), Some(true), "{on:?}");
        }
        for off in ["0", "false", "No", "off", " OFF "] {
            assert_eq!(parse_env_override(Some(off)), Some(false), "{off:?}");
        }
        // unset / blank / unrecognized → no opinion, fall back to files
        assert_eq!(parse_env_override(None), None);
        assert_eq!(parse_env_override(Some("")), None);
        assert_eq!(parse_env_override(Some("maybe")), None);
    }

    #[test]
    fn harness_flag_in_reads_section_leniently() {
        assert!(harness_flag_in("[harness]\nenforce_writes = true\n"));
        assert!(!harness_flag_in("[harness]\nenforce_writes = false\n"));
        // full project config carrying the flag still reads true
        assert!(harness_flag_in(
            "[defaults]\nworktree_root = \"x\"\n[harness]\nenforce_writes = true\n"
        ));
        // no [harness] section, or junk → off (never panics)
        assert!(!harness_flag_in("[defaults]\nworktree_root = \"x\"\n"));
        assert!(!harness_flag_in("not even toml ["));
    }

    #[test]
    fn enforcement_precedence_env_then_files() {
        // env is an explicit master switch, wins over both files
        assert!(resolve_enforcement(Some(true), false, false));
        assert!(!resolve_enforcement(Some(false), true, true));
        // no env → enforce if either the checkout file or the global config opts in
        assert!(resolve_enforcement(None, true, false));
        assert!(resolve_enforcement(None, false, true));
        assert!(resolve_enforcement(None, true, true));
        assert!(!resolve_enforcement(None, false, false));
    }
}
