use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(60);

#[test]
fn the_lock_path_sits_under_the_reserved_stem_and_outside_the_library_dir() {
    let root = Path::new("/tmp/docm-lockpath");
    let p = devkit_docs::locks::lock_path(root, "@types/node").unwrap();
    assert_eq!(p, root.join("registry.locks").join("@types~node.lock"));
    assert!(devkit_docs::locks::is_control("registry.locks"));
    assert!(devkit_docs::locks::is_control("registry.json.tmp"));
    assert!(!devkit_docs::locks::is_control("registryfoo"));
}

/// Every fixed control file in the lock directory shares that directory with
/// the per-library lock files, whose names come from user input. A control
/// file whose stem a library could claim is one lock reached by two paths.
#[test]
fn no_library_can_claim_the_name_of_a_control_lock() {
    let root = Path::new("/tmp/docm-lockpath");
    let manifest_lock = devkit_docs::locks::manifest_lock_path(root);
    assert_eq!(
        manifest_lock.parent().unwrap(),
        devkit_docs::locks::control_dir(root)
    );
    let stem = manifest_lock.file_stem().unwrap().to_str().unwrap();
    assert!(
        devkit_docs::names::validate_lib(stem).is_err(),
        "a library named `{stem}` would lock {}",
        manifest_lock.display()
    );
}

/// `rm` takes the library lock and then the manifest lock. A library name that
/// resolves to the manifest lock's own path makes those one file, and the
/// advisory lock behind them is not reentrant.
#[test]
fn removing_a_library_named_after_a_control_lock_returns_instead_of_blocking() {
    let root =
        std::env::temp_dir().join(format!("devkit-docs-control-name-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let manifest = root.join("docs.toml");
    let cache_root = root.join("cache");

    let (finished, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let removed = devkit_docs::rm_library(
            devkit_docs::ManifestTarget::Global(&manifest),
            &cache_root,
            "manifest",
        );
        let _ = finished.send(removed.map_err(|error| format!("{error:#}")));
    });

    let removed = outcome
        .recv_timeout(PROCESS_TIMEOUT)
        .expect("`docm rm manifest` never returned");
    let error = removed.expect_err("a name that resolves to a control lock must be rejected");
    assert!(error.contains("manifest"), "{error}");
    assert!(error.contains("--package"), "{error}");
}

#[test]
fn a_long_library_name_is_rejected_before_the_lock_suffix_overflows() {
    let root = Path::new("/tmp/docm-lockpath");
    let long = "n".repeat(252);
    assert!(devkit_docs::locks::lock_path(root, &long).is_err());
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    while !path.try_exists().unwrap() {
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::yield_now();
    }
}

fn wait_for_either(first: &Path, second: &Path) -> bool {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if first.try_exists().unwrap() {
            return true;
        }
        if second.try_exists().unwrap() {
            return false;
        }
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for {} or {}",
            first.display(),
            second.display()
        );
        std::thread::yield_now();
    }
}

fn wait_for_child(mut child: Child, label: &str) -> Output {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("{label} worker timed out");
        }
        std::thread::yield_now();
    }
}

fn spawn_upsert_worker(
    exe: &Path,
    manifest: &Path,
    cache_root: &Path,
    barrier: &Path,
    name: &str,
) -> Child {
    Command::new(exe)
        .env("DEVKIT_DOCS_TEST_MANIFEST_UPSERT", name)
        .env("DEVKIT_DOCS_TEST_MANIFEST_PATH", manifest)
        .env("DEVKIT_DOCS_TEST_CACHE_ROOT", cache_root)
        .env(devkit_docs::barrier::VAR, barrier)
        .args([
            "--exact",
            "concurrent_adds_of_different_libraries_both_survive",
            "--nocapture",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn assert_worker_succeeded(output: Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn concurrent_adds_of_different_libraries_both_survive() {
    if let Ok(name) = std::env::var("DEVKIT_DOCS_TEST_MANIFEST_UPSERT") {
        let manifest = std::env::var_os("DEVKIT_DOCS_TEST_MANIFEST_PATH")
            .map(std::path::PathBuf::from)
            .unwrap();
        let cache_root = std::env::var_os("DEVKIT_DOCS_TEST_CACHE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap();
        let entry = devkit_docs::manifest::LibEntry {
            name,
            ..Default::default()
        };
        devkit_docs::manifest::upsert_global(&manifest, &entry, &cache_root).unwrap();
        return;
    }

    let root =
        std::env::temp_dir().join(format!("devkit-docs-manifest-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let manifest = root.join("docs.toml");
    let cache_root = root.join("cache");
    let first_barrier = root.join("first");
    let second_barrier = root.join("second");
    let exe = std::env::current_exe().unwrap();

    let first = spawn_upsert_worker(&exe, &manifest, &cache_root, &first_barrier, "alpha");
    wait_for(&first_barrier.with_extension("manifest-ready"));

    let second = spawn_upsert_worker(&exe, &manifest, &cache_root, &second_barrier, "beta");
    let second_ready = second_barrier.with_extension("manifest-ready");
    let second_contended = second_barrier.with_extension("contended.manifest");
    if wait_for_either(&second_ready, &second_contended) {
        std::fs::write(second_barrier.with_extension("manifest-go"), "").unwrap();
        assert_worker_succeeded(wait_for_child(second, "second"), "second");
        std::fs::write(first_barrier.with_extension("manifest-go"), "").unwrap();
        assert_worker_succeeded(wait_for_child(first, "first"), "first");
    } else {
        std::fs::write(first_barrier.with_extension("manifest-go"), "").unwrap();
        assert_worker_succeeded(wait_for_child(first, "first"), "first");
        wait_for(&second_ready);
        std::fs::write(second_barrier.with_extension("manifest-go"), "").unwrap();
        assert_worker_succeeded(wait_for_child(second, "second"), "second");
    }

    let manifest = devkit_docs::manifest::load_global(&manifest).unwrap();
    let mut names: Vec<&str> = manifest
        .libs
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["alpha", "beta"], "manifest lost an entry");
}
