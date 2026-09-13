#![allow(dead_code)]

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Word},
    context::PathStyle,
    model::Value,
};

pub(crate) fn basename(prog: &str) -> &str {
    prog.rsplit(['/', '\\']).next().unwrap_or(prog)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CwdChange {
    Inherit,
    To(String),
    Unknown,
}

impl CwdChange {
    pub(crate) fn apply(&self, inherited: Option<String>) -> Option<String> {
        match self {
            Self::Inherit => inherited,
            Self::To(dir) => Some(dir.clone()),
            Self::Unknown => None,
        }
    }
}

pub(crate) struct Unwrapped {
    pub(crate) argv: Vec<Word>,
    pub(crate) wrappers: Vec<Vec<Word>>,
    pub(crate) cwd: CwdChange,
}

pub(crate) fn unwrap(_a: &mut Analyzer<'_>, raw: &RawInvocation, _frame: &Frame) -> Unwrapped {
    Unwrapped {
        argv: raw.words.clone(),
        wrappers: Vec::new(),
        cwd: CwdChange::Inherit,
    }
}

pub(crate) struct ProgramOptions {
    pub(crate) semantic_args: Vec<Value>,
    pub(crate) cwd: CwdChange,
}

pub(crate) fn program_options(
    _program: &Value,
    args: &[Value],
    _cwd: Option<&str>,
    _style: PathStyle,
) -> ProgramOptions {
    ProgramOptions {
        semantic_args: args.to_vec(),
        cwd: CwdChange::Inherit,
    }
}
