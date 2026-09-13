//! The `[harness]` table: the coding-agent enforcement opt-ins.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The shell whose syntax a hook command is read in. `auto` resolves from
/// the tool name and the harness; see `docs/configuration.md`.
#[derive(
    Deserialize, Serialize, Default, Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ShellSetting {
    #[default]
    Auto,
    Bash,
    Powershell,
}

/// What a policy finding does to the tool call.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PolicyAction {
    /// Deny the tool call with an actionable reason.
    Block,
    /// Allow it and tell the agent what was not checked.
    Warn,
    /// Allow it silently.
    Allow,
}

/// What a matching command rule does. A rule that should not act is disabled
/// with `enabled = false`, so there is no `allow`.
#[derive(
    Deserialize, Serialize, Default, Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    #[default]
    Block,
    Warn,
}

/// How a diagnostic is classified for the agent.
#[derive(
    Deserialize, Serialize, Default, Debug, Clone, Copy, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warning,
    #[default]
    Error,
}

/// One `[harness.commands.<name>]` entry: a set of programs whose invocation
/// the guard refuses, and the correction it offers instead.
///
/// Deliberately not a regex. Parsing the command, unwrapping wrappers and
/// runners, and removing a program's own global options (`git -C`) are
/// devkit's job; a rule that had to restate them would get them wrong.
#[derive(Deserialize, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct CommandRule {
    /// Program names this rule refuses, matched against the segment's command
    /// word by basename. An empty list matches nothing, which is how a child
    /// layer exempts a subtree from a rule its parent declared.
    #[serde(default)]
    pub programs: Vec<String>,
    /// Arguments that must appear, in order, at the head of the typed
    /// arguments for the rule to fire. Empty matches any arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Shown to the agent verbatim when the rule denies. Name the replacement
    /// command; the agent retries from this text.
    #[serde(default)]
    pub reason: String,
    /// `false` turns off a rule a parent layer declared.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    #[serde(default)]
    #[schemars(default)]
    pub action: RuleAction,
    #[serde(default)]
    #[schemars(default)]
    pub severity: Severity,
}

fn enabled_default() -> bool {
    true
}

impl Default for CommandRule {
    fn default() -> Self {
        Self {
            programs: Vec::new(),
            args: Vec::new(),
            reason: String::new(),
            enabled: true,
            action: RuleAction::Block,
            severity: Severity::Error,
        }
    }
}

/// `[harness.app_match]`: how the guard turns a hint into an app name.
///
/// Exact-name, exact-path and path-under-path matching are unconditional and
/// take no configuration. This table tunes only the fuzzy rung that runs when
/// none of those resolve, which is the one place the guard guesses.
///
/// `#[serde(default)]` sits on the container so a layer naming one key inherits
/// the other two rather than zeroing them.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq, schemars::JsonSchema)]
#[serde(default)]
pub struct AppMatch {
    /// Run the fuzzy matcher at all. `false` stops after exact and path
    /// matching, so an unrecognised hint names no app and the guard falls back
    /// to `devkit config apps`.
    pub fuzzy: bool,
    /// Substitutions, insertions and deletions the matcher forgives.
    ///
    /// One is what separates `lab-tools` from an app declared `lab_tools`.
    /// `frizbee::Config::default()` allows zero, which filters exactly that
    /// case, so this default is devkit's rather than the library's. Raising it
    /// buys confidently wrong app names.
    pub max_typos: u16,
    /// Below this score no app is named. Pointing an agent at another app's
    /// server is worse than naming none.
    pub min_score: u16,
}

impl Default for AppMatch {
    fn default() -> Self {
        Self {
            fuzzy: true,
            max_typos: 1,
            min_score: 60,
        }
    }
}

/// The `[harness]` table of a checkout's `devkit.toml`.
///
/// This is the shape `devkit schema` renders. Nothing at runtime deserializes
/// the table through it: the probe reads each key independently so one bad key
/// cannot take the others down with it.
#[derive(Deserialize, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct HarnessSection {
    /// Refuse writes to paths this checkout has not claimed with `lockm`.
    #[serde(default)]
    pub enforce_writes: bool,
    /// Refuse shell commands devkit already has a wired-up path for.
    #[serde(default)]
    pub enforce_commands: bool,
    /// The shell a hook command is read in.
    #[serde(default)]
    #[schemars(default)]
    pub shell: ShellSetting,
    /// A write whose target could not be determined.
    #[serde(default = "block")]
    #[schemars(default = "block")]
    pub unresolved_writes: PolicyAction,
    /// Executable source in a language devkit cannot analyze.
    #[serde(default = "block")]
    #[schemars(default = "block")]
    pub unsupported_language: PolicyAction,
    /// A call to a stored script, whose contents are not read.
    #[serde(default = "allow")]
    #[schemars(default = "allow")]
    pub script_files: PolicyAction,
    /// Extra refusals beyond the ones devkit derives from `[apps]` and
    /// `[tasks]`. Merged across config layers like every other table.
    #[serde(default)]
    pub commands: BTreeMap<String, CommandRule>,
    /// How the guard resolves a guarded command to one of `[apps]`.
    #[serde(default)]
    pub app_match: AppMatch,
}

fn block() -> PolicyAction {
    PolicyAction::Block
}

fn allow() -> PolicyAction {
    PolicyAction::Allow
}

impl Default for HarnessSection {
    fn default() -> Self {
        Self {
            enforce_writes: false,
            enforce_commands: false,
            shell: ShellSetting::Auto,
            unresolved_writes: PolicyAction::Block,
            unsupported_language: PolicyAction::Block,
            script_files: PolicyAction::Allow,
            commands: BTreeMap::new(),
            app_match: AppMatch::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_rule_keeps_its_behaviour() {
        let rule: CommandRule = toml::from_str(
            "programs = [\"git\"]\nargs = [\"worktree\", \"add\"]\nreason = \"use issue\"",
        )
        .unwrap();
        assert!(rule.enabled);
        assert_eq!(rule.action, RuleAction::Block);
        assert_eq!(rule.severity, Severity::Error);
    }

    #[test]
    fn a_rule_takes_an_action_severity_and_switch() {
        let rule: CommandRule = toml::from_str(
            "programs = [\"git\"]\nenabled = false\naction = \"warn\"\nseverity = \"info\"",
        )
        .unwrap();
        assert!(!rule.enabled);
        assert_eq!(rule.action, RuleAction::Warn);
        assert_eq!(rule.severity, Severity::Info);
    }

    #[test]
    fn allow_is_not_a_rule_action() {
        assert!(toml::from_str::<CommandRule>("programs = [\"git\"]\naction = \"allow\"").is_err());
    }

    #[test]
    fn the_policy_keys_default_conservatively() {
        let h = HarnessSection::default();
        assert_eq!(h.shell, ShellSetting::Auto);
        assert_eq!(h.unresolved_writes, PolicyAction::Block);
        assert_eq!(h.unsupported_language, PolicyAction::Block);
        assert_eq!(h.script_files, PolicyAction::Allow);
        let parsed: HarnessSection =
            toml::from_str("shell = \"powershell\"\nscript_files = \"warn\"").unwrap();
        assert_eq!(parsed.shell, ShellSetting::Powershell);
        assert_eq!(parsed.script_files, PolicyAction::Warn);
        assert_eq!(parsed.unresolved_writes, PolicyAction::Block);
    }
}
