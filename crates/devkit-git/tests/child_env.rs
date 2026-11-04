use std::process::Command;

use devkit_git::child_env::child_env;

/// Set, `a_failing_child_probe` runs a body that panics in a child.
const FAILING_PROBE_ENV: &str = "DEVKIT_TEST_FAILING_CHILD_PROBE";

#[test]
fn a_failing_child_probe() {
    if std::env::var_os(FAILING_PROBE_ENV).is_some() {
        child_env().run(|| panic!("the child body failed"));
    }
}

/// The helper fails the test that called it, so the check runs that test in a
/// process of its own.
#[test]
fn the_child_helper_reports_a_child_failure() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "a_failing_child_probe"])
        .env(FAILING_PROBE_ENV, "1")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("the child body failed"), "{stdout}");
}

#[test]
fn the_body_runs_with_the_given_variables() {
    child_env()
        .set("DEVKIT_TEST_CHILD_ENV_VALUE", "set on the child")
        .remove("PATH")
        .run(|| {
            assert_eq!(
                std::env::var("DEVKIT_TEST_CHILD_ENV_VALUE").as_deref(),
                Ok("set on the child")
            );
            assert_eq!(std::env::var_os("PATH"), None);
        });
}

/// A child whose filter matches no test exits with success, which would read
/// as a pass with the body never run.
#[test]
fn a_child_that_runs_no_test_fails() {
    let run = std::thread::Builder::new()
        .name("not_a_test_in_this_binary".to_string())
        .spawn(|| child_env().run(|| {}))
        .unwrap()
        .join();

    assert!(run.is_err());
}
