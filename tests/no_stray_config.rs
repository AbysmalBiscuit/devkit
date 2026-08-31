//! One door to config resolution. `devkit_common::config::resolve` is where the
//! shared worker pool is sized from `[parallelism] threads`, so a second door
//! is a subcommand running the pool at a width its project did not ask for —
//! which is invisible until someone measures it. Enforced rather than
//! documented.

use std::path::Path;

#[test]
fn config_is_only_resolved_by_the_config_module() {
    let offenders = scan(&["devkit_config::resolve(", "config::resolve("]);
    assert!(
        offenders.is_empty(),
        "config must be resolved only through devkit_common::config::resolve; found:\n{}",
        offenders.join("\n")
    );
}

fn scan(needles: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    walk(Path::new(env!("CARGO_MANIFEST_DIR")), &mut |path, body| {
        // The door itself, and the crate whose own tests exercise the layer
        // discovery it wraps.
        if path.ends_with("crates/devkit-common/src/config.rs")
            || path.ends_with("tests/no_stray_config.rs")
            || path.components().any(|c| c.as_os_str() == "devkit-config")
        {
            return;
        }
        for (n, line) in body.lines().enumerate() {
            let goes_through_the_door = line.contains("devkit_common::config::resolve(")
                || line.contains("crate::config::resolve(");
            if goes_through_the_door {
                continue;
            }
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
