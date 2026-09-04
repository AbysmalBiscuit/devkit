//! One test in its own binary. It sets `DEVKIT_CONFIG`, which every other test
//! sharing the process would see.

use devkit_common::harness::resolve_rules;

/// The global config is the lowest layer of the command guard's rule stack, so
/// a rule written there governs a project whose own `devkit.toml` says nothing
/// about `[harness.commands]`.
#[test]
fn a_global_config_rule_applies_in_a_project_that_declares_none() {
    let home = tempfile::tempdir().expect("home dir");
    let global = home.path().join("config.toml");
    std::fs::write(
        &global,
        "[harness.commands.bun-only]\nprograms = [\"node\"]\nreason = \"This workspace is bun-only.\"\n",
    )
    .expect("write the global config");

    let project = tempfile::tempdir().expect("project dir");
    std::fs::write(
        project.path().join("devkit.toml"),
        "[defaults]\napps_dir = \"apps\"\n",
    )
    .expect("write the project config");

    unsafe { std::env::set_var("DEVKIT_CONFIG", &global) };
    let (rules, warnings) = resolve_rules(project.path());

    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    let rule = rules
        .commands
        .get("bun-only")
        .expect("the global rule reaches a project that declares none");
    assert_eq!(rule.programs, vec!["node"]);
    assert_eq!(rule.reason, "This workspace is bun-only.");
}
