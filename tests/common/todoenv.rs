//! A git checkout named `proj` on `main`, with a private state home, for
//! driving `devkit todo` and the todo hooks.
#![allow(dead_code)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use devkit_todo::{BuiltinStore, Filter, Todo, TodoStore};

#[path = "testenv.rs"]
mod testenv;

pub struct Proj {
    root: tempfile::TempDir,
    home: tempfile::TempDir,
    pub path: PathBuf,
}

impl Proj {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("proj");
        std::fs::create_dir(&path).unwrap();
        devkit_git::Git::fixture(&path)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        let (home, _) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
        Proj { root, home, path }
    }

    /// `devkit` run in `dir` with `env` set and no ambient session.
    pub fn devkit_in(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
        cmd.env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.home.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .env_remove("DEVKIT_CALLER");
        testenv::scrub_identity(&mut cmd);
        cmd.args(args)
            .envs(env.iter().copied())
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    pub fn devkit(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.devkit_in(&self.path, args, env, "")
    }

    /// `devkit hook <verb> --harness <harness>` with `payload` on stdin.
    pub fn hook(&self, verb: &str, harness: &str, payload: &serde_json::Value) -> Output {
        self.devkit_in(
            &self.path,
            &["hook", verb, "--harness", harness],
            &[],
            &payload.to_string(),
        )
    }

    pub fn state(&self) -> PathBuf {
        self.home.path().join("devkit")
    }

    pub fn store(&self) -> BuiltinStore {
        BuiltinStore::at(self.state().join("todo"))
    }

    pub fn todos(&self) -> Vec<Todo> {
        self.store().list(&Filter::all()).unwrap()
    }

    pub fn todo(&self, id: &str) -> Todo {
        self.todos()
            .into_iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("no todo {id}"))
    }

    pub fn outside(&self) -> &Path {
        self.root.path()
    }
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}
