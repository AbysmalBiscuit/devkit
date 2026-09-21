//! The one `devkit.toml` example `docs/configuration.md` still carries.
//!
//! Per-table examples live on the config types as doctests. The whole-config
//! example stays in the reference, so this parses it through the same
//! `Config::parse` the doctests use.

use devkit_config::Config;

const REFERENCE: &str = include_str!("../docs/configuration.md");

/// The first fenced ```toml block after `heading`.
fn toml_block_under(heading: &str) -> String {
    let after = REFERENCE
        .split_once(heading)
        .unwrap_or_else(|| panic!("docs/configuration.md has no `{heading}` heading"))
        .1;
    let body = after
        .split_once("```toml\n")
        .unwrap_or_else(|| panic!("no toml example under `{heading}`"))
        .1;
    body.split_once("\n```")
        .expect("unterminated toml fence")
        .0
        .to_string()
}

#[test]
fn the_reference_example_is_a_config_devkit_accepts() {
    let cfg = Config::parse(&toml_block_under("\n## Example\n")).unwrap();

    // The shapes the example exists to demonstrate: a provider app other apps
    // wire to, an app whose directory is not its name, a prep file, a hook, and
    // a reviewer alias.
    assert!(cfg.apps["api"].provides_url);
    assert_eq!(cfg.apps["worker"].path.as_deref(), Some("services/worker"));
    assert_eq!(cfg.apps["web"].prep_files[0].path, ".env.local");
    assert_eq!(cfg.hooks.after_worktree_create[0][0], "zoxide");
    assert_eq!(cfg.people["alice"].github.as_deref(), Some("alice-gh"));
    assert_eq!(cfg.defaults.pr_base, "staging");
}
