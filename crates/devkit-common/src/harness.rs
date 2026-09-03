//! Coding-agent harness glue shared by every devkit hook: the deny envelope,
//! and the per-checkout activation gate over the `[harness]` table.

use serde::Deserialize;
use serde_json::{Value, json};
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

#[derive(Deserialize, Default)]
struct Probe {
    #[serde(default)]
    harness: devkit_config::HarnessSection,
}

/// Read the named `[harness]` flag from a devkit-config TOML body. Parses
/// leniently — only the `[harness]` table is consulted, so a full project
/// config and a bare `[harness]`-only file both work; unparseable input reads
/// as off.
fn harness_flag_in(body: &str, flag: &str) -> bool {
    let Ok(p) = toml::from_str::<Probe>(body) else {
        return false;
    };
    match flag {
        "enforce_writes" => p.harness.enforce_writes,
        _ => false,
    }
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
}
