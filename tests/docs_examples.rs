//! The one `devkit.toml` example `docs/configuration.md` still carries.
//!
//! Every per-table example moved onto the config types, where a doctest parses
//! it. The full config stayed in the reference, because a whole config reads as
//! more than its tables do separately — and an example nothing checks is the
//! thing this issue set out to stop. So the document is read at test time and
//! the block under `## Example` goes through the same `Config::parse` every
//! doctest uses.

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
