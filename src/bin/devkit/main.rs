use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

mod auth;
mod doctor;

#[derive(Parser)]
#[command(name = "devkit", about = "Configure and diagnose the devkit toolkit")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate and store a Linear or Slack credential.
    Auth {
        provider: Provider,
        /// Provide the token non-interactively instead of being prompted.
        #[arg(long)]
        token: Option<String>,
    },
    /// Check configured credentials and report what is missing.
    Doctor {
        /// Emit the report as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions { shell: Shell },
}

#[derive(Clone, Copy, ValueEnum)]
enum Provider {
    Linear,
    Slack,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Provider::Linear => "Linear",
            Provider::Slack => "Slack",
        }
    }
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkit");
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Auth { provider, token } => auth::run(provider, token),
        Cmd::Doctor { json } => doctor::run(json),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "devkit", &mut std::io::stdout());
            Ok(())
        }
    }
}
