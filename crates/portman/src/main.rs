use anyhow::Result;
use clap::{Parser, Subcommand};
use devkit_common::ui;
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
    Alloc { #[arg(long)] holder: String, #[arg(long, value_enum, default_value = "issue")] role: Role, apps: Vec<String> },
    Release { #[arg(long)] holder: String, #[arg(long, value_enum)] role: Option<Role> },
    Prune,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Status) {
        Cmd::Status => status()?,
        Cmd::Prune => { let freed = registry::prune()?; println!("pruned: {freed:?}"); }
        Cmd::Release { holder, role } => {
            let freed = registry::release(&holder, role)?;
            println!("released: {freed:?}");
        }
        Cmd::Alloc { holder, role, apps } => {
            let start = cli.dir.clone().unwrap_or_else(|| ".".into());
            let loaded = devkit_ports::load::load(None, std::path::Path::new(&start))?;
            let mut reqs = Vec::with_capacity(apps.len());
            for app in &apps {
                let base = loaded.catalog.get(app)
                    .ok_or_else(|| anyhow::anyhow!("unknown app `{app}`"))?.base_port;
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
    let mut t = ui::table(&["PORT", "APP", "ROLE", "HOLDER", "PID", "LISTENING", "AGE"]);
    for (port, e) in &data.entries {
        let id = holder_label(&e.holder);
        t.add_row(vec![
            port.to_string(), e.app.clone(),
            e.role.to_string(), id,
            e.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            if registry::listening(*port) { "yes".into() } else { "no".into() },
            format!("{}s", registry::now().saturating_sub(e.ts)),
        ]);
    }
    println!("{t}");
    Ok(())
}

fn holder_label(h: &str) -> String {
    std::path::Path::new(h).file_name().and_then(|s| s.to_str()).unwrap_or(h).to_string()
}
