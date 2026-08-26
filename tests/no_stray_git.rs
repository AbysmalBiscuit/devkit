//! One door to git. A second one reintroduces the environment redirect and the
//! unbounded wait that the module exists to prevent, so this is enforced rather
//! than documented.

use std::path::Path;

#[test]
fn git_is_only_spawned_by_the_git_module() {
    let offenders = scan(&["Command::new(\"git\")", "cmd::git(", "capture(\"git\""]);
    assert!(
        offenders.is_empty(),
        "git must be spawned only by devkit_common::git; found:\n{}",
        offenders.join("\n")
    );
}

fn scan(needles: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    walk(Path::new(env!("CARGO_MANIFEST_DIR")), &mut |path, body| {
        if path.ends_with("crates/devkit-common/src/git.rs")
            || path.ends_with("tests/no_stray_git.rs")
        {
            return;
        }
        for (n, line) in body.lines().enumerate() {
            if needles.iter().any(|needle| line.contains(needle)) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    });
    found
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path, &str)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "target" || name == ".git" || name == "docs" {
            continue;
        }
        if path.is_dir() {
            walk(&path, f);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(body) = std::fs::read_to_string(&path)
        {
            f(&path, &body);
        }
    }
}
