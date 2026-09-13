#![allow(dead_code)]

use crate::{
    analyzer::Stdin,
    model::{Language, Value},
};

pub(crate) enum Exec {
    Source {
        language: Language,
        source: Value,
        script_args: Vec<Value>,
    },
    ScriptFile {
        script: Value,
    },
    Unsupported {
        language: &'static str,
    },
    Plain,
}

pub(crate) fn classify(_name: &str, _args: &[Value], _stdin: &Stdin) -> Exec {
    Exec::Plain
}
