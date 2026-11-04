//! A relative `--config` or `$DEVKIT_CONFIG` must resolve its layer-relative
//! paths exactly as the upward walk does. `worktree_root` is a ports-registry
//! holder identity and a prefix-match key, so a value that still depends on the
//! caller's working directory is a correctness fault, not untidiness.
//!
//! The bodies change the current directory, which only their own child sees.

use std::path::{Path, PathBuf};

use devkit_git::child_env::child_env;
use tempfile::TempDir;

/// `[config] root = true` keeps `~/.config/devkit/config.toml` out of the
/// discovered case, so all three resolutions see exactly this one layer.
const BODY: &str = "[config]\n\
                    root = true\n\
                    [defaults]\n\
                    worktree_root = \"../proj-worktrees\"\n\
                    branch_prefix = \"lev/\"\n\
                    baseline_ref = \"origin/main\"\n";

/// Run `check` with the current directory in a new project, passing the project
/// and the `worktree_root` its config must resolve to.
fn in_project(check: impl FnOnce(&Path, &str)) {
    let tmp: TempDir = tempfile::tempdir().unwrap();
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

    check(&proj, &expected);

    // The temp dir is removed when it drops, so step out of it first.
    std::env::set_current_dir(PathBuf::from("/")).unwrap();
}

#[test]
fn a_relative_config_path_resolves_like_the_discovered_one() {
    child_env().remove("DEVKIT_CONFIG").run(|| {
        in_project(|proj, expected| {
            let (discovered, _) =
                devkit_config::resolve(None, proj, None, None, None, &Default::default()).unwrap();
            assert_eq!(discovered.defaults.worktree_root, expected);

            let (explicit, prov) = devkit_config::resolve(
                Some(Path::new("devkit.toml")),
                proj,
                None,
                None,
                None,
                &Default::default(),
            )
            .unwrap();
            assert_eq!(
                explicit.defaults.worktree_root, expected,
                "a relative --config resolves like the discovered config"
            );
            assert!(
                prov.layers.iter().all(|l| l.is_absolute()),
                "every recorded layer path is absolute: {:?}",
                prov.layers
            );
        });
    });
}

#[test]
fn a_relative_devkit_config_variable_resolves_like_the_discovered_one() {
    child_env().set("DEVKIT_CONFIG", "devkit.toml").run(|| {
        in_project(|proj, expected| {
            let (from_env, _) =
                devkit_config::resolve(None, proj, None, None, None, &Default::default()).unwrap();
            assert_eq!(
                from_env.defaults.worktree_root, expected,
                "a relative $DEVKIT_CONFIG resolves like the discovered config"
            );
        });
    });
}
