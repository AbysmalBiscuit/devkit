//! Every shipped skill loads, every reference it carries is reachable, and its
//! committed `agents/openai.yaml` matches its frontmatter.
//!
//! A harness skips a skill whose frontmatter it cannot read without saying so,
//! and an agent only opens a reference file that its `SKILL.md` names. Codex
//! reads invocation policy only from `agents/openai.yaml`, ignoring
//! `disable-model-invocation`. Whether an agent then does what a skill says is
//! `evals/scenario.sh`'s job.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

const SKILLS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/plugin/skills");

/// Every directory whose skills Codex can load: the ones the plugin ships and
/// the ones this repository's own agents use.
const CODEX_SKILL_ROOTS: [&str; 2] = [
    SKILLS,
    concat!(env!("CARGO_MANIFEST_DIR"), "/.agents/skills"),
];

const REGENERATE: &str = "DEVKIT_UPDATE_OPENAI_YAML=1 cargo test --test skills";

fn skill_dirs() -> Vec<PathBuf> {
    skill_dirs_under(Path::new(SKILLS))
}

fn skill_dirs_under(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<_> = fs::read_dir(root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "no skills under {}", root.display());
    dirs
}

fn skill_md(dir: &Path) -> String {
    let path = dir.join("SKILL.md");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn frontmatter<T: DeserializeOwned>(dir: &Path) -> T {
    let body = skill_md(dir);
    let yaml = body
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map(|(yaml, _)| yaml)
        .unwrap_or_else(|| panic!("{}: SKILL.md opens with no frontmatter", dir.display()));
    serde_yaml_ng::from_str(yaml).unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
}

#[derive(Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(rename = "disable-model-invocation", default)]
    disable_model_invocation: bool,
}

#[derive(Serialize)]
struct OpenAiYaml {
    interface: Interface,
    policy: Policy,
}

#[derive(Serialize)]
struct Interface {
    display_name: String,
    short_description: String,
}

#[derive(Serialize)]
struct Policy {
    allow_implicit_invocation: bool,
}

fn openai_yaml(dir: &Path) -> String {
    let meta: SkillFrontmatter = frontmatter(dir);
    serde_yaml_ng::to_string(&OpenAiYaml {
        interface: Interface {
            display_name: meta.name,
            short_description: meta.description,
        },
        policy: Policy {
            allow_implicit_invocation: !meta.disable_model_invocation,
        },
    })
    .unwrap()
}

/// Compares each skill's committed `agents/openai.yaml` with the one its
/// frontmatter generates. With `update`, rewrites the stale ones; without, an
/// error lists them and names the command that regenerates them.
fn sync_openai_yaml(roots: &[&Path], update: bool) -> Result<(), String> {
    let mut stale = Vec::new();
    for dir in roots.iter().flat_map(|root| skill_dirs_under(root)) {
        let path = dir.join("agents/openai.yaml");
        let generated = openai_yaml(&dir);
        if fs::read_to_string(&path).ok().as_deref() == Some(generated.as_str()) {
            continue;
        }
        if update {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, generated).unwrap();
        } else {
            let shown = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(&path);
            stale.push(shown.display().to_string());
        }
    }
    if stale.is_empty() {
        return Ok(());
    }
    Err(format!(
        "agents/openai.yaml is missing or stale for:\n  {}\nregenerate with `{REGENERATE}`",
        stale.join("\n  ")
    ))
}

#[test]
fn every_committed_openai_yaml_matches_its_skill() {
    let roots = CODEX_SKILL_ROOTS.map(Path::new);
    let update = std::env::var("DEVKIT_UPDATE_OPENAI_YAML").as_deref() == Ok("1");
    if let Err(message) = sync_openai_yaml(&roots, update) {
        panic!("{message}");
    }
}

#[test]
fn the_generated_cloud_openai_yaml_matches_the_hand_written_one() {
    let cloud = Path::new(CODEX_SKILL_ROOTS[1]).join("cloud");
    assert_eq!(
        openai_yaml(&cloud),
        "interface:\n  \
           display_name: cloud\n  \
           short_description: Find existing issue work and select a cloud execution workflow.\n\
         policy:\n  \
           allow_implicit_invocation: false\n"
    );
}

#[test]
fn a_stale_openai_yaml_fails_naming_the_regenerate_command() {
    let root = tempfile::tempdir().unwrap();
    let skill = root.path().join("demo");
    fs::create_dir_all(skill.join("agents")).unwrap();
    fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: Old words.\n---\nBody.\n",
    )
    .unwrap();
    sync_openai_yaml(&[root.path()], true).unwrap();
    sync_openai_yaml(&[root.path()], false).unwrap();

    fs::write(
        skill.join("SKILL.md"),
        "---\nname: demo\ndescription: New words.\ndisable-model-invocation: true\n---\nBody.\n",
    )
    .unwrap();
    let message = sync_openai_yaml(&[root.path()], false).unwrap_err();
    assert!(message.contains("demo"), "{message}");
    assert!(message.contains(REGENERATE), "{message}");

    sync_openai_yaml(&[root.path()], true).unwrap();
    let regenerated = fs::read_to_string(skill.join("agents/openai.yaml")).unwrap();
    assert!(
        regenerated.contains("short_description: New words."),
        "{regenerated}"
    );
    assert!(
        regenerated.contains("allow_implicit_invocation: false"),
        "{regenerated}"
    );
}

/// The limits the Agent Skills specification sets, which every harness this
/// plugin installs into enforces.
#[test]
fn every_skill_has_a_valid_name_and_description() {
    for dir in skill_dirs() {
        let meta = frontmatter::<serde_yaml_ng::Value>(&dir);
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
