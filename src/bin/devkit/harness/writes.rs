//! The shell hook's write stage: turn an analysis into claims, scope checks
//! and policy findings, and claim through the same registry path structured
//! edits use.

#![allow(dead_code)]

use std::{sync::mpsc, time::Duration};

use devkit_command::{Analysis, FileOp, Target, UncertaintyKind, Value};
use devkit_common::harness::HarnessPolicy;
use devkit_config::PolicyAction;
use devkit_locks::model::{Conflict, WriteDecision};

const PREFIX: &str = "devkit write-harness:";

#[derive(Debug, Default)]
pub struct Evaluation {
    pub blocks: Vec<String>,
    pub warnings: Vec<String>,
    /// Absolute write targets, in order, without repeats.
    pub claims: Vec<String>,
    /// `(directory, whole_checkout)` for writers of an unenumerated file set.
    pub scopes: Vec<(String, bool)>,
}

impl Evaluation {
    pub fn needs_registry(&self) -> bool {
        !self.claims.is_empty() || !self.scopes.is_empty()
    }

    fn apply(&mut self, action: PolicyAction, message: String) {
        match action {
            PolicyAction::Block => self.blocks.push(message),
            PolicyAction::Warn => self.warnings.push(message),
            PolicyAction::Allow => {}
        }
    }
}

const UNRESOLVED_FIX: &str = "Rewrite the edit so each target is a literal path, or a variable assigned a literal earlier in the same command, or make it with a structured edit tool.";

pub fn evaluate(analysis: &Analysis, policy: &HarnessPolicy) -> Evaluation {
    let mut e = Evaluation::default();
    for effect in &analysis.file_effects {
        match &effect.target {
            Target::Path(p) => {
                if !e.claims.contains(p) {
                    e.claims.push(p.clone());
                }
            }
            Target::Unresolved => e.apply(
                policy.unresolved_writes,
                format!(
                    "{PREFIX} a {} target could not be determined. {UNRESOLVED_FIX}",
                    op_name(effect.op)
                ),
            ),
        }
    }
    for tree in &analysis.tree_effects {
        let scope = (tree.scope.clone(), tree.whole_checkout);
        if !e.scopes.contains(&scope) {
            e.scopes.push(scope);
        }
    }
    for u in &analysis.uncertainties {
        match &u.kind {
            UncertaintyKind::UnresolvedWrite
            | UncertaintyKind::ParseError
            | UncertaintyKind::LimitExhausted(_)
            | UncertaintyKind::UnresolvedInvocation => e.apply(
                policy.unresolved_writes,
                format!("{PREFIX} {}. {UNRESOLVED_FIX}", u.detail),
            ),
            UncertaintyKind::UnsupportedLanguage(language) => e.apply(
                policy.unsupported_language,
                format!(
                    "{PREFIX} {}. devkit cannot see what {language} source writes; make the edit in bash, PowerShell, Python, JavaScript or TypeScript, or with a structured edit tool.",
                    u.detail
                ),
            ),
        }
    }
    for script in &analysis.script_files {
        let name = match &script.script {
            Value::Known(s) => format!("`{s}`"),
            Value::Unknown => "a script".to_string(),
        };
        e.apply(
            policy.script_files,
            format!(
                "{PREFIX} {name} is a stored script, and devkit does not read what it writes. Make the edit inline or with a structured edit tool."
            ),
        );
    }
    e
}

fn op_name(op: FileOp) -> &'static str {
    match op {
        FileOp::Create => "create",
        FileOp::Overwrite => "write",
        FileOp::Append => "append",
        FileOp::Delete => "delete",
        FileOp::Rename => "rename",
        FileOp::Copy => "copy",
    }
}

/// Check every scope, then claim every target. A scope conflict stops before
/// any claim; a claim conflict leaves the claims already made to the normal
/// release lifecycle.
pub fn enforce(evaluation: &Evaluation, holder: &str) -> anyhow::Result<Vec<Conflict>> {
    let mut resolver = devkit_locks::WriteResolver::new();
    let mut conflicts = Vec::new();
    for (scope, whole) in &evaluation.scopes {
        conflicts.extend(resolver.check_scope(scope, *whole, holder)?);
    }
    if !conflicts.is_empty() {
        return Ok(conflicts);
    }
    for path in &evaluation.claims {
        if let WriteDecision::Denied(c) =
            resolver.decide_write(path, holder, Some("shell-harness"), 1800)?
        {
            conflicts.extend(c);
        }
    }
    Ok(conflicts)
}

pub fn conflict_message(conflicts: &[Conflict]) -> String {
    let who = conflicts
        .iter()
        .map(|c| format!("{} (held by {})", c.path, c.held_by))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{PREFIX} {who} is locked by another agent; edit a different file or wait for it to finish"
    )
}

#[derive(Debug)]
pub enum StageError {
    TimedOut,
    Panicked,
}

/// Run `work` on its own thread and wait at most `deadline`. On a timeout the
/// thread is left running; the caller exits the process, which ends it.
pub fn with_deadline<T: Send + 'static>(
    deadline: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, StageError> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    match rx.recv_timeout(deadline) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(StageError::TimedOut),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(StageError::Panicked),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use devkit_command::{Context, Dialect, Limits, PathStyle};
    use devkit_common::harness::HarnessPolicy;
    use devkit_config::PolicyAction;

    use super::*;

    fn eval(command: &str, policy: HarnessPolicy) -> Evaluation {
        let ctx = Context {
            dialect: Dialect::Bash,
            cwd: Some("/repo".into()),
            path_style: PathStyle::Unix,
            limits: Limits::default(),
        };
        evaluate(&devkit_command::analyze(command, &ctx), &policy)
    }

    #[test]
    fn known_targets_are_claims_and_tree_writers_are_scopes() {
        let e = eval(
            "echo x > a.txt; mv b.txt c.txt; cargo fmt",
            HarnessPolicy::default(),
        );
        assert_eq!(e.claims, ["/repo/a.txt", "/repo/b.txt", "/repo/c.txt"]);
        assert_eq!(e.scopes, [("/repo".to_string(), true)]);
        assert!(e.blocks.is_empty());
    }

    #[test]
    fn each_policy_governs_only_its_own_findings() {
        let unresolved = "echo x > \"$OUT\"";
        assert_eq!(eval(unresolved, HarnessPolicy::default()).blocks.len(), 1);
        let warn = HarnessPolicy {
            unresolved_writes: PolicyAction::Warn,
            ..HarnessPolicy::default()
        };
        let e = eval(unresolved, warn);
        assert!(e.blocks.is_empty());
        assert_eq!(e.warnings.len(), 1);
        let allow = HarnessPolicy {
            unresolved_writes: PolicyAction::Allow,
            ..HarnessPolicy::default()
        };
        let e = eval(unresolved, allow);
        assert!(e.blocks.is_empty() && e.warnings.is_empty());

        assert_eq!(
            eval("perl -e 'print 1'", HarnessPolicy::default())
                .blocks
                .len(),
            1
        );
        assert!(
            eval("python3 tools/gen.py", HarnessPolicy::default())
                .blocks
                .is_empty()
        );
        let block_scripts = HarnessPolicy {
            script_files: PolicyAction::Block,
            ..HarnessPolicy::default()
        };
        assert_eq!(eval("python3 tools/gen.py", block_scripts).blocks.len(), 1);
    }

    #[test]
    fn an_allowed_finding_keeps_the_known_targets() {
        let allow = HarnessPolicy {
            unresolved_writes: PolicyAction::Allow,
            ..HarnessPolicy::default()
        };
        let e = eval("echo x > a.txt; echo y > \"$OUT\"", allow);
        assert_eq!(e.claims, ["/repo/a.txt"]);
    }

    #[test]
    fn a_blocking_diagnostic_names_a_correction_and_no_lock_workaround() {
        let e = eval("echo x > \"$OUT\"", HarnessPolicy::default());
        assert!(e.blocks[0].contains("literal"), "{}", e.blocks[0]);
        assert!(!e.blocks[0].contains("lockm acquire"), "{}", e.blocks[0]);
    }

    #[test]
    fn the_deadline_returns_before_slow_work_finishes() {
        let start = Instant::now();
        let r = with_deadline(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_secs(5))
        });
        assert!(matches!(r, Err(StageError::TimedOut)));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(matches!(with_deadline(Duration::from_secs(5), || 7), Ok(7)));
        assert!(matches!(
            with_deadline(Duration::from_secs(5), || -> u8 { panic!("boom") }),
            Err(StageError::Panicked)
        ));
    }
}
