//! `doppler_yaml` names a file inside the repository being worked on, so it
//! anchors to the checkout reading it: a worktree resolves its own copy, and
//! a branch that edits the app map takes effect without merging first.
//! `worktree_root` names a location on this machine and keeps anchoring to
//! the layer that declared it — even when that layer is a different
//! repository's main checkout.
//!
//! `start` sits two levels below the checkout root, so an implementation
//! that anchors `doppler_yaml` to `start` (or to `start`'s absolutized form)
//! instead of to the actual checkout root fails this test, even though it
//! would pass if `start` were the checkout root itself.
use std::path::Path;

#[test]
fn repository_relative_paths_anchor_to_the_checkout_root() {
    let main = tempfile::tempdir().unwrap();
    std::fs::write(
        main.path().join("devkit.toml"),
        "[defaults]\n\
         worktree_root = \"trees\"\n\
         branch_prefix = \"x/\"\n\
         baseline_ref = \"origin/main\"\n\
         baseline_path = \".\"\n\
         doppler_yaml = \"doppler.yaml\"\n",
    )
    .unwrap();

    let checkout = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(checkout.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    let start = checkout.path().join("apps/web");
    std::fs::create_dir_all(&start).unwrap();
    let checkout_root = devkit_common::git::checkout_root(&start).unwrap();

    let (cfg, _) = devkit_config::resolve(
        None,
        &start,
        Some(main.path()),
        Some(checkout_root.as_path()),
        None,
    )
    .unwrap();

    assert_eq!(
        Path::new(&cfg.defaults.doppler_yaml),
        checkout_root.join("doppler.yaml"),
        "repository-relative: anchors to the checkout root, not to `start`'s own subdirectory"
    );
    assert_eq!(
        Path::new(&cfg.defaults.worktree_root),
        main.path().join("trees"),
        "host path: still anchors to the declaring layer"
    );
    assert_eq!(
        Path::new(&cfg.defaults.baseline_path),
        main.path(),
        "host path: baseline_path anchors to the declaring layer too"
    );
}
