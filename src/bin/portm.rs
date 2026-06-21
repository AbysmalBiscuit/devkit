use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use devkit_ports::registry::{self, Data, Role};

#[derive(Parser)]
#[command(about = "Port registry for local dev servers")]
struct Cli {
    #[arg(short = 'C', long = "dir")]
    dir: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    Status,
    Alloc {
        #[arg(long)]
        holder: String,
        #[arg(long, value_enum, default_value = "issue")]
        role: Role,
        apps: Vec<String>,
    },
    Release {
        #[arg(long)]
        holder: String,
        #[arg(long, value_enum)]
        role: Option<Role>,
    },
    Prune,
    /// Print a shell-completion script (bash, zsh, fish, …) to stdout.
    Completions {
        shell: Shell,
    },
}

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("portm");
    devkit_common::paths::migrate_legacy_state();
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Status) {
        Cmd::Completions { shell } => {
            clap_complete::generate(
                shell,
                &mut Cli::command(),
                "portm",
                &mut std::io::stdout(),
            );
        }
        Cmd::Status => status()?,
        Cmd::Prune => {
            let freed = registry::prune()?;
            println!("pruned: {freed:?}");
        }
        Cmd::Release { holder, role } => {
            let freed = registry::release(&holder, role)?;
            println!("released: {freed:?}");
        }
        Cmd::Alloc { holder, role, apps } => {
            let start = cli.dir.clone().unwrap_or_else(|| ".".into());
            let loaded = devkit_ports::load::load(None, std::path::Path::new(&start))?;
            let mut reqs = Vec::with_capacity(apps.len());
            for app in &apps {
                let base = loaded
                    .catalog
                    .get(app)
                    .ok_or_else(|| anyhow::anyhow!("unknown app `{app}`"))?
                    .base_port;
                reqs.push((app.clone(), base));
            }
            for (app, port) in registry::alloc(&holder, &reqs, role)? {
                println!("{app}={port}");
            }
        }
    }
    Ok(())
}

fn status() -> Result<()> {
    let data: Data = registry::snapshot()?;
    println!("{}", registry::status_table(&data, None));
    Ok(())
}
