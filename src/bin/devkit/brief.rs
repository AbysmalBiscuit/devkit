//! `devkit brief` — a compact project orientation for coding-agent session
//! hooks. Prints the configured apps, canned tasks, and any live servers for
//! the current worktree when the working directory belongs to a
//! devkit-managed project, plus a library-versions table for any registered
//! library this checkout evidences — the two sections are independent, so a
//! docs-only checkout with no devrun setup still gets the latter. Prints
//! nothing when neither applies, so a SessionStart hook can call it
//! unconditionally from any repository.

use anyhow::Result;
use devkit_ports::apps::App;
use devkit_ports::config::BriefConfig;
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

/// The `[brief]` settings for `cwd`, defaulting to on. `config::resolve` and
/// not `load::load`: `load` also reads doppler.yaml and builds the app
/// catalog, which is what fails on a docs-only project. An unreadable config
/// falls open to the defaults.
fn brief_config(cwd: &Path) -> BriefConfig {
    config::resolve(None, cwd)
        .map(|(cfg, _)| cfg.brief)
        .unwrap_or_default()
}

fn render(cwd: &Path) -> Option<String> {
    let settings = brief_config(cwd);
    if !settings.enabled {
        return None;
    }
    let cwd_str = cwd.to_str()?;
    let root = devkit_common::cmd::git(&["rev-parse", "--show-toplevel"], cwd_str)
        .ok()?
        .trim()
        .to_string();

    // Pins are computed before `load`: a devkit.toml carrying [docs] and
    // nothing devrun can use must still produce a brief.
    let pins = settings.pins.then(|| pins_section(cwd)).flatten();
    let devrun = devrun_sections(&root, cwd);
    if pins.is_none() && devrun.is_none() {
        return None;
    }

    let mut out = String::new();
    out.push_str("## devkit project context\n\n");
    out.push_str(&wrap(&format!(
        "This checkout ({root}) is a devkit-managed project: dev servers, ports, \
         canned tasks, and cross-session file locks are coordinated by the devkit \
         CLIs. Load the `using-devkit` skill before using them."
    )));
    out.push_str("\n\n");
    if let Some((apps, tasks, servers)) = devrun {
        out.push_str(&devrun_text(&apps, &tasks, servers.as_deref()));
    }
    if let Some(section) = pins {
        out.push_str(&section);
    }
    Some(out)
}

/// The library-versions section, or `None` when the manifest cannot be read or
/// this checkout evidences nothing. A broken `docs.toml` omits this section; it
/// never suppresses the rest. An empty relevant set is what keeps the section
/// out of unrelated repositories — the machine-wide catalog accumulates every
/// library ever asked about, and a checkout that evidences none of them is not
/// a project this section has anything to say about.
fn pins_section(cwd: &Path) -> Option<String> {
    let pins = devkit_docs::pins::pins(cwd, None).ok()?;
    let (relevant, _) = devkit_docs::pins::relevant(&pins);
    if relevant.is_empty() {
        return None;
    }
    Some(pins_text(&devkit_docs::pins::render(&pins)))
}

/// The caveat carried once, at O(1) rather than per row.
fn pins_text(table: &str) -> String {
    let mut out = String::from("\n### Library versions in this checkout\n\n");
    out.push_str(table);
    out.push('\n');
    out.push_str(&wrap(
        "These are the versions this checkout's manifests and lockfiles name. \
         `docm info <lib>` resolves the matching source and reports the version it \
         actually serves. Answer questions about these libraries from those \
         checkouts; training-set recall is a different version.",
    ));
    out.push('\n');
    out
}

/// Greedy word-wrap to the terminal width: never splits a word, so a single
/// token longer than the width still overflows its line. Prose in this file
/// embeds a run-time path of unbounded length, so a fixed-width literal
/// cannot stand in for wrapping.
fn wrap(text: &str) -> String {
    let width = devkit_common::ui::term_width();
    let mut out = String::new();
    let mut line_len = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if line_len == 0 {
            out.push_str(word);
            line_len = word_len;
        } else if line_len + 1 + word_len <= width {
            out.push(' ');
            out.push_str(word);
            line_len += 1 + word_len;
        } else {
            out.push('\n');
            out.push_str(word);
            line_len = word_len;
        }
    }
    out
}

/// Apps, tasks and live servers, or `None` when this checkout is not a
/// devrun-configured project.
fn devrun_sections(root: &str, cwd: &Path) -> Option<(String, String, Option<String>)> {
    let loaded = load::load(None, cwd).ok()?;
    let home = config::home_config_path();
    if !is_project_member(
        root,
        &loaded.provenance.layers,
        home.as_deref(),
        &loaded.catalog,
    ) {
        return None;
    }
    Some((
        apps_line(&loaded.catalog),
        task::tasks_text(&task::list(&loaded.config)),
        live_servers(root),
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

fn devrun_text(apps: &str, tasks: &str, servers: Option<&str>) -> String {
    let mut out = String::new();
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
    fn a_malformed_config_falls_back_to_the_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("devkit.toml"), "this is not toml [[[").unwrap();
        let cfg = brief_config(tmp.path());
        assert!(
            cfg.enabled,
            "an unreadable config costs a brief, never withholds one"
        );
        assert!(cfg.pins);
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
        let text = devrun_text("api (apps/api)", "NAME KIND\n", None);
        assert!(text.contains("api (apps/api)"), "{text}");
        assert!(text.contains("devrun task"), "{text}");
        assert!(!text.contains("Live servers"), "{text}");

        let with = devrun_text("api (apps/api)", "NAME KIND\n", Some("PORT APP\n"));
        assert!(with.contains("Live servers in this worktree"), "{with}");
        assert!(with.contains("PORT APP"), "{with}");
    }
}
