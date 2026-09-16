//! Reading a recorded corpus back. Behind the `corpus` feature, because it
//! exists for offline measurement rather than for anything the hooks do.
//!
//! Two record formats feed it. `devkit hook pre-tool-use` writes a `shell_pre`
//! record with the command at the top level; an external capture script wrote a
//! nested `tool_input.command`. Orphaning an accumulated corpus would be a bad
//! trade for a format change, so both are read.

use serde_json::Value;

use crate::Dialect;

/// The command a record is about, whichever format wrote it.
pub fn command_of(record: &Value) -> Option<&str> {
    record.get("command").and_then(Value::as_str).or_else(|| {
        record
            .as_object()?
            .values()
            .find_map(|v| v.get("command").and_then(Value::as_str))
    })
}

/// The dialect the record was analysed in, when it says. A devkit-written
/// record knows, so a reader should not re-derive it from harness plus
/// platform and risk reading the command in a shell it was never parsed as.
pub fn dialect_of(record: &Value) -> Option<Dialect> {
    Dialect::from_name(record.get("dialect").and_then(Value::as_str)?)
}

/// The dialect to infer when a record does not carry one, from whatever the
/// external capture format recorded. This is the older, weaker reading:
/// `dialect_of` wins wherever it answers.
pub fn infer_dialect(harness: &str, platform: &str, tool: &str) -> Dialect {
    if tool == "PowerShell" || (harness == "codex" && platform == "windows") {
        Dialect::PowerShell
    } else {
        Dialect::Bash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn both_corpus_formats_feed_the_probe() {
        // A devkit `shell_pre` record: the command at the top level.
        let native = parse(r#"{"kind":"shell_pre","command":"ls -la","dialect":"bash"}"#);
        // An externally produced record: nested under the tool input.
        let legacy = parse(r#"{"tool_input":{"command":"ls -la"}}"#);
        assert_eq!(command_of(&native).unwrap(), "ls -la");
        assert_eq!(command_of(&legacy).unwrap(), "ls -la");
        assert_eq!(
            dialect_of(&native),
            Some(Dialect::Bash),
            "the record's own dialect wins"
        );
        assert_eq!(dialect_of(&legacy), None, "and an old record has none");
    }

    #[test]
    fn a_record_with_no_command_is_skipped_rather_than_guessed_at() {
        assert!(command_of(&parse(r#"{"kind":"session","end":"start"}"#)).is_none());
        assert!(command_of(&parse("{}")).is_none());
    }

    #[test]
    fn an_unknown_dialect_name_is_none_not_a_default() {
        // A reader that silently fell back to bash would report a PowerShell
        // corpus as a parser regression.
        assert_eq!(dialect_of(&parse(r#"{"dialect":"zsh"}"#)), None);
        assert_eq!(dialect_of(&parse("{}")), None);
    }

    #[test]
    fn inference_is_the_fallback_it_always_was() {
        assert_eq!(infer_dialect("claude-code", "linux", "Bash"), Dialect::Bash);
        assert_eq!(
            infer_dialect("claude-code", "windows", "PowerShell"),
            Dialect::PowerShell
        );
        assert_eq!(
            infer_dialect("codex", "windows", "Bash"),
            Dialect::PowerShell
        );
    }
}
