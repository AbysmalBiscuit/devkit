//! The `[harness]` table: the coding-agent enforcement opt-ins, and the
//! `[harness.log]` table underneath it.

use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

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

/// What a logged command carries. Ordered least to most revealing, so
/// resolving across config layers is `min` and no layer can raise it.
#[derive(
    Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Fidelity {
    /// A stable digest and nothing else.
    Hashed,
    /// The text, with known token shapes substituted by kind.
    Redacted,
    /// The text verbatim.
    Full,
}

/// What a logged prompt carries. Same ordering rule as [`Fidelity`], with an
/// `off` below every level of it: a corpus can carry full command text without
/// carrying what the human typed.
#[derive(
    Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum PromptFidelity {
    /// Record that a prompt was submitted, and none of its text.
    Off,
    Hashed,
    Redacted,
    Full,
}

/// Harness logging: what agents tried to run, and what devkit decided about
/// it. Off unless the global config turns it on.
///
/// `enabled = true`, `dir`, `auto_prune`, `max_age_days` and `max_bytes` are
/// read from `~/.config/devkit/config.toml` alone and ignored wherever else
/// they appear. A project layer may do exactly two things: set
/// `enabled = false`, and lower `command` or `prompt`. Everything a project
/// layer can do tightens. `dir` is on that list for the same reason
/// `enabled = true` is: a project layer setting `dir = "./.logs"` would land
/// command text inside the checkout.
///
/// ```
/// # use devkit_config::{Fidelity, HarnessSection, PromptFidelity};
/// # let doc: toml::Table = toml::from_str(r#"
/// [harness.log]
/// enabled      = true              # global config only
/// command      = "redacted"        # full | redacted | hashed
/// prompt       = "off"             # off | hashed | redacted | full
/// auto_prune   = true              # global config only
/// dir          = "${HOME}/logs/devkit" # global only; defaults under state_dir
/// max_age_days = 30                # global only; absent means unlimited
/// max_bytes    = 2000000000        # global only; absent means unlimited
/// # "#).unwrap();
/// # let h: HarnessSection = doc["harness"].clone().try_into().unwrap();
/// # assert_eq!(h.log.enabled, Some(true));
/// # assert_eq!(h.log.command, Some(Fidelity::Redacted));
/// # assert_eq!(h.log.prompt, Some(PromptFidelity::Off));
/// # assert_eq!(h.log.max_age_days, Some(30));
/// ```
///
/// Every leaf is `Option` so a layer that set nothing is distinguishable from
/// one that set the default, which is what the downward clamp needs.
#[derive(Deserialize, Serialize, Debug, Clone, Default, PartialEq, schemars::JsonSchema)]
#[serde(default)]
pub struct LogSection {
    /// Turn logging on. Read from the global config alone.
    pub enabled: Option<bool>,
    /// How much of a command's text a record carries.
    pub command: Option<Fidelity>,
    /// How much of a submitted prompt a record carries.
    pub prompt: Option<PromptFidelity>,
    /// Sweep the log directory at session end. Global config only.
    pub auto_prune: Option<bool>,
    /// Where records land. Global config only; defaults under the state dir.
    pub dir: Option<PathBuf>,
    /// Days of records to keep. Global config only; absent means unlimited.
    #[serde(deserialize_with = "nonzero_u32")]
    pub max_age_days: Option<u32>,
    /// Bytes of records to keep. Global config only; absent means unlimited.
    #[serde(deserialize_with = "nonzero_u64")]
    pub max_bytes: Option<u64>,
}

/// Both retention caps reject `0` by name rather than guessing at it. Unlimited
/// and delete-everything are both plausible readings of a zero cap, and they
/// differ by the whole corpus.
fn nonzero_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    match Option::<u32>::deserialize(d)? {
        Some(0) => Err(D::Error::custom(
            "max_age_days = 0 is ambiguous: omit the key for unlimited retention",
        )),
        other => Ok(other),
    }
}

fn nonzero_u64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    match Option::<u64>::deserialize(d)? {
        Some(0) => Err(D::Error::custom(
            "max_bytes = 0 is ambiguous: omit the key for unlimited retention",
        )),
        other => Ok(other),
    }
}

/// One `[harness.commands.<name>]` entry: a set of programs whose invocation
/// the guard refuses, and the correction it offers instead.
///
/// Deliberately not a regex. Parsing the command, unwrapping wrappers and
/// runners, and removing a program's own global options (`git -C`) are
/// devkit's job; a rule that had to restate them would get them wrong.
///
/// ```
/// # use devkit_config::{CommandRule, RuleAction, Severity};
/// # let doc: toml::Table = toml::from_str(r#"
/// [harness.commands.git-worktree]
/// programs = ["git"]
/// args     = ["worktree", "add"]
/// reason   = "use `issue setup <id>`, which records the worktree"
///
/// [harness.commands.nitro-dev]
/// programs = ["bun", "node"]
/// args     = ["**", "*nitro", "dev"] # bun --cwd . nitro dev, node node_modules/.bin/nitro dev
/// reason   = "start the api with `devrun up api`"
/// # "#).unwrap();
/// # let rule: CommandRule =
/// #     doc["harness"]["commands"]["git-worktree"].clone().try_into().unwrap();
/// # assert_eq!(rule.args, ["worktree", "add"]);
/// # assert!(rule.enabled);
/// # assert_eq!(rule.action, RuleAction::Block);
/// # assert_eq!(rule.severity, Severity::Error);
/// # let glob: CommandRule =
/// #     doc["harness"]["commands"]["nitro-dev"].clone().try_into().unwrap();
/// # assert_eq!(glob.args, ["**", "*nitro", "dev"]);
/// ```
///
/// `args` matches the typed arguments after the program's own global options
/// are removed, so `git -C /repo worktree add` fires the first rule and
/// `git worktree list` does not. In the second rule, `**` skips any flags
/// before the server and `*nitro` matches the binary at any path, so it
/// refuses `bun --filter=api nitro dev` and allows `bun nitro build`.
#[derive(Deserialize, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct CommandRule {
    /// Program names this rule refuses, matched against the segment's command
    /// word by basename. An empty list matches nothing, which is how a child
    /// layer exempts a subtree from a rule its parent declared.
    #[serde(default)]
    pub programs: Vec<String>,
    /// Arguments that must appear, in order, at the head of the typed
    /// arguments for the rule to fire. Empty matches any arguments. `*`
    /// matches any run of characters, `/` included, within one argument; an
    /// entry that is exactly `"**"` matches zero or more whole arguments. A
    /// leading `"**"` also skips a script path, so `["**", "*nitro", "dev"]`
    /// fires on `node server.js --name nitro dev` too.
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
///
/// ```
/// # use devkit_config::AppMatch;
/// # let doc: toml::Table = toml::from_str(r#"
/// [harness.app_match]
/// fuzzy     = false   # name no app rather than guess one
/// min_score = 75
/// # "#).unwrap();
/// # let m: AppMatch = doc["harness"]["app_match"].clone().try_into().unwrap();
/// # assert!(!m.fuzzy);
/// # assert_eq!(m.min_score, 75);
/// # assert_eq!(m.max_typos, 1);
/// ```
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
///
/// ```
/// # use devkit_config::{HarnessSection, PolicyAction};
/// # let doc: toml::Table = toml::from_str(r#"
/// [harness]
/// enforce_writes   = true
/// enforce_commands = true
/// script_files     = "warn"
/// # "#).unwrap();
/// # let h: HarnessSection = doc["harness"].clone().try_into().unwrap();
/// # assert!(h.enforce_writes);
/// # assert_eq!(h.script_files, PolicyAction::Warn);
/// # assert_eq!(h.unresolved_writes, PolicyAction::Block);
/// ```
///
/// The same two lines in `~/.config/devkit/config.toml` enforce across every
/// checkout, with no per-project file at all.
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
    /// What devkit records about the events a harness sends it.
    #[serde(default)]
    pub log: LogSection,
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
            log: LogSection::default(),
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

    #[test]
    fn fidelity_orders_from_least_to_most_revealing() {
        assert!(Fidelity::Hashed < Fidelity::Redacted);
        assert!(Fidelity::Redacted < Fidelity::Full);
        assert!(PromptFidelity::Off < PromptFidelity::Hashed);
        assert!(PromptFidelity::Hashed < PromptFidelity::Redacted);
        assert!(PromptFidelity::Redacted < PromptFidelity::Full);
    }

    #[test]
    fn a_zero_cap_is_rejected_rather_than_guessed() {
        let err = toml::from_str::<HarnessSection>("[log]\nmax_age_days = 0\n").unwrap_err();
        assert!(format!("{err}").contains("max_age_days"), "{err}");
        let err = toml::from_str::<HarnessSection>("[log]\nmax_bytes = 0\n").unwrap_err();
        assert!(format!("{err}").contains("max_bytes"), "{err}");
    }

    #[test]
    fn an_unset_key_stays_none() {
        let h: HarnessSection = toml::from_str("[log]\nenabled = true\n").unwrap();
        assert_eq!(h.log.enabled, Some(true));
        assert_eq!(
            h.log.command, None,
            "an unset key must not read as its default"
        );
        assert_eq!(h.log.max_age_days, None, "absent means unlimited");
    }

    #[test]
    fn logging_is_absent_by_default() {
        assert_eq!(HarnessSection::default().log, LogSection::default());
        assert_eq!(LogSection::default().enabled, None);
    }
}
