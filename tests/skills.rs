//! Every shipped skill loads, and every reference it carries is reachable.
//!
//! A harness skips a skill whose frontmatter it cannot read without saying so,
//! and an agent only opens a reference file that its `SKILL.md` names. Whether
//! an agent then does what a skill says is `evals/scenario.sh`'s job.

use std::{
    fs,
    path::{Path, PathBuf},
};

const SKILLS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/plugin/skills");

fn skill_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<_> = fs::read_dir(SKILLS)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "no skills under {SKILLS}");
    dirs
}

fn skill_md(dir: &Path) -> String {
    let path = dir.join("SKILL.md");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn frontmatter(dir: &Path) -> serde_yaml_ng::Value {
    let body = skill_md(dir);
    let yaml = body
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map(|(yaml, _)| yaml)
        .unwrap_or_else(|| panic!("{}: SKILL.md opens with no frontmatter", dir.display()));
    serde_yaml_ng::from_str(yaml).unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
}

/// The limits the Agent Skills specification sets, which every harness this
/// plugin installs into enforces.
#[test]
fn every_skill_has_a_valid_name_and_description() {
    for dir in skill_dirs() {
        let meta = frontmatter(&dir);
        let dir_name = dir.file_name().unwrap().to_str().unwrap();

        let name = meta["name"].as_str().unwrap_or_default();
        assert_eq!(
            name, dir_name,
            "{dir_name}: `name` must match the directory"
        );
        assert!(
            (1..=64).contains(&name.len())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !name.starts_with('-')
                && !name.ends_with('-')
                && !name.contains("--"),
            "{dir_name}: `name` must be 1-64 lowercase letters, digits and single inner hyphens"
        );

        let description = meta["description"].as_str().unwrap_or_default();
        assert!(
            (1..=1024).contains(&description.chars().count()),
            "{dir_name}: `description` must be 1-1024 characters"
        );
    }
}

#[test]
fn every_reference_is_named_in_its_skill() {
    for dir in skill_dirs() {
        let Ok(entries) = fs::read_dir(dir.join("references")) else {
            continue;
        };
        let body = skill_md(&dir);
        for entry in entries {
            let file = entry.unwrap().file_name();
            let link = format!("references/{}", file.to_str().unwrap());
            assert!(
                body.contains(&link),
                "{}: SKILL.md never names {link}, so no agent reads it",
                dir.display()
            );
        }
    }
}
