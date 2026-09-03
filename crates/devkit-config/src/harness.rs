//! The `[harness]` table: the coding-agent enforcement opt-ins.

use serde::Deserialize;
use std::collections::BTreeMap;

/// One `[harness.commands.<name>]` entry: a set of programs whose invocation
/// the guard refuses, and the correction it offers instead.
///
/// Deliberately not a regex. Splitting the command into segments, stripping
/// runner prefixes, and anchoring the command word are devkit's job; a rule
/// that had to restate them would get them wrong.
#[derive(Deserialize, Default, Debug, Clone, PartialEq, schemars::JsonSchema)]
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
#[derive(Deserialize, Default, Debug, Clone, PartialEq, schemars::JsonSchema)]
pub struct HarnessSection {
    /// Refuse writes to paths this checkout has not claimed with `lockm`.
    #[serde(default)]
    pub enforce_writes: bool,
    /// Refuse shell commands devkit already has a wired-up path for.
    #[serde(default)]
    pub enforce_commands: bool,
    /// Extra refusals beyond the ones devkit derives from `[apps]` and
    /// `[tasks]`. Merged across config layers like every other table.
    #[serde(default)]
    pub commands: BTreeMap<String, CommandRule>,
    /// How the guard resolves a guarded command to one of `[apps]`.
    #[serde(default)]
    pub app_match: AppMatch,
}
