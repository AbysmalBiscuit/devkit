//! `devkit brief` — a compact project orientation for coding-agent session
//! hooks. Prints the configured apps, canned tasks, and any live servers for
//! the current worktree when the working directory belongs to a
//! devkit-managed project, plus a library-versions table for any registered
//! library this checkout evidences — the two sections are independent, so a
//! docs-only checkout with no devrun setup still gets the latter. Prints
//! nothing when neither applies, so a SessionStart hook can call it
//! unconditionally from any repository.
//!
//! Silence is for a checkout with nothing to say, never for one that cannot be
//! read. A `devkit.toml` that exists and fails to load is reported with its
//! cause chain: it makes every devkit command fail, and an empty brief would
//! instead teach the session that this is not a devkit project.
//!
//! Within the devrun half every section earns its place: a project with no
//! configured apps is not told about `devrun up` or `portm`, one with no
//! `[tasks]` table is not told about `devrun task`, and the intro names only
//! the facilities that survive. `[brief]` can suppress a section the checkout
//! does have — `apps`, `tasks` and `locks` — which reads downstream exactly as
//! an absent one, so the bullets introducing it go too. `locks` has no other
//! way to be decided: whether sessions share this checkout is not observable.
//!
//! Two narrower emission modes let other hook events call it without spamming
//! the session: `--pins-only` emits the library table alone, and `--if-changed`
//! emits only when this checkout's state differs from what the session was last
//! told, tracked by a per-session watermark over a structured snapshot. A full
//! brief stamps that watermark itself, so the session that received one is not
//! handed the same thing again by the next `--if-changed` call. `--pins-only`
//! carries neither the apps, tasks nor server sections, so it clears any
//! existing stamp instead of leaving it in place — otherwise a later
//! `--if-changed` would find the checkout's state unchanged since that stamp
//! and stay silent, leaving the rest of the brief permanently owed.
//!
//! `--additional-context` decides how the brief travels rather than what it
//! says: Claude Code injects a hook's plain stdout, while Codex and Cursor read
//! it out of a JSON field, so those two ask for the envelope and every emission
//! mode above is available under either.

use anyhow::Result;
use devkit_config as config;
use devkit_config::BriefConfig;
use devkit_ports::apps::App;
use devkit_ports::{load, registry, task};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

/// How the brief reaches the session. Claude Code injects a hook's plain
/// stdout; Codex and Cursor read it out of a JSON field instead, and spell that
/// field differently — Cursor `additional_context`, Codex
/// `hookSpecificOutput.additionalContext`, which rejects an object carrying any
/// other key, so one payload cannot serve both and the host has to be told
/// apart.
#[derive(Clone, Copy)]
enum Emit {
    Stdout,
    AdditionalContext,
}

impl Emit {
    fn text(self, text: &str) {
        // An empty envelope is not the same as no output: it would hand the
        // session a context block with nothing in it.
        if text.is_empty() {
            return;
        }
        match self {
            Emit::Stdout => print!("{text}"),
            Emit::AdditionalContext => println!("{}", envelope(text)),
        }
    }
}

/// `CURSOR_PROJECT_DIR` is the variable Cursor documents as passed to every
/// hook process; `CURSOR_PLUGIN_ROOT` is accepted alongside it but is not
/// documented anywhere.
fn envelope(text: &str) -> serde_json::Value {
    let cursor = ["CURSOR_PROJECT_DIR", "CURSOR_PLUGIN_ROOT"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
    if cursor {
        serde_json::json!({ "additional_context": text })
    } else {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": text,
            }
        })
    }
}

pub fn run(pins_only: bool, if_changed: bool, additional_context: bool) -> Result<()> {
    let out = if additional_context {
        Emit::AdditionalContext
    } else {
        Emit::Stdout
    };
    // A brief is context injection, never a gate: any failure (no cwd, no git,
    // no config, unreadable registry) means no output, exit 0.
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    // `run` resolves this once for the functions in this file, which take it
    // as a parameter. Resolvers below it do not share this value: `load::load`
    // and the `config::resolve` callers each ask git for their own main
    // checkout.
    let main_checkout = devkit_common::git::main_checkout(&cwd).ok().flatten();
    let settings = brief_config(&cwd, main_checkout.as_deref());
    if !settings.enabled {
        return Ok(());
    }

    if !if_changed {
        if pins_only {
            // This carries neither the apps, tasks nor server sections, so an
            // earlier full-brief stamp must not survive it: left in place, the
            // next `--if-changed` would compare against a watermark that
            // already matches the current state and stay silent, leaving the
            // devrun half of the brief permanently owed.
            if let Some(session) = session_id() {
                let _ = std::fs::remove_file(watermark_path(&session));
            }
            if let Some(text) = pins_only_text(&cwd, &settings) {
                out.text(&text);
            }
            return Ok(());
        }
        if let Some(text) = render(&cwd, &settings, main_checkout.as_deref()) {
            out.text(&text);
            stamp(&cwd, &settings, main_checkout.as_deref());
        }
        return Ok(());
    }

    let session = session_id();
    let digest = snapshot(&cwd, &settings, main_checkout.as_deref()).map(|s| s.digest());
    let Some(session) = session else {
        // No id means emit without persisting: a shared per-cwd key would let
        // one session's brief suppress another's re-injection, and a withheld
        // brief is the worse failure.
        if let Some(text) = render(&cwd, &settings, main_checkout.as_deref()) {
            out.text(&text);
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
    match render(&cwd, &settings, main_checkout.as_deref()) {
        Some(text) => out.text(&text),
        // Left the project: silence would leave the previous checkout's brief
        // as the most recent thing the agent was told.
        None if previous.is_some() => {
            let _ = std::fs::remove_file(&path);
            out.text(
                "## devkit project context\n\nThis directory is not a devkit-managed project; the earlier project brief no longer applies.\n",
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
    locks: bool,
    pins: Vec<PinKey>,
    /// Why the config does not load, so that fixing it is a change
    /// `--if-changed` can see — otherwise the session that was told about the
    /// fault would never be told it is over.
    config_fault: Option<String>,
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
    resolved: Option<String>,
}

/// The devrun half's content, each part absent when there is nothing to say.
struct DevrunBrief {
    apps: Option<String>,
    tasks: Option<String>,
    servers: Option<String>,
    locks: bool,
}

/// Which devrun facilities a checkout has anything to say about. `render` and
/// `snapshot` both decide the devrun half's existence through `any`: a
/// snapshot that reports content for a brief `render` refuses to emit would
/// stamp a watermark against text nobody was shown, and the session would
/// never be told again.
#[derive(Clone, Copy)]
struct Facilities {
    apps: bool,
    tasks: bool,
    servers: bool,
    locks: bool,
}

impl Facilities {
    /// A registry row is a port this worktree holds whether or not the catalog
    /// still names the app that bound it, so either one keeps `devrun down`
    /// and `portm status` relevant.
    fn ports(self) -> bool {
        self.apps || self.servers
    }

    fn any(self) -> bool {
        self.ports() || self.tasks || self.locks
    }
}

impl DevrunBrief {
    fn facilities(&self) -> Facilities {
        Facilities {
            apps: self.apps.is_some(),
            tasks: self.tasks.is_some(),
            servers: self.servers.is_some(),
            locks: self.locks,
        }
    }
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
        out.push_str(&format!("locks\t{}\n", self.locks));
        for p in &self.pins {
            out.push_str(&format!(
                "pin\t{}\t{}\t{}\t{}\t{}\n",
                p.name,
                p.project_scoped,
                p.declared,
                p.outcome,
                p.resolved.as_deref().unwrap_or("-")
            ));
        }
        if let Some(fault) = &self.config_fault {
            out.push_str(&format!("config-fault\t{fault}\n"));
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
                Outcome::Rollup { versions, lockfile } => {
                    let members: Vec<String> = versions
                        .iter()
                        .map(|(version, workspaces)| format!("{version}@{}", workspaces.join("+")))
                        .collect();
                    format!("rollup:{lockfile}:{}", members.join(","))
                }
                Outcome::Ref(git_ref) => format!("ref:{git_ref}"),
                Outcome::Unresolved(reason) => format!("unresolved:{reason}"),
                Outcome::Undeclared => "undeclared".to_string(),
            },
            resolved: pin.resolved.clone(),
        }
    }
}

/// Record the full brief this session has just been told, so `--if-changed`
/// has something to compare against. Without a session id — an interactive run
/// — there is no session to record it for.
fn stamp(cwd: &Path, settings: &BriefConfig, main_checkout: Option<&Path>) {
    let Some(session) = session_id() else {
        return;
    };
    let Some(digest) = snapshot(cwd, settings, main_checkout).map(|s| s.digest()) else {
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
fn snapshot(
    cwd: &Path,
    settings: &BriefConfig,
    main_checkout: Option<&Path>,
) -> Option<BriefSnapshot> {
    let root = devkit_common::git::checkout_root(cwd)
        .ok()?
        .to_string_lossy()
        .into_owned();

    let pins = checkout_pins(cwd, settings);
    let (relevant, _) = devkit_docs::pins::relevant(&pins);
    let pin_keys: Vec<PinKey> = relevant.iter().map(|pin| PinKey::of(pin)).collect();
    let devrun = devrun_project(&root, cwd);

    // A switched-off section hashes as absent, matching what `render` emits:
    // a digest that counted suppressed rows would report "changed" for a brief
    // whose visible text never moved.
    let (apps, tasks) = match &devrun {
        Some(loaded) => {
            let mut apps: Vec<String> = if settings.apps {
                loaded
                    .catalog
                    .values()
                    .map(|a| format!("{} ({})", a.name, a.path))
                    .collect()
            } else {
                Vec::new()
            };
            apps.sort();
            let mut tasks: Vec<(String, String, String, String)> = if settings.tasks {
                task::list(&loaded.config)
                    .into_iter()
                    .map(|r| (r.name, r.kind.to_string(), r.app, r.description))
                    .collect()
            } else {
                Vec::new()
            };
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
    if devrun.is_some()
        && let Ok(data) = registry::snapshot()
    {
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

    let locks = devrun.is_some() && settings.locks;
    let facilities = Facilities {
        apps: !apps.is_empty(),
        tasks: !tasks.is_empty(),
        servers: !servers.is_empty(),
        locks,
    };
    let config_fault = config_fault(cwd, main_checkout);
    if pin_keys.is_empty() && !facilities.any() && config_fault.is_none() {
        return None;
    }

    Some(BriefSnapshot {
        root,
        apps,
        tasks,
        servers,
        locks,
        pins: pin_keys,
        config_fault,
    })
}

/// The `[brief]` settings for `cwd`, defaulting to on. `config::resolve` and
/// not `load::load`: `load` also reads doppler.yaml and builds the app
/// catalog, which is what fails on a docs-only project. An unreadable config
/// falls open to the defaults.
fn brief_config(cwd: &Path, main_checkout: Option<&Path>) -> BriefConfig {
    let checkout_root = devkit_common::git::checkout_root(cwd).ok();
    config::resolve(None, cwd, main_checkout, checkout_root.as_deref())
        .map(|(cfg, _)| cfg.brief)
        .unwrap_or_default()
}

/// The reason this checkout's config does not load, or `None` when it loads or
/// does not exist. An absent config is how every non-devkit repository looks,
/// so only a config that exists and fails is worth a word.
fn config_fault(cwd: &Path, main_checkout: Option<&Path>) -> Option<String> {
    match config::health(cwd, main_checkout) {
        config::Health::Broken(why) => Some(why),
        config::Health::Ok | config::Health::Absent => None,
    }
}

/// The fault stated plainly, with the cause chain set off from the prose so an
/// agent quoting it back to the user does not fold it into a sentence. Every
/// devkit CLI fails on this config, so the brief says so rather than leaving
/// the agent to discover it one command at a time.
fn fault_text(why: &str) -> String {
    let mut out = wrap(
        "This checkout's devkit.toml does not load, so no project context follows. \
         Every devkit CLI fails the same way until it is fixed.",
    );
    out.push_str("\n\n");
    // A toml deserialization error carries its own line breaks (the key on one
    // line, the table on the next); indenting per line keeps the whole cause
    // inside the block rather than letting its tail escape to column zero.
    for line in why.lines() {
        out.push_str(&format!("    {}\n", line.trim_end()));
    }
    out.push('\n');
    out
}

fn render(cwd: &Path, settings: &BriefConfig, main_checkout: Option<&Path>) -> Option<String> {
    let root = devkit_common::git::checkout_root(cwd)
        .ok()?
        .to_string_lossy()
        .into_owned();

    // Pins are computed before `load`: a devkit.toml carrying [docs] and
    // nothing devrun can use must still produce a brief.
    let pins = pins_section(&checkout_pins(cwd, settings));
    let devrun = devrun_sections(&root, cwd, settings);
    let fault = config_fault(cwd, main_checkout);
    if pins.is_none() && devrun.is_none() && fault.is_none() {
        return None;
    }

    let mut out = String::new();
    out.push_str("## devkit project context\n\n");
    if let Some(fault) = &fault {
        out.push_str(&fault_text(fault));
    }
    // The devrun claim is only true when `devrun` resolved; a pins-only
    // checkout has no devrun setup at all, so asserting it here would be a
    // false claim injected into an agent's context. A checkout whose only
    // content is the fault gets neither claim: what it has is a broken config,
    // which the block above already stated.
    let intro = match (&devrun, &pins, &fault) {
        (Some(sections), _, _) => Some(devrun_intro(&root, sections.facilities())),
        (None, Some(_), _) => Some(
            "This checkout has libraries registered with devkit; the table below is \
             what its lockfiles pin."
                .to_string(),
        ),
        (None, None, _) => None,
    };
    if let Some(intro) = intro {
        out.push_str(&wrap(&intro));
        out.push_str("\n\n");
    }
    if let Some(sections) = &devrun {
        out.push_str(&devrun_text(sections));
    }
    if let Some(section) = pins {
        out.push_str(&section);
    }
    // Each block separates itself from the next by ending in a blank line; the
    // last one has nothing to separate from.
    while out.ends_with("\n\n") {
        out.pop();
    }
    Some(out)
}

/// The devrun claim, naming only the facilities this checkout actually has.
/// Naming all four unconditionally would tell an agent about canned tasks a
/// project has never configured, and about file locks its owner switched off.
fn devrun_intro(root: &str, facilities: Facilities) -> String {
    let mut named = Vec::new();
    if facilities.ports() {
        named.push("dev servers");
        named.push("ports");
    }
    if facilities.tasks {
        named.push("canned tasks");
    }
    if facilities.locks {
        named.push("cross-session file locks");
    }
    format!(
        "This checkout ({root}) is a devkit-managed project: {} are coordinated by the \
         devkit CLIs. Load the `using-devkit` skill before using them.",
        join_and(&named)
    )
}

/// `["a"] → "a"`, `["a", "b"] → "a and b"`, `["a", "b", "c"] → "a, b, and c"`.
fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_string(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
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
        "SOURCE says where each version came from: `resolved checkout` means this \
         project resolved the library once, not that a manifest here declares it. \
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
/// devrun-configured project or has nothing to report about being one. A
/// `[brief]` switch turned off reads here exactly as an empty catalog or task
/// list does, so a suppressed section takes the bullets that introduce it with
/// it.
fn devrun_sections(root: &str, cwd: &Path, settings: &BriefConfig) -> Option<DevrunBrief> {
    let loaded = devrun_project(root, cwd)?;
    let rows = task::list(&loaded.config);
    let sections = DevrunBrief {
        apps: settings.apps.then(|| apps_line(&loaded.catalog)).flatten(),
        tasks: (settings.tasks && !rows.is_empty()).then(|| task::tasks_text(&rows)),
        servers: live_servers(root),
        locks: settings.locks,
    };
    sections.facilities().any().then_some(sections)
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

/// The configured apps, or `None` when none resolve — a line whose only
/// content is that there is nothing to say is worth less than the space it
/// takes in an agent's context.
fn apps_line(catalog: &HashMap<String, App>) -> Option<String> {
    if catalog.is_empty() {
        return None;
    }
    let mut apps: Vec<&App> = catalog.values().collect();
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Some(
        apps.iter()
            .map(|a| format!("{} ({})", a.name, a.path))
            .collect::<Vec<_>>()
            .join(", "),
    )
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

fn devrun_text(sections: &DevrunBrief) -> String {
    let facilities = sections.facilities();
    let mut out = String::new();
    if facilities.ports() {
        out.push_str(
            "- `devrun up <app>` / `devrun down` — start/stop supervised dev servers for this worktree\n",
        );
    }
    if facilities.tasks {
        out.push_str(
            "- `devrun task <name> [--dry-run]` — run a canned project task (table below)\n",
        );
    }
    let mut tools = Vec::new();
    if facilities.ports() {
        tools.push("`portm status` — port registry");
    }
    if facilities.locks {
        tools.push("`lockm status` — advisory file locks");
    }
    if !tools.is_empty() {
        out.push_str(&format!("- {}\n", tools.join("; ")));
    }

    if let Some(apps) = &sections.apps {
        separate(&mut out);
        out.push_str(&format!("Apps (`devrun up`): {apps}\n"));
    }
    if let Some(tasks) = &sections.tasks {
        separate(&mut out);
        out.push_str("### Tasks (`devrun task <name>`)\n\n");
        out.push_str(tasks);
    }
    if let Some(servers) = &sections.servers {
        separate(&mut out);
        out.push_str("### Live servers in this worktree\n\n");
        out.push_str(servers);
    }
    // `ui::table` renders without a trailing newline, so a half ending in a
    // table would otherwise butt the library section's heading against its
    // last row.
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Open a blank line before the next block, unless one is already there.
/// Sections appear or not independently, so no block can know whether it is
/// following another one.
fn separate(out: &mut String) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
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
        let cfg = brief_config(tmp.path(), None);
        assert!(
            cfg.enabled,
            "an unreadable config costs a brief, never withholds one"
        );
        assert!(cfg.pins);
        assert!(cfg.locks);
        assert!(cfg.apps);
        assert!(cfg.tasks);
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

    fn brief(
        apps: Option<&str>,
        tasks: Option<&str>,
        servers: Option<&str>,
        locks: bool,
    ) -> DevrunBrief {
        DevrunBrief {
            apps: apps.map(str::to_string),
            tasks: tasks.map(str::to_string),
            servers: servers.map(str::to_string),
            locks,
        }
    }

    #[test]
    fn render_text_sections_and_optional_servers() {
        let text = devrun_text(&brief(
            Some("api (apps/api)"),
            Some("NAME KIND\n"),
            None,
            true,
        ));
        assert!(text.contains("api (apps/api)"), "{text}");
        assert!(text.contains("devrun task"), "{text}");
        assert!(!text.contains("Live servers"), "{text}");

        let with = devrun_text(&brief(
            Some("api (apps/api)"),
            Some("NAME KIND\n"),
            Some("PORT APP\n"),
            true,
        ));
        assert!(with.contains("Live servers in this worktree"), "{with}");
        assert!(with.contains("PORT APP"), "{with}");
    }

    #[test]
    fn no_apps_drops_the_server_and_port_lines() {
        let text = devrun_text(&brief(None, Some("NAME KIND\n"), None, true));
        assert!(!text.contains("Apps (`devrun up`)"), "{text}");
        assert!(!text.contains("devrun up"), "{text}");
        assert!(!text.contains("portm status"), "{text}");
        assert!(text.contains("devrun task"), "{text}");
        assert!(text.contains("lockm status"), "{text}");
    }

    #[test]
    fn no_tasks_drops_the_task_bullet_and_section() {
        let text = devrun_text(&brief(Some("api (apps/api)"), None, None, true));
        assert!(!text.contains("devrun task"), "{text}");
        assert!(text.contains("Apps (`devrun up`)"), "{text}");
    }

    #[test]
    fn locks_off_drops_the_lockm_line_and_keeps_portm() {
        let text = devrun_text(&brief(
            Some("api (apps/api)"),
            Some("NAME KIND\n"),
            None,
            false,
        ));
        assert!(!text.contains("lockm"), "{text}");
        assert!(text.contains("portm status"), "{text}");
    }

    #[test]
    fn a_live_server_claims_ports_without_a_catalog_entry() {
        let text = devrun_text(&brief(None, None, Some("PORT APP\n"), false));
        assert!(text.contains("devrun down"), "{text}");
        assert!(text.contains("portm status"), "{text}");
        assert!(text.contains("Live servers"), "{text}");
    }

    #[test]
    fn the_intro_names_only_the_facilities_present() {
        let full = devrun_intro("/w", brief(Some("a"), Some("t"), None, true).facilities());
        assert!(
            full.contains("dev servers, ports, canned tasks, and cross-session file locks"),
            "{full}"
        );

        let ports = devrun_intro("/w", brief(Some("a"), None, None, false).facilities());
        assert!(
            ports.contains("dev servers and ports are coordinated"),
            "{ports}"
        );
        assert!(!ports.contains("canned tasks"), "{ports}");
        assert!(!ports.contains("file locks"), "{ports}");

        let locks = devrun_intro("/w", brief(None, None, None, true).facilities());
        assert!(
            locks.contains("cross-session file locks are coordinated"),
            "{locks}"
        );
    }

    #[test]
    fn a_member_with_nothing_to_report_has_no_devrun_half() {
        assert!(!brief(None, None, None, false).facilities().any());
        assert!(brief(None, None, None, true).facilities().any());
    }
}
