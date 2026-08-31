//! `[parallelism] threads` reaches the shared pool through config resolution.
//!
//! Its own test binary, because the pool is process-global and built once: a
//! width assertion sharing a process with tests that build the pool for other
//! reasons would race them.

#[test]
fn resolving_a_config_sizes_the_shared_pool() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("devkit.toml"),
        "[config]\nroot = true\n[parallelism]\nthreads = 3\n",
    )
    .unwrap();

    devkit_common::config::resolve(None, &project).unwrap();

    // DEVKIT_THREADS outranks the config, so the expectation follows the same
    // precedence `width` does.
    let expected = std::env::var("DEVKIT_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(3);
    assert_eq!(devkit_common::pool::width(), expected);
}
