//! One test in its own binary. It changes the process's current directory and
//! sets `DEVKIT_CONFIG`, both of which every other config test would see.

use std::path::{Path, PathBuf};

/// `[config] root = true` keeps `~/.config/devkit/config.toml` out of the
/// discovered case, so all three resolutions see exactly this one layer.
const BODY: &str = "[config]\n\
                    root = true\n\
                    [defaults]\n\
                    worktree_root = \"../proj-worktrees\"\n\
                    branch_prefix = \"lev/\"\n\
                    baseline_ref = \"origin/main\"\n\
                    baseline_path = \"\"\n";

/// A relative `--config` or `$DEVKIT_CONFIG` must resolve its layer-relative
/// paths exactly as the upward walk does. `worktree_root` is a ports-registry
/// holder identity and a prefix-match key, so a value that still depends on the
/// caller's working directory is a correctness fault, not untidiness.
#[test]
fn a_relative_config_path_resolves_like_the_discovered_one() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("devkit.toml"), BODY).unwrap();

    // Every spelling here comes from the directory the process reports rather
    // than the one `tempfile` handed back: macOS resolves a `/var/folders/...`
    // temp dir through the `/private/var` symlink once it is the current
    // directory, so a `tmp`-derived expectation would name a path the relative
    // resolutions never produce.
    std::env::set_current_dir(&proj).unwrap();
    let proj = std::env::current_dir().unwrap();
    let expected = proj
        .parent()
        .unwrap()
        .join("proj-worktrees")
        .to_string_lossy()
        .into_owned();

    let (discovered, _) = devkit_config::resolve(None, &proj, None).unwrap();
    assert_eq!(discovered.defaults.worktree_root, expected);

    let (explicit, prov) =
        devkit_config::resolve(Some(Path::new("devkit.toml")), &proj, None).unwrap();
    assert_eq!(
        explicit.defaults.worktree_root, expected,
        "a relative --config resolves like the discovered config"
    );
    assert!(
        prov.layers.iter().all(|l| l.is_absolute()),
        "every recorded layer path is absolute: {:?}",
        prov.layers
    );

    unsafe { std::env::set_var("DEVKIT_CONFIG", "devkit.toml") };
    let (from_env, _) = devkit_config::resolve(None, &proj, None).unwrap();
    assert_eq!(
        from_env.defaults.worktree_root, expected,
        "a relative $DEVKIT_CONFIG resolves like the discovered config"
    );

    // The temp dir is removed when it drops, so step out of it first.
    std::env::set_current_dir(PathBuf::from("/")).unwrap();
}
