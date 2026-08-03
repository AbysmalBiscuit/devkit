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

fn run_docm(home: &Path, data_home: &Path, project: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_docm"))
        .args(["path", "up"])
        .current_dir(project)
        .env("HOME", home)
        .env("XDG_DATA_HOME", data_home)
        .env_remove(devkit_docs::barrier::VAR)
        .output()
        .unwrap()
}

#[test]
fn moved_tag_reporting_only_claims_success_after_repair() {
    let root = std::env::temp_dir().join(format!("docm-reporting-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
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

    let initial = run_docm(&home, &data_home, &project);
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
    let repaired = run_docm(&home, &data_home, &project);
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
    let failed = run_docm(&home, &data_home, &project);
    assert!(!failed.status.success());
    assert!(
        !String::from_utf8_lossy(&failed.stderr).contains("re-pointed"),
        "failed repair reported success: {}",
        String::from_utf8_lossy(&failed.stderr)
    );
}
