//! `pr create` gates an agent's PR on a passing run of the project's required
//! task, read from the run ledger, and renders that run into the PR body.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/gittrace.rs"]
mod gittrace;

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
    project_with("", gates, false)
}

/// [`project`] with `defaults` appended to the config's defaults. With
/// `drop_test`, `main` also commits a test file that the branch deletes.
fn project_with(defaults: &str, gates: &str, drop_test: bool) -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr(defaults);
    let config = fake.project().join("devkit.toml");
    let mut toml = std::fs::read_to_string(&config).unwrap();
    toml.push_str(TASKS);
    toml.push_str(gates);
    std::fs::write(&config, toml).unwrap();
    let suite = fake.project().join("tests/math.rs");
    if drop_test {
        std::fs::create_dir_all(suite.parent().unwrap()).unwrap();
        std::fs::write(
            &suite,
            "#[test]\nfn adds() {\n    assert_eq!(1 + 2, 3);\n}\n",
        )
        .unwrap();
    }
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "tasks"]);
    git(&fake, &["branch", "-f", "main", "HEAD"]);
    if drop_test {
        std::fs::remove_file(&suite).unwrap();
    }
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

const CREATE: [&str; 7] = [
    "pr",
    "create",
    "--no-push",
    "--pr-title",
    "feat: a thing",
    "--pr-body",
    "Adds a thing.",
];

fn create(p: &ghfake::Fake, caller: &str) -> (bool, String) {
    let out = p.issue_as(caller, &CREATE);
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
fn a_human_override_inside_a_harness_session_is_still_gated() {
    let p = project(GATE);
    let out = p.issue_in_session("human", Some("S"), &CREATE);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("`verify`"), "names the task: {stderr}");
    assert!(!opened(&p), "{}", p.calls());
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

/// The body of a human's PR opened after `devrun task broken <extra>`, a
/// task that prints `going down` and exits 3, ran at HEAD. `dirty` leaves
/// an untracked file in the tree during the run, removed before the PR.
fn body_after_failed_run(extra: &[&str], dirty: bool) -> (ghfake::Fake, String) {
    let p = project(
        &GATE
            .replace("\"verify\"", "\"broken\"")
            .replace("verify_args = { scope = \"all\" }\n", ""),
    );
    let scratch = p.project().join("scratch.txt");
    if dirty {
        std::fs::write(&scratch, "uncommitted\n").unwrap();
    }
    let mut args = vec!["broken"];
    args.extend_from_slice(extra);
    task(&p, &args);
    if dirty {
        std::fs::remove_file(&scratch).unwrap();
    }
    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    let body = body(&p);
    (p, body)
}

#[test]
fn a_failing_run_at_head_shows_its_output_tail_beside_not_run() {
    let (p, body) = body_after_failed_run(&[], false);
    assert!(body.contains("NOT RUN"), "{body}");
    assert!(
        body.contains(&format!(
            "\nThe latest run of `devrun task broken` at `{}`:\n\n\
             - Exit code: 3\n\
             - Tree: clean\n",
            head(&p)
        )),
        "{body}"
    );
    assert!(
        body.contains("\nOutput tail:\n\n```text\ngoing down\n```\n"),
        "{body}"
    );
}

#[test]
fn a_failing_run_on_a_dirty_tree_says_its_tree_was_not_clean() {
    let (_p, body) = body_after_failed_run(&[], true);
    assert!(
        body.contains(
            "- Tree: not clean: a file was modified, staged or untracked when it started or \
             ended, or HEAD moved during it\n"
        ),
        "{body}"
    );
    assert!(body.contains("going down"), "{body}");
}

#[test]
fn a_failing_run_given_env_names_the_variables_but_not_their_values() {
    let (_p, body) = body_after_failed_run(&["--env", "GIT_TRACE=0"], false);
    assert!(
        body.contains("- Tree: not proven clean: it was given `--env` for `GIT_TRACE`\n"),
        "{body}"
    );
    assert!(!body.contains("GIT_TRACE=0"), "{body}");
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

/// The refusal's stderr, once [`assert_refused`] holds.
fn refusal(p: &ghfake::Fake) -> String {
    assert_refused(p);
    create(p, "agent").1
}

#[test]
fn a_pass_on_a_dirty_tree_does_not_satisfy_the_gate() {
    let p = project(GATE);
    let scratch = p.project().join("scratch.txt");
    std::fs::write(&scratch, "uncommitted\n").unwrap();
    verify(&p);
    std::fs::remove_file(&scratch).unwrap();
    let stderr = refusal(&p);
    assert!(stderr.contains("tree was not clean"), "says why: {stderr}");
}

#[test]
fn a_pass_with_other_args_does_not_satisfy_the_gate() {
    let p = project(GATE);
    task(&p, &["verify", "--arg", "scope=some"]);
    let stderr = refusal(&p);
    assert!(
        stderr.contains("`--arg scope=some`") && stderr.contains("`--arg scope=all`"),
        "shows the args it ran with and the args required: {stderr}"
    );
}

#[test]
fn a_pass_given_env_does_not_satisfy_the_gate_and_says_so() {
    let p = project(GATE);
    task(&p, &[
        "verify",
        "--arg",
        "scope=all",
        "--env",
        "GIT_TRACE=0",
    ]);
    let stderr = refusal(&p);
    assert!(
        stderr.contains("--env") && stderr.contains("`GIT_TRACE`"),
        "names the variables it was given: {stderr}"
    );
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
    assert_eq!(
        body,
        format!(
            "Adds a thing.\n\n### Verification\n\n\
             - Command: `devrun task verify --arg scope=all`\n\
             - Exit code: 0\n\
             - SHA: `{sha}`\n\
             - Tree: clean\n"
        ),
        "a passing run lists no steps and quotes no output"
    );
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

/// The PR's Verification section, once an agent's `pr create` opened it.
fn opened_body(p: &ghfake::Fake) -> String {
    let (ok, stderr) = create(p, "agent");
    assert!(ok, "{stderr}");
    body(p)
}

/// `body` says the PR changes what `task` runs, with each of `lines` in
/// the diff it shows.
fn assert_shows_change(body: &str, task: &str, lines: &[&str]) {
    assert!(
        body.contains(&format!("This PR changes what `{task}` runs")),
        "{body}"
    );
    assert!(body.contains("```diff\n"), "{body}");
    for line in lines {
        assert!(body.contains(&format!("\n{line}\n")), "{line}: {body}");
    }
}

#[test]
fn a_refusal_caps_a_long_arg_and_never_quotes_an_arg_file() {
    let p = project(GATE);
    let long = "v".repeat(300);
    task(&p, &["verify", "--arg", &format!("scope={long}")]);
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(
        stderr.contains(&format!("`--arg scope={}...`", "v".repeat(200))),
        "{stderr}"
    );
    assert!(!stderr.contains(&"v".repeat(201)), "{stderr}");

    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("scope.txt");
    std::fs::write(&file, "file-contents-stay-out").unwrap();
    task(&p, &[
        "verify",
        "--arg-file",
        &format!("scope={}", file.display()),
    ]);
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(!stderr.contains("file-contents-stay-out"), "{stderr}");
    assert!(
        stderr.contains(&format!("--arg-file scope={}", file.display())),
        "{stderr}"
    );
}

/// The merge base's `verify_args` name a variable HEAD's task no longer
/// reads, so no run at HEAD can take them; the gate requires HEAD's
/// committed task and args instead, and shows the change.
#[test]
fn a_branch_renaming_the_required_variable_opens_on_heads_args() {
    let p = project(GATE);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        toml.replace("{{ scope }}", "{{ level }}")
            .replace("verify_args = { scope", "verify_args = { level"),
    )
    .unwrap();
    git(&p, &["commit", "-q", "-am", "rename the variable"]);
    serve_head(&p);
    task(&p, &["verify", "--arg", "level=all"]);
    let body = opened_body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert!(
        body.contains("- Command: `devrun task verify --arg level=all`"),
        "{body}"
    );
    assert!(
        body.contains(
            "This PR changes the required run from `devrun task verify --arg scope=all` to \
             `devrun task verify --arg level=all`"
        ),
        "{body}"
    );
}

#[test]
fn a_branch_redefining_the_required_task_opens_showing_the_change() {
    let p = project(GATE);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        toml.replace(
            r#"run = ["git", "-c", "x.scope={{ scope }}", "version"]"#,
            r#"run = ["git", "-c", "y.scope={{ scope }}", "--version"]"#,
        ),
    )
    .unwrap();
    git(&p, &["commit", "-q", "-am", "make verify a no-op"]);
    serve_head(&p);
    verify(&p);
    let body = opened_body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert_shows_change(&body, "verify", &[
        "  step 1: task verify",
        r#"-   argv: ["git","-c","x.scope=all","version"]"#,
        r#"+   argv: ["git","-c","y.scope=all","--version"]"#,
    ]);
}

#[test]
fn a_human_with_no_run_sees_the_change_beside_not_run() {
    let p = project(GATE);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, toml.replace("x.scope=", "y.scope=")).unwrap();
    git(&p, &["commit", "-q", "-am", "rename the key"]);
    serve_head(&p);
    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    let body = body(&p);
    assert!(body.contains("NOT RUN"), "{body}");
    assert_shows_change(&body, "verify", &[
        r#"+   argv: ["git","-c","y.scope=all","version"]"#,
    ]);
}

#[test]
fn a_branch_changing_only_the_required_tasks_env_opens_showing_the_change() {
    let p = project(GATE);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    let run = r#"run = ["git", "-c", "x.scope={{ scope }}", "version"]"#;
    std::fs::write(
        &config,
        toml.replace(
            run,
            &format!("{run}\nenv = {{ GIT_CONFIG_NOSYSTEM = \"1\" }}"),
        ),
    )
    .unwrap();
    git(&p, &["commit", "-q", "-am", "set an env on verify"]);
    serve_head(&p);
    verify(&p);
    assert_shows_change(&opened_body(&p), "verify", &[
        r#"+   env GIT_CONFIG_NOSYSTEM = "1""#,
    ]);
}

/// A linked worktree reads the primary clone's `devkit.toml` as a layer,
/// and the primary clone's uncommitted edit to it is no part of what the
/// branch commits, so it neither stops the run qualifying nor shows as a
/// change.
#[test]
fn a_run_in_a_linked_worktree_beside_an_edited_primary_config_qualifies() {
    let p = project(GATE);
    let branch = git(&p, &["branch", "--show-current"]).trim().to_string();
    git(&p, &["checkout", "-q", "--detach"]);
    let outside = tempfile::tempdir().unwrap();
    let wt = outside.path().join("wt");
    git(&p, &[
        "worktree",
        "add",
        "-q",
        wt.to_str().unwrap(),
        &branch,
    ]);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        format!("{toml}\n[tasks.verify.env]\nGIT_CONFIG_NOSYSTEM = \"1\"\n"),
    )
    .unwrap();

    let out = p.devkit_here(&[
        "run",
        "-C",
        wt.to_str().unwrap(),
        "task",
        "verify",
        "--arg",
        "scope=all",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = p.issue_as_in("agent", &wt, &[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "feat: a thing",
        "--pr-body",
        "Adds a thing.",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let body = body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert!(!body.contains("NOT RUN"), "{body}");
    assert!(!body.contains("This PR changes"), "{body}");
}

/// A layer outside the committed files, such as `--config`, can change
/// what the task runs while the tree stays clean, so the section shows the
/// steps the run executed and says they differ from HEAD's.
#[test]
fn a_run_under_an_uncommitted_layer_shows_its_steps_and_says_so() {
    let p = project(GATE);
    let outside = tempfile::tempdir().unwrap();
    let layer = outside.path().join("devkit.toml");
    let toml = std::fs::read_to_string(p.project().join("devkit.toml")).unwrap();
    std::fs::write(&layer, toml.replace("x.scope=", "z.scope=")).unwrap();
    task(&p, &[
        "--config",
        layer.to_str().unwrap(),
        "verify",
        "--arg",
        "scope=all",
    ]);
    let body = opened_body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert!(
        body.contains("\nverify: git -c z.scope=all version\n"),
        "shows the steps that ran: {body}"
    );
    assert!(
        body.contains("An uncommitted config layer changed what ran"),
        "{body}"
    );
}

#[test]
fn a_run_under_the_committed_config_lists_no_steps() {
    let p = project(GATE);
    verify(&p);
    let body = opened_body(&p);
    assert!(!body.contains("git -c x.scope=all version"), "{body}");
    assert!(!body.contains("uncommitted config layer"), "{body}");
}

#[test]
fn a_task_no_commit_defines_says_so() {
    let p = project("\n[pr.gates]\nverify_task = \"ghost\"\n");
    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    let body = body(&p);
    assert!(body.contains("NOT RUN"), "{body}");
    assert!(
        body.contains("do not define `ghost`"),
        "says the committed config lacks the task: {body}"
    );
}

#[test]
fn a_branch_changing_only_a_tasks_description_still_qualifies() {
    let p = project(GATE);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        &config,
        toml.replace(
            "[tasks.verify]\n",
            "[tasks.verify]\ndescription = \"Check it\"\n",
        ),
    )
    .unwrap();
    git(&p, &["commit", "-q", "-am", "describe verify"]);
    serve_head(&p);
    verify(&p);
    let body = opened_body(&p);
    assert!(!body.contains("This PR changes"), "{body}");
}

/// A project whose merge base commits a `[pr]` table that does not parse,
/// with `gates` after it, and whose branch repairs it.
fn project_with_a_broken_base_pr_table(gates: &str) -> ghfake::Fake {
    let p = project(&format!("\n[pr]\ntest_files = 3\n{gates}"));
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, toml.replace("test_files = 3\n", "")).unwrap();
    git(&p, &["commit", "-q", "-am", "repair [pr]"]);
    serve_head(&p);
    p
}

#[test]
fn a_merge_base_whose_pr_table_does_not_parse_opens_a_humans_pr_ungated() {
    let p = project_with_a_broken_base_pr_table("");
    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    assert!(
        !stderr.contains("warning"),
        "no gate binds a human: {stderr}"
    );
    assert_eq!(body(&p), "Adds a thing.");
}

#[test]
fn a_merge_base_whose_pr_table_does_not_parse_warns_a_human_with_gates_on() {
    let p = project_with_a_broken_base_pr_table(GATE);
    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    assert!(
        stderr.contains("warning: opening the PR without its gates"),
        "{stderr}"
    );
    assert!(stderr.contains("[pr]"), "says what failed: {stderr}");
    assert_eq!(body(&p), "Adds a thing.");
}

fn create_against_nowhere(p: &ghfake::Fake) -> std::process::Output {
    p.issue_as("human", &[
        "pr",
        "create",
        "--no-push",
        "--base",
        "nowhere",
        "--pr-title",
        "feat: a thing",
        "--pr-body",
        "Adds a thing.",
    ])
}

#[test]
fn a_base_that_resolves_nowhere_opens_a_humans_pr_with_no_gates_configured_silently() {
    let p = project("");
    let out = create_against_nowhere(&p);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(!stderr.contains("warning"), "{stderr}");
    assert_eq!(body(&p), "Adds a thing.");
}

#[test]
fn a_base_that_resolves_nowhere_opens_a_humans_pr_ungated_with_a_warning() {
    let p = project(GATE);
    let out = create_against_nowhere(&p);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("warning: opening the PR without its gates"),
        "{stderr}"
    );
    assert!(
        stderr.contains("`origin/nowhere`") && stderr.contains("`nowhere`"),
        "names the base that does not resolve: {stderr}"
    );
    assert_eq!(body(&p), "Adds a thing.");
}

#[test]
fn a_merge_base_whose_pr_table_does_not_parse_refuses_an_agents_pr() {
    let p = project_with_a_broken_base_pr_table("");
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("[pr]"), "{stderr}");
    assert!(!opened(&p), "{}", p.calls());
}

#[test]
fn a_passing_sequence_unchanged_from_the_merge_base_satisfies_the_gate() {
    let p = project(&format!(
        "\n[tasks.both]\nsteps = [{{ task = \"verify\" }}, {{ task = \"verify\" }}]\n{}",
        GATE.replace("\"verify\"", "\"both\"")
    ));
    task(&p, &["both", "--arg", "scope=all"]);
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    assert!(body(&p).contains("Exit code: 0"), "{}", body(&p));
}

/// `project(gates)` with `layer` committed on `main` as `sub/devkit.toml`.
fn project_with_nested_layer(gates: &str, layer: &str) -> ghfake::Fake {
    let p = project(gates);
    git(&p, &["checkout", "-q", "main"]);
    let sub = p.project().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("devkit.toml"), layer).unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "a nested layer"]);
    git(&p, &["checkout", "-q", "-"]);
    git(&p, &["rebase", "-q", "main"]);
    serve_head(&p);
    p
}

fn verify_in_sub(p: &ghfake::Fake) {
    let sub = p.project().join("sub");
    let out = p.devkit_here(&[
        "run",
        "-C",
        sub.to_str().unwrap(),
        "task",
        "verify",
        "--arg",
        "scope=all",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_run_under_an_unchanged_nested_layer_qualifies() {
    let p = project_with_nested_layer(GATE, "[tasks.verify.env]\nGIT_TRACE = \"0\"\n");
    verify_in_sub(&p);
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    assert!(body(&p).contains("Exit code: 0"), "{}", body(&p));
}

#[test]
fn a_branch_changing_the_tasks_env_in_a_nested_layer_opens_showing_the_change() {
    let p = project_with_nested_layer(GATE, "[tasks.verify.env]\nGIT_TRACE = \"0\"\n");
    std::fs::write(
        p.project().join("sub/devkit.toml"),
        "[tasks.verify.env]\nGIT_TRACE = \"0\"\nGIT_CONFIG_NOSYSTEM = \"1\"\n",
    )
    .unwrap();
    git(&p, &[
        "commit",
        "-q",
        "-am",
        "set an env on verify under sub",
    ]);
    serve_head(&p);
    verify_in_sub(&p);
    assert_shows_change(&opened_body(&p), "verify", &[
        r#"+   env GIT_CONFIG_NOSYSTEM = "1""#,
    ]);
}

/// The required task lives only in a nested layer, and the branch renames
/// it there and in the root gate: HEAD's task applies in the directory its
/// run ran from, so the gate requires it and shows the change.
#[test]
fn a_branch_renaming_a_nested_layers_required_task_opens_on_heads_task() {
    let p = project_with_nested_layer(
        "\n[pr.gates]\nverify_task = \"inner\"\n",
        "[tasks.inner]\nrun = [\"git\", \"version\"]\n",
    );
    let layer = p.project().join("sub/devkit.toml");
    std::fs::write(&layer, "[tasks.renamed]\nrun = [\"git\", \"version\"]\n").unwrap();
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, toml.replace("\"inner\"", "\"renamed\"")).unwrap();
    git(&p, &["commit", "-q", "-am", "rename the nested task"]);
    serve_head(&p);
    let sub = p.project().join("sub");
    let out = p.devkit_here(&["run", "-C", sub.to_str().unwrap(), "task", "renamed"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = opened_body(&p);
    assert!(body.contains("- Command: `devrun task renamed`"), "{body}");
    assert!(
        body.contains(
            "This PR changes the required run from `devrun task inner` to `devrun task renamed`"
        ),
        "{body}"
    );
}

/// Tasks reading the template variable `mode`, through their own `env` and
/// through their app's `static_env`.
const MODE_READERS: [&str; 2] = [
    "[tasks.envy]\nrun = [\"git\", \"version\"]\nenv = { GIT_MODE = \"{{ mode }}\" }\n",
    "[tasks.envy]\napp = \"tool\"\nrun = [\"git\", \"version\"]\n\
     [apps.tool]\nbase_port = 39201\npath = \".\"\nlaunch = [\"git\", \"version\"]\n\
     static_env = { GIT_MODE = \"{{ mode }}\" }\n",
];

fn project_reading_mode(reader: &str) -> ghfake::Fake {
    project(&format!(
        "{reader}\n[templates.variables]\nmode = \"full\"\n\n[pr.gates]\nverify_task = \"envy\"\n"
    ))
}

#[test]
fn a_task_reading_an_unchanged_template_variable_qualifies() {
    for reader in MODE_READERS {
        let p = project_reading_mode(reader);
        task(&p, &["envy"]);
        let (ok, stderr) = create(&p, "agent");
        assert!(ok, "{reader}: {stderr}");
        assert!(body(&p).contains("Exit code: 0"), "{reader}: {}", body(&p));
    }
}

/// A variable read by the task's `env` or its app's `static_env` shapes
/// what it runs, so a branch changing its default shows as a change, with
/// the rendered values left out.
#[test]
fn a_branch_changing_a_template_variable_the_task_reads_shows_the_change_without_values() {
    for reader in MODE_READERS {
        let p = project_reading_mode(reader);
        let file = p.project().join("devkit.toml");
        let toml = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, toml.replace("mode = \"full\"", "mode = \"s3cr3t\"")).unwrap();
        git(&p, &["commit", "-q", "-am", "change the variable"]);
        serve_head(&p);
        task(&p, &["envy"]);
        let body = opened_body(&p);
        assert_shows_change(&body, "envy", &[
            r#"~   env GIT_MODE = "{{ mode }}" (rendered value changed)"#,
        ]);
        assert!(!body.contains("s3cr3t"), "{reader}: {body}");
        assert!(!body.contains("full"), "{reader}: {body}");
    }
}

/// A run never records the values its env rendered.
#[test]
fn the_ledger_records_no_env_value() {
    let p = project(&format!(
        "
[tasks.secretive]
run = [\"git\", \"version\"]
env = {{ TOKEN = \"hunter2-{{{{ branch }}}}\" }}
{}",
        "\n[pr.gates]\nverify_task = \"secretive\"\n"
    ));
    task(&p, &["secretive"]);
    let runs = p.state().join("devkit/runs");
    let text: String = std::fs::read_dir(&runs)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert!(!text.contains("hunter2"), "{text}");
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
}

/// A task rendering the checkout's branch.
const BRANCH_READER: &str = "\n[tasks.named]\nrun = [\"git\", \"-c\", \"x.branch={{ branch }}\", \"version\"]\n\n[pr.gates]\nverify_task = \"named\"\n";

#[test]
fn a_task_rendering_the_branch_qualifies_on_the_branch_it_ran_on() {
    let p = project(BRANCH_READER);
    task(&p, &["named"]);
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    assert!(body(&p).contains("Exit code: 0"), "{}", body(&p));
}

/// Both commits' tasks render the branch as it is when the PR opens, so a
/// rename after the run changes nothing the gate compares.
#[test]
fn a_branch_renamed_after_the_run_still_qualifies_a_task_rendering_it() {
    let p = project(BRANCH_READER);
    task(&p, &["named"]);
    git(&p, &["branch", "-m", "lev/eng-1-renamed"]);
    let body = opened_body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert!(!body.contains("This PR changes"), "{body}");
}

#[test]
fn a_branch_renamed_after_the_run_still_qualifies_a_task_not_rendering_it() {
    let p = project(GATE);
    verify(&p);
    git(&p, &["branch", "-m", "lev/eng-1-renamed"]);
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
}

/// A task that needs `api` live, through `require_live`.
const LIVE_READER: &str = "[tasks.live]\nrun = [\"git\", \"-c\", \"x.api={{ ports['api'] }}\", \"version\"]\n\
     require_live = [\"api\"]\n\
     [apps.api]\nbase_port = 39202\npath = \".\"\nlaunch = [\"git\", \"version\"]\n";

/// `require_live` is a precondition of the run, so a branch dropping it
/// shows as a change.
#[test]
fn a_branch_dropping_a_require_live_entry_opens_showing_the_change() {
    let p = project(&format!(
        "{LIVE_READER}\n[pr.gates]\nverify_task = \"live\"\n"
    ));
    let file = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, toml.replace("require_live = [\"api\"]\n", "")).unwrap();
    git(&p, &["commit", "-q", "-am", "drop require_live"]);
    serve_head(&p);
    task(&p, &["live"]);
    assert_shows_change(&opened_body(&p), "live", &["-   require_live: api"]);
}

const AGENTS_NAME_SCOPE: &str = "[tasks.verify.required_args]\nscope = \"agents\"\n";

/// An arg the merge base requires of agents is a precondition of the run,
/// so a branch dropping the requirement shows as a change.
#[test]
fn a_branch_dropping_a_required_arg_opens_showing_the_change() {
    let p = project(&format!(
        "\n{AGENTS_NAME_SCOPE}\n[templates.variables]\nscope = \"all\"\n\n[pr.gates]\nverify_task = \"verify\"\n"
    ));
    let file = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, toml.replace(AGENTS_NAME_SCOPE, "")).unwrap();
    git(&p, &["commit", "-q", "-am", "drop required_args"]);
    serve_head(&p);
    task(&p, &["verify"]);
    assert_shows_change(&opened_body(&p), "verify", &[
        "- args accepted from: human",
        "+ args accepted from: human, agent",
    ]);
}

#[test]
fn a_task_rendering_a_port_qualifies() {
    let p = project(&format!(
        "{}\n[pr.gates]\nverify_task = \"live\"\n",
        LIVE_READER.replace("require_live = [\"api\"]\n", "")
    ));
    task(&p, &["live"]);
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    assert!(body(&p).contains("Exit code: 0"), "{}", body(&p));
}

/// A required sequence that brings `srv` up before running `verify`.
const UP_VERIFY: &str = "[apps.srv]\nbase_port = 39203\npath = \".\"\nlaunch = [\"git\", \"version\"]\n\
     [tasks.e2e]\nsteps = [{ up = \"srv\" }, { task = \"verify\" }]\n\
     [pr.gates]\nverify_task = \"e2e\"\nverify_args = { scope = \"all\" }\n";

/// [`hold`] for `srv`.
fn hold_srv(p: &ghfake::Fake, pid: Option<u32>) -> std::net::TcpListener {
    hold(p, "srv", pid)
}

/// Seed the port registry with this checkout's `app` row at a port the
/// returned listener answers on, so an `up` step reports ready at once.
/// With `pid`, the row is a live server the run reuses; without, a
/// reservation an `up` step launches into.
fn hold(p: &ghfake::Fake, app: &str, pid: Option<u32>) -> std::net::TcpListener {
    hold_since(p, app, pid, devkit_ports::registry::now())
}

/// [`hold`], with the row recorded at `ts`, in seconds since the epoch.
fn hold_since(p: &ghfake::Fake, app: &str, pid: Option<u32>, ts: u64) -> std::net::TcpListener {
    use devkit_ports::registry::{self, Data, Entry, Role};
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let holder = devkit_common::vcs::checkout_root(p.project())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut data = Data {
        version: registry::SCHEMA_VERSION,
        entries: Default::default(),
    };
    data.entries.insert(port, Entry {
        app: app.into(),
        holder,
        role: Role::Workspace,
        pid,
        logfile: None,
        ts,
    });
    let dir = p.state().join("devkit");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("ports.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    listener
}

#[test]
fn a_run_whose_up_step_reused_a_live_server_is_refused() {
    let p = project(UP_VERIFY);
    let _server = hold_srv(&p, Some(std::process::id()));
    task(&p, &["e2e", "--arg", "scope=all"]);
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("reused"), "says why: {stderr}");
    assert!(stderr.contains("`srv`"), "names the server: {stderr}");
    assert!(stderr.contains("devrun down"), "says how to fix: {stderr}");
    assert!(!opened(&p), "{}", p.calls());
}

/// A server `require_live` needs is the run's precondition, started before
/// it by design, so the run qualifies and its section names the server.
#[test]
fn a_run_against_a_require_live_server_qualifies_naming_the_server() {
    let p = project(&format!(
        "{LIVE_READER}\n[pr.gates]\nverify_task = \"live\"\n"
    ));
    let _server = hold_since(&p, "api", Some(std::process::id()), 1_760_000_000);
    task(&p, &["live"]);
    let body = opened_body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert!(
        body.contains("Against live server `api`, started 2025-10-09T08:53:20Z"),
        "{body}"
    );
}

/// `devrun runs passed <args>`'s exit status: whether the ledger holds a
/// run the gate would accept.
fn runs_passed(p: &ghfake::Fake, args: &[&str]) -> bool {
    let mut argv = vec!["run", "runs", "passed"];
    argv.extend_from_slice(args);
    p.devkit_here(&argv).status.success()
}

#[test]
fn runs_passed_answers_no_where_the_gate_refuses() {
    let p = project(UP_VERIFY);
    let _server = hold_srv(&p, Some(std::process::id()));
    task(&p, &["e2e", "--arg", "scope=all"]);
    task(&p, &["verify", "--arg", "scope=some"]);
    assert!(
        !runs_passed(&p, &["e2e", "--arg", "scope=all"]),
        "a run that reused a server"
    );
    assert!(
        !runs_passed(&p, &["verify", "--arg", "scope=all"]),
        "a run given other args"
    );
    assert!(runs_passed(&p, &["verify", "--arg", "scope=some"]));
    assert!(!create(&p, "agent").0, "the gate refuses too");
}

/// A layer can leave the required task only `up` steps while HEAD's
/// committed task also runs a command; the run differs from what HEAD
/// defines.
#[test]
fn a_run_of_only_up_steps_under_an_uncommitted_layer_says_so() {
    let p = project(
        "[apps.srv]\nbase_port = 39204\npath = \".\"\nlaunch = [\"git\", \"version\"]\n\
         [tasks.ver]\nrun = [\"git\", \"version\"]\n\
         [tasks.e2e]\nsteps = [{ up = \"srv\" }, { task = \"ver\" }]\n\
         [pr.gates]\nverify_task = \"e2e\"\n",
    );
    let _server = hold_srv(&p, None);
    let outside = tempfile::tempdir().unwrap();
    let layer = outside.path().join("devkit.toml");
    let toml = std::fs::read_to_string(p.project().join("devkit.toml")).unwrap();
    let only_up = toml.replace(
        "steps = [{ up = \"srv\" }, { task = \"ver\" }]",
        "steps = [{ up = \"srv\" }]",
    );
    assert_ne!(only_up, toml);
    std::fs::write(&layer, only_up).unwrap();
    task(&p, &["--config", layer.to_str().unwrap(), "e2e"]);
    let body = opened_body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert!(body.contains("\nup: srv\n"), "{body}");
    assert!(
        body.contains("An uncommitted config layer changed what ran"),
        "{body}"
    );
}

#[test]
fn a_run_whose_up_step_launched_its_server_qualifies() {
    let p = project(UP_VERIFY);
    let _server = hold_srv(&p, None);
    task(&p, &["e2e", "--arg", "scope=all"]);
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    assert!(body(&p).contains("Exit code: 0"), "{}", body(&p));
}

/// Serves the port `DEVKIT_TEST_SERVE_PORT` names, for a minute, when this
/// test binary is launched as an app's server; a no-op otherwise.
#[test]
fn serve_port_when_launched_as_an_app() {
    let Ok(port) = std::env::var("DEVKIT_TEST_SERVE_PORT") else {
        return;
    };
    let _listener =
        std::net::TcpListener::bind(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(60));
}

#[test]
fn a_run_whose_second_up_step_reuses_the_server_its_first_launched_qualifies() {
    let exe = std::env::current_exe().unwrap();
    // A TOML literal string: a Windows path's backslashes are not escapes.
    let p = project(&format!(
        "[apps.srv]\nbase_port = 39211\npath = \".\"\n\
         launch = ['{}', '--exact', 'serve_port_when_launched_as_an_app', '--nocapture']\n\
         static_env = {{ DEVKIT_TEST_SERVE_PORT = \"{{{{ port }}}}\" }}\n\
         [tasks.e2e]\nsteps = [{{ up = \"srv\" }}, {{ up = \"srv\" }}, {{ task = \"verify\" }}]\n\
         [pr.gates]\nverify_task = \"e2e\"\nverify_args = {{ scope = \"all\" }}\n",
        exe.display()
    ));
    task(&p, &["e2e", "--arg", "scope=all"]);
    let down = p.devkit_here(&["run", "down", "--app", "srv"]);
    assert!(
        down.status.success(),
        "{}",
        String::from_utf8_lossy(&down.stderr)
    );
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    assert!(body(&p).contains("Exit code: 0"), "{}", body(&p));
}

/// The value `srv`'s `TOKEN` renders to in [`serving_project`].
const TOKEN: &str = "s3cret-token-value";

/// A project whose required `e2e` brings `srv` up, this test binary serving
/// its port, then runs `verify`. `srv`'s `TOKEN` renders [`TOKEN`].
fn serving_project(base_port: u16) -> ghfake::Fake {
    let exe = std::env::current_exe().unwrap();
    project(&format!(
        "[templates.variables]\nscope = \"all\"\ntoken = \"{TOKEN}\"\n\
         [apps.srv]\nbase_port = {base_port}\npath = \".\"\n\
         launch = ['{}', '--exact', 'serve_port_when_launched_as_an_app', '--nocapture']\n\
         static_env = {{ DEVKIT_TEST_SERVE_PORT = \"{{{{ port }}}}\", TOKEN = \"{{{{ token }}}}\" }}\n\
         [tasks.e2e]\nsteps = [{{ up = \"srv\" }}, {{ task = \"verify\" }}]\n\
         [pr.gates]\nverify_task = \"e2e\"\nverify_args = {{ scope = \"all\" }}\n",
        exe.display()
    ))
}

/// `srv` launched by `devrun up`, stopped when dropped.
struct Srv<'a>(&'a ghfake::Fake);

impl<'a> Srv<'a> {
    fn up(p: &'a ghfake::Fake) -> Self {
        let out = p.devkit_here(&["run", "up", "srv"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Self(p)
    }
}

impl Drop for Srv<'_> {
    fn drop(&mut self) {
        self.0.devkit_here(&["run", "down", "--app", "srv"]);
    }
}

/// The origin the run ledger holds for `srv`'s process.
fn srv_origin(p: &ghfake::Fake) -> Option<devkit_ports::ledger::Origin> {
    let data = registry(p);
    let (port, entry) = data.entries.iter().find(|(_, e)| e.app == "srv").unwrap();
    devkit_ports::ledger::Ledger::at(p.state().join("devkit").join("runs"))
        .origin(*port, entry.pid.unwrap())
}

fn registry(p: &ghfake::Fake) -> devkit_ports::registry::Data {
    let text = std::fs::read(p.state().join("devkit").join("ports.json")).unwrap();
    serde_json::from_slice(&text).unwrap()
}

#[test]
fn devrun_up_records_the_launch_commit_and_plan_digest() {
    let p = serving_project(39212);
    let _srv = Srv::up(&p);
    let origin = srv_origin(&p).expect("an origin is recorded");
    assert_eq!(origin.commit, head(&p));
    assert!(origin.plan.starts_with("sha256:"), "{}", origin.plan);
    assert!(!origin.plan.contains(TOKEN), "{}", origin.plan);
    let text = std::fs::read_to_string(p.state().join("devkit").join("ports.json")).unwrap();
    assert!(!text.contains(TOKEN), "{text}");
}

#[test]
fn a_run_reusing_a_server_launched_at_head_with_its_plan_qualifies() {
    let p = serving_project(39213);
    let _srv = Srv::up(&p);
    task(&p, &["e2e", "--arg", "scope=all"]);
    let body = opened_body(&p);
    assert!(body.contains("Exit code: 0"), "{body}");
    assert!(
        body.contains("Against reused server `srv`, launched at this commit"),
        "{body}"
    );
    let plan = srv_origin(&p).unwrap().plan;
    let hex = plan.trim_start_matches("sha256:");
    assert!(!body.contains(hex), "the plan digest stays local: {body}");
}

/// The refusal an agent gets after a run that reused `srv`.
fn assert_refused_for_reusing_srv(p: &ghfake::Fake) {
    let (ok, stderr) = create(p, "agent");
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("reused"), "says why: {stderr}");
    assert!(stderr.contains("`srv`"), "names the server: {stderr}");
    assert!(!opened(p), "{}", p.calls());
}

#[test]
fn a_server_launched_on_a_dirty_tree_records_no_origin() {
    let p = serving_project(39220);
    std::fs::write(p.project().join("untracked.txt"), "dirty\n").unwrap();
    let _srv = Srv::up(&p);
    assert_eq!(srv_origin(&p), None);
}

/// `devrun up --supervise` with a daemon that exits once it supervises
/// nothing, so the test leaves no daemon behind.
#[cfg(unix)]
fn up_supervised(p: &ghfake::Fake) {
    let log = p.state().join("up.log");
    let ok = p.devkit_here_logged(
        &["run", "up", "srv", "--supervise"],
        &[("DEVKIT_DAEMON_IDLE_SECS", "1")],
        &log,
    );
    assert!(ok, "{}", std::fs::read_to_string(&log).unwrap_or_default());
}

/// Wait for the idle daemon to stop accepting. Its shutdown wakes its accept
/// loop through the socket, so it must exit before the test's state
/// directory, and the socket in it, is deleted. Each probe is activity that
/// restarts the daemon's idle timer, so probes stay wider apart than
/// `DEVKIT_DAEMON_IDLE_SECS`.
#[cfg(unix)]
fn await_daemon_exit(p: &ghfake::Fake) {
    let sock = p.state().join("devkit").join("ports.sock");
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if std::os::unix::net::UnixStream::connect(&sock).is_err() {
            return;
        }
    }
    panic!("devkitd did not idle out");
}

#[cfg(unix)]
#[test]
fn a_second_supervised_up_at_a_later_commit_keeps_the_first_origin() {
    let p = serving_project(39221);
    up_supervised(&p);
    let srv = Srv(&p);
    let first = srv_origin(&p).expect("the first up records an origin");
    std::fs::write(p.project().join("later.txt"), "later\n").unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "later"]);
    serve_head(&p);
    up_supervised(&p);
    assert_eq!(srv_origin(&p), Some(first));
    task(&p, &["e2e", "--arg", "scope=all"]);
    assert_refused_for_reusing_srv(&p);
    drop(srv);
    await_daemon_exit(&p);
}

#[test]
fn a_run_reusing_a_server_launched_at_another_commit_is_refused() {
    let p = serving_project(39214);
    let _srv = Srv::up(&p);
    std::fs::write(p.project().join("later.txt"), "later\n").unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "later"]);
    serve_head(&p);
    task(&p, &["e2e", "--arg", "scope=all"]);
    assert_refused_for_reusing_srv(&p);
}

#[test]
fn a_run_reusing_a_server_launched_with_another_plan_is_refused() {
    let p = serving_project(39215);
    let _srv = Srv::up(&p);
    let outside = tempfile::tempdir().unwrap();
    let layer = outside.path().join("devkit.toml");
    let toml = std::fs::read_to_string(p.project().join("devkit.toml")).unwrap();
    let other = toml.replace(
        "TOKEN = \"{{ token }}\" }",
        "TOKEN = \"{{ token }}\", EXTRA = \"x\" }",
    );
    assert_ne!(other, toml);
    std::fs::write(&layer, other).unwrap();
    task(&p, &[
        "--config",
        layer.to_str().unwrap(),
        "e2e",
        "--arg",
        "scope=all",
    ]);
    assert_refused_for_reusing_srv(&p);
}

#[test]
fn a_run_reusing_a_server_whose_env_rendered_another_value_is_refused() {
    let p = serving_project(39216);
    let _srv = Srv::up(&p);
    let outside = tempfile::tempdir().unwrap();
    let layer = outside.path().join("devkit.toml");
    let toml = std::fs::read_to_string(p.project().join("devkit.toml")).unwrap();
    let other = toml.replace(TOKEN, "another-token-value");
    assert_ne!(other, toml);
    std::fs::write(&layer, other).unwrap();
    task(&p, &[
        "--config",
        layer.to_str().unwrap(),
        "e2e",
        "--arg",
        "scope=all",
    ]);
    assert_refused_for_reusing_srv(&p);
}

/// The port registry is a file any process can rewrite, so an origin written
/// there names no commit the gate trusts.
#[test]
fn an_origin_forged_in_the_port_registry_does_not_qualify_a_stale_server() {
    let p = serving_project(39219);
    let _srv = Srv::up(&p);
    let launched_at = head(&p);
    std::fs::write(p.project().join("later.txt"), "later\n").unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "later"]);
    serve_head(&p);
    let file = p.state().join("devkit").join("ports.json");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let entry = json["entries"]
        .as_object_mut()
        .unwrap()
        .values_mut()
        .find(|e| e["app"] == "srv")
        .unwrap();
    entry["origin"] = serde_json::json!({
        "commit": head(&p),
        "plan": srv_origin(&p).unwrap().plan,
    });
    assert_ne!(launched_at, head(&p));
    std::fs::write(&file, serde_json::to_string(&json).unwrap()).unwrap();
    task(&p, &["e2e", "--arg", "scope=all"]);
    assert_refused_for_reusing_srv(&p);
}

#[test]
fn a_run_whose_reused_server_restarts_during_the_task_is_refused() {
    let p = serving_project(39218);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config).unwrap();
    let restarting = toml.replace(
        "steps = [{ up = \"srv\" }, { task = \"verify\" }]",
        &format!(
            "steps = [{{ up = \"srv\" }}, {{ task = \"srv-down\" }}, {{ up = \"srv\" }}, \
             {{ task = \"verify\" }}]\n\
             [tasks.srv-down]\nrun = ['{}', 'run', 'down', '--app', 'srv']",
            env!("CARGO_BIN_EXE_devkit")
        ),
    );
    assert_ne!(restarting, toml);
    std::fs::write(&config, restarting).unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "restart srv"]);
    serve_head(&p);
    let _srv = Srv::up(&p);
    task(&p, &["e2e", "--arg", "scope=all"]);
    assert_refused_for_reusing_srv(&p);
}

/// A run reads the commit and the tree state once before the task and once
/// after, and the gate reads `devkit.toml` once at the merge base and once
/// at HEAD; every other question a run and a gated `pr create` ask of git is
/// asked once.
#[test]
fn a_run_and_its_gated_pr_ask_git_each_question_once() {
    let p = project(GATE);
    let trace = gittrace::GitTrace::new();
    let traced = (gittrace::GitTrace::VAR, trace.dir().to_str().unwrap());

    let out = p.devkit_here_with_env(&["run", "task", "verify", "--arg", "scope=all"], &[traced]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = trace.take();
    assert_eq!(
        gittrace::GitTrace::repeated(&calls),
        [("rev-parse HEAD", 2), ("status --porcelain", 2)].into(),
        "{calls:#?}"
    );

    let out = p.issue_with_env(
        &[
            "pr",
            "create",
            "--no-push",
            "--pr-title",
            "feat: a thing",
            "--pr-body",
            "Adds a thing.",
        ],
        &[("DEVKIT_CALLER", "agent"), traced],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = trace.take();
    // Each read names its object on standard input, which the trace leaves
    // out, so the two reads show as one call made twice.
    assert_eq!(
        gittrace::GitTrace::repeated(&calls),
        [("cat-file --batch", 2)].into(),
        "{calls:#?}"
    );
}

/// `pr verification` run as `caller` with `extra` args: its success and
/// standard output, failing the test on a nonzero exit.
fn verification(p: &ghfake::Fake, caller: &str, extra: &[&str]) -> String {
    let mut args = vec!["pr", "verification"];
    args.extend_from_slice(extra);
    let out = p.issue_as(caller, &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn pr_verification_prints_the_section_pr_create_writes() {
    let p = project(GATE);
    verify(&p);
    let section = verification(&p, "agent", &[]);
    assert!(
        section.starts_with("### Verification\n\n- Command:"),
        "{section}"
    );
    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "{stderr}");
    let body = body(&p);
    assert!(body.contains(section.trim_end()), "{section}\n---\n{body}");
}

#[test]
fn pr_verification_prints_not_run_to_an_agent_with_no_run_at_head() {
    let p = project(GATE);
    let section = verification(&p, "agent", &[]);
    assert!(
        section.starts_with(&format!(
            "### Verification\n\nNOT RUN: the run ledger records no run of \
             `devrun task verify --arg scope=all` that exited 0 on a clean tree at `{}`.",
            head(&p)
        )),
        "{section}"
    );
    let (ok, _) = create(&p, "human");
    assert!(ok);
    assert!(body(&p).contains(section.trim_end()), "{}", body(&p));
}

/// A detached linked worktree has no branch and no worktree record, as a
/// checkout that only refreshes a PR body can be.
#[test]
fn pr_verification_runs_in_a_checkout_with_no_worktree_record() {
    let p = project(GATE);
    verify(&p);
    let scratch = tempfile::tempdir().unwrap();
    let detached = scratch.path().join("detached");
    git(&p, &[
        "worktree",
        "add",
        "-q",
        "--detach",
        detached.to_str().unwrap(),
        "HEAD",
    ]);
    assert!(!detached.join(".devkit").exists());

    let out = p.issue_as_in("agent", &detached, &["pr", "verification"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let section = String::from_utf8_lossy(&out.stdout);
    assert!(
        section.starts_with("### Verification\n\n- Command:"),
        "{section}"
    );
    assert!(
        section.contains(&format!("- SHA: `{}`\n", head(&p))),
        "{section}"
    );
}

#[test]
fn pr_verification_renders_the_head_it_is_given() {
    let p = project(GATE);
    verify(&p);
    let verified = head(&p);
    std::fs::write(p.project().join("feature.txt"), "a fix\n").unwrap();
    git(&p, &["commit", "-q", "-am", "a fix"]);

    let section = verification(&p, "agent", &["--head", &verified]);
    assert!(section.contains("- Exit code: 0\n"), "{section}");
    assert!(
        section.contains(&format!("- SHA: `{verified}`\n")),
        "{section}"
    );
    assert_eq!(
        verification(&p, "agent", &["--head", &verified[..12]]),
        section,
        "a short id names the same commit"
    );

    let now = verification(&p, "agent", &[]);
    assert!(now.contains("NOT RUN"), "{now}");
    assert!(now.contains(&head(&p)), "{now}");
}

const BROKEN_GATE: &str = "\n[pr.gates]\nverify_task = \"broken\"\n";

const WAIVE: [&str; 4] = ["pr", "waive-verify", "--reason", "flaky under host load"];

/// `pr waive-verify` for HEAD as `caller`, outside any harness session.
fn waive(p: &ghfake::Fake, caller: &str) -> (bool, String) {
    let out = p.issue_in_session(caller, None, &WAIVE);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_persons_waiver_passes_the_verify_gate_at_that_head_only() {
    let p = project(BROKEN_GATE);
    task(&p, &["broken"]);
    let (ok, stderr) = waive(&p, "human");
    assert!(ok, "{stderr}");

    let (ok, stderr) = create(&p, "agent");
    assert!(ok, "the waived head opens: {stderr}");
    let body = body(&p);
    assert!(
        body.starts_with("Adds a thing.\n\n### Verification\n\nWAIVED: "),
        "{body}"
    );
    assert!(
        body.contains("flaky under host load"),
        "names the reason: {body}"
    );
    assert!(body.contains(&head(&p)), "names the head: {body}");
    assert!(
        body.contains("\nOutput tail:\n\n```text\ngoing down\n```\n"),
        "shows the failed run: {body}"
    );

    git(&p, &["commit", "-q", "--allow-empty", "-m", "another head"]);
    serve_head(&p);
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "a new head needs a new waiver: {stderr}");
    assert!(stderr.contains("`verify` gate"), "{stderr}");
    assert!(stderr.contains(&head(&p)), "{stderr}");
}

#[test]
fn a_waived_head_still_runs_the_weakened_tests_gate() {
    let p = project_with("", BROKEN_GATE, true);
    let (ok, stderr) = waive(&p, "human");
    assert!(ok, "{stderr}");
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(stderr.contains("tests/math.rs"), "names the flag: {stderr}");
    assert!(!opened(&p), "{}", p.calls());
}

#[test]
fn a_waived_head_still_runs_the_proof_check() {
    let p = project_with(
        "pr_proof_variable = \"proof\"\n\n[tracker]\nkind = \"github\"\n\n\
         [templates.variables]\nproof = { default = \"\", required = \"agents\" }\n",
        BROKEN_GATE,
        false,
    );
    p.record_issue("7");
    p.serve_issue("Why.\n\n## Done when\n\n- [ ] One (proof: test)\n- [ ] Two (proof: test)\n");
    let (ok, stderr) = waive(&p, "human");
    assert!(ok, "{stderr}");
    let out = p.issue_as("agent", &[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "feat: a thing",
        "--arg",
        "proof=1. test `one`",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("2. Two (proof: test)"),
        "names the gap: {stderr}"
    );
    assert!(!opened(&p), "{}", p.calls());
}

#[test]
fn an_agent_cannot_waive_the_verify_gate() {
    let p = project(BROKEN_GATE);
    task(&p, &["broken"]);
    let (ok, stderr) = waive(&p, "agent");
    assert!(!ok, "{stderr}");
    assert!(
        stderr.contains("an agent caller cannot write a waiver"),
        "says why: {stderr}"
    );
    let out = p.issue_in_session("human", Some("S"), &WAIVE);
    assert!(
        !out.status.success(),
        "a human override inside a harness session is an agent: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (ok, stderr) = create(&p, "agent");
    assert!(!ok, "no waiver was written: {stderr}");
    assert!(!opened(&p), "{}", p.calls());
}
