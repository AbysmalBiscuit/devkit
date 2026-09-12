# Shared command analysis implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce file locks for writes made through shell tools by analyzing commands and embedded scripts with Tree-sitter, and move the command guard onto the same analysis with per-rule actions and severity.

**Architecture:** A new `devkit-command` crate parses a command (bash, PowerShell, fish) and the scripts it runs (Python, JavaScript, TypeScript, nested shells) into one `Analysis`: invocations, file effects, tree effects, script-file invocations, and uncertainties. It never reads config, touches a registry, or runs anything. `devkit harness shell` resolves the payload, the dialect, and the `[harness]` policy, analyzes once, hands the result to the command guard in `devkit-ports::guard` and to a write stage that claims known targets through `devkit_locks::WriteResolver` under a 5-second deadline, and emits one deny, warning, or silent allow.

**Tech Stack:** Rust 2024, `tree-sitter` 0.27 with `tree-sitter-bash`, `tree-sitter-powershell` (airbus-cert), `tree-sitter-python`, `tree-sitter-javascript`, `tree-sitter-typescript`, `tree-sitter-fish`; `serde`/`toml`/`schemars` for config; `fd-lock` and `tempfile` in tests.

**Spec:** `docs/superpowers/specs/2026-09-11-command-analysis-design.md`

## Global constraints

- `cargo nextest run --workspace --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace --doc` pass at every commit. Format with `devrun task fmt` (nightly rustfmt; stable rustfmt silently formats to the wrong options).
- CI builds and tests on ubuntu, macos, and windows. Every grammar crate compiles C through `cc`; a task that adds one is not done until a Windows build of it has been seen.
- `devkit-command` depends on no devkit crate: not `devkit-config`, `devkit-common`, `devkit-locks`, or `devkit-ports`. It reads no files, spawns nothing, and creates no thread pool.
- `devkit-config` stays a leaf. New config types live there with `schemars::JsonSchema` derives and no internal library dependency.
- `devkit-common` does not depend on `devkit-command`. Dialect resolution and the write stage live in the `devkit` binary (`src/bin/devkit/harness/`), which depends on both.
- Analysis limits: 256 KiB outer source, 1 MiB cumulative decoded source, nesting depth 8, 100,000 visited syntax nodes, 64 KiB per resolved value. Exhaustion is an uncertainty, never a panic, and never discards findings already made.
- The write stage deadline is 5 seconds; the manifest timeout for `devkit harness shell` is 30 seconds on Claude Code and Codex.
- Known write targets are always claimed or conflict-checked when `enforce_writes` is on, whatever `unresolved_writes`, `unsupported_language`, or `script_files` say. `allow` and `warn` suppress only their own finding.
- The command-guard half fails open. The write half fails closed: a panic, an unusable payload, a registry error, or a deadline miss while the write stage is active is a denial.
- Cursor keeps the command guard only. Its payload is never routed to the write stage.
- Test fixtures are synthetic. No captured command text, path, repository name, or file contents from `data_analysis.local` enters the repository. No recorded command is ever executed.
- Test scratch comes from `tempfile`; bind the `TempDir` for as long as its path is used.
- Markdown in this repo is not hand-wrapped. Comments are timeless: no issue references, no `now`/`used to`, no RED/GREEN narration.
- No `_ =>` arm over a devkit enum this plan adds (`Language`, `Dialect`, `FileOp`, `UncertaintyKind`, `PolicyAction`, `RuleAction`, `Severity`, `ShellSetting`, `Harness`). Match exhaustively.

## Decisions this plan makes inside the spec

These fill gaps the spec leaves to implementation. Each is tested where it is introduced.

1. **Directory creation is not a file effect.** `mkdir`, `New-Item -ItemType Directory`, `os.makedirs`, `Path.mkdir`, and `fs.mkdirSync` produce nothing. The lock registry treats a directory row as covering everything under it, so claiming `src` for `mkdir -p src` would lock the whole subtree for the claim's 30-minute lease. The spec lists `mkdir` among catalog commands; recognizing it as known and effect-free satisfies "a known harmless operation" without that cost.
2. **Recursive deletion and recursive copy are tree effects.** `rm -r`, `Remove-Item -Recurse`, `shutil.rmtree`, `shutil.copytree`, `fs.rmSync(p, {recursive: true})`, and `cp -r` name a directory whose file set is unknown, so they take the conflict check the spec defines for whole-tree writers.
3. **A parse error is an uncertainty only when its region could write.** An `ERROR` or `MISSING` region yields `UnresolvedWrite` when it contains a redirect operator, a program word in the effect catalog, an interpreter, or no identifiable program word. A region whose program word is identified and is outside the catalog yields nothing. Without this, the spec's statement-scoped recovery still blocks read-only PowerShell such as `Get-ChildItem | Format-Table Mode, Name -AutoSize`.
4. **"Unmodeled calls that may write" means calls that can reach the filesystem.** In Python and JavaScript an uncertainty comes from: a call into a module outside the adapter's module table, a dynamic callee (`eval`, `exec`, `getattr` with an unknown name, `new Function`, dynamic `import()`), a write-named method on a receiver of unknown type, or a subprocess call whose command is not constant. Builtins and table modules marked read-only produce nothing; calls on values the adapter knows are plain data produce nothing.
5. **`perl -i` and `sed -i` claim their file operands and report no unsupported language.** Plain `perl -e`, `ruby -e`, `awk`, and `nu -c` follow `unsupported_language`.
6. **Shell functions defined in the command are analyzed at their call sites**, in a copy of the caller's scope, at depth + 1. A definition alone produces nothing.
7. **A literal list iterated by `for` is analyzed once per element** when it has at most 32 elements, in Python, bash, and PowerShell. Any other loop variable is unknown.
8. **Unknown program words are write uncertainties.** `$cmd file` or `& $exe` could be anything, so the invocation carries `UnresolvedInvocation`, which the write stage maps to `unresolved_writes` and the guard maps to a possible-match warning.

## File structure

```
crates/devkit-command/
  Cargo.toml
  src/lib.rs          public entry points: analyze, analyze_argv; re-exports
  src/model.rs        Analysis and everything it holds
  src/context.rs      Context, Dialect, PathStyle, Limits
  src/budget.rs       limit accounting shared by every adapter
  src/paths.rs        target resolution against a cwd, per path style
  src/ts.rs           parser construction and node helpers
  src/analyzer.rs     the invocation pipeline every adapter feeds
  src/normalize.rs    wrappers, runners, git global options
  src/embed.rs        which invocations carry source, and its arguments
  src/catalog.rs      program + argument form -> effect
  src/bash.rs         bash adapter
  src/fish.rs         fish adapter
  src/powershell.rs   PowerShell adapter and cmdlet/.NET catalog
  src/python.rs       Python adapter
  src/js.rs           JavaScript and TypeScript adapter
  examples/corpus_probe.rs   local-only measurement over a corpus snapshot
crates/devkit-config/src/harness.rs     new policy keys and rule fields
crates/devkit-common/src/harness.rs     payload identity, policy merge, warning envelope
crates/devkit-locks/src/lib.rs          WriteResolver::check_scope
crates/devkit-ports/src/guard/          consumes Analysis; lex.rs deleted, norm.rs trimmed
src/bin/devkit/harness.rs -> src/bin/devkit/harness/mod.rs
src/bin/devkit/harness/dialect.rs
src/bin/devkit/harness/writes.rs
tests/harness_shell_writes.rs
hooks/hooks.json, hooks/hooks-codex.json
schema/devkit-config.json, docs/configuration.md, docs/commands.md, skills/using-devkit/SKILL.md, skills/using-devkit/references/locks.md, AGENTS.md
```

---

### Task 1: Scaffold `devkit-command` with the result model and grammars

**Files:**
- Create: `crates/devkit-command/Cargo.toml`, `crates/devkit-command/src/lib.rs`, `src/model.rs`, `src/context.rs`, `src/budget.rs`, `src/ts.rs`
- Modify: `Cargo.toml` (workspace members, `[workspace.dependencies]`)
- Modify: `release-please-config.json` if it lists per-crate `Cargo.toml` files

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `devkit_command::{Analysis, Invocation, FileEffect, FileOp, Target, TreeEffect, ScriptFileInvocation, Uncertainty, UncertaintyKind, Limit, Location, Value, Language}`
  - `devkit_command::{Context, Dialect, PathStyle, Limits}`
  - `devkit_command::analyze(source: &str, ctx: &Context) -> Analysis`
  - `devkit_command::analyze_argv(argv: &[String], ctx: &Context) -> Analysis`
  - crate-private `ts::parse(Language, &str) -> Option<tree_sitter::Tree>`, `budget::Budget`

- [ ] **Step 1: Add the workspace entries**

In the root `Cargo.toml`, add `"crates/devkit-command"` to `members`, and to `[workspace.dependencies]`:

```toml
devkit-command = { path = "crates/devkit-command" }
tree-sitter = "0.27"
tree-sitter-bash = "0.25"
tree-sitter-powershell = "0.26"
tree-sitter-python = "0.25"
tree-sitter-javascript = "0.25"
tree-sitter-typescript = "0.23"
tree-sitter-fish = "3.6"
```

Create `crates/devkit-command/Cargo.toml`:

```toml
[package]
name = "devkit-command"
edition.workspace = true
version = "0.14.1" # x-release-please-version

[dependencies]
tree-sitter.workspace = true
tree-sitter-bash.workspace = true
tree-sitter-powershell.workspace = true
tree-sitter-python.workspace = true
tree-sitter-javascript.workspace = true
tree-sitter-typescript.workspace = true
tree-sitter-fish.workspace = true

[dev-dependencies]
serde_json.workspace = true
```

Run `rg -n "devkit-locks/Cargo.toml" release-please-config.json`. If it matches, add `crates/devkit-command/Cargo.toml` beside it in the same list.

- [ ] **Step 2: Write the failing grammar test**

`crates/devkit-command/src/ts.rs`:

```rust
//! Parser construction and the node helpers every adapter shares.

use tree_sitter::{Node, Parser, Tree};

use crate::model::Language;

/// A syntax tree for `source`, or `None` when the parser refused to produce
/// one. A tree containing `ERROR` or `MISSING` nodes is still returned: the
/// adapters recover per statement.
pub(crate) fn parse(language: Language, source: &str) -> Option<Tree> {
    let grammar: tree_sitter::Language = match language {
        Language::Bash => tree_sitter_bash::LANGUAGE.into(),
        Language::PowerShell => tree_sitter_powershell::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Fish => tree_sitter_fish::LANGUAGE.into(),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar).ok()?;
    parser.parse(source, None)
}

pub(crate) fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    &source[node.byte_range()]
}

pub(crate) fn is_broken(node: Node<'_>) -> bool {
    node.is_error() || node.is_missing()
}

pub(crate) fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_grammar_parses_a_trivial_program() {
        for (language, source) in [
            (Language::Bash, "echo hi > out.txt"),
            (Language::PowerShell, "Set-Content -Path out.txt -Value hi"),
            (Language::Python, "open('out.txt', 'w').write('hi')"),
            (Language::JavaScript, "require('fs').writeFileSync('out.txt', 'hi')"),
            (Language::TypeScript, "const p: string = 'out.txt'"),
            (Language::Fish, "echo hi > out.txt"),
        ] {
            let tree = parse(language, source).expect("a tree");
            assert!(
                !tree.root_node().has_error(),
                "{language:?}: {}",
                tree.root_node().to_sexp()
            );
        }
    }
}
```

- [ ] **Step 3: Run it to see it fail**

Run: `cargo nextest run -p devkit-command`
Expected: compile failure, because `model` does not exist yet.

If instead a grammar symbol does not resolve (`tree_sitter_fish::LANGUAGE` or `tree_sitter_powershell::LANGUAGE`), find the crate's bindings with `fd -g '**/tree-sitter-fish-*/bindings/rust/lib.rs' ~/.cargo/registry/src` and use the exported `LanguageFn` constant it names. If a grammar's `tree-sitter-language` requirement conflicts with `tree-sitter` 0.27, pick the newest `tree-sitter` minor all six accept and record it in the workspace entry.

- [ ] **Step 4: Write the model**

`crates/devkit-command/src/model.rs`:

```rust
//! The analysis result every consumer reads.

use std::ops::Range;

/// Where a finding came from: a byte range in the command the hook received,
/// and, for a finding inside embedded source, the range inside that decoded
/// source. The outer range of an embedded finding is its enclosing
/// invocation's, so a diagnostic never points into the middle of a quoted
/// argument as if it were outer syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub outer: Range<usize>,
    pub embedded: Option<Range<usize>>,
}

/// A word or expression value after static resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Known(String),
    Unknown,
}

impl Value {
    pub fn known(&self) -> Option<&str> {
        match self {
            Value::Known(s) => Some(s),
            Value::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Bash,
    PowerShell,
    Python,
    JavaScript,
    TypeScript,
    Fish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// The program word as typed, not reduced to a basename.
    pub program: Value,
    /// Every argument, including options that change execution context.
    pub args: Vec<Value>,
    /// `args` with the program's own global options removed (`git -C dir`,
    /// `git -c k=v`). Command rules match against this.
    pub semantic_args: Vec<Value>,
    /// Wrappers removed to reach this invocation, outermost first, each as the
    /// words it consumed.
    pub wrappers: Vec<Vec<Value>>,
    /// The words as they appear in the command, quotes removed where the value
    /// is known. Diagnostics quote these.
    pub typed: Vec<String>,
    pub cwd: Option<String>,
    pub language: Language,
    pub depth: usize,
    pub location: Location,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOp {
    Create,
    Overwrite,
    Append,
    Delete,
    Rename,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Path(String),
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEffect {
    pub op: FileOp,
    pub target: Target,
    pub location: Location,
}

/// A write to an unenumerated set of files under `scope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEffect {
    pub scope: String,
    /// The writer rewrites the whole checkout containing `scope`, not only
    /// the subtree: `git checkout` run from `src/` still rewrites the root.
    pub whole_checkout: bool,
    /// What rewrites the tree, for the diagnostic: `git checkout`, `cargo fmt`.
    pub by: String,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptFileInvocation {
    pub interpreter: Option<String>,
    pub script: Value,
    pub location: Location,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    OuterSource,
    CumulativeSource,
    Depth,
    Nodes,
    ValueSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UncertaintyKind {
    /// A write, or code that may write, whose effect or target is unknown.
    UnresolvedWrite,
    /// Executable source in a language with no adapter.
    UnsupportedLanguage(String),
    /// A statement the parser could not read that could write.
    ParseError,
    LimitExhausted(Limit),
    /// A program word that could not be resolved.
    UnresolvedInvocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uncertainty {
    pub kind: UncertaintyKind,
    pub detail: String,
    pub location: Location,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    pub invocations: Vec<Invocation>,
    pub file_effects: Vec<FileEffect>,
    pub tree_effects: Vec<TreeEffect>,
    pub script_files: Vec<ScriptFileInvocation>,
    pub uncertainties: Vec<Uncertainty>,
}
```

`crates/devkit-command/src/context.rs`:

```rust
//! What the caller establishes about where and how the source runs.

/// The shell that runs the outer command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Bash,
    PowerShell,
    Fish,
}

/// Path rules for the machine the command runs on. Windows accepts drive
/// letters, UNC paths and both separators, and reads a Git Bash `/c/...` path
/// as `C:/...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStyle {
    Unix,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub outer_source: usize,
    pub cumulative_source: usize,
    pub depth: usize,
    pub nodes: usize,
    pub value: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            outer_source: 256 * 1024,
            cumulative_source: 1024 * 1024,
            depth: 8,
            nodes: 100_000,
            value: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub dialect: Dialect,
    /// The requested working directory. `None` leaves relative targets
    /// unresolved.
    pub cwd: Option<String>,
    pub path_style: PathStyle,
    pub limits: Limits,
}
```

`crates/devkit-command/src/budget.rs`:

```rust
//! Limit accounting shared across one analysis, nested sources included.

use crate::{context::Limits, model::Limit};

#[derive(Debug)]
pub(crate) struct Budget {
    limits: Limits,
    nodes: usize,
    source: usize,
}

impl Budget {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            limits,
            nodes: 0,
            source: 0,
        }
    }

    pub(crate) fn limits(&self) -> Limits {
        self.limits
    }

    /// Count one visited node. `Err` once the node limit is spent.
    pub(crate) fn visit(&mut self) -> Result<(), Limit> {
        self.nodes += 1;
        if self.nodes > self.limits.nodes {
            Err(Limit::Nodes)
        } else {
            Ok(())
        }
    }

    /// Admit `len` bytes of source to be parsed at `depth`.
    pub(crate) fn admit(&mut self, len: usize, depth: usize) -> Result<(), Limit> {
        if depth == 0 && len > self.limits.outer_source {
            return Err(Limit::OuterSource);
        }
        if depth > self.limits.depth {
            return Err(Limit::Depth);
        }
        if self.source + len > self.limits.cumulative_source {
            return Err(Limit::CumulativeSource);
        }
        self.source += len;
        Ok(())
    }

    pub(crate) fn value_fits(&self, len: usize) -> bool {
        len <= self.limits.value
    }
}
```

`crates/devkit-command/src/lib.rs`:

```rust
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
```

For this task only, create each remaining module (`analyzer`, `bash`, `catalog`, `embed`, `fish`, `js`, `normalize`, `paths`, `powershell`, `python`) as an empty file, and give `analyzer.rs` the stub below. Tasks 2 to 10 replace them.

```rust
//! The invocation pipeline every adapter feeds.

use crate::{budget::Budget, context::Context, model::Analysis};

pub(crate) struct Analyzer<'c> {
    pub(crate) ctx: &'c Context,
    pub(crate) budget: Budget,
    pub(crate) out: Analysis,
}

impl<'c> Analyzer<'c> {
    pub(crate) fn new(ctx: &'c Context) -> Self {
        Self {
            ctx,
            budget: Budget::new(ctx.limits),
            out: Analysis::default(),
        }
    }

    pub(crate) fn run_source(self, _source: &str) -> Analysis {
        self.out
    }

    pub(crate) fn run_argv(self, _argv: &[String]) -> Analysis {
        self.out
    }
}
```

- [ ] **Step 5: Run the test and the Windows build**

Run: `cargo nextest run -p devkit-command`
Expected: `every_grammar_parses_a_trivial_program` passes.

Run: `cargo clippy -p devkit-command --all-targets -- -D warnings`
Expected: no warnings. Dead-code warnings on the stub modules are expected to be absent because every item is `pub(crate)` and reached from `lib.rs`; if clippy flags one, add the use in the stub rather than an `allow`.

Push the branch and confirm the `test` job's windows leg compiles `devkit-command` before starting Task 2. A grammar whose C does not build under MSVC is a Task 1 problem, not a later one.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/devkit-command release-please-config.json
git commit -m "feat(command): add the analysis crate and result model"
```

---

### Task 2: Path resolution and the invocation pipeline

**Files:**
- Modify: `crates/devkit-command/src/paths.rs`, `crates/devkit-command/src/analyzer.rs`

**Interfaces:**
- Consumes: Task 1 model, `Budget`.
- Produces:
  - `paths::resolve(value: &Value, cwd: Option<&str>, style: PathStyle) -> Target`
  - `paths::join(base: &str, rel: &str, style: PathStyle) -> String`
  - `paths::is_absolute(p: &str, style: PathStyle) -> bool`
  - `analyzer::Word { value: Value, typed: String, span: Range<usize> }`
  - `analyzer::Stdin` enum: `None`, `Source { value: Value, span: Range<usize> }`
  - `analyzer::RawInvocation { words: Vec<Word>, stdin: Stdin, cwd: Option<String>, language: Language, location: Location }`
  - `analyzer::Frame { depth: usize, script_args: Vec<Value>, base: Location }`
  - `Analyzer::invocation(&mut self, raw: RawInvocation, frame: &Frame)`, `Analyzer::file_effect(op, &Value, cwd, location)`, `Analyzer::uncertain(kind, detail, location)`, `Analyzer::source(language, text: &str, frame: Frame)`

- [ ] **Step 1: Write the failing path tests**

`crates/devkit-command/src/paths.rs`:

```rust
//! Resolve a statically known path against the execution directory, using the
//! rules of the machine the command runs on rather than the analyzer's own.

use crate::{
    context::PathStyle,
    model::{Target, Value},
};

pub(crate) fn is_absolute(p: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Unix => p.starts_with('/'),
        PathStyle::Windows => {
            let b = p.as_bytes();
            p.starts_with(['/', '\\'])
                || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'/' | b'\\'))
        }
    }
}

/// A Git Bash drive path (`/c/Users`) in its Windows spelling (`C:/Users`).
fn from_msys(p: &str) -> Option<String> {
    let b = p.as_bytes();
    (b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/')
        .then(|| format!("{}:{}", (b[1] as char).to_ascii_uppercase(), &p[2..]))
}

/// Join with `/`, which both path styles accept, trimming only the separators
/// the style treats as separators.
pub(crate) fn join(base: &str, rel: &str, style: PathStyle) -> String {
    let base = match style {
        PathStyle::Unix => base.trim_end_matches('/'),
        PathStyle::Windows => base.trim_end_matches(['/', '\\']),
    };
    let rel = rel.strip_prefix("./").unwrap_or(rel);
    if rel == "." || rel.is_empty() {
        return base.to_string();
    }
    format!("{base}/{rel}")
}

pub(crate) fn resolve(value: &Value, cwd: Option<&str>, style: PathStyle) -> Target {
    let Some(p) = value.known() else {
        return Target::Unresolved;
    };
    if p.is_empty() || p.starts_with('~') || p.contains(['*', '?', '[']) {
        return Target::Unresolved;
    }
    if style == PathStyle::Windows
        && let Some(win) = from_msys(p)
    {
        return Target::Path(win);
    }
    if is_absolute(p, style) {
        return Target::Path(p.to_string());
    }
    match cwd {
        Some(dir) => Target::Path(join(dir, p, style)),
        None => Target::Unresolved,
    }
}

/// The directory part of a known path, for a rename whose new name is given
/// relative to the old one.
pub(crate) fn parent(p: &str) -> Option<&str> {
    p.rfind(['/', '\\']).map(|i| &p[..i]).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Value {
        Value::Known(s.into())
    }

    #[test]
    fn a_relative_path_joins_the_cwd() {
        assert_eq!(
            resolve(&k("src/a.rs"), Some("/repo"), PathStyle::Unix),
            Target::Path("/repo/src/a.rs".into())
        );
        assert_eq!(
            resolve(&k("./a.rs"), Some("/repo/"), PathStyle::Unix),
            Target::Path("/repo/a.rs".into())
        );
    }

    #[test]
    fn a_relative_path_without_a_cwd_is_unresolved() {
        assert_eq!(resolve(&k("a.rs"), None, PathStyle::Unix), Target::Unresolved);
    }

    #[test]
    fn an_absolute_path_needs_no_cwd() {
        assert_eq!(resolve(&k("/etc/x"), None, PathStyle::Unix), Target::Path("/etc/x".into()));
        assert_eq!(
            resolve(&k(r"C:\repo\a.txt"), None, PathStyle::Windows),
            Target::Path(r"C:\repo\a.txt".into())
        );
    }

    #[test]
    fn windows_rules_do_not_apply_on_unix() {
        assert_eq!(
            resolve(&k(r"C:\repo\a.txt"), Some("/home/u"), PathStyle::Unix),
            Target::Path(r"/home/u/C:\repo\a.txt".into())
        );
    }

    #[test]
    fn a_git_bash_drive_path_reads_as_a_windows_path() {
        assert_eq!(
            resolve(&k("/c/repo/a.txt"), None, PathStyle::Windows),
            Target::Path("C:/repo/a.txt".into())
        );
    }

    #[test]
    fn globs_tildes_and_unknowns_are_unresolved() {
        for v in [k("src/*.rs"), k("~/x"), k(""), Value::Unknown] {
            assert_eq!(resolve(&v, Some("/repo"), PathStyle::Unix), Target::Unresolved, "{v:?}");
        }
    }
}
```

- [ ] **Step 2: Run to see the tests pass or fail for the right reason**

Run: `cargo nextest run -p devkit-command paths`
Expected: PASS (the implementation above is minimal; if `windows_rules_do_not_apply_on_unix` fails, `is_absolute` is reading the host's rules).

- [ ] **Step 3: Write the pipeline test**

Add to `crates/devkit-command/src/analyzer.rs` a test that feeds a hand-built `RawInvocation` so the pipeline is tested before any adapter exists:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::{Dialect, Limits, PathStyle},
        model::{FileOp, Language, Location, Target, UncertaintyKind, Value},
    };

    pub(crate) fn ctx() -> Context {
        Context {
            dialect: Dialect::Bash,
            cwd: Some("/repo".into()),
            path_style: PathStyle::Unix,
            limits: Limits::default(),
        }
    }

    fn word(s: &str) -> Word {
        Word {
            value: Value::Known(s.into()),
            typed: s.into(),
            span: 0..s.len(),
        }
    }

    fn at() -> Location {
        Location { outer: 0..1, embedded: None }
    }

    #[test]
    fn an_invocation_is_recorded_with_its_cwd() {
        let c = ctx();
        let mut a = Analyzer::new(&c);
        a.invocation(
            RawInvocation {
                words: vec![word("ls"), word("-la")],
                stdin: Stdin::None,
                cwd: Some("/repo".into()),
                language: Language::Bash,
                location: at(),
            },
            &Frame::outer(),
        );
        let inv = &a.out.invocations[0];
        assert_eq!(inv.program, Value::Known("ls".into()));
        assert_eq!(inv.args, vec![Value::Known("-la".into())]);
        assert_eq!(inv.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn an_unknown_program_word_is_an_unresolved_invocation() {
        let c = ctx();
        let mut a = Analyzer::new(&c);
        a.invocation(
            RawInvocation {
                words: vec![Word { value: Value::Unknown, typed: "$cmd".into(), span: 0..4 }],
                stdin: Stdin::None,
                cwd: None,
                language: Language::Bash,
                location: at(),
            },
            &Frame::outer(),
        );
        assert_eq!(a.out.uncertainties[0].kind, UncertaintyKind::UnresolvedInvocation);
    }

    #[test]
    fn a_file_effect_resolves_against_the_given_cwd() {
        let c = ctx();
        let mut a = Analyzer::new(&c);
        a.file_effect(FileOp::Overwrite, &Value::Known("out.txt".into()), Some("/repo/sub"), at());
        assert_eq!(a.out.file_effects[0].target, Target::Path("/repo/sub/out.txt".into()));
    }
}
```

- [ ] **Step 4: Run to see it fail**

Run: `cargo nextest run -p devkit-command analyzer`
Expected: compile failure on `Word`, `RawInvocation`, `Stdin`, `Frame`, `invocation`, `file_effect`.

- [ ] **Step 5: Implement the pipeline**

Replace `crates/devkit-command/src/analyzer.rs` (keep the tests module):

```rust
//! The invocation pipeline every adapter feeds.
//!
//! An adapter walks its own syntax, resolves words, tracks its own bindings,
//! and hands each command it finds to [`Analyzer::invocation`]. From there the
//! path is shared: unwrap wrappers, record the invocation, extract embedded
//! source and recurse, then look the program up in the effect catalog.

use std::ops::Range;

use crate::{
    budget::Budget,
    catalog, embed,
    context::{Context, Dialect},
    model::{
        Analysis, FileEffect, FileOp, Invocation, Language, Location, ScriptFileInvocation,
        TreeEffect, Uncertainty, UncertaintyKind, Value,
    },
    normalize, paths,
};

#[derive(Debug, Clone)]
pub(crate) struct Word {
    pub(crate) value: Value,
    pub(crate) typed: String,
    pub(crate) span: Range<usize>,
}

/// What an invocation reads on stdin, when that is statically known to be
/// text rather than a file or another process's output.
#[derive(Debug, Clone)]
pub(crate) enum Stdin {
    None,
    Source { value: Value, span: Range<usize> },
}

#[derive(Debug, Clone)]
pub(crate) struct RawInvocation {
    pub(crate) words: Vec<Word>,
    pub(crate) stdin: Stdin,
    pub(crate) cwd: Option<String>,
    pub(crate) language: Language,
    pub(crate) location: Location,
}

/// Where the source being analyzed sits: how deeply nested, what arguments
/// its interpreter handed it, and the outer location every finding inside it
/// reports.
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub(crate) depth: usize,
    pub(crate) script_args: Vec<Value>,
    pub(crate) base: Option<Location>,
    pub(crate) cwd: Option<String>,
}

impl Frame {
    pub(crate) fn outer() -> Self {
        Self { depth: 0, script_args: Vec::new(), base: None, cwd: None }
    }

    /// The location a finding at `span` in this frame's source reports.
    pub(crate) fn locate(&self, span: Range<usize>) -> Location {
        match &self.base {
            None => Location { outer: span, embedded: None },
            Some(base) => Location { outer: base.outer.clone(), embedded: Some(span) },
        }
    }
}

pub(crate) struct Analyzer<'c> {
    pub(crate) ctx: &'c Context,
    pub(crate) budget: Budget,
    pub(crate) out: Analysis,
}

impl<'c> Analyzer<'c> {
    pub(crate) fn new(ctx: &'c Context) -> Self {
        Self { ctx, budget: Budget::new(ctx.limits), out: Analysis::default() }
    }

    pub(crate) fn run_source(mut self, source: &str) -> Analysis {
        let language = match self.ctx.dialect {
            Dialect::Bash => Language::Bash,
            Dialect::PowerShell => Language::PowerShell,
            Dialect::Fish => Language::Fish,
        };
        let frame = Frame { cwd: self.ctx.cwd.clone(), ..Frame::outer() };
        self.source(language, source, frame);
        self.out
    }

    pub(crate) fn run_argv(mut self, argv: &[String]) -> Analysis {
        let words = argv
            .iter()
            .map(|w| Word { value: Value::Known(w.clone()), typed: w.clone(), span: 0..0 })
            .collect();
        let raw = RawInvocation {
            words,
            stdin: Stdin::None,
            cwd: self.ctx.cwd.clone(),
            language: Language::Bash,
            location: Location { outer: 0..0, embedded: None },
        };
        let frame = Frame { cwd: self.ctx.cwd.clone(), ..Frame::outer() };
        self.invocation(raw, &frame);
        self.out
    }

    /// Parse and walk `text` as `language` inside `frame`.
    pub(crate) fn source(&mut self, language: Language, text: &str, frame: Frame) {
        let at = frame.locate(0..text.len());
        if let Err(limit) = self.budget.admit(text.len(), frame.depth) {
            self.uncertain(
                UncertaintyKind::LimitExhausted(limit),
                format!("{language:?} source was not analyzed"),
                at,
            );
            return;
        }
        match language {
            Language::Bash => crate::bash::walk(self, text, &frame),
            Language::Fish => crate::fish::walk(self, text, &frame),
            Language::PowerShell => crate::powershell::walk(self, text, &frame),
            Language::Python => crate::python::walk(self, text, &frame),
            Language::JavaScript | Language::TypeScript => crate::js::walk(self, language, text, &frame),
        }
    }

    pub(crate) fn uncertain(&mut self, kind: UncertaintyKind, detail: impl Into<String>, location: Location) {
        self.out.uncertainties.push(Uncertainty { kind, detail: detail.into(), location });
    }

    pub(crate) fn file_effect(&mut self, op: FileOp, path: &Value, cwd: Option<&str>, location: Location) {
        let target = paths::resolve(path, cwd, self.ctx.path_style);
        self.out.file_effects.push(FileEffect { op, target, location });
    }

    pub(crate) fn tree_effect(
        &mut self,
        scope: &Value,
        whole_checkout: bool,
        cwd: Option<&str>,
        by: &str,
        location: Location,
    ) {
        match paths::resolve(scope, cwd, self.ctx.path_style) {
            crate::model::Target::Path(scope) => self.out.tree_effects.push(TreeEffect {
                scope,
                whole_checkout,
                by: by.to_string(),
                location,
            }),
            crate::model::Target::Unresolved => self.uncertain(
                UncertaintyKind::UnresolvedWrite,
                format!("`{by}` rewrites a directory that could not be determined"),
                location,
            ),
        }
    }

    pub(crate) fn invocation(&mut self, raw: RawInvocation, frame: &Frame) {
        let unwrapped = normalize::unwrap(self, &raw, frame);
        let Some(program) = unwrapped.argv.first() else {
            return;
        };
        let location = raw.location.clone();
        let cwd = unwrapped.cwd.apply(raw.cwd.clone());
        let program_value = program.value.clone();
        let args: Vec<Value> = unwrapped.argv[1..].iter().map(|w| w.value.clone()).collect();
        let git = normalize::program_options(&program_value, &args, cwd.as_deref(), self.ctx.path_style);

        self.out.invocations.push(Invocation {
            program: program_value.clone(),
            args: args.clone(),
            semantic_args: git.semantic_args.clone(),
            wrappers: unwrapped
                .wrappers
                .iter()
                .map(|ws| ws.iter().map(|w| w.value.clone()).collect())
                .collect(),
            typed: raw.words.iter().map(|w| w.typed.clone()).collect(),
            cwd: cwd.clone(),
            language: raw.language,
            depth: frame.depth,
            location: location.clone(),
        });

        let Some(name) = program_value.known().map(normalize::basename).map(str::to_string) else {
            self.uncertain(
                UncertaintyKind::UnresolvedInvocation,
                format!("the program `{}` could not be determined", program.typed),
                location,
            );
            return;
        };

        match embed::classify(&name, &args, &raw.stdin) {
            embed::Exec::Source { language, source, script_args } => match source.known() {
                Some(text) => {
                    let child = Frame {
                        depth: frame.depth + 1,
                        script_args,
                        base: Some(location.clone()),
                        cwd: cwd.clone(),
                    };
                    let text = text.to_string();
                    self.source(language, &text, child);
                }
                None => self.uncertain(
                    UncertaintyKind::UnresolvedWrite,
                    format!("`{name}` runs source that could not be determined"),
                    location.clone(),
                ),
            },
            embed::Exec::ScriptFile { script } => self.out.script_files.push(ScriptFileInvocation {
                interpreter: Some(name.clone()),
                script,
                location: location.clone(),
            }),
            embed::Exec::Unsupported { language } => self.uncertain(
                UncertaintyKind::UnsupportedLanguage(language.to_string()),
                format!("`{name}` runs {language} source, which devkit cannot analyze"),
                location.clone(),
            ),
            embed::Exec::Plain => {}
        }

        let effective_cwd = git.cwd.apply(cwd.clone());
        let effective_cwd = effective_cwd.as_deref();
        for hit in catalog::effects(&name, &git.semantic_args) {
            match hit {
                catalog::Hit::File(op, path) => self.file_effect(op, &path, effective_cwd, location.clone()),
                catalog::Hit::Tree { scope, whole_checkout, by } => {
                    self.tree_effect(&scope, whole_checkout, effective_cwd, &by, location.clone())
                }
                catalog::Hit::Unresolved(detail) => {
                    self.uncertain(UncertaintyKind::UnresolvedWrite, detail, location.clone())
                }
                catalog::Hit::ScriptFile(script) => self.out.script_files.push(ScriptFileInvocation {
                    interpreter: None,
                    script,
                    location: location.clone(),
                }),
            }
        }
    }
}
```

Give `normalize`, `embed`, and `catalog` the minimal bodies the pipeline calls, so this task compiles; Tasks 4 to 6 fill them.

`crates/devkit-command/src/normalize.rs`:

```rust
use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Word},
    context::PathStyle,
    model::Value,
};

pub(crate) fn basename(prog: &str) -> &str {
    prog.rsplit(['/', '\\']).next().unwrap_or(prog)
}

/// How a wrapper or a program's own option moves the directory the inner
/// command runs in. `Unknown` is distinct from `Inherit`: `git -C "$d"` must
/// not fall back to the shell's directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CwdChange {
    Inherit,
    To(String),
    Unknown,
}

impl CwdChange {
    pub(crate) fn apply(&self, inherited: Option<String>) -> Option<String> {
        match self {
            CwdChange::Inherit => inherited,
            CwdChange::To(dir) => Some(dir.clone()),
            CwdChange::Unknown => None,
        }
    }
}

pub(crate) struct Unwrapped {
    pub(crate) argv: Vec<Word>,
    pub(crate) wrappers: Vec<Vec<Word>>,
    pub(crate) cwd: CwdChange,
}

pub(crate) fn unwrap(_a: &mut Analyzer<'_>, raw: &RawInvocation, _frame: &Frame) -> Unwrapped {
    Unwrapped { argv: raw.words.clone(), wrappers: Vec::new(), cwd: CwdChange::Inherit }
}

pub(crate) struct ProgramOptions {
    pub(crate) semantic_args: Vec<Value>,
    pub(crate) cwd: CwdChange,
}

pub(crate) fn program_options(_program: &Value, args: &[Value], _cwd: Option<&str>, _style: PathStyle) -> ProgramOptions {
    ProgramOptions { semantic_args: args.to_vec(), cwd: CwdChange::Inherit }
}
```

`crates/devkit-command/src/embed.rs`:

```rust
use crate::{analyzer::Stdin, model::{Language, Value}};

pub(crate) enum Exec {
    Source { language: Language, source: Value, script_args: Vec<Value> },
    ScriptFile { script: Value },
    Unsupported { language: &'static str },
    Plain,
}

pub(crate) fn classify(_name: &str, _args: &[Value], _stdin: &Stdin) -> Exec {
    Exec::Plain
}
```

`crates/devkit-command/src/catalog.rs`:

```rust
use crate::model::{FileOp, Value};

pub(crate) enum Hit {
    File(FileOp, Value),
    Tree { scope: Value, whole_checkout: bool, by: String },
    Unresolved(String),
    ScriptFile(Value),
}

pub(crate) fn effects(_name: &str, _args: &[Value]) -> Vec<Hit> {
    Vec::new()
}
```

Give each adapter module a no-op `walk` with the signature the pipeline calls:

```rust
// bash.rs, fish.rs, powershell.rs, python.rs
use crate::analyzer::{Analyzer, Frame};
pub(crate) fn walk(_a: &mut Analyzer<'_>, _source: &str, _frame: &Frame) {}
```

```rust
// js.rs
use crate::{analyzer::{Analyzer, Frame}, model::Language};
pub(crate) fn walk(_a: &mut Analyzer<'_>, _language: Language, _source: &str, _frame: &Frame) {}
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): resolve paths and route invocations"
```

### Task 3: Bash adapter

**Files:**
- Modify: `crates/devkit-command/src/bash.rs`
- Modify: `crates/devkit-command/src/lib.rs` (a `#[cfg(test)] pub(crate) mod testutil`)

**Interfaces:**
- Consumes: `Analyzer::{invocation, file_effect, uncertain, source}`, `Word`, `Stdin`, `RawInvocation`, `Frame`, `ts::*`, `catalog::is_cataloged(&str) -> bool` and `embed::is_interpreter(&str) -> bool` (both added here as `false`-returning stubs if Tasks 5 and 6 have not landed; they replace the stubs).
- Produces: `bash::walk(&mut Analyzer, &str, &Frame)`; `bash::simple_argv(&str) -> Option<Vec<String>>` (one plain command, every word known), used by Task 4 for `doppler run --command`.

The adapter owns bash syntax and bindings. It does not decide what a program does: every command goes through `Analyzer::invocation`. Redirects are the one effect it records itself, because they belong to the shell, not the program.

- [ ] **Step 1: Pin the grammar's node shapes**

Add a test that prints the tree for each construct this task handles, run it once, and keep it as a regression guard on the pinned grammar:

```rust
#[cfg(test)]
mod shapes {
    use crate::{model::Language, ts};

    #[test]
    fn node_shapes_this_adapter_relies_on() {
        let cases = [
            ("echo x > a.txt", "(program (redirected_statement body: (command name: (command_name (word)) argument: (word)) redirect: (file_redirect destination: (word))))"),
            ("cat > n.md <<'EOF'\nhi\nEOF\n", "heredoc_redirect"),
            ("cat <<EOF | python3 -\nprint(1)\nEOF\n", "heredoc_redirect"),
            ("f=a; echo \"$f\"", "variable_assignment"),
            ("for f in a b; do echo $f; done", "for_statement"),
        ];
        for (source, expected) in cases {
            let tree = ts::parse(Language::Bash, source).unwrap();
            let sexp = tree.root_node().to_sexp();
            assert!(sexp.contains(expected), "{source:?}\n{sexp}");
        }
    }
}
```

Run: `cargo nextest run -p devkit-command shapes --no-capture`
Expected: the first case may fail on exact field layout. Replace each expected string with the smallest fragment of the printed S-expression that names the node kinds and fields the walker below reads (`body:`, `redirect:`, `destination:`, `heredoc_body`, `variable:`, `value:`). Where the printed tree for `cat <<EOF | python3 -` nests the pipeline inside `heredoc_redirect`, note it: Step 5's `redirected` handles both layouts.

- [ ] **Step 2: Write the failing adapter tests**

Add the test helper to `crates/devkit-command/src/lib.rs`:

```rust
#[cfg(test)]
pub(crate) mod testutil {
    use crate::{Analysis, Context, Dialect, Limits, PathStyle, Target};

    pub(crate) fn ctx(dialect: Dialect) -> Context {
        Context { dialect, cwd: Some("/repo".into()), path_style: PathStyle::Unix, limits: Limits::default() }
    }

    pub(crate) fn bash(source: &str) -> Analysis {
        crate::analyze(source, &ctx(Dialect::Bash))
    }

    /// Every resolved write target, in order, as strings; `?` for unresolved.
    pub(crate) fn targets(a: &Analysis) -> Vec<String> {
        a.file_effects
            .iter()
            .map(|e| match &e.target {
                Target::Path(p) => p.clone(),
                Target::Unresolved => "?".into(),
            })
            .collect()
    }

    pub(crate) fn programs(a: &Analysis) -> Vec<String> {
        a.invocations.iter().map(|i| i.program.known().unwrap_or("?").to_string()).collect()
    }
}
```

In `crates/devkit-command/src/bash.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::{
        model::{FileOp, UncertaintyKind},
        testutil::{bash, programs, targets},
    };

    #[test]
    fn redirects_overwrite_and_append() {
        let a = bash("echo hi > a.txt; echo x >> b.txt");
        assert_eq!(targets(&a), ["/repo/a.txt", "/repo/b.txt"]);
        assert_eq!(a.file_effects[0].op, FileOp::Overwrite);
        assert_eq!(a.file_effects[1].op, FileOp::Append);
    }

    #[test]
    fn device_redirects_and_descriptor_duplication_are_not_writes() {
        let a = bash("make >/dev/null 2>&1; ls 2>/dev/stderr");
        assert!(a.file_effects.is_empty(), "{:?}", a.file_effects);
    }

    #[test]
    fn quoted_text_is_data() {
        let a = bash(r#"git commit -m "echo x > y.txt && rm -rf src""#);
        assert!(a.file_effects.is_empty());
        assert_eq!(programs(&a), ["git"]);
    }

    #[test]
    fn cd_moves_relative_targets_in_execution_order() {
        assert_eq!(targets(&bash("echo a > a.txt; cd sub && echo b > b.txt")), ["/repo/a.txt", "/repo/sub/b.txt"]);
    }

    #[test]
    fn a_subshell_directory_change_does_not_leak() {
        assert_eq!(targets(&bash("(cd sub); echo x > a.txt")), ["/repo/a.txt"]);
    }

    #[test]
    fn a_literal_assignment_resolves_a_later_word() {
        assert_eq!(targets(&bash("f=out.txt; echo x > \"$f\"")), ["/repo/out.txt"]);
        assert_eq!(targets(&bash("d=gen; echo x > ${d}/a.txt")), ["/repo/gen/a.txt"]);
    }

    #[test]
    fn an_unbound_or_conditional_variable_is_unresolved() {
        assert_eq!(targets(&bash("echo x > \"$OUT\"")), ["?"]);
        assert_eq!(targets(&bash("false || f=a.txt; echo x > \"$f\"")), ["?"]);
        assert_eq!(targets(&bash("if true; then f=a.txt; fi; echo x > \"$f\"")), ["?"]);
    }

    #[test]
    fn globs_and_brace_expansion_are_unresolved() {
        assert_eq!(targets(&bash("echo x > *.txt")), ["?"]);
        assert_eq!(targets(&bash("echo x > {a,b}.txt")), ["?"]);
    }

    #[test]
    fn a_command_substitution_is_analyzed_and_its_value_is_unknown() {
        let a = bash("x=$(echo hi > s.txt); echo y > \"$x\"");
        assert_eq!(targets(&a), ["/repo/s.txt", "?"]);
    }

    #[test]
    fn heredoc_content_is_data() {
        let a = bash("cat > notes.md <<'EOF'\nrm -rf src\necho x > y.txt\nEOF\n");
        assert_eq!(programs(&a), ["cat"]);
        assert_eq!(targets(&a), ["/repo/notes.md"]);
    }

    #[test]
    fn a_substitution_inside_an_unquoted_heredoc_runs() {
        let a = bash("cat > n.md <<EOF\n$(echo t > t.txt)\nEOF\n");
        assert!(targets(&a).contains(&"/repo/t.txt".to_string()), "{:?}", targets(&a));
    }

    #[test]
    fn a_loop_over_literal_words_is_analyzed_per_word() {
        assert_eq!(
            targets(&bash("for f in a.txt b.txt; do echo x > \"$f\"; done")),
            ["/repo/a.txt", "/repo/b.txt"]
        );
        assert_eq!(targets(&bash("for f in $(ls); do echo x > \"$f\"; done")), ["?"]);
    }

    #[test]
    fn a_function_is_analyzed_where_it_is_called() {
        assert_eq!(targets(&bash("w() { echo x > w.txt; }; w")), ["/repo/w.txt"]);
        assert!(bash("w() { echo x > w.txt; }").file_effects.is_empty());
    }

    #[test]
    fn pipeline_elements_are_invocations() {
        assert_eq!(programs(&bash("cat a | grep b | sort")), ["cat", "grep", "sort"]);
    }

    #[test]
    fn a_broken_statement_does_not_discard_its_siblings() {
        let a = bash("echo a > a.txt\necho b > b.txt )\necho c > c.txt\n");
        let t = targets(&a);
        assert!(t.contains(&"/repo/a.txt".to_string()), "{t:?}");
        assert!(t.contains(&"/repo/c.txt".to_string()), "{t:?}");
        assert!(a.uncertainties.iter().any(|u| u.kind == UncertaintyKind::ParseError));
    }

    #[test]
    fn a_broken_read_only_statement_is_silent() {
        let a = bash("ls -la )\n");
        assert!(a.uncertainties.is_empty(), "{:?}", a.uncertainties);
    }

    #[test]
    fn simple_argv_reads_one_plain_command() {
        assert_eq!(super::simple_argv("bun test --watch"), Some(vec!["bun".into(), "test".into(), "--watch".into()]));
        assert_eq!(super::simple_argv("bun test && ls"), None);
        assert_eq!(super::simple_argv("bun $X"), None);
    }
}
```

If `a_broken_statement_does_not_discard_its_siblings` or `a_broken_read_only_statement_is_silent` shows tree-sitter extending the `ERROR` region across the neighbouring lines, change the fixture's broken token (try `fi`, `done`, or `;;` in place of `)`) until the printed tree confines the error to the middle statement. Do not weaken the assertions.

- [ ] **Step 3: Run to see them fail**

Run: `cargo nextest run -p devkit-command bash::tests`
Expected: FAIL; `walk` is a no-op.

- [ ] **Step 4: Implement the adapter**

`crates/devkit-command/src/bash.rs` (keep the tests and shapes modules):

```rust
//! Bash: statements in execution order, literal bindings, redirects,
//! substitutions, heredocs, and the commands handed to the pipeline.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    catalog, embed,
    model::{FileOp, Language, Limit, UncertaintyKind, Value},
    normalize, paths, ts,
};

const MAX_LOOP_WORDS: usize = 32;

#[derive(Debug, Clone, Default, PartialEq)]
struct Scope {
    vars: HashMap<String, Value>,
    cwd: Option<String>,
    positional: Vec<Value>,
    functions: HashMap<String, std::ops::Range<usize>>,
}

impl Scope {
    /// Fold the bindings of a branch that may or may not have run back into
    /// this scope: anything the branch changed is no longer a single value.
    fn merge_uncertain(&mut self, branch: &Scope) {
        for (name, value) in &branch.vars {
            if self.vars.get(name) != Some(value) {
                self.vars.insert(name.clone(), Value::Unknown);
            }
        }
        if branch.cwd != self.cwd {
            self.cwd = None;
        }
    }
}

pub(crate) fn walk(a: &mut Analyzer<'_>, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(Language::Bash, source) else {
        a.uncertain(UncertaintyKind::ParseError, "bash source did not parse", frame.locate(0..source.len()));
        return;
    };
    let mut scope = Scope { cwd: frame.cwd.clone(), positional: frame.script_args.clone(), ..Scope::default() };
    let mut w = Walker { a, source, frame, root: tree.root_node(), exhausted: false };
    w.statements(tree.root_node(), &mut scope);
}

/// One plain command with every word known, or `None`.
pub(crate) fn simple_argv(source: &str) -> Option<Vec<String>> {
    let tree = ts::parse(Language::Bash, source)?;
    let root = tree.root_node();
    let [command] = ts::named_children(root)[..] else {
        return None;
    };
    if command.kind() != "command" || command.has_error() {
        return None;
    }
    ts::named_children(command)
        .into_iter()
        .map(|n| match n.kind() {
            "command_name" | "word" => Some(unescape(ts::text(n, source))),
            "raw_string" => Some(ts::text(n, source).trim_matches('\'').to_string()),
            _ => None,
        })
        .map(|w| w.filter(|w| !w.contains(['$', '*', '?', '`'])))
        .collect()
}

struct Walker<'a, 'c, 's, 't> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
    root: Node<'t>,
    exhausted: bool,
}

impl<'t> Walker<'_, '_, '_, 't> {
    fn visit(&mut self, node: Node<'t>) -> bool {
        if self.exhausted {
            return false;
        }
        if self.a.budget.visit().is_err() {
            self.exhausted = true;
            let at = self.frame.locate(node.byte_range());
            self.a.uncertain(UncertaintyKind::LimitExhausted(Limit::Nodes), "bash source was only partly analyzed", at);
            return false;
        }
        true
    }

    fn statements(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            self.statement(child, scope, Stdin::None);
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        if !self.visit(node) {
            return;
        }
        if node.has_error() && !matches!(node.kind(), "program" | "compound_statement" | "subshell") {
            self.broken(node);
            return;
        }
        match node.kind() {
            "comment" => {}
            "command" => self.command(node, scope, stdin),
            "redirected_statement" => self.redirected(node, scope, stdin),
            "variable_assignment" => self.assign(node, scope),
            "variable_assignments" | "declaration_command" => {
                for child in ts::named_children(node) {
                    if child.kind() == "variable_assignment" {
                        self.assign(child, scope);
                    }
                }
            }
            "list" => self.list(node, scope),
            "pipeline" => self.pipeline(ts::named_children(node), scope, stdin),
            "subshell" => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
            }
            "compound_statement" | "negated_command" => self.statements(node, scope),
            "for_statement" => self.for_loop(node, scope),
            "if_statement" | "while_statement" | "case_statement" | "c_style_for_statement" | "elif_clause"
            | "else_clause" | "do_group" | "case_item" => {
                let mut branch = scope.clone();
                self.statements(node, &mut branch);
                scope.merge_uncertain(&branch);
            }
            "function_definition" => {
                if let (Some(name), Some(body)) = (node.child_by_field_name("name"), node.child_by_field_name("body")) {
                    scope.functions.insert(ts::text(name, self.source).to_string(), body.byte_range());
                }
            }
            _ => self.substitutions(node, scope),
        }
    }

    /// A statement the parser could not read. Uncertain only if it could write.
    fn broken(&mut self, node: Node<'t>) {
        let mut could_write = false;
        let mut program: Option<String> = None;
        let mut cursor = node.walk();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if !n.is_named() && matches!(ts::text(n, self.source), ">" | ">>" | "&>" | "&>>" | ">|") {
                could_write = true;
            }
            // Inside an ERROR node the grammar often emits bare `word` nodes
            // with no `command_name`; the first one stands in for it.
            if program.is_none() && matches!(n.kind(), "command_name" | "word") {
                program = Some(normalize::basename(ts::text(n, self.source)).to_string());
            }
            stack.extend(n.children(&mut cursor).collect::<Vec<_>>().into_iter().rev());
        }
        let program_could_write = match &program {
            None => true,
            Some(p) => catalog::is_cataloged(p) || embed::is_interpreter(p),
        };
        if could_write || program_could_write {
            let at = self.frame.locate(node.byte_range());
            self.a.uncertain(UncertaintyKind::ParseError, "a statement could not be parsed", at);
        }
    }

    fn list(&mut self, node: Node<'t>, scope: &mut Scope) {
        let mut cursor = node.walk();
        let mut after_or = false;
        for child in node.children(&mut cursor).collect::<Vec<_>>() {
            if !child.is_named() {
                after_or = ts::text(child, self.source) == "||";
                continue;
            }
            if after_or {
                let mut branch = scope.clone();
                self.statement(child, &mut branch, Stdin::None);
                scope.merge_uncertain(&branch);
            } else {
                self.statement(child, scope, Stdin::None);
            }
        }
    }

    /// Each element runs in its own subshell. What a known producer writes to
    /// stdout becomes the next element's stdin; any other producer hands the
    /// next element stdin of unknown content.
    fn pipeline(&mut self, elements: Vec<Node<'t>>, scope: &Scope, first_stdin: Stdin) {
        let mut stdin = first_stdin;
        for element in elements {
            let produced = self.produced_output(element, scope);
            let mut inner = scope.clone();
            self.statement(element, &mut inner, stdin);
            stdin = Stdin::Source { value: produced, span: element.byte_range() };
        }
    }

    /// The stdout of `echo`, `printf '%s'`, or `cat` fed by a heredoc, when
    /// statically known.
    fn produced_output(&mut self, node: Node<'t>, scope: &Scope) -> Value {
        let (command, heredoc) = match node.kind() {
            "command" => (node, None),
            "redirected_statement" => (
                match node.child_by_field_name("body") {
                    Some(b) if b.kind() == "command" => b,
                    _ => return Value::Unknown,
                },
                ts::named_children(node).into_iter().find(|n| matches!(n.kind(), "heredoc_redirect" | "herestring_redirect")),
            ),
            _ => return Value::Unknown,
        };
        let mut probe = scope.clone();
        let words: Vec<Word> = ts::named_children(command)
            .into_iter()
            .filter(|n| n.kind() != "variable_assignment")
            .map(|n| self.word(n, &mut probe))
            .collect();
        let Some(program) = words.first().and_then(|w| w.value.known()).map(normalize::basename) else {
            return Value::Unknown;
        };
        let rest: Option<Vec<&str>> = words[1..].iter().map(|w| w.value.known()).collect();
        match (program, rest, heredoc) {
            ("echo", Some(rest), None) if rest.first().is_none_or(|w| !w.starts_with('-')) => {
                Value::Known(format!("{}\n", rest.join(" ")))
            }
            ("printf", Some(rest), None) => match rest.as_slice() {
                [single] if !single.contains('%') && !single.contains('\\') => Value::Known((*single).to_string()),
                [fmt, arg] if matches!(*fmt, "%s" | "%s\\n") => Value::Known((*arg).to_string()),
                _ => Value::Unknown,
            },
            ("cat", Some(rest), Some(h)) if rest.is_empty() => match self.stdin_of(h, &mut probe) {
                Stdin::Source { value, .. } => value,
                Stdin::None => Value::Unknown,
            },
            _ => Value::Unknown,
        }
    }

    fn redirected(&mut self, node: Node<'t>, scope: &mut Scope, mut stdin: Stdin) {
        let mut trailing_pipeline: Option<Node<'t>> = None;
        for redirect in ts::named_children(node) {
            match redirect.kind() {
                "file_redirect" => self.file_redirect(redirect, scope),
                "heredoc_redirect" | "herestring_redirect" => {
                    stdin = self.stdin_of(redirect, scope);
                    trailing_pipeline = ts::named_children(redirect)
                        .into_iter()
                        .find(|n| matches!(n.kind(), "pipeline" | "command" | "list" | "redirected_statement"));
                }
                _ => {}
            }
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        match trailing_pipeline {
            None => self.statement(body, scope, stdin),
            Some(rest) => {
                let produced = self.produced_output(node, scope);
                let mut inner = scope.clone();
                self.statement(body, &mut inner, stdin);
                let elements = match rest.kind() {
                    "pipeline" => ts::named_children(rest),
                    _ => vec![rest],
                };
                self.pipeline(elements, scope, Stdin::Source { value: produced, span: body.byte_range() });
            }
        }
    }

    fn file_redirect(&mut self, node: Node<'t>, scope: &mut Scope) {
        let mut cursor = node.walk();
        let operator = node
            .children(&mut cursor)
            .find(|c| !c.is_named())
            .map(|c| ts::text(c, self.source))
            .unwrap_or("");
        let op = match operator {
            ">" | ">|" | "&>" => FileOp::Overwrite,
            ">>" | "&>>" => FileOp::Append,
            _ => return,
        };
        let Some(dest) = node.child_by_field_name("destination") else {
            return;
        };
        let word = self.word(dest, scope);
        if matches!(word.value.known(), Some("/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty"))
            || dest.kind() == "number"
        {
            return;
        }
        let at = self.frame.locate(node.byte_range());
        self.a.file_effect(op, &word.value, scope.cwd.as_deref(), at);
    }

    fn stdin_of(&mut self, redirect: Node<'t>, scope: &mut Scope) -> Stdin {
        let span = redirect.byte_range();
        if redirect.kind() == "herestring_redirect" {
            let value = ts::named_children(redirect)
                .into_iter()
                .last()
                .map(|n| self.word(n, scope).value)
                .map(|v| match v {
                    Value::Known(s) => Value::Known(format!("{s}\n")),
                    Value::Unknown => Value::Unknown,
                })
                .unwrap_or(Value::Unknown);
            return Stdin::Source { value, span };
        }
        let quoted = ts::named_children(redirect)
            .into_iter()
            .find(|n| n.kind() == "heredoc_start")
            .is_some_and(|s| ts::text(s, self.source).contains(['\'', '"', '\\']));
        let Some(body) = ts::named_children(redirect).into_iter().find(|n| n.kind() == "heredoc_body") else {
            return Stdin::Source { value: Value::Known(String::new()), span };
        };
        if quoted {
            return Stdin::Source { value: Value::Known(ts::text(body, self.source).to_string()), span: body.byte_range() };
        }
        let mut expands = false;
        for part in ts::named_children(body) {
            match part.kind() {
                "command_substitution" | "process_substitution" => {
                    expands = true;
                    let mut inner = scope.clone();
                    self.statements(part, &mut inner);
                }
                "simple_expansion" | "expansion" | "arithmetic_expansion" => expands = true,
                _ => {}
            }
        }
        let value = if expands { Value::Unknown } else { Value::Known(ts::text(body, self.source).to_string()) };
        Stdin::Source { value, span: body.byte_range() }
    }

    fn assign(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let name = ts::text(name, self.source).to_string();
        let appends = ts::text(node, self.source).contains("+=");
        let value = match node.child_by_field_name("value") {
            None => Value::Known(String::new()),
            Some(v) if v.kind() == "array" || appends => {
                self.substitutions(v, scope);
                Value::Unknown
            }
            Some(v) => self.word(v, scope).value,
        };
        scope.vars.insert(name, value);
    }

    fn for_loop(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(var) = node.child_by_field_name("variable").map(|v| ts::text(v, self.source).to_string()) else {
            return;
        };
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut cursor = node.walk();
        let value_nodes: Vec<Node<'t>> = node.children_by_field_name("value", &mut cursor).collect();
        let values: Vec<Value> = value_nodes.iter().map(|n| self.word(*n, scope).value).collect();
        let literal = !values.is_empty() && values.len() <= MAX_LOOP_WORDS && values.iter().all(|v| v.known().is_some());
        let mut after = scope.clone();
        if literal {
            for value in values {
                let mut iteration = scope.clone();
                iteration.vars.insert(var.clone(), value);
                self.statements(body, &mut iteration);
                after.merge_uncertain(&iteration);
            }
        } else {
            let mut iteration = scope.clone();
            iteration.vars.insert(var.clone(), Value::Unknown);
            self.statements(body, &mut iteration);
            after.merge_uncertain(&iteration);
        }
        after.vars.insert(var, Value::Unknown);
        *scope = after;
    }

    fn command(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        let mut words: Vec<Word> = Vec::new();
        for child in ts::named_children(node) {
            match child.kind() {
                "variable_assignment" => self.substitutions(child, scope),
                "file_redirect" => self.file_redirect(child, scope),
                _ => words.push(self.word(child, scope)),
            }
        }
        let Some(first) = words.first() else {
            return;
        };
        match first.value.known() {
            Some("cd" | "pushd" | "Set-Location") => {
                scope.cwd = match words.get(1).map(|w| &w.value) {
                    Some(v @ Value::Known(p)) if p != "-" => {
                        match paths::resolve(v, scope.cwd.as_deref(), self.a.ctx.path_style) {
                            crate::model::Target::Path(dir) => Some(dir),
                            crate::model::Target::Unresolved => None,
                        }
                    }
                    _ => None,
                };
                return;
            }
            Some("popd") => {
                scope.cwd = None;
                return;
            }
            Some("read" | "mapfile" | "readarray") => {
                for w in &words[1..] {
                    if let Some(name) = w.value.known().filter(|n| !n.starts_with('-')) {
                        scope.vars.insert(name.to_string(), Value::Unknown);
                    }
                }
                return;
            }
            Some(name) if scope.functions.contains_key(name) => {
                let body = scope.functions[name].clone();
                if let Some(body) = self.root.descendant_for_byte_range(body.start, body.end) {
                    let mut call = scope.clone();
                    call.positional = std::iter::once(Value::Known(name.to_string()))
                        .chain(words[1..].iter().map(|w| w.value.clone()))
                        .collect();
                    self.statements(body, &mut call);
                }
                return;
            }
            _ => {}
        }
        let raw = RawInvocation {
            words,
            stdin,
            cwd: scope.cwd.clone(),
            language: Language::Bash,
            location: self.frame.locate(node.byte_range()),
        };
        self.a.invocation(raw, self.frame);
    }

    /// Run every substitution under `node` for its effects.
    fn substitutions(&mut self, node: Node<'t>, scope: &Scope) {
        for child in ts::named_children(node) {
            match child.kind() {
                "command_substitution" | "process_substitution" => {
                    let mut inner = scope.clone();
                    self.statements(child, &mut inner);
                }
                _ => self.substitutions(child, scope),
            }
        }
    }

    fn word(&mut self, node: Node<'t>, scope: &mut Scope) -> Word {
        let typed = ts::text(node, self.source);
        let value = self.value(node, scope);
        let value = match value {
            Value::Known(s) if !self.a.budget.value_fits(s.len()) => Value::Unknown,
            v => v,
        };
        Word {
            typed: value.known().map_or_else(|| typed.to_string(), str::to_string),
            value,
            span: node.byte_range(),
        }
    }

    fn value(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let text = ts::text(node, self.source);
        match node.kind() {
            "word" | "number" | "command_name" => {
                if let Some(inner) = ts::named_children(node).into_iter().next() {
                    return self.value(inner, scope);
                }
                if text.starts_with('~') || has_unescaped(text, &['*', '?', '[']) || (text.contains('{') && text.contains(',')) {
                    Value::Unknown
                } else {
                    Value::Known(unescape(text))
                }
            }
            "raw_string" => Value::Known(text[1..text.len() - 1].to_string()),
            "ansi_c_string" if !text.contains('\\') => Value::Known(text[2..text.len() - 1].to_string()),
            "string" => {
                let mut out = String::new();
                let mut known = true;
                let mut last = node.start_byte() + 1;
                for part in ts::named_children(node) {
                    out.push_str(&unescape_dquoted(&self.source[last..part.start_byte()]));
                    last = part.end_byte();
                    match self.value(part, scope) {
                        Value::Known(s) => out.push_str(&s),
                        Value::Unknown => known = false,
                    }
                }
                out.push_str(&unescape_dquoted(&self.source[last..node.end_byte().saturating_sub(1).max(last)]));
                if known { Value::Known(out) } else { Value::Unknown }
            }
            "string_content" => Value::Known(unescape_dquoted(text)),
            "concatenation" => {
                let mut out = String::new();
                for part in ts::named_children(node) {
                    match self.value(part, scope) {
                        Value::Known(s) => out.push_str(&s),
                        Value::Unknown => return Value::Unknown,
                    }
                }
                Value::Known(out)
            }
            "simple_expansion" => self.lookup(&text[1..], scope),
            "expansion" => {
                let inner = &text[2..text.len() - 1];
                if inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    self.lookup(inner, scope)
                } else {
                    self.substitutions(node, scope);
                    Value::Unknown
                }
            }
            "command_substitution" | "process_substitution" => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
                Value::Unknown
            }
            _ => {
                self.substitutions(node, scope);
                Value::Unknown
            }
        }
    }

    fn lookup(&self, name: &str, scope: &Scope) -> Value {
        if let Ok(i) = name.parse::<usize>() {
            return scope.positional.get(i).cloned().unwrap_or(Value::Unknown);
        }
        scope.vars.get(name).cloned().unwrap_or(Value::Unknown)
    }
}

fn has_unescaped(text: &str, chars: &[char]) -> bool {
    let mut escaped = false;
    text.chars().any(|c| {
        let hit = !escaped && chars.contains(&c);
        escaped = !escaped && c == '\\';
        hit
    })
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

/// Inside double quotes a backslash escapes only `$`, `` ` ``, `"`, `\` and a
/// newline.
fn unescape_dquoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some('$' | '`' | '"' | '\\')) => out.extend(chars.next()),
            ('\\', Some('\n')) => {
                chars.next();
            }
            (c, _) => out.push(c),
        }
    }
    out
}
```

Add the two predicates the broken-statement check reads, as stubs if their tasks have not landed:

```rust
// catalog.rs
pub(crate) fn is_cataloged(_name: &str) -> bool { false }
// embed.rs
pub(crate) fn is_interpreter(_name: &str) -> bool { false }
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS. When a test fails because the grammar names a node or field differently from the walker, fix the walker to the printed S-expression from Step 1; do not change the expected targets.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): analyze bash structure, bindings and redirects"
```

---

### Task 4: Wrappers, runners and program options

**Files:**
- Modify: `crates/devkit-command/src/normalize.rs`

**Interfaces:**
- Consumes: `Analyzer::invocation` (for `find -exec` children), `bash::simple_argv`, `paths::resolve`.
- Produces: `normalize::unwrap` and `normalize::program_options` with real behaviour; `normalize::doppler_flags(words: &[Value]) -> (Option<String>, Option<String>)` (config, project) for the guard.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use crate::{
        model::Value,
        testutil::{bash, programs},
    };

    fn k(s: &str) -> Value {
        Value::Known(s.into())
    }

    #[test]
    fn process_wrappers_are_removed_and_kept() {
        let a = bash("nohup timeout -k 5 30 env FOO=1 nice -n 5 python3 x.py");
        let inv = a.invocations.iter().find(|i| i.program == k("python3")).expect("python3");
        assert_eq!(inv.wrappers.len(), 4, "{:?}", inv.wrappers);
        assert_eq!(inv.args, vec![k("x.py")]);
    }

    #[test]
    fn a_runner_prefix_is_removed_with_its_options() {
        let a = bash("uv run --with rich --directory sub python -c 'print(1)'");
        let inv = a.invocations.iter().find(|i| i.program == k("python")).expect("python");
        assert_eq!(inv.cwd.as_deref(), Some("/repo/sub"));
        assert_eq!(bash("uvx ruff format src").invocations[0].program, k("ruff"));
        assert_eq!(bash("pnpm exec prettier --write .").invocations[0].program, k("prettier"));
        assert_eq!(bash("npx -y prettier --write .").invocations[0].program, k("prettier"));
    }

    #[test]
    fn a_doppler_wrapper_is_removed_in_both_forms() {
        let a = bash("doppler run -c dev -- bun test");
        assert_eq!(a.invocations[0].program, k("bun"));
        assert_eq!(super::doppler_flags(&a.invocations[0].wrappers[0]), (Some("dev".into()), None));
        let b = bash("doppler run --config prd --command='bun test'");
        assert_eq!(b.invocations[0].program, k("bun"));
        assert_eq!(super::doppler_flags(&b.invocations[0].wrappers[0]), (Some("prd".into()), None));
    }

    #[test]
    fn git_global_options_leave_the_semantic_arguments() {
        let a = bash("git -C /other -c core.x=1 --no-pager worktree add ../wt");
        let inv = &a.invocations[0];
        assert_eq!(inv.semantic_args, vec![k("worktree"), k("add"), k("../wt")]);
        assert_eq!(inv.args.len(), 7, "the full vector keeps -C and -c");
    }

    #[test]
    fn xargs_and_find_exec_run_their_command_with_unknown_arguments() {
        let a = bash("ls | xargs -n 1 rm -f");
        let rm = a.invocations.iter().find(|i| i.program == k("rm")).expect("rm");
        assert_eq!(rm.args.last(), Some(&Value::Unknown));

        let b = bash("find . -name '*.bak' -exec rm {} \\;");
        assert_eq!(programs(&b), ["rm", "find"]);
    }
}
```

The `find -exec` child is recorded before `find` itself because `unwrap` runs it before the outer invocation is pushed; the order is asserted so a change to it is deliberate.

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run -p devkit-command normalize`
Expected: FAIL; `unwrap` returns the words unchanged.

- [ ] **Step 3: Implement**

Replace the stub bodies in `crates/devkit-command/src/normalize.rs` (keep `basename`, `CwdChange`, the structs, and tests):

```rust
/// A wrapper that runs the rest of its arguments as a command: how to
/// recognize it, which of its options take a value, which option moves the
/// directory, and whether a bare `--` ends its own options.
struct Wrapper {
    prefix: &'static [&'static str],
    value_flags: &'static [&'static str],
    cwd_flags: &'static [&'static str],
    /// Positional words the wrapper consumes before the command.
    positional: usize,
}

const WRAPPERS: &[Wrapper] = &[
    Wrapper { prefix: &["nohup"], value_flags: &[], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["setsid"], value_flags: &[], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["exec"], value_flags: &["-a"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["time"], value_flags: &["-f", "-o"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["command"], value_flags: &[], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["builtin"], value_flags: &[], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["nice"], value_flags: &["-n", "--adjustment"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["stdbuf"], value_flags: &["-i", "-o", "-e"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["sudo"], value_flags: &["-u", "-g", "-C", "-h", "-p"], cwd_flags: &["-D", "--chdir"], positional: 0 },
    Wrapper { prefix: &["timeout"], value_flags: &["-k", "--kill-after", "-s", "--signal"], cwd_flags: &[], positional: 1 },
    Wrapper { prefix: &["env"], value_flags: &["-u", "--unset", "-S", "--split-string"], cwd_flags: &["-C", "--chdir"], positional: 0 },
    Wrapper { prefix: &["uv", "run"], value_flags: &["--with", "--with-requirements", "--with-editable", "--python", "-p", "--project", "--env-file", "--group", "--extra", "--package", "--index", "--index-url", "--extra-index-url", "--script"], cwd_flags: &["--directory"], positional: 0 },
    Wrapper { prefix: &["uv", "tool", "run"], value_flags: &["--with", "--from", "--python", "-p"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["uvx"], value_flags: &["--with", "--from", "--python", "-p"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["pipx", "run"], value_flags: &["--spec", "--python"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["poetry", "run"], value_flags: &[], cwd_flags: &["-C", "--directory"], positional: 0 },
    Wrapper { prefix: &["pdm", "run"], value_flags: &[], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["hatch", "run"], value_flags: &[], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["bun", "run"], value_flags: &["--cwd"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["bun", "x"], value_flags: &[], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["bunx"], value_flags: &["-p", "--package"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["pnpm", "exec"], value_flags: &["--filter", "-F"], cwd_flags: &["-C", "--dir"], positional: 0 },
    Wrapper { prefix: &["pnpm", "dlx"], value_flags: &["--package"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["npm", "exec"], value_flags: &["-p", "--package", "-w", "--workspace"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["npx"], value_flags: &["-p", "--package"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["yarn", "dlx"], value_flags: &["-p", "--package"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["mise", "exec"], value_flags: &["-C", "--cd"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["mise", "x"], value_flags: &["-C", "--cd"], cwd_flags: &[], positional: 0 },
    Wrapper { prefix: &["direnv", "exec"], value_flags: &[], cwd_flags: &[], positional: 1 },
];

/// Whether `word` names a wrapper's first word, compared by basename with a
/// Windows `.exe` suffix ignored.
fn program_name(word: &Word) -> Option<String> {
    let name = basename(word.value.known()?);
    Some(name.strip_suffix(".exe").unwrap_or(name).to_string())
}

fn is_assignment(word: &Word) -> bool {
    word.value.known().is_some_and(|w| {
        w.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !name.starts_with(|c: char| c.is_ascii_digit())
        })
    })
}

pub(crate) fn unwrap(a: &mut Analyzer<'_>, raw: &RawInvocation, frame: &Frame) -> Unwrapped {
    let mut argv = raw.words.clone();
    let mut wrappers: Vec<Vec<Word>> = Vec::new();
    let mut cwd = CwdChange::Inherit;
    let mut current_cwd = raw.cwd.clone();

    loop {
        while argv.first().is_some_and(is_assignment) {
            argv.remove(0);
        }
        let Some(first) = argv.first().and_then(program_name) else {
            break;
        };
        if first == "doppler" && argv.get(1).and_then(|w| w.value.known()) == Some("run") {
            match strip_doppler(&argv) {
                Some((consumed, inner)) => {
                    wrappers.push(argv[..consumed].to_vec());
                    argv = inner;
                    continue;
                }
                None => break,
            }
        }
        if first == "xargs" {
            let (consumed, replace) = skip_options(&argv, 1, &["-I", "-i", "-n", "-P", "-d", "-L", "-a", "-s", "-E", "--max-args", "--max-procs", "--delimiter", "--arg-file"]);
            wrappers.push(argv[..consumed].to_vec());
            argv = argv[consumed..].to_vec();
            if argv.is_empty() {
                argv.push(literal("echo"));
            }
            if replace {
                for w in argv.iter_mut().skip(1) {
                    w.value = Value::Unknown;
                }
            } else {
                argv.push(Word { value: Value::Unknown, typed: "<input>".into(), span: 0..0 });
            }
            continue;
        }
        if first == "find" {
            run_find_exec(a, raw, frame, &argv, current_cwd.clone());
            break;
        }
        let Some(wrapper) = WRAPPERS.iter().find(|w| {
            argv.len() > w.prefix.len()
                && w.prefix.iter().enumerate().all(|(i, p)| {
                    if i == 0 { program_name(&argv[0]).as_deref() == Some(*p) } else { argv[i].value.known() == Some(*p) }
                })
        }) else {
            break;
        };
        let mut i = wrapper.prefix.len();
        while let Some(word) = argv.get(i) {
            let Some(text) = word.value.known() else { break };
            if text == "--" {
                i += 1;
                break;
            }
            if !text.starts_with('-') {
                if first == "env" && is_assignment(word) {
                    i += 1;
                    continue;
                }
                break;
            }
            let (flag, inline) = match text.split_once('=') {
                Some((f, v)) => (f, Some(v.to_string())),
                None => (text, None),
            };
            if wrapper.cwd_flags.contains(&flag) {
                let value = match inline {
                    Some(v) => Value::Known(v),
                    None => {
                        i += 1;
                        argv.get(i).map_or(Value::Unknown, |w| w.value.clone())
                    }
                };
                cwd = match paths::resolve(&value, current_cwd.as_deref(), a.ctx.path_style) {
                    crate::model::Target::Path(dir) => CwdChange::To(dir),
                    crate::model::Target::Unresolved => CwdChange::Unknown,
                };
                current_cwd = cwd.apply(current_cwd.clone());
            } else if wrapper.value_flags.contains(&flag) && inline.is_none() {
                i += 1;
            }
            i += 1;
        }
        if first == "direnv" {
            if let Some(dir) = argv.get(i) {
                cwd = match paths::resolve(&dir.value, current_cwd.as_deref(), a.ctx.path_style) {
                    crate::model::Target::Path(d) => CwdChange::To(d),
                    crate::model::Target::Unresolved => CwdChange::Unknown,
                };
                current_cwd = cwd.apply(current_cwd.clone());
            }
        }
        i += wrapper.positional;
        if i >= argv.len() {
            break;
        }
        wrappers.push(argv[..i].to_vec());
        argv = argv[i..].to_vec();
    }
    Unwrapped { argv, wrappers, cwd }
}

fn literal(s: &str) -> Word {
    Word { value: Value::Known(s.into()), typed: s.into(), span: 0..0 }
}

/// Skip `argv[start..]`'s leading options; returns where the command starts
/// and whether an `-I`/`-i` replacement string was given.
fn skip_options(argv: &[Word], start: usize, value_flags: &[&str]) -> (usize, bool) {
    let mut i = start;
    let mut replace = false;
    while let Some(text) = argv.get(i).and_then(|w| w.value.known()) {
        if !text.starts_with('-') {
            break;
        }
        if text == "--" {
            return (i + 1, replace);
        }
        replace |= text.starts_with("-I") || text.starts_with("-i");
        if value_flags.contains(&text) {
            i += 1;
        }
        i += 1;
    }
    (i, replace)
}

/// `(words consumed by the wrapper, inner command)` for `doppler run ... --`
/// and `doppler run ... --command <cmd>`.
fn strip_doppler(argv: &[Word]) -> Option<(usize, Vec<Word>)> {
    if let Some(sep) = argv.iter().position(|w| w.value.known() == Some("--")) {
        return Some((sep + 1, argv[sep + 1..].to_vec()));
    }
    let idx = argv.iter().position(|w| {
        w.value.known().is_some_and(|t| t == "--command" || t.starts_with("--command="))
    })?;
    let text = argv[idx].value.known()?;
    let (value, consumed) = match text.strip_prefix("--command=") {
        Some(v) => (v.to_string(), idx + 1),
        None => (argv.get(idx + 1)?.value.known()?.to_string(), idx + 2),
    };
    let inner = crate::bash::simple_argv(&value)?;
    Some((consumed, inner.iter().map(|w| literal(w)).collect()))
}

pub(crate) fn doppler_flags(words: &[Value]) -> (Option<String>, Option<String>) {
    let (mut config, mut project) = (None, None);
    let mut i = 0;
    while i < words.len() {
        let Some(text) = words[i].known() else {
            i += 1;
            continue;
        };
        let (key, inline) = match text.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (text, None),
        };
        let value = inline.clone().or_else(|| words.get(i + 1).and_then(|v| v.known()).map(str::to_string));
        match key {
            "-c" | "--config" => config = value,
            "-p" | "--project" => project = value,
            _ => {}
        }
        i += if inline.is_some() || !key.starts_with('-') || !matches!(key, "-c" | "--config" | "-p" | "--project") { 1 } else { 2 };
    }
    (config, project)
}

/// Each `-exec`/`-execdir`/`-ok` segment of a `find` runs as its own
/// invocation, with `{}` unknown.
fn run_find_exec(a: &mut Analyzer<'_>, raw: &RawInvocation, frame: &Frame, argv: &[Word], cwd: Option<String>) {
    let mut i = 1;
    while i < argv.len() {
        if matches!(argv[i].value.known(), Some("-exec" | "-execdir" | "-ok" | "-okdir")) {
            let start = i + 1;
            let end = argv[start..]
                .iter()
                .position(|w| matches!(w.value.known(), Some(";" | "+")))
                .map_or(argv.len(), |p| start + p);
            let words = argv[start..end]
                .iter()
                .map(|w| if w.value.known() == Some("{}") { Word { value: Value::Unknown, ..w.clone() } } else { w.clone() })
                .collect();
            let child = RawInvocation { words, stdin: crate::analyzer::Stdin::None, cwd: cwd.clone(), language: raw.language, location: raw.location.clone() };
            a.invocation(child, frame);
            i = end;
        }
        i += 1;
    }
}

/// Git options that precede the subcommand. Each is kept in `args`; only the
/// subcommand and what follows reach `semantic_args`.
const GIT_VALUE_OPTIONS: &[&str] = &["-C", "-c", "--git-dir", "--work-tree", "--namespace", "--exec-path", "--config-env", "--super-prefix"];

pub(crate) fn program_options(program: &Value, args: &[Value], cwd: Option<&str>, style: PathStyle) -> ProgramOptions {
    if program.known().map(basename).map(|n| n.strip_suffix(".exe").unwrap_or(n)) != Some("git") {
        return ProgramOptions { semantic_args: args.to_vec(), cwd: CwdChange::Inherit };
    }
    let mut change = CwdChange::Inherit;
    let mut current = cwd.map(str::to_string);
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else { break };
        if !text.starts_with('-') {
            break;
        }
        let (flag, inline) = match text.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (text, None),
        };
        if GIT_VALUE_OPTIONS.contains(&flag) {
            let value = match inline {
                Some(v) => Value::Known(v),
                None => {
                    i += 1;
                    args.get(i).cloned().unwrap_or(Value::Unknown)
                }
            };
            if matches!(flag, "-C" | "--work-tree") {
                change = match paths::resolve(&value, current.as_deref(), style) {
                    crate::model::Target::Path(dir) => CwdChange::To(dir),
                    crate::model::Target::Unresolved => CwdChange::Unknown,
                };
                current = change.apply(current.clone());
            }
        }
        i += 1;
    }
    ProgramOptions { semantic_args: args[i.min(args.len())..].to_vec(), cwd: change }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): unwrap wrappers, runners and git options"
```

---

### Task 5: Embedded source and interpreter arguments

**Files:**
- Modify: `crates/devkit-command/src/embed.rs`

**Interfaces:**
- Consumes: `Stdin`.
- Produces: `embed::classify(name: &str, args: &[Value], stdin: &Stdin) -> Exec` with real behaviour; `embed::is_interpreter(&str) -> bool`. `Exec::Source.script_args` follows each language's own convention: index `i` is what the script reads as `sys.argv[i]`, `$i`, or `process.argv[i]`.

- [ ] **Step 1: Confirm `process.argv` for evaluated source**

Run once and record the output in the test comment below:

```bash
node -e 'console.log(JSON.stringify(process.argv))' a b
bun -e 'console.log(JSON.stringify(process.argv))' a b
```

Expected shape: node prints `["<node path>","a","b"]`. If bun prints a different offset (for example `["bun","-e","a","b"]` or an extra entry), use that offset for bun in `classify` and assert it in `bun_eval_binds_process_argv_with_its_own_offset`.

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::UncertaintyKind,
        testutil::{bash, targets},
    };

    fn k(s: &str) -> Value {
        Value::Known(s.into())
    }

    fn args(words: &[&str]) -> Vec<Value> {
        words.iter().map(|w| k(w)).collect()
    }

    #[test]
    fn python_c_and_stdin_carry_their_arguments() {
        match classify("python3", &args(&["-c", "print(1)", "a.txt"]), &Stdin::None) {
            Exec::Source { language: Language::Python, source, script_args } => {
                assert_eq!(source, k("print(1)"));
                assert_eq!(script_args, args(&["-c", "a.txt"]));
            }
            _ => panic!("expected python source"),
        }
        let stdin = Stdin::Source { value: k("print(2)"), span: 0..0 };
        match classify("python3", &args(&["-", "src/a.ts"]), &stdin) {
            Exec::Source { source, script_args, .. } => {
                assert_eq!(source, k("print(2)"));
                assert_eq!(script_args, args(&["-", "src/a.ts"]));
            }
            _ => panic!("expected python source"),
        }
    }

    #[test]
    fn a_script_path_is_a_script_file_and_a_module_is_plain() {
        assert!(matches!(classify("python3", &args(&["-u", "tools/gen.py", "x"]), &Stdin::None), Exec::ScriptFile { .. }));
        assert!(matches!(classify("python3", &args(&["-m", "pytest"]), &Stdin::None), Exec::Plain));
        assert!(matches!(classify("bash", &args(&["deploy.sh"]), &Stdin::None), Exec::ScriptFile { .. }));
        assert!(matches!(classify("pwsh", &args(&["-File", "a.ps1"]), &Stdin::None), Exec::ScriptFile { .. }));
    }

    #[test]
    fn shell_c_binds_dollar_zero_from_the_first_trailing_word() {
        match classify("sh", &args(&["-ec", "echo $1", "sh", "out.txt"]), &Stdin::None) {
            Exec::Source { language: Language::Bash, script_args, .. } => assert_eq!(script_args, args(&["sh", "out.txt"])),
            _ => panic!("expected bash source"),
        }
    }

    #[test]
    fn powershell_command_joins_the_remaining_words() {
        match classify("pwsh", &args(&["-NoProfile", "-Command", "Set-Content", "a.txt", "x"]), &Stdin::None) {
            Exec::Source { language: Language::PowerShell, source, .. } => assert_eq!(source, k("Set-Content a.txt x")),
            _ => panic!("expected powershell source"),
        }
    }

    #[test]
    fn node_eval_binds_process_argv() {
        match classify("node", &args(&["-e", "x", "a.txt"]), &Stdin::None) {
            Exec::Source { language: Language::JavaScript, script_args, .. } => {
                assert_eq!(script_args, vec![Value::Unknown, k("a.txt")]);
            }
            _ => panic!("expected javascript source"),
        }
    }

    #[test]
    fn bun_eval_binds_process_argv_with_its_own_offset() {
        match classify("bun", &args(&["-e", "x", "a.txt"]), &Stdin::None) {
            Exec::Source { language: Language::TypeScript, script_args, .. } => {
                assert_eq!(script_args.last(), Some(&k("a.txt")));
            }
            _ => panic!("expected typescript source"),
        }
    }

    #[test]
    fn languages_without_an_adapter_are_unsupported() {
        for (name, a) in [("perl", vec!["-e", "print 1"]), ("ruby", vec!["-e", "1"]), ("nu", vec!["-c", "ls"]), ("awk", vec!["{print $1}", "f"])] {
            assert!(matches!(classify(name, &args(&a), &Stdin::None), Exec::Unsupported { .. }), "{name}");
        }
        assert!(matches!(classify("perl", &args(&["-pi", "-e", "s/a/b/", "f.txt"]), &Stdin::None), Exec::Plain));
    }

    #[test]
    fn source_from_an_unknown_producer_is_unknown() {
        let stdin = Stdin::Source { value: Value::Unknown, span: 0..0 };
        match classify("python3", &args(&["-"]), &stdin) {
            Exec::Source { source, .. } => assert_eq!(source, Value::Unknown),
            _ => panic!("expected python source"),
        }
    }

    #[test]
    fn nested_shell_source_is_analyzed_with_its_positional_arguments() {
        let a = bash("sh -c 'echo x > \"$1\"' sh out.txt");
        assert_eq!(targets(&a), ["/repo/out.txt"]);
        assert!(a.file_effects[0].location.embedded.is_some());
    }

    #[test]
    fn eval_of_known_words_is_bash_source() {
        assert_eq!(targets(&bash("eval 'echo x > e.txt'")), ["/repo/e.txt"]);
    }

    #[test]
    fn a_piped_script_of_unknown_content_is_an_unresolved_write() {
        let a = bash("curl -s https://example.invalid/x | python3 -");
        assert!(a.uncertainties.iter().any(|u| u.kind == UncertaintyKind::UnresolvedWrite), "{:?}", a.uncertainties);
    }

    #[test]
    fn nesting_past_the_depth_limit_is_reported() {
        let a = bash(&format!("{} echo hi > deep.txt", "eval ".repeat(12)));
        assert!(a
            .uncertainties
            .iter()
            .any(|u| u.kind == UncertaintyKind::LimitExhausted(crate::model::Limit::Depth)), "{:?}", a.uncertainties);
    }
}
```

- [ ] **Step 3: Run to see them fail**

Run: `cargo nextest run -p devkit-command embed`
Expected: FAIL; `classify` returns `Plain`.

- [ ] **Step 4: Implement**

Replace `crates/devkit-command/src/embed.rs` (keep tests):

```rust
//! Which invocations run source handed to them, in which language, with
//! which arguments, and which run a stored script instead.

use crate::{
    analyzer::Stdin,
    model::{Language, Value},
};

pub(crate) enum Exec {
    Source { language: Language, source: Value, script_args: Vec<Value> },
    ScriptFile { script: Value },
    Unsupported { language: &'static str },
    Plain,
}

fn program(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

fn is_python(name: &str) -> bool {
    name == "py"
        || name.strip_prefix("python").is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

pub(crate) fn is_interpreter(name: &str) -> bool {
    let name = program(name);
    is_python(&name)
        || matches!(
            name.as_str(),
            "node" | "bun" | "deno" | "tsx" | "ts-node" | "bash" | "sh" | "dash" | "ksh" | "zsh" | "fish" | "pwsh"
                | "powershell" | "eval" | "perl" | "ruby" | "php" | "lua" | "nu" | "awk" | "gawk" | "mawk" | "osascript"
                | "rscript" | "julia" | "cmd"
        )
}

fn stdin_source(stdin: &Stdin) -> Option<Value> {
    match stdin {
        Stdin::Source { value, .. } => Some(value.clone()),
        Stdin::None => None,
    }
}

pub(crate) fn classify(name: &str, args: &[Value], stdin: &Stdin) -> Exec {
    let name = program(name);
    match name.as_str() {
        n if is_python(n) => python(args, stdin),
        "node" => eval_style(args, stdin, Language::JavaScript, &["-e", "--eval", "-p", "--print"], &["-r", "--require", "--import", "--loader"], 1),
        "tsx" | "ts-node" => eval_style(args, stdin, Language::TypeScript, &["-e", "--eval", "-p", "--print"], &["-r", "--require"], 1),
        "bun" => eval_style(args, stdin, Language::TypeScript, &["-e", "--eval", "-p", "--print"], &["--cwd", "-r", "--preload"], 1),
        "deno" => match args.first().and_then(Value::known) {
            Some("eval") => Exec::Source { language: Language::TypeScript, source: args.get(1).cloned().unwrap_or(Value::Unknown), script_args: Vec::new() },
            Some("run") => args.iter().skip(1).find(|a| a.known().is_none_or(|t| !t.starts_with('-'))).map_or(Exec::Plain, |s| Exec::ScriptFile { script: s.clone() }),
            _ => Exec::Plain,
        },
        "bash" | "sh" | "dash" | "ksh" => shell(args, stdin, Language::Bash),
        "fish" => shell(args, stdin, Language::Fish),
        "zsh" => match shell(args, stdin, Language::Bash) {
            Exec::Source { .. } => Exec::Unsupported { language: "zsh" },
            other => other,
        },
        "pwsh" | "powershell" => powershell(args, stdin),
        "eval" => {
            let words: Option<Vec<&str>> = args.iter().map(Value::known).collect();
            Exec::Source {
                language: Language::Bash,
                source: words.map_or(Value::Unknown, |w| Value::Known(w.join(" "))),
                script_args: Vec::new(),
            }
        }
        "perl" => {
            let in_place = args.iter().filter_map(Value::known).take_while(|a| a.starts_with('-')).any(|a| !a.starts_with("--") && a[1..].contains('i'));
            if in_place { Exec::Plain } else { unsupported_or_script(args, "perl", &["-e", "-E"]) }
        }
        "ruby" => unsupported_or_script(args, "ruby", &["-e"]),
        "php" => unsupported_or_script(args, "php", &["-r"]),
        "lua" => unsupported_or_script(args, "lua", &["-e"]),
        "rscript" => unsupported_or_script(args, "R", &["-e"]),
        "julia" => unsupported_or_script(args, "julia", &["-e", "--eval"]),
        "osascript" => unsupported_or_script(args, "AppleScript", &["-e"]),
        "nu" => unsupported_or_script(args, "nushell", &["-c", "--commands"]),
        "cmd" => Exec::Unsupported { language: "cmd" },
        "awk" | "gawk" | "mawk" => {
            let mut words = args.iter();
            while let Some(w) = words.next() {
                match w.known() {
                    Some("-f") => return words.next().map_or(Exec::Plain, |s| Exec::ScriptFile { script: s.clone() }),
                    Some("-v" | "-F") => {
                        words.next();
                    }
                    Some(t) if t.starts_with('-') => {}
                    _ => return Exec::Unsupported { language: "awk" },
                }
            }
            Exec::Plain
        }
        _ => Exec::Plain,
    }
}

fn python(args: &[Value], stdin: &Stdin) -> Exec {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            return Exec::ScriptFile { script: Value::Unknown };
        };
        match text {
            "-c" => {
                return Exec::Source {
                    language: Language::Python,
                    source: args.get(i + 1).cloned().unwrap_or(Value::Unknown),
                    script_args: std::iter::once(Value::Known("-c".into())).chain(args.iter().skip(i + 2).cloned()).collect(),
                };
            }
            "-m" => return Exec::Plain,
            "-" => {
                return Exec::Source {
                    language: Language::Python,
                    source: stdin_source(stdin).unwrap_or(Value::Unknown),
                    script_args: args[i..].to_vec(),
                };
            }
            "-W" | "-X" | "--check-hash-based-pycs" => i += 2,
            t if t.starts_with('-') => i += 1,
            _ => return Exec::ScriptFile { script: arg.clone() },
        }
    }
    match stdin_source(stdin) {
        Some(source) => Exec::Source { language: Language::Python, source, script_args: vec![Value::Known(String::new())] },
        None => Exec::Plain,
    }
}

/// `node`, `bun`, `tsx`: an evaluation flag carries source; the words after it
/// are the script's arguments, preceded by `offset` runtime entries of
/// unknown value in `process.argv`.
fn eval_style(args: &[Value], stdin: &Stdin, language: Language, eval_flags: &[&str], value_flags: &[&str], offset: usize) -> Exec {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            return Exec::ScriptFile { script: Value::Unknown };
        };
        if eval_flags.contains(&text) {
            return Exec::Source {
                language,
                source: args.get(i + 1).cloned().unwrap_or(Value::Unknown),
                script_args: std::iter::repeat_n(Value::Unknown, offset).chain(args.iter().skip(i + 2).cloned()).collect(),
            };
        }
        match text {
            "-" => {
                return Exec::Source {
                    language,
                    source: stdin_source(stdin).unwrap_or(Value::Unknown),
                    script_args: std::iter::repeat_n(Value::Unknown, offset).chain(args.iter().skip(i + 1).cloned()).collect(),
                };
            }
            t if value_flags.contains(&t) => i += 2,
            t if t.starts_with('-') => i += 1,
            t if has_script_extension(t) => return Exec::ScriptFile { script: arg.clone() },
            _ => return Exec::Plain,
        }
    }
    Exec::Plain
}

fn has_script_extension(word: &str) -> bool {
    [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts", ".tsx", ".jsx"].iter().any(|e| word.ends_with(e))
}

fn shell(args: &[Value], stdin: &Stdin, language: Language) -> Exec {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            return Exec::ScriptFile { script: Value::Unknown };
        };
        if text.starts_with('-') && !text.starts_with("--") && text[1..].contains('c') {
            return Exec::Source {
                language,
                source: args.get(i + 1).cloned().unwrap_or(Value::Unknown),
                script_args: args.iter().skip(i + 2).cloned().collect(),
            };
        }
        match text {
            "-s" => {
                return Exec::Source { language, source: stdin_source(stdin).unwrap_or(Value::Unknown), script_args: args[i + 1..].to_vec() };
            }
            "-o" | "+o" | "--init-file" | "--rcfile" => i += 2,
            t if t.starts_with('-') || t.starts_with('+') => i += 1,
            _ => return Exec::ScriptFile { script: arg.clone() },
        }
    }
    match stdin_source(stdin) {
        Some(source) => Exec::Source { language, source, script_args: Vec::new() },
        None => Exec::Plain,
    }
}

/// PowerShell parameters are case-insensitive and accept unambiguous
/// prefixes; only the ones that change how source is supplied are matched.
fn powershell(args: &[Value], stdin: &Stdin) -> Exec {
    const VALUE_PARAMS: &[&str] = &["-executionpolicy", "-ep", "-workingdirectory", "-wd", "-outputformat", "-of", "-inputformat", "-if", "-windowstyle", "-w", "-configurationname", "-settingsfile"];
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            return Exec::Source { language: Language::PowerShell, source: Value::Unknown, script_args: Vec::new() };
        };
        let lower = text.to_ascii_lowercase();
        if lower == "-c" || (lower.len() >= 2 && "-command".starts_with(&lower) && lower.len() > 2) {
            let rest = &args[i + 1..];
            if rest.first().and_then(Value::known) == Some("-") {
                return Exec::Source { language: Language::PowerShell, source: stdin_source(stdin).unwrap_or(Value::Unknown), script_args: Vec::new() };
            }
            let words: Option<Vec<&str>> = rest.iter().map(Value::known).collect();
            return Exec::Source { language: Language::PowerShell, source: words.map_or(Value::Unknown, |w| Value::Known(w.join(" "))), script_args: Vec::new() };
        }
        if lower == "-f" || (lower.len() > 2 && "-file".starts_with(&lower)) {
            return Exec::ScriptFile { script: args.get(i + 1).cloned().unwrap_or(Value::Unknown) };
        }
        if lower == "-e" || lower == "-ec" || (lower.len() > 3 && "-encodedcommand".starts_with(&lower)) {
            return Exec::Source { language: Language::PowerShell, source: Value::Unknown, script_args: Vec::new() };
        }
        if VALUE_PARAMS.contains(&lower.as_str()) {
            i += 2;
        } else if lower.starts_with('-') {
            i += 1;
        } else {
            return Exec::ScriptFile { script: arg.clone() };
        }
    }
    Exec::Plain
}

fn unsupported_or_script(args: &[Value], language: &'static str, eval_flags: &[&str]) -> Exec {
    for arg in args {
        match arg.known() {
            Some(t) if eval_flags.contains(&t) => return Exec::Unsupported { language },
            Some(t) if t.starts_with('-') => {}
            Some(_) | None => return Exec::ScriptFile { script: arg.clone() },
        }
    }
    Exec::Plain
}
```

`pwsh -Command` source uses the `-c` spelling for `-Command` only after `-c` fails to match `-configurationname`'s prefix; that case is covered because `-c` is tested first as an exact word.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): extract embedded source and its arguments"
```

---

### Task 6: The effect catalog

**Files:**
- Modify: `crates/devkit-command/src/catalog.rs`

**Interfaces:**
- Consumes: `FileOp`, `Value`.
- Produces: `catalog::effects(name: &str, args: &[Value]) -> Vec<Hit>` with real behaviour; `catalog::is_cataloged(&str) -> bool`. `args` are `semantic_args`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{bash, targets};

    fn k(s: &str) -> Value {
        Value::Known(s.into())
    }

    fn hits(name: &str, words: &[&str]) -> Vec<String> {
        let args: Vec<Value> = words.iter().map(|w| k(w)).collect();
        effects(name, &args)
            .into_iter()
            .map(|h| match h {
                Hit::File(op, v) => format!("{op:?} {}", v.known().unwrap_or("?")),
                Hit::Tree { scope, whole_checkout, by } => format!("Tree {} {whole_checkout} {by}", scope.known().unwrap_or("?")),
                Hit::Unresolved(_) => "Unresolved".into(),
                Hit::ScriptFile(v) => format!("Script {}", v.known().unwrap_or("?")),
            })
            .collect()
    }

    #[test]
    fn file_management_commands() {
        assert_eq!(hits("tee", &["-a", "log.txt"]), ["Append log.txt"]);
        assert_eq!(hits("touch", &["a", "b"]), ["Create a", "Create b"]);
        assert_eq!(hits("rm", &["-f", "a"]), ["Delete a"]);
        assert_eq!(hits("rm", &["-rf", "build"]), ["Tree build false rm -r"]);
        assert_eq!(hits("cp", &["a", "b"]), ["Copy b"]);
        assert_eq!(hits("cp", &["-r", "a", "b"]), ["Tree b false cp -r"]);
        assert_eq!(hits("mv", &["a", "b"]), ["Rename a", "Rename b"]);
        assert_eq!(hits("mv", &["-t", "dir", "a"]), ["Rename a", "Rename dir/a"]);
        assert_eq!(hits("dd", &["if=/dev/zero", "of=img", "bs=1M"]), ["Overwrite img"]);
        assert!(hits("mkdir", &["-p", "src"]).is_empty());
        assert!(hits("cat", &["a"]).is_empty());
    }

    #[test]
    fn in_place_editors_claim_their_file_operands() {
        assert_eq!(hits("sed", &["-i", "s/a/b/", "x.rs"]), ["Overwrite x.rs"]);
        assert_eq!(hits("sed", &["-i.bak", "-e", "s/a/b/", "x.rs", "y.rs"]), ["Overwrite x.rs", "Overwrite y.rs"]);
        assert!(hits("sed", &["s/a/b/", "x.rs"]).is_empty());
        assert_eq!(hits("perl", &["-pi", "-e", "s/a/b/", "x.rs"]), ["Overwrite x.rs"]);
        assert_eq!(hits("perl", &["-i", "-pe", "s/a/b/", "x.rs"]), ["Overwrite x.rs"]);
    }

    #[test]
    fn git_verbs_that_rewrite_the_tree() {
        assert_eq!(hits("git", &["checkout", "main"]), ["Tree . true git checkout"]);
        assert_eq!(hits("git", &["checkout", "--", "a.rs"]), ["Overwrite a.rs"]);
        assert_eq!(hits("git", &["restore", "a.rs"]), ["Overwrite a.rs"]);
        assert_eq!(hits("git", &["stash"]), ["Tree . true git stash"]);
        assert!(hits("git", &["stash", "list"]).is_empty());
        assert_eq!(hits("git", &["reset", "--hard", "HEAD"]), ["Tree . true git reset --hard"]);
        assert!(hits("git", &["reset", "HEAD~1"]).is_empty());
        assert_eq!(hits("git", &["mv", "a", "b"]), ["Rename a", "Rename b"]);
        assert!(hits("git", &["status"]).is_empty());
        assert!(hits("git", &["worktree", "list"]).is_empty());
    }

    #[test]
    fn formatters_write_their_operands_or_the_tree() {
        assert_eq!(hits("cargo", &["fmt"]), ["Tree . true cargo fmt"]);
        assert!(hits("cargo", &["fmt", "--check"]).is_empty());
        assert_eq!(hits("prettier", &["--write", "src/a.ts"]), ["Overwrite src/a.ts"]);
        assert_eq!(hits("prettier", &["--write", "."]), ["Tree . false prettier"]);
        assert!(hits("prettier", &["--check", "."]).is_empty());
        assert_eq!(hits("ruff", &["format"]), ["Tree . false ruff"]);
        assert!(hits("ruff", &["check", "src"]).is_empty());
        assert_eq!(hits("ruff", &["check", "--fix", "a.py"]), ["Overwrite a.py"]);
    }

    #[test]
    fn an_unknown_operand_is_unresolved() {
        let args = vec![k("-f"), Value::Unknown];
        assert!(matches!(effects("rm", &args)[..], [Hit::Unresolved(_)]));
    }

    #[test]
    fn source_and_dot_run_a_script_file() {
        assert_eq!(hits("source", &["env.sh"]), ["Script env.sh"]);
        assert_eq!(hits(".", &["env.sh"]), ["Script env.sh"]);
    }

    #[test]
    fn git_c_scopes_its_effects() {
        let a = bash("git -C sub checkout -- a.rs");
        assert_eq!(targets(&a), ["/repo/sub/a.rs"]);
        let b = bash("git -C \"$d\" checkout main");
        assert!(b.tree_effects.is_empty());
        assert!(!b.uncertainties.is_empty());
    }

    #[test]
    fn an_outer_redirect_around_a_devkit_command_is_still_a_write() {
        assert_eq!(targets(&bash("devrun task check > shared.txt")), ["/repo/shared.txt"]);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run -p devkit-command catalog`
Expected: FAIL.

- [ ] **Step 3: Implement**

Replace `crates/devkit-command/src/catalog.rs` (keep tests):

```rust
//! Built-in effects of the programs devkit models. A program outside this
//! table produces no file effect: an unmodeled program is one devkit has no
//! opinion about. The table grows by pull request, never by config.

use crate::model::{FileOp, Value};

pub(crate) enum Hit {
    File(FileOp, Value),
    Tree { scope: Value, whole_checkout: bool, by: String },
    Unresolved(String),
    ScriptFile(Value),
}

const PROGRAMS: &[&str] = &[
    "tee", "touch", "truncate", "dd", "rm", "unlink", "shred", "cp", "install", "mv", "ln", "sed", "perl", "git", "cargo",
    "rustfmt", "prettier", "biome", "eslint", "ruff", "black", "isort", "taplo", "dprint", "deno", "gofmt", "goimports",
    "clang-format", "shfmt", "stylua", "source", ".", "patch", "curl", "wget", "tar", "unzip",
];

pub(crate) fn is_cataloged(name: &str) -> bool {
    PROGRAMS.contains(&name)
}

struct Parsed {
    operands: Vec<Value>,
    flags: Vec<String>,
    values: Vec<(String, Value)>,
}

impl Parsed {
    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    /// A single-dash cluster containing `c`, such as `-rf` for `r`.
    fn has_short(&self, c: char) -> bool {
        self.flags.iter().any(|f| f.starts_with('-') && !f.starts_with("--") && f[1..].contains(c))
    }

    fn value(&self, names: &[&str]) -> Option<&Value> {
        self.values.iter().find(|(n, _)| names.contains(&n.as_str())).map(|(_, v)| v)
    }
}

fn parse(args: &[Value], value_flags: &[&str]) -> Parsed {
    let mut p = Parsed { operands: Vec::new(), flags: Vec::new(), values: Vec::new() };
    let mut i = 0;
    let mut options_done = false;
    while let Some(arg) = args.get(i) {
        match arg.known() {
            Some("--") if !options_done => options_done = true,
            Some(t) if !options_done && t.starts_with('-') && t.len() > 1 => match t.split_once('=') {
                Some((flag, v)) if flag.starts_with("--") => p.values.push((flag.to_string(), Value::Known(v.to_string()))),
                _ if value_flags.contains(&t) => {
                    i += 1;
                    p.values.push((t.to_string(), args.get(i).cloned().unwrap_or(Value::Unknown)));
                }
                _ => p.flags.push(t.to_string()),
            },
            _ => p.operands.push(arg.clone()),
        }
        i += 1;
    }
    p
}

fn each(op: FileOp, operands: &[Value]) -> Vec<Hit> {
    operands.iter().map(|v| file(op, v)).collect()
}

fn file(op: FileOp, v: &Value) -> Hit {
    match v {
        Value::Known(_) => Hit::File(op, v.clone()),
        Value::Unknown => Hit::Unresolved(format!("a {op:?} target could not be determined")),
    }
}

fn tree(scope: &Value, whole_checkout: bool, by: &str) -> Hit {
    Hit::Tree { scope: scope.clone(), whole_checkout, by: by.to_string() }
}

fn join_name(dir: &Value, source: &Value) -> Value {
    match (dir.known(), source.known()) {
        (Some(d), Some(s)) => Value::Known(format!("{}/{}", d.trim_end_matches('/'), crate::normalize::basename(s))),
        _ => Value::Unknown,
    }
}

/// A formatter operand: a path with an extension is a file, anything else a
/// directory it rewrites.
fn formatter_operands(operands: &[Value], by: &str) -> Vec<Hit> {
    if operands.is_empty() {
        return vec![tree(&Value::Known(".".into()), false, by)];
    }
    operands
        .iter()
        .map(|v| match v.known() {
            Some(p) if crate::normalize::basename(p).contains('.') && !p.ends_with('.') => Hit::File(FileOp::Overwrite, v.clone()),
            Some(_) => tree(v, false, by),
            None => Hit::Unresolved(format!("`{by}` rewrites a path that could not be determined")),
        })
        .collect()
}

pub(crate) fn effects(name: &str, args: &[Value]) -> Vec<Hit> {
    match name {
        "tee" => {
            let p = parse(args, &[]);
            each(if p.has("-a") || p.has("--append") { FileOp::Append } else { FileOp::Overwrite }, &p.operands)
        }
        "touch" => each(FileOp::Create, &parse(args, &["-r", "-d", "-t", "--reference", "--date"]).operands),
        "truncate" => each(FileOp::Overwrite, &parse(args, &["-s", "--size", "-r", "--reference"]).operands),
        "dd" => args
            .iter()
            .filter_map(|a| match a.known() {
                Some(t) => t.strip_prefix("of=").map(|p| Hit::File(FileOp::Overwrite, Value::Known(p.to_string()))),
                None => Some(Hit::Unresolved("a `dd` operand could not be determined".into())),
            })
            .collect(),
        "rm" | "unlink" | "shred" => {
            let p = parse(args, &[]);
            if p.has_short('r') || p.has_short('R') || p.has("--recursive") {
                p.operands.iter().map(|v| match v {
                    Value::Known(_) => tree(v, false, "rm -r"),
                    Value::Unknown => Hit::Unresolved("`rm -r` removes a path that could not be determined".into()),
                }).collect()
            } else {
                each(FileOp::Delete, &p.operands)
            }
        }
        "cp" | "install" => {
            let p = parse(args, &["-t", "--target-directory", "-S", "--suffix", "-m", "--mode", "-o", "--owner", "-g", "--group"]);
            if name == "install" && p.has("-d") {
                return Vec::new();
            }
            let recursive = p.has_short('r') || p.has_short('R') || p.has_short('a') || p.has("--recursive");
            if let Some(dir) = p.value(&["-t", "--target-directory"]) {
                return p.operands.iter().map(|s| file(FileOp::Copy, &join_name(dir, s))).collect();
            }
            match p.operands.split_last() {
                Some((dest, sources)) if !sources.is_empty() => {
                    if recursive {
                        vec![match dest { Value::Known(_) => tree(dest, false, "cp -r"), Value::Unknown => Hit::Unresolved("`cp -r` destination could not be determined".into()) }]
                    } else {
                        vec![file(FileOp::Copy, dest)]
                    }
                }
                _ => Vec::new(),
            }
        }
        "mv" => {
            let p = parse(args, &["-t", "--target-directory", "-S", "--suffix"]);
            if let Some(dir) = p.value(&["-t", "--target-directory"]) {
                return p.operands.iter().flat_map(|s| [file(FileOp::Rename, s), file(FileOp::Rename, &join_name(dir, s))]).collect();
            }
            match p.operands.split_last() {
                Some((dest, sources)) if !sources.is_empty() => {
                    sources.iter().map(|s| file(FileOp::Rename, s)).chain([file(FileOp::Rename, dest)]).collect()
                }
                _ => Vec::new(),
            }
        }
        "ln" => {
            let p = parse(args, &["-t", "--target-directory", "-S", "--suffix"]);
            match p.operands.as_slice() {
                [_, .., link] => vec![file(FileOp::Create, link)],
                _ => Vec::new(),
            }
        }
        "sed" => {
            let p = parse(args, &["-e", "--expression", "-f", "--file", "-l", "--line-length"]);
            let in_place = p.has("--in-place")
                || p.values.iter().any(|(f, _)| f == "--in-place")
                || p.flags.iter().any(|f| f.starts_with("-i") || (f.starts_with('-') && !f.starts_with("--") && f[1..].contains('i')));
            if !in_place {
                return Vec::new();
            }
            let files = if p.value(&["-e", "--expression", "-f", "--file"]).is_some() { &p.operands[..] } else { p.operands.get(1..).unwrap_or(&[]) };
            each(FileOp::Overwrite, files)
        }
        "perl" => {
            let mut in_place = false;
            let mut code_given = false;
            let mut files = Vec::new();
            let mut i = 0;
            while let Some(arg) = args.get(i) {
                match arg.known() {
                    Some(t) if t.starts_with('-') && !t.starts_with("--") && t.len() > 1 => {
                        in_place |= t[1..].contains('i');
                        if t.ends_with('e') || t.ends_with('E') {
                            code_given = true;
                            i += 1;
                        }
                    }
                    _ => files.push(arg.clone()),
                }
                i += 1;
            }
            if !in_place {
                return Vec::new();
            }
            let files = if code_given { &files[..] } else { files.get(1..).unwrap_or(&[]) };
            each(FileOp::Overwrite, files)
        }
        "git" => git(args),
        "cargo" => match args.first().and_then(Value::known) {
            Some("fmt") if !args.iter().any(|a| a.known() == Some("--check")) => vec![tree(&Value::Known(".".into()), true, "cargo fmt")],
            Some("clippy") if args.iter().any(|a| a.known() == Some("--fix")) => vec![tree(&Value::Known(".".into()), true, "cargo clippy --fix")],
            Some("fix") => vec![tree(&Value::Known(".".into()), true, "cargo fix")],
            _ => Vec::new(),
        },
        "rustfmt" => {
            let p = parse(args, &["--edition", "--config-path", "--config"]);
            if p.has("--check") { Vec::new() } else { each(FileOp::Overwrite, &p.operands) }
        }
        "prettier" | "eslint" | "gofmt" | "goimports" | "clang-format" | "shfmt" => {
            let write_flags: &[&str] = match name {
                "prettier" => &["--write", "-w"],
                "eslint" => &["--fix"],
                "clang-format" => &["-i"],
                _ => &["-w"],
            };
            let p = parse(args, &["--config", "-c", "--ignore-path", "--plugin", "--parser", "--style", "-i"]);
            let writes = write_flags.iter().any(|f| p.has(f)) || (name == "clang-format" && p.values.iter().any(|(f, _)| f == "-i"));
            if !writes {
                return Vec::new();
            }
            let mut operands = p.operands.clone();
            if name == "clang-format"
                && let Some((_, v)) = p.values.iter().find(|(f, _)| f == "-i")
            {
                operands.insert(0, v.clone());
            }
            formatter_operands(&operands, name)
        }
        "ruff" | "biome" | "taplo" | "dprint" | "deno" => {
            let Some(sub) = args.first().and_then(Value::known) else { return Vec::new() };
            let rest = parse(&args[1..], &["--config", "--line-length", "--target-version", "--select", "--ignore"]);
            let checks = rest.has("--check") || rest.has("--diff");
            let writes = match (name, sub) {
                ("ruff", "format") | ("taplo", "fmt" | "format") | ("deno", "fmt") | ("dprint", "fmt") => !checks,
                ("ruff", "check") => rest.has("--fix"),
                ("biome", "format" | "check" | "lint") => rest.has("--write") || rest.has("--apply") || rest.has("--fix"),
                _ => false,
            };
            if writes { formatter_operands(&rest.operands, name) } else { Vec::new() }
        }
        "black" | "isort" | "stylua" => {
            let p = parse(args, &["--config", "-l", "--line-length", "--settings-path"]);
            if p.has("--check") || p.has("--diff") { Vec::new() } else { formatter_operands(&p.operands, name) }
        }
        "source" | "." => args.first().map(|s| vec![Hit::ScriptFile(s.clone())]).unwrap_or_default(),
        "patch" => {
            let p = parse(args, &["-i", "--input", "-o", "--output", "-d", "--directory", "-p", "-D", "-B", "-z"]);
            if let Some(out) = p.value(&["-o", "--output"]) {
                return vec![file(FileOp::Overwrite, out)];
            }
            match p.operands.first() {
                Some(original) => vec![file(FileOp::Overwrite, original)],
                None => vec![tree(p.value(&["-d", "--directory"]).unwrap_or(&Value::Known(".".into())), false, "patch")],
            }
        }
        "curl" => {
            let p = parse(args, &["-o", "--output", "-X", "-H", "-d", "--data", "-u", "-A", "-e", "--url"]);
            if let Some(out) = p.value(&["-o", "--output"]) {
                if out.known() == Some("-") { return Vec::new(); }
                return vec![file(FileOp::Overwrite, out)];
            }
            if p.has("-O") || p.has("--remote-name") || p.has_short('O') {
                return vec![Hit::Unresolved("`curl -O` names its file from the URL".into())];
            }
            Vec::new()
        }
        "wget" => {
            let p = parse(args, &["-O", "--output-document", "-P", "--directory-prefix", "-o", "-a"]);
            match p.value(&["-O", "--output-document"]) {
                Some(out) if out.known() == Some("-") => Vec::new(),
                Some(out) => vec![file(FileOp::Overwrite, out)],
                None if p.operands.is_empty() => Vec::new(),
                None => vec![Hit::Unresolved("`wget` names its file from the URL".into())],
            }
        }
        "tar" => {
            let first = args.first().and_then(Value::known).unwrap_or("");
            let p = parse(args, &["-C", "--directory", "-f", "--file", "-T", "-X"]);
            let bundled = !first.starts_with('-');
            let extract = p.has("--extract") || p.has("--get") || p.has_short('x') || (bundled && first.contains('x'));
            let create = p.has("--create") || p.has_short('c') || (bundled && first.contains('c'));
            if extract {
                vec![tree(p.value(&["-C", "--directory"]).unwrap_or(&Value::Known(".".into())), false, "tar -x")]
            } else if create {
                p.value(&["-f", "--file"]).map(|f| vec![file(FileOp::Overwrite, f)]).unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        "unzip" => {
            let p = parse(args, &["-d", "-x"]);
            if p.has("-l") || p.has("-t") { Vec::new() } else { vec![tree(p.value(&["-d"]).unwrap_or(&Value::Known(".".into())), false, "unzip")] }
        }
        _ => Vec::new(),
    }
}

fn git(args: &[Value]) -> Vec<Hit> {
    let Some(sub) = args.first().and_then(Value::known) else { return Vec::new() };
    let rest = &args[1..];
    let whole = |by: &str| vec![tree(&Value::Known(".".into()), true, by)];
    let has = |flag: &str| rest.iter().any(|a| a.known() == Some(flag));
    match sub {
        "checkout" => match rest.iter().position(|a| a.known() == Some("--")) {
            Some(sep) => each(FileOp::Overwrite, &rest[sep + 1..]),
            None => whole("git checkout"),
        },
        "restore" => {
            let p = parse(rest, &["-s", "--source"]);
            if p.has("--staged") && !p.has("--worktree") && !p.has("-W") { Vec::new() } else { each(FileOp::Overwrite, &p.operands) }
        }
        "switch" | "merge" | "rebase" | "cherry-pick" | "revert" | "pull" | "am" | "clean" => {
            if has("--abort") && sub != "merge" && sub != "rebase" { return Vec::new(); }
            if sub == "clean" && (has("-n") || has("--dry-run")) { return Vec::new(); }
            whole(&format!("git {sub}"))
        }
        "apply" => if has("--cached") || has("--check") || has("--stat") || has("--numstat") { Vec::new() } else { whole("git apply") },
        "stash" => match rest.first().and_then(Value::known) {
            None | Some("push" | "save" | "pop" | "apply" | "branch") => whole("git stash"),
            Some(flag) if flag.starts_with('-') => whole("git stash"),
            _ => Vec::new(),
        },
        "reset" => {
            if has("--hard") { whole("git reset --hard") } else if has("--merge") || has("--keep") { whole("git reset") } else { Vec::new() }
        }
        "mv" => {
            let p = parse(rest, &[]);
            match p.operands.split_last() {
                Some((dest, sources)) if !sources.is_empty() => sources.iter().map(|s| file(FileOp::Rename, s)).chain([file(FileOp::Rename, dest)]).collect(),
                _ => Vec::new(),
            }
        }
        "rm" => {
            let p = parse(rest, &[]);
            if p.has("--cached") {
                Vec::new()
            } else if p.has_short('r') {
                p.operands.iter().map(|v| match v { Value::Known(_) => tree(v, false, "git rm -r"), Value::Unknown => Hit::Unresolved("`git rm -r` path could not be determined".into()) }).collect()
            } else {
                each(FileOp::Delete, &p.operands)
            }
        }
        _ => Vec::new(),
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): catalog file and tree effects"
```

---

### Task 7: Python adapter

**Files:**
- Modify: `crates/devkit-command/src/python.rs`

**Interfaces:**
- Consumes: `Analyzer::{invocation, file_effect, tree_effect, uncertain, source}`, `Frame.script_args` (index `i` is `sys.argv[i]`), `RawInvocation`, `Word`, `ts::*`.
- Produces: `python::walk(&mut Analyzer, &str, &Frame)`.

- [ ] **Step 1: Write the failing tests**

Every test goes through the real outer form, a bash command running Python, so extraction, argument binding, and the adapter are exercised together.

```rust
#[cfg(test)]
mod tests {
    use crate::{
        model::{FileOp, UncertaintyKind},
        testutil::{bash, targets},
        Analysis,
    };

    fn py(source: &str) -> Analysis {
        bash(&format!("python3 - <<'PY'\n{source}\nPY\n"))
    }

    fn unresolved(a: &Analysis) -> bool {
        a.uncertainties.iter().any(|u| u.kind == UncertaintyKind::UnresolvedWrite) || targets(a).contains(&"?".to_string())
    }

    #[test]
    fn pathlib_writes_a_literal_path() {
        let a = py("from pathlib import Path\nPath('src/a.ts').write_text('x')");
        assert_eq!(targets(&a), ["/repo/src/a.ts"]);
        assert!(a.file_effects[0].location.embedded.is_some());
    }

    #[test]
    fn open_modes_decide_the_operation() {
        let a = py("open('a.txt', 'w')\nopen('b.txt', 'a')\nopen('c.txt')\nwith open('d.txt', mode='x') as f:\n    f.write('1')");
        assert_eq!(targets(&a), ["/repo/a.txt", "/repo/b.txt", "/repo/d.txt"]);
        let ops: Vec<FileOp> = a.file_effects.iter().map(|e| e.op).collect();
        assert_eq!(ops, [FileOp::Overwrite, FileOp::Append, FileOp::Create]);
    }

    #[test]
    fn an_unknown_mode_is_unresolved() {
        assert!(unresolved(&py("import sys\nopen('a.txt', sys.stdin.read())")));
    }

    #[test]
    fn aliases_and_path_composition_resolve() {
        assert_eq!(targets(&py("import pathlib as pl\nroot = pl.Path('src')\n(root / 'b.rs').write_text('')")), ["/repo/src/b.rs"]);
        assert_eq!(
            targets(&py("import os\nname = 'c'\nopen(os.path.join('gen', f'{name}.txt'), 'w')")),
            ["/repo/gen/c.txt"]
        );
    }

    #[test]
    fn a_shell_variable_reaches_sys_argv() {
        let a = bash("f=src/a.ts; python3 - \"$f\" <<'PY'\nimport sys\nfrom pathlib import Path\np = Path(sys.argv[1])\np.write_text('x')\nPY\n");
        assert_eq!(targets(&a), ["/repo/src/a.ts"]);
        let c = bash("python3 -c \"import sys; open(sys.argv[1], 'w')\" out.txt");
        assert_eq!(targets(&c), ["/repo/out.txt"]);
    }

    #[test]
    fn argv_that_cannot_be_bound_is_unresolved() {
        assert!(unresolved(&bash("for f in $(ls); do python3 - \"$f\" <<'PY'\nimport sys\nopen(sys.argv[1], 'w')\nPY\ndone")));
        assert!(unresolved(&bash("python3 - a.txt <<'PY'\nimport sys\nopen(sys.argv[2], 'w')\nPY\n")));
        assert!(unresolved(&bash("python3 - a.txt <<'PY'\nimport sys\nsys.argv = ['x', 'b.txt']\nopen(sys.argv[1], 'w')\nPY\n")));
    }

    #[test]
    fn rebinding_a_name_drops_what_it_meant() {
        let a = py("def open(p, m):\n    pass\nopen('a.txt', 'w')");
        assert!(a.file_effects.is_empty() && a.uncertainties.is_empty(), "{a:?}");
    }

    #[test]
    fn a_function_body_runs_only_when_called() {
        assert!(py("def f():\n    open('a.txt', 'w')\n").file_effects.is_empty());
        assert_eq!(targets(&py("def f():\n    open('a.txt', 'w')\nf()")), ["/repo/a.txt"]);
        assert_eq!(targets(&py("def f(x=open('d.txt', 'w')):\n    pass")), ["/repo/d.txt"]);
    }

    #[test]
    fn subprocess_commands_are_analyzed_when_constant() {
        assert_eq!(targets(&py("import subprocess\nsubprocess.run(['rm', 'a.txt'])")), ["/repo/a.txt"]);
        assert_eq!(targets(&py("import subprocess\nsubprocess.run('echo x > s.txt', shell=True)")), ["/repo/s.txt"]);
        assert!(unresolved(&py("import subprocess, sys\nsubprocess.run(sys.stdin.read(), shell=True)")));
    }

    #[test]
    fn a_call_into_an_unmodeled_module_is_unresolved() {
        assert!(unresolved(&py("import pandas as pd\npd.DataFrame().to_csv('o.csv')")));
        assert!(unresolved(&py("undefined_helper('a.txt')")));
    }

    #[test]
    fn a_read_only_script_is_silent() {
        let a = py("import json, sys\nfrom pathlib import Path\ndata = json.loads(Path('a.json').read_text())\nprint(len(data), sorted(data))\nfor k in data:\n    print(k.upper())");
        assert!(a.file_effects.is_empty(), "{:?}", a.file_effects);
        assert!(a.uncertainties.is_empty(), "{:?}", a.uncertainties);
    }

    #[test]
    fn recursive_removal_is_a_tree_effect() {
        let a = py("import shutil\nshutil.rmtree('build')");
        assert_eq!(a.tree_effects[0].scope, "/repo/build");
    }

    #[test]
    fn a_loop_over_a_literal_list_runs_per_element() {
        assert_eq!(
            targets(&py("from pathlib import Path\nfor n in ['a.txt', 'b.txt']:\n    Path(n).write_text('')")),
            ["/repo/a.txt", "/repo/b.txt"]
        );
    }

    #[test]
    fn exec_of_a_constant_is_python_source_and_chdir_moves_the_cwd() {
        assert_eq!(targets(&py("exec(\"open('e.txt', 'w')\")")), ["/repo/e.txt"]);
        assert_eq!(targets(&py("import os\nos.chdir('sub')\nopen('a.txt', 'w')")), ["/repo/sub/a.txt"]);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run -p devkit-command python`
Expected: FAIL; `walk` is a no-op.

- [ ] **Step 3: Implement**

`crates/devkit-command/src/python.rs` (keep tests):

```rust
//! Python: bindings, path construction, and the filesystem, process and
//! dynamic-execution calls that can write. A receiver's name alone never
//! establishes its type; knowledge comes from imports and assignments, and a
//! rebinding drops it.

use std::{collections::HashMap, ops::Range};

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    model::{FileOp, Language, Location, UncertaintyKind, Value},
    normalize, paths, ts,
};

const MAX_LOOP_ITEMS: usize = 32;

/// Modules the adapter either models or knows to be free of file writes.
const KNOWN_MODULES: &[&str] = &[
    "pathlib", "os", "os.path", "shutil", "sys", "subprocess", "io", "codecs", "builtins", "importlib", "json", "re",
    "math", "collections", "itertools", "functools", "typing", "dataclasses", "datetime", "time", "textwrap", "pprint",
    "string", "hashlib", "base64", "difflib", "statistics", "random", "uuid", "enum", "ast", "tomllib", "csv", "glob",
    "fnmatch", "argparse", "copy", "operator", "decimal", "fractions", "heapq", "bisect", "shlex", "contextlib", "types",
    "inspect", "platform", "locale", "unicodedata", "struct", "html", "urllib.parse", "configparser", "traceback",
    "warnings", "keyword", "tokenize", "calendar", "zoneinfo",
];

const BUILTINS: &[&str] = &[
    "print", "len", "sorted", "reversed", "enumerate", "zip", "range", "map", "filter", "min", "max", "sum", "any", "all",
    "abs", "round", "isinstance", "issubclass", "hasattr", "repr", "str", "int", "float", "bool", "list", "dict", "set",
    "tuple", "frozenset", "bytes", "bytearray", "type", "id", "hash", "iter", "next", "chr", "ord", "hex", "oct", "bin",
    "format", "vars", "dir", "divmod", "pow", "input", "object", "super", "property", "staticmethod", "classmethod",
    "slice", "callable", "ascii", "memoryview", "Exception", "ValueError", "KeyError", "TypeError", "RuntimeError",
    "SystemExit", "OSError", "FileNotFoundError", "StopIteration", "NotImplementedError", "AssertionError",
];

/// Method names that write when their receiver is a path or file-like object
/// of a type the adapter could not establish.
const WRITE_METHODS: &[&str] = &[
    "write_text", "write_bytes", "touch", "unlink", "rename", "replace", "symlink_to", "hardlink_to", "to_csv", "to_json",
    "to_parquet", "to_excel", "to_pickle", "savefig", "save",
];

#[derive(Debug, Clone, PartialEq)]
enum Py {
    Str(String),
    Path(String),
    Int(i64),
    List(Vec<Py>),
    /// A modeled module or callable, by qualified name: `pathlib.Path`.
    Api(String),
    Method(Box<Py>, String),
    Argv,
    WriteHandle,
    Foreign(String),
    Def(Range<usize>),
    Data,
    Unknown,
}

impl Py {
    fn as_value(&self) -> Value {
        match self {
            Py::Str(s) | Py::Path(s) => Value::Known(s.clone()),
            _ => Value::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Scope {
    names: HashMap<String, Py>,
    cwd: Option<String>,
    argv: Option<Vec<Value>>,
}

impl Scope {
    fn merge_uncertain(&mut self, branch: &Scope) {
        for (name, value) in &branch.names {
            if self.names.get(name) != Some(value) {
                self.names.insert(name.clone(), Py::Unknown);
            }
        }
        if branch.cwd != self.cwd {
            self.cwd = None;
        }
        if branch.argv != self.argv {
            self.argv = None;
        }
    }
}

pub(crate) fn walk(a: &mut Analyzer<'_>, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(Language::Python, source) else {
        a.uncertain(UncertaintyKind::ParseError, "Python source did not parse", frame.locate(0..source.len()));
        return;
    };
    let mut scope = Scope { cwd: frame.cwd.clone(), argv: Some(frame.script_args.clone()), ..Scope::default() };
    let mut w = Walker { a, source, frame, root: tree.root_node(), calls: 0, exhausted: false };
    w.block(tree.root_node(), &mut scope);
}

struct Walker<'a, 'c, 's, 't> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
    root: Node<'t>,
    calls: usize,
    exhausted: bool,
}

impl<'t> Walker<'_, '_, '_, 't> {
    fn at(&self, node: Node<'_>) -> Location {
        self.frame.locate(node.byte_range())
    }

    fn visit(&mut self, node: Node<'t>) -> bool {
        if self.exhausted {
            return false;
        }
        if self.a.budget.visit().is_err() {
            self.exhausted = true;
            let at = self.at(node);
            self.a.uncertain(UncertaintyKind::LimitExhausted(crate::model::Limit::Nodes), "Python source was only partly analyzed", at);
            return false;
        }
        true
    }

    fn unresolved(&mut self, node: Node<'_>, detail: impl Into<String>) {
        let at = self.at(node);
        self.a.uncertain(UncertaintyKind::UnresolvedWrite, detail, at);
    }

    fn block(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            self.statement(child, scope);
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope) {
        if !self.visit(node) {
            return;
        }
        if ts::is_broken(node) || (node.has_error() && node.kind() != "module" && node.kind() != "block") {
            let at = self.at(node);
            self.a.uncertain(UncertaintyKind::ParseError, "a Python statement could not be parsed", at);
            return;
        }
        match node.kind() {
            "comment" | "pass_statement" | "break_statement" | "continue_statement" | "global_statement" | "nonlocal_statement" => {}
            "import_statement" => self.import(node, scope),
            "import_from_statement" => self.import_from(node, scope),
            "expression_statement" => {
                for child in ts::named_children(node) {
                    self.expression_or_assignment(child, scope);
                }
            }
            "with_statement" => self.with(node, scope),
            "for_statement" => self.for_loop(node, scope),
            "if_statement" | "while_statement" | "try_statement" | "match_statement" => {
                for child in ts::named_children(node) {
                    match child.kind() {
                        "block" | "elif_clause" | "else_clause" | "except_clause" | "finally_clause" | "case_clause" => {
                            let mut branch = scope.clone();
                            self.block_like(child, &mut branch);
                            scope.merge_uncertain(&branch);
                        }
                        _ => {
                            self.eval(child, scope);
                        }
                    }
                }
            }
            "decorated_definition" => {
                for child in ts::named_children(node) {
                    match child.kind() {
                        "decorator" => {
                            self.eval_children(child, scope);
                        }
                        _ => self.statement(child, scope),
                    }
                }
            }
            "function_definition" => {
                if let Some(params) = node.child_by_field_name("parameters") {
                    for p in ts::named_children(params) {
                        if let Some(default) = p.child_by_field_name("value") {
                            self.eval(default, scope);
                        }
                    }
                }
                if let Some(name) = node.child_by_field_name("name") {
                    scope.names.insert(ts::text(name, self.source).to_string(), Py::Def(node.byte_range()));
                }
            }
            "class_definition" => {
                if let Some(body) = node.child_by_field_name("body") {
                    let mut class_scope = scope.clone();
                    self.block(body, &mut class_scope);
                }
                if let Some(name) = node.child_by_field_name("name") {
                    scope.names.insert(ts::text(name, self.source).to_string(), Py::Data);
                }
            }
            _ => self.eval_children(node, scope),
        }
    }

    fn block_like(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            match child.kind() {
                "block" => self.block(child, scope),
                k if k.ends_with("_statement") || k == "expression_statement" || k == "decorated_definition" || k == "function_definition" || k == "class_definition" => self.statement(child, scope),
                _ => {
                    self.eval(child, scope);
                }
            }
        }
    }

    fn module_value(qualified: &str) -> Py {
        if qualified == "sys.argv" {
            return Py::Argv;
        }
        let root = qualified.split('.').next().unwrap_or(qualified);
        if KNOWN_MODULES.contains(&qualified) || KNOWN_MODULES.contains(&root) {
            Py::Api(qualified.to_string())
        } else {
            Py::Foreign(root.to_string())
        }
    }

    fn import(&mut self, node: Node<'t>, scope: &mut Scope) {
        for item in ts::named_children(node) {
            match item.kind() {
                "dotted_name" => {
                    let full = ts::text(item, self.source);
                    let top = full.split('.').next().unwrap_or(full);
                    scope.names.insert(top.to_string(), Self::module_value(top));
                }
                "aliased_import" => {
                    if let (Some(name), Some(alias)) = (item.child_by_field_name("name"), item.child_by_field_name("alias")) {
                        scope.names.insert(ts::text(alias, self.source).to_string(), Self::module_value(ts::text(name, self.source)));
                    }
                }
                _ => {}
            }
        }
    }

    fn import_from(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(module) = node.child_by_field_name("module_name").map(|m| ts::text(m, self.source).to_string()) else {
            return;
        };
        let mut cursor = node.walk();
        for item in node.children_by_field_name("name", &mut cursor) {
            let (name, alias) = match item.kind() {
                "aliased_import" => (
                    item.child_by_field_name("name").map(|n| ts::text(n, self.source)),
                    item.child_by_field_name("alias").map(|n| ts::text(n, self.source)),
                ),
                _ => (Some(ts::text(item, self.source)), None),
            };
            if let Some(name) = name {
                let bound = alias.unwrap_or(name).to_string();
                scope.names.insert(bound, Self::module_value(&format!("{module}.{name}")));
            }
        }
    }

    fn expression_or_assignment(&mut self, node: Node<'t>, scope: &mut Scope) {
        match node.kind() {
            "assignment" => {
                let value = node.child_by_field_name("right").map_or(Py::Unknown, |r| self.eval(r, scope));
                if let Some(left) = node.child_by_field_name("left") {
                    self.bind(left, value, scope);
                }
            }
            "augmented_assignment" => {
                if let Some(right) = node.child_by_field_name("right") {
                    self.eval(right, scope);
                }
                if let Some(left) = node.child_by_field_name("left") {
                    self.bind(left, Py::Unknown, scope);
                }
            }
            _ => {
                self.eval(node, scope);
            }
        }
    }

    fn bind(&mut self, target: Node<'t>, value: Py, scope: &mut Scope) {
        match target.kind() {
            "identifier" => {
                scope.names.insert(ts::text(target, self.source).to_string(), value);
            }
            "pattern_list" | "tuple_pattern" | "list_pattern" => {
                let parts = ts::named_children(target);
                let items = match value {
                    Py::List(items) if items.len() == parts.len() => items,
                    _ => vec![Py::Unknown; parts.len()],
                };
                for (part, item) in parts.into_iter().zip(items) {
                    self.bind(part, item, scope);
                }
            }
            // Resolved rather than matched on text, so `import sys as s;
            // s.argv = ...` and `a = sys.argv; a[1] = ...` invalidate too.
            "attribute" => {
                let object = target.child_by_field_name("object").map_or(Py::Unknown, |o| self.eval(o, scope));
                let attr = target.child_by_field_name("attribute").map(|a| ts::text(a, self.source));
                if object == Py::Api("sys".into()) && attr == Some("argv") {
                    scope.argv = None;
                }
            }
            "subscript" => {
                if target.child_by_field_name("value").map(|v| self.eval(v, scope)) == Some(Py::Argv) {
                    scope.argv = None;
                }
            }
            _ => {}
        }
    }

    fn with(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            match child.kind() {
                "with_clause" => {
                    for item in ts::named_children(child) {
                        let Some(value) = item.child_by_field_name("value") else { continue };
                        if value.kind() == "as_pattern" {
                            let parts = ts::named_children(value);
                            let bound = parts.first().map_or(Py::Unknown, |e| self.eval(*e, scope));
                            if let Some(alias) = value.child_by_field_name("alias").and_then(|a| ts::named_children(a).into_iter().next()) {
                                self.bind(alias, bound, scope);
                            }
                        } else {
                            self.eval(value, scope);
                        }
                    }
                }
                "block" => self.block(child, scope),
                _ => {}
            }
        }
    }

    fn for_loop(&mut self, node: Node<'t>, scope: &mut Scope) {
        let items = node.child_by_field_name("right").map_or(Py::Unknown, |r| self.eval(r, scope));
        let (Some(left), Some(body)) = (node.child_by_field_name("left"), node.child_by_field_name("body")) else {
            return;
        };
        let mut after = scope.clone();
        match items {
            Py::List(items) if items.len() <= MAX_LOOP_ITEMS => {
                for item in items {
                    let mut iteration = scope.clone();
                    self.bind(left, item, &mut iteration);
                    self.block(body, &mut iteration);
                    after.merge_uncertain(&iteration);
                }
            }
            _ => {
                let mut iteration = scope.clone();
                self.bind(left, Py::Unknown, &mut iteration);
                self.block(body, &mut iteration);
                after.merge_uncertain(&iteration);
            }
        }
        let mut unbind = after.clone();
        self.bind(left, Py::Unknown, &mut unbind);
        *scope = unbind;
        if let Some(alternative) = node.child_by_field_name("alternative") {
            self.block_like(alternative, scope);
        }
    }

    fn eval_children(&mut self, node: Node<'t>, scope: &mut Scope) -> Py {
        for child in ts::named_children(node) {
            self.eval(child, scope);
        }
        Py::Unknown
    }

    fn eval(&mut self, node: Node<'t>, scope: &mut Scope) -> Py {
        if !self.visit(node) {
            return Py::Unknown;
        }
        let text = ts::text(node, self.source);
        match node.kind() {
            "string" => self.string(node, scope),
            "concatenated_string" => {
                let mut out = String::new();
                for part in ts::named_children(node) {
                    match self.eval(part, scope) {
                        Py::Str(s) => out.push_str(&s),
                        _ => return Py::Unknown,
                    }
                }
                Py::Str(out)
            }
            "integer" => text.parse().map_or(Py::Data, Py::Int),
            "true" | "false" | "none" | "float" | "lambda" => Py::Data,
            "identifier" => match scope.names.get(text) {
                Some(v) => v.clone(),
                None if text == "open" => Py::Api("builtins.open".into()),
                None if matches!(text, "exec" | "eval" | "compile" | "__import__" | "getattr") => Py::Api(format!("builtins.{text}")),
                None if BUILTINS.contains(&text) => Py::Api(format!("builtins.{text}")),
                None => Py::Unknown,
            },
            "attribute" => {
                let object = node.child_by_field_name("object").map_or(Py::Unknown, |o| self.eval(o, scope));
                let attr = node.child_by_field_name("attribute").map_or("", |a| ts::text(a, self.source)).to_string();
                self.attribute(object, &attr, scope)
            }
            "subscript" => {
                let value = node.child_by_field_name("value").map_or(Py::Unknown, |v| self.eval(v, scope));
                let index = node.child_by_field_name("subscript").map_or(Py::Unknown, |s| self.eval(s, scope));
                match (value, index) {
                    (Py::Argv, Py::Int(i)) => match scope.argv.as_ref().and_then(|argv| usize::try_from(i).ok().and_then(|i| argv.get(i))) {
                        Some(Value::Known(s)) => Py::Str(s.clone()),
                        _ => Py::Unknown,
                    },
                    (Py::List(items), Py::Int(i)) => usize::try_from(i).ok().and_then(|i| items.get(i).cloned()).unwrap_or(Py::Unknown),
                    _ => Py::Unknown,
                }
            }
            "binary_operator" => {
                let left = node.child_by_field_name("left").map_or(Py::Unknown, |l| self.eval(l, scope));
                let right = node.child_by_field_name("right").map_or(Py::Unknown, |r| self.eval(r, scope));
                let op = node.child_by_field_name("operator").map_or("", |o| ts::text(o, self.source));
                match (op, left, right) {
                    ("/", Py::Path(l), Py::Str(r) | Py::Path(r)) => Py::Path(join(&l, &r)),
                    ("+", Py::Str(l), Py::Str(r)) => Py::Str(l + &r),
                    _ => Py::Unknown,
                }
            }
            "call" => self.call(node, scope),
            "list" | "tuple" => Py::List(ts::named_children(node).into_iter().map(|c| self.eval(c, scope)).collect()),
            "parenthesized_expression" | "await" => ts::named_children(node).into_iter().map(|c| self.eval(c, scope)).last().unwrap_or(Py::Unknown),
            "conditional_expression" => {
                let values: Vec<Py> = ts::named_children(node).into_iter().map(|c| self.eval(c, scope)).collect();
                match values.as_slice() {
                    [a, _, b] if a == b => a.clone(),
                    _ => Py::Unknown,
                }
            }
            _ => self.eval_children(node, scope),
        }
    }

    fn string(&mut self, node: Node<'t>, scope: &mut Scope) -> Py {
        let mut out = String::new();
        let mut known = true;
        for part in ts::named_children(node) {
            match part.kind() {
                "string_start" | "string_end" => {}
                "string_content" => out.push_str(ts::text(part, self.source)),
                "escape_sequence" => out.push_str(match ts::text(part, self.source) {
                    "\\n" => "\n",
                    "\\t" => "\t",
                    "\\\\" => "\\",
                    "\\'" => "'",
                    "\\\"" => "\"",
                    other => other,
                }),
                "interpolation" => match part.child_by_field_name("expression").map(|e| self.eval(e, scope)) {
                    Some(Py::Str(s) | Py::Path(s)) => out.push_str(&s),
                    Some(Py::Int(i)) => out.push_str(&i.to_string()),
                    _ => known = false,
                },
                _ => known = false,
            }
        }
        if known { Py::Str(out) } else { Py::Unknown }
    }

    fn attribute(&mut self, object: Py, attr: &str, scope: &Scope) -> Py {
        match object {
            Py::Api(module) => Self::module_value(&format!("{module}.{attr}")),
            Py::Foreign(module) => Py::Foreign(module),
            Py::Path(p) => match attr {
                "parent" => Py::Path(paths::parent(&p).unwrap_or(".").to_string()),
                "name" => Py::Str(normalize::basename(&p).to_string()),
                "stem" | "suffix" => Py::Data,
                _ => Py::Method(Box::new(Py::Path(p)), attr.to_string()),
            },
            Py::Argv if scope.argv.is_some() => Py::Method(Box::new(Py::Argv), attr.to_string()),
            other => Py::Method(Box::new(other), attr.to_string()),
        }
    }

    fn call(&mut self, node: Node<'t>, scope: &mut Scope) -> Py {
        let callee = node.child_by_field_name("function").map_or(Py::Unknown, |f| self.eval(f, scope));
        let mut positional: Vec<Py> = Vec::new();
        let mut keywords: HashMap<String, (Py, Node<'t>)> = HashMap::new();
        if let Some(arguments) = node.child_by_field_name("arguments") {
            for arg in ts::named_children(arguments) {
                if arg.kind() == "keyword_argument" {
                    if let (Some(name), Some(value)) = (arg.child_by_field_name("name"), arg.child_by_field_name("value")) {
                        let v = self.eval(value, scope);
                        keywords.insert(ts::text(name, self.source).to_string(), (v, value));
                    }
                } else {
                    positional.push(self.eval(arg, scope));
                }
            }
        }
        let arg = |i: usize, name: &str| positional.get(i).cloned().or_else(|| keywords.get(name).map(|(v, _)| v.clone())).unwrap_or(Py::Unknown);
        let cwd = scope.cwd.clone();

        match callee {
            Py::Api(api) => match api.as_str() {
                "builtins.open" | "io.open" | "codecs.open" => {
                    self.open(node, &arg(0, "file"), &arg(1, "mode"), keywords.contains_key("mode") || positional.len() > 1, cwd.as_deref())
                }
                "pathlib.Path" | "pathlib.PurePath" | "pathlib.PosixPath" | "pathlib.WindowsPath" => {
                    let mut out: Option<String> = None;
                    for part in &positional {
                        match (part, &out) {
                            (Py::Str(s) | Py::Path(s), None) => out = Some(s.clone()),
                            (Py::Str(s) | Py::Path(s), Some(base)) => out = Some(join(base, s)),
                            _ => return Py::Unknown,
                        }
                    }
                    Py::Path(out.unwrap_or_else(|| ".".into()))
                }
                "pathlib.Path.cwd" | "os.getcwd" => cwd.map_or(Py::Unknown, Py::Path),
                "os.path.join" => {
                    let parts: Option<Vec<String>> = positional.iter().map(|p| match p { Py::Str(s) | Py::Path(s) => Some(s.clone()), _ => None }).collect();
                    parts.map_or(Py::Unknown, |parts| Py::Str(parts.iter().skip(1).fold(parts[0].clone(), |acc, p| join(&acc, p))))
                }
                "os.path.dirname" => match arg(0, "p") { Py::Str(s) | Py::Path(s) => Py::Str(paths::parent(&s).unwrap_or("").to_string()), _ => Py::Unknown },
                "os.path.basename" => match arg(0, "p") { Py::Str(s) | Py::Path(s) => Py::Str(normalize::basename(&s).to_string()), _ => Py::Unknown },
                "os.path.abspath" | "os.path.realpath" => match (arg(0, "path"), cwd) {
                    (Py::Str(s) | Py::Path(s), _) if s.starts_with('/') => Py::Str(s),
                    (Py::Str(s) | Py::Path(s), Some(dir)) => Py::Str(join(&dir, &s)),
                    _ => Py::Unknown,
                },
                "os.chdir" => {
                    scope.cwd = match arg(0, "path") {
                        Py::Str(s) | Py::Path(s) => match paths::resolve(&Value::Known(s), scope.cwd.as_deref(), self.a.ctx.path_style) {
                            crate::model::Target::Path(d) => Some(d),
                            crate::model::Target::Unresolved => None,
                        },
                        _ => None,
                    };
                    Py::Data
                }
                "os.remove" | "os.unlink" => self.effect(node, FileOp::Delete, &arg(0, "path"), scope),
                "os.rename" | "os.replace" | "os.renames" | "shutil.move" => {
                    self.effect(node, FileOp::Rename, &arg(0, "src"), scope);
                    self.effect(node, FileOp::Rename, &arg(1, "dst"), scope)
                }
                "os.truncate" => self.effect(node, FileOp::Overwrite, &arg(0, "path"), scope),
                "os.symlink" | "os.link" => self.effect(node, FileOp::Create, &arg(1, "dst"), scope),
                "os.open" => {
                    self.unresolved(node, "`os.open` flags were not analyzed");
                    Py::WriteHandle
                }
                "shutil.copy" | "shutil.copy2" | "shutil.copyfile" => self.effect(node, FileOp::Copy, &arg(1, "dst"), scope),
                "shutil.rmtree" => self.tree(node, &arg(0, "path"), "shutil.rmtree", scope),
                "shutil.copytree" => self.tree(node, &arg(1, "dst"), "shutil.copytree", scope),
                "shutil.unpack_archive" => {
                    let dir = match arg(1, "extract_dir") { Py::Unknown if positional.len() < 2 && !keywords.contains_key("extract_dir") => Py::Str(".".into()), d => d };
                    self.tree(node, &dir, "shutil.unpack_archive", scope)
                }
                "shutil.make_archive" => {
                    self.unresolved(node, "`shutil.make_archive` output was not analyzed");
                    Py::Data
                }
                "os.system" | "os.popen" => self.shell_source(node, &arg(0, "command"), scope),
                "subprocess.run" | "subprocess.call" | "subprocess.check_call" | "subprocess.check_output" | "subprocess.Popen" => {
                    let shell = keywords.get("shell").is_some_and(|(_, n)| ts::text(*n, self.source) == "True");
                    let cwd_arg = keywords.get("cwd").map(|(v, _)| v.clone());
                    self.subprocess(node, &arg(0, "args"), shell, cwd_arg, scope)
                }
                "builtins.exec" | "builtins.eval" | "builtins.compile" => match arg(0, "source") {
                    Py::Str(source) => {
                        let child = Frame { depth: self.frame.depth + 1, script_args: scope.argv.clone().unwrap_or_default(), base: Some(self.at(node)), cwd: scope.cwd.clone() };
                        self.a.source(Language::Python, &source, child);
                        Py::Unknown
                    }
                    _ => {
                        self.unresolved(node, "dynamically evaluated Python source");
                        Py::Unknown
                    }
                },
                "builtins.__import__" | "importlib.import_module" | "builtins.getattr" => Py::Unknown,
                _ => Py::Data,
            },
            Py::Method(receiver, method) => self.method(node, *receiver, &method, &positional, scope),
            Py::Foreign(module) => {
                self.unresolved(node, format!("a call into `{module}`, which devkit does not model"));
                Py::Unknown
            }
            Py::Def(range) => {
                self.calls += 1;
                if self.frame.depth + self.calls > self.a.budget.limits().depth {
                    let at = self.at(node);
                    self.a.uncertain(UncertaintyKind::LimitExhausted(crate::model::Limit::Depth), "a Python call chain was not followed", at);
                } else if let Some(def) = self.root.descendant_for_byte_range(range.start, range.end)
                    && let Some(body) = def.child_by_field_name("body")
                {
                    let mut call_scope = scope.clone();
                    if let Some(params) = def.child_by_field_name("parameters") {
                        for (i, p) in ts::named_children(params).into_iter().enumerate() {
                            let name = if p.kind() == "identifier" { Some(p) } else { p.child_by_field_name("name") };
                            if let Some(name) = name {
                                call_scope.names.insert(ts::text(name, self.source).to_string(), positional.get(i).cloned().unwrap_or(Py::Unknown));
                            }
                        }
                    }
                    self.block(body, &mut call_scope);
                }
                self.calls -= 1;
                Py::Unknown
            }
            Py::Unknown => {
                let name = node.child_by_field_name("function").map_or("", |f| ts::text(f, self.source)).to_string();
                self.unresolved(node, format!("a call to `{name}`, which could not be resolved"));
                Py::Unknown
            }
            Py::Str(_) | Py::Path(_) | Py::Int(_) | Py::List(_) | Py::Argv | Py::WriteHandle | Py::Data => Py::Data,
        }
    }

    fn method(&mut self, node: Node<'t>, receiver: Py, method: &str, args: &[Py], scope: &mut Scope) -> Py {
        match receiver {
            Py::Path(p) => {
                let this = Py::Path(p.clone());
                match method {
                    "write_text" | "write_bytes" => self.effect(node, FileOp::Overwrite, &this, scope),
                    "touch" | "symlink_to" | "hardlink_to" => self.effect(node, FileOp::Create, &this, scope),
                    "unlink" => self.effect(node, FileOp::Delete, &this, scope),
                    "rename" | "replace" => {
                        self.effect(node, FileOp::Rename, &this, scope);
                        let dest = args.first().cloned().unwrap_or(Py::Unknown);
                        self.effect(node, FileOp::Rename, &dest, scope);
                        dest
                    }
                    "open" => self.open(node, &this, &args.first().cloned().unwrap_or(Py::Unknown), !args.is_empty(), scope.cwd.as_deref()),
                    "with_suffix" => match args.first() {
                        Some(Py::Str(s)) => Py::Path(format!("{}{s}", p.rsplit_once('.').filter(|(stem, _)| !stem.ends_with('/')).map_or(p.as_str(), |(stem, _)| stem))),
                        _ => Py::Unknown,
                    },
                    "with_name" => match args.first() {
                        Some(Py::Str(s)) => Py::Path(join(paths::parent(&p).unwrap_or("."), s)),
                        _ => Py::Unknown,
                    },
                    "joinpath" => args.iter().try_fold(p, |acc, a| match a { Py::Str(s) | Py::Path(s) => Some(join(&acc, s)), _ => None }).map_or(Py::Unknown, Py::Path),
                    "resolve" | "absolute" => match &scope.cwd {
                        Some(dir) if !p.starts_with('/') => Py::Path(join(dir, &p)),
                        _ => Py::Path(p),
                    },
                    "iterdir" | "glob" | "rglob" | "expanduser" => Py::Unknown,
                    _ => Py::Data,
                }
            }
            Py::Argv => {
                if matches!(method, "append" | "extend" | "insert" | "pop" | "remove" | "clear" | "reverse" | "sort") {
                    scope.argv = None;
                }
                Py::Data
            }
            Py::Str(s) if method == "join" => match args.first() {
                Some(Py::List(items)) => items.iter().map(|i| match i { Py::Str(x) => Some(x.as_str()), _ => None }).collect::<Option<Vec<_>>>().map_or(Py::Unknown, |parts| Py::Str(parts.join(&s))),
                _ => Py::Unknown,
            },
            Py::Unknown if WRITE_METHODS.contains(&method) => {
                self.unresolved(node, format!("`.{method}()` on a value whose type could not be determined"));
                Py::Unknown
            }
            Py::Foreign(module) => {
                self.unresolved(node, format!("a call into `{module}`, which devkit does not model"));
                Py::Unknown
            }
            _ => Py::Unknown,
        }
    }

    fn open(&mut self, node: Node<'t>, target: &Py, mode: &Py, mode_given: bool, cwd: Option<&str>) -> Py {
        if !mode_given {
            return Py::Data;
        }
        let op = match mode {
            Py::Str(m) if m.contains('w') => FileOp::Overwrite,
            Py::Str(m) if m.contains('a') => FileOp::Append,
            Py::Str(m) if m.contains('x') => FileOp::Create,
            Py::Str(m) if m.contains('+') => FileOp::Overwrite,
            Py::Str(_) => return Py::Data,
            _ => {
                self.unresolved(node, "a file opened with a mode that could not be determined");
                return Py::WriteHandle;
            }
        };
        let at = self.at(node);
        self.a.file_effect(op, &target.as_value(), cwd, at);
        Py::WriteHandle
    }

    fn effect(&mut self, node: Node<'t>, op: FileOp, target: &Py, scope: &Scope) -> Py {
        let at = self.at(node);
        self.a.file_effect(op, &target.as_value(), scope.cwd.as_deref(), at);
        Py::Data
    }

    fn tree(&mut self, node: Node<'t>, scope_path: &Py, by: &str, scope: &Scope) -> Py {
        let at = self.at(node);
        self.a.tree_effect(&scope_path.as_value(), false, scope.cwd.as_deref(), by, at);
        Py::Data
    }

    fn shell_source(&mut self, node: Node<'t>, command: &Py, scope: &Scope) -> Py {
        match command {
            Py::Str(source) => {
                let child = Frame { depth: self.frame.depth + 1, script_args: Vec::new(), base: Some(self.at(node)), cwd: scope.cwd.clone() };
                self.a.source(Language::Bash, source, child);
            }
            _ => self.unresolved(node, "a shell command that could not be determined"),
        }
        Py::Data
    }

    fn subprocess(&mut self, node: Node<'t>, args: &Py, shell: bool, cwd_arg: Option<Py>, scope: &Scope) -> Py {
        let cwd = match cwd_arg {
            None => scope.cwd.clone(),
            Some(Py::Str(d) | Py::Path(d)) => match paths::resolve(&Value::Known(d), scope.cwd.as_deref(), self.a.ctx.path_style) {
                crate::model::Target::Path(p) => Some(p),
                crate::model::Target::Unresolved => None,
            },
            Some(_) => None,
        };
        let inner_scope = Scope { cwd: cwd.clone(), ..scope.clone() };
        match (args, shell) {
            (Py::Str(_), true) => self.shell_source(node, args, &inner_scope),
            (Py::Str(program), false) => self.run_argv(node, vec![Value::Known(program.clone())], cwd),
            (Py::List(items), false) => self.run_argv(node, items.iter().map(Py::as_value).collect(), cwd),
            _ => {
                self.unresolved(node, "a subprocess command that could not be determined");
                Py::Data
            }
        }
    }

    fn run_argv(&mut self, node: Node<'t>, argv: Vec<Value>, cwd: Option<String>) -> Py {
        let words = argv
            .into_iter()
            .map(|value| Word { typed: value.known().unwrap_or("?").to_string(), value, span: node.byte_range() })
            .collect();
        let raw = RawInvocation { words, stdin: Stdin::None, cwd, language: Language::Python, location: self.at(node) };
        self.a.invocation(raw, self.frame);
        Py::Data
    }
}

fn join(base: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        return rel.to_string();
    }
    if base == "." {
        return rel.to_string();
    }
    format!("{}/{}", base.trim_end_matches('/'), rel.strip_prefix("./").unwrap_or(rel))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS. Where tree-sitter-python names a field differently (`module_name`, `alias`, `alternative`), fix the walker to the grammar's `node-types.json` in the registry checkout, not the test.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): analyze Python file, process and exec calls"
```

---

### Task 8: PowerShell adapter

**Files:**
- Modify: `crates/devkit-command/src/powershell.rs`

**Interfaces:**
- Consumes: the same `Analyzer` surface as Task 7.
- Produces: `powershell::walk(&mut Analyzer, &str, &Frame)`.

The airbus-cert grammar fails on ordinary argument shapes, so this adapter is written around statement-scoped recovery from the start. Parameter binding follows PowerShell's rules: names are case-insensitive, an unambiguous prefix names a parameter, `-Name:value` binds inline, and unnamed arguments bind positionally.

- [ ] **Step 1: Pin the grammar's node shapes**

```rust
#[cfg(test)]
mod shapes {
    use crate::{model::Language, ts};

    #[test]
    fn node_shapes_this_adapter_relies_on() {
        for (source, fragment) in [
            ("Set-Content -Path a.txt -Value x", "command_name"),
            ("Set-Content -Path a.txt -Value x", "command_parameter"),
            ("echo x > a.txt", "redirection"),
            ("$p = 'a.txt'", "assignment_expression"),
            ("[System.IO.File]::WriteAllText('a.txt', 'x')", "invokation_expression"),
            ("@'\nprint(1)\n'@ | python -", "verbatim_here_string"),
            ("foreach ($f in 'a','b') { Remove-Item $f }", "foreach_statement"),
        ] {
            let tree = ts::parse(Language::PowerShell, source).unwrap();
            let sexp = tree.root_node().to_sexp();
            assert!(sexp.contains(fragment), "{source:?}\n{sexp}");
        }
    }

    #[test]
    fn the_grammar_still_fails_on_the_recorded_argument_shapes() {
        for source in [
            "Get-ChildItem | Format-Table Mode, Name -AutoSize",
            "git -C 'C:/repo' log --format='%h %s'",
            "git push --force-with-lease=a:b origin c",
        ] {
            let tree = ts::parse(Language::PowerShell, source).unwrap();
            assert!(tree.root_node().has_error(), "{source:?} now parses; the recovery fixtures below may no longer exercise recovery");
        }
    }
}
```

Run: `cargo nextest run -p devkit-command powershell::shapes --no-capture`
Expected: adjust each fragment to the kind the printed tree uses (`verbatim_here_string_characters`, `expandable_here_string_literal`, `command_elements`, `generic_token`, `redirected_file_name`, `type_literal`, `member_name`, `argument_list`). Record the names in the `kinds` module in Step 3. The second test is a tripwire: when an upstream grammar release fixes those shapes, it fails and says why.

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use crate::{
        model::UncertaintyKind,
        testutil::{ctx, targets},
        Analysis, Dialect, PathStyle,
    };

    fn ps(source: &str) -> Analysis {
        let mut c = ctx(Dialect::PowerShell);
        c.cwd = Some("C:/repo".into());
        c.path_style = PathStyle::Windows;
        crate::analyze(source, &c)
    }

    #[test]
    fn content_cmdlets_by_name_prefix_and_position() {
        assert_eq!(targets(&ps("Set-Content -Path a.txt -Value x")), ["C:/repo/a.txt"]);
        assert_eq!(targets(&ps("set-content -pa b.txt -va x")), ["C:/repo/b.txt"]);
        assert_eq!(targets(&ps("Add-Content c.txt 'x'")), ["C:/repo/c.txt"]);
        assert_eq!(targets(&ps("'x' | Out-File -FilePath:d.txt -Append")), ["C:/repo/d.txt"]);
    }

    #[test]
    fn item_cmdlets_and_aliases() {
        assert_eq!(targets(&ps("Remove-Item old.txt")), ["C:/repo/old.txt"]);
        assert_eq!(targets(&ps("Move-Item a.txt b.txt")), ["C:/repo/a.txt", "C:/repo/b.txt"]);
        assert_eq!(targets(&ps("Rename-Item -Path src/a.txt -NewName b.txt")), ["C:/repo/src/a.txt", "C:/repo/src/b.txt"]);
        assert_eq!(targets(&ps("ni new.txt -ItemType File")), ["C:/repo/new.txt"]);
        assert!(targets(&ps("New-Item -ItemType Directory -Path gen")).is_empty());
        assert_eq!(ps("Remove-Item build -Recurse -Force").tree_effects[0].scope, "C:/repo/build");
    }

    #[test]
    fn redirects_and_null() {
        assert_eq!(targets(&ps("echo x > a.txt; echo y >> b.txt")), ["C:/repo/a.txt", "C:/repo/b.txt"]);
        assert!(targets(&ps("git status > $null")).is_empty());
    }

    #[test]
    fn variables_join_path_and_set_location() {
        assert_eq!(targets(&ps("$p = Join-Path 'gen' 'a.txt'; Set-Content $p x")), ["C:/repo/gen/a.txt"]);
        assert_eq!(targets(&ps("$d = 'gen'; Set-Content \"$d/b.txt\" x")), ["C:/repo/gen/b.txt"]);
        assert_eq!(targets(&ps("Set-Location sub; Set-Content a.txt x")), ["C:/repo/sub/a.txt"]);
        assert_eq!(targets(&ps("Set-Content $env:OUT x")), ["?"]);
    }

    #[test]
    fn dotnet_file_apis() {
        assert_eq!(targets(&ps("[System.IO.File]::WriteAllText('a.txt', 'x')")), ["C:/repo/a.txt"]);
        assert_eq!(targets(&ps("[IO.File]::AppendAllLines('b.txt', @('x'))")), ["C:/repo/b.txt"]);
    }

    #[test]
    fn a_here_string_piped_to_python_is_python_source() {
        let a = ps("@'\nopen('h.txt', 'w')\n'@ | python -");
        assert_eq!(targets(&a), ["C:/repo/h.txt"]);
    }

    #[test]
    fn a_literal_foreach_runs_per_item_and_wildcards_are_unresolved() {
        assert_eq!(targets(&ps("foreach ($f in 'a.txt','b.txt') { Remove-Item $f }")), ["C:/repo/a.txt", "C:/repo/b.txt"]);
        assert_eq!(targets(&ps("Remove-Item *.log")), ["?"]);
    }

    #[test]
    fn dynamic_invocation_is_unresolved() {
        let a = ps("Invoke-Expression $cmd");
        assert!(a.uncertainties.iter().any(|u| u.kind == UncertaintyKind::UnresolvedWrite));
        assert_eq!(targets(&ps("iex 'Set-Content e.txt x'")), ["C:/repo/e.txt"]);
    }

    #[test]
    fn a_statement_that_fails_to_parse_leaves_its_siblings_enforced() {
        let a = ps("Set-Content a.txt x\ngit push --force-with-lease=a:b origin c\nSet-Content b.txt y");
        let t = targets(&a);
        assert!(t.contains(&"C:/repo/a.txt".to_string()) && t.contains(&"C:/repo/b.txt".to_string()), "{t:?}");
    }

    #[test]
    fn read_only_statements_that_fail_to_parse_are_silent() {
        for source in ["Get-ChildItem | Format-Table Mode, Name -AutoSize", "git -C 'C:/repo' log --format='%h %s'"] {
            let a = ps(source);
            assert!(a.uncertainties.is_empty() && a.file_effects.is_empty(), "{source:?}: {a:?}");
        }
    }

    #[test]
    fn a_broken_statement_that_redirects_is_uncertain() {
        let a = ps("Get-ChildItem | Format-Table Mode, Name -AutoSize > out.txt");
        assert!(a.uncertainties.iter().any(|u| u.kind == UncertaintyKind::ParseError), "{a:?}");
    }

    #[test]
    fn external_programs_reach_the_catalog() {
        assert_eq!(targets(&ps("git checkout -- a.rs")), ["C:/repo/a.rs"]);
    }
}
```

The parse-recovery tests go through `git` because the `git` catalog gives `push` and `log` no effect, so a read-only broken statement is silent by Decision 3.

- [ ] **Step 3: Run to see them fail**

Run: `cargo nextest run -p devkit-command powershell`
Expected: FAIL.

- [ ] **Step 4: Implement**

`crates/devkit-command/src/powershell.rs` (keep `shapes` and `tests`):

```rust
//! PowerShell: pipelines, redirects, variables, cmdlet parameter binding, the
//! file cmdlets and .NET file APIs, here-strings, and external programs.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    catalog, embed,
    model::{FileOp, Language, Location, UncertaintyKind, Value},
    normalize, paths, ts,
};

/// Node kinds as the pinned grammar names them; Step 1's shapes test is the
/// source of truth.
mod kinds {
    pub const PIPELINE: &str = "pipeline";
    pub const PIPELINE_CHAIN: &str = "pipeline_chain";
    pub const COMMAND: &str = "command";
    pub const COMMAND_NAME: &str = "command_name";
    pub const COMMAND_NAME_EXPR: &str = "command_name_expr";
    pub const COMMAND_ELEMENTS: &str = "command_elements";
    pub const PARAMETER: &str = "command_parameter";
    pub const REDIRECTION: &str = "redirection";
    pub const REDIRECTED_FILE: &str = "redirected_file_name";
    pub const ASSIGNMENT: &str = "assignment_expression";
    pub const VARIABLE: &str = "variable";
    pub const INVOKATION: &str = "invokation_expression";
    pub const TYPE_LITERAL: &str = "type_literal";
    pub const MEMBER_NAME: &str = "member_name";
    pub const ARGUMENT_LIST: &str = "argument_list";
    pub const FOREACH: &str = "foreach_statement";
    pub const FUNCTION: &str = "function_statement";
    pub const SUB_EXPRESSION: &str = "sub_expression";
    pub const SCRIPT_BLOCK: &str = "script_block_expression";
}

const MAX_LOOP_ITEMS: usize = 32;

#[derive(Debug, Clone, Default, PartialEq)]
struct Scope {
    vars: HashMap<String, Value>,
    cwd: Option<String>,
    functions: HashMap<String, std::ops::Range<usize>>,
}

impl Scope {
    fn merge_uncertain(&mut self, branch: &Scope) {
        for (name, value) in &branch.vars {
            if self.vars.get(name) != Some(value) {
                self.vars.insert(name.clone(), Value::Unknown);
            }
        }
        if branch.cwd != self.cwd {
            self.cwd = None;
        }
    }
}

/// A cmdlet's parameters: canonical names in positional order, the ones that
/// are switches, and aliases.
struct Cmdlet {
    verb: Verb,
    params: &'static [&'static str],
    positional: &'static [&'static str],
    switches: &'static [&'static str],
    aliases: &'static [(&'static str, &'static str)],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    SetContent,
    AddContent,
    ClearContent,
    OutFile,
    TeeObject,
    NewItem,
    RemoveItem,
    MoveItem,
    RenameItem,
    CopyItem,
    ExportFile,
    ExpandArchive,
    WebRequest,
    SetLocation,
    PopLocation,
    InvokeExpression,
    JoinPath,
    GetLocation,
}

const PATH_ALIASES: &[(&str, &str)] = &[("literalpath", "path"), ("pspath", "path"), ("lp", "path")];

fn cmdlet(name: &str) -> Option<Cmdlet> {
    let c = |verb, params, positional, switches| Some(Cmdlet { verb, params, positional, switches, aliases: PATH_ALIASES });
    match name.to_ascii_lowercase().as_str() {
        "set-content" => c(Verb::SetContent, &["path", "value", "encoding"], &["path", "value"], &["force", "nonewline"]),
        "add-content" | "ac" => c(Verb::AddContent, &["path", "value", "encoding"], &["path", "value"], &["force", "nonewline"]),
        "clear-content" | "clc" => c(Verb::ClearContent, &["path"], &["path"], &["force"]),
        "out-file" => c(Verb::OutFile, &["filepath", "path", "encoding", "inputobject", "width"], &["filepath"], &["append", "force", "noclobber", "nonewline"]),
        "tee-object" | "tee" => c(Verb::TeeObject, &["filepath", "path", "variable", "inputobject"], &["filepath"], &["append"]),
        "new-item" | "ni" => c(Verb::NewItem, &["path", "name", "itemtype", "value"], &["path"], &["force"]),
        "remove-item" | "ri" | "rm" | "del" | "erase" | "rd" | "rmdir" => c(Verb::RemoveItem, &["path", "include", "exclude", "filter"], &["path"], &["recurse", "force"]),
        "move-item" | "mi" | "mv" | "move" => c(Verb::MoveItem, &["path", "destination"], &["path", "destination"], &["force"]),
        "rename-item" | "rni" | "ren" => c(Verb::RenameItem, &["path", "newname"], &["path", "newname"], &["force"]),
        "copy-item" | "cpi" | "cp" | "copy" => c(Verb::CopyItem, &["path", "destination", "include", "exclude", "filter"], &["path", "destination"], &["recurse", "force", "container"]),
        "export-csv" | "export-clixml" => c(Verb::ExportFile, &["path", "inputobject", "delimiter", "encoding"], &["path"], &["append", "force", "notypeinformation"]),
        "expand-archive" => c(Verb::ExpandArchive, &["path", "destinationpath"], &["path", "destinationpath"], &["force"]),
        "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm" | "curl" | "wget" => c(Verb::WebRequest, &["uri", "outfile", "method", "headers", "body"], &["uri"], &["usebasicparsing"]),
        "set-location" | "sl" | "cd" | "chdir" | "push-location" | "pushd" => c(Verb::SetLocation, &["path"], &["path"], &[]),
        "pop-location" | "popd" => c(Verb::PopLocation, &[], &[], &[]),
        "invoke-expression" | "iex" => c(Verb::InvokeExpression, &["command"], &["command"], &[]),
        "join-path" => c(Verb::JoinPath, &["path", "childpath"], &["path", "childpath"], &["resolve"]),
        "get-location" | "gl" | "pwd" => c(Verb::GetLocation, &[], &[], &[]),
        _ => None,
    }
}

pub(crate) fn walk(a: &mut Analyzer<'_>, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(Language::PowerShell, source) else {
        a.uncertain(UncertaintyKind::ParseError, "PowerShell source did not parse", frame.locate(0..source.len()));
        return;
    };
    let mut scope = Scope { cwd: frame.cwd.clone(), ..Scope::default() };
    let mut w = Walker { a, source, frame, root: tree.root_node() };
    w.statements(tree.root_node(), &mut scope);
}

struct Walker<'a, 'c, 's, 't> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
    root: Node<'t>,
}

impl<'t> Walker<'_, '_, '_, 't> {
    fn at(&self, node: Node<'_>) -> Location {
        self.frame.locate(node.byte_range())
    }

    fn statements(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            self.statement(child, scope);
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope) {
        if self.a.budget.visit().is_err() {
            return;
        }
        if node.has_error() && !matches!(node.kind(), "program" | "statement_list" | "statement_block" | "script_block" | "script_block_body") {
            self.broken(node);
            return;
        }
        match node.kind() {
            "comment" => {}
            k if k == kinds::PIPELINE || k == kinds::PIPELINE_CHAIN => self.pipeline(node, scope),
            k if k == kinds::ASSIGNMENT => self.assign(node, scope),
            k if k == kinds::FOREACH => self.foreach(node, scope),
            k if k == kinds::FUNCTION => {
                let name = ts::named_children(node).into_iter().find(|n| n.kind() == "function_name");
                let body = ts::named_children(node).into_iter().find(|n| n.kind() == "script_block");
                if let (Some(name), Some(body)) = (name, body) {
                    scope.functions.insert(ts::text(name, self.source).to_ascii_lowercase(), body.byte_range());
                }
            }
            "if_statement" | "while_statement" | "do_statement" | "for_statement" | "switch_statement" | "try_statement" | "trap_statement" => {
                let mut branch = scope.clone();
                self.statements(node, &mut branch);
                scope.merge_uncertain(&branch);
            }
            _ => self.statements(node, scope),
        }
    }

    /// Decision 3: a broken statement is uncertain only if it could write.
    fn broken(&mut self, node: Node<'t>) {
        let text = ts::text(node, self.source);
        let mut program: Option<String> = None;
        let mut redirects = false;
        let mut cursor = node.walk();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == kinds::REDIRECTION || n.kind() == "file_redirection_operator" {
                redirects = true;
            }
            if program.is_none() && matches!(n.kind(), "command_name" | "generic_token") {
                program = Some(ts::text(n, self.source).to_string());
            }
            stack.extend(n.children(&mut cursor).collect::<Vec<_>>().into_iter().rev());
        }
        if program.is_none() {
            program = text.split_whitespace().next().map(str::to_string);
        }
        redirects |= text.split_whitespace().any(|w| matches!(w, ">" | ">>" | "*>" | "2>" | "2>>"));
        let could_write = match &program {
            None => true,
            Some(p) => {
                let base = normalize::basename(p);
                cmdlet(base).is_some_and(|c| !matches!(c.verb, Verb::JoinPath | Verb::GetLocation | Verb::PopLocation))
                    || catalog::is_cataloged(base)
                    || embed::is_interpreter(base)
            }
        };
        if redirects || could_write {
            let at = self.at(node);
            self.a.uncertain(UncertaintyKind::ParseError, "a PowerShell statement could not be parsed", at);
        }
    }

    fn pipeline(&mut self, node: Node<'t>, scope: &mut Scope) {
        let mut stdin = Stdin::None;
        for element in ts::named_children(node) {
            match element.kind() {
                k if k == kinds::COMMAND => {
                    let produced = Value::Unknown;
                    self.command(element, scope, stdin);
                    stdin = Stdin::Source { value: produced, span: element.byte_range() };
                }
                k if k == kinds::REDIRECTION => self.redirection(element, scope),
                k if k == kinds::PIPELINE || k == kinds::PIPELINE_CHAIN => self.pipeline(element, scope),
                _ => {
                    let value = self.value(element, scope);
                    stdin = Stdin::Source { value, span: element.byte_range() };
                }
            }
        }
    }

    fn redirection(&mut self, node: Node<'t>, scope: &mut Scope) {
        let mut cursor = node.walk();
        let op_text = node.children(&mut cursor).find(|c| c.kind().contains("redirection_operator")).map(|c| ts::text(c, self.source).to_string()).unwrap_or_default();
        let op = if op_text.ends_with(">>") {
            FileOp::Append
        } else if op_text.ends_with('>') && !op_text.contains('&') {
            FileOp::Overwrite
        } else {
            return;
        };
        let Some(dest) = ts::named_children(node).into_iter().find(|n| n.kind() == kinds::REDIRECTED_FILE) else {
            return;
        };
        if ts::text(dest, self.source).eq_ignore_ascii_case("$null") {
            return;
        }
        let value = self.value(dest, scope);
        let at = self.at(node);
        self.a.file_effect(op, &value, scope.cwd.as_deref(), at);
    }

    fn assign(&mut self, node: Node<'t>, scope: &mut Scope) {
        let children = ts::named_children(node);
        let (Some(left), Some(right)) = (children.first(), children.last()) else { return };
        let value = match right.kind() {
            k if k == kinds::PIPELINE => {
                let elements = ts::named_children(*right);
                match elements.as_slice() {
                    [only] if only.kind() == kinds::COMMAND => self.command_value(*only, scope),
                    [only] => self.value(*only, scope),
                    _ => {
                        self.pipeline(*right, scope);
                        Value::Unknown
                    }
                }
            }
            _ => self.value(*right, scope),
        };
        let name = ts::text(*left, self.source).trim_start_matches('$').to_ascii_lowercase();
        if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            scope.vars.insert(name, value);
        }
    }

    fn foreach(&mut self, node: Node<'t>, scope: &mut Scope) {
        let children = ts::named_children(node);
        let Some(var) = children.iter().find(|n| n.kind() == kinds::VARIABLE).map(|v| ts::text(*v, self.source).trim_start_matches('$').to_ascii_lowercase()) else { return };
        let Some(body) = children.iter().find(|n| n.kind() == "statement_block") else { return };
        let items: Vec<Value> = children
            .iter()
            .filter(|n| n.kind() != kinds::VARIABLE && n.kind() != "statement_block")
            .flat_map(|n| self.list(*n, scope))
            .collect();
        let mut after = scope.clone();
        if !items.is_empty() && items.len() <= MAX_LOOP_ITEMS && items.iter().all(|v| v.known().is_some()) {
            for item in items {
                let mut iteration = scope.clone();
                iteration.vars.insert(var.clone(), item);
                self.statements(*body, &mut iteration);
                after.merge_uncertain(&iteration);
            }
        } else {
            let mut iteration = scope.clone();
            iteration.vars.insert(var.clone(), Value::Unknown);
            self.statements(*body, &mut iteration);
            after.merge_uncertain(&iteration);
        }
        after.vars.insert(var, Value::Unknown);
        *scope = after;
    }

    /// The items of a comma array expression, or the single value.
    fn list(&mut self, node: Node<'t>, scope: &mut Scope) -> Vec<Value> {
        match node.kind() {
            "array_literal_expression" => ts::named_children(node).into_iter().flat_map(|c| self.list(c, scope)).collect(),
            "pipeline" | "logical_expression" | "bitwise_expression" | "comparison_expression" | "additive_expression" | "multiplicative_expression" | "format_expression" | "range_expression" | "unary_expression"
                if ts::named_children(node).len() == 1 =>
            {
                self.list(ts::named_children(node)[0], scope)
            }
            _ => vec![self.value(node, scope)],
        }
    }

    fn value(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let text = ts::text(node, self.source);
        match node.kind() {
            "verbatim_string_characters" | "verbatim_string_literal" => {
                Value::Known(text.trim_start_matches('\'').trim_end_matches('\'').replace("''", "'"))
            }
            "verbatim_here_string_characters" | "verbatim_here_string_literal" => Value::Known(here_string_body(text)),
            "expandable_string_literal" | "expandable_here_string_literal" => {
                let body = if node.kind().contains("here") { here_string_body(text) } else { text.trim_matches('"').to_string() };
                self.expand(&body, node, scope)
            }
            "generic_token" | "command_argument" | "decimal_integer_literal" => {
                if text.contains(['*', '?', '[']) { Value::Unknown } else { Value::Known(text.replace('`', "")) }
            }
            k if k == kinds::VARIABLE => self.lookup(text, scope),
            k if k == kinds::SUB_EXPRESSION => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
                Value::Unknown
            }
            k if k == kinds::INVOKATION => self.invokation(node, scope),
            k if k == kinds::COMMAND => self.command_value(node, scope),
            k if k == kinds::PIPELINE && ts::named_children(node).len() == 1 => self.value(ts::named_children(node)[0], scope),
            _ => match ts::named_children(node).as_slice() {
                [only] => self.value(*only, scope),
                _ => {
                    self.statements(node, scope);
                    Value::Unknown
                }
            },
        }
    }

    fn lookup(&self, text: &str, scope: &Scope) -> Value {
        let name = text.trim_start_matches('$').trim_start_matches('{').trim_end_matches('}').to_ascii_lowercase();
        match name.as_str() {
            "pwd" => scope.cwd.clone().map_or(Value::Unknown, Value::Known),
            n if n.contains(':') => Value::Unknown,
            n => scope.vars.get(n).cloned().unwrap_or(Value::Unknown),
        }
    }

    /// Resolve `$name` and `${name}` in an expandable string; `$(...)` and any
    /// unbound variable make it unknown.
    fn expand(&mut self, body: &str, node: Node<'t>, scope: &mut Scope) -> Value {
        for sub in ts::named_children(node) {
            if sub.kind() == kinds::SUB_EXPRESSION {
                let mut inner = scope.clone();
                self.statements(sub, &mut inner);
                return Value::Unknown;
            }
        }
        let mut out = String::new();
        let mut chars = body.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '`' => out.extend(chars.next().map(|(_, c)| c)),
                '$' => {
                    let rest = &body[i + 1..];
                    let name: String = if let Some(braced) = rest.strip_prefix('{') {
                        braced.split('}').next().unwrap_or("").to_string()
                    } else {
                        rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':').collect()
                    };
                    if name.is_empty() {
                        out.push('$');
                        continue;
                    }
                    let consumed = if rest.starts_with('{') { name.len() + 2 } else { name.len() };
                    for _ in 0..consumed {
                        chars.next();
                    }
                    match self.lookup(&name, scope) {
                        Value::Known(v) => out.push_str(&v),
                        Value::Unknown => return Value::Unknown,
                    }
                }
                c => out.push(c),
            }
        }
        Value::Known(out)
    }

    fn invokation(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let children = ts::named_children(node);
        let type_name = children.iter().find(|n| n.kind() == kinds::TYPE_LITERAL).map(|t| ts::text(*t, self.source).trim_matches(['[', ']']).to_ascii_lowercase());
        let method = children.iter().find(|n| n.kind() == kinds::MEMBER_NAME).map(|m| ts::text(*m, self.source).to_ascii_lowercase());
        let args: Vec<Value> = children
            .iter()
            .find(|n| n.kind() == kinds::ARGUMENT_LIST)
            .map(|list| {
                let mut out = Vec::new();
                let mut stack = ts::named_children(*list);
                while let Some(n) = stack.first().copied() {
                    stack.remove(0);
                    if n.kind() == "argument_expression_list" {
                        stack.splice(0..0, ts::named_children(n));
                    } else {
                        out.push(self.value(n, scope));
                    }
                }
                out
            })
            .unwrap_or_default();
        let (Some(type_name), Some(method)) = (type_name, method) else {
            return Value::Unknown;
        };
        let type_name = type_name.strip_prefix("system.").unwrap_or(&type_name).to_string();
        let at = self.at(node);
        let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Unknown);
        match (type_name.as_str(), method.as_str()) {
            ("io.path", "combine") => {
                let parts: Option<Vec<&str>> = args.iter().map(Value::known).collect();
                parts.map_or(Value::Unknown, |p| Value::Known(p.join("/")))
            }
            ("io.file", "writealltext" | "writealllines" | "writeallbytes" | "create" | "createtext" | "openwrite") => {
                self.a.file_effect(FileOp::Overwrite, &arg(0), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "appendalltext" | "appendalllines" | "appendtext") => {
                self.a.file_effect(FileOp::Append, &arg(0), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "delete") => {
                self.a.file_effect(FileOp::Delete, &arg(0), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "move" | "replace") => {
                self.a.file_effect(FileOp::Rename, &arg(0), scope.cwd.as_deref(), at.clone());
                self.a.file_effect(FileOp::Rename, &arg(1), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "copy") => {
                self.a.file_effect(FileOp::Copy, &arg(1), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.directory", "delete") if args.len() > 1 => {
                self.a.tree_effect(&arg(0), false, scope.cwd.as_deref(), "[IO.Directory]::Delete", at);
                Value::Unknown
            }
            ("io.directory", "delete") => {
                self.a.file_effect(FileOp::Delete, &arg(0), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.directory", "move") => {
                self.a.tree_effect(&arg(0), false, scope.cwd.as_deref(), "[IO.Directory]::Move", at.clone());
                self.a.tree_effect(&arg(1), false, scope.cwd.as_deref(), "[IO.Directory]::Move", at);
                Value::Unknown
            }
            ("io.streamwriter", "new") => {
                self.a.file_effect(FileOp::Overwrite, &arg(0), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            _ => Value::Unknown,
        }
    }

    /// A command used as a value: `Join-Path` and `Get-Location` produce one;
    /// anything else runs for its effects.
    fn command_value(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let (name, elements) = self.command_parts(node, scope);
        if let Some(Value::Known(name)) = &name
            && let Some(c) = cmdlet(name)
        {
            match c.verb {
                Verb::JoinPath => {
                    let bound = bind(&c, &elements);
                    return match (bound.get("path").and_then(|v| v.known()), bound.get("childpath").and_then(|v| v.known())) {
                        (Some(p), Some(child)) => Value::Known(format!("{}/{}", p.trim_end_matches(['/', '\\']), child)),
                        _ => Value::Unknown,
                    };
                }
                Verb::GetLocation => return scope.cwd.clone().map_or(Value::Unknown, Value::Known),
                _ => {}
            }
        }
        self.command(node, scope, Stdin::None);
        Value::Unknown
    }

    /// `(program word, [(parameter or None, value)])`.
    fn command_parts(&mut self, node: Node<'t>, scope: &mut Scope) -> (Option<Value>, Vec<(Option<String>, Word)>) {
        let mut name = None;
        let mut elements = Vec::new();
        let mut pending: Option<String> = None;
        for child in ts::named_children(node) {
            match child.kind() {
                k if k == kinds::COMMAND_NAME || k == kinds::COMMAND_NAME_EXPR => name = Some(self.value(child, scope)),
                k if k == kinds::COMMAND_ELEMENTS => {
                    for el in ts::named_children(child) {
                        match el.kind() {
                            k if k == kinds::PARAMETER => {
                                if let Some(p) = pending.take() {
                                    elements.push((Some(p), Word { value: Value::Known("$true".into()), typed: String::new(), span: el.byte_range() }));
                                }
                                pending = Some(ts::text(el, self.source).trim_start_matches('-').to_ascii_lowercase());
                            }
                            k if k == kinds::REDIRECTION => self.redirection(el, scope),
                            "command_argument_sep" => {}
                            _ => {
                                for value in self.list(el, scope) {
                                    let word = Word { typed: value.known().map_or_else(|| ts::text(el, self.source).to_string(), str::to_string), value, span: el.byte_range() };
                                    elements.push((pending.take(), word));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(p) = pending {
            elements.push((Some(p), Word { value: Value::Known("$true".into()), typed: String::new(), span: node.byte_range() }));
        }
        (name, elements)
    }

    fn command(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        let (name, elements) = self.command_parts(node, scope);
        let at = self.at(node);
        let Some(name_value) = name else { return };
        let Some(name) = name_value.known().map(str::to_string) else {
            self.a.uncertain(UncertaintyKind::UnresolvedInvocation, "a command invoked through a value that could not be determined", at);
            return;
        };
        if let Some(body) = scope.functions.get(&name.to_ascii_lowercase()).cloned() {
            if let Some(body) = self.root.descendant_for_byte_range(body.start, body.end) {
                let mut call = scope.clone();
                self.statements(body, &mut call);
            }
            return;
        }
        let Some(c) = cmdlet(&name) else {
            let words = std::iter::once(Word { value: name_value.clone(), typed: name.clone(), span: node.byte_range() })
                .chain(elements.into_iter().flat_map(|(param, word)| {
                    param.map(|p| Word { value: Value::Known(format!("-{p}")), typed: format!("-{p}"), span: word.span.clone() }).into_iter().chain((!word.typed.is_empty()).then_some(word))
                }))
                .collect();
            let raw = RawInvocation { words, stdin, cwd: scope.cwd.clone(), language: Language::PowerShell, location: at };
            self.a.invocation(raw, self.frame);
            return;
        };
        let bound = bind(&c, &elements);
        let get = |k: &str| bound.get(k).cloned();
        let cwd = scope.cwd.clone();
        let file = |w: &mut Self, op: FileOp, v: Option<Value>, literal: bool| {
            let v = v.unwrap_or(Value::Unknown);
            let v = match &v {
                Value::Known(p) if !literal && p.contains(['*', '?', '[']) => Value::Unknown,
                _ => v,
            };
            w.a.file_effect(op, &v, cwd.as_deref(), w.at(node));
        };
        let switch = |k: &str| bound.contains_key(k);
        let literal = elements.iter().any(|(p, _)| p.as_deref().is_some_and(|p| "literalpath".starts_with(p) && p.len() > 1));
        match c.verb {
            Verb::SetContent | Verb::ClearContent | Verb::ExportFile => file(self, FileOp::Overwrite, get("path"), literal),
            Verb::AddContent => file(self, FileOp::Append, get("path"), literal),
            Verb::OutFile | Verb::TeeObject => {
                let target = get("filepath").or_else(|| get("path"));
                file(self, if switch("append") { FileOp::Append } else { FileOp::Overwrite }, target, true);
            }
            Verb::NewItem => {
                let kind = get("itemtype").and_then(|v| v.known().map(str::to_ascii_lowercase));
                if !matches!(kind.as_deref(), Some("directory" | "dir")) {
                    let target = match (get("path"), get("name")) {
                        (Some(Value::Known(p)), Some(Value::Known(n))) => Some(Value::Known(format!("{p}/{n}"))),
                        (None, Some(n)) => Some(n),
                        (p, _) => p,
                    };
                    file(self, FileOp::Create, target, literal);
                }
            }
            Verb::RemoveItem => {
                if switch("recurse") {
                    let at = self.at(node);
                    self.a.tree_effect(&get("path").unwrap_or(Value::Unknown), false, cwd.as_deref(), "Remove-Item -Recurse", at);
                } else {
                    for (_, w) in elements.iter().filter(|(p, _)| p.as_deref().is_none_or(|p| "path".starts_with(p) || "literalpath".starts_with(p))) {
                        file(self, FileOp::Delete, Some(w.value.clone()), literal);
                    }
                }
            }
            Verb::MoveItem => {
                file(self, FileOp::Rename, get("path"), literal);
                file(self, FileOp::Rename, get("destination"), true);
            }
            Verb::RenameItem => {
                let path = get("path");
                file(self, FileOp::Rename, path.clone(), literal);
                let renamed = match (path.as_ref().and_then(Value::known), get("newname")) {
                    (Some(p), Some(Value::Known(n))) => Value::Known(match paths::parent(p) { Some(dir) => format!("{dir}/{n}"), None => n }),
                    _ => Value::Unknown,
                };
                file(self, FileOp::Rename, Some(renamed), true);
            }
            Verb::CopyItem => {
                if switch("recurse") {
                    let at = self.at(node);
                    self.a.tree_effect(&get("destination").unwrap_or(Value::Unknown), false, cwd.as_deref(), "Copy-Item -Recurse", at);
                } else {
                    file(self, FileOp::Copy, get("destination"), true);
                }
            }
            Verb::ExpandArchive => {
                let at = self.at(node);
                self.a.tree_effect(&get("destinationpath").unwrap_or(Value::Known(".".into())), false, cwd.as_deref(), "Expand-Archive", at);
            }
            Verb::WebRequest => {
                if let Some(out) = get("outfile") {
                    file(self, FileOp::Overwrite, Some(out), true);
                }
            }
            Verb::SetLocation => {
                scope.cwd = match get("path") {
                    Some(v) => match paths::resolve(&v, scope.cwd.as_deref(), self.a.ctx.path_style) {
                        crate::model::Target::Path(d) => Some(d),
                        crate::model::Target::Unresolved => None,
                    },
                    None => None,
                };
            }
            Verb::PopLocation => scope.cwd = None,
            Verb::InvokeExpression => match get("command").or_else(|| match &stdin { Stdin::Source { value, .. } => Some(value.clone()), Stdin::None => None }) {
                Some(Value::Known(source)) => {
                    let child = Frame { depth: self.frame.depth + 1, script_args: Vec::new(), base: Some(self.at(node)), cwd: scope.cwd.clone() };
                    self.a.source(Language::PowerShell, &source, child);
                }
                _ => {
                    let at = self.at(node);
                    self.a.uncertain(UncertaintyKind::UnresolvedWrite, "`Invoke-Expression` of source that could not be determined", at);
                }
            },
            Verb::JoinPath | Verb::GetLocation => {}
        }
    }
}

/// Bind named and positional arguments to a cmdlet's canonical parameters.
///
/// A switch takes no value, so a word the element list paired with a switch
/// (`-Recurse build`) is returned to the positional queue. Positional words
/// fill the positional parameters not already named.
fn bind(c: &Cmdlet, elements: &[(Option<String>, Word)]) -> HashMap<&'static str, Value> {
    let mut out = HashMap::new();
    let mut positional_words: Vec<&Word> = Vec::new();
    for (param, word) in elements {
        let Some(p) = param else {
            positional_words.push(word);
            continue;
        };
        match resolve_param(c, p) {
            Some(name) if c.switches.contains(&name) => {
                out.insert(name, Value::Known("$true".into()));
                if !word.typed.is_empty() {
                    positional_words.push(word);
                }
            }
            Some(name) => {
                out.insert(name, word.value.clone());
            }
            None => {}
        }
    }
    let open: Vec<&'static str> = c.positional.iter().copied().filter(|n| !out.contains_key(n)).collect();
    for (name, word) in open.into_iter().zip(positional_words) {
        out.insert(name, word.value.clone());
    }
    out
}

/// A parameter as typed, resolved to its canonical name: exact name or alias
/// first, then an unambiguous prefix.
fn resolve_param(c: &Cmdlet, typed: &str) -> Option<&'static str> {
    let p = typed.trim_end_matches(':');
    if let Some((_, canonical)) = c.aliases.iter().find(|(alias, _)| *alias == p)
        && c.params.contains(canonical)
    {
        return Some(canonical);
    }
    let names = || c.params.iter().chain(c.switches).copied();
    if let Some(exact) = names().find(|n| *n == p) {
        return Some(exact);
    }
    let mut prefixed = names().filter(|n| n.starts_with(p));
    match (prefixed.next(), prefixed.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

fn here_string_body(text: &str) -> String {
    let inner = text.trim_start_matches(['@']).trim_start_matches(['\'', '"']).trim_end_matches('@').trim_end_matches(['\'', '"']);
    let inner = inner.strip_prefix("\r\n").or_else(|| inner.strip_prefix('\n')).unwrap_or(inner);
    let inner = inner.strip_suffix("\r\n").or_else(|| inner.strip_suffix('\n')).unwrap_or(inner);
    inner.to_string()
}
```

`item_cmdlets_and_aliases` covers a switch before and after its positional value (`Remove-Item build -Recurse -Force`, `ni new.txt -ItemType File`). The here-string test relies on the pipeline's first element being a here-string expression whose value becomes stdin for `python -`; `pipeline` handles that in its `_` arm.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS after aligning `kinds` with Step 1's printed trees.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): analyze PowerShell cmdlets, .NET APIs and recovery"
```

---

### Task 9: JavaScript and TypeScript adapter

**Files:**
- Modify: `crates/devkit-command/src/js.rs`

**Interfaces:**
- Consumes: the same `Analyzer` surface; `Frame.script_args` as `process.argv`.
- Produces: `js::walk(&mut Analyzer, Language, &str, &Frame)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use crate::{
        model::UncertaintyKind,
        testutil::{bash, targets},
    };

    fn unresolved(a: &crate::Analysis) -> bool {
        a.uncertainties.iter().any(|u| u.kind == UncertaintyKind::UnresolvedWrite) || targets(a).contains(&"?".to_string())
    }

    #[test]
    fn node_fs_writes_through_require_and_imports() {
        assert_eq!(targets(&bash("node -e \"require('fs').writeFileSync('a.txt', 'x')\"")), ["/repo/a.txt"]);
        assert_eq!(targets(&bash("node -e \"const { appendFileSync: add } = require('node:fs'); add('b.txt', 'x')\"")), ["/repo/b.txt"]);
        assert_eq!(
            targets(&bash("bun -e \"import { writeFile } from 'node:fs/promises'; await writeFile('c.txt', 'x')\"")),
            ["/repo/c.txt"]
        );
    }

    #[test]
    fn path_join_templates_and_argv_resolve() {
        assert_eq!(
            targets(&bash("node -e \"const fs = require('fs'); const path = require('path'); const d = 'gen'; fs.writeFileSync(path.join(d, `${'x'}.txt`), '')\"")),
            ["/repo/gen/x.txt"]
        );
        assert_eq!(targets(&bash("f=out.txt; node -e \"require('fs').writeFileSync(process.argv[1], '')\" \"$f\"")), ["/repo/out.txt"]);
    }

    #[test]
    fn bun_and_deno_writers() {
        assert_eq!(targets(&bash("bun -e \"await Bun.write('d.txt', 'x')\"")), ["/repo/d.txt"]);
        assert_eq!(targets(&bash("deno eval \"await Deno.writeTextFile('e.txt', 'x')\"")), ["/repo/e.txt"]);
    }

    #[test]
    fn shadowing_drops_the_api_binding() {
        let a = bash("node -e \"const fs = require('fs'); { const fs = { writeFileSync() {} }; fs.writeFileSync('a.txt') }\"");
        assert!(a.file_effects.is_empty(), "{:?}", a.file_effects);
    }

    #[test]
    fn a_method_name_alone_is_not_a_known_write() {
        let a = bash("node -e \"thing.writeFile('a.txt')\"");
        assert!(a.file_effects.iter().all(|e| e.target == crate::Target::Unresolved));
        assert!(unresolved(&a));
    }

    #[test]
    fn resolution_probes_and_reads_are_silent() {
        let a = bash("node -e \"console.log(require.resolve('vite')); const fs = require('fs'); fs.readFileSync('a.json', 'utf8'); fs.existsSync('b')\"");
        assert!(a.file_effects.is_empty() && a.uncertainties.is_empty(), "{a:?}");
    }

    #[test]
    fn child_process_commands_are_analyzed_when_constant() {
        assert_eq!(targets(&bash("node -e \"require('child_process').execSync('rm a.txt')\"")), ["/repo/a.txt"]);
        assert_eq!(targets(&bash("node -e \"require('child_process').spawnSync('rm', ['b.txt'])\"")), ["/repo/b.txt"]);
        assert!(unresolved(&bash("node -e \"require('child_process').execSync(process.env.CMD)\"")));
    }

    #[test]
    fn typescript_syntax_parses_as_syntax() {
        assert_eq!(targets(&bash("bun -e \"import fs from 'node:fs'; const p: string = 'f.txt'; fs.writeFileSync(p as string, '')\"")), ["/repo/f.txt"]);
    }

    #[test]
    fn dynamic_code_is_unresolved() {
        assert!(unresolved(&bash("node -e \"eval(process.env.X)\"")));
        assert!(unresolved(&bash("node -e \"import(process.env.M)\"")));
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run -p devkit-command js`
Expected: FAIL.

- [ ] **Step 3: Implement**

`crates/devkit-command/src/js.rs` (keep tests). The structure mirrors `python.rs`: a value type, a scope with lexical blocks, statements in order, and calls dispatched on the resolved callee.

```rust
//! JavaScript and TypeScript run by Node, Bun, Deno, tsx and ts-node: module
//! bindings, path construction, and the filesystem and process calls that can
//! write. TypeScript is parsed with its own grammar, never stripped with text
//! substitution.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    model::{FileOp, Language, Location, UncertaintyKind, Value},
    normalize, paths, ts,
};

/// Modules the adapter models or knows to be free of file writes, by
/// specifier with any `node:` prefix removed.
const KNOWN_MODULES: &[&str] = &["fs", "fs/promises", "path", "child_process", "os", "util", "url", "crypto", "assert", "events", "process", "module", "readline", "stream", "buffer", "zlib", "querystring"];

const WRITES: &[(&str, FileOp, usize)] = &[
    ("writeFileSync", FileOp::Overwrite, 0), ("writeFile", FileOp::Overwrite, 0), ("appendFileSync", FileOp::Append, 0),
    ("appendFile", FileOp::Append, 0), ("createWriteStream", FileOp::Overwrite, 0), ("unlinkSync", FileOp::Delete, 0),
    ("unlink", FileOp::Delete, 0), ("truncateSync", FileOp::Overwrite, 0), ("truncate", FileOp::Overwrite, 0),
    ("copyFileSync", FileOp::Copy, 1), ("copyFile", FileOp::Copy, 1), ("symlinkSync", FileOp::Create, 1), ("symlink", FileOp::Create, 1),
];

const WRITE_METHOD_NAMES: &[&str] = &["writeFileSync", "writeFile", "appendFileSync", "appendFile", "createWriteStream", "unlinkSync", "unlink", "renameSync", "rename", "rmSync", "rm", "copyFileSync", "copyFile", "cpSync", "cp", "write", "writeTextFile"];

#[derive(Debug, Clone, PartialEq)]
enum Js {
    Str(String),
    Num(i64),
    Array(Vec<Js>),
    /// A modeled module or member by qualified name: `fs.writeFileSync`.
    Api(String),
    Method(Box<Js>, String),
    Argv,
    Env,
    Foreign(String),
    Data,
    Unknown,
}

impl Js {
    fn as_value(&self) -> Value {
        match self {
            Js::Str(s) => Value::Known(s.clone()),
            _ => Value::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Scope {
    names: HashMap<String, Js>,
    argv: Vec<Value>,
    cwd: Option<String>,
}

pub(crate) fn walk(a: &mut Analyzer<'_>, language: Language, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(language, source) else {
        a.uncertain(UncertaintyKind::ParseError, "script source did not parse", frame.locate(0..source.len()));
        return;
    };
    let mut scope = Scope { argv: frame.script_args.clone(), cwd: frame.cwd.clone(), ..Scope::default() };
    let mut w = Walker { a, source, frame };
    w.statements(tree.root_node(), &mut scope);
}

struct Walker<'a, 'c, 's> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
}

impl<'t> Walker<'_, '_, '_> {
    fn at(&self, node: Node<'_>) -> Location {
        self.frame.locate(node.byte_range())
    }

    fn unresolved(&mut self, node: Node<'_>, detail: impl Into<String>) {
        let at = self.at(node);
        self.a.uncertain(UncertaintyKind::UnresolvedWrite, detail, at);
    }

    fn statements(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            self.statement(child, scope);
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope) {
        if self.a.budget.visit().is_err() {
            return;
        }
        if node.has_error() && node.kind() != "program" && node.kind() != "statement_block" {
            let at = self.at(node);
            self.a.uncertain(UncertaintyKind::ParseError, "a script statement could not be parsed", at);
            return;
        }
        match node.kind() {
            "comment" | "empty_statement" | "type_alias_declaration" | "interface_declaration" => {}
            "import_statement" => self.import(node, scope),
            "lexical_declaration" | "variable_declaration" => {
                for declarator in ts::named_children(node).into_iter().filter(|d| d.kind() == "variable_declarator") {
                    let value = declarator.child_by_field_name("value").map_or(Js::Unknown, |v| self.eval(v, scope));
                    if let Some(name) = declarator.child_by_field_name("name") {
                        self.bind(name, value, scope);
                    }
                }
            }
            "statement_block" => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
            }
            "function_declaration" | "class_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    scope.names.insert(ts::text(name, self.source).to_string(), Js::Data);
                }
            }
            "if_statement" | "for_statement" | "for_in_statement" | "while_statement" | "do_statement" | "try_statement" | "switch_statement" => {
                let mut branch = scope.clone();
                for child in ts::named_children(node) {
                    if child.kind().ends_with("statement") || child.kind() == "statement_block" || child.kind().ends_with("clause") || child.kind() == "switch_body" {
                        self.statement(child, &mut branch);
                    } else {
                        self.eval(child, &mut branch);
                    }
                }
                for (name, value) in &branch.names {
                    if scope.names.get(name) != Some(value) {
                        scope.names.insert(name.clone(), Js::Unknown);
                    }
                }
            }
            _ => {
                for child in ts::named_children(node) {
                    self.eval(child, scope);
                }
            }
        }
    }

    fn module(specifier: &str) -> Js {
        let s = specifier.strip_prefix("node:").unwrap_or(specifier);
        if KNOWN_MODULES.contains(&s) { Js::Api(s.to_string()) } else { Js::Foreign(s.to_string()) }
    }

    fn import(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(source) = node.child_by_field_name("source") else { return };
        let module = match self.eval(source, scope) {
            Js::Str(s) => Self::module(&s),
            _ => Js::Unknown,
        };
        for clause in ts::named_children(node).into_iter().filter(|c| c.kind() == "import_clause") {
            for part in ts::named_children(clause) {
                match part.kind() {
                    "identifier" => {
                        scope.names.insert(ts::text(part, self.source).to_string(), module.clone());
                    }
                    "namespace_import" => {
                        if let Some(id) = ts::named_children(part).into_iter().find(|n| n.kind() == "identifier") {
                            scope.names.insert(ts::text(id, self.source).to_string(), module.clone());
                        }
                    }
                    "named_imports" => {
                        for spec in ts::named_children(part).into_iter().filter(|s| s.kind() == "import_specifier") {
                            let name = spec.child_by_field_name("name").map(|n| ts::text(n, self.source).to_string());
                            let alias = spec.child_by_field_name("alias").map(|n| ts::text(n, self.source).to_string());
                            if let Some(name) = name {
                                let bound = alias.unwrap_or_else(|| name.clone());
                                scope.names.insert(bound, member(&module, &name));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn bind(&mut self, pattern: Node<'t>, value: Js, scope: &mut Scope) {
        match pattern.kind() {
            "identifier" => {
                scope.names.insert(ts::text(pattern, self.source).to_string(), value);
            }
            "object_pattern" => {
                for prop in ts::named_children(pattern) {
                    match prop.kind() {
                        "shorthand_property_identifier_pattern" => {
                            let name = ts::text(prop, self.source).to_string();
                            scope.names.insert(name.clone(), member(&value, &name));
                        }
                        "pair_pattern" => {
                            if let (Some(key), Some(target)) = (prop.child_by_field_name("key"), prop.child_by_field_name("value")) {
                                let key = ts::text(key, self.source).trim_matches(['\'', '"']).to_string();
                                self.bind(target, member(&value, &key), scope);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "array_pattern" => {
                for (i, part) in ts::named_children(pattern).into_iter().enumerate() {
                    let item = match &value { Js::Array(items) => items.get(i).cloned().unwrap_or(Js::Unknown), _ => Js::Unknown };
                    self.bind(part, item, scope);
                }
            }
            _ => {}
        }
    }

    fn eval(&mut self, node: Node<'t>, scope: &mut Scope) -> Js {
        if self.a.budget.visit().is_err() {
            return Js::Unknown;
        }
        let text = ts::text(node, self.source);
        match node.kind() {
            "string" => Js::Str(text[1..text.len().saturating_sub(1)].to_string()),
            "template_string" => {
                let mut out = String::new();
                let mut last = node.start_byte() + 1;
                for sub in ts::named_children(node).into_iter().filter(|n| n.kind() == "template_substitution") {
                    out.push_str(&self.source[last..sub.start_byte()]);
                    last = sub.end_byte();
                    match ts::named_children(sub).first().map(|e| self.eval(*e, scope)) {
                        Some(Js::Str(s)) => out.push_str(&s),
                        Some(Js::Num(n)) => out.push_str(&n.to_string()),
                        _ => return Js::Unknown,
                    }
                }
                out.push_str(&self.source[last..node.end_byte() - 1]);
                Js::Str(out)
            }
            "number" => text.parse().map_or(Js::Data, Js::Num),
            "true" | "false" | "null" | "undefined" | "arrow_function" | "function_expression" | "function" | "regex" => Js::Data,
            "identifier" => match scope.names.get(text) {
                Some(v) => v.clone(),
                None => match text {
                    "require" => Js::Api("require".into()),
                    "process" => Js::Api("process".into()),
                    "console" | "JSON" | "Math" | "Object" | "Array" | "String" | "Number" | "Promise" | "Date" | "Map" | "Set" | "Error" => Js::Data,
                    "Bun" => Js::Api("Bun".into()),
                    "Deno" => Js::Api("Deno".into()),
                    "eval" | "Function" => Js::Api(text.into()),
                    "__dirname" | "__filename" => Js::Unknown,
                    _ => Js::Unknown,
                },
            },
            "member_expression" => {
                let object = node.child_by_field_name("object").map_or(Js::Unknown, |o| self.eval(o, scope));
                let property = node.child_by_field_name("property").map_or("", |p| ts::text(p, self.source)).to_string();
                match (&object, property.as_str()) {
                    (Js::Api(p), "argv") if p == "process" => Js::Argv,
                    (Js::Api(p), "env") if p == "process" => Js::Env,
                    (Js::Api(p), "promises") if p == "fs" => Js::Api("fs/promises".into()),
                    _ => member(&object, &property),
                }
            }
            "subscript_expression" => {
                let object = node.child_by_field_name("object").map_or(Js::Unknown, |o| self.eval(o, scope));
                let index = node.child_by_field_name("index").map_or(Js::Unknown, |i| self.eval(i, scope));
                match (object, index) {
                    (Js::Argv, Js::Num(i)) => match usize::try_from(i).ok().and_then(|i| scope.argv.get(i)) {
                        Some(Value::Known(s)) => Js::Str(s.clone()),
                        _ => Js::Unknown,
                    },
                    (Js::Array(items), Js::Num(i)) => usize::try_from(i).ok().and_then(|i| items.get(i).cloned()).unwrap_or(Js::Unknown),
                    _ => Js::Unknown,
                }
            }
            "binary_expression" => {
                let left = node.child_by_field_name("left").map_or(Js::Unknown, |l| self.eval(l, scope));
                let right = node.child_by_field_name("right").map_or(Js::Unknown, |r| self.eval(r, scope));
                let op = node.child_by_field_name("operator").map_or("", |o| ts::text(o, self.source));
                match (op, left, right) {
                    ("+", Js::Str(l), Js::Str(r)) => Js::Str(l + &r),
                    _ => Js::Data,
                }
            }
            "array" => Js::Array(ts::named_children(node).into_iter().map(|c| self.eval(c, scope)).collect()),
            "call_expression" => self.call(node, scope),
            "await_expression" | "parenthesized_expression" | "as_expression" | "satisfies_expression" | "non_null_expression" | "type_assertion" => {
                ts::named_children(node).into_iter().map(|c| self.eval(c, scope)).find(|v| *v != Js::Data).unwrap_or(Js::Data)
            }
            "assignment_expression" => {
                let value = node.child_by_field_name("right").map_or(Js::Unknown, |r| self.eval(r, scope));
                if let Some(left) = node.child_by_field_name("left") {
                    self.bind(left, value.clone(), scope);
                }
                value
            }
            "import" => Js::Api("import".into()),
            _ => {
                for child in ts::named_children(node) {
                    self.eval(child, scope);
                }
                Js::Unknown
            }
        }
    }

    fn call(&mut self, node: Node<'t>, scope: &mut Scope) -> Js {
        let callee = node.child_by_field_name("function").map_or(Js::Unknown, |f| self.eval(f, scope));
        let args: Vec<(Js, Node<'t>)> = node
            .child_by_field_name("arguments")
            .map(|list| ts::named_children(list).into_iter().map(|a| (self.eval(a, scope), a)).collect())
            .unwrap_or_default();
        let arg = |i: usize| args.get(i).map_or(Js::Unknown, |(v, _)| v.clone());
        let cwd = scope.cwd.clone();
        let at = self.at(node);
        let api = match &callee {
            Js::Api(name) => Some(name.clone()),
            _ => None,
        };
        match (api.as_deref(), &callee) {
            (Some("require"), _) => match arg(0) {
                Js::Str(s) => Self::module(&s),
                _ => {
                    self.unresolved(node, "a `require` of a module that could not be determined");
                    Js::Unknown
                }
            },
            (Some("require.resolve"), _) => Js::Data,
            (Some("import"), _) => match arg(0) {
                Js::Str(s) => Self::module(&s),
                _ => {
                    self.unresolved(node, "a dynamic `import()` that could not be determined");
                    Js::Unknown
                }
            },
            (Some("eval" | "Function"), _) => {
                self.unresolved(node, "dynamically evaluated script source");
                Js::Unknown
            }
            (Some("path.join" | "path.posix.join"), _) => {
                let parts: Option<Vec<String>> = args.iter().map(|(v, _)| match v { Js::Str(s) => Some(s.clone()), _ => None }).collect();
                parts.filter(|p| !p.is_empty()).map_or(Js::Unknown, |p| Js::Str(p.iter().skip(1).fold(p[0].clone(), |acc, s| paths::join(&acc, s, self.a.ctx.path_style))))
            }
            (Some("path.resolve"), _) => {
                let parts: Option<Vec<String>> = args.iter().map(|(v, _)| match v { Js::Str(s) => Some(s.clone()), _ => None }).collect();
                match (parts, cwd) {
                    (Some(p), Some(dir)) => Js::Str(p.iter().fold(dir, |acc, s| if s.starts_with('/') { s.clone() } else { paths::join(&acc, s, self.a.ctx.path_style) })),
                    _ => Js::Unknown,
                }
            }
            (Some("path.dirname"), _) => match arg(0) { Js::Str(s) => Js::Str(paths::parent(&s).unwrap_or(".").to_string()), _ => Js::Unknown },
            (Some("path.basename"), _) => match arg(0) { Js::Str(s) => Js::Str(normalize::basename(&s).to_string()), _ => Js::Unknown },
            (Some("process.cwd"), _) => cwd.map_or(Js::Unknown, Js::Str),
            (Some("process.chdir"), _) => {
                scope.cwd = match arg(0) {
                    Js::Str(s) => match paths::resolve(&Value::Known(s), scope.cwd.as_deref(), self.a.ctx.path_style) {
                        crate::model::Target::Path(d) => Some(d),
                        crate::model::Target::Unresolved => None,
                    },
                    _ => None,
                };
                Js::Data
            }
            (Some(name), _) if name.starts_with("fs.") || name.starts_with("fs/promises.") => {
                let method = name.rsplit('.').next().unwrap_or("");
                if let Some((_, op, index)) = WRITES.iter().find(|(m, _, _)| *m == method) {
                    self.a.file_effect(*op, &arg(*index).as_value(), cwd.as_deref(), at);
                } else if matches!(method, "renameSync" | "rename") {
                    self.a.file_effect(FileOp::Rename, &arg(0).as_value(), cwd.as_deref(), at.clone());
                    self.a.file_effect(FileOp::Rename, &arg(1).as_value(), cwd.as_deref(), at);
                } else if matches!(method, "rmSync" | "rm" | "cpSync" | "cp") {
                    let recursive = args.get(if method.starts_with("cp") { 2 } else { 1 }).is_some_and(|(_, n)| ts::text(*n, self.source).contains("recursive: true"));
                    let target = if method.starts_with("cp") { arg(1) } else { arg(0) };
                    if recursive {
                        self.a.tree_effect(&target.as_value(), false, cwd.as_deref(), &format!("fs.{method}"), at);
                    } else {
                        let op = if method.starts_with("cp") { FileOp::Copy } else { FileOp::Delete };
                        self.a.file_effect(op, &target.as_value(), cwd.as_deref(), at);
                    }
                } else if matches!(method, "openSync" | "open") {
                    match arg(1) {
                        Js::Str(flags) if flags.contains(['w', 'a', '+']) => self.a.file_effect(FileOp::Overwrite, &arg(0).as_value(), cwd.as_deref(), at),
                        Js::Str(_) => {}
                        _ if args.len() < 2 => {}
                        _ => self.unresolved(node, "a file opened with flags that could not be determined"),
                    }
                }
                Js::Data
            }
            (Some("Bun.write"), _) => {
                self.a.file_effect(FileOp::Overwrite, &arg(0).as_value(), cwd.as_deref(), at);
                Js::Data
            }
            (Some(name), _) if name.starts_with("Deno.") => {
                let method = &name[5..];
                match method {
                    "writeTextFile" | "writeFile" => self.a.file_effect(FileOp::Overwrite, &arg(0).as_value(), cwd.as_deref(), at),
                    "remove" => self.a.file_effect(FileOp::Delete, &arg(0).as_value(), cwd.as_deref(), at),
                    "rename" => {
                        self.a.file_effect(FileOp::Rename, &arg(0).as_value(), cwd.as_deref(), at.clone());
                        self.a.file_effect(FileOp::Rename, &arg(1).as_value(), cwd.as_deref(), at);
                    }
                    "copyFile" => self.a.file_effect(FileOp::Copy, &arg(1).as_value(), cwd.as_deref(), at),
                    _ => {}
                }
                Js::Data
            }
            (Some("child_process.execSync" | "child_process.exec"), _) => {
                match arg(0) {
                    Js::Str(command) => {
                        let child = Frame { depth: self.frame.depth + 1, script_args: Vec::new(), base: Some(at), cwd: scope.cwd.clone() };
                        self.a.source(Language::Bash, &command, child);
                    }
                    _ => self.unresolved(node, "a shell command that could not be determined"),
                }
                Js::Data
            }
            (Some("child_process.spawnSync" | "child_process.spawn" | "child_process.execFileSync" | "child_process.execFile"), _) => {
                let mut argv = vec![arg(0).as_value()];
                match arg(1) {
                    Js::Array(items) => argv.extend(items.iter().map(Js::as_value)),
                    Js::Unknown if args.len() > 1 => argv.push(Value::Unknown),
                    _ => {}
                }
                self.run_argv(node, argv, scope.cwd.clone());
                Js::Data
            }
            (Some("Bun.spawn" | "Bun.spawnSync"), _) => {
                match arg(0) {
                    Js::Array(items) => self.run_argv(node, items.iter().map(Js::as_value).collect(), scope.cwd.clone()),
                    _ => self.unresolved(node, "a `Bun.spawn` command that could not be determined"),
                }
                Js::Data
            }
            (_, Js::Foreign(module)) => {
                self.unresolved(node, format!("a call into `{module}`, which devkit does not model"));
                Js::Unknown
            }
            (_, Js::Method(receiver, method)) if matches!(**receiver, Js::Unknown) && WRITE_METHOD_NAMES.contains(&method.as_str()) => {
                self.unresolved(node, format!("`.{method}()` on a value whose type could not be determined"));
                Js::Unknown
            }
            _ => Js::Unknown,
        }
    }

    fn run_argv(&mut self, node: Node<'t>, argv: Vec<Value>, cwd: Option<String>) {
        let words = argv.into_iter().map(|value| Word { typed: value.known().unwrap_or("?").to_string(), value, span: node.byte_range() }).collect();
        let raw = RawInvocation { words, stdin: Stdin::None, cwd, language: Language::JavaScript, location: self.at(node) };
        self.a.invocation(raw, self.frame);
    }
}

fn member(object: &Js, property: &str) -> Js {
    match object {
        Js::Api(base) => Js::Api(format!("{base}.{property}")),
        Js::Foreign(module) => Js::Foreign(module.clone()),
        Js::Env => Js::Unknown,
        Js::Data => Js::Data,
        other => Js::Method(Box::new(other.clone()), property.to_string()),
    }
}
```

In `shadowing_drops_the_api_binding`, the inner `const fs = { ... }` binds `fs` to `Js::Unknown` because an object literal evaluates through the `_` arm; `fs.writeFileSync` then becomes `Method(Unknown, "writeFileSync")` and reports an uncertainty rather than a file effect. The test asserts the absence of file effects, which is the property the spec requires: shadowing invalidates the binding.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): analyze JavaScript and TypeScript file and process calls"
```

---

### Task 10: fish adapter

**Files:**
- Modify: `crates/devkit-command/src/fish.rs`

**Interfaces:**
- Consumes: the same `Analyzer` surface.
- Produces: `fish::walk(&mut Analyzer, &str, &Frame)`.

fish is in v1 so read-only `fish -c` probes are not unsupported-language findings. It needs redirects, commands, `set`, `cd`, pipes, conditionals, `for`, and command substitution. It has no heredocs.

- [ ] **Step 1: Pin node shapes**

```rust
#[cfg(test)]
mod shapes {
    use crate::{model::Language, ts};

    #[test]
    fn node_shapes_this_adapter_relies_on() {
        for (source, fragment) in [
            ("echo x > a.txt", "redirect"),
            ("set f a.txt", "command"),
            ("echo (pwd)", "command_substitution"),
            ("for f in a b; echo $f; end", "for_statement"),
            ("true; and echo x", "conditional_execution"),
            ("echo \"$f\"", "variable_expansion"),
        ] {
            let tree = ts::parse(Language::Fish, source).unwrap();
            let sexp = tree.root_node().to_sexp();
            assert!(sexp.contains(fragment), "{source:?}\n{sexp}");
        }
    }
}
```

Run: `cargo nextest run -p devkit-command fish::shapes --no-capture` and align fragments and the walker's kind strings with the printed trees (`redirect_statement`, `file_redirect`, `double_quote_string`, `single_quote_string`, `concatenation`, `pipe`).

- [ ] **Step 2: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use crate::{
        testutil::{ctx, programs, targets},
        Analysis, Dialect,
    };

    fn fish(source: &str) -> Analysis {
        crate::analyze(source, &ctx(Dialect::Fish))
    }

    #[test]
    fn redirects_set_and_cd() {
        assert_eq!(targets(&fish("echo x > a.txt; echo y >> b.txt")), ["/repo/a.txt", "/repo/b.txt"]);
        assert_eq!(targets(&fish("set f out.txt; echo x > $f")), ["/repo/out.txt"]);
        assert_eq!(targets(&fish("cd sub; and echo x > a.txt")), ["/repo/sub/a.txt"]);
    }

    #[test]
    fn catalog_commands_and_substitutions() {
        assert_eq!(targets(&fish("rm (echo a.txt)")), ["?"]);
        assert_eq!(targets(&fish("echo (touch t.txt)")), ["/repo/t.txt"]);
    }

    #[test]
    fn read_only_completion_probes_are_silent() {
        let a = fish("complete -C 'git che' | head -n 5");
        assert!(a.file_effects.is_empty() && a.uncertainties.is_empty(), "{a:?}");
        assert_eq!(programs(&a), ["complete", "head"]);
    }

    #[test]
    fn fish_c_from_bash_is_fish_source() {
        let a = crate::testutil::bash("fish -c 'echo x > f.txt'");
        assert_eq!(targets(&a), ["/repo/f.txt"]);
    }
}
```

- [ ] **Step 3: Run to see them fail**

Run: `cargo nextest run -p devkit-command fish`
Expected: FAIL.

- [ ] **Step 4: Implement**

`crates/devkit-command/src/fish.rs` (keep `shapes` and `tests`):

```rust
//! fish: commands, redirects, `set`, `cd`, pipes, conditionals, loops, and
//! command substitution.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    model::{FileOp, Language, UncertaintyKind, Value},
    paths, ts,
};

#[derive(Debug, Clone, Default, PartialEq)]
struct Scope {
    vars: HashMap<String, Value>,
    cwd: Option<String>,
}

impl Scope {
    fn merge_uncertain(&mut self, branch: &Scope) {
        for (name, value) in &branch.vars {
            if self.vars.get(name) != Some(value) {
                self.vars.insert(name.clone(), Value::Unknown);
            }
        }
        if branch.cwd != self.cwd {
            self.cwd = None;
        }
    }
}

pub(crate) fn walk(a: &mut Analyzer<'_>, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(Language::Fish, source) else {
        a.uncertain(UncertaintyKind::ParseError, "fish source did not parse", frame.locate(0..source.len()));
        return;
    };
    let mut scope = Scope { cwd: frame.cwd.clone(), ..Scope::default() };
    let mut w = Walker { a, source, frame };
    w.statements(tree.root_node(), &mut scope);
}

struct Walker<'a, 'c, 's> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
}

impl<'t> Walker<'_, '_, '_> {
    fn statements(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            self.statement(child, scope, Stdin::None);
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        if self.a.budget.visit().is_err() {
            return;
        }
        if node.has_error() && node.kind() != "program" {
            let at = self.frame.locate(node.byte_range());
            self.a.uncertain(UncertaintyKind::ParseError, "a fish statement could not be parsed", at);
            return;
        }
        match node.kind() {
            "comment" => {}
            "command" => self.command(node, scope, stdin),
            "redirect_statement" | "redirected_statement" => {
                for child in ts::named_children(node) {
                    match child.kind() {
                        "file_redirect" | "stream_redirect" => self.redirect(child, scope),
                        _ => self.statement(child, scope, Stdin::None),
                    }
                }
            }
            "pipe" => {
                for element in ts::named_children(node) {
                    let mut inner = scope.clone();
                    self.statement(element, &mut inner, Stdin::Source { value: Value::Unknown, span: element.byte_range() });
                }
            }
            "conditional_execution" | "negated_statement" => self.statements(node, scope),
            "if_statement" | "while_statement" | "switch_statement" | "begin_statement" | "else_clause" | "else_if_clause" | "case_clause" => {
                let mut branch = scope.clone();
                self.statements(node, &mut branch);
                scope.merge_uncertain(&branch);
            }
            "for_statement" => {
                let mut branch = scope.clone();
                if let Some(var) = node.child_by_field_name("variable") {
                    branch.vars.insert(ts::text(var, self.source).to_string(), Value::Unknown);
                }
                self.statements(node, &mut branch);
                scope.merge_uncertain(&branch);
            }
            "function_definition" => {}
            _ => self.statements(node, scope),
        }
    }

    fn redirect(&mut self, node: Node<'t>, scope: &mut Scope) {
        let text = ts::text(node, self.source);
        let op = if text.contains(">>") { FileOp::Append } else if text.contains('>') && !text.contains(">&") { FileOp::Overwrite } else { return };
        let Some(dest) = node.child_by_field_name("destination").or_else(|| ts::named_children(node).into_iter().last()) else { return };
        let value = self.value(dest, scope);
        if matches!(value.known(), Some("/dev/null" | "/dev/stderr" | "/dev/stdout")) {
            return;
        }
        let at = self.frame.locate(node.byte_range());
        self.a.file_effect(op, &value, scope.cwd.as_deref(), at);
    }

    fn command(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        let mut words = Vec::new();
        for child in ts::named_children(node) {
            match child.kind() {
                "file_redirect" | "stream_redirect" => self.redirect(child, scope),
                _ => {
                    let value = self.value(child, scope);
                    words.push(Word { typed: value.known().map_or_else(|| ts::text(child, self.source).to_string(), str::to_string), value, span: child.byte_range() });
                }
            }
        }
        match words.first().and_then(|w| w.value.known()) {
            Some("set") => {
                let rest: Vec<&Word> = words[1..].iter().filter(|w| w.value.known().is_none_or(|t| !t.starts_with('-'))).collect();
                if let Some(name) = rest.first().and_then(|w| w.value.known()) {
                    let value = match rest.get(1..) {
                        Some([single]) => single.value.clone(),
                        _ => Value::Unknown,
                    };
                    scope.vars.insert(name.to_string(), value);
                }
                return;
            }
            Some("cd") => {
                scope.cwd = words.get(1).and_then(|w| match paths::resolve(&w.value, scope.cwd.as_deref(), self.a.ctx.path_style) {
                    crate::model::Target::Path(d) => Some(d),
                    crate::model::Target::Unresolved => None,
                });
                return;
            }
            _ => {}
        }
        if words.is_empty() {
            return;
        }
        let raw = RawInvocation { words, stdin, cwd: scope.cwd.clone(), language: Language::Fish, location: self.frame.locate(node.byte_range()) };
        self.a.invocation(raw, self.frame);
    }

    fn value(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let text = ts::text(node, self.source);
        match node.kind() {
            "word" | "integer" | "float" => {
                if text.contains(['*', '?', '~', '{']) { Value::Unknown } else { Value::Known(text.replace('\\', "")) }
            }
            "single_quote_string" => Value::Known(text[1..text.len() - 1].replace("\\'", "'")),
            "double_quote_string" => {
                let mut out = String::new();
                let mut last = node.start_byte() + 1;
                for part in ts::named_children(node) {
                    out.push_str(&self.source[last..part.start_byte()]);
                    last = part.end_byte();
                    match self.value(part, scope) {
                        Value::Known(s) => out.push_str(&s),
                        Value::Unknown => return Value::Unknown,
                    }
                }
                out.push_str(&self.source[last..node.end_byte() - 1]);
                Value::Known(out)
            }
            "variable_expansion" => scope.vars.get(text.trim_start_matches('$')).cloned().unwrap_or(Value::Unknown),
            "concatenation" => {
                let mut out = String::new();
                for part in ts::named_children(node) {
                    match self.value(part, scope) {
                        Value::Known(s) => out.push_str(&s),
                        Value::Unknown => return Value::Unknown,
                    }
                }
                Value::Known(out)
            }
            "command_substitution" => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
                Value::Unknown
            }
            _ => Value::Unknown,
        }
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-command`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-command
git commit -m "feat(command): analyze fish commands and redirects"
```

---

### Task 11: Analysis limits

**Files:**
- Modify: `crates/devkit-command/src/lib.rs` (tests only, unless a test exposes a gap)

**Interfaces:**
- Consumes: `Limits`, `Budget`, every adapter.
- Produces: nothing new; proves exhaustion is an uncertainty that keeps earlier findings.

- [ ] **Step 1: Write the tests**

```rust
#[cfg(test)]
mod limit_tests {
    use crate::{
        testutil::{ctx, targets},
        Dialect, Limit, Limits, UncertaintyKind,
    };

    fn with(limits: Limits, source: &str) -> crate::Analysis {
        let mut c = ctx(Dialect::Bash);
        c.limits = limits;
        crate::analyze(source, &c)
    }

    fn exhausted(a: &crate::Analysis, limit: Limit) -> bool {
        a.uncertainties.iter().any(|u| u.kind == UncertaintyKind::LimitExhausted(limit))
    }

    #[test]
    fn an_oversized_command_is_not_parsed() {
        let a = with(Limits { outer_source: 16, ..Limits::default() }, "echo x > a.txt; echo y > b.txt");
        assert!(exhausted(&a, Limit::OuterSource));
        assert!(a.file_effects.is_empty());
    }

    #[test]
    fn cumulative_embedded_source_is_bounded_and_earlier_findings_stay() {
        let a = with(
            Limits { cumulative_source: 40, ..Limits::default() },
            "echo a > a.txt; bash -c 'echo b > b.txt; echo c > c.txt; echo d > d.txt'",
        );
        assert!(exhausted(&a, Limit::CumulativeSource));
        assert_eq!(targets(&a), ["/repo/a.txt"]);
    }

    #[test]
    fn the_node_budget_stops_the_walk_and_keeps_what_it_found() {
        let source = (0..200).map(|i| format!("echo {i} > f{i}.txt")).collect::<Vec<_>>().join("\n");
        let a = with(Limits { nodes: 60, ..Limits::default() }, &source);
        assert!(exhausted(&a, Limit::Nodes));
        assert!(!a.file_effects.is_empty() && a.file_effects.len() < 200);
    }

    #[test]
    fn an_oversized_value_is_unknown() {
        let big = "x".repeat(100);
        let a = with(Limits { value: 64, ..Limits::default() }, &format!("f={big}; echo y > \"$f\""));
        assert_eq!(targets(&a), ["?"]);
    }
}
```

- [ ] **Step 2: Run them**

Run: `cargo nextest run -p devkit-command limit_tests`
Expected: PASS. A failure here is a real gap in an adapter's budget handling: fix the adapter (each walker must call `budget.visit()` per node and stop cleanly), not the test.

- [ ] **Step 3: Commit**

```bash
git add crates/devkit-command
git commit -m "test(command): cover analysis limits"
```

---

### Task 12: Policy and rule configuration types

**Files:**
- Modify: `crates/devkit-config/src/harness.rs`
- Modify: `crates/devkit-ports/src/guard/mod.rs` (test helper constructs `CommandRule`)
- Modify: `schema/devkit-config.json` (regenerated)

**Interfaces:**
- Consumes: nothing.
- Produces: `devkit_config::{ShellSetting, PolicyAction, RuleAction, Severity}`; `CommandRule { programs, args, reason, enabled, action, severity }` with `impl Default` (`enabled: true`, `action: Block`, `severity: Error`); `HarnessSection { enforce_writes, enforce_commands, shell, unresolved_writes, unsupported_language, script_files, commands, app_match }`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/devkit-config/src/harness.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_rule_keeps_its_behaviour() {
        let rule: CommandRule = toml::from_str("programs = [\"git\"]\nargs = [\"worktree\", \"add\"]\nreason = \"use issue\"").unwrap();
        assert!(rule.enabled);
        assert_eq!(rule.action, RuleAction::Block);
        assert_eq!(rule.severity, Severity::Error);
    }

    #[test]
    fn a_rule_takes_an_action_severity_and_switch() {
        let rule: CommandRule = toml::from_str("programs = [\"git\"]\nenabled = false\naction = \"warn\"\nseverity = \"info\"").unwrap();
        assert!(!rule.enabled);
        assert_eq!(rule.action, RuleAction::Warn);
        assert_eq!(rule.severity, Severity::Info);
    }

    #[test]
    fn allow_is_not_a_rule_action() {
        assert!(toml::from_str::<CommandRule>("programs = [\"git\"]\naction = \"allow\"").is_err());
    }

    #[test]
    fn the_policy_keys_default_conservatively() {
        let h = HarnessSection::default();
        assert_eq!(h.shell, ShellSetting::Auto);
        assert_eq!(h.unresolved_writes, PolicyAction::Block);
        assert_eq!(h.unsupported_language, PolicyAction::Block);
        assert_eq!(h.script_files, PolicyAction::Allow);
        let parsed: HarnessSection = toml::from_str("shell = \"powershell\"\nscript_files = \"warn\"").unwrap();
        assert_eq!(parsed.shell, ShellSetting::Powershell);
        assert_eq!(parsed.script_files, PolicyAction::Warn);
        assert_eq!(parsed.unresolved_writes, PolicyAction::Block);
    }
}
```

`devkit-config` already depends on `toml`; if it is only a dev-dependency there, the tests compile as they are.

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run -p devkit-config harness`
Expected: compile failure on the new types.

- [ ] **Step 3: Implement**

In `crates/devkit-config/src/harness.rs`, add above `CommandRule`:

```rust
/// The shell whose syntax a hook command is read in. `auto` resolves from
/// the tool name and the harness; see `docs/configuration.md`.
#[derive(Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ShellSetting {
    #[default]
    Auto,
    Bash,
    Powershell,
}

/// What a policy finding does to the tool call.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PolicyAction {
    /// Deny the tool call with an actionable reason.
    Block,
    /// Allow it and tell the agent what was not checked.
    Warn,
    /// Allow it silently.
    Allow,
}

/// What a matching command rule does. A rule that should not act is disabled
/// with `enabled = false`, so there is no `allow`.
#[derive(Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    #[default]
    Block,
    Warn,
}

/// How a diagnostic is classified for the agent.
#[derive(Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    #[default]
    Error,
}
```

Replace `CommandRule`'s derive line and add the fields and `Default`:

```rust
#[derive(Deserialize, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct CommandRule {
    // programs, args, reason as before, unchanged
    /// `false` turns off a rule a parent layer declared.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    #[serde(default)]
    pub action: RuleAction,
    #[serde(default)]
    pub severity: Severity,
}

fn enabled_default() -> bool {
    true
}

impl Default for CommandRule {
    fn default() -> Self {
        Self {
            programs: Vec::new(),
            args: Vec::new(),
            reason: String::new(),
            enabled: true,
            action: RuleAction::Block,
            severity: Severity::Error,
        }
    }
}
```

Update the `CommandRule` doc comment: replace "Deliberately not a regex. Splitting the command into segments, stripping runner prefixes, and anchoring the command word are devkit's job" with "Deliberately not a regex. Parsing the command, unwrapping wrappers and runners, and removing a program's own global options (`git -C`) are devkit's job".

Replace `HarnessSection`'s derive with `#[derive(Deserialize, Debug, Clone, PartialEq, schemars::JsonSchema)]`, add after `enforce_commands`:

```rust
    /// The shell a hook command is read in.
    #[serde(default)]
    pub shell: ShellSetting,
    /// A write whose target could not be determined.
    #[serde(default = "block")]
    pub unresolved_writes: PolicyAction,
    /// Executable source in a language devkit cannot analyze.
    #[serde(default = "block")]
    pub unsupported_language: PolicyAction,
    /// A call to a stored script, whose contents are not read.
    #[serde(default = "allow")]
    pub script_files: PolicyAction,
```

and below the struct:

```rust
fn block() -> PolicyAction {
    PolicyAction::Block
}

fn allow() -> PolicyAction {
    PolicyAction::Allow
}

impl Default for HarnessSection {
    fn default() -> Self {
        Self {
            enforce_writes: false,
            enforce_commands: false,
            shell: ShellSetting::Auto,
            unresolved_writes: PolicyAction::Block,
            unsupported_language: PolicyAction::Block,
            script_files: PolicyAction::Allow,
            commands: BTreeMap::new(),
            app_match: AppMatch::default(),
        }
    }
}
```

Re-export the four enums wherever `CommandRule` and `AppMatch` are re-exported from `crates/devkit-config/src/lib.rs` (`rg -n "pub use harness" crates/devkit-config/src/lib.rs`).

In `crates/devkit-ports/src/guard/mod.rs`, the test helper `rules` builds `CommandRule { programs, args, reason }`; add `..CommandRule::default()`.

- [ ] **Step 4: Run tests and regenerate the schema**

Run: `cargo nextest run -p devkit-config`
Expected: PASS.

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`, then `cargo nextest run --test config_schema`
Expected: the second run passes; `git diff schema/devkit-config.json` shows the four keys and three rule fields.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-config crates/devkit-ports/src/guard/mod.rs schema/devkit-config.json
git commit -m "feat(config): add shell-write policy and rule actions"
```

---

### Task 13: Payload identity, policy merge, and the warning envelope

**Files:**
- Modify: `crates/devkit-common/src/harness.rs`

**Interfaces:**
- Consumes: Task 12 types.
- Produces:
  - `Harness::{ClaudeCode, Codex, Cursor}`
  - `ShellPayload { harness, tool_name: Option<String>, command, cwd: Option<PathBuf>, session_id: Option<String>, agent_id: Option<String> }`
  - `HarnessPolicy { shell: ShellSetting, unresolved_writes: PolicyAction, unsupported_language: PolicyAction, script_files: PolicyAction }` with `Default`
  - `HarnessRules { commands, app_match, policy: HarnessPolicy }`
  - `writes_enabled(cwd: &Path) -> bool`
  - `warn_shell_json(harness: Harness, context: &str) -> Option<Value>`

- [ ] **Step 1: Confirm the warning channel on both harnesses**

Use the `devkit:docs` skill (or the claude-code-guide agent for Claude Code) to read the `PreToolUse` hook output schema for Claude Code and for Codex. Record the answer to one question for each: does a `hookSpecificOutput` object with `hookEventName: "PreToolUse"` and `additionalContext`, and no `permissionDecision`, allow the call and deliver the text to the model? Do not send `permissionDecision: "allow"`: on Claude Code that also skips the user's permission prompt, which is not this hook's decision to make.

If Codex has no such field, `warn_shell_json` returns `None` for `Harness::Codex`, and Task 18's docs say that `warn` is a silent allow on Codex.

- [ ] **Step 2: Write the failing tests**

Add to the existing `tests` module:

```rust
    #[test]
    fn a_codex_payload_is_told_apart_by_its_turn_fields() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse", "tool_name": "Bash", "turn_id": "t1", "model": "m",
            "session_id": "S", "tool_input": { "command": "ls" }, "cwd": "/repo"
        });
        let parsed = parse_shell_payload(&p).unwrap();
        assert_eq!(parsed.harness, Harness::Codex);
        assert_eq!(parsed.session_id.as_deref(), Some("S"));
    }

    #[test]
    fn claude_codes_powershell_tool_is_a_shell_payload() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse", "tool_name": "PowerShell", "prompt_id": "p",
            "session_id": "S", "agent_id": "a1", "tool_input": { "command": "Get-ChildItem" }
        });
        let parsed = parse_shell_payload(&p).unwrap();
        assert_eq!(parsed.harness, Harness::ClaudeCode);
        assert_eq!(parsed.tool_name.as_deref(), Some("PowerShell"));
        assert_eq!(parsed.agent_id.as_deref(), Some("a1"));
    }

    #[test]
    fn missing_identity_stays_missing() {
        let p = serde_json::json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": { "command": "ls" } });
        let parsed = parse_shell_payload(&p).unwrap();
        assert!(parsed.session_id.is_none() && parsed.cwd.is_none());
    }

    #[test]
    fn the_warning_envelope_allows_without_a_permission_decision() {
        let w = warn_shell_json(Harness::ClaudeCode, "not checked").unwrap();
        assert_eq!(w["hookSpecificOutput"]["additionalContext"], "not checked");
        assert!(w["hookSpecificOutput"].get("permissionDecision").is_none());
        assert!(warn_shell_json(Harness::Cursor, "not checked").is_none());
        assert_eq!(deny_shell_json(Harness::Codex, "no")["hookSpecificOutput"]["permissionDecision"], "deny");
    }

    #[test]
    fn the_closest_layer_sets_each_policy_key() {
        let (h, warns) = merge_rules(&[
            layer("global", "[harness]\nunresolved_writes = \"warn\"\nscript_files = \"block\"\n"),
            layer("project", "[harness]\nunresolved_writes = \"allow\"\n"),
        ]);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(h.policy.unresolved_writes, devkit_config::PolicyAction::Allow);
        assert_eq!(h.policy.script_files, devkit_config::PolicyAction::Block);
        assert_eq!(h.policy.unsupported_language, devkit_config::PolicyAction::Block);
    }

    #[test]
    fn an_invalid_policy_value_warns_and_keeps_that_keys_default_only() {
        let (h, warns) = merge_rules(&[layer(
            "root",
            "[harness]\nunresolved_writes = \"sometimes\"\nscript_files = \"warn\"\n[harness.commands.bad]\nprograms = \"git\"\n",
        )]);
        assert_eq!(h.policy.unresolved_writes, devkit_config::PolicyAction::Block);
        assert_eq!(h.policy.script_files, devkit_config::PolicyAction::Warn);
        assert_eq!(warns.len(), 2, "{warns:?}");
    }

    #[test]
    fn a_child_layer_disables_an_inherited_rule_without_repeating_it() {
        let (h, _) = merge_rules(&[
            layer("global", "[harness.commands.no-node]\nprograms = [\"node\"]\nreason = \"use bun\"\n"),
            layer("project", "[harness.commands.no-node]\nenabled = false\n"),
        ]);
        assert!(!h.commands["no-node"].enabled);
        assert_eq!(h.commands["no-node"].programs, vec!["node"]);
    }
```

Change `a_non_bash_tool_is_not_a_shell_payload` to keep using `Edit`, and leave the Cursor tests as they are.

- [ ] **Step 3: Run to see them fail**

Run: `cargo nextest run -p devkit-common harness`
Expected: compile failure.

- [ ] **Step 4: Implement**

Replace the `Harness` enum, `ShellPayload`, `parse_shell_payload`, and `deny_shell_json`:

```rust
/// Which harness sent a payload, and therefore which envelope answers it and
/// whether the write stage may run for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    ClaudeCode,
    Codex,
    Cursor,
}

/// Tool names whose `tool_input.command` is a shell command.
const SHELL_TOOLS: [&str; 2] = ["Bash", "PowerShell"];

/// A pre-execution shell payload. Fields the harness did not send stay
/// `None`; nothing here is filled in from the hook process's environment.
#[derive(Debug, Clone)]
pub struct ShellPayload {
    pub harness: Harness,
    pub tool_name: Option<String>,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
}

/// Read a pre-execution shell payload. `None` when the event is not about a
/// shell command.
///
/// Claude Code and Codex send a string `hook_event_name`; Cursor does not.
/// Codex's payload carries `turn_id` and `model`, which Claude Code's does
/// not, and that is what separates the two.
pub fn parse_shell_payload(p: &Value) -> Option<ShellPayload> {
    let text = |k: &str| p.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let harness = match p.get("hook_event_name").and_then(Value::as_str) {
        None => Harness::Cursor,
        Some(_) if p.get("turn_id").is_some() || p.get("model").is_some() => Harness::Codex,
        Some(_) => Harness::ClaudeCode,
    };
    let tool_name = text("tool_name");
    if harness != Harness::Cursor && !tool_name.as_deref().is_some_and(|t| SHELL_TOOLS.contains(&t)) {
        return None;
    }
    let command = p
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| p.get("tool_input")?.get("command")?.as_str())
        .filter(|s| !s.trim().is_empty())?
        .to_string();
    Some(ShellPayload {
        harness,
        tool_name,
        command,
        cwd: text("cwd").map(PathBuf::from),
        session_id: text("session_id"),
        agent_id: text("agent_id"),
    })
}

pub fn deny_shell_json(harness: Harness, reason: &str) -> Value {
    match harness {
        Harness::ClaudeCode | Harness::Codex => deny_json(reason),
        Harness::Cursor => json!({
            "permission": "deny",
            "agent_message": reason,
            "continue": true
        }),
    }
}

/// An allow that carries text to the agent, or `None` where the harness has
/// no such channel and a warning can only be a silent allow. No
/// `permissionDecision` is sent: an explicit allow would also bypass the
/// user's own permission prompt.
pub fn warn_shell_json(harness: Harness, context: &str) -> Option<Value> {
    match harness {
        Harness::ClaudeCode | Harness::Codex => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": context
            }
        })),
        Harness::Cursor => None,
    }
}

/// Whether the shell hook's write stage is active for a command at `cwd`.
pub fn writes_enabled(cwd: &Path) -> bool {
    enforcement_enabled(cwd, "enforce_writes", "DEVKIT_ENFORCE_WRITES")
}
```

If Step 1 found no Codex channel, split the `warn_shell_json` arm so `Harness::Codex => None`, and change the test to assert it.

Add the policy to `HarnessRules` and parse it in `merge_rules`:

```rust
use devkit_config::{AppMatch, CommandRule, PolicyAction, ShellSetting};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessPolicy {
    pub shell: ShellSetting,
    pub unresolved_writes: PolicyAction,
    pub unsupported_language: PolicyAction,
    pub script_files: PolicyAction,
}

impl Default for HarnessPolicy {
    fn default() -> Self {
        Self {
            shell: ShellSetting::Auto,
            unresolved_writes: PolicyAction::Block,
            unsupported_language: PolicyAction::Block,
            script_files: PolicyAction::Allow,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct HarnessRules {
    pub commands: BTreeMap<String, CommandRule>,
    pub app_match: AppMatch,
    pub policy: HarnessPolicy,
}
```

In `merge_rules`, after `app_match`:

```rust
    // Each key on its own: an invalid value costs that key its setting, not
    // its siblings, and never the enforcement flags, which are read apart.
    fn key<T: serde::de::DeserializeOwned>(merged: &toml::Table, name: &str, default: T, warnings: &mut Vec<String>) -> T {
        match merged.get(name) {
            None => default,
            Some(v) => v.clone().try_into::<T>().unwrap_or_else(|e| {
                warnings.push(format!("ignoring `[harness] {name}`: {e}"));
                default
            }),
        }
    }
    let defaults = HarnessPolicy::default();
    let policy = HarnessPolicy {
        shell: key(&merged, "shell", defaults.shell, &mut warnings),
        unresolved_writes: key(&merged, "unresolved_writes", defaults.unresolved_writes, &mut warnings),
        unsupported_language: key(&merged, "unsupported_language", defaults.unsupported_language, &mut warnings),
        script_files: key(&merged, "script_files", defaults.script_files, &mut warnings),
    };
```

and return `HarnessRules { commands, app_match, policy }`. `devkit-common` needs `serde` as a dependency for the `DeserializeOwned` bound; it already has it if `rg -n '^serde' crates/devkit-common/Cargo.toml` matches.

Update the doc comment on `HarnessRules`: "The merged `[harness]` tables and policy keys the shell hook reads. The two enforcement flags are not here: they ratchet on across layers rather than merging by precedence, and `enforcement_enabled` owns that."

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p devkit-common && cargo build`
Expected: PASS, and the binary still builds; `src/bin/devkit/harness.rs` destructures `ShellPayload` by named fields, so add `..` to that pattern.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-common src/bin/devkit/harness.rs
git commit -m "feat(harness): read shell payload identity and policy keys"
```

---

### Task 14: Conflict check for a directory scope

**Files:**
- Modify: `crates/devkit-locks/src/lib.rs`

**Interfaces:**
- Consumes: `WriteResolver`, `rel_under_root`, `existing_ancestor`, `check_resolved`.
- Produces: `WriteResolver::scope_key(&mut self, dir: &str, whole_checkout: bool) -> Result<(String, String)>` and `WriteResolver::check_scope(&mut self, dir: &str, whole_checkout: bool, holder: &str) -> Result<Vec<Conflict>>`.

A tree effect is a read of the registry: no row is written. `check_resolved` already reports every live row that overlaps a path, and a directory path overlaps every row under it.

- [ ] **Step 1: Write the failing test**

Add to `crates/devkit-locks/src/lib.rs`'s test module (create one with `#[cfg(test)] mod tests { use super::*; }` if absent):

```rust
    #[test]
    fn a_scope_keys_to_its_checkout_root() {
        let repo = tempfile::tempdir().unwrap();
        devkit_common::git::Git::fixture(repo.path()).args(["init", "-q", "-b", "main"]).output().unwrap();
        let sub = repo.path().join("src");
        std::fs::create_dir_all(&sub).unwrap();
        let root = find_root_from(repo.path()).to_string_lossy().into_owned();
        let mut r = WriteResolver::new();

        assert_eq!(r.scope_key(sub.to_str().unwrap(), false).unwrap(), (root.clone(), "src".to_string()));
        assert_eq!(r.scope_key(sub.to_str().unwrap(), true).unwrap(), (root.clone(), ".".to_string()));
        assert_eq!(r.scope_key(repo.path().to_str().unwrap(), false).unwrap(), (root, ".".to_string()));
    }
```

- [ ] **Step 2: Run to see it fail**

Run: `cargo nextest run -p devkit-locks a_scope_keys_to_its_checkout_root`
Expected: compile failure on `scope_key`.

- [ ] **Step 3: Implement**

Add to `impl WriteResolver`:

```rust
    /// The registry key for a directory an unenumerated write rewrites:
    /// its checkout root, and the directory relative to it, or `.` for the
    /// root itself or when the writer rewrites the whole checkout.
    pub fn scope_key(&mut self, dir: &str, whole_checkout: bool) -> Result<(String, String)> {
        let p = Path::new(dir);
        let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().context("getting current dir")?.join(p) };
        let root = self.root_for(&existing_ancestor(&abs));
        let rel = if whole_checkout || abs == root {
            ".".to_string()
        } else {
            match rel_under_root(&abs, &root)? {
                r if r.is_empty() => ".".to_string(),
                r => r,
            }
        };
        Ok((root.to_string_lossy().into_owned(), rel))
    }

    /// Live rows another session holds anywhere under `dir`. Takes no lock.
    pub fn check_scope(&mut self, dir: &str, whole_checkout: bool, holder: &str) -> Result<Vec<Conflict>> {
        let (root, rel) = self.scope_key(dir, whole_checkout)?;
        check_resolved(&root, holder, &[rel])
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run -p devkit-locks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-locks
git commit -m "feat(locks): check a directory scope for foreign locks"
```

---

### Task 15: Move the command guard onto the shared analysis

**Files:**
- Modify: `crates/devkit-ports/Cargo.toml` (add `devkit-command.workspace = true`)
- Modify: `crates/devkit-ports/src/guard/mod.rs`
- Modify: `crates/devkit-ports/src/guard/norm.rs` (reduced to `basename`, `Doppler`, `doppler_of`)
- Delete: `crates/devkit-ports/src/guard/lex.rs`
- Modify: `crates/devkit-command/src/lib.rs` (re-export `normalize::doppler_flags` as `pub fn doppler_flags`)

**Interfaces:**
- Consumes: `devkit_command::{analyze, analyze_argv, Analysis, Invocation, Value, Context}`, `CommandRule { enabled, action, severity }`.
- Produces:
  - `guard::Finding { rule: Option<String>, severity: Severity, message: String }`
  - `guard::Verdict { blocks: Vec<Finding>, warnings: Vec<Finding> }`
  - `guard::decide(analysis: &Analysis, rules: &BTreeMap<String, CommandRule>, project: Option<&Project>) -> Verdict`
  - `guard::decide_with(command: &str, rules, project) -> Decision` kept for tests and callers that hold a bash string

- [ ] **Step 1: Port the lexer and normalizer tests**

For every test in `crates/devkit-ports/src/guard/lex.rs` and `norm.rs` `mod tests`, confirm an equivalent assertion exists in `devkit-command` (Tasks 3 and 4) by searching for the construct it covers (`rg -n "timeout|doppler|env -|bunx|heredoc|comment" crates/devkit-command/src`). Where one is missing, add it to `crates/devkit-command/src/normalize.rs` or `bash.rs` tests, phrased through `crate::testutil::bash`. The doppler nesting-depth test becomes a `simple_argv` test: a `--command` value that is not one plain command leaves the wrapper in place.

Run: `cargo nextest run -p devkit-command`
Expected: PASS.

- [ ] **Step 2: Write the failing guard tests**

Add to `crates/devkit-ports/src/guard/mod.rs` tests:

```rust
    fn rule(programs: &[&str], args: &[&str], edit: impl FnOnce(&mut CommandRule)) -> BTreeMap<String, CommandRule> {
        let mut r = CommandRule {
            programs: programs.iter().map(|s| s.to_string()).collect(),
            args: args.iter().map(|s| s.to_string()).collect(),
            reason: "use issue".into(),
            ..CommandRule::default()
        };
        edit(&mut r);
        BTreeMap::from([("worktree-add".to_string(), r)])
    }

    fn verdict(command: &str, rules: &BTreeMap<String, CommandRule>) -> Verdict {
        let ctx = devkit_command::Context {
            dialect: devkit_command::Dialect::Bash,
            cwd: None,
            path_style: devkit_command::PathStyle::Unix,
            limits: devkit_command::Limits::default(),
        };
        decide(&devkit_command::analyze(command, &ctx), rules, None)
    }

    #[test]
    fn git_global_options_do_not_hide_a_worktree_rule() {
        let r = rule(&["git"], &["worktree", "add"], |_| {});
        assert_eq!(verdict("git -C /repo worktree add ../wt", &r).blocks.len(), 1);
        assert!(verdict("git -C /repo worktree list", &r).blocks.is_empty());
    }

    #[test]
    fn a_warn_rule_warns_and_a_disabled_rule_is_silent() {
        let warn = rule(&["git"], &["worktree", "add"], |r| {
            r.action = RuleAction::Warn;
            r.severity = Severity::Warning;
        });
        let v = verdict("git worktree add x", &warn);
        assert!(v.blocks.is_empty());
        assert_eq!(v.warnings[0].severity, Severity::Warning);
        let off = rule(&["git"], &["worktree", "add"], |r| r.enabled = false);
        let v = verdict("git worktree add x", &off);
        assert!(v.blocks.is_empty() && v.warnings.is_empty());
    }

    #[test]
    fn an_undetermined_argument_is_a_possible_match_that_warns() {
        let r = rule(&["git"], &["worktree", "add"], |_| {});
        let v = verdict("git \"$verb\" add /tmp/wt", &r);
        assert!(v.blocks.is_empty());
        assert_eq!(v.warnings.len(), 1, "{:?}", v.warnings.iter().map(|w| &w.message).collect::<Vec<_>>());
    }

    #[test]
    fn rules_see_nested_commands_but_not_quoted_mentions() {
        let r = rule(&["git"], &["worktree", "add"], |_| {});
        assert_eq!(verdict("bash -c 'git worktree add x'", &r).blocks.len(), 1);
        assert!(verdict("echo 'git worktree add x'", &r).blocks.is_empty());
        assert!(verdict("git commit -m \"git worktree add x\"", &r).blocks.is_empty());
    }
```

Import `RuleAction` and `Severity` from `devkit_config` in the test module.

- [ ] **Step 3: Run to see them fail**

Run: `cargo nextest run -p devkit-ports guard`
Expected: compile failure on `decide`, `Verdict`.

- [ ] **Step 4: Implement**

`crates/devkit-ports/src/guard/norm.rs` becomes:

```rust
//! What the guard still needs after the shared analysis has unwrapped a
//! command: a program's basename and the doppler wrapper's identity.

use devkit_command::{Invocation, Value};

/// A doppler wrapper's identity, normalized so `-c dev` and `--config dev`
/// compare equal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Doppler {
    pub config: Option<String>,
    pub project: Option<String>,
}

/// The last path component of a program word, without a Windows `.exe`.
pub fn basename(prog: &str) -> &str {
    let base = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    base.strip_suffix(".exe").unwrap_or(base)
}

/// The doppler wrapper the analysis removed to reach `inv`, if any.
pub fn doppler_of(inv: &Invocation) -> Option<Doppler> {
    inv.wrappers
        .iter()
        .find(|w| w.first().and_then(Value::known).map(basename) == Some("doppler"))
        .map(|w| {
            let (config, project) = devkit_command::doppler_flags(w);
            Doppler { config, project }
        })
}
```

In `crates/devkit-command/src/lib.rs` add:

```rust
/// `(config, project)` from a `doppler run` wrapper's words.
pub fn doppler_flags(words: &[Value]) -> (Option<String>, Option<String>) {
    normalize::doppler_flags(words)
}
```

In `crates/devkit-ports/src/guard/mod.rs`, remove `pub mod lex;`, delete `lex.rs`, and replace `decide_with` and `rule_hit` with:

```rust
use devkit_command::{Analysis, Invocation, Value};
use devkit_config::{RuleAction, Severity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: Option<String>,
    pub severity: Severity,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Verdict {
    pub blocks: Vec<Finding>,
    pub warnings: Vec<Finding>,
}

/// The guard's reading of one invocation: the program and arguments as
/// strings when every word is known.
struct Known {
    argv: Vec<String>,
    doppler: Option<norm::Doppler>,
}

fn known(inv: &Invocation) -> Option<Known> {
    let mut argv = vec![inv.program.known()?.to_string()];
    for a in &inv.args {
        argv.push(a.known()?.to_string());
    }
    Some(Known { argv, doppler: norm::doppler_of(inv) })
}

enum Match {
    Yes,
    Possible,
    No,
}

fn rule_match(inv: &Invocation, rule: &CommandRule) -> Match {
    let program = match inv.program.known() {
        Some(p) => basename(p),
        None => return Match::Possible,
    };
    if !rule.programs.iter().any(|p| basename(p) == program) {
        return Match::No;
    }
    let mut possible = false;
    for (i, expected) in rule.args.iter().enumerate() {
        match inv.semantic_args.get(i) {
            Some(Value::Known(a)) if a == expected => {}
            Some(Value::Known(_)) | None => return Match::No,
            Some(Value::Unknown) => possible = true,
        }
    }
    if possible { Match::Possible } else { Match::Yes }
}

/// Decide over an analysis. Every invocation is checked, nested ones
/// included; findings keep invocation order, then rule name order.
pub fn decide(analysis: &Analysis, rules: &BTreeMap<String, CommandRule>, project: Option<&Project>) -> Verdict {
    let mut v = Verdict::default();
    for inv in &analysis.invocations {
        if inv.program.known().map(basename).is_some_and(|p| SHIMS.contains(&p)) {
            continue;
        }
        let typed = inv.typed.join(" ");
        let mut undetermined = false;
        for (name, rule) in rules.iter().filter(|(_, r)| r.enabled && !r.programs.is_empty()) {
            match rule_match(inv, rule) {
                Match::Yes => {
                    let finding = Finding { rule: Some(name.clone()), severity: rule.severity, message: rule.reason.clone() };
                    match rule.action {
                        RuleAction::Block => v.blocks.push(finding),
                        RuleAction::Warn => v.warnings.push(finding),
                    }
                }
                Match::Possible => undetermined = true,
                Match::No => {}
            }
        }
        if undetermined {
            v.warnings.push(Finding {
                rule: None,
                severity: Severity::Warning,
                message: format!("`{typed}` could not be fully resolved, so devkit could not tell whether a `[harness.commands]` rule applies; it was allowed."),
            });
        }
        let Some(p) = project else { continue };
        match known(inv) {
            Some(k) => {
                let prog = basename(&k.argv[0]).to_string();
                let n = norm_view(&k);
                if let Some(message) = project_hit(&inv.typed, &n, &prog, p) {
                    v.blocks.push(Finding { rule: None, severity: Severity::Error, message });
                }
            }
            None if !p.config.tasks.is_empty() || !p.catalog.is_empty() => v.warnings.push(Finding {
                rule: None,
                severity: Severity::Info,
                message: format!("`{typed}` could not be fully resolved, so devkit did not check it against the project's tasks and apps."),
            }),
            None => {}
        }
    }
    v
}

/// Kept for callers holding a bash command string: the first block, if any.
pub fn decide_with(command: &str, rules: &BTreeMap<String, CommandRule>, project: Option<&Project>) -> Decision {
    let analysis = devkit_command::analyze(command, &bash_context());
    match decide(&analysis, rules, project).blocks.into_iter().next() {
        Some(f) => Decision::Deny { reason: f.message },
        None => Decision::Allow,
    }
}

fn bash_context() -> devkit_command::Context {
    devkit_command::Context {
        dialect: devkit_command::Dialect::Bash,
        cwd: None,
        path_style: if cfg!(windows) { devkit_command::PathStyle::Windows } else { devkit_command::PathStyle::Unix },
        limits: devkit_command::Limits::default(),
    }
}

/// A configured argv, unwrapped the same way a typed command is.
fn configured(argv: &[String]) -> Option<Known> {
    devkit_command::analyze_argv(argv, &bash_context()).invocations.first().and_then(known)
}
```

The existing `project_hit`, `best_task`, `matching_apps`, `narrowed_apps`, `searched_app` take `n: &Normalized` where `Normalized { argv, doppler }`. Keep them and give them that shape through a local struct:

```rust
struct Normalized {
    argv: Vec<String>,
    doppler: Option<norm::Doppler>,
}

fn norm_view(k: &Known) -> Normalized {
    Normalized { argv: k.argv.clone(), doppler: k.doppler.clone() }
}
```

and replace the two `norm::normalize(&task.run)?` / `norm::normalize(&app.launch)?` calls with `configured(&task.run)?` / `configured(&app.launch)?`, reading `.argv` and `.doppler` from the result where `cfg.argv` and `cfg.doppler` were read. `project_hit`'s `typed: &[String]` parameter now receives `&inv.typed`.

`tasks.rs`, `sig.rs`, and `catalog.rs` import `norm::Doppler` and `norm::basename`, which remain.

- [ ] **Step 5: Run the whole guard suite**

Run: `cargo nextest run -p devkit-ports && cargo nextest run --test harness_guard`
Expected: PASS, including every pre-existing guard test unchanged. A pre-existing test that fails names a behaviour the migration changed; fix the migration, not the test, unless the change is one this plan makes on purpose (`git -C` rule matching), in which case say so in the commit body.

- [ ] **Step 6: Commit**

```bash
git add crates/devkit-ports crates/devkit-command/src/lib.rs
git rm crates/devkit-ports/src/guard/lex.rs
git commit -m "refactor(guard): decide over the shared command analysis"
```

The commit body notes that rules now match after git's global options, so `git -C dir worktree add` meets a `worktree add` rule.

---

### Task 16: Dialect resolution and the write stage

**Files:**
- Move: `src/bin/devkit/harness.rs` -> `src/bin/devkit/harness/mod.rs` (`git mv`, no content change in this task)
- Create: `src/bin/devkit/harness/dialect.rs`, `src/bin/devkit/harness/writes.rs`
- Modify: `Cargo.toml` (root package: `devkit-command.workspace = true`)

**Interfaces:**
- Consumes: `devkit_command::{Analysis, Dialect, Target, UncertaintyKind}`, `devkit_common::harness::{Harness, HarnessPolicy}`, `devkit_config::{PolicyAction, ShellSetting}`, `devkit_locks::{WriteResolver, model::{Conflict, WriteDecision}}`.
- Produces:
  - `dialect::resolve(setting: ShellSetting, harness: Harness, tool_name: Option<&str>, windows: bool) -> Dialect`
  - `writes::Evaluation { blocks: Vec<String>, warnings: Vec<String>, claims: Vec<String>, scopes: Vec<(String, bool)> }` with `needs_registry(&self) -> bool`
  - `writes::evaluate(analysis: &Analysis, policy: &HarnessPolicy) -> Evaluation`
  - `writes::enforce(evaluation: &Evaluation, holder: &str) -> anyhow::Result<Vec<Conflict>>`
  - `writes::conflict_message(&[Conflict]) -> String`
  - `writes::with_deadline<T: Send + 'static>(Duration, impl FnOnce() -> T + Send + 'static) -> Result<T, StageError>`; `StageError::{TimedOut, Panicked}`

- [ ] **Step 1: Move the module**

```bash
mkdir -p src/bin/devkit/harness
git mv src/bin/devkit/harness.rs src/bin/devkit/harness/mod.rs
cargo build
```

Expected: builds unchanged.

- [ ] **Step 2: Write the failing tests**

`src/bin/devkit/harness/dialect.rs`:

```rust
//! Which shell's syntax a hook command is read in.

use devkit_command::Dialect;
use devkit_common::harness::Harness;
use devkit_config::ShellSetting;

/// Resolve the dialect from what the payload and the platform establish.
///
/// The hook process's own environment is deliberately not an input: on
/// Claude Code's Windows `PowerShell` tool the hook runs under Git Bash, so
/// `SHELL` and `MSYSTEM` describe devkit's process, not the command's.
pub fn resolve(setting: ShellSetting, harness: Harness, tool_name: Option<&str>, windows: bool) -> Dialect {
    match setting {
        ShellSetting::Bash => Dialect::Bash,
        ShellSetting::Powershell => Dialect::PowerShell,
        ShellSetting::Auto => match (tool_name, harness) {
            (Some("PowerShell"), _) => Dialect::PowerShell,
            (_, Harness::Codex) if windows => Dialect::PowerShell,
            (_, Harness::ClaudeCode | Harness::Codex | Harness::Cursor) => Dialect::Bash,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_setting_wins() {
        assert_eq!(resolve(ShellSetting::Bash, Harness::Codex, Some("PowerShell"), true), Dialect::Bash);
        assert_eq!(resolve(ShellSetting::Powershell, Harness::ClaudeCode, Some("Bash"), false), Dialect::PowerShell);
    }

    #[test]
    fn auto_follows_tool_name_then_harness_and_platform() {
        assert_eq!(resolve(ShellSetting::Auto, Harness::ClaudeCode, Some("PowerShell"), true), Dialect::PowerShell);
        assert_eq!(resolve(ShellSetting::Auto, Harness::Codex, Some("Bash"), true), Dialect::PowerShell);
        assert_eq!(resolve(ShellSetting::Auto, Harness::Codex, Some("Bash"), false), Dialect::Bash);
        assert_eq!(resolve(ShellSetting::Auto, Harness::ClaudeCode, Some("Bash"), true), Dialect::Bash);
    }
}
```

`src/bin/devkit/harness/writes.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use devkit_command::{Context, Dialect, Limits, PathStyle};
    use devkit_common::harness::HarnessPolicy;
    use devkit_config::PolicyAction;

    use super::*;

    fn eval(command: &str, policy: HarnessPolicy) -> Evaluation {
        let ctx = Context { dialect: Dialect::Bash, cwd: Some("/repo".into()), path_style: PathStyle::Unix, limits: Limits::default() };
        evaluate(&devkit_command::analyze(command, &ctx), &policy)
    }

    #[test]
    fn known_targets_are_claims_and_tree_writers_are_scopes() {
        let e = eval("echo x > a.txt; mv b.txt c.txt; cargo fmt", HarnessPolicy::default());
        assert_eq!(e.claims, ["/repo/a.txt", "/repo/b.txt", "/repo/c.txt"]);
        assert_eq!(e.scopes, [("/repo".to_string(), true)]);
        assert!(e.blocks.is_empty());
    }

    #[test]
    fn each_policy_governs_only_its_own_findings() {
        let unresolved = "echo x > \"$OUT\"";
        assert_eq!(eval(unresolved, HarnessPolicy::default()).blocks.len(), 1);
        let warn = HarnessPolicy { unresolved_writes: PolicyAction::Warn, ..HarnessPolicy::default() };
        let e = eval(unresolved, warn);
        assert!(e.blocks.is_empty());
        assert_eq!(e.warnings.len(), 1);
        let allow = HarnessPolicy { unresolved_writes: PolicyAction::Allow, ..HarnessPolicy::default() };
        let e = eval(unresolved, allow);
        assert!(e.blocks.is_empty() && e.warnings.is_empty());

        assert_eq!(eval("perl -e 'print 1'", HarnessPolicy::default()).blocks.len(), 1);
        assert!(eval("python3 tools/gen.py", HarnessPolicy::default()).blocks.is_empty());
        let block_scripts = HarnessPolicy { script_files: PolicyAction::Block, ..HarnessPolicy::default() };
        assert_eq!(eval("python3 tools/gen.py", block_scripts).blocks.len(), 1);
    }

    #[test]
    fn an_allowed_finding_keeps_the_known_targets() {
        let allow = HarnessPolicy { unresolved_writes: PolicyAction::Allow, ..HarnessPolicy::default() };
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
        let r = with_deadline(Duration::from_millis(50), || std::thread::sleep(Duration::from_secs(5)));
        assert!(matches!(r, Err(StageError::TimedOut)));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(matches!(with_deadline(Duration::from_secs(5), || 7), Ok(7)));
        assert!(matches!(with_deadline(Duration::from_secs(5), || -> u8 { panic!("boom") }), Err(StageError::Panicked)));
    }
}
```

Add `mod dialect; mod writes;` to `harness/mod.rs` so the tests compile.

- [ ] **Step 3: Run to see them fail**

Run: `cargo nextest run --bin devkit harness`
Expected: compile failure on `writes::*`.

- [ ] **Step 4: Implement `writes.rs`**

```rust
//! The shell hook's write stage: turn an analysis into claims, scope checks
//! and policy findings, and claim through the same registry path structured
//! edits use.

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
                format!("{PREFIX} a {} target could not be determined. {UNRESOLVED_FIX}", op_name(effect.op)),
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
            UncertaintyKind::UnresolvedWrite | UncertaintyKind::ParseError | UncertaintyKind::LimitExhausted(_) | UncertaintyKind::UnresolvedInvocation => {
                e.apply(policy.unresolved_writes, format!("{PREFIX} {}. {UNRESOLVED_FIX}", u.detail))
            }
            UncertaintyKind::UnsupportedLanguage(language) => e.apply(
                policy.unsupported_language,
                format!("{PREFIX} {}. devkit cannot see what {language} source writes; make the edit in bash, PowerShell, Python, JavaScript or TypeScript, or with a structured edit tool.", u.detail),
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
            format!("{PREFIX} {name} is a stored script, and devkit does not read what it writes. Make the edit inline or with a structured edit tool."),
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
        if let WriteDecision::Denied(c) = resolver.decide_write(path, holder, Some("shell-harness"), 1800)? {
            conflicts.extend(c);
        }
    }
    Ok(conflicts)
}

pub fn conflict_message(conflicts: &[Conflict]) -> String {
    let who = conflicts.iter().map(|c| format!("{} (held by {})", c.path, c.held_by)).collect::<Vec<_>>().join(", ");
    format!("{PREFIX} {who} is locked by another agent; edit a different file or wait for it to finish")
}

#[derive(Debug)]
pub enum StageError {
    TimedOut,
    Panicked,
}

/// Run `work` on its own thread and wait at most `deadline`. On a timeout the
/// thread is left running; the caller exits the process, which ends it.
pub fn with_deadline<T: Send + 'static>(deadline: Duration, work: impl FnOnce() -> T + Send + 'static) -> Result<T, StageError> {
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
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run --bin devkit harness`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/bin/devkit/harness
git commit -m "feat(harness): resolve dialect and evaluate shell writes"
```

---

### Task 17: Compose the shell hook, register it, and test it end to end

**Files:**
- Modify: `src/bin/devkit/harness/mod.rs`
- Modify: `hooks/hooks.json`, `hooks/hooks-codex.json`
- Modify: `tests/harness_guard.rs` (clear `DEVKIT_ENFORCE_WRITES` in the helper)
- Create: `tests/harness_shell_writes.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: the `devkit harness shell` behaviour the spec's "Hook execution and locks" section defines.

- [ ] **Step 1: Write the failing end-to-end tests**

`tests/harness_shell_writes.rs`:

```rust
//! `devkit harness shell` with write enforcement: claims, conflicts, policy,
//! and the deadline, through the real binary. Each test has a private
//! project, HOME, and state directory, so no developer config, daemon, or
//! registry is reached.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Env {
    project: tempfile::TempDir,
    state: tempfile::TempDir,
}

fn env(config: &str) -> Env {
    let project = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(project.path()).args(["init", "-q", "-b", "main"]).output().unwrap();
    std::fs::write(project.path().join("devkit.toml"), config).unwrap();
    Env { project, state: tempfile::tempdir().unwrap() }
}

const WRITES: &str = "[harness]\nenforce_writes = true\n";

fn devkit(e: &Env, args: &[&str], stdin: Option<&str>, extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(args)
        .current_dir(e.project.path())
        .env("HOME", e.state.path())
        .env("XDG_STATE_HOME", e.state.path())
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .env_remove("DEVKIT_ENFORCE_COMMANDS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    if let Some(s) = stdin {
        pipe.write_all(s.as_bytes()).unwrap();
    }
    drop(pipe);
    child.wait_with_output().unwrap()
}

fn payload(e: &Env, session: Option<&str>, tool: &str, command: &str) -> String {
    let mut p = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "prompt_id": "p",
        "tool_input": { "command": command },
        "cwd": e.project.path().to_string_lossy(),
    });
    if let Some(s) = session {
        p["session_id"] = s.into();
    }
    p.to_string()
}

fn hook(e: &Env, session: Option<&str>, command: &str) -> Output {
    devkit(e, &["harness", "shell"], Some(&payload(e, session, "Bash", command)), &[])
}

fn acquire(e: &Env, holder: &str, path: &str) {
    let out = devkit(e, &["locks", "acquire", "--as", holder, path], None, &[]);
    assert!(out.status.success(), "acquire: {}", String::from_utf8_lossy(&out.stderr));
}

fn envelope(out: &Output) -> Option<serde_json::Value> {
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8_lossy(&out.stdout);
    (!s.trim().is_empty()).then(|| serde_json::from_str(&s).expect("stdout is JSON"))
}

fn denial(out: &Output) -> Option<String> {
    envelope(out).and_then(|v| {
        (v["hookSpecificOutput"]["permissionDecision"] == "deny").then(|| v["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap_or("").to_string())
    })
}

/// `(root-relative path, holder)` for every live row.
fn rows(e: &Env) -> Vec<(String, String)> {
    let file = e.state.path().join("devkit/locks.json");
    let Ok(body) = std::fs::read_to_string(file) else { return Vec::new() };
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let mut out: Vec<(String, String)> = v["locks"]
        .as_object()
        .map(|m| m.values().map(|r| (r["path"].as_str().unwrap().to_string(), r["holder"].as_str().unwrap().to_string())).collect())
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn a_free_redirect_target_is_claimed_for_the_session() {
    let e = env(WRITES);
    let out = hook(&e, Some("S1"), "printf '%s\\n' 'print(1)' > .temp_demo.py");
    assert_eq!(denial(&out), None);
    assert_eq!(rows(&e), [(".temp_demo.py".to_string(), "S1".to_string())]);
    assert!(!e.project.path().join(".temp_demo.py").exists(), "the hook never runs the command");
}

#[test]
fn another_sessions_lock_denies() {
    let e = env(WRITES);
    acquire(&e, "S2", "a.txt");
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("S2"), "{reason}");
    assert_eq!(rows(&e), [("a.txt".to_string(), "S2".to_string())]);
}

#[test]
fn own_and_ancestor_claims_allow() {
    let e = env(WRITES);
    acquire(&e, "S1", "a.txt");
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    let sub = devkit(&e, &["harness", "shell"], Some(&{
        let mut p: serde_json::Value = serde_json::from_str(&payload(&e, Some("S1"), "Bash", "echo x > a.txt")).unwrap();
        p["agent_id"] = "a1".into();
        p.to_string()
    }), &[]);
    assert_eq!(denial(&sub), None);
}

#[test]
fn both_ends_of_a_rename_are_checked() {
    let e = env(WRITES);
    acquire(&e, "S2", "b.txt");
    assert!(denial(&hook(&e, Some("S1"), "mv a.txt b.txt")).is_some());
}

#[test]
fn an_outer_redirect_around_a_devkit_command_is_enforced() {
    let e = env("[harness]\nenforce_writes = true\nenforce_commands = true\n");
    acquire(&e, "S2", "shared.txt");
    assert!(denial(&hook(&e, Some("S1"), "devrun task check > shared.txt")).is_some());
}

#[test]
fn an_unresolved_write_blocks_by_default_and_warns_when_configured() {
    let e = env(WRITES);
    let reason = denial(&hook(&e, Some("S1"), "echo x > \"$OUT\"")).expect("denied");
    assert!(reason.contains("literal"), "{reason}");

    let w = env("[harness]\nenforce_writes = true\nunresolved_writes = \"warn\"\n");
    let v = envelope(&hook(&w, Some("S1"), "echo x > \"$OUT\"")).expect("a warning envelope");
    assert!(v["hookSpecificOutput"]["additionalContext"].as_str().is_some_and(|s| s.contains("could not be determined")), "{v}");
    assert!(v["hookSpecificOutput"].get("permissionDecision").is_none(), "{v}");
}

#[test]
fn a_warning_cannot_override_a_known_conflict() {
    let e = env("[harness]\nenforce_writes = true\nunresolved_writes = \"warn\"\n");
    acquire(&e, "S2", "a.txt");
    assert!(denial(&hook(&e, Some("S1"), "echo x > a.txt; echo y > \"$OUT\"")).is_some());
}

#[test]
fn an_argv_bound_python_edit_claims_its_target() {
    let e = env(WRITES);
    let command = "f=src/a.ts; python3 - \"$f\" <<'PY'\nimport sys\nfrom pathlib import Path\nPath(sys.argv[1]).write_text('x')\nPY\n";
    assert_eq!(denial(&hook(&e, Some("S1"), command)), None);
    assert_eq!(rows(&e), [("src/a.ts".to_string(), "S1".to_string())]);
}

#[test]
fn read_only_commands_and_quoted_text_claim_nothing() {
    let e = env(WRITES);
    for command in ["cat README.md | rg foo", "git commit -m \"echo x > y.txt\"", "node -e \"console.log(require.resolve('x'))\""] {
        assert_eq!(denial(&hook(&e, Some("S1"), command)), None, "{command}");
    }
    assert!(rows(&e).is_empty(), "{:?}", rows(&e));
}

#[test]
fn a_tree_writer_is_checked_and_claims_nothing() {
    let e = env(WRITES);
    assert_eq!(denial(&hook(&e, Some("S1"), "cargo fmt")), None);
    assert!(rows(&e).is_empty());
    acquire(&e, "S2", "src/lib.rs");
    assert!(denial(&hook(&e, Some("S1"), "cargo fmt")).is_some());
    assert_eq!(rows(&e), [("src/lib.rs".to_string(), "S2".to_string())]);
}

#[test]
fn unsupported_language_and_script_file_policies() {
    let e = env(WRITES);
    assert!(denial(&hook(&e, Some("S1"), "perl -e 'print 1'")).is_some());
    assert_eq!(denial(&hook(&e, Some("S1"), "python3 tools/gen.py")), None);

    let open = env("[harness]\nenforce_writes = true\nunsupported_language = \"allow\"\nscript_files = \"block\"\n");
    assert_eq!(denial(&hook(&open, Some("S1"), "perl -e 'print 1'")), None);
    assert!(denial(&hook(&open, Some("S1"), "python3 tools/gen.py")).is_some());
}

#[test]
fn a_write_without_a_session_id_is_denied() {
    let e = env(WRITES);
    let reason = denial(&hook(&e, None, "echo x > a.txt")).expect("denied");
    assert!(reason.contains("session_id"), "{reason}");
}

#[test]
fn with_write_enforcement_off_the_hook_writes_no_registry_row() {
    let e = env("[harness]\nenforce_commands = true\n");
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    assert!(!e.state.path().join("devkit/locks.json").exists());
}

#[test]
fn a_malformed_rule_does_not_disable_write_enforcement() {
    let e = env("[harness]\nenforce_writes = true\nenforce_commands = true\n[harness.commands.bad]\nprograms = \"git\"\n");
    assert_eq!(denial(&hook(&e, Some("S1"), "echo x > a.txt")), None);
    assert_eq!(rows(&e), [("a.txt".to_string(), "S1".to_string())]);
}

/// An advisory lock on `path`, created if absent. The caller takes the guard
/// from it and holds the guard for as long as the lock should be held.
fn hold(path: &Path) -> fd_lock::RwLock<std::fs::File> {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).unwrap();
    fd_lock::RwLock::new(file)
}

#[test]
fn a_stalled_registry_denies_within_the_deadline() {
    let e = env(WRITES);
    let mut lock = hold(&e.state.path().join("devkit/locks.lock"));
    let _held = lock.write().unwrap();
    let start = Instant::now();
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("did not answer"), "{reason}");
    assert!(start.elapsed() < Duration::from_secs(20), "took {:?}", start.elapsed());
}

#[test]
fn a_registry_failure_denies() {
    let e = env(WRITES);
    let mut gate = hold(&e.state.path().join("devkit/devkitd.lock"));
    let _held = gate.write().unwrap();
    let reason = denial(&hook(&e, Some("S1"), "echo x > a.txt")).expect("denied");
    assert!(reason.contains("registry error"), "{reason}");
}

#[test]
fn the_powershell_tool_is_read_as_powershell_whatever_the_hook_env_says() {
    let e = env(WRITES);
    let out = devkit(
        &e,
        &["harness", "shell"],
        Some(&payload(&e, Some("S1"), "PowerShell", "Set-Content -Path out.txt -Value x")),
        &[("SHELL", "/usr/bin/bash"), ("MSYSTEM", "MINGW64")],
    );
    assert_eq!(denial(&out), None);
    assert_eq!(rows(&e), [("out.txt".to_string(), "S1".to_string())]);
}

#[test]
fn a_codex_payload_claims_through_the_same_path() {
    let e = env(WRITES);
    let p = serde_json::json!({
        "hook_event_name": "PreToolUse", "tool_name": "Bash", "turn_id": "t", "model": "m", "session_id": "C1",
        "tool_input": { "command": "echo x > c.txt" }, "cwd": e.project.path().to_string_lossy(),
    });
    assert_eq!(denial(&devkit(&e, &["harness", "shell"], Some(&p.to_string()), &[])), None);
    assert_eq!(rows(&e), [("c.txt".to_string(), "C1".to_string())]);
}

#[test]
fn a_cursor_payload_never_claims() {
    let e = env(WRITES);
    let p = serde_json::json!({ "command": "echo x > a.txt", "cwd": e.project.path().to_string_lossy(), "conversation_id": "x" });
    let out = devkit(&e, &["harness", "shell"], Some(&p.to_string()), &[]);
    assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
    assert!(rows(&e).is_empty());
}
```

`the_powershell_tool_is_read_as_powershell_whatever_the_hook_env_says` runs on every CI platform: on Linux and macOS the path style is Unix, and the assertion is on the root-relative row, which is the same everywhere.

In `tests/harness_guard.rs`, add `.env_remove("DEVKIT_ENFORCE_WRITES")` to `run_hook_with`, so a developer's machine-wide switch cannot turn the write stage on under the guard's own tests.

- [ ] **Step 2: Run to see them fail**

Run: `cargo nextest run --test harness_shell_writes`
Expected: FAIL; the hook claims nothing yet.

- [ ] **Step 3: Compose the hook**

Replace the body of `src/bin/devkit/harness/mod.rs` from the module doc through `guard_shell` (keep `print_envelope`, `warn`, `load_project`, the `compile_error!` guard, `HarnessCli`, `Cmd`, and `run`):

```rust
//! `devkit harness shell`: the pre-execution hook for shell tools.
//!
//! One analysis of the command feeds two stages. The command guard fails
//! open: its own failures allow the command. The write stage, active for
//! Claude Code and Codex when `enforce_writes` is on, fails closed: a write it
//! cannot evaluate, a registry it cannot reach, or a deadline it misses is a
//! denial.

mod dialect;
mod writes;

use std::{
    io::{Read, Write},
    sync::OnceLock,
    time::Duration,
};

use anyhow::Result;
use clap::{Args, Subcommand};
use devkit_command::{Context, Limits, PathStyle};
use devkit_common::harness::{self, Harness};
use devkit_ports::guard::{self, Project};

/// Longer than a healthy registry ever takes and well inside the manifest's
/// 30-second timeout, which allows the call when it fires.
const WRITE_STAGE_DEADLINE: Duration = Duration::from_secs(5);

enum Response {
    Silent,
    Envelope(serde_json::Value),
}

/// Never returns an error. A panic allows the command unless the write stage
/// had started, in which case it denies.
fn guard_shell() {
    let write_stage: OnceLock<Harness> = OnceLock::new();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| respond(&write_stage)));
    match outcome {
        Ok(Response::Envelope(v)) => print_envelope(&v),
        Ok(Response::Silent) => {}
        Err(_) => match write_stage.get() {
            Some(h) => print_envelope(&harness::deny_shell_json(
                *h,
                "devkit write-harness: internal failure while evaluating a shell write (fail-closed)",
            )),
            None => warn("command guard panicked; allowing the command"),
        },
    }
}

fn deny(which: Harness, reasons: &[String]) -> Response {
    Response::Envelope(harness::deny_shell_json(which, &reasons.join("\n")))
}

fn respond(write_stage: &OnceLock<Harness>) -> Response {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return Response::Silent;
    }
    let payload: serde_json::Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(e) => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            return if harness::writes_enabled(&cwd) {
                Response::Envelope(harness::deny_json(&format!("devkit write-harness: hook payload did not parse ({e}) (fail-closed)")))
            } else {
                Response::Silent
            };
        }
    };
    let Some(shell) = harness::parse_shell_payload(&payload) else {
        return Response::Silent;
    };
    let Some(cwd) = shell.cwd.clone().or_else(|| std::env::current_dir().ok()) else {
        return Response::Silent;
    };
    let commands_on = harness::commands_enabled(&cwd);
    let writes_on = shell.harness != Harness::Cursor && harness::writes_enabled(&cwd);
    if !commands_on && !writes_on {
        return Response::Silent;
    }
    if writes_on {
        let _ = write_stage.set(shell.harness);
    }

    let (rules, warnings) = harness::resolve_rules(&cwd);
    for w in &warnings {
        warn(w);
    }
    let ctx = Context {
        dialect: dialect::resolve(rules.policy.shell, shell.harness, shell.tool_name.as_deref(), cfg!(windows)),
        cwd: shell.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        path_style: if cfg!(windows) { PathStyle::Windows } else { PathStyle::Unix },
        limits: Limits::default(),
    };
    let analysis = devkit_command::analyze(&shell.command, &ctx);

    let mut blocks: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    if commands_on {
        let project = load_project(&cwd, rules.app_match.clone());
        let verdict = guard::decide(&analysis, &rules.commands, project.as_ref());
        blocks.extend(verdict.blocks.into_iter().map(|f| f.message));
        notes.extend(verdict.warnings.into_iter().map(|f| f.message));
    }
    if writes_on {
        let evaluation = writes::evaluate(&analysis, &rules.policy);
        blocks.extend(evaluation.blocks.iter().cloned());
        notes.extend(evaluation.warnings.iter().cloned());
        if blocks.is_empty() && evaluation.needs_registry() {
            let Some(session) = shell.session_id.clone() else {
                return deny(shell.harness, &["devkit write-harness: shell write payload carries no session_id (fail-closed)".into()]);
            };
            let holder = devkit_locks::hook::holder_from_fields(&session, shell.agent_id.as_deref());
            match writes::with_deadline(WRITE_STAGE_DEADLINE, move || writes::enforce(&evaluation, &holder)) {
                Ok(Ok(conflicts)) if conflicts.is_empty() => {}
                Ok(Ok(conflicts)) => blocks.push(writes::conflict_message(&conflicts)),
                Ok(Err(e)) => blocks.push(format!("devkit write-harness: registry error (fail-closed): {e:#}")),
                Err(writes::StageError::Panicked) => {
                    blocks.push("devkit write-harness: internal failure while claiming shell write targets (fail-closed)".into())
                }
                Err(writes::StageError::TimedOut) => {
                    print_envelope(&harness::deny_shell_json(
                        shell.harness,
                        &format!(
                            "devkit write-harness: the lock registry did not answer within {}s (fail-closed). Retry; if it persists, check `lockm status` and `devkit doctor`.",
                            WRITE_STAGE_DEADLINE.as_secs()
                        ),
                    ));
                    // The worker is still blocked on the registry; exiting the
                    // process is what ends it.
                    std::process::exit(0);
                }
            }
        }
    }
    if !blocks.is_empty() {
        return deny(shell.harness, &blocks);
    }
    if notes.is_empty() {
        return Response::Silent;
    }
    harness::warn_shell_json(shell.harness, &notes.join("\n")).map_or(Response::Silent, Response::Envelope)
}
```

Remove the now-unused `Decision` and `ShellPayload` imports.

- [ ] **Step 4: Register the hooks**

In `hooks/hooks.json`, change the shell entry to:

```json
      {
        "matcher": "Bash|PowerShell",
        "hooks": [
          {
            "type": "command",
            "command": "devkit harness shell",
            "timeout": 30
          }
        ]
      }
```

In `hooks/hooks-codex.json`, change the `devkit harness shell` entry's `"timeout": 10` to `"timeout": 30`. Leave `hooks/hooks-cursor.json` unchanged.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run --test harness_shell_writes --test harness_guard --test locks`
Expected: PASS.

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/bin/devkit/harness hooks/hooks.json hooks/hooks-codex.json tests/harness_shell_writes.rs tests/harness_guard.rs
git commit -m "feat(harness): claim locks for shell writes"
```

---

### Task 18: Documentation and the invariant

**Files:**
- Modify: `AGENTS.md`, `docs/configuration.md`, `docs/commands.md`, `skills/using-devkit/SKILL.md`, `skills/using-devkit/references/locks.md`, `README.md` if `rg -n "Bash write|shell write" README.md` matches

**Interfaces:** none.

- [ ] **Step 1: Rewrite the command-guard invariant in `AGENTS.md`**

Replace the bullet that begins `- **The command guard fails open and never mutates.**` with:

```markdown
- **The shell hook's command guard fails open; its write stage fails closed.** `devkit harness shell` analyzes a command once with `devkit-command` and hands the result to two stages. The command guard allocates no port, writes no registry row, takes no lock, and never calls `task::resolve`; any failure of its own (a config that will not load, a malformed rule, a panic before the write stage starts) warns on stderr and allows the command. The write stage runs only for Claude Code and Codex when `enforce_writes` is on. It claims statically resolved targets through `WriteResolver::decide_write`, the path structured edits use, checks tree effects with `check_scope` without claiming, and denies on an unusable payload, a missing `session_id`, a registry error, a panic, or the 5-second deadline. The deadline is in-process because a harness timeout allows the call, so the manifest timeout (30 seconds) is only a backstop. `devkit-command` itself never reads config, a file, or a registry, and never runs anything.
```

- [ ] **Step 2: Update `docs/configuration.md`**

In the `[harness]` key table, add rows after `enforce_commands`:

```markdown
| `shell` | `"auto"` \| `"bash"` \| `"powershell"` | `"auto"` | The shell a hook command is read in. `auto`: Claude Code's `PowerShell` tool is PowerShell; Codex on Windows is PowerShell; everything else is bash. The hook process's own `SHELL` and `MSYSTEM` are never consulted. |
| `unresolved_writes` | `"block"` \| `"warn"` \| `"allow"` | `"block"` | A write, or code that may write, whose target devkit could not determine. |
| `unsupported_language` | `"block"` \| `"warn"` \| `"allow"` | `"block"` | Executable source in a language devkit cannot analyze, such as `perl -e`, `ruby -e`, `awk` programs, or `nu -c`. |
| `script_files` | `"block"` \| `"warn"` \| `"allow"` | `"allow"` | A call to a stored script (`python3 tools/gen.py`, `bash deploy.sh`), whose contents devkit does not read. |
```

Replace the paragraph beginning `**What enforcement gates.**` with:

```markdown
**What enforcement gates.** The write hook intercepts `Edit`, `MultiEdit`, `Write`, `NotebookEdit`, and Codex's `apply_patch`. Shell tools (`Bash` and Claude Code's `PowerShell`, on Claude Code and Codex) go through `devkit harness shell`, which parses the command and the scripts it runs and claims every write target it can resolve before the command runs. It understands redirects, `tee`, `cp`, `mv`, `rm`, `touch`, `dd`, `sed -i`, `perl -i`, the git verbs that rewrite files, and common formatters; inline Python, JavaScript and TypeScript file APIs, including a target passed as `sys.argv` or `process.argv` from a literal shell variable; and PowerShell's content and item cmdlets and `System.IO.File`. A whole-tree writer such as `cargo fmt` or `git checkout` claims nothing and is refused while another session holds a lock under the tree. A program devkit does not model, including build tools and package managers, is not treated as a writer. `devrun task <name>` is not expanded, so a task that formats the tree is not checked. Cursor keeps the command guard only; its shell calls claim nothing.

A write devkit cannot resolve is not treated as covered. `unresolved_writes`, `unsupported_language`, and `script_files` decide what happens: `block` refuses the call and says how to make the target explicit, `warn` allows it and tells the agent what was not checked, `allow` says nothing. A warning cannot override a conflict on a target devkit did resolve. Holding a lock on some other path is never taken as covering an unresolved write.
```

If Task 13 Step 1 found Codex has no allow-with-context channel, add: "On Codex, `warn` is a silent allow: its hook output has no field that carries text to the agent on an allowed call."

After the paragraph beginning `**What the command guard gates.**`, add:

```markdown
**Command rules.** A `[harness.commands.<name>]` rule matches a parsed invocation: `programs` against the program's basename, `args` against its leading arguments after the program's own global options are removed, so `git -C /repo worktree add` meets `args = ["worktree", "add"]` and `git worktree list` does not. Rules see nested commands (`bash -c '...'`, a Python `subprocess.run([...])`), never quoted text that only mentions one. `enabled = false` turns an inherited rule off. `action` is `block` (default) or `warn`; `severity` is `info`, `warning`, or `error` (default) and classifies the message. When an argument cannot be resolved (`git "$verb" add`), a rule that might match warns and allows. `shell`, `unresolved_writes`, `unsupported_language`, and `script_files` take the closest layer's value; `enforce_writes` and `enforce_commands` still turn on if any layer sets them.
```

- [ ] **Step 3: Update `docs/commands.md`**

Replace the `devkit harness shell` paragraph (starts with `` `devkit harness shell` is the pre-execution hook entry point ``) with:

```markdown
`devkit harness shell` is the pre-execution hook entry point for shell tools. It reads the hook payload on stdin, parses the command and any script it runs, and answers on stdout with a denial, a warning, or nothing. With `enforce_commands` on it refuses commands devkit already has a wired-up path for, and fails open. With `enforce_writes` on, for Claude Code and Codex, it claims the files the command writes before it runs, using the session's lock identity, and fails closed: an unresolved write, a registry error, or a registry that does not answer within 5 seconds is a denial. It always exits 0 and never runs the command. Wired into the plugin's `PreToolUse` hooks for `Bash` and `PowerShell` (Claude Code and Codex) and `beforeShellExecution` (Cursor, command guard only); see [configuration.md](configuration.md#harness).
```

- [ ] **Step 4: Update the skill**

In `skills/using-devkit/SKILL.md`, replace the last sentence of the first `## Enforced checkouts` paragraph ("Claim by hand only for a `Bash` write the hook never sees.") with "Shell writes are claimed too when devkit can resolve their targets." Replace the second paragraph's sentences from "`Bash` writes are not covered" to the end of that sentence with:

```markdown
A shell write is claimed automatically when its target is a literal path or a variable assigned a literal in the same command, including a path passed to an inline `python3 -` or `node -e` script as an argument. When devkit cannot resolve a target (a path built at runtime, a script file, `perl -e`), the call is refused with the reason; rewrite the edit with an explicit path or use `Edit`/`Write`. Claiming some other path with `lockm acquire` does not get an unresolved write through.
```

In `skills/using-devkit/references/locks.md`, add a section after the enforcement switches list:

```markdown
## Shell writes

`devkit harness shell` claims the targets of a shell command before it runs, with the same holder, TTL, and release as `Edit`/`Write`. A conflict is refused naming the holder. A whole-tree writer (`cargo fmt`, `git checkout`, `rm -r dir`) claims nothing and is refused while another session holds any lock under the tree. What devkit cannot resolve follows `[harness] unresolved_writes`, `unsupported_language`, and `script_files` (`block`, `warn`, or `allow`); the defaults block the first two and allow script files. A refused unresolved write is fixed by making the target explicit, not by acquiring a lock.
```

- [ ] **Step 5: Verify**

Run: `cargo nextest run --workspace --no-fail-fast && cargo test --workspace --doc`
Expected: PASS. Read the rendered diffs once for stale wording: `rg -n "Shell-level \*writes\*|never mutates|documented gap" AGENTS.md docs skills` returns nothing.

- [ ] **Step 6: Commit**

```bash
git add AGENTS.md docs skills README.md
git commit -m "docs(harness): describe shell write enforcement"
```

---

### Task 19: Measure against the corpus

**Files:**
- Create: `crates/devkit-command/examples/corpus_probe.rs`

**Interfaces:**
- Consumes: `devkit_command::analyze`.
- Produces: a local-only report. Nothing from the corpus is committed.

This is not a precision or recall result: the corpus has no labels for what each command actually wrote. It answers two questions the spec leaves open: how often the default policy would refuse a recorded call, and which unresolved shapes dominate.

- [ ] **Step 1: Write the probe**

```rust
//! Run the analyzer over a frozen shell-call corpus and print how the default
//! write policy would treat it. Local measurement only: pass the path to a
//! `corpus.jsonl` from `data_analysis.local`; nothing is written anywhere.

use std::collections::BTreeMap;

use devkit_command::{Context, Dialect, Limits, PathStyle, Target, UncertaintyKind};

fn command_of(record: &serde_json::Value) -> Option<&str> {
    record.get("command").and_then(|c| c.as_str()).or_else(|| {
        record.as_object()?.values().find_map(|v| v.get("command").and_then(|c| c.as_str()))
    })
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: corpus_probe <corpus.jsonl>");
    let body = std::fs::read_to_string(&path).expect("read corpus");
    let mut cohorts: BTreeMap<String, [usize; 7]> = BTreeMap::new();
    let mut details: BTreeMap<String, usize> = BTreeMap::new();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let record: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
        let Some(command) = command_of(&record) else { continue };
        let harness = record.get("harness").and_then(|v| v.as_str()).unwrap_or("?");
        let platform = record.get("platform").and_then(|v| v.as_str()).unwrap_or("?");
        let tool = record.get("tool_name").and_then(|v| v.as_str()).unwrap_or("Bash");
        let dialect = if tool == "PowerShell" || (harness == "codex" && platform == "windows") { Dialect::PowerShell } else { Dialect::Bash };
        let ctx = Context {
            dialect,
            cwd: record.get("cwd").and_then(|v| v.as_str()).map(str::to_string),
            path_style: if platform == "windows" && dialect == Dialect::PowerShell { PathStyle::Windows } else { PathStyle::Unix },
            limits: Limits::default(),
        };
        let a = devkit_command::analyze(command, &ctx);
        let unresolved = a.file_effects.iter().any(|e| e.target == Target::Unresolved)
            || a.uncertainties.iter().any(|u| matches!(u.kind, UncertaintyKind::UnresolvedWrite | UncertaintyKind::ParseError | UncertaintyKind::LimitExhausted(_) | UncertaintyKind::UnresolvedInvocation));
        let unsupported = a.uncertainties.iter().any(|u| matches!(u.kind, UncertaintyKind::UnsupportedLanguage(_)));
        let row = cohorts.entry(format!("{harness}/{platform} {tool}")).or_default();
        row[0] += 1;
        row[1] += usize::from(a.file_effects.iter().any(|e| matches!(e.target, Target::Path(_))));
        row[2] += usize::from(!a.tree_effects.is_empty());
        row[3] += usize::from(unresolved);
        row[4] += usize::from(unsupported);
        row[5] += usize::from(!a.script_files.is_empty());
        row[6] += usize::from(unresolved || unsupported);
        for u in &a.uncertainties {
            let shape: String = u.detail.split('`').enumerate().map(|(i, part)| if i % 2 == 1 { "`_`" } else { part }).collect();
            *details.entry(shape).or_default() += 1;
        }
    }
    println!("cohort | calls | claims | tree | unresolved | unsupported | script files | refused by default");
    for (cohort, r) in &cohorts {
        println!("{cohort} | {} | {} | {} | {} | {} | {} | {}", r[0], r[1], r[2], r[3], r[4], r[5], r[6]);
    }
    let mut ranked: Vec<_> = details.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
    println!("\nmost common uncertainty shapes (names elided):");
    for (shape, n) in ranked.into_iter().take(20) {
        println!("{n:>6}  {shape}");
    }
}
```

Backtick-quoted names in uncertainty details are elided so the printed report carries no program, function, or module names from recorded sessions.

- [ ] **Step 2: Run it against the latest snapshot**

Run: `cargo run -p devkit-command --release --example corpus_probe -- /home/lev/Git/lev/devkit/data_analysis.local/outputs-20260912T195539042804Z/corpus.jsonl`

If `data_analysis.local` has a newer `outputs-*` directory, use that one and name it in the report. Expected: a table per cohort and a ranked list of uncertainty shapes.

- [ ] **Step 3: Check the misses and false blocks by hand**

From the same corpus, pick 20 calls the probe marks "refused by default" and 20 marked read-only, spread across cohorts, and read each command locally. For each refused call, record whether the refusal is correct (an edit whose target devkit could not know) or a false block (read-only, or a target devkit should have resolved). For each read-only call, record whether it writes something the analyzer missed. Keep these notes outside the repository.

A false block or a miss whose shape is common in the ranked list gets a synthetic fixture in the adapter it belongs to, a failing test, and a fix, as its own commit before this task closes.

- [ ] **Step 4: Commit the probe**

```bash
git add crates/devkit-command/examples/corpus_probe.rs
git commit -m "test(command): add a local corpus probe"
```

Report the per-cohort table and the hand-check tallies to the user in the PR description, labelled with the snapshot directory name. They are a snapshot, not a claim of accuracy, and they do not go into any committed document.

---

## Self-review notes

Spec coverage, section by section:

- Separate crate, Tree-sitter, the six grammars, no config or registry dependency: Task 1.
- Analysis contract (invocations, file and tree effects, script files, uncertainties, locations, wrappers kept visible): Tasks 1, 2, 4.
- Runtime input, payload identity, missing fields stay missing, Codex vs Claude Code: Task 13.
- Shell dialect resolution and its order, hook-process env ignored: Tasks 16, 17.
- Execution directory, `cd`/`Set-Location`, relative without cwd unresolved: Tasks 2, 3, 8.
- Grammars pinned, PowerShell recovery per statement, tripwire on upstream fixes: Tasks 1, 3, 8.
- Embedded source from recognized forms only, heredoc data vs executable, unknown expansions: Tasks 3, 5.
- Interpreter arguments into `sys.argv` and `process.argv`: Tasks 5, 7, 9, 17.
- Unsupported languages, fish in v1: Tasks 5, 10.
- Effect catalog, unmodeled programs silent, `devrun task` not expanded: Task 6; the gap is documented in Task 18.
- Tree effects as conflict checks: Tasks 6, 14, 16, 17.
- Shell and PowerShell bindings, branches invalidate: Tasks 3, 8. Redirects independent of rule exemptions: Tasks 6, 17.
- Python and JavaScript/TypeScript rules (receivers, rebinding, function bodies, defaults, shadowing, `require.resolve` negatives): Tasks 7, 9.
- Configuration keys, action vocabulary, rule fields, no `allow` action, no regex: Task 12.
- Inheritance and precedence, per-key parsing, malformed rules isolated from writes: Tasks 13, 17.
- Hook stages, `decide_write` claims, missing identity denies, deadline and manifest timeout, warning channel, Cursor excluded, PowerShell registration: Tasks 13, 16, 17.
- Failure handling (write stage closed, guard open, panic boundary chooses): Task 17.
- Limits: Tasks 1, 11.
- Validation list: Tasks 3 to 11 (analysis), 15 (rules), 17 (hook), 19 (corpus).
- Documentation and the rewritten invariant: Task 18.

The panic boundary's write-stage branch has no automated test: nothing in the hook can be made to panic from outside without a test-only hook, and adding one to production code for this is worse than the gap.

## Unresolved questions

1. **`mkdir` is not a file effect (Decision 1).** The spec lists `mkdir` among catalog commands. Claiming a directory locks its whole subtree for 30 minutes, so this plan treats directory creation as effect-free. Confirm, or say what `mkdir` should do.
2. **Codex's warning channel.** Task 13 Step 1 checks whether Codex's `PreToolUse` output can carry text on an allowed call. If it cannot, `warn` is a silent allow on Codex. Is that acceptable, or should `warn` block on Codex instead?
3. **The default for `unresolved_writes`.** It stays `block` as the spec says. Task 19 measures how often that refuses recorded calls, and the hand check separates correct refusals from false blocks. If the false-block share is high, do you want the default revisited before merge, or shipped as specified with the numbers in the PR?

