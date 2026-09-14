//! Static analysis of shell commands and the scripts they run: which programs
//! run, which files they write, and what could not be determined.
//!
//! Nothing here reads configuration, touches a registry, reads a file, or runs
//! a process. Consumers decide what a finding means.

mod analyzer;
mod bash;
mod budget;
mod catalog;
mod context;
mod embed;
mod fish;
mod js;
mod model;
mod normalize;
mod paths;
mod powershell;
mod python;
mod ts;

pub use context::{Context, Dialect, Limits, PathStyle};
pub use model::{
    Analysis, FileEffect, FileOp, Invocation, Language, Limit, Location, ScriptFileInvocation,
    Target, TreeEffect, Uncertainty, UncertaintyKind, Value,
};

/// Analyze a command in `ctx.dialect`.
pub fn analyze(source: &str, ctx: &Context) -> Analysis {
    analyzer::Analyzer::new(ctx).run_source(source)
}

/// Analyze an argument vector a caller already holds, such as a configured
/// task's `run`. The words are not joined into a string and reparsed.
pub fn analyze_argv(argv: &[String], ctx: &Context) -> Analysis {
    analyzer::Analyzer::new(ctx).run_argv(argv)
}

/// `(config, project)` from a `doppler run` wrapper's words.
pub fn doppler_flags(words: &[Value]) -> (Option<String>, Option<String>) {
    normalize::doppler_flags(words)
}

#[cfg(test)]
pub(crate) mod testutil {
    use crate::{Analysis, Context, Dialect, Limits, PathStyle, Target};

    pub(crate) fn ctx(dialect: Dialect) -> Context {
        Context {
            dialect,
            cwd: Some("/repo".into()),
            path_style: PathStyle::Unix,
            limits: Limits::default(),
        }
    }

    pub(crate) fn bash(source: &str) -> Analysis {
        crate::analyze(source, &ctx(Dialect::Bash))
    }

    pub(crate) fn targets(a: &Analysis) -> Vec<String> {
        a.file_effects
            .iter()
            .map(|e| match &e.target {
                Target::Path(p) => p.clone(),
                Target::Unresolved => "?".into(),
                Target::Ephemeral { .. } => "<ephemeral>".into(),
            })
            .collect()
    }

    pub(crate) fn programs(a: &Analysis) -> Vec<String> {
        a.invocations
            .iter()
            .map(|i| i.program.known().unwrap_or("?").to_string())
            .collect()
    }
}

#[cfg(test)]
mod limit_tests {
    use crate::{
        Dialect, Limit, Limits, UncertaintyKind,
        testutil::{ctx, targets},
    };

    fn with(limits: Limits, source: &str) -> crate::Analysis {
        let mut c = ctx(Dialect::Bash);
        c.limits = limits;
        crate::analyze(source, &c)
    }

    fn exhaustion_count(a: &crate::Analysis, limit: Limit) -> usize {
        a.uncertainties
            .iter()
            .filter(|u| u.kind == UncertaintyKind::LimitExhausted(limit))
            .count()
    }

    #[test]
    fn an_oversized_command_is_not_parsed() {
        let a = with(
            Limits {
                outer_source: 16,
                ..Limits::default()
            },
            "echo x > a.txt; echo y > b.txt",
        );
        assert_eq!(exhaustion_count(&a, Limit::OuterSource), 1);
        assert!(a.file_effects.is_empty());
    }

    #[test]
    fn cumulative_embedded_source_is_bounded_and_earlier_findings_stay() {
        let a = with(
            Limits {
                cumulative_source: 100,
                ..Limits::default()
            },
            "echo a > a.txt; bash -c 'echo b > b.txt; echo c > c.txt; echo d > d.txt'",
        );
        assert_eq!(exhaustion_count(&a, Limit::CumulativeSource), 1);
        assert_eq!(targets(&a), ["/repo/a.txt"]);
    }

    #[test]
    fn embedded_recursion_stops_at_the_depth_limit_once() {
        let a = with(
            Limits {
                depth: 1,
                ..Limits::default()
            },
            "echo a > a.txt; bash -c 'echo b > b.txt; bash -c \"echo c > c.txt\"'",
        );
        assert_eq!(targets(&a), ["/repo/a.txt", "/repo/b.txt"]);
        assert_eq!(
            a.uncertainties
                .iter()
                .filter(|u| u.kind == UncertaintyKind::LimitExhausted(Limit::Depth))
                .count(),
            1
        );
    }

    #[test]
    fn the_node_budget_stops_the_walk_and_keeps_what_it_found() {
        let source = (0..200)
            .map(|i| format!("echo {i} > f{i}.txt"))
            .collect::<Vec<_>>()
            .join("\n");
        let a = with(
            Limits {
                nodes: 60,
                ..Limits::default()
            },
            &source,
        );
        assert_eq!(exhaustion_count(&a, Limit::Nodes), 1);
        assert!(!a.file_effects.is_empty() && a.file_effects.len() < 200);
    }

    #[test]
    fn an_oversized_value_is_unknown() {
        let big = "x".repeat(100);
        let a = with(
            Limits {
                value: 64,
                ..Limits::default()
            },
            &format!("python3 - <<'PY'\nopen('{big}', 'w')\nPY"),
        );
        assert_eq!(exhaustion_count(&a, Limit::ValueSize), 1);
        assert_eq!(targets(&a), ["?"]);
    }
}
