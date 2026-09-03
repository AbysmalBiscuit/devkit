//! Coding-agent harness glue shared by every devkit hook: the deny envelope,
//! and the per-checkout activation gate over the `[harness]` table.

use devkit_config::{AppMatch, CommandRule};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The Claude Code / Codex `PreToolUse` deny envelope. `reason` reaches the agent.
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

/// The global devkit config file: `$DEVKIT_CONFIG`, else `~/.config/devkit/config.toml`.
/// Mirrors the `~/.config/devkit/config.toml` base layer the resolver loads, so the
/// harness reads the same global config the other binaries do.
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
fn harness_enabled(cwd: &Path, flag: &str) -> bool {
    let main = crate::git::main_checkout(cwd).ok().flatten();
    let Ok(layers) = devkit_config::project_layers(cwd, main.as_deref()) else {
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
/// Takes the working directory rather than a pre-resolved checkout root, so a
/// declaration in a directory between the root and the action is part of the
/// answer.
pub fn enforcement_enabled(cwd: &Path, flag: &str, env_var: &str) -> bool {
    resolve_enforcement(
        parse_env_override(std::env::var(env_var).ok().as_deref()),
        || harness_enabled(cwd, flag),
        || global_harness_enabled(flag),
    )
}

/// The merged `[harness]` tables the command guard reads. The two enforcement
/// flags are not here: they ratchet on with `any` across layers rather than
/// merging by precedence, and `enforcement_enabled` already owns that.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct HarnessRules {
    pub commands: BTreeMap<String, CommandRule>,
    pub app_match: AppMatch,
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
    let (merged, _, _) = devkit_config::merge_layers(&projected);
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
pub fn resolve_rules(cwd: &Path) -> (HarnessRules, Vec<String>) {
    let mut layers: Vec<(PathBuf, toml::Table)> = Vec::new();
    if let Some(p) = global_config_path()
        && let Ok(body) = std::fs::read_to_string(&p)
        && let Ok(t) = toml::from_str::<toml::Table>(&body)
    {
        layers.push((p, t));
    }
    let main = crate::git::main_checkout(cwd).ok().flatten();
    if let Ok(project) = devkit_config::project_layers(cwd, main.as_deref()) {
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

/// Whether the command guard is active for a command originating at `cwd`.
pub fn commands_enabled(cwd: &Path) -> bool {
    enforcement_enabled(cwd, "enforce_commands", "DEVKIT_ENFORCE_COMMANDS")
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
        assert!(harness_enabled(dir.path(), "enforce_writes"));
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[harness]\nenforce_writes = false\n",
        )
        .unwrap();
        assert!(!harness_enabled(dir.path(), "enforce_writes"));
        std::fs::write(
            dir.path().join("devkit.toml"),
            "[defaults]\nworktree_root = \"x\"\n",
        )
        .unwrap();
        assert!(!harness_enabled(dir.path(), "enforce_writes")); // missing section → off, despite unrelated keys
        let _ = std::fs::remove_file(dir.path().join("devkit.toml"));
        assert!(!harness_enabled(dir.path(), "enforce_writes")); // no devkit.toml → off
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

        assert!(harness_enabled(&nested, "enforce_writes"));
        assert!(!harness_enabled(repo.path(), "enforce_writes"));
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

        assert!(harness_enabled(&linked, "enforce_writes"));
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
}
