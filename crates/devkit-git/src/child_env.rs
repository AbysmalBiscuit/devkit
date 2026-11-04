//! Runs a test body in a child of the test binary, with variables set on that
//! child, because `std::env::set_var` races with the binary's other threads.
//!
//! The whole test function runs in both processes, and only the child runs the
//! body. A value made before `run`, such as a temp dir, differs between the
//! two, so a body reads the parent's value back from the variable.

use std::{
    ffi::OsStr,
    process::{Command, Stdio},
    thread,
};

/// Names the test that a child process runs its body for.
const CHILD_TEST: &str = "DEVKIT_TEST_CHILD";

/// The file a child writes once its body returns, so a child that ran no test
/// cannot pass.
const CHILD_RAN: &str = "DEVKIT_TEST_CHILD_RAN";

/// Start describing the environment of a child process for the current test.
pub fn child_env() -> ChildEnv {
    ChildEnv {
        command: Command::new(std::env::current_exe().expect("the test binary's path")),
    }
}

/// True in the child that [`ChildEnv::run`] starts. Setup that the child's
/// environment cannot run, such as a fixture built with git, checks it.
pub fn in_child() -> bool {
    let current = thread::current();
    std::env::var_os(CHILD_TEST).is_some_and(|test| current.name() == test.to_str())
}

/// The variables to change on the child. [`child_env`] makes one, and
/// [`ChildEnv::run`] finishes it.
#[must_use = "the body runs only through `run`"]
pub struct ChildEnv {
    command: Command,
}

impl ChildEnv {
    pub fn set(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    pub fn remove(mut self, key: impl AsRef<OsStr>) -> Self {
        self.command.env_remove(key);
        self
    }

    /// Give the child this stdout instead of a pipe the failure report shows.
    pub fn stdout(mut self, stdout: Stdio) -> Self {
        self.command.stdout(stdout);
        self
    }

    /// Run `body` in a child, and fail the test if the child fails. Call it on
    /// the test's own thread, which libtest names after the test.
    #[track_caller]
    pub fn run(mut self, body: impl FnOnce()) {
        if in_child() {
            body();
            let ran = std::env::var_os(CHILD_RAN).expect("the parent names the marker file");
            std::fs::write(ran, "").expect("write the marker file");
            return;
        }

        let current = thread::current();
        let test = current.name().expect("libtest names a test's thread");
        let marker = tempfile::tempdir().expect("a temp dir for the marker file");
        let ran = marker.path().join("ran");
        let output = self
            .command
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_TEST, test)
            .env(CHILD_RAN, &ran)
            .output()
            .expect("the test binary runs as a child");
        assert!(
            output.status.success() && ran.is_file(),
            "the child running `{test}` did not pass ({})\n--- stdout\n{}\n--- stderr\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
