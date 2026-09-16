//! The shell hook's write stage: turn an analysis into claims, scope checks
//! and policy findings, and claim through the same registry path structured
//! edits use.

#![allow(dead_code)]

use std::{sync::mpsc, time::Duration};

use devkit_command::{Analysis, FileOp, Target, TreeReach, UncertaintyKind, Value};
use devkit_common::{git::Checkout, harness::HarnessPolicy};
use devkit_config::PolicyAction;
use devkit_locks::model::{Conflict, WriteDecision};

const PREFIX: &str = "devkit write-harness:";

#[derive(Debug, Default)]
pub struct Evaluation {
    pub blocks: Vec<String>,
    pub warnings: Vec<String>,
    /// Absolute write targets, in order, without repeats.
    pub claims: Vec<String>,
    /// Directories to check without claiming, in order, without repeats.
    pub scopes: Vec<ScopeCheck>,
}

/// A directory the registry is asked about, and which question to ask of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeCheck {
    /// A writer rewrites an unenumerated set of files under this directory, so
    /// any claim overlapping it conflicts.
    Tree { dir: String, whole_checkout: bool },
    /// A name was created fresh under this directory. Nobody else can produce
    /// that name, so only a claim covering the directory's children conflicts.
    Fresh { dir: String },
}

impl Evaluation {
    pub fn needs_registry(&self) -> bool {
        !self.claims.is_empty() || !self.scopes.is_empty()
    }

    fn scope(&mut self, check: ScopeCheck) {
        if !self.scopes.contains(&check) {
            self.scopes.push(check);
        }
    }

    fn apply(&mut self, action: PolicyAction, message: String) {
        match action {
            PolicyAction::Block => self.blocks.push(message),
            PolicyAction::Warn => self.warnings.push(message),
            PolicyAction::Allow => {}
        }
    }
}

/// The correction, which differs by what the policy does with the finding: a
/// blocked command has to be rewritten, a warned one runs and leaves the agent
/// to claim what devkit could not name.
fn unresolved_fix(action: PolicyAction) -> &'static str {
    match action {
        PolicyAction::Warn => {
            "Claim the destinations with `lockm acquire` before writing, or name a literal path \
             and they are claimed for you."
        }
        PolicyAction::Block | PolicyAction::Allow => {
            "Name a literal path, or a variable assigned a literal earlier in the same command, \
             or make the edit with a structured edit tool; those targets are claimed for you. \
             `lockm acquire` does not lift this block."
        }
    }
}

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
                    "{PREFIX} a {} target could not be determined. {}",
                    op_name(effect.op),
                    unresolved_fix(policy.unresolved_writes)
                ),
            ),
            // A path an API created fresh under a random name is uncontendable:
            // no other session holds it, and none can produce it. Claiming it
            // would write a row nobody could ever conflict with. The directory
            // it was created in is another matter, since a claim there covers
            // every path born under it, so that is checked like any other scope.
            Target::Ephemeral { at } => {
                if let Some(dir) = at.named() {
                    e.scope(ScopeCheck::Fresh {
                        dir: dir.to_string(),
                    });
                }
            }
        }
    }
    for tree in &analysis.tree_effects {
        let dir = tree.scope.clone();
        let whole_checkout = tree.whole_checkout;
        e.scope(match tree.reach {
            TreeReach::All => ScopeCheck::Tree {
                dir,
                whole_checkout,
            },
            TreeReach::FreshSubtree => ScopeCheck::Fresh { dir },
        });
    }
    for u in &analysis.uncertainties {
        match &u.kind {
            UncertaintyKind::UnresolvedWrite
            | UncertaintyKind::ParseError
            | UncertaintyKind::LimitExhausted(_)
            | UncertaintyKind::UnresolvedInvocation => e.apply(
                policy.unresolved_writes,
                format!(
                    "{PREFIX} {}. {}",
                    u.detail,
                    unresolved_fix(policy.unresolved_writes)
                ),
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
            Value::Unknown | Value::Ephemeral(_) => "a script".to_string(),
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
pub fn enforce(
    evaluation: &Evaluation,
    holder: &str,
    checkout: Checkout,
) -> anyhow::Result<Vec<Conflict>> {
    let mut resolver = devkit_locks::WriteResolver::with_checkout(checkout);
    let mut conflicts = Vec::new();
    for check in &evaluation.scopes {
        conflicts.extend(match check {
            ScopeCheck::Tree {
                dir,
                whole_checkout,
            } => resolver.check_scope(dir, *whole_checkout, holder)?,
            ScopeCheck::Fresh { dir } => resolver.check_covering(dir, holder)?,
        });
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
        assert_eq!(e.scopes, [ScopeCheck::Tree {
            dir: "/repo".to_string(),
            whole_checkout: true,
        }]);
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
    fn a_tempfile_write_claims_nothing_and_blocks_nothing() {
        let e = eval(
            "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(); open(os.path.join(d, 'out.txt'), 'w').write('x')\"",
            HarnessPolicy::default(),
        );
        assert!(e.claims.is_empty(), "{:?}", e.claims);
        assert!(e.blocks.is_empty(), "{:?}", e.blocks);
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    }

    #[test]
    fn an_mktemp_write_claims_nothing_and_blocks_nothing() {
        let e = eval("T=$(mktemp); echo x > \"$T\"", HarnessPolicy::default());
        assert!(e.claims.is_empty(), "{:?}", e.claims);
        assert!(e.blocks.is_empty(), "{:?}", e.blocks);
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
        assert!(e.scopes.is_empty(), "{:?}", e.scopes);
    }

    #[test]
    fn a_named_temp_directory_is_checked_for_a_covering_claim() {
        let e = eval(
            "T=$(mktemp -p sub); echo x > \"$T\"",
            HarnessPolicy::default(),
        );
        assert!(e.claims.is_empty(), "{:?}", e.claims);
        assert_eq!(e.scopes, [ScopeCheck::Fresh {
            dir: "/repo/sub".to_string(),
        }]);

        let e = eval(
            "python3 -c \"import tempfile; f = tempfile.NamedTemporaryFile(dir='.'); f.write(b'x')\"",
            HarnessPolicy::default(),
        );
        assert_eq!(e.scopes, [ScopeCheck::Fresh {
            dir: "/repo".to_string(),
        }]);
    }

    #[test]
    fn a_block_names_a_rewrite_and_a_warning_names_a_claim() {
        let e = eval("echo x > \"$OUT\"", HarnessPolicy::default());
        assert!(e.blocks[0].contains("literal path"), "{}", e.blocks[0]);
        assert!(
            e.blocks[0].contains("does not lift this block"),
            "{}",
            e.blocks[0]
        );

        let warn = HarnessPolicy {
            unresolved_writes: PolicyAction::Warn,
            ..HarnessPolicy::default()
        };
        let e = eval("echo x > \"$OUT\"", warn);
        assert!(e.warnings[0].contains("lockm acquire"), "{}", e.warnings[0]);
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
