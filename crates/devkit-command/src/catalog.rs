#![allow(dead_code)]

use crate::model::{FileOp, Value};

pub(crate) enum Hit {
    File(FileOp, Value),
    Tree {
        scope: Value,
        whole_checkout: bool,
        by: String,
    },
    Unresolved(String),
    ScriptFile(Value),
}

pub(crate) fn effects(_name: &str, _args: &[Value]) -> Vec<Hit> {
    Vec::new()
}

pub(crate) fn is_cataloged(_name: &str) -> bool {
    false
}
