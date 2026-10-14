//! `pr create` gates an agent's PR on the weakened-tests check, at the
//! strength `[pr.gates]` sets in the devkit.toml committed at the merge base.

#[path = "common/ghfake.rs"]
mod ghfake;

const SUITE: &str = "\
#[test]
fn adds() {
    assert_eq!(1 + 2, 3);
}

#[test]
fn subtracts() {
    assert_eq!(3 - 2, 1);
}
";

fn git(p: &ghfake::Fake, args: &[&str]) {
    devkit_git::Git::fixture(p.project())
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"));
}

/// A project whose `main` commits a two-test suite and `devkit.toml` with
/// `gates` appended, and whose branch then drops a test.
fn project(gates: &str) -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr("");
    let config = fake.project().join("devkit.toml");
    let mut toml = std::fs::read_to_string(&config).unwrap();
    toml.push_str(gates);
    std::fs::write(&config, toml).unwrap();
    std::fs::create_dir_all(fake.project().join("tests")).unwrap();
    std::fs::write(fake.project().join("tests/math.rs"), SUITE).unwrap();
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "suite"]);
    git(&fake, &["branch", "-f", "main", "HEAD"]);
    std::fs::write(
        fake.project().join("tests/math.rs"),
        SUITE.replace("#[test]\nfn subtracts", "fn subtracts"),
    )
    .unwrap();
    git(&fake, &["commit", "-q", "-am", "drop a test"]);
    let pr = ghfake::Pr {
        number: 9,
        state: "OPEN",
        is_draft: true,
        author: "LevValle",
    };
    fake.create_opens(&pr);
    let head = devkit_git::Git::fixture(fake.project())
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    fake.serve_pr_at(&pr, head.trim());
    fake
}

fn create(p: &ghfake::Fake, caller: &str, body: &str) -> (bool, String) {
    let out = p.issue_as(caller, &[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "feat: a thing",
        "--pr-body",
        body,
    ]);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn opened(p: &ghfake::Fake) -> bool {
    p.calls().contains("pr create")
}

#[test]
fn an_unexplained_flag_refuses_the_pr_listing_each_flag() {
    let p = project("");
    let (ok, stderr) = create(&p, "agent", "Adds a thing.");
    assert!(!ok, "{stderr}");
    assert!(
        stderr.contains("tests/math.rs: fewer-tests: test declarations 2 -> 1"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Test changes:"),
        "names the way out: {stderr}"
    );
    assert!(!opened(&p), "{}", p.calls());
}

#[test]
fn a_test_changes_line_lets_the_pr_open() {
    let p = project("");
    let body = "Adds a thing.\n\nTest changes: subtracts is covered by the doctest";
    let (ok, stderr) = create(&p, "agent", body);
    assert!(ok, "{stderr}");
    assert!(opened(&p), "{}", p.calls());
}

#[test]
fn a_human_caller_is_not_gated() {
    let p = project("");
    let (ok, stderr) = create(&p, "human", "Adds a thing.");
    assert!(ok, "{stderr}");
}

#[test]
fn advise_lists_the_flags_in_the_body_and_opens_the_pr() {
    let p = project("\n[pr.gates]\nweakened_tests = \"advise\"\n");
    let (ok, stderr) = create(&p, "agent", "Adds a thing.");
    assert!(ok, "{stderr}");
    let body = p.created_pr_arg("--body").expect("a body reached gh");
    assert!(body.starts_with("Adds a thing."), "{body}");
    assert!(
        body.contains("tests/math.rs: fewer-tests: test declarations 2 -> 1"),
        "{body}"
    );
}

#[test]
fn off_skips_the_check() {
    let p = project("\n[pr.gates]\nweakened_tests = \"off\"\n");
    let (ok, stderr) = create(&p, "agent", "Adds a thing.");
    assert!(ok, "{stderr}");
    let body = p.created_pr_arg("--body").expect("a body reached gh");
    assert_eq!(body, "Adds a thing.");
}

#[test]
fn a_branch_turning_the_gate_off_is_still_gated_by_the_merge_base() {
    let p = project("");
    let config = p.project().join("devkit.toml");
    let mut toml = std::fs::read_to_string(&config).unwrap();
    toml.push_str("\n[pr.gates]\nweakened_tests = \"off\"\n");
    std::fs::write(&config, toml).unwrap();
    git(&p, &["commit", "-q", "-am", "loosen the gate"]);

    let (ok, stderr) = create(&p, "agent", "Adds a thing.");
    assert!(!ok, "the merge base still requires the gate: {stderr}");
    assert!(stderr.contains("tests/math.rs: fewer-tests"), "{stderr}");
    assert!(!opened(&p), "{}", p.calls());
}

#[test]
fn reusing_an_open_pr_does_not_read_the_gate() {
    let pr = ghfake::Pr {
        number: 7,
        state: "OPEN",
        is_draft: true,
        author: "LevValle",
    };
    let p = ghfake::Fake::new("", &pr);
    p.serve_pr(&pr);
    let out = p.issue_as("agent", &["pr", "create", "--no-push", "--base", "nowhere"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("/pull/7"),
        "{stderr}"
    );
    assert!(!opened(&p), "{}", p.calls());
}
