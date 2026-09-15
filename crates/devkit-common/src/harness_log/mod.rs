//! `harness_log`: what agents tried to run, and what devkit decided about it.
//!
//! Off by default and enablable only from the global config. The module owns
//! the record types, redaction, the writer and the prune sweep behind one
//! infallible entry point, so a logging failure can never reach a hook's
//! verdict.
//!
//! It lives in `devkit-common` rather than a crate of its own because it needs
//! `paths::state_dir` and `secrets`.

pub mod redact;
pub mod writer;

use std::path::{Path, PathBuf};

use devkit_config::{Fidelity, LogSection, PromptFidelity};
use serde::{Deserialize, Serialize};
pub use writer::{now_rfc3339, record, record_to};

use crate::{harness::parse_env_override, paths};

/// Bumped when the record envelope changes shape. Every record carries it, so
/// a reader meeting an unfamiliar one knows it rather than guessing.
pub const SCHEMA_VERSION: u32 = 1;

/// One line of the log.
///
/// Every record is useful by itself. Records join on their components —
/// `(session_id, agent_id, tool_use_id)` — rather than a derived hash, because
/// a hash is one more step between the reader and the data and hides why a join
/// failed. Where a harness sends no `tool_use_id`, nothing joins and both
/// records stand alone; the join adds analysis and is never a prerequisite for
/// reading one.
///
/// Records are append-only and never rewritten, so pairing never mutates a
/// prior line, no reader-writer lock is ever needed, and a process killed
/// mid-session cannot corrupt what is already there.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Record {
    pub schema_version: u32,
    /// RFC 3339, UTC. File order is not logical order, so readers sort by this.
    pub recorded_at: String,
    pub devkit_version: String,
    /// `devkit_command::ANALYZER_VERSION`, passed in: `devkit-common` does not
    /// depend on `devkit-command`, which would compile six tree-sitter C
    /// grammars into every library crate.
    pub analyzer_version: u32,
    pub harness: Option<String>,
    /// The devkit verb.
    pub event: String,
    /// The payload's own `hook_event_name`. Both are needed because the mapping
    /// is not injective: Codex sends `Stop` and `Interrupt` to one verb, and a
    /// reader asking which vendor event produced a record cannot recover it
    /// from the verb alone.
    pub vendor_event: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub cwd: Option<PathBuf>,
    pub project_root: Option<PathBuf>,
    #[serde(flatten)]
    pub kind: Kind,
}

/// What this record is about. Flattened into the envelope under a `kind` tag,
/// so a reader can select on one field and read the payload beside it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Kind {
    /// A shell command, its analysis, and the verdict devkit reached. Boxed
    /// because it dwarfs every other variant, and one is built per process.
    /// `Box` is serde-transparent, so the record shape is unchanged.
    ShellPre(Box<ShellPre>),
    /// How that call turned out.
    ShellPost(ShellPost),
    /// A structured edit and the targets it named.
    EditPre(EditPre),
    /// A session or subagent frame.
    Session(SessionFrame),
    /// What the harness asked about, or what its own classifier blocked.
    Permission(Permission),
    /// A turn or context boundary.
    Lifecycle,
    /// A worktree appearing or going away.
    Worktree(Worktree),
    /// A submitted prompt, behind its own fidelity key.
    Prompt(Prompt),
}

/// Which end of a frame a `session` record names. Neither is a dependency:
/// session end does not fire on a crash or a `kill -9`, and per-call records
/// repeat harness, project and cwd, so a missing frame costs nothing. The start
/// record is what gives a crashed session any frame at all.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FrameEnd {
    Start,
    End,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SessionFrame {
    pub end: FrameEnd,
    /// Whether this frame is a subagent's rather than the session's own.
    pub subagent: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ShellPre {
    /// The command at the resolved fidelity. `corpus_probe` reads this key.
    pub command: String,
    /// Whether redaction substituted anything.
    pub redacted: bool,
    /// Whether the command was cut at the size cap before recording. A heredoc
    /// carrying a whole file is not corpus signal.
    pub truncated: bool,
    pub tool_name: Option<String>,
    /// The dialect the command was read in, so an offline re-analysis does not
    /// have to infer one from harness plus platform.
    pub dialect: Option<String>,
    pub analysis: Option<AnalysisProjection>,
    pub verdict: Verdict,
}

/// What devkit decided, and the full text of every message it offered. Those
/// messages are the correction devkit gave the agent, and whether a denial was
/// a good one is not answerable without them.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    pub blocks: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
    /// The guard ran and could not decide: a payload it could not read, an
    /// analyser panic, or a write stage that missed its deadline. An analyser
    /// panic on real traffic is the highest-value record in the corpus.
    Undecided,
}

/// A summary of the analysis rather than the tree. The full invocation tree is
/// reconstructable by replaying the command through `analyze`, which is what
/// makes `analyzer_version` load-bearing: a record whose stamp differs from the
/// current binary is a regression candidate, not stale data.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct AnalysisProjection {
    pub resolved_writes: Vec<String>,
    pub unresolved_writes: Vec<String>,
    pub tree_effects: Vec<String>,
    pub script_files: Vec<String>,
    pub programs: Vec<String>,
    pub uncertainties: Vec<Uncertainty>,
    pub analyze_micros: u64,
    /// So a cohort query can rank without reading every projection.
    pub counts: Counts,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub invocations: usize,
    pub resolved_writes: usize,
    pub unresolved_writes: usize,
    pub uncertainties: usize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Uncertainty {
    pub kind: String,
    pub detail: Option<String>,
    /// Byte span into the command as analysed.
    pub start: Option<usize>,
    pub end: Option<usize>,
}

/// Each field is absent, not zero, where the harness does not supply it. Codex
/// forces that wording: its post payload exposes `tool_response` as output text
/// rather than a structured result, so no exit code reaches the hook.
///
/// Not the output itself. Command output is large and is where credentials
/// actually surface — a `gh auth status`, a printenv, a failed curl echoing its
/// own headers — and redaction over arbitrary program output would be theatre.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellPost {
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
    pub error: Option<bool>,
    pub interrupted: Option<bool>,
    pub stdout_bytes: Option<usize>,
    pub stderr_bytes: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct EditPre {
    pub tool_name: Option<String>,
    pub targets: Vec<String>,
    pub verdict: Verdict,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Permission {
    /// Whether the harness asked about this or blocked it outright.
    pub blocked: bool,
    pub tool_name: Option<String>,
    pub detail: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeChange {
    Create,
    Remove,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub change: WorktreeChange,
    pub path: Option<String>,
}

/// `text` is `None` at the default fidelity, where the record says a prompt was
/// submitted and carries none of it. A corpus can hold full command text
/// without holding what the human typed.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub text: Option<String>,
    pub chars: Option<usize>,
}

/// The env override, matching `DEVKIT_ENFORCE_WRITES`. It is the user's own
/// environment, so it wins over a project layer's `enabled = false`.
const ENV_OVERRIDE: &str = "DEVKIT_HARNESS_LOG";

/// Every `[harness.log]` key, resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub command: Fidelity,
    pub prompt: PromptFidelity,
    pub auto_prune: bool,
    pub dir: PathBuf,
    /// `None` means unlimited. `0` is rejected at parse time rather than
    /// guessed at.
    pub max_age_days: Option<u32>,
    pub max_bytes: Option<u64>,
}

/// Where records land when the global config names no directory.
pub fn default_dir() -> PathBuf {
    paths::state_dir().join("harness-log")
}

/// Resolve the settings for a hook firing at `cwd`.
pub fn resolve(cwd: &Path) -> Settings {
    resolve_at(crate::harness::global_config_path().as_deref(), cwd)
}

/// [`resolve`] against an explicit global config path, so a caller — and a test
/// on a machine that has none — can say where the global layer is rather than
/// rearranging the environment to move it.
pub fn resolve_at(global_path: Option<&Path>, cwd: &Path) -> Settings {
    let global = global_path.and_then(|p| {
        let section = read_section(p)?;
        // A relative `dir` anchors to the directory that declared it, the way
        // every other path key resolves.
        Some(anchor_dir(section, p.parent()))
    });
    let layers = project_sections(cwd);
    let env = parse_env_override(std::env::var(ENV_OVERRIDE).ok().as_deref());
    resolve_from(global.as_ref(), &layers, env)
}

/// The global table is carried separately rather than read by index.
/// `resolve_rules` pushes a global layer only when `global_config_path()`
/// resolves and the file parses, so on a machine with no global config index 0
/// is a project layer, and an index-based read would let a project enable
/// logging and move its directory.
///
/// The boundary this draws is honest but narrow: `global_config_path()` honours
/// `DEVKIT_CONFIG`, which can point anywhere including into a repository. It is
/// a real boundary against a `devkit.toml` a project ships to its
/// contributors, and not against a user's own environment.
pub fn resolve_from(
    global: Option<&LogSection>,
    layers: &[LogSection],
    env: Option<bool>,
) -> Settings {
    let enabled_globally = global.and_then(|g| g.enabled).unwrap_or(false);
    let disabled_anywhere = layers.iter().any(|l| l.enabled == Some(false));
    let enabled = env.unwrap_or(enabled_globally && !disabled_anywhere);

    // Minimum is monotone and order-independent, so layer precedence stops
    // mattering for the two fidelity keys and no project can raise one from
    // anywhere in the chain.
    let command = layers.iter().filter_map(|l| l.command).fold(
        global.and_then(|g| g.command).unwrap_or(Fidelity::Redacted),
        std::cmp::min,
    );
    let prompt = layers.iter().filter_map(|l| l.prompt).fold(
        global.and_then(|g| g.prompt).unwrap_or(PromptFidelity::Off),
        std::cmp::min,
    );
    Settings {
        enabled,
        command,
        prompt,
        // `dir`, `auto_prune` and both caps read from `global` alone and
        // ignore `layers` entirely.
        auto_prune: global.and_then(|g| g.auto_prune).unwrap_or(true),
        dir: global
            .and_then(|g| g.dir.clone())
            .unwrap_or_else(default_dir),
        max_age_days: global.and_then(|g| g.max_age_days),
        max_bytes: global.and_then(|g| g.max_bytes),
    }
}

/// Read one layer's `[harness.log]`. Only that subtable is deserialised, so a
/// malformed sibling `[harness]` key cannot take this answer down, and a
/// malformed `log` table reads as absent rather than failing the hook.
fn read_section(path: &Path) -> Option<LogSection> {
    let body = std::fs::read_to_string(path).ok()?;
    section_in(&body)
}

fn section_in(body: &str) -> Option<LogSection> {
    let table: toml::Table = toml::from_str(body).ok()?;
    table.get("harness")?.get("log")?.clone().try_into().ok()
}

fn anchor_dir(mut section: LogSection, anchor: Option<&Path>) -> LogSection {
    if let Some(raw) = section.dir.take() {
        section.dir =
            devkit_config::resolve_host_path(&raw.to_string_lossy(), "harness.log.dir", anchor)
                .ok()
                .filter(|p| !p.as_os_str().is_empty());
    }
    section
}

/// Every project layer applying at `cwd`, lowest precedence first. Order does
/// not matter to any key here — the two a project may set resolve by `min` and
/// by `any` — but it is what `resolve_rules` produces, so the two agree.
fn project_sections(cwd: &Path) -> Vec<LogSection> {
    let main = crate::git::main_checkout(cwd).ok().flatten();
    let Ok(layers) = devkit_config::project_layers(cwd, main.as_deref()) else {
        return Vec::new();
    };
    layers
        .iter()
        .filter_map(|layer| Some(anchor_dir(read_section(&layer.path)?, layer.path.parent())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_global() -> LogSection {
        LogSection {
            enabled: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn a_project_layer_cannot_enable_logging() {
        let project = LogSection {
            enabled: Some(true),
            ..Default::default()
        };
        let s = resolve_from(None, &[project], None);
        assert!(!s.enabled, "only the global config turns logging on");
    }

    #[test]
    fn any_layer_can_disable_logging() {
        let project = LogSection {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(!resolve_from(Some(&enabled_global()), &[project], None).enabled);
    }

    #[test]
    fn a_project_layer_cannot_move_the_directory_or_the_caps() {
        let global = LogSection {
            enabled: Some(true),
            dir: Some("/global/logs".into()),
            max_bytes: Some(10),
            max_age_days: Some(7),
            auto_prune: Some(false),
            ..Default::default()
        };
        let project = LogSection {
            dir: Some("./.logs".into()),
            max_bytes: Some(1_000_000),
            max_age_days: Some(400),
            auto_prune: Some(true),
            ..Default::default()
        };
        let s = resolve_from(Some(&global), &[project], None);
        assert_eq!(s.dir, PathBuf::from("/global/logs"));
        assert_eq!(s.max_bytes, Some(10));
        assert_eq!(s.max_age_days, Some(7));
        assert!(!s.auto_prune);
    }

    #[test]
    fn fidelity_clamps_downward_in_any_order() {
        let global = LogSection {
            enabled: Some(true),
            command: Some(Fidelity::Full),
            ..Default::default()
        };
        let lower = LogSection {
            command: Some(Fidelity::Hashed),
            ..Default::default()
        };
        let mid = LogSection {
            command: Some(Fidelity::Redacted),
            ..Default::default()
        };
        let a = resolve_from(Some(&global), &[lower.clone(), mid.clone()], None).command;
        let b = resolve_from(Some(&global), &[mid, lower], None).command;
        assert_eq!(a, Fidelity::Hashed);
        assert_eq!(
            a, b,
            "min is order-independent, so layer precedence cannot matter here"
        );
    }

    #[test]
    fn a_project_layer_cannot_raise_either_fidelity() {
        let global = LogSection {
            enabled: Some(true),
            command: Some(Fidelity::Hashed),
            prompt: Some(PromptFidelity::Off),
            ..Default::default()
        };
        let greedy = LogSection {
            command: Some(Fidelity::Full),
            prompt: Some(PromptFidelity::Full),
            ..Default::default()
        };
        let s = resolve_from(Some(&global), &[greedy], None);
        assert_eq!(s.command, Fidelity::Hashed);
        assert_eq!(s.prompt, PromptFidelity::Off);
    }

    #[test]
    fn the_env_override_beats_a_project_disable() {
        let project = LogSection {
            enabled: Some(false),
            ..Default::default()
        };
        assert!(resolve_from(Some(&enabled_global()), &[project], Some(true)).enabled);
        assert!(
            !resolve_from(Some(&enabled_global()), &[], Some(false)).enabled,
            "and turns it off where the global config turned it on"
        );
    }

    #[test]
    fn neither_unsafe_mode_is_reached_by_accident() {
        let s = resolve_from(Some(&enabled_global()), &[], None);
        assert_eq!(s.command, Fidelity::Redacted);
        assert_eq!(s.prompt, PromptFidelity::Off);
        assert!(s.auto_prune, "a retention promise nothing enforces is none");
        assert_eq!(s.dir, default_dir());
        assert_eq!(s.max_age_days, None, "absent means unlimited");
    }

    /// The case the obvious implementation passes and the correct one is needed
    /// for: on a machine with no `~/.config/devkit/config.toml`, the global
    /// layer was never pushed, so index 0 is a project layer. A resolver that
    /// read the global-only keys by index would read a project's.
    #[test]
    fn a_machine_with_no_global_config_cannot_be_enabled_by_a_project() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("devkit.toml"),
            "[config]\nroot = true\n[harness.log]\nenabled = true\ncommand = \"full\"\ndir = \"./.logs\"\n",
        )
        .unwrap();
        let s = resolve_at(None, project.path());
        assert!(!s.enabled, "only the global config turns logging on");
        assert_eq!(s.command, Fidelity::Redacted, "and cannot raise fidelity");
        assert_eq!(s.dir, default_dir(), "or move the directory");
    }

    #[test]
    fn a_project_layer_reaches_the_resolver_at_all() {
        // The negative test above passes whether or not the layer was read, so
        // this pins that it was: the one thing a project layer may do.
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("devkit.toml"),
            "[config]\nroot = true\n[harness.log]\ncommand = \"hashed\"\n",
        )
        .unwrap();
        let global = tempfile::tempdir().unwrap();
        let global_path = global.path().join("config.toml");
        std::fs::write(
            &global_path,
            "[harness.log]\nenabled = true\ncommand = \"full\"\n",
        )
        .unwrap();
        let s = resolve_at(Some(&global_path), project.path());
        assert!(s.enabled);
        assert_eq!(s.command, Fidelity::Hashed);
    }

    #[test]
    fn a_relative_global_dir_anchors_to_the_config_that_declared_it() {
        let global = tempfile::tempdir().unwrap();
        let global_path = global.path().join("config.toml");
        std::fs::write(
            &global_path,
            "[harness.log]\nenabled = true\ndir = \"./logs\"\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        let s = resolve_at(Some(&global_path), project.path());
        assert_eq!(s.dir, global.path().join("logs"));
    }

    #[test]
    fn a_malformed_log_table_reads_as_absent_rather_than_failing() {
        // The hook cannot afford to fail over a config it cannot read, and a
        // record that never lands is the cost of a bad key here.
        assert!(section_in("[harness.log]\ncommand = \"loud\"\n").is_none());
        assert!(section_in("[[[ not toml").is_none());
        assert!(section_in("[harness]\nenforce_writes = true\n").is_none());
    }

    #[test]
    fn a_broken_sibling_key_does_not_take_the_log_table_down() {
        let s = section_in("[harness]\nshell = 42\n[harness.log]\nenabled = true\n")
            .expect("only the log subtable is deserialised");
        assert_eq!(s.enabled, Some(true));
    }
}
