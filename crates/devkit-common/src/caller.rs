//! Who is running this command. A coding agent and a person get different
//! answers from the `required` markings in `[templates.variables]`, and this
//! is the single door that decides which.

use std::io::IsTerminal;

/// Environment variables carrying a coding-agent harness's own session id, one
/// per harness. `CODEX_THREAD_ID` is excluded deliberately: it holds the same
/// value as `CODEX_SESSION_ID`, so listing it would manufacture a false
/// ambiguity for `devkit-locks`, which reads this same list.
pub const HARNESS_SESSION_VARS: [&str; 2] = ["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Caller {
    Human,
    Agent,
}

/// The classification, with the reads done by the caller so this stays pure.
///
/// A harness session id is positive evidence of a coding agent, so it leads: a
/// pipe, a cron job and `devrun task foo < /dev/null` are not agents. The
/// terminal check stays as a backstop because the list above covers Claude
/// Code and Codex, while Cursor is recognised from a hook payload field rather
/// than the environment.
///
/// The classification leans toward `Agent`. An agent misread as human means a
/// marking never fires and the default is taken silently, which is the defect
/// this exists to close. A human misread as agent is asked for an arg they
/// expected to be defaulted: recoverable, and never silent.
pub fn decide_caller(harness_session: bool, is_tty: bool) -> Caller {
    if harness_session || !is_tty {
        Caller::Agent
    } else {
        Caller::Human
    }
}

/// `DEVKIT_CALLER` as an explicit answer. Blank or unrecognised is no opinion,
/// the same tri-state shape `harness::parse_env_override` uses.
pub fn parse_caller_override(val: Option<&str>) -> Option<Caller> {
    match val.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("agent") => Some(Caller::Agent),
        Some("human") => Some(Caller::Human),
        _ => None,
    }
}

/// Resolve once per command at the CLI edge and pass the result down.
pub fn caller() -> Caller {
    if let Some(c) = parse_caller_override(std::env::var("DEVKIT_CALLER").ok().as_deref()) {
        return c;
    }
    let harness = HARNESS_SESSION_VARS
        .iter()
        .any(|v| std::env::var(v).is_ok_and(|s| !s.is_empty()));
    decide_caller(harness, std::io::stdin().is_terminal())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harness_session_means_agent_whatever_the_terminal_says() {
        assert_eq!(decide_caller(true, true), Caller::Agent);
        assert_eq!(decide_caller(true, false), Caller::Agent);
    }

    #[test]
    fn without_a_harness_session_the_terminal_decides() {
        assert_eq!(decide_caller(false, true), Caller::Human);
        assert_eq!(decide_caller(false, false), Caller::Agent);
    }

    #[test]
    fn the_override_parses_both_names_and_ignores_anything_else() {
        assert_eq!(parse_caller_override(Some("agent")), Some(Caller::Agent));
        assert_eq!(parse_caller_override(Some(" HUMAN ")), Some(Caller::Human));
        assert_eq!(parse_caller_override(Some("")), None);
        assert_eq!(parse_caller_override(Some("yes")), None);
        assert_eq!(parse_caller_override(None), None);
    }
}
