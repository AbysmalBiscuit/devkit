//! `project_layers` places the main checkout's config below the checkout's
//! own files and above every ancestor, and never lets a file reachable both
//! by name and by the ordinary upward walk contribute twice.

/// The main checkout's file sits beneath the worktree's own file, and rises
/// to the front once the worktree gains a config of its own.
#[test]
fn main_checkout_layer_sits_below_the_checkout() {
    let main = tempfile::tempdir().unwrap();
    std::fs::write(
        main.path().join("devkit.toml"),
        "[apps.web]\nlaunch = [\"main\"]\n",
    )
    .unwrap();

    let worktree = tempfile::tempdir().unwrap();

    let layers = devkit_config::project_layers(worktree.path(), Some(main.path())).unwrap();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].kind, devkit_config::LayerKind::MainCheckout);

    std::fs::write(
        worktree.path().join("devkit.toml"),
        "[apps.web]\nlaunch = [\"mine\"]\n",
    )
    .unwrap();
    let layers = devkit_config::project_layers(worktree.path(), Some(main.path())).unwrap();
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].kind, devkit_config::LayerKind::MainCheckout);
    assert_eq!(layers[1].kind, devkit_config::LayerKind::Checkout);
}

/// A worktree living beneath its own main checkout's directory tree reaches
/// that checkout's file both as `main_checkout` and by the ordinary upward
/// walk. Dedupe keeps the `Checkout` occurrence: the file is reachable
/// without the injected parameter at all, so it is the checkout's own — the
/// distinction a `--project` write target depends on — not one merely
/// inherited from elsewhere.
#[test]
fn a_nested_worktree_does_not_duplicate_the_main_layer() {
    let main = tempfile::tempdir().unwrap();
    std::fs::write(main.path().join("devkit.toml"), "").unwrap();
    let nested = main.path().join("worktrees/side");
    std::fs::create_dir_all(&nested).unwrap();

    let layers = devkit_config::project_layers(&nested, Some(main.path())).unwrap();
    assert_eq!(layers.len(), 1, "contributed once, not twice");
    assert_eq!(
        layers[0].kind,
        devkit_config::LayerKind::Checkout,
        "reachable by the upward walk, so it stays a writable checkout target"
    );
}
