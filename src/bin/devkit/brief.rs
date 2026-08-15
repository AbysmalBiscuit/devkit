//! `devkit brief` — a compact project orientation for coding-agent session
//! hooks. Prints the configured apps, canned tasks, and any live servers for
//! the current worktree when the working directory belongs to a
//! devkit-managed project, plus a library-versions table for any registered
//! library this checkout evidences — the two sections are independent, so a
//! docs-only checkout with no devrun setup still gets the latter. Prints
//! nothing when neither applies, so a SessionStart hook can call it
//! unconditionally from any repository.
//!
//! Two narrower emission modes let other hook events call it without spamming
//! the session: `--pins-only` emits the library table alone, and `--if-changed`
//! emits only when this checkout's state differs from what the session was last
//! told, tracked by a per-session watermark over a structured snapshot. A full
//! brief stamps that watermark itself, so the session that received one is not
//! handed the same thing again by the next `--if-changed` call.

use anyhow::Result;
use devkit_ports::apps::App;
use devkit_ports::config::BriefConfig;
use devkit_ports::{config, load, registry, task};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

pub fn run(pins_only: bool, if_changed: bool) -> Result<()> {
    // A brief is context injection, never a gate: any failure (no cwd, no git,
    // no config, unreadable registry) means no output, exit 0.
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    let settings = brief_config(&cwd);
    if !settings.enabled {
        return Ok(());
    }

    if !if_changed {
        if pins_only {
            // No stamp: this carries neither the apps, tasks nor server
            // sections, so recording it as delivered would suppress a full
            // brief the session never saw.
            if let Some(text) = pins_only_text(&cwd, &settings) {
                print!("{text}");
            }
            return Ok(());
        }
        if let Some(text) = render(&cwd, &settings) {
            print!("{text}");
            stamp(&cwd, &settings);
        }
        return Ok(());
    }

    let session = session_id();
    let digest = snapshot(&cwd, &settings).map(|s| s.digest());
    let Some(session) = session else {
        // No id means emit without persisting: a shared per-cwd key would let
        // one session's brief suppress another's re-injection, and a withheld
        // brief is the worse failure.
        if let Some(text) = render(&cwd, &settings) {
            print!("{text}");
        }
        return Ok(());
    };

    let path = watermark_path(&session);
    let previous = std::fs::read_to_string(&path).ok();
    let current = digest.map(|d| format!("{d:016x}"));
    if previous.is_some() && previous.as_deref() == current.as_deref() {
        return Ok(());
    }
    if let Some(current) = &current {
        write_watermark(&path, current);
    }
    match render(&cwd, &settings) {
        Some(text) => print!("{text}"),
        // Left the project: silence would leave the previous checkout's brief
        // as the most recent thing the agent was told.
        None if previous.is_some() => {
            let _ = std::fs::remove_file(&path);
            println!(
                "## devkit project context\n\nThis directory is not a devkit-managed project; the earlier project brief no longer applies."
            );
        }
        None => {}
    }
    Ok(())
}

/// The canonical form `--if-changed` hashes: every section's identity, no
/// rendering and no clock. Hashing rendered text would make the watermark
/// terminal-width sensitive; hashing only the pins would suppress a brief
/// whose apps, tasks or servers changed while the pins held still.
struct BriefSnapshot {
    root: String,
    apps: Vec<String>,
    tasks: Vec<(String, String, String, String)>,
    servers: Vec<ServerKey>,
    pins: Vec<PinKey>,
}

/// Identity plus the probed listening state. `AGE` is excluded: it is computed
/// against `now()`, so including it makes the digest change every second.
struct ServerKey {
    port: u16,
    app: String,
    role: String,
    pid: String,
    listening: bool,
}

struct PinKey {
    name: String,
    project_scoped: bool,
    declared: &'static str,
    outcome: String,
}

impl BriefSnapshot {
    /// A stable byte string, so the digest does not depend on struct layout.
    fn canonical(&self) -> String {
        let mut out = format!("root\t{}\n", self.root);
        for app in &self.apps {
            out.push_str(&format!("app\t{app}\n"));
        }
        for (name, kind, app, description) in &self.tasks {
            out.push_str(&format!("task\t{name}\t{kind}\t{app}\t{description}\n"));
        }
        for s in &self.servers {
            out.push_str(&format!(
                "server\t{}\t{}\t{}\t{}\t{}\n",
                s.port, s.app, s.role, s.pid, s.listening
            ));
        }
        for p in &self.pins {
            out.push_str(&format!(
                "pin\t{}\t{}\t{}\t{}\n",
                p.name, p.project_scoped, p.declared, p.outcome
            ));
        }
        out
    }

    fn digest(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.canonical().hash(&mut hasher);
        hasher.finish()
    }
}

impl PinKey {
    fn of(pin: &devkit_docs::pins::Pin) -> Self {
        use devkit_docs::pins::Outcome;
        PinKey {
            name: pin.name.clone(),
            project_scoped: pin.project_scoped,
            declared: pin.declared.as_str(),
            outcome: match &pin.outcome {
                Outcome::Version {
                    version,
                    workspace,
                    lockfile,
                } => format!("version:{version}:{lockfile}:{}", workspace.display()),
                Outcome::Ref(git_ref) => format!("ref:{git_ref}"),
                Outcome::Unresolved(reason) => format!("unresolved:{reason}"),
                Outcome::Undeclared => "undeclared".to_string(),
            },
        }
    }
}

/// Record the full brief this session has just been told, so `--if-changed`
/// has something to compare against. Without a session id — an interactive run
/// — there is no session to record it for.
fn stamp(cwd: &Path, settings: &BriefConfig) {
    let Some(session) = session_id() else {
        return;
    };
    let Some(digest) = snapshot(cwd, settings).map(|s| s.digest()) else {
        return;
    };
    write_watermark(&watermark_path(&session), &format!("{digest:016x}"));
}

/// Fails open: an unreadable or unwritable state directory reports "changed",
/// costing a duplicate brief rather than withholding one.
fn write_watermark(path: &Path, digest: &str) {
    let _ = std::fs::create_dir_all(path.parent().expect("watermark has a parent"));
    let _ = std::fs::write(path, digest);
}

/// The session id from the hook's stdin JSON. `None` when there is no stdin
/// to read (an interactive run) or no id in it.
fn session_id() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// The watermark file for `session`. The name is a hash of the complete raw
/// id: dropping disallowed characters is lossy, and two ids differing only in
/// what was dropped would collide onto one watermark.
fn watermark_path(session: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    session.hash(&mut hasher);
    devkit_common::paths::state_dir()
        .join("brief")
        .join(format!("{:016x}", hasher.finish()))
}

/// The same content `render` emits, in the canonical form the watermark
/// hashes. `None` when this checkout produces no brief at all — the emptiness
/// rule has to match `render`'s exactly, or a digest saying "changed" for a
/// brief `render` refuses to emit would rewrite the watermark and stay silent.
fn snapshot(cwd: &Path, settings: &BriefConfig) -> Option<BriefSnapshot> {
    let cwd_str = cwd.to_str()?;
    let root = devkit_common::cmd::git(&["rev-parse", "--show-toplevel"], cwd_str)
        .ok()?
        .trim()
        .to_string();

    let pins = checkout_pins(cwd, settings);
    let (relevant, _) = devkit_docs::pins::relevant(&pins);
    let pin_keys: Vec<PinKey> = relevant.iter().map(|pin| PinKey::of(pin)).collect();
    let devrun = devrun_project(&root, cwd);
    if pin_keys.is_empty() && devrun.is_none() {
        return None;
    }

    let (apps, tasks) = match &devrun {
        Some(loaded) => {
            let mut apps: Vec<String> = loaded
                .catalog
                .values()
                .map(|a| format!("{} ({})", a.name, a.path))
                .collect();
            apps.sort();
            let mut tasks: Vec<(String, String, String, String)> = task::list(&loaded.config)
                .into_iter()
                .map(|r| (r.name, r.kind.to_string(), r.app, r.description))
                .collect();
            tasks.sort();
            (apps, tasks)
        }
        None => (Vec::new(), Vec::new()),
    };

    // One probe, two consumers: `status_table` probes liveness itself, so
    // hashing here and rendering there would take two probes, and a server
    // going down between them would make the watermark certify text nobody
    // was shown.
    let mut servers = Vec::new();
    if let Ok(data) = registry::snapshot() {
        let view = registry::listening_view(&data, Some(&root));
        for (port, entry) in &data.entries {
            if entry.holder != root {
                continue;
            }
            servers.push(ServerKey {
                port: *port,
                app: entry.app.clone(),
                role: entry.role.to_string(),
                pid: entry
                    .pid
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "-".into()),
                listening: *view.get(port).unwrap_or(&false),
            });
        }
    }
    servers.sort_by_key(|s| s.port);

    Some(BriefSnapshot {
        root,
        apps,
        tasks,
        servers,
        pins: pin_keys,
    })
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

fn render(cwd: &Path, settings: &BriefConfig) -> Option<String> {
    let cwd_str = cwd.to_str()?;
    let root = devkit_common::cmd::git(&["rev-parse", "--show-toplevel"], cwd_str)
        .ok()?
        .trim()
        .to_string();

    // Pins are computed before `load`: a devkit.toml carrying [docs] and
    // nothing devrun can use must still produce a brief.
    let pins = pins_section(&checkout_pins(cwd, settings));
    let devrun = devrun_sections(&root, cwd);
    if pins.is_none() && devrun.is_none() {
        return None;
    }

    let mut out = String::new();
    out.push_str("## devkit project context\n\n");
    // The devrun claim ("dev servers, ports... are coordinated by the devkit
    // CLIs") is only true when `devrun` resolved; a pins-only checkout has no
    // devrun setup at all, so asserting it here would be a false claim
    // injected into an agent's context.
    let intro = if devrun.is_some() {
        format!(
            "This checkout ({root}) is a devkit-managed project: dev servers, ports, \
             canned tasks, and cross-session file locks are coordinated by the devkit \
             CLIs. Load the `using-devkit` skill before using them."
        )
    } else {
        "This checkout has libraries registered with devkit; the table below is \
         what its lockfiles pin."
            .to_string()
    };
    out.push_str(&wrap(&intro));
    out.push_str("\n\n");
    if let Some((apps, tasks, servers)) = devrun {
        out.push_str(&devrun_text(&apps, &tasks, servers.as_deref()));
    }
    if let Some(section) = pins {
        out.push_str(&section);
    }
    Some(out)
}

/// Every registered library's pin for this checkout, empty when the `[brief]`
/// gate is off or the manifest cannot be read. The single source both the
/// rendered section and the hashed snapshot read, so "this checkout evidences
/// nothing" means exactly the same thing to each of them.
fn checkout_pins(cwd: &Path, settings: &BriefConfig) -> Vec<devkit_docs::pins::Pin> {
    settings
        .pins
        .then(|| devkit_docs::pins::pins(cwd, None).ok())
        .flatten()
        .unwrap_or_default()
}

/// The library-versions section, or `None` when the manifest cannot be read or
/// this checkout evidences nothing. A broken `docs.toml` omits this section; it
/// never suppresses the rest. An empty relevant set is what keeps the section
/// out of unrelated repositories — the machine-wide catalog accumulates every
/// library ever asked about, and a checkout that evidences none of them is not
/// a project this section has anything to say about.
fn pins_section(pins: &[devkit_docs::pins::Pin]) -> Option<String> {
    let (relevant, _) = devkit_docs::pins::relevant(pins);
    (!relevant.is_empty()).then(|| pins_text(&devkit_docs::pins::render(pins)))
}

/// Just the library-versions section, gated the same way the full brief's is.
fn pins_only_text(cwd: &Path, settings: &BriefConfig) -> Option<String> {
    pins_section(&checkout_pins(cwd, settings))
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
///
/// Takes a single paragraph: `split_whitespace` treats `\n` as ordinary
/// whitespace, so any line breaks already in `text` are not preserved —
/// multi-line input is rejoined and re-wrapped as one paragraph.
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

/// The devrun project this checkout belongs to, or `None` when it belongs to
/// none. The single place that decision is made, so the rendered brief and the
/// hashed snapshot never disagree about whether a devrun section exists.
fn devrun_project(root: &str, cwd: &Path) -> Option<load::Loaded> {
    let loaded = load::load(None, cwd).ok()?;
    let home = config::home_config_path();
    is_project_member(
        root,
        &loaded.provenance.layers,
        home.as_deref(),
        &loaded.catalog,
    )
    .then_some(loaded)
}

/// Apps, tasks and live servers, or `None` when this checkout is not a
/// devrun-configured project.
fn devrun_sections(root: &str, cwd: &Path) -> Option<(String, String, Option<String>)> {
    let loaded = devrun_project(root, cwd)?;
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
    let view = registry::listening_view(&data, Some(root));
    data.entries
        .values()
        .any(|e| e.holder == root)
        .then(|| registry::status_table_with(&data, Some(root), &view))
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
