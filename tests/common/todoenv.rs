//! A git checkout named `proj` on `main`, with a private state home, for
//! driving `devkit todo` and the todo hooks.
#![allow(dead_code)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use devkit_todo::{Filter, Todo, TodoStore};
use devkit_todo_builtin::BuiltinStore;

#[path = "testenv.rs"]
mod testenv;

pub struct Proj {
    root: tempfile::TempDir,
    home: tempfile::TempDir,
    pub path: PathBuf,
}

impl Proj {
    pub fn new() -> Self {
        Self::named("proj")
    }

    /// A checkout whose directory, and so whose repository node, is `name`.
    pub fn named(name: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(name);
        std::fs::create_dir(&path).unwrap();
        devkit_git::Git::fixture(&path)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        let (home, _) = testenv::isolated(env!("CARGO_BIN_EXE_devkit"));
        Proj { root, home, path }
    }

    /// As [`Proj::new`], with `toml` as the isolated home's
    /// `~/.config/devkit/config.toml`.
    pub fn with_home_config(toml: &str) -> Self {
        Self::new().home_config_of(toml)
    }

    /// This checkout with `toml` as the isolated home's
    /// `~/.config/devkit/config.toml`.
    pub fn home_config_of(self, toml: &str) -> Self {
        let dir = self.home.path().join(".config/devkit");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), toml).unwrap();
        self
    }

    pub fn home_config(&self) -> PathBuf {
        self.home.path().join(".config/devkit/config.toml")
    }

    /// `devkit` run in `dir` with `env` set and no ambient session.
    pub fn devkit_in(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> Output {
        self.spawn(dir, env!("CARGO_BIN_EXE_devkit"), args, env, stdin)
    }

    fn spawn(
        &self,
        dir: &Path,
        program: &str,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> Output {
        let mut cmd = Command::new(program);
        cmd.env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.home.path())
            .env("DEVKIT_SKIP_AUTOLINK", "1")
            .env_remove("DEVKIT_CALLER")
            .env_remove("DEVKIT_CONFIG")
            .env_remove("DEVKIT_ENFORCE_WRITES")
            .env_remove("DEVKIT_ENFORCE_COMMANDS");
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
        self.hook_with(verb, harness, payload, &[])
    }

    /// As [`Proj::hook`], with `env` set.
    pub fn hook_with(
        &self,
        verb: &str,
        harness: &str,
        payload: &serde_json::Value,
        env: &[(&str, &str)],
    ) -> Output {
        self.devkit_in(
            &self.path,
            &["hook", verb, "--harness", harness],
            env,
            &payload.to_string(),
        )
    }

    /// `command` run by bash in the checkout, as a harness's Bash tool runs
    /// it: with `devkit` on `PATH` and `env` set.
    pub fn shell(&self, command: &str, env: &[(&str, &str)]) -> Output {
        let bin = self.root.path().join("bin");
        if !bin.exists() {
            std::fs::create_dir(&bin).unwrap();
            std::fs::copy(env!("CARGO_BIN_EXE_devkit"), bin.join("devkit")).unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut all = vec![("PATH", path.as_str())];
        all.extend_from_slice(env);
        self.spawn(&self.path, "bash", &["-c", command], &all, "")
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
