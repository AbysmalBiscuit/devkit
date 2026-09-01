//! The `[hooks]` runner: render one command's argv, run it in a given
//! directory, and draw a progress step per command. Every hook key shares it.

use anyhow::{Context, Result};
use devkit_common::cmd::capture;
use devkit_common::progress::Steps;
use std::collections::BTreeMap;
use std::path::Path;

/// Width a hook's command is elided to in its progress step: sized for an
/// 80-column terminal alongside the mark, the `[i/n]` counter, and the
/// elapsed time.
const HOOK_LABEL_MAX: usize = 56;

/// Render one hook's argv against `ctx`/`vars`.
fn render_hook(
    hook: &[String],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    hook.iter()
        .map(|part| {
            devkit_common::template::render(part, ctx, vars)
                .with_context(|| format!("rendering hook argument `{part}`"))
        })
        .collect()
}

/// Run an already-rendered hook argv in `cwd`.
fn run_rendered(cwd: &Path, argv: &[String]) -> Result<()> {
    let (prog, rest) = argv.split_first().context("empty hook command")?;
    capture(
        prog,
        &rest.iter().map(String::as_str).collect::<Vec<_>>(),
        cwd.to_str(),
    )?;
    Ok(())
}

/// The progress-step label for a hook command.
fn hook_label(argv: &[String]) -> String {
    format!(
        "Hook: {}",
        devkit_common::ui::truncate(
            &argv.join(" ").replace(['\n', '\r', '\t'], " "),
            HOOK_LABEL_MAX
        )
    )
}

/// Run each command in `hooks` in `cwd`, in order, one progress step each.
/// `key` names the config key the commands came from, for the warning a
/// failure prints. Fail-open: the state a hook reacts to has already happened
/// by the time it runs, so a hook that fails warns on stderr and the rest
/// still run.
pub(crate) fn run_all(
    cwd: &Path,
    key: &str,
    hooks: &[Vec<String>],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    steps: &Steps,
) {
    for hook in hooks {
        let rendered = render_hook(hook, ctx, vars);
        // A hook that cannot render still draws its step, labelled from the
        // template source so the offending argument is visible. The step
        // counter only advances inside `during_result`, so skipping the step
        // would leave the run ending short of its total.
        let label = hook_label(rendered.as_deref().unwrap_or(hook));
        if let Err(e) = steps.during_result(&label, || run_rendered(cwd, &rendered?)) {
            eprintln!("warning: {key} hook `{}` failed: {e:#}", hook.join(" "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> serde_json::Value {
        json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix", "apps": ["web"], "app": "web"})
    }

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn run(dir: &Path, hooks: &[Vec<String>], steps: &Steps) {
        run_all(
            dir,
            "after_worktree_create",
            hooks,
            &ctx(),
            &novars(),
            steps,
        );
    }

    #[test]
    fn render_hook_expands_each_argument() {
        let hook = vec![
            "git".to_string(),
            "init".to_string(),
            "{{ slug }}-wt".to_string(),
        ];
        let argv = render_hook(&hook, &ctx(), &novars()).unwrap();
        assert_eq!(argv, vec!["git", "init", "fix-wt"]);
    }

    #[test]
    fn render_hook_reports_the_argument_it_could_not_render() {
        let hook = vec!["git".to_string(), "{{ nope }}".to_string()];
        let err = render_hook(&hook, &ctx(), &novars()).unwrap_err();
        assert!(
            format!("{err:#}").contains("{{ nope }}"),
            "the error names the offending argument: {err:#}"
        );
    }

    #[test]
    fn hook_label_keeps_a_short_command_whole() {
        let argv = vec!["bun".to_string(), "install".to_string()];
        assert_eq!(hook_label(&argv), "Hook: bun install");
    }

    #[test]
    fn hook_label_elides_a_long_command() {
        let argv = vec!["bash".to_string(), "-c".to_string(), "x".repeat(200)];
        let label = hook_label(&argv);
        assert!(label.starts_with("Hook: bash -c "), "label was {label}");
        assert!(label.ends_with('…'), "label was {label}");
        assert_eq!(
            label.chars().count(),
            "Hook: ".chars().count() + HOOK_LABEL_MAX
        );
    }

    #[test]
    fn hook_label_collapses_embedded_newlines() {
        let argv = vec![
            "bash".to_string(),
            "-c".to_string(),
            "echo one\necho two".to_string(),
        ];
        let label = hook_label(&argv);
        assert!(!label.contains('\n'), "label was {label:?}");
        assert_eq!(label, "Hook: bash -c echo one\necho two".replace('\n', " "));
    }

    #[test]
    fn hook_renders_args_and_runs_in_the_given_directory() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![vec![
            "git".to_string(),
            "init".to_string(),
            "{{ slug }}-wt".to_string(),
        ]];
        run(dir.path(), &hooks, &Steps::persistent());
        assert!(dir.path().join("fix-wt").exists());
    }

    #[test]
    fn failing_hook_does_not_stop_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec!["devkit-no-such-program-xyz".to_string()],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        run(dir.path(), &hooks, &Steps::persistent());
        assert!(dir.path().join("after").exists());
    }

    #[test]
    fn unrenderable_hook_does_not_stop_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec![
                "git".to_string(),
                "init".to_string(),
                "{{ nope }}".to_string(),
            ],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        run(dir.path(), &hooks, &Steps::persistent());
        assert!(dir.path().join("after").exists());
    }

    #[test]
    fn every_hook_consumes_a_step_even_when_it_cannot_render() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec![
                "git".to_string(),
                "init".to_string(),
                "{{ nope }}".to_string(),
            ],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        let steps = Steps::persistent_with_total(hooks.len());
        run(dir.path(), &hooks, &steps);
        assert_eq!(
            steps.started(),
            2,
            "an unrenderable hook must still consume its step"
        );
        assert!(dir.path().join("after").exists(), "the next hook still ran");
    }
}
