//! `harness_log`: what agents tried to run, and what devkit decided about it.
//!
//! Off by default and enablable only from the global config. The module owns
//! the record types, redaction, the writer and the prune sweep behind one
//! infallible entry point, so a logging failure can never reach a hook's
//! verdict.
//!
//! It lives in `devkit-common` rather than a crate of its own because it needs
//! `paths::state_dir` and `secrets`.

use std::path::{Path, PathBuf};

use devkit_config::{Fidelity, LogSection, PromptFidelity};

use crate::{harness::parse_env_override, paths};

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
