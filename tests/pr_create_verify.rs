//! `pr create` gates an agent's PR on a passing run of the project's required
//! task, read from the run ledger, and renders that run into the PR body.

#[path = "common/ghfake.rs"]
mod ghfake;

const TASKS: &str = r#"
[tasks.verify]
run = ["git", "-c", "x.scope={{ scope }}", "version"]

[tasks.broken]
run = ["git", "-c", "alias.boom=!echo going down; exit 3", "boom"]
"#;

const GATE: &str = "\n[pr.gates]\nverify_task = \"verify\"\nverify_args = { scope = \"all\" }\n";

const PR: ghfake::Pr = ghfake::Pr {
    number: 9,
    state: "OPEN",
    is_draft: true,
    author: "LevValle",
};

fn git(p: &ghfake::Fake, args: &[&str]) -> String {
    devkit_git::Git::fixture(p.project())
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e:#}"))
}

/// A project whose `main` commits `devkit.toml` with the tasks and `gates`
/// appended, and whose branch adds one commit on top.
fn project(gates: &str) -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr("");
    let config = fake.project().join("devkit.toml");
    let mut toml = std::fs::read_to_string(&config).unwrap();
    toml.push_str(TASKS);
    toml.push_str(gates);
    std::fs::write(&config, toml).unwrap();
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "tasks"]);
    git(&fake, &["branch", "-f", "main", "HEAD"]);
    std::fs::write(fake.project().join("feature.txt"), "a thing\n").unwrap();
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "a thing"]);
    fake.create_opens(&PR);
    serve_head(&fake);
    fake
}

fn head(p: &ghfake::Fake) -> String {
    git(p, &["rev-parse", "HEAD"]).trim().to_string()
}

fn serve_head(p: &ghfake::Fake) {
    p.serve_pr_at(&PR, &head(p));
}

/// `devrun task <args>` in the project, recorded in the run's ledger.
fn task(p: &ghfake::Fake, args: &[&str]) {
    let mut argv = vec!["run", "task"];
    argv.extend_from_slice(args);
    let out = p.devkit_here(&argv);
    assert!(
        out.status.success() || args[0] == "broken",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn verify(p: &ghfake::Fake) {
    task(p, &["verify", "--arg", "scope=all"]);
}

fn create(p: &ghfake::Fake, caller: &str) -> (bool, String) {
    let out = p.issue_as(caller, &[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "feat: a thing",
        "--pr-body",
        "Adds a thing.",
    ]);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn opened(p: &ghfake::Fake) -> bool {
    p.calls().contains("pr create")
}

fn body(p: &ghfake::Fake) -> String {
    p.created_pr_arg("--body").expect("a body reached gh")
}

/// The refusal an agent gets when the ledger holds no qualifying run.
fn assert_refused(p: &ghfake::Fake) {
    let (ok, stderr) = create(p, "agent");
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("`verify`"), "names the task: {stderr}");
    assert!(stderr.contains(&head(p)), "names the SHA: {stderr}");
    assert!(
        stderr.contains("devrun task verify --arg scope=all"),
        "names the run that would satisfy it: {stderr}"
    );
    assert!(!opened(p), "{}", p.calls());
}

#[test]
fn an_agent_with_no_passing_run_at_head_is_refused_naming_the_task_and_sha() {
    let p = project(GATE);
    assert_refused(&p);
}

#[test]
fn a_failing_run_at_head_does_not_satisfy_the_gate() {
    let p = project(
        &GATE
            .replace("\"verify\"", "\"broken\"")
            .replace("verify_args = { scope = \"all\" }\n", ""),
    );
    task(&p, &["broken"]);
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("`broken`"), "{stderr}");
    assert!(!opened(&p), "{}", p.calls());
}

#[test]
fn a_pass_at_an_ancestor_of_head_does_not_satisfy_the_gate() {
    let p = project(GATE);
    verify(&p);
    git(&p, &[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "a fix after the run",
    ]);
    serve_head(&p);
    assert_refused(&p);
}

#[test]
fn a_pass_on_a_dirty_tree_does_not_satisfy_the_gate() {
    let p = project(GATE);
    let scratch = p.project().join("scratch.txt");
    std::fs::write(&scratch, "uncommitted\n").unwrap();
    verify(&p);
    std::fs::remove_file(&scratch).unwrap();
    assert_refused(&p);
}

#[test]
fn a_pass_with_other_args_does_not_satisfy_the_gate() {
    let p = project(GATE);
    task(&p, &["verify", "--arg", "scope=some"]);
    assert_refused(&p);
}

#[test]
fn the_body_carries_a_verification_section_built_from_the_qualifying_run() {
    let p = project(GATE);
    verify(&p);
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    let body = body(&p);
    assert!(
        body.starts_with("Adds a thing.\n\n### Verification\n"),
        "{body}"
    );
    let sha = head(&p);
    for want in [
        "`devrun task verify --arg scope=all`",
        "Exit code: 0",
        &format!("`{sha}`"),
        "git version",
    ] {
        assert!(body.contains(want), "{want}: {body}");
    }
    assert!(!body.contains("NOT RUN"), "{body}");
}

#[test]
fn a_human_with_no_qualifying_run_gets_a_pr_saying_not_run() {
    let p = project(GATE);
    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    let body = body(&p);
    assert!(
        body.starts_with("Adds a thing.\n\n### Verification\n"),
        "{body}"
    );
    assert!(body.contains("NOT RUN"), "{body}");
    assert!(
        body.contains("devrun task verify --arg scope=all"),
        "{body}"
    );
    assert!(body.contains(&head(&p)), "{body}");
}

#[test]
fn a_human_with_a_qualifying_run_gets_it_in_the_body() {
    let p = project(GATE);
    verify(&p);
    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    let body = body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert!(!body.contains("NOT RUN"), "{body}");
}

#[test]
fn with_no_required_task_configured_the_pr_opens_as_before() {
    for gates in ["", "\n[pr.gates]\nverify = \"require\"\n"] {
        for caller in ["agent", "human"] {
            let p = project(gates);
            let (ok, stderr) = create(&p, caller);
            assert!(ok, "{gates} {caller}: {stderr}");
            assert_eq!(body(&p), "Adds a thing.", "{gates} {caller}");
        }
    }
}

#[test]
fn advise_opens_an_agents_pr_saying_not_run() {
    let p = project(&format!("{GATE}verify = \"advise\"\n"));
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    let body = body(&p);
    assert!(
        body.starts_with("Adds a thing.\n\n### Verification\n"),
        "{body}"
    );
    assert!(body.contains("NOT RUN"), "{body}");
}

#[test]
fn off_leaves_the_body_alone() {
    let p = project(&format!("{GATE}verify = \"off\"\n"));
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    assert_eq!(body(&p), "Adds a thing.");
}

#[test]
fn a_branch_dropping_the_required_task_is_still_gated_by_the_merge_base() {
    let p = project(GATE);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, toml.replace("verify_task = \"verify\"\n", "")).unwrap();
    git(&p, &["commit", "-q", "-am", "drop the gate"]);
    serve_head(&p);
    assert_refused(&p);
}

#[test]
fn a_blank_required_task_is_a_config_error() {
    let p = project("\n[pr.gates]\nverify_task = \"  \"\n");
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("verify_task"), "{stderr}");
    assert!(!opened(&p), "{}", p.calls());
}

#[test]
fn a_merge_base_whose_pr_table_does_not_parse_opens_a_humans_pr_ungated() {
    let p = project("\n[pr]\ntest_files = 3\n");
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, toml.replace("test_files = 3\n", "")).unwrap();
    git(&p, &["commit", "-q", "-am", "repair [pr]"]);
    serve_head(&p);

    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    assert!(stderr.contains("warning"), "{stderr}");
    assert!(stderr.contains("[pr]"), "says what failed: {stderr}");
    assert_eq!(body(&p), "Adds a thing.");
}

#[test]
fn a_merge_base_whose_pr_table_does_not_parse_refuses_an_agents_pr() {
    let p = project("\n[pr]\ntest_files = 3\n");
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, toml.replace("test_files = 3\n", "")).unwrap();
    git(&p, &["commit", "-q", "-am", "repair [pr]"]);
    serve_head(&p);

    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("[pr]"), "{stderr}");
    assert!(!opened(&p), "{}", p.calls());
}
