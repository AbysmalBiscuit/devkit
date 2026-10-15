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
