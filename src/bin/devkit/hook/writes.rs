//! The shell hook's write evaluation: turn an analysis into the claims the
//! write gate takes to the registry, and the policy findings it cannot.

use devkit_command::{Analysis, FileOp, Target, TreeReach, UncertaintyKind, Value};
use devkit_common::harness::HarnessPolicy;
use devkit_config::PolicyAction;

use super::gate::{Claims, PREFIX, ScopeCheck};

#[derive(Debug, Default)]
pub struct Evaluation {
    pub blocks: Vec<String>,
    pub warnings: Vec<String>,
    pub claims: Claims,
}

pub(super) fn file_finding(
    action: PolicyAction,
    message: String,
    blocks: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    match action {
        PolicyAction::Block => blocks.push(message),
        PolicyAction::Warn => warnings.push(message),
        PolicyAction::Allow => {}
    }
}

impl Evaluation {
    fn apply(&mut self, action: PolicyAction, message: String) {
        file_finding(action, message, &mut self.blocks, &mut self.warnings);
    }
}

/// The correction, which differs by what the policy does with the finding: a
/// blocked command has to be rewritten, a warned one runs and leaves the agent
/// to claim what devkit could not name.
pub(super) fn unresolved_fix(action: PolicyAction) -> &'static str {
    match action {
        PolicyAction::Warn => {
            "Claim the destinations with `lockm acquire` before writing, or name a literal path \
             and they are claimed for you."
        }
        PolicyAction::Block | PolicyAction::Allow => {
            "Name a literal path, or a variable assigned a literal earlier in the same command, \
             or make the edit with a structured edit tool; those targets are claimed for you. \
             A glob, or `find` from one directory, below the checkout root claims that \
             directory instead. `lockm acquire` does not lift this block."
        }
    }
}

pub fn evaluate(analysis: &Analysis, policy: &HarnessPolicy) -> Evaluation {
    let mut e = Evaluation::default();
    for effect in &analysis.file_effects {
        // A permission change leaves the contents alone. Claiming its path
        // would hold the file against the session editing it for the rest of
        // this one, so the path is only checked against claims already made.
        let permissions = effect.op == FileOp::Permissions;
        match &effect.target {
            Target::Path(path) if permissions => {
                e.claims.scope(ScopeCheck::Covering { path: path.clone() })
            }
            Target::Within(dir) if permissions => e.claims.scope(ScopeCheck::Tree {
                dir: dir.clone(),
                whole_checkout: false,
            }),
            Target::Ephemeral { .. } if permissions => {}
            Target::Path(p) => e.claims.path(p),
            Target::Within(dir) => e.claims.tree(dir),
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
                    e.claims.scope(ScopeCheck::Fresh {
                        dir: dir.to_string(),
                    });
                }
            }
        }
    }
    for tree in &analysis.tree_effects {
        let dir = tree.scope.clone();
        let whole_checkout = tree.whole_checkout;
        e.claims.scope(match tree.reach {
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
            Value::Unknown | Value::Ephemeral(_) | Value::Within(_) => "a script".to_string(),
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
        FileOp::Permissions => "permission change",
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(e.claims.paths, [
            "/repo/a.txt",
            "/repo/b.txt",
            "/repo/c.txt"
        ]);
        assert_eq!(e.claims.scopes, [ScopeCheck::Tree {
            dir: "/repo".to_string(),
            whole_checkout: true,
        }]);
        assert!(e.blocks.is_empty());
    }

    #[test]
    fn a_permission_change_is_a_scope_check_and_never_a_claim() {
        let e = eval(
            "chmod +x run.sh; chmod -R 755 bin; chown me src/*.rs; T=$(mktemp); chmod 600 \"$T\"",
            HarnessPolicy::default(),
        );
        assert!(e.claims.paths.is_empty(), "{:?}", e.claims.paths);
        assert!(e.claims.trees.is_empty(), "{:?}", e.claims.trees);
        assert!(e.blocks.is_empty(), "{:?}", e.blocks);
        assert_eq!(e.claims.scopes, [
            ScopeCheck::Covering {
                path: "/repo/run.sh".to_string(),
            },
            ScopeCheck::Tree {
                dir: "/repo/src".to_string(),
                whole_checkout: false,
            },
            ScopeCheck::Tree {
                dir: "/repo/bin".to_string(),
                whole_checkout: false,
            },
        ]);

        let e = eval(
            "python3 -c \"import os; os.fchmod(3, 0o755)\"",
            HarnessPolicy::default(),
        );
        assert!(e.blocks[0].contains("permission change"), "{}", e.blocks[0]);
    }

    #[test]
    fn a_bounded_write_claims_its_outermost_directory_once() {
        let e = eval(
            "rm -f src/gen/*.rs; sed -i s/a/b/ src/*.rs; rm -f src/*.o docs/*.md",
            HarnessPolicy::default(),
        );
        assert_eq!(e.claims.trees, ["/repo/src", "/repo/docs"]);
        assert!(e.claims.paths.is_empty(), "{:?}", e.claims.paths);
        assert!(e.blocks.is_empty(), "{:?}", e.blocks);
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
        assert_eq!(e.claims.paths, ["/repo/a.txt"]);
    }

    #[test]
    fn a_tempfile_write_claims_nothing_and_blocks_nothing() {
        let e = eval(
            "python3 -c \"import tempfile, os; d = tempfile.mkdtemp(); open(os.path.join(d, 'out.txt'), 'w').write('x')\"",
            HarnessPolicy::default(),
        );
        assert!(e.claims.paths.is_empty(), "{:?}", e.claims.paths);
        assert!(e.blocks.is_empty(), "{:?}", e.blocks);
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    }

    #[test]
    fn an_mktemp_write_claims_nothing_and_blocks_nothing() {
        let e = eval("T=$(mktemp); echo x > \"$T\"", HarnessPolicy::default());
        assert!(e.claims.paths.is_empty(), "{:?}", e.claims.paths);
        assert!(e.blocks.is_empty(), "{:?}", e.blocks);
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
        assert!(e.claims.scopes.is_empty(), "{:?}", e.claims.scopes);
    }

    #[test]
    fn a_named_temp_directory_is_checked_for_a_covering_claim() {
        let e = eval(
            "T=$(mktemp -p sub); echo x > \"$T\"",
            HarnessPolicy::default(),
        );
        assert!(e.claims.paths.is_empty(), "{:?}", e.claims.paths);
        assert_eq!(e.claims.scopes, [ScopeCheck::Fresh {
            dir: "/repo/sub".to_string(),
        }]);

        let e = eval(
            "python3 -c \"import tempfile; f = tempfile.NamedTemporaryFile(dir='.'); f.write(b'x')\"",
            HarnessPolicy::default(),
        );
        assert_eq!(e.claims.scopes, [ScopeCheck::Fresh {
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
}
