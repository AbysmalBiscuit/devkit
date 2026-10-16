//! Fixture repositories built once and copied into each test.
//!
//! nextest runs each test in a process of its own, so a fixture every test
//! builds the same way costs its git spawns once per test. A [`Template`] is
//! built once on disk, under a name its steps decide, and each test copies it
//! instead.

use std::{
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
};

use crate::Git;

/// One step building a template repository.
#[derive(Debug, Hash)]
pub enum Step<'a> {
    /// Write the text to the file at the path, relative to the repository.
    Write(&'a str, &'a str),
    /// Run [`Git::fixture`] with these arguments in the repository.
    Git(&'a [&'a str]),
}

/// A repository that a list of [`Step`]s built.
pub struct Template {
    dir: PathBuf,
}

impl Template {
    /// The template `steps` build in an empty directory, under `root`. The
    /// first process to ask builds it; one that finishes after another has
    /// keeps the other's. A directory without `head`, which a cache that saves
    /// directories but not their files leaves behind, is passed over for the
    /// next number.
    pub fn of(root: &Path, steps: &[Step]) -> Self {
        let mut hasher = DefaultHasher::new();
        steps.hash(&mut hasher);
        let name = format!("git-template-{:016x}", hasher.finish());
        let mut n = 0u32;
        loop {
            let dir = root.join(format!("{name}-{n}"));
            if dir.join("head").exists() {
                return Self { dir };
            }
            if !dir.exists() {
                let built = root.join(format!("{name}-{n}.{}", std::process::id()));
                build(&built, steps);
                if std::fs::rename(&built, &dir).is_ok() {
                    return Self { dir };
                }
                if dir.join("head").exists() {
                    let _ = std::fs::remove_dir_all(&built);
                    return Self { dir };
                }
                return Self { dir: built };
            }
            n += 1;
        }
    }

    /// The commit the template's HEAD names, empty when it has none.
    pub fn head(&self) -> String {
        std::fs::read_to_string(self.dir.join("head")).expect("read template head")
    }

    /// Copies the repository into `to`, creating it when missing.
    pub fn copy_to(&self, to: &Path) {
        copy_tree(&self.dir.join("repo"), to);
    }
}

/// Runs `steps` in `dir/repo` and records the commit they leave in `dir/head`.
fn build(dir: &Path, steps: &[Step]) {
    let _ = std::fs::remove_dir_all(dir);
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).expect("template repo");
    for step in steps {
        match step {
            Step::Write(path, text) => {
                let path = repo.join(path);
                std::fs::create_dir_all(path.parent().expect("a file's parent"))
                    .expect("template file's directory");
                std::fs::write(path, text).expect("write template file");
            }
            Step::Git(args) => {
                Git::fixture(&repo)
                    .args(args.iter().copied())
                    .output()
                    .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"));
            }
        }
    }
    let head = Git::fixture(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap_or_default();
    std::fs::write(dir.join("head"), head.trim()).expect("write template head");
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("copy template dir");
    for entry in std::fs::read_dir(from).expect("read template") {
        let entry = entry.expect("template entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("template entry type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy template file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEPS: &[Step] = &[
        Step::Git(&["init", "-q", "-b", "main"]),
        Step::Write("f.txt", "x"),
        Step::Git(&["add", "."]),
        Step::Git(&["commit", "-qm", "init"]),
    ];

    /// Deletes every file under `dir` and keeps the directories, as
    /// `Swatinem/rust-cache` leaves `target/tmp` when it saves the target.
    fn keep_only_directories(dir: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                keep_only_directories(&path);
            } else {
                std::fs::remove_file(path).unwrap();
            }
        }
    }

    #[test]
    fn a_template_emptied_to_its_directories_is_rebuilt() {
        let root = tempfile::tempdir().unwrap();
        Template::of(root.path(), STEPS);
        keep_only_directories(root.path());

        let template = Template::of(root.path(), STEPS);
        let copy = tempfile::tempdir().unwrap();
        template.copy_to(copy.path());
        let head = Git::fixture(copy.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(head.trim(), template.head());
    }
}
