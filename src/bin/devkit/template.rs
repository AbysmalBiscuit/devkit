use std::{collections::BTreeMap, io::Write, path::Path};

use anyhow::Result;
use clap::Subcommand;
use devkit_ports::{load, task, templates};

/// `--arg` and `--arg-file`, shared by every command that renders templates.
#[derive(clap::Args, Debug, Default, PartialEq)]
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

#[derive(clap::Args)]
pub struct TemplateCli {
    #[command(subcommand)]
    cmd: TemplateCmd,
    /// Run as if this command had started in DIR instead of the current
    /// directory.
    #[arg(short = 'C', long = "dir", global = true)]
    dir: Option<String>,
    /// devkit.toml to load instead of the one discovered from the start
    /// directory.
    #[arg(long, global = true)]
    config: Option<String>,
}

#[derive(Subcommand)]
enum TemplateCmd {
    /// List the custom and built-in templates, with the args each reads.
    List {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print a template's source and the args it reads.
    ///
    /// Each arg lists who must pass it, its default, and its description.
    Show {
        /// Template to show.
        name: String,
        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Render a template and print exactly the result.
    ///
    /// Every template sees `prefix`, `branch`, and `issue`, `slug`, `apps`
    /// from `.devkit/issue.toml`; every other name it reads, `input` or
    /// `pr_url` say, is an `--arg`. No length limit such as `branch_max` is
    /// applied.
    ///
    /// `commit_message` takes the message's parts as `devkit commit` does,
    /// by `--subject`, `--body` and `--coauthor`, and refuses them as
    /// `--arg`. Every other template refuses those three flags.
    Render {
        /// Template to render.
        name: String,
        #[command(flatten)]
        message: crate::commit::MessageArgs,
        #[command(flatten)]
        vars: VarArgs,
        /// Emit `{"text": ...}` instead of the bare text.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(cli: TemplateCli) -> Result<()> {
    let start = cli.dir.as_deref().unwrap_or(".");
    let loaded = load::load(cli.config.as_deref().map(Path::new), Path::new(start))?;
    let cfg = &loaded.config;
    let caller = devkit_common::caller::caller();
    match cli.cmd {
        TemplateCmd::List { json } => {
            let rows = templates::list(cfg, Path::new(start), caller)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                let mut t = devkit_common::ui::table(&["NAME", "KIND", "ARGS", "DESCRIPTION"]);
                for r in &rows {
                    t.add_row(vec![
                        r.name.clone(),
                        r.kind.label().to_string(),
                        task::args_text(&r.args),
                        r.description.clone(),
                    ]);
                }
                println!("{t}");
            }
        }
        TemplateCmd::Show { name, json } => {
            let t = templates::show(cfg, Path::new(start), &name, caller)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&t)?);
            } else {
                print!("{}", t.source);
                if !t.source.ends_with('\n') {
                    println!();
                }
                println!();
                if t.args.is_empty() {
                    println!("no args");
                } else {
                    print!("{}", crate::config::args_table(&t.args));
                }
            }
        }
        TemplateCmd::Render {
            name,
            message,
            vars,
            json,
        } => {
            let text = templates::render(
                cfg,
                Path::new(start),
                &name,
                (&message.parts(), templates::PartNames::Flags),
                &vars.parse()?,
                caller,
            )?;
            if json {
                println!("{}", serde_json::json!({ "text": text }));
            } else {
                let mut out = std::io::stdout().lock();
                out.write_all(text.as_bytes())?;
                out.flush()?;
            }
        }
    }
    Ok(())
}
