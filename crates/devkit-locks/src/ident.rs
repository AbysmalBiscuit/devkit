//! Session identity resolution and anchor-PID policy.

/// Environment variables carrying the harness's own session id, one per
/// harness. `CODEX_THREAD_ID` is excluded deliberately: it holds the same value
/// as `CODEX_SESSION_ID`, so listing it would manufacture a false ambiguity.
pub const HARNESS_SESSION_VARS: [&str; 2] = ["CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"];

/// One harness's answer to "which session is this": the variable it came from
/// and the value it held. The variable name travels with the value so a refusal
/// can tell the reader which harness contributed which id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub var: &'static str,
    pub value: String,
}

impl std::fmt::Display for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}={}", self.var, self.value)
    }
}

/// The outcome of resolving a holder. `Ambiguous` carries the candidates whose
/// values differ, in table order; each value is a string a caller pastes back
/// as `--as`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    Resolved(String),
    Ambiguous(Vec<Candidate>),
}

impl Identity {
    /// The id to label read-only output with; the first candidate under
    /// ambiguity.
    pub fn or_first(self) -> String {
        match self {
            Identity::Resolved(s) => s,
            Identity::Ambiguous(c) => c
                .into_iter()
                .next()
                .map(|c| c.value)
                .unwrap_or_else(|| "unknown".into()),
        }
    }
}

/// Environment inputs for identity resolution, captured so the logic is
/// pure/testable.
pub struct Env {
    pub harness_sessions: Vec<Candidate>,
    pub devkit_session: Option<String>,
    pub tmux_pane: Option<String>,
    pub tty: Option<String>,
    pub ppid: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Env {
            harness_sessions: harness_candidates(),
            devkit_session: nonempty("DEVKIT_SESSION"),
            tmux_pane: nonempty("TMUX_PANE"),
            tty: devkit_common::sys::controlling_tty(),
            ppid: devkit_common::sys::parent_pid().map(|p| p.to_string()),
        }
    }
}

/// Resolve the holder identity by precedence: `--as` > a harness session id >
/// `$DEVKIT_SESSION` > `$TMUX_PANE` > controlling tty > parent pid.
///
/// The harness id outranks `$DEVKIT_SESSION` because the write hook ignores
/// that variable outright, so any value visible under a harness already
/// disagrees with what enforcement decided; it outranks tmux and the tty
/// because those name a terminal rather than a session.
pub fn resolve_identity(as_flag: Option<&str>, env: &Env) -> Identity {
    if let Some(f) = as_flag {
        return Identity::Resolved(f.to_string());
    }
    let mut distinct: Vec<Candidate> = Vec::new();
    for c in &env.harness_sessions {
        if !distinct.iter().any(|d| d.value == c.value) {
            distinct.push(c.clone());
        }
    }
    match distinct.len() {
        0 => {}
        1 => return Identity::Resolved(distinct.remove(0).value),
        _ => return Identity::Ambiguous(distinct),
    }
    let fallback = env
        .devkit_session
        .clone()
        .or_else(|| env.tmux_pane.clone())
        .or_else(|| env.tty.clone())
        .or_else(|| env.ppid.clone())
        .unwrap_or_else(|| "unknown".to_string());
    Identity::Resolved(fallback)
}

pub fn identity(as_flag: Option<&str>) -> Identity {
    resolve_identity(as_flag, &Env::from_process())
}

/// A durable anchor pid, recorded only when one can be trusted: the tmux pane's
/// process, else a parent pid when attached to a tty. Agent-via-Bash sessions
/// (no tmux, no tty) get None and rely on TTL + explicit release.
pub fn decide_anchor_pid(tmux_pid: Option<u32>, is_tty: bool, ppid: u32) -> Option<u32> {
    if let Some(p) = tmux_pid {
        return Some(p);
    }
    if is_tty {
        return Some(ppid);
    }
    None
}

pub fn anchor_pid() -> Option<u32> {
    use std::io::IsTerminal;
    decide_anchor_pid(
        tmux_pane_pid(),
        std::io::stdin().is_terminal(),
        devkit_common::sys::parent_pid().unwrap_or(0),
    )
}

/// Best-effort: ask tmux for the current pane's process pid when inside tmux.
fn tmux_pane_pid() -> Option<u32> {
    std::env::var_os("TMUX_PANE")?;
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-F", "#{pane_pid}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// Environment prefixes every known harness stamps on its variables. A process
/// carrying one of these but none of `HARNESS_SESSION_VARS` is running under a
/// harness whose session variable devkit no longer recognises.
pub const HARNESS_ENV_PREFIXES: [&str; 2] = ["CLAUDE_CODE_", "CODEX_"];

/// The harness session ids visible to this process, in table order.
pub fn harness_candidates() -> Vec<Candidate> {
    HARNESS_SESSION_VARS
        .iter()
        .filter_map(|var| {
            std::env::var(var)
                .ok()
                .filter(|s| !s.is_empty())
                .map(|value| Candidate { var, value })
        })
        .collect()
}

/// True when some variable carries a known harness prefix.
pub fn harness_env_present() -> bool {
    std::env::vars_os().any(|(k, _)| {
        k.to_str()
            .is_some_and(|k| HARNESS_ENV_PREFIXES.iter().any(|p| k.starts_with(p)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(
        harness: Vec<(&'static str, &str)>,
        session: Option<&str>,
        pane: Option<&str>,
        tty: Option<&str>,
        ppid: Option<&str>,
    ) -> Env {
        Env {
            harness_sessions: harness
                .into_iter()
                .map(|(var, value)| Candidate {
                    var,
                    value: value.to_string(),
                })
                .collect(),
            devkit_session: session.map(str::to_string),
            tmux_pane: pane.map(str::to_string),
            tty: tty.map(str::to_string),
            ppid: ppid.map(str::to_string),
        }
    }

    #[test]
    fn explicit_flag_wins() {
        let e = env(
            vec![],
            Some("envsess"),
            Some("%3"),
            Some("/dev/pts/1"),
            Some("42"),
        );
        assert_eq!(
            resolve_identity(Some("flag"), &e),
            Identity::Resolved("flag".into())
        );
    }
    #[test]
    fn env_session_beats_tmux() {
        let e = env(vec![], Some("envsess"), Some("%3"), None, Some("42"));
        assert_eq!(
            resolve_identity(None, &e),
            Identity::Resolved("envsess".into())
        );
    }
    #[test]
    fn tmux_pane_beats_tty_and_ppid() {
        let e = env(vec![], None, Some("%3"), Some("/dev/pts/1"), Some("42"));
        assert_eq!(resolve_identity(None, &e), Identity::Resolved("%3".into()));
    }
    #[test]
    fn falls_through_to_tty_then_ppid() {
        assert_eq!(
            resolve_identity(
                None,
                &env(vec![], None, None, Some("/dev/pts/1"), Some("42"))
            ),
            Identity::Resolved("/dev/pts/1".into())
        );
        assert_eq!(
            resolve_identity(None, &env(vec![], None, None, None, Some("42"))),
            Identity::Resolved("42".into())
        );
    }

    #[test]
    fn harness_session_beats_devkit_session_and_tmux() {
        let e = env(
            vec![("CLAUDE_CODE_SESSION_ID", "sess-1")],
            Some("envsess"),
            Some("%3"),
            None,
            Some("42"),
        );
        assert_eq!(
            resolve_identity(None, &e),
            Identity::Resolved("sess-1".into())
        );
    }

    #[test]
    fn explicit_flag_still_beats_a_harness_session() {
        let e = env(
            vec![("CODEX_SESSION_ID", "sess-1")],
            None,
            None,
            None,
            Some("42"),
        );
        assert_eq!(
            resolve_identity(Some("flag"), &e),
            Identity::Resolved("flag".into())
        );
    }

    #[test]
    fn equal_harness_values_are_one_candidate() {
        let e = env(
            vec![
                ("CLAUDE_CODE_SESSION_ID", "sess-1"),
                ("CODEX_SESSION_ID", "sess-1"),
            ],
            None,
            None,
            None,
            Some("42"),
        );
        assert_eq!(
            resolve_identity(None, &e),
            Identity::Resolved("sess-1".into())
        );
    }

    #[test]
    fn two_distinct_harness_values_are_ambiguous() {
        let e = env(
            vec![
                ("CLAUDE_CODE_SESSION_ID", "outer"),
                ("CODEX_SESSION_ID", "inner"),
            ],
            None,
            None,
            None,
            Some("42"),
        );
        match resolve_identity(None, &e) {
            Identity::Ambiguous(c) => {
                let shown: Vec<String> = c.iter().map(|c| c.to_string()).collect();
                assert_eq!(shown, vec![
                    "CLAUDE_CODE_SESSION_ID=outer".to_string(),
                    "CODEX_SESSION_ID=inner".to_string()
                ]);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn an_explicit_flag_resolves_an_ambiguous_environment() {
        let e = env(
            vec![
                ("CLAUDE_CODE_SESSION_ID", "outer"),
                ("CODEX_SESSION_ID", "inner"),
            ],
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            resolve_identity(Some("mine"), &e),
            Identity::Resolved("mine".into())
        );
    }

    #[test]
    fn anchor_pid_prefers_tmux_then_tty_else_none() {
        assert_eq!(decide_anchor_pid(Some(5), true, 9), Some(5));
        assert_eq!(decide_anchor_pid(None, true, 9), Some(9));
        assert_eq!(decide_anchor_pid(None, false, 9), None);
    }
}
