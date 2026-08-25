use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "tag.gpgsign", "false"]);
    std::fs::write(dir.join("src/lib.rs"), "// v1").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "v1"]);
    git(dir, &["tag", "v1.0.0"]);
    std::fs::write(dir.join("src/lib.rs"), "// v2").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "v2"]);
    git(dir, &["tag", "v1.1.0"]);
}

fn docm_command(root: &Path, project: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_docm"));
    command
        .args(["path", "up"])
        .current_dir(project)
        .env("HOME", root.join("home"))
        .env("XDG_DATA_HOME", root.join("data"))
        // Without this, `docs_root` computes its legacy path from the
        // caller's cache home and moves a real store into the temp tree.
        .env("XDG_CACHE_HOME", root.join("xdg-cache"))
        .env_remove(devkit_docs::barrier::VAR);
    command
}

fn run_docm(root: &Path, project: &Path) -> Output {
    docm_command(root, project).output().unwrap()
}

/// `docs_root` migrates a legacy store by renaming it, so a harness that
/// leaves the cache home pointing at the caller's own relocates the real
/// store the moment the suite runs.
#[test]
fn the_harness_confines_the_cache_home_to_its_sandbox() {
    let root = Path::new("/sandbox-root");
    let command = docm_command(root, Path::new("/sandbox-root/project"));
    let cache_home = command
        .get_envs()
        .find(|(key, _)| *key == OsStr::new("XDG_CACHE_HOME"))
        .and_then(|(_, value)| value)
        .expect("harness must set XDG_CACHE_HOME");
    assert!(
        Path::new(cache_home).starts_with(root),
        "cache home {cache_home:?} escapes the sandbox at {}",
        root.display()
    );
}

#[test]
fn moved_tag_reporting_only_claims_success_after_repair() {
    let root = devkit_testtmp::dir("docm-reporting");
    let home = root.join("home");
    let data_home = root.join("data");
    let project = root.join("project");
    let upstream = root.join("upstream");
    std::fs::create_dir_all(home.join(".config/devkit")).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    fixture_repo(&upstream);
    std::fs::write(
        home.join(".config/devkit/docs.toml"),
        format!(
            "[[libs]]\nname = \"up\"\necosystem = \"git\"\nrepo = {:?}\nref = \"v1.0.0\"\n",
            upstream.to_string_lossy()
        ),
    )
    .unwrap();

    let initial = run_docm(&root, &project);
    assert!(initial.status.success());
    let checkout = std::path::PathBuf::from(
        String::from_utf8(initial.stdout)
            .unwrap()
            .trim()
            .to_string(),
    );

    git(&upstream, &["tag", "-f", "v1.0.0", "v1.1.0"]);
    let cache_root = data_home.join("devkit/docs");
    devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .fetch()
        .unwrap();
    let repaired = run_docm(&root, &project);
    assert!(repaired.status.success());
    assert!(
        String::from_utf8_lossy(&repaired.stderr).contains("re-pointed"),
        "successful repair report missing: {}",
        String::from_utf8_lossy(&repaired.stderr)
    );

    git(&upstream, &["tag", "-f", "v1.0.0", "HEAD~1"]);
    devkit_docs::cache::LibCache::new(&cache_root, "up")
        .unwrap()
        .fetch()
        .unwrap();
    std::fs::write(checkout.join("src/lib.rs"), "// local change").unwrap();
    let failed = run_docm(&root, &project);
    assert!(!failed.status.success());
    assert!(
        !String::from_utf8_lossy(&failed.stderr).contains("re-pointed"),
        "failed repair reported success: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
}
