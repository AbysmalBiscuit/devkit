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
    /// A path an API created fresh under a random name, atomically, carrying
    /// the directory it was created in. Resolves to [`Target::Ephemeral`]; see
    /// its documentation for why that is not [`Value::Unknown`].
    Ephemeral(Option<String>),
}

impl Value {
    pub fn known(&self) -> Option<&str> {
        match self {
            Value::Known(s) => Some(s),
            Value::Unknown | Value::Ephemeral(_) => None,
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
    /// `tempfile` entry, `mktemp`, `fs.mkdtemp`. No other session already holds
    /// that name and none can independently produce it, so it needs no claim of
    /// its own. Distinct from `Unresolved`, whose path is merely unknown and
    /// may well be shared.
    ///
    /// `dir` is the directory the caller asked for it to be created in, when
    /// there was one. A fresh name proves nothing about that directory. A claim
    /// on it, or on one of its ancestors, covers every path born under it, so
    /// `dir` is still checked for conflicts. `None` means the API chose the
    /// system temp directory, which no project claim reaches.
    Ephemeral {
        dir: Option<String>,
    },
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
