//! Runs a test body in a child of the test binary, with variables set on that
//! child, because `std::env::set_var` races with the binary's other threads.
//!
//! The whole test function runs in both processes, and only the child runs the
//! body. A value made before `run`, such as a temp dir, differs between the
//! two, so a body reads the parent's value back from the variable.

use std::{ffi::OsStr, process::Command, thread};

/// Names the test that a child process runs its body for.
const CHILD_TEST: &str = "DEVKIT_TEST_CHILD";

/// Start describing the environment of a child process for the current test.
pub fn child_env() -> ChildEnv {
    ChildEnv {
        command: Command::new(std::env::current_exe().expect("the test binary's path")),
    }
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

    /// Run `body` in a child, and fail the test if the child fails. Call it on
    /// the test's own thread, which libtest names after the test.
    #[track_caller]
    pub fn run(mut self, body: impl FnOnce()) {
        let current = thread::current();
        let test = current.name().expect("libtest names a test's thread");
        if std::env::var_os(CHILD_TEST).is_some_and(|child_test| child_test == test) {
            body();
            return;
        }

        let output = self
            .command
            .args(["--exact", test])
            .env(CHILD_TEST, test)
            .output()
            .expect("the test binary runs as a child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // A filter that matches no test also exits with success.
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "the child running `{test}` did not pass ({})\n--- stdout\n{stdout}\n--- stderr\n{stderr}",
            output.status,
        );
    }
}
