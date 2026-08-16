use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

mod auth;
mod brief;
mod doctor;
mod schema;

#[derive(Parser)]
#[command(
    name = "devkit",
    version,
    about = "Configure and diagnose the devkit toolkit"
)]
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
    /// Print a compact project brief (apps, tasks, live servers, library
    /// versions) for the current checkout; silent outside a devkit-managed
    /// project. Intended for coding-agent session hooks.
    Brief {
        /// Emit only the library-versions section — what a post-compaction
        /// re-injection wants, without respending the context compaction
        /// just reclaimed.
        #[arg(long)]
        pins_only: bool,
        /// Print nothing when this session already received the same brief.
        /// Reads `session_id` from the hook's stdin JSON.
        ///
        /// Rejected with `--pins-only`: the watermark records the whole brief,
        /// so suppressing on it after emitting only the library table would
        /// tell the session it had seen a brief it never got.
        #[arg(long, conflicts_with = "pins_only")]
        if_changed: bool,
    },
    /// Check configured credentials and report what is missing.
    Doctor {
        /// Emit the report as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Print the JSON Schema for `devkit.toml` to stdout. Editors that speak
    /// the TOML language server use it for completion and validation; see
    /// `docs/configuration.md`.
    Schema,
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
        Cmd::Brief {
            pins_only,
            if_changed,
        } => brief::run(pins_only, if_changed),
        Cmd::Schema => schema::run(),
        Cmd::Doctor { json } => doctor::run(json),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "devkit", &mut std::io::stdout());
            Ok(())
        }
    }
}
