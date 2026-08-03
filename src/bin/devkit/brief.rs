//! `devkit brief` — a compact project orientation for coding-agent session
//! hooks. Prints the configured apps, canned tasks, and any live servers for
//! the current worktree when the working directory belongs to a
//! devkit-managed project, and nothing at all otherwise — so a SessionStart
//! hook can call it unconditionally from any repository.

use anyhow::Result;
use devkit_ports::apps::App;
use devkit_ports::{config, load, registry, task};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;
    // A brief is context injection, never a gate: any failure (no git, no
    // config, unreadable registry) means no output, exit 0.
    if let Some(text) = render(&cwd) {
        print!("{text}");
    }
    Ok(())
}

fn render(cwd: &Path) -> Option<String> {
    let cwd_str = cwd.to_str()?;
    let root = devkit_common::cmd::git(&["rev-parse", "--show-toplevel"], cwd_str)
        .ok()?
        .trim()
        .to_string();
    let loaded = load::load(None, cwd).ok()?;
    let home = config::home_config_path();
    if !is_project_member(
        &root,
        &loaded.provenance.layers,
        home.as_deref(),
        &loaded.catalog,
    ) {
        return None;
    }
    let tasks = task::tasks_text(&task::list(&loaded.config));
    let servers = live_servers(&root);
    Some(render_text(
        &root,
        &apps_line(&loaded.catalog),
        &tasks,
        servers.as_deref(),
    ))
}

/// Whether the checkout at `root` is part of the configured project: either a
/// devkit.toml layer was found walking up from the cwd (any non-home layer),
/// or at least one configured app's directory exists under the worktree root.
/// The personal config (`~/.config/devkit/config.toml`) resolves from
/// anywhere, so on its own it never makes an unrelated repository a member.
fn is_project_member(
    root: &str,
    layers: &[PathBuf],
    home: Option<&Path>,
    catalog: &HashMap<String, App>,
) -> bool {
    layers.iter().any(|l| Some(l.as_path()) != home)
        || catalog.values().any(|a| {
            // An app rooted at "." (utility apps) exists under every
            // directory and proves nothing; only probe paths with a real
            // component.
            let p = Path::new(&a.path);
            p.components()
                .any(|c| matches!(c, std::path::Component::Normal(_)))
                && Path::new(root).join(p).is_dir()
        })
}

fn apps_line(catalog: &HashMap<String, App>) -> String {
    if catalog.is_empty() {
        return "none resolved".into();
    }
    let mut apps: Vec<&App> = catalog.values().collect();
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    apps.iter()
        .map(|a| format!("{} ({})", a.name, a.path))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The port-registry rows held by this worktree, or `None` when it holds
/// nothing (the section is omitted rather than rendered empty).
fn live_servers(root: &str) -> Option<String> {
    let data = registry::snapshot().ok()?;
    data.entries
        .values()
        .any(|e| e.holder == root)
        .then(|| registry::status_table(&data, Some(root)))
}

fn render_text(root: &str, apps: &str, tasks: &str, servers: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("## devkit project context\n\n");
    out.push_str(&format!(
        "This checkout ({root}) is a devkit-managed project: dev servers, ports, \
         canned tasks, and cross-session file locks are coordinated by the devkit \
         CLIs. Load the `using-devkit` skill before using them.\n\n"
    ));
    out.push_str(
        "- `devrun up <app>` / `devrun down` — start/stop supervised dev servers for this worktree\n",
    );
    out.push_str("- `devrun task <name> [--dry-run]` — run a canned project task (table below)\n");
    out.push_str("- `portm status` — port registry; `lockm status` — advisory file locks\n\n");
    out.push_str(&format!("Apps (`devrun up`): {apps}\n\n"));
    out.push_str("### Tasks (`devrun task <name>`)\n\n");
    out.push_str(tasks);
    if let Some(s) = servers {
        out.push_str("\n### Live servers in this worktree\n\n");
        out.push_str(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, path: &str) -> App {
        App {
            name: name.into(),
            base_port: 9100,
            path: path.into(),
            launch: vec![],
            url: None,
            url_env: None,
            provides_url: false,
            static_env: HashMap::new(),
            prep_files: vec![],
            setup: vec![],
        }
    }

    #[test]
    fn membership_requires_project_layer_or_present_app_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let home = PathBuf::from("/home/x/.config/devkit/config.toml");
        let mut catalog = HashMap::new();
        catalog.insert("api".to_string(), app("api", "apps/api"));

        // Only the home layer, app dir absent → not a member.
        let layers = vec![home.clone()];
        assert!(!is_project_member(root, &layers, Some(&home), &catalog));

        // An app rooted at "." exists under every directory; it must not
        // make an unrelated checkout a member.
        catalog.insert("chrome".to_string(), app("chrome", "."));
        assert!(!is_project_member(root, &layers, Some(&home), &catalog));

        // A project devkit.toml layer → member even without app dirs.
        let project = vec![home.clone(), tmp.path().join("devkit.toml")];
        assert!(is_project_member(root, &project, Some(&home), &catalog));

        // App dir exists under root → member from the home layer alone.
        std::fs::create_dir_all(tmp.path().join("apps/api")).unwrap();
        assert!(is_project_member(root, &layers, Some(&home), &catalog));
    }

    #[test]
    fn render_text_sections_and_optional_servers() {
        let text = render_text("/w/root", "api (apps/api)", "NAME KIND\n", None);
        assert!(text.contains("devkit project context"), "{text}");
        assert!(text.contains("using-devkit"), "{text}");
        assert!(text.contains("api (apps/api)"), "{text}");
        assert!(text.contains("devrun task"), "{text}");
        assert!(!text.contains("Live servers"), "{text}");

        let with = render_text(
            "/w/root",
            "api (apps/api)",
            "NAME KIND\n",
            Some("PORT APP\n"),
        );
        assert!(with.contains("Live servers in this worktree"), "{with}");
        assert!(with.contains("PORT APP"), "{with}");
    }
}
