//! Coding-agent harness glue shared by every devkit hook: the deny envelope,
//! and the per-checkout activation gate over the `[harness]` table.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use devkit_config::{AppMatch, CommandRule, PolicyAction, ShellSetting};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::git::Checkout;

/// The Claude Code / Codex `PreToolUse` deny envelope. `reason` reaches the
/// agent.
pub fn deny_json(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
}

/// Read one `[harness]` flag from a config body.
///
/// Parses to a `toml::Table` and reads the single key, so a malformed or
/// unrelated sibling key cannot change this answer. A body that is not valid
/// TOML, a missing table, and a key of the wrong type all read as off.
pub fn harness_flag_in(body: &str, flag: &str) -> bool {
    toml::from_str::<toml::Table>(body)
        .ok()
        .and_then(|t| t.get("harness")?.get(flag)?.as_bool())
        .unwrap_or(false)
}

/// Parse an enforcement env override into an explicit on/off, or `None` when
/// unset/blank/unrecognized. Case- and whitespace-insensitive.
pub fn parse_env_override(val: Option<&str>) -> Option<bool> {
    match val.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on") => Some(true),
        Some(v) if matches!(v.as_str(), "0" | "false" | "no" | "off") => Some(false),
        _ => None,
    }
}

/// The global devkit config file: `$DEVKIT_CONFIG`, else
/// `~/.config/devkit/config.toml`. Mirrors the `~/.config/devkit/config.toml`
/// base layer the resolver loads, so the harness reads the same global config
/// the other binaries do.
pub fn global_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DEVKIT_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/devkit/config.toml"))
}

/// Combine the enforcement opt-in sources. The env override is an explicit
/// on/off master switch; without it, enforcement is on when either a project
/// layer or the global config opts in. `checkout` and `global` are thunks
/// because each does filesystem and, for `checkout`, git work that an explicit
/// override must not pay for.
pub fn resolve_enforcement(
    env: Option<bool>,
    checkout: impl FnOnce() -> bool,
    global: impl FnOnce() -> bool,
) -> bool {
    match env {
        Some(v) => v,
        None => checkout() || global(),
    }
}

/// True iff any project layer applying at `cwd` sets `[harness] <flag>`.
/// Combined with `any` rather than by precedence: enforcement ratchets on, and
/// only the env override turns it off, so one layer opting in must win even if
/// a closer layer leaves the flag unset.
fn harness_enabled(checkout: &Checkout, cwd: &Path, flag: &str) -> bool {
    let Ok(layers) = devkit_config::project_layers(cwd, checkout.main_checkout()) else {
        return false;
    };
    layers.iter().any(|layer| {
        std::fs::read_to_string(&layer.path)
            .map(|b| harness_flag_in(&b, flag))
            .unwrap_or(false)
    })
}

fn global_harness_enabled(flag: &str) -> bool {
    global_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|b| harness_flag_in(&b, flag))
        .unwrap_or(false)
}

/// Whether the named `[harness]` flag is active for an action originating at
/// `cwd`, across the env override, the project layers, and the global config.
/// Takes the working directory as well as the checkout, so a declaration in a
/// directory between the root and the action is part of the answer.
///
/// The checkout stays behind the thunk `resolve_enforcement` takes: an
/// explicit env override answers on its own, and a [`Checkout`] resolves
/// lazily, so a harness switched off by environment still spawns no git.
pub fn enforcement_enabled_in(checkout: &Checkout, cwd: &Path, flag: &str, env_var: &str) -> bool {
    resolve_enforcement(
        parse_env_override(std::env::var(env_var).ok().as_deref()),
        || harness_enabled(checkout, cwd, flag),
        || global_harness_enabled(flag),
    )
}

/// [`enforcement_enabled_in`] for a caller with no checkout of its own.
pub fn enforcement_enabled(cwd: &Path, flag: &str, env_var: &str) -> bool {
    enforcement_enabled_in(&Checkout::at(cwd), cwd, flag, env_var)
}

/// The policy keys the shell hook reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessPolicy {
    pub shell: ShellSetting,
    pub unresolved_writes: PolicyAction,
    pub unsupported_language: PolicyAction,
    pub script_files: PolicyAction,
}

impl Default for HarnessPolicy {
    fn default() -> Self {
        Self {
            shell: ShellSetting::Auto,
            unresolved_writes: PolicyAction::Block,
            unsupported_language: PolicyAction::Block,
            script_files: PolicyAction::Allow,
        }
    }
}

/// The merged `[harness]` tables and policy keys the shell hook reads. The two
/// enforcement flags are not here: they ratchet on across layers rather than
/// merging by precedence, and `enforcement_enabled` owns that.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HarnessRules {
    pub commands: BTreeMap<String, CommandRule>,
    pub app_match: AppMatch,
    pub policy: HarnessPolicy,
}

/// Merge the `[harness]` tables of parsed layers, lowest precedence first, and
/// deserialize each merged rule.
///
/// Routed through `devkit_config::merge_layers` rather than a hand-rolled
/// merge, so these inherit exactly the way every other config table does: a
/// child adds names, and a same-named rule overrides only the keys it sets.
/// Returns what survived alongside a warning per piece that did not.
pub fn merge_rules(layers: &[(PathBuf, toml::Table)]) -> (HarnessRules, Vec<String>) {
    let projected: Vec<_> = layers
        .iter()
        .map(|(p, t)| {
            let harness = t
                .get("harness")
                .and_then(toml::Value::as_table)
                .cloned()
                .unwrap_or_default();
            (p.clone(), harness)
        })
        .collect();
    let (merged, ..) = devkit_config::merge_layers(&projected);
    let mut warnings = Vec::new();

    // Each key is deserialized on its own, so one that will not parse costs
    // only itself. A bad `app_match` degrades to the defaults rather than
    // failing the command: the guard is fail-open throughout.
    let app_match = match merged.get("app_match") {
        None => AppMatch::default(),
        Some(v) => v.clone().try_into::<AppMatch>().unwrap_or_else(|e| {
            warnings.push(format!("ignoring `[harness.app_match]`: {e}"));
            AppMatch::default()
        }),
    };

    fn key<T: DeserializeOwned>(
        merged: &toml::Table,
        name: &str,
        default: T,
        warnings: &mut Vec<String>,
    ) -> T {
        match merged.get(name) {
            None => default,
            Some(v) => v.clone().try_into::<T>().unwrap_or_else(|e| {
                warnings.push(format!("ignoring `[harness] {name}`: {e}"));
                default
            }),
        }
    }
    let defaults = HarnessPolicy::default();
    let policy = HarnessPolicy {
        shell: key(&merged, "shell", defaults.shell, &mut warnings),
        unresolved_writes: key(
            &merged,
            "unresolved_writes",
            defaults.unresolved_writes,
            &mut warnings,
        ),
        unsupported_language: key(
            &merged,
            "unsupported_language",
            defaults.unsupported_language,
            &mut warnings,
        ),
        script_files: key(
            &merged,
            "script_files",
            defaults.script_files,
            &mut warnings,
        ),
    };

    let mut commands = BTreeMap::new();
    match merged.get("commands") {
        None => {}
        Some(v) => match v.as_table() {
            Some(table) => {
                for (name, value) in table {
                    match value.clone().try_into::<CommandRule>() {
                        Ok(rule) if rule.programs.is_empty() && !value_names_programs(value) => {
                            warnings.push(format!(
                                "skipping `[harness.commands.{name}]`: no `programs`"
                            ));
                        }
                        Ok(rule) => {
                            commands.insert(name.clone(), rule);
                        }
                        Err(e) => {
                            warnings.push(format!("skipping `[harness.commands.{name}]`: {e}"))
                        }
                    }
                }
            }
            // Not a table at all: every inherited rule is lost, so this must
            // warn rather than silently empty the map, the same way a
            // malformed `app_match` does above.
            None => warnings.push(format!(
                "ignoring `[harness.commands]`: expected a table, found {}",
                v.type_str()
            )),
        },
    }
    (
        HarnessRules {
            commands,
            app_match,
            policy,
        },
        warnings,
    )
}

/// Whether a merged rule table set `programs` at all. An explicit empty list is
/// a deliberate exemption and is kept; an absent key is an incomplete rule and
/// is skipped.
fn value_names_programs(v: &toml::Value) -> bool {
    v.as_table().is_some_and(|t| t.contains_key("programs"))
}

/// The merged `[harness]` command-guard tables applying at `cwd`, lowest
/// precedence first: the global config, then every project layer.
///
/// Warnings are returned rather than printed. This runs inside the shared gate,
/// which `lockm hook pretooluse` also calls, and a rule warning printed here
/// would fire on every `Edit` as well as every `Bash`.
pub fn resolve_rules_in(checkout: &Checkout, cwd: &Path) -> (HarnessRules, Vec<String>) {
    let mut layers: Vec<(PathBuf, toml::Table)> = Vec::new();
    if let Some(p) = global_config_path()
        && let Ok(body) = std::fs::read_to_string(&p)
        && let Ok(t) = toml::from_str::<toml::Table>(&body)
    {
        layers.push((p, t));
    }
    if let Ok(project) = devkit_config::project_layers(cwd, checkout.main_checkout()) {
        for layer in project {
            if let Ok(body) = std::fs::read_to_string(&layer.path)
                && let Ok(t) = toml::from_str::<toml::Table>(&body)
            {
                layers.push((layer.path, t));
            }
        }
    }
    merge_rules(&layers)
}

/// [`resolve_rules_in`] for a caller with no checkout of its own.
pub fn resolve_rules(cwd: &Path) -> (HarnessRules, Vec<String>) {
    resolve_rules_in(&Checkout::at(cwd), cwd)
}

/// Whether the command guard is active for a command originating at `cwd`.
pub fn commands_enabled(checkout: &Checkout, cwd: &Path) -> bool {
    enforcement_enabled_in(checkout, cwd, "enforce_commands", "DEVKIT_ENFORCE_COMMANDS")
}

/// Which harness sent a payload, and therefore which envelope answers it and
/// whether the write stage may run for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    ClaudeCode,
    Codex,
    Cursor,
}

/// Tool names whose `tool_input.command` is a shell command. `Shell` is
/// Cursor's spelling on its generic `preToolUse`.
pub const SHELL_TOOLS: [&str; 3] = ["Bash", "PowerShell", "Shell"];

/// A pre-execution shell payload. Fields the harness did not send stay
/// `None`; nothing here is filled in from the hook process's environment.
#[derive(Debug, Clone)]
pub struct ShellPayload {
    pub harness: Harness,
    pub tool_name: Option<String>,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
}

/// Read a pre-execution shell payload. `None` when the event is not about a
/// shell command, which is not a failure: harnesses send events this hook does
/// not model.
///
/// All three harnesses send `hook_event_name`, and Cursor sends `model` as
/// well, so identity is read from a field each one *sends* rather than one it
/// omits: `cursor_version` is Cursor's, and `turn_id` or `model` without it is
/// Codex's. Positive evidence does not rot when a vendor adds a key, which the
/// earlier absence-based reading did the moment Cursor started sending both.
///
/// This is the fallback. A hook invoked from one of devkit's own manifests is
/// told which harness it is answering, and that wins over anything inferred
/// here.
pub fn infer_harness(p: &Value) -> Harness {
    if p.get("cursor_version").is_some() {
        Harness::Cursor
    } else if p.get("turn_id").is_some() || p.get("model").is_some() {
        Harness::Codex
    } else {
        Harness::ClaudeCode
    }
}

pub fn parse_shell_payload(p: &Value) -> Option<ShellPayload> {
    let text = |key: &str| {
        p.get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let harness = infer_harness(p);
    let tool_name = text("tool_name");
    if harness != Harness::Cursor
        && !tool_name
            .as_deref()
            .is_some_and(|tool| SHELL_TOOLS.contains(&tool))
    {
        return None;
    }
    let command = p
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| p.get("tool_input")?.get("command")?.as_str())
        .filter(|s| !s.trim().is_empty())?
        .to_string();
    // Cursor names the directory inside the tool input and the session a
    // conversation, so each identity field falls back to Cursor's spelling of
    // it. The order is harness-independent: no harness sends both.
    let working_dir = p
        .get("tool_input")
        .and_then(|ti| ti.get("working_directory"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    Some(ShellPayload {
        harness,
        tool_name,
        command,
        cwd: text("cwd").map(PathBuf::from).or(working_dir),
        session_id: text("session_id").or_else(|| text("conversation_id")),
        agent_id: text("agent_id").or_else(|| text("parent_conversation_id")),
    })
}

/// The deny envelope this harness reads. Cursor's reason goes in
/// `agent_message`: `user_message` is shown to the human, and only the agent
/// message reaches the agent, which is the whole point of handing back a
/// command it can retry.
pub fn deny_shell_json(harness: Harness, reason: &str) -> Value {
    match harness {
        Harness::ClaudeCode | Harness::Codex => deny_json(reason),
        Harness::Cursor => json!({
            "permission": "deny",
            "agent_message": reason,
            "continue": true
        }),
    }
}

/// An allow that carries text to the agent, or `None` where the harness has no
/// such channel and a warning can only be a silent allow. No
/// `permissionDecision` is sent: an explicit allow would also bypass the
/// user's own permission prompt.
pub fn warn_shell_json(harness: Harness, context: &str) -> Option<Value> {
    match harness {
        Harness::ClaudeCode | Harness::Codex => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": context
            }
        })),
        Harness::Cursor => None,
    }
}

/// Whether the shell hook's write stage is active for a command at `cwd`.
pub fn writes_enabled(checkout: &Checkout, cwd: &Path) -> bool {
    enforcement_enabled_in(checkout, cwd, "enforce_writes", "DEVKIT_ENFORCE_WRITES")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_beats_both_file_sources() {
        assert!(!resolve_enforcement(Some(false), || true, || true));
        assert!(resolve_enforcement(Some(true), || false, || false));
    }

    #[test]
    fn either_file_source_enables() {
        assert!(resolve_enforcement(None, || true, || false));
        assert!(resolve_enforcement(None, || false, || true));
        assert!(!resolve_enforcement(None, || false, || false));
    }

    #[test]
    fn env_override_parses_both_spellings() {
        assert_eq!(parse_env_override(Some(" ON ")), Some(true));
        assert_eq!(parse_env_override(Some("0")), Some(false));
        assert_eq!(parse_env_override(Some("maybe")), None);
        assert_eq!(parse_env_override(None), None);
    }

    /// An explicit override must answer without evaluating either opt-in
    /// source: neither thunk may run once `env` is `Some`, or an enforcement
    /// check pays for a layer walk and a git spawn it has no need of.
    #[test]
    fn resolve_enforcement_short_circuits_on_an_explicit_override() {
        assert!(resolve_enforcement(
            Some(true),
            || panic!("checkout thunk ran despite an explicit override"),
            || panic!("global thunk ran despite an explicit override")
        ));
        assert!(!resolve_enforcement(
            Some(false),
            || panic!("checkout thunk ran despite an explicit override"),
            || panic!("global thunk ran despite an explicit override")
        ));
    }

    #[test]
    fn harness_enabled_reads_flag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[harness]\nenforce_writes = true\n",
        )
        .unwrap();
        assert!(harness_enabled(
            &Checkout::at(dir.path()),
            dir.path(),
            "enforce_writes"
        ));
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[harness]\nenforce_writes = false\n",
        )
        .unwrap();
        assert!(!harness_enabled(
            &Checkout::at(dir.path()),
            dir.path(),
            "enforce_writes"
        ));
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[defaults]\nworktree_root = \"x\"\n",
        )
        .unwrap();
        assert!(!harness_enabled(
            &Checkout::at(dir.path()),
            dir.path(),
            "enforce_writes"
        )); // missing section → off, despite unrelated keys
        let _ = std::fs::remove_file(dir.path().join("devkit.toml"));
        assert!(!harness_enabled(
            &Checkout::at(dir.path()),
            dir.path(),
            "enforce_writes"
        )); // no devkit.toml → off
    }

    #[test]
    fn harness_flag_in_reads_section_leniently() {
        assert!(harness_flag_in(
            "[harness]\nenforce_writes = true\n",
            "enforce_writes"
        ));
        assert!(!harness_flag_in(
            "[harness]\nenforce_writes = false\n",
            "enforce_writes"
        ));
        // full project config carrying the flag still reads true
        assert!(harness_flag_in(
            "[defaults]\nworktree_root = \"x\"\n[harness]\nenforce_writes = true\n",
            "enforce_writes"
        ));
        // no [harness] section, or junk → off (never panics)
        assert!(!harness_flag_in(
            "[defaults]\nworktree_root = \"x\"\n",
            "enforce_writes"
        ));
        assert!(!harness_flag_in("not even toml [", "enforce_writes"));
    }

    /// A directory between the checkout root and the write is part of the
    /// layer stack: a harness declaration there must be seen.
    #[test]
    fn harness_declared_in_a_nested_directory_is_honored() {
        let repo = tempfile::tempdir().unwrap();
        crate::git::Git::fixture(repo.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(repo.path().join("devkit.toml"), "").unwrap();
        let nested = repo.path().join("packages/thing");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("devkit.local.toml"),
            "[harness]\nenforce_writes = true\n",
        )
        .unwrap();

        assert!(harness_enabled(
            &Checkout::at(&nested),
            &nested,
            "enforce_writes"
        ));
        assert!(!harness_enabled(
            &Checkout::at(repo.path()),
            repo.path(),
            "enforce_writes"
        ));
    }

    /// A linked worktree inherits its main checkout's `[harness]` declaration:
    /// `harness_enabled` must see it even though the worktree itself carries
    /// no `devkit.toml` of its own. The config is written into the main
    /// checkout only after the worktree exists, and is never committed, so
    /// nothing about `git worktree add` could have copied it into the
    /// worktree — a pass here can only come from inheritance.
    #[test]
    fn harness_is_inherited_from_the_main_checkout() {
        let main = tempfile::tempdir().unwrap();
        crate::git::Git::fixture(main.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(main.path().join("f.txt"), "x\n").unwrap();
        crate::git::Git::fixture(main.path())
            .args(["add", "."])
            .output()
            .unwrap();
        crate::git::Git::fixture(main.path())
            .args(["commit", "-qm", "init"])
            .output()
            .unwrap();

        let holder = tempfile::tempdir().unwrap();
        let linked = holder.path().join("wt");
        crate::git::Git::fixture(main.path())
            .args([
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "side",
            ])
            .output()
            .unwrap();

        std::fs::write(
            main.path().join("devkit.toml"),
            "[harness]\nenforce_writes = true\n",
        )
        .unwrap();

        assert!(harness_enabled(
            &Checkout::at(&linked),
            &linked,
            "enforce_writes"
        ));
    }

    #[test]
    fn a_malformed_sibling_key_does_not_disable_the_flag() {
        let body = r#"
[harness]
enforce_writes = true

[harness.commands.bun-only]
programs = "node"
"#;
        assert!(harness_flag_in(body, "enforce_writes"));
    }

    #[test]
    fn a_wrong_typed_flag_reads_as_off() {
        assert!(!harness_flag_in(
            "[harness]\nenforce_writes = \"yes\"\n",
            "enforce_writes"
        ));
    }

    #[test]
    fn a_syntax_error_reads_as_off() {
        assert!(!harness_flag_in("[[[", "enforce_writes"));
    }

    #[test]
    fn an_absent_table_reads_as_off() {
        assert!(!harness_flag_in(
            "[defaults]\napps_dir = \"apps\"\n",
            "enforce_writes"
        ));
    }

    fn layer(name: &str, body: &str) -> (PathBuf, toml::Table) {
        (
            PathBuf::from(name),
            toml::from_str(body).expect("layer parses"),
        )
    }

    #[test]
    fn a_child_layer_adds_a_rule_and_keeps_the_parents() {
        let (h, warns) = merge_rules(&[
            layer(
                "root",
                "[harness.commands.bun-only]\nprograms = [\"node\"]\nreason = \"use bun\"\n",
            ),
            layer(
                "child",
                "[harness.commands.no-curl]\nprograms = [\"curl\"]\nreason = \"use ureq\"\n",
            ),
        ]);
        assert_eq!(h.commands.len(), 2);
        assert!(warns.is_empty());
        assert_eq!(h.commands["bun-only"].reason, "use bun");
        assert_eq!(h.commands["no-curl"].programs, vec!["curl"]);
    }

    #[test]
    fn a_same_named_child_rule_overrides_only_the_keys_it_sets() {
        let (h, _) = merge_rules(&[
            layer(
                "root",
                "[harness.commands.bun-only]\nprograms = [\"node\"]\nreason = \"use bun\"\n",
            ),
            layer("child", "[harness.commands.bun-only]\nprograms = []\n"),
        ]);
        assert!(h.commands["bun-only"].programs.is_empty());
        assert_eq!(h.commands["bun-only"].reason, "use bun");
    }

    #[test]
    fn a_rule_with_no_programs_after_merging_is_skipped_with_a_warning() {
        let (h, warns) =
            merge_rules(&[layer("root", "[harness.commands.oops]\nreason = \"hi\"\n")]);
        assert!(h.commands.is_empty());
        assert_eq!(warns.len(), 1);
        assert!(
            warns[0].contains("oops"),
            "warning names the rule: {}",
            warns[0]
        );
    }

    #[test]
    fn a_non_table_commands_value_is_ignored_with_a_warning() {
        let (h, warns) = merge_rules(&[layer("root", "[harness]\ncommands = \"oops\"\n")]);
        assert!(h.commands.is_empty());
        assert_eq!(warns.len(), 1);
        assert!(
            warns[0].contains("commands"),
            "warning names the table: {}",
            warns[0]
        );
    }

    #[test]
    fn a_malformed_rule_is_skipped_and_its_siblings_survive() {
        let (h, warns) = merge_rules(&[layer(
            "root",
            "[harness.commands.bad]\nprograms = \"node\"\n\
             [harness.commands.good]\nprograms = [\"curl\"]\nreason = \"use ureq\"\n",
        )]);
        assert_eq!(h.commands.len(), 1);
        assert!(h.commands.contains_key("good"));
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("bad"));
    }

    #[test]
    fn an_absent_app_match_is_the_default() {
        let (h, warns) = merge_rules(&[layer("root", "[harness]\nenforce_commands = true\n")]);
        assert!(warns.is_empty());
        assert_eq!(h.app_match, devkit_config::AppMatch::default());
    }

    #[test]
    fn app_match_merges_key_by_key_across_layers() {
        let (h, warns) = merge_rules(&[
            layer(
                "root",
                "[harness.app_match]\nmax_typos = 2\nmin_score = 40\n",
            ),
            layer("child", "[harness.app_match]\nmin_score = 80\n"),
        ]);
        assert!(warns.is_empty());
        assert_eq!(h.app_match.max_typos, 2, "inherited from the parent layer");
        assert_eq!(h.app_match.min_score, 80, "the child's own value wins");
        assert!(
            h.app_match.fuzzy,
            "a key neither layer sets keeps its default"
        );
    }

    #[test]
    fn a_malformed_app_match_falls_back_to_the_defaults_with_a_warning() {
        let (h, warns) = merge_rules(&[layer(
            "root",
            "[harness.app_match]\nmax_typos = \"lots\"\n\
             [harness.commands.good]\nprograms = [\"curl\"]\nreason = \"use ureq\"\n",
        )]);
        assert_eq!(h.app_match, devkit_config::AppMatch::default());
        assert!(
            h.commands.contains_key("good"),
            "a bad app_match spares its siblings"
        );
        assert_eq!(warns.len(), 1);
        assert!(
            warns[0].contains("app_match"),
            "warning names the table: {}",
            warns[0]
        );
    }

    #[test]
    fn a_claude_code_payload_carries_its_command_under_tool_input() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": "vite dev" },
            "cwd": "/repo"
        });
        let parsed = parse_shell_payload(&p).expect("a Bash payload parses");
        assert_eq!(parsed.harness, Harness::ClaudeCode);
        assert_eq!(parsed.command, "vite dev");
        assert_eq!(parsed.cwd.unwrap(), std::path::Path::new("/repo"));
    }

    #[test]
    fn a_cursor_payload_carries_its_command_at_the_top_level() {
        let p = serde_json::json!({
            "hook_event_name": "beforeShellExecution",
            "cursor_version": "1.7.0",
            "command": "vite dev",
            "cwd": "/repo"
        });
        let parsed = parse_shell_payload(&p).expect("a Cursor payload parses");
        assert_eq!(parsed.harness, Harness::Cursor);
        assert_eq!(parsed.command, "vite dev");
    }

    /// The payload that misresolved: Cursor sends `hook_event_name` and `model`
    /// like the other two, so reading it as the absence of either answered it
    /// with an envelope Cursor rejects.
    #[test]
    fn a_cursor_payload_is_not_codex() {
        let p = serde_json::json!({
            "hook_event_name": "preToolUse",
            "cursor_version": "1.7.0",
            "model": "claude-4.5-sonnet",
            "conversation_id": "c1",
            "tool_name": "Shell",
            "tool_input": {"command": "npm install", "working_directory": "/w"}
        });
        let parsed = parse_shell_payload(&p).expect("a Shell payload is a shell payload");
        assert_eq!(parsed.harness, Harness::Cursor);
        assert_eq!(parsed.session_id.as_deref(), Some("c1"));
        assert_eq!(parsed.cwd.as_deref(), Some(std::path::Path::new("/w")));
    }

    #[test]
    fn a_cursor_subagent_is_named_by_its_parent_conversation() {
        let p = serde_json::json!({
            "hook_event_name": "preToolUse",
            "cursor_version": "1.7.0",
            "conversation_id": "c1",
            "parent_conversation_id": "p1",
            "tool_name": "Shell",
            "tool_input": {"command": "ls"}
        });
        let parsed = parse_shell_payload(&p).unwrap();
        assert_eq!(parsed.agent_id.as_deref(), Some("p1"));
    }

    #[test]
    fn codex_is_still_codex() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse", "turn_id": "t1", "model": "gpt-5",
            "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": "/w"
        });
        assert_eq!(parse_shell_payload(&p).unwrap().harness, Harness::Codex);
    }

    #[test]
    fn claude_code_is_still_claude_code() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": "/w"
        });
        assert_eq!(
            parse_shell_payload(&p).unwrap().harness,
            Harness::ClaudeCode
        );
    }

    /// `cwd` wins where a harness sends both, so the Cursor fallback cannot
    /// displace the field the other two send.
    #[test]
    fn an_explicit_cwd_beats_the_tool_inputs_working_directory() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse", "tool_name": "Bash", "cwd": "/repo",
            "tool_input": {"command": "ls", "working_directory": "/elsewhere"}
        });
        assert_eq!(
            parse_shell_payload(&p).unwrap().cwd.as_deref(),
            Some(std::path::Path::new("/repo"))
        );
    }

    #[test]
    fn a_non_bash_tool_is_not_a_shell_payload() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/a.rs" }
        });
        assert!(parse_shell_payload(&p).is_none());
    }

    #[test]
    fn a_payload_with_no_command_is_not_a_shell_payload() {
        assert!(parse_shell_payload(&serde_json::json!({ "cwd": "/repo" })).is_none());
    }

    #[test]
    fn a_codex_payload_is_told_apart_by_its_turn_fields() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse", "tool_name": "Bash", "turn_id": "t1", "model": "m",
            "session_id": "S", "tool_input": { "command": "ls" }, "cwd": "/repo"
        });
        let parsed = parse_shell_payload(&p).unwrap();
        assert_eq!(parsed.harness, Harness::Codex);
        assert_eq!(parsed.session_id.as_deref(), Some("S"));
    }

    #[test]
    fn claude_codes_powershell_tool_is_a_shell_payload() {
        let p = serde_json::json!({
            "hook_event_name": "PreToolUse", "tool_name": "PowerShell", "prompt_id": "p",
            "session_id": "S", "agent_id": "a1", "tool_input": { "command": "Get-ChildItem" }
        });
        let parsed = parse_shell_payload(&p).unwrap();
        assert_eq!(parsed.harness, Harness::ClaudeCode);
        assert_eq!(parsed.tool_name.as_deref(), Some("PowerShell"));
        assert_eq!(parsed.agent_id.as_deref(), Some("a1"));
    }

    #[test]
    fn missing_identity_stays_missing() {
        let p = serde_json::json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": { "command": "ls" } });
        let parsed = parse_shell_payload(&p).unwrap();
        assert!(parsed.session_id.is_none() && parsed.cwd.is_none());
    }

    #[test]
    fn the_warning_envelope_allows_without_a_permission_decision() {
        let w = warn_shell_json(Harness::ClaudeCode, "not checked").unwrap();
        assert_eq!(w["hookSpecificOutput"]["additionalContext"], "not checked");
        assert!(w["hookSpecificOutput"].get("permissionDecision").is_none());
        assert!(warn_shell_json(Harness::Cursor, "not checked").is_none());
        assert_eq!(
            deny_shell_json(Harness::Codex, "no")["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
    }

    #[test]
    fn the_closest_layer_sets_each_policy_key() {
        let (h, warns) = merge_rules(&[
            layer(
                "global",
                "[harness]\nunresolved_writes = \"warn\"\nscript_files = \"block\"\n",
            ),
            layer("project", "[harness]\nunresolved_writes = \"allow\"\n"),
        ]);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(
            h.policy.unresolved_writes,
            devkit_config::PolicyAction::Allow
        );
        assert_eq!(h.policy.script_files, devkit_config::PolicyAction::Block);
        assert_eq!(
            h.policy.unsupported_language,
            devkit_config::PolicyAction::Block
        );
    }

    #[test]
    fn an_invalid_policy_value_warns_and_keeps_that_keys_default_only() {
        let (h, warns) = merge_rules(&[layer(
            "root",
            "[harness]\nunresolved_writes = \"sometimes\"\nscript_files = \"warn\"\n[harness.commands.bad]\nprograms = \"git\"\n",
        )]);
        assert_eq!(
            h.policy.unresolved_writes,
            devkit_config::PolicyAction::Block
        );
        assert_eq!(h.policy.script_files, devkit_config::PolicyAction::Warn);
        assert_eq!(warns.len(), 2, "{warns:?}");
    }

    #[test]
    fn a_child_layer_disables_an_inherited_rule_without_repeating_it() {
        let (h, _) = merge_rules(&[
            layer(
                "global",
                "[harness.commands.no-node]\nprograms = [\"node\"]\nreason = \"use bun\"\n",
            ),
            layer("project", "[harness.commands.no-node]\nenabled = false\n"),
        ]);
        assert!(!h.commands["no-node"].enabled);
        assert_eq!(h.commands["no-node"].programs, vec!["node"]);
    }

    #[test]
    fn each_harness_gets_its_own_deny_envelope() {
        let cc = deny_shell_json(Harness::ClaudeCode, "use devrun");
        assert_eq!(cc["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            cc["hookSpecificOutput"]["permissionDecisionReason"],
            "use devrun"
        );

        let cur = deny_shell_json(Harness::Cursor, "use devrun");
        assert_eq!(cur["permission"], "deny");
        assert_eq!(cur["agent_message"], "use devrun");
    }
}
