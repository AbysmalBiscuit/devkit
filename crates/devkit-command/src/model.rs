//! The analysis result every consumer reads.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub outer: Range<usize>,
    pub embedded: Option<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Known(String),
    Unknown,
    /// A path an API created fresh under a random name, atomically. Resolves to
    /// [`Target::Ephemeral`]; see its documentation for why that is not
    /// [`Value::Unknown`].
    Ephemeral,
}

impl Value {
    pub fn known(&self) -> Option<&str> {
        match self {
            Value::Known(s) => Some(s),
            Value::Unknown | Value::Ephemeral => None,
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
    pub program: Value,
    pub args: Vec<Value>,
    pub semantic_args: Vec<Value>,
    pub wrappers: Vec<Vec<Value>>,
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
    /// A path an API created fresh under a random name, atomically: a
    /// `tempfile` entry, `mktemp`, `fs.mkdtemp`. No other session can already
    /// hold it and none can independently produce it, so it is uncontendable
    /// and needs no claim. Distinct from `Unresolved`, whose path is merely
    /// unknown and may well be shared.
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEffect {
    pub op: FileOp,
    pub target: Target,
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEffect {
    pub scope: String,
    pub whole_checkout: bool,
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
    UnresolvedWrite,
    UnsupportedLanguage(String),
    ParseError,
    LimitExhausted(Limit),
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
