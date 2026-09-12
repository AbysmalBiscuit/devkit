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
