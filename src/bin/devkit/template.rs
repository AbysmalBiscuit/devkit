use std::collections::BTreeMap;

use anyhow::Result;

/// `--arg` and `--arg-file`, shared by every command that renders templates.
#[derive(clap::Args, Debug, Default)]
pub(crate) struct VarArgs {
    /// Set a variable the templates read: `--arg key=value`. Repeatable.
    #[arg(short = 'a', long = "arg", value_name = "KEY=VALUE")]
    pub args: Vec<String>,
    /// Set a variable to a file's contents, trailing newline included:
    /// `--arg-file key=path`, or `key=-` for stdin. Repeatable.
    #[arg(long = "arg-file", value_name = "KEY=PATH")]
    pub arg_files: Vec<String>,
}

impl VarArgs {
    pub(crate) fn parse(&self) -> Result<BTreeMap<String, String>> {
        devkit_common::args::parse(&self.args, &self.arg_files)
    }
}
