//! `devkit brief` — a compact project orientation for coding-agent session
//! hooks. Prints the configured apps, canned tasks, and any live servers for
//! the current worktree when the working directory belongs to a
//! devkit-managed project, a rules section when the write hook has a rule
//! index to inject from, and a library-versions table for any registered
//! library this checkout evidences. The sections are independent, so a
//! docs-only checkout with no devrun setup still gets the last. Prints
//! nothing when none applies, so a SessionStart hook can call it
//! unconditionally from any repository.
//!
//! Silence is for a checkout with nothing to say, never for one that cannot be
//! read. A `devkit.toml` that exists and fails to load is reported with its
//! cause chain: it makes every devkit command fail, and an empty brief would
//! instead teach the session that this is not a devkit project.
//!
//! Within the devrun half every section earns its place: a project with no
//! configured apps is not told about `devrun up`, and one with no `[tasks]`
//! table is not told about `devrun task`. `[brief]` can suppress a section the
//! checkout does have (`apps`, `tasks` and `locks`), which reads downstream
//! exactly as an absent one, so the bullets introducing it go too. `locks` has
//! no other way to be decided: whether sessions share this checkout is not
//! observable. Every task is listed under Tasks with its description, a task
//! an app owns tagged with that app, so a scan of Tasks finds every check.
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
//! `--harness` decides how the brief travels rather than what it says: Claude
//! Code's session start reads plain stdout, every other hook the event's JSON
//! answer. A subagent's brief keeps no watermark, since its payload carries its
//! parent's session id, and a fork, which inherits its parent's context, gets
//! none.

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::Result;
use devkit_common::vcs::Checkout;
use devkit_config as config;
use devkit_config::BriefConfig;
use devkit_ports::{apps::App, load, registry, task};
use devkit_rules::vocab::Severity;
use pabal::AnyHarness;

use crate::hook::{
    HookEvent,
    payload::{Holder, Payload},
};

/// The hook a brief or a rules block answers: its payload, and whether the
/// text travels inside the event's context answer or as plain stdout.
pub(crate) struct Hook {
    payload: Payload,
    answer: bool,
}

impl Hook {
    /// The payload on stdin, or an empty one when there is none to read: an
    /// interactive run, or a pipe carrying no JSON object.
    pub(crate) fn read(harness: Option<AnyHarness>) -> Self {
        let read = (!std::io::stdin().is_terminal())
            .then(|| crate::hook::read_payload(harness, HookEvent::SessionStart))
            .flatten();
        Self {
            payload: read.unwrap_or_else(|| Payload::empty(harness, HookEvent::SessionStart)),
            answer: harness.is_some(),
        }
    }

    /// Print `text` the way the host reads it. Nothing where the harness has
    /// no context channel on this event, and nothing for empty text, which
    /// would hand the agent a context block with nothing in it.
    pub(crate) fn emit(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.answer {
            print!("{text}");
        } else if let Some(answer) = self.payload.context_answer(text) {
            println!("{answer}");
        }
    }

    /// A fork starts with its parent's context, brief and rules included.
    pub(crate) fn is_fork(&self) -> bool {
        self.payload.is_fork()
    }

    /// The holder the rules this hook shows are recorded under.
    pub(crate) fn holder(&self) -> Option<Holder> {
        self.payload.holder().ok()
    }

    /// The session whose `--if-changed` watermark this brief keeps. `None`
    /// for a subagent, whose payload carries its parent's session id.
    fn watermark_session(&self) -> Option<&str> {
        match self.payload.agent() {
            Some(_) => None,
            None => self.payload.session_id().filter(|id| !id.is_empty()),
        }
    }

    /// Cursor's hooks never see an edit tool, so nothing injects rules for it.
    fn cursor(&self) -> bool {
        self.payload.harness() == AnyHarness::Cursor
    }
}

pub fn run(pins_only: bool, if_changed: bool, harness: Option<AnyHarness>) -> Result<()> {
    let hook = Hook::read(harness);
    if hook.is_fork() {
        return Ok(());
    }
    let cursor = hook.cursor();
    // A brief is context injection, never a gate: any failure (no cwd, no git,
    // no config, unreadable registry) means no output, exit 0.
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    // One `git worktree list` for the whole brief: every helper below takes
    // this rather than asking git for the checkout root or the main worktree
    // again. It resolves lazily, so a brief switched off in config still
    // spawns nothing.
    let checkout = Checkout::at(&cwd);
    let settings = brief_config(&checkout, &cwd);
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
            if let Some(session) = hook.watermark_session() {
                let _ = std::fs::remove_file(watermark_path(session));
            }
            if let Some(text) = pins_only_text(&cwd, &settings) {
                hook.emit(&text);
            }
            return Ok(());
        }
        if let Some(text) = render(&cwd, &settings, &checkout, cursor) {
            hook.emit(&text);
            if let Some(session) = hook.watermark_session() {
                stamp(session, &cwd, &settings, &checkout);
            }
        }
        return Ok(());
    }

    let session = hook.watermark_session();
    let digest = snapshot(&cwd, &settings, &checkout).map(|s| s.digest());
    let Some(session) = session else {
        // No id means emit without persisting: a shared per-cwd key would let
        // one session's brief suppress another's re-injection, and a withheld
        // brief is the worse failure.
        if let Some(text) = render(&cwd, &settings, &checkout, cursor) {
            hook.emit(&text);
        }
        return Ok(());
    };

    let path = watermark_path(session);
    let previous = std::fs::read_to_string(&path).ok();
    let current = digest.map(|d| format!("{d:016x}"));
    if previous.is_some() && previous.as_deref() == current.as_deref() {
        return Ok(());
    }
    if let Some(current) = &current {
        write_watermark(&path, current);
    }
    match render(&cwd, &settings, &checkout, cursor) {
        Some(text) => hook.emit(&text),
        // Left the project: silence would leave the previous checkout's brief
        // as the most recent thing the agent was told.
        None if previous.is_some() => {
            let _ = std::fs::remove_file(&path);
            hook.emit(
                "## devkit project context\n\nThis directory is not a devkit-managed project; the earlier project brief no longer applies.\n",
            );
        }
        None => {}
    }
    Ok(())
}

/// The canonical form `--if-changed` hashes: every section's identity and no
/// clock. Apps and tasks are hashed as rendered, since their lists carry no
/// table and so no terminal width; servers and pins render as tables and are
/// hashed as fields instead. Hashing only the pins would suppress a brief whose
/// apps, tasks or servers changed while the pins held still.
struct BriefSnapshot {
    root: String,
    apps: Option<String>,
    tasks: Option<String>,
    servers: Vec<ServerKey>,
    locks: bool,
    enforced: bool,
    pins: Vec<PinKey>,
    rules: Option<Severity>,
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
    /// Each app with its directory.
    apps: Option<String>,
    /// Every task, with the app it runs in.
    tasks: Option<String>,
    servers: Option<String>,
    locks: bool,
    /// The write stage claims locks for this checkout, so the agent does not
    /// reach for `lockm` before an edit.
    enforced: bool,
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
    enforced: bool,
}

impl Facilities {
    /// A registry row is a port this worktree holds whether or not the catalog
    /// still names the app that bound it, so either one keeps `devrun down`
    /// relevant.
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
            enforced: self.enforced,
        }
    }
}

impl BriefSnapshot {
    /// A stable byte string, so the digest does not depend on struct layout.
    fn canonical(&self) -> String {
        let mut out = format!("root\t{}\n", self.root);
        for line in self.apps.iter().flat_map(|a| a.lines()) {
            out.push_str(&format!("app\t{line}\n"));
        }
        for line in self.tasks.iter().flat_map(|t| t.lines()) {
            out.push_str(&format!("task\t{line}\n"));
        }
        for s in &self.servers {
            out.push_str(&format!(
                "server\t{}\t{}\t{}\t{}\t{}\n",
                s.port, s.app, s.role, s.pid, s.listening
            ));
        }
        out.push_str(&format!("locks\t{}\n", self.locks));
        out.push_str(&format!("enforced\t{}\n", self.enforced));
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
        if let Some(floor) = self.rules {
            out.push_str(&format!("rules\t{floor}\n"));
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
/// has something to compare against.
fn stamp(session: &str, cwd: &Path, settings: &BriefConfig, checkout: &Checkout) {
    let Some(digest) = snapshot(cwd, settings, checkout).map(|s| s.digest()) else {
        return;
    };
    write_watermark(&watermark_path(session), &format!("{digest:016x}"));
}

/// Fails open: an unreadable or unwritable state directory reports "changed",
/// costing a duplicate brief rather than withholding one.
fn write_watermark(path: &Path, digest: &str) {
    let _ = std::fs::create_dir_all(path.parent().expect("watermark has a parent"));
    let _ = std::fs::write(path, digest);
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
fn snapshot(cwd: &Path, settings: &BriefConfig, checkout: &Checkout) -> Option<BriefSnapshot> {
    let root = checkout.root()?.to_string_lossy().into_owned();

    let pins = checkout_pins(cwd, settings);
    let (relevant, _) = devkit_docs::pins::relevant(&pins);
    let pin_keys: Vec<PinKey> = relevant.iter().map(|pin| PinKey::of(pin)).collect();
    let devrun = devrun_project(checkout, &root, cwd);

    // The listing is the same text `render` emits, which carries no table and
    // so no terminal width, and a switched-off section is absent from it.
    let listing = devrun.as_ref().map(|loaded| Listing::of(loaded, settings));

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
    let enforced = locks && devkit_common::harness::writes_enabled(checkout, cwd);
    let (apps, tasks) = match listing {
        Some(l) => (l.apps, l.tasks),
        None => (None, None),
    };
    let facilities = Facilities {
        apps: apps.is_some(),
        tasks: tasks.is_some(),
        servers: !servers.is_empty(),
        locks,
        enforced,
    };
    let config_fault = config_fault(cwd, checkout);
    let rules = rules_floor(checkout, cwd, settings);
    if pin_keys.is_empty() && !facilities.any() && config_fault.is_none() && rules.is_none() {
        return None;
    }

    Some(BriefSnapshot {
        root,
        apps,
        tasks,
        servers,
        locks,
        enforced,
        pins: pin_keys,
        rules,
        config_fault,
    })
}

/// The `[brief]` settings for `cwd`, defaulting to on. Config resolution alone
/// and not `load::load`: `load` also reads doppler.yaml and builds the app
/// catalog, which is what fails on a docs-only project. An unreadable config
/// falls open to the defaults.
fn brief_config(checkout: &Checkout, cwd: &Path) -> BriefConfig {
    devkit_common::config::resolve_in(checkout, None, cwd)
        .map(|(cfg, _)| cfg.brief)
        .unwrap_or_default()
}

/// The reason this checkout's config does not load, or `None` when it loads or
/// does not exist. An absent config is how every non-devkit repository looks,
/// so only a config that exists and fails is worth a word.
fn config_fault(cwd: &Path, checkout: &Checkout) -> Option<String> {
    match config::health(cwd, checkout.main_checkout()) {
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
         Every devkit command fails the same way until it is fixed.",
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

fn render(cwd: &Path, settings: &BriefConfig, checkout: &Checkout, cursor: bool) -> Option<String> {
    let root = checkout.root()?.to_string_lossy().into_owned();

    // Pins are computed before `load`: a devkit.toml carrying [docs] and
    // nothing devrun can use must still produce a brief.
    let pins = pins_section(&checkout_pins(cwd, settings));
    let devrun = devrun_sections(checkout, &root, cwd, settings);
    let fault = config_fault(cwd, checkout);
    let rules = rules_floor(checkout, cwd, settings);
    if pins.is_none() && devrun.is_none() && fault.is_none() && rules.is_none() {
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
        (Some(sections), ..) => Some(devrun_intro(&root, sections.facilities())),
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
    if let Some(floor) = rules {
        out.push_str(&rules_text(floor, cursor));
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

/// The devrun claim. The facilities themselves are named by the bullets that
/// follow, so the intro does not list them again.
fn devrun_intro(root: &str, facilities: Facilities) -> String {
    let mut out = format!(
        "This checkout ({root}) is a devkit project. Load the `using-devkit` skill before \
         running devkit commands."
    );
    if facilities.enforced {
        out.push_str(
            " Writes here are lock-enforced: devkit claims every file you change, through an \
             edit tool or a shell command, and releases it at session end. A denied write says \
             why, naming the holder when another session has the file.",
        );
    }
    out
}

/// The least severe rule the write hook injects, or `None` when the brief has
/// no rules section: switched off, `[rules]` disabled, or no rules to read. An
/// unparseable floor reads as `should`, the same fallback the hook takes.
///
/// Decided once per process: a brief renders and hashes its sections in one
/// run, and a rule source on a database must be asked only once within the
/// hook's timeout.
fn rules_floor(checkout: &Checkout, cwd: &Path, settings: &BriefConfig) -> Option<Severity> {
    static FLOOR: OnceLock<Option<Severity>> = OnceLock::new();
    if !settings.rules {
        return None;
    }
    *FLOOR.get_or_init(|| {
        let rules = crate::rules::enabled_rules(checkout, cwd)?;
        Some(rules.min_severity.parse().unwrap_or(Severity::Should))
    })
}

/// What the write hook does with the index, as the reading host sees it. The
/// hook injects before edit-tool writes only, so shell writes get nothing, and
/// under Cursor, whose hooks never see an edit tool, nothing does.
fn rules_text(floor: Severity, cursor: bool) -> String {
    let body = if cursor {
        "This repository has a rule index. Cursor edits get no rules added automatically, so run \
         `devkit rules query --path <file>` before editing a file to see the rules that govern it."
            .to_string()
    } else {
        let injected = match floor {
            Severity::Must => "`must` rules",
            Severity::Should => "`must` and `should` rules",
            Severity::Can => "rules of every severity",
        };
        format!(
            "This repository has a rule index. Before an edit tool writes a file, devkit adds that \
             file's {injected} to your context, each rule once per session; shell writes (`sed \
             -i`, `>`, heredocs) get none. `devkit rules query --path <file>` lists all of a \
             file's rules, whatever their severity."
        )
    };
    format!("\n### Rules\n\n{}\n", wrap(&body))
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
fn devrun_project(checkout: &Checkout, root: &str, cwd: &Path) -> Option<load::Loaded> {
    let loaded = load::load_in(checkout, None, cwd).ok()?;
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
fn devrun_sections(
    checkout: &Checkout,
    root: &str,
    cwd: &Path,
    settings: &BriefConfig,
) -> Option<DevrunBrief> {
    let loaded = devrun_project(checkout, root, cwd)?;
    let listing = Listing::of(&loaded, settings);
    let sections = DevrunBrief {
        apps: listing.apps,
        tasks: listing.tasks,
        servers: live_servers(root),
        locks: settings.locks,
        enforced: settings.locks
            && devkit_common::harness::writes_enabled(checkout, Path::new(root)),
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

/// A task as the brief lists it.
struct TaskEntry {
    name: String,
    app: Option<String>,
    description: String,
    /// What has to be in place before it runs, as the commands that put it
    /// there: `devrun up` for each server it needs live, and each `--arg` this
    /// caller must pass.
    needs: Vec<String>,
    invalid: bool,
}

impl TaskEntry {
    fn all(config: &config::Config, catalog: &HashMap<String, App>) -> Vec<TaskEntry> {
        task::list(config, catalog, devkit_common::caller::caller())
            .into_iter()
            .map(|row| {
                let mut needs: Vec<String> = row
                    .require_live
                    .iter()
                    .map(|app| format!("`devrun up {app}`"))
                    .collect();
                needs.extend(
                    row.args
                        .iter()
                        .filter(|arg| arg.required)
                        .map(|arg| format!("`--arg {}=...`", arg.name)),
                );
                TaskEntry {
                    app: config.tasks.get(&row.name).and_then(|t| t.app.clone()),
                    invalid: row.kind.is_err(),
                    name: row.name,
                    description: row.description,
                    needs,
                }
            })
            .collect()
    }

    fn line(&self) -> String {
        let mut out = format!("- {}", self.name);
        if !self.description.is_empty() {
            out.push_str(&format!(": {}", self.description));
        }
        out.push_str(&self.caveat());
        out
    }

    /// The app it runs in, then what the agent would otherwise learn from a
    /// failed run.
    fn caveat(&self) -> String {
        let mut parts: Vec<String> = self.app.iter().map(|app| format!("app {app}")).collect();
        if self.invalid {
            parts.push(format!(
                "invalid: `devkit config tasks {}` says why",
                self.name
            ));
        } else if !self.needs.is_empty() {
            parts.push(format!("needs {}", self.needs.join(", ")));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        }
    }
}

/// The apps section and the task list.
struct Listing {
    apps: Option<String>,
    tasks: Option<String>,
}

impl Listing {
    fn of(loaded: &load::Loaded, settings: &BriefConfig) -> Listing {
        let entries = if settings.tasks {
            TaskEntry::all(&loaded.config, &loaded.catalog)
        } else {
            Vec::new()
        };
        let mut apps: Vec<&App> = if settings.apps {
            loaded.catalog.values().collect()
        } else {
            Vec::new()
        };
        apps.sort_by(|a, b| a.name.cmp(&b.name));
        Listing::build(&entries, &apps)
    }

    /// `apps` sorted by name.
    fn build(entries: &[TaskEntry], apps: &[&App]) -> Listing {
        let apps_text: String = apps
            .iter()
            .map(|app| format!("- {} ({})\n", app.name, app.path))
            .collect();
        let tasks_text: String = entries.iter().map(|e| format!("{}\n", e.line())).collect();
        Listing {
            apps: (!apps_text.is_empty()).then_some(apps_text),
            tasks: (!tasks_text.is_empty()).then_some(tasks_text),
        }
    }
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
            "- `devrun up <app>` / `devrun down`: start or stop this worktree's dev servers, which run in the background\n",
        );
    }
    if facilities.tasks {
        out.push_str(
            "- `devrun task <name>`: run a task from anywhere in the checkout; `--dry-run` prints what it would run\n",
        );
        out.push_str(if facilities.ports() {
            "- `devkit config tasks <name>`: what a task runs, the servers it needs, and every arg it takes\n"
        } else {
            "- `devkit config tasks <name>`: what a task runs and every arg it takes\n"
        });
    }
    // Under enforcement the hooks claim locks for every write, and a denial
    // names the holder, so pointing at `lockm` only invites a manual acquire.
    if facilities.locks && !facilities.enforced {
        out.push_str("- `lockm status`: advisory file locks held by other sessions\n");
    }

    if let Some(tasks) = &sections.tasks {
        separate(&mut out);
        out.push_str("### Tasks (`devrun task <name>`)\n\n");
        out.push_str(tasks);
    }
    if let Some(apps) = &sections.apps {
        separate(&mut out);
        out.push_str("### Apps (`devrun up <app>`)\n\n");
        if facilities.tasks {
            out.push_str("`devrun task <name>` runs an app's tasks in the app's directory.\n\n");
        }
        out.push_str(apps);
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
        let cfg = brief_config(&Checkout::at(tmp.path()), tmp.path());
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

        // Only the home layer, app dir absent -> not a member.
        let layers = vec![home.clone()];
        assert!(!is_project_member(root, &layers, Some(&home), &catalog));

        // An app rooted at "." exists under every directory; it must not
        // make an unrelated checkout a member.
        catalog.insert("chrome".to_string(), app("chrome", "."));
        assert!(!is_project_member(root, &layers, Some(&home), &catalog));

        // A project devkit.toml layer -> member even without app dirs.
        let project = vec![home.clone(), tmp.path().join("devkit.toml")];
        assert!(is_project_member(root, &project, Some(&home), &catalog));

        // App dir exists under root -> member from the home layer alone.
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
            enforced: false,
        }
    }

    fn brief_enforced(
        apps: Option<&str>,
        tasks: Option<&str>,
        servers: Option<&str>,
    ) -> DevrunBrief {
        DevrunBrief {
            apps: apps.map(str::to_string),
            tasks: tasks.map(str::to_string),
            servers: servers.map(str::to_string),
            locks: true,
            enforced: true,
        }
    }

    const APPS: &str = "- api (apps/api)\n";
    const TASKS: &str = "- check: tests\n";

    #[test]
    fn enforcement_replaces_the_lockm_pointer_with_the_automatic_claim() {
        let on = brief_enforced(Some(APPS), Some(TASKS), None);
        let intro = devrun_intro("/w", on.facilities());
        assert!(intro.contains("claims every file you change"), "{intro}");
        let text = devrun_text(&on);
        assert!(!text.contains("lockm"), "{text}");

        let off = brief(Some(APPS), Some(TASKS), None, true);
        let intro = devrun_intro("/w", off.facilities());
        assert!(!intro.contains("claims every file"), "{intro}");
        let text = devrun_text(&off);
        assert!(text.contains("lockm status"), "{text}");
    }

    #[test]
    fn enforcement_alone_still_yields_an_intro() {
        let only = devrun_intro("/w", brief_enforced(None, None, None).facilities());
        assert!(only.contains("is a devkit project"), "{only}");
        assert!(only.contains("claims every file you change"), "{only}");
    }

    #[test]
    fn render_text_sections_and_optional_servers() {
        let text = devrun_text(&brief(Some(APPS), Some(TASKS), None, true));
        assert!(text.contains("### Apps (`devrun up <app>`)"), "{text}");
        assert!(text.contains("- api (apps/api)"), "{text}");
        assert!(text.contains("### Tasks (`devrun task <name>`)"), "{text}");
        assert!(text.contains("- check: tests"), "{text}");
        assert!(!text.contains("Live servers"), "{text}");

        let with = devrun_text(&brief(Some(APPS), Some(TASKS), Some("PORT APP\n"), true));
        assert!(with.contains("Live servers in this worktree"), "{with}");
        assert!(with.contains("PORT APP"), "{with}");
    }

    #[test]
    fn no_apps_drops_every_mention_of_servers() {
        let text = devrun_text(&brief(None, Some(TASKS), None, true));
        assert!(!text.contains("### Apps"), "{text}");
        assert!(!text.contains("devrun up"), "{text}");
        assert!(!text.contains("servers"), "{text}");
        assert!(text.contains("devrun task"), "{text}");
    }

    #[test]
    fn no_tasks_drops_the_task_bullets_and_section() {
        let text = devrun_text(&brief(Some(APPS), None, None, true));
        assert!(!text.contains("devrun task"), "{text}");
        assert!(!text.contains("devkit config tasks"), "{text}");
        assert!(text.contains("### Apps"), "{text}");
    }

    #[test]
    fn locks_off_drops_the_lockm_line() {
        let text = devrun_text(&brief(Some(APPS), Some(TASKS), None, false));
        assert!(!text.contains("lockm"), "{text}");
    }

    #[test]
    fn a_live_server_claims_ports_without_a_catalog_entry() {
        let text = devrun_text(&brief(None, None, Some("PORT APP\n"), false));
        assert!(text.contains("devrun down"), "{text}");
        assert!(text.contains("Live servers"), "{text}");
    }

    #[test]
    fn the_devrun_half_carries_no_em_dash_and_no_port_registry() {
        let text = devrun_text(&brief(Some(APPS), Some(TASKS), Some("PORT APP\n"), true));
        let intro = devrun_intro("/w", brief_enforced(Some(APPS), None, None).facilities());
        for out in [&text, &intro] {
            assert!(!out.contains('\u{2014}'), "{out}");
            assert!(!out.contains("portm"), "{out}");
        }
    }

    fn entry(name: &str, app: Option<&str>, needs: &[&str]) -> TaskEntry {
        TaskEntry {
            name: name.into(),
            app: app.map(str::to_string),
            description: format!("{name} does things"),
            needs: needs.iter().map(|n| n.to_string()).collect(),
            invalid: false,
        }
    }

    #[test]
    fn every_task_is_listed_and_apps_list_none() {
        let (api, web) = (app("api", "apps/api"), app("web", "apps/web"));
        let entries = [
            entry("test", None, &[]),
            entry("migrate", Some("api"), &["`--arg env=...`"]),
            entry("e2e", Some("web"), &["`devrun up api`"]),
        ];
        let listing = Listing::build(&entries, &[&api, &web]);
        assert_eq!(
            listing.apps.unwrap(),
            "- api (apps/api)\n- web (apps/web)\n"
        );
        assert_eq!(
            listing.tasks.unwrap(),
            "- test: test does things\n\
             - migrate: migrate does things (app api, needs `--arg env=...`)\n\
             - e2e: e2e does things (app web, needs `devrun up api`)\n"
        );
    }

    #[test]
    fn an_invalid_task_points_at_the_command_that_explains_it() {
        let mut bad = entry("bad", None, &["`--arg x=...`"]);
        bad.invalid = true;
        assert_eq!(
            bad.line(),
            "- bad: bad does things (invalid: `devkit config tasks bad` says why)"
        );
    }

    #[test]
    fn a_member_with_nothing_to_report_has_no_devrun_half() {
        assert!(!brief(None, None, None, false).facilities().any());
        assert!(brief(None, None, None, true).facilities().any());
    }
}
