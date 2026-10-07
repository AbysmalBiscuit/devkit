//! `devkit commit`: a selection committed with the message the
//! `commit_message` template renders, leaving the working tree and everyone
//! else's staging alone.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::ArgGroup;
use devkit_common::vcs::{Selection, Vcs, VersionControl};
use devkit_config::{Config, NoConfig};
use devkit_ports::templates::{self, CommitMessage};

use crate::template::VarArgs;

/// The parts of the message the `commit_message` template renders, taken by
/// `devkit commit` and `devkit template render`.
#[derive(clap::Args, Debug, Default)]
pub(crate) struct MessageArgs {
    /// The message's subject line, e.g. `fix(scope): imperative summary`.
    #[arg(long)]
    subject: Option<String>,
    /// Why the change is needed, when the subject does not say.
    #[arg(long)]
    body: Option<String>,
    /// A co-author, `Name <email>`, for a `Co-authored-by` trailer.
    /// Repeatable.
    #[arg(long = "coauthor", value_name = "NAME <EMAIL>")]
    coauthors: Vec<String>,
}

impl MessageArgs {
    pub(crate) fn parts(&self) -> CommitMessage<'_> {
        CommitMessage {
            subject: self.subject.as_deref(),
            body: self.body.as_deref(),
            coauthors: &self.coauthors,
        }
    }
}

#[derive(clap::Args)]
#[command(group(ArgGroup::new("selection").required(true).args(["files", "patch", "amend"])))]
#[command(mut_arg("subject", |a| a.required(true)))]
pub struct CommitCli {
    /// Commit these paths as the working tree has them, new and deleted
    /// files included. Changes staged on other paths stay staged.
    #[arg(long, num_args = 1.., value_name = "PATH")]
    files: Vec<PathBuf>,
    /// Commit the hunks of this patch, made against HEAD, whatever the
    /// working tree holds. Staged changes stay staged; a patch overlapping
    /// them is refused with HEAD and the index unchanged.
    #[arg(long, value_name = "FILE")]
    patch: Option<PathBuf>,
    /// Give the last commit a new message, leaving staged changes staged.
    #[arg(long)]
    amend: bool,
    #[command(flatten)]
    message: MessageArgs,
    #[command(flatten)]
    vars: VarArgs,
    /// Run as if this command had started in DIR instead of the current
    /// directory. Relative paths are taken from it.
    #[arg(short = 'C', long = "dir")]
    dir: Option<PathBuf>,
    /// devkit.toml to load instead of the one discovered from the start
    /// directory.
    #[arg(long)]
    config: Option<PathBuf>,
}

pub fn run(cli: CommitCli) -> Result<()> {
    let start = cli.dir.unwrap_or_else(|| PathBuf::from("."));
    // The built-in template needs no config, so a repository without one
    // commits with it.
    let config = match devkit_common::config::resolve(cli.config.as_deref(), &start) {
        Ok((config, _)) => config,
        Err(e) if e.downcast_ref::<NoConfig>().is_some() => Config::default(),
        Err(e) => return Err(e),
    };
    let message = templates::commit_message(
        &config,
        &start,
        &cli.message.parts(),
        &cli.vars.parse()?,
        devkit_common::caller::caller(),
    )?;
    // git applies the patch from the repository's top, not from here.
    let patch = cli
        .patch
        .map(|p| std::path::absolute(start.join(p)))
        .transpose()?;
    let selection = match (&patch, cli.amend) {
        (Some(patch), _) => Selection::Patch(patch),
        (None, true) => Selection::Amend,
        (None, false) => Selection::Paths(&cli.files),
    };
    print!("{}", Vcs::at(&start).commit(&start, &selection, &message)?);
    Ok(())
}

/// The hidden verb a commit hook wrapper runs; exits with the hook's status.
pub fn run_hook(state: &Path, hook: &str, args: &[String]) -> Result<()> {
    let code = devkit_common::vcs::run_commit_hook(state, hook, args).unwrap_or_else(|e| {
        eprintln!("devkit commit: {e:#}");
        1
    });
    std::process::exit(code)
}
