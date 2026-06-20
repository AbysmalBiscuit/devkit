//! Session identity resolution and anchor-PID policy.

/// Environment inputs for identity resolution, captured so the logic is pure/testable.
pub struct Env {
    pub devkit_session: Option<String>,
    pub tmux_pane: Option<String>,
    pub tty: Option<String>,
    pub ppid: Option<String>,
}

impl Env {
    pub fn from_process() -> Self {
        let nonempty = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Env {
            devkit_session: nonempty("DEVKIT_SESSION"),
            tmux_pane: nonempty("TMUX_PANE"),
            tty: devkit_common::sys::controlling_tty(),
            ppid: devkit_common::sys::parent_pid().map(|p| p.to_string()),
        }
    }
}

/// Resolve the holder identity by precedence:
/// `--as` > `$DEVKIT_SESSION` > `$TMUX_PANE` > controlling tty > parent pid.
pub fn resolve_identity(as_flag: Option<&str>, env: &Env) -> String {
    as_flag
        .map(str::to_string)
        .or_else(|| env.devkit_session.clone())
        .or_else(|| env.tmux_pane.clone())
        .or_else(|| env.tty.clone())
        .or_else(|| env.ppid.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// A durable anchor pid, recorded only when one can be trusted: the tmux pane's
/// process, else a parent pid when attached to a tty. Agent-via-Bash sessions (no
/// tmux, no tty) get None and rely on TTL + explicit release.
pub fn decide_anchor_pid(tmux_pid: Option<u32>, is_tty: bool, ppid: u32) -> Option<u32> {
    if let Some(p) = tmux_pid {
        return Some(p);
    }
    if is_tty {
        return Some(ppid);
    }
    None
}

pub fn identity(as_flag: Option<&str>) -> String {
    resolve_identity(as_flag, &Env::from_process())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn env(
        session: Option<&str>,
        pane: Option<&str>,
        tty: Option<&str>,
        ppid: Option<&str>,
    ) -> Env {
        Env {
            devkit_session: session.map(str::to_string),
            tmux_pane: pane.map(str::to_string),
            tty: tty.map(str::to_string),
            ppid: ppid.map(str::to_string),
        }
    }

    #[test]
    fn explicit_flag_wins() {
        let e = env(Some("envsess"), Some("%3"), Some("/dev/pts/1"), Some("42"));
        assert_eq!(resolve_identity(Some("flag"), &e), "flag");
    }
    #[test]
    fn env_session_beats_tmux() {
        let e = env(Some("envsess"), Some("%3"), None, Some("42"));
        assert_eq!(resolve_identity(None, &e), "envsess");
    }
    #[test]
    fn tmux_pane_beats_tty_and_ppid() {
        let e = env(None, Some("%3"), Some("/dev/pts/1"), Some("42"));
        assert_eq!(resolve_identity(None, &e), "%3");
    }
    #[test]
    fn falls_through_to_tty_then_ppid() {
        assert_eq!(
            resolve_identity(None, &env(None, None, Some("/dev/pts/1"), Some("42"))),
            "/dev/pts/1"
        );
        assert_eq!(
            resolve_identity(None, &env(None, None, None, Some("42"))),
            "42"
        );
    }

    #[test]
    fn anchor_pid_prefers_tmux_then_tty_else_none() {
        assert_eq!(decide_anchor_pid(Some(5), true, 9), Some(5));
        assert_eq!(decide_anchor_pid(None, true, 9), Some(9));
        assert_eq!(decide_anchor_pid(None, false, 9), None);
    }
}
