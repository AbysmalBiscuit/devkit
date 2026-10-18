//! `pr create` checks each `Done when` item marked `(proof: repro
//! <selector>)` red to green: the selector must fail in a throwaway worktree
//! with the branch's non-test changes reverted, and pass at HEAD.

#[path = "common/ghfake.rs"]
mod ghfake;
#[path = "common/gittrace.rs"]
mod gittrace;

/// A "test runner" that passes when a line of the test file the selector
/// names appears under `src/`, and exits 128 when the file does not exist.
const TASKS: &str = r#"
[tracker]
kind = "github"

[tasks.repro]
run = ["git", "grep", "--no-index", "-q", "-f", "{{ selector }}", "--", "src"]
"#;

const GATE: &str = "\n[pr.gates]\nrepro_task = \"repro\"\nrepro_no_tests_exit = 128\n";

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

fn write(p: &ghfake::Fake, path: &str, text: &str) {
    let path = p.project().join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A project whose `main` commits the tasks, `gates` and `src/lib.txt`
/// reading `broken`, and whose branch fixes it and adds tests: `fix.txt`,
/// which needs the fix, and `always.txt`, which passes either way. The
/// recorded issue's `Done when` items are `items`.
fn project(gates: &str, items: &str) -> ghfake::Fake {
    let fake = ghfake::Fake::without_pr("");
    let config = fake.project().join("devkit.toml");
    let mut toml = std::fs::read_to_string(&config).unwrap();
    toml.push_str(TASKS);
    toml.push_str(gates);
    std::fs::write(&config, toml).unwrap();
    write(&fake, "src/lib.txt", "lib: broken\n");
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "tasks"]);
    git(&fake, &["branch", "-f", "main", "HEAD"]);
    write(&fake, "src/lib.txt", "lib: fixed\n");
    write(&fake, "tests/fix.txt", "fixed\n");
    write(&fake, "tests/always.txt", "lib:\n");
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "fix the lib"]);
    fake.create_opens(&PR);
    fake.serve_pr_at(&PR, git(&fake, &["rev-parse", "HEAD"]).trim());
    fake.record_issue("7");
    fake.serve_issue(&format!("Why.\n\n## Done when\n\n{items}\n"));
    fake
}

fn create(p: &ghfake::Fake, caller: &str) -> (bool, String) {
    let out = p.issue_as(caller, &[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "fix: the lib",
        "--pr-body",
        "Fixes the lib.",
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

/// The body of the PR an agent's `pr create` opened.
fn opened_body(p: &ghfake::Fake) -> String {
    let (ok, stderr) = create(p, "agent");
    assert!(ok, "{stderr}");
    body(p)
}

/// The refusal an agent's `pr create` got, once it opened nothing.
fn refusal(p: &ghfake::Fake) -> String {
    let (ok, stderr) = create(p, "agent");
    assert!(!ok, "{stderr}");
    assert!(!opened(p), "{}", p.calls());
    stderr
}

const FIX: &str = "- [ ] The lib is fixed (proof: repro tests/fix.txt)";
const ALWAYS: &str = "- [ ] The lib reads well (proof: repro tests/always.txt)";
const NONE: &str = "- [ ] The lib is tested (proof: repro tests/none.txt)";
const UNMARKED: &str = "- [ ] The docs say so (proof: review)";

#[test]
fn a_fix_that_turns_its_repro_red_to_green_opens_with_both_runs_in_the_body() {
    let p = project(GATE, FIX);
    let body = opened_body(&p);
    assert!(
        body.starts_with("Fixes the lib.\n\n### Verification\n"),
        "{body}"
    );
    let head = git(&p, &["rev-parse", "HEAD"]).trim().to_string();
    for want in [
        "1. The lib is fixed",
        "PASSED",
        "`devrun task repro --arg selector=tests/fix.txt`",
        "With the fix reverted",
        "exited 1",
        &format!("At HEAD `{head}`"),
        "exited 0",
        "`src/lib.txt`",
    ] {
        assert!(body.contains(want), "{want}: {body}");
    }

    let codes = repro_runs(&p);
    assert_eq!(codes.len(), 2, "both runs land in the ledger: {codes:?}");
    assert!(codes.contains(&0) && codes.contains(&1), "{codes:?}");
}

#[test]
fn a_repro_check_lists_the_worktrees_once_per_checkout() {
    let p = project(GATE, FIX);
    let trace = gittrace::GitTrace::new();
    let out = p.issue_with_env(
        &[
            "pr",
            "create",
            "--no-push",
            "--pr-title",
            "fix: the lib",
            "--pr-body",
            "Fixes the lib.",
        ],
        &[
            ("DEVKIT_CALLER", "agent"),
            (gittrace::GitTrace::VAR, trace.dir().to_str().unwrap()),
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = trace.take();
    // One listing for the caller's checkout, one for the throwaway worktree,
    // and one in each of the two repro runs there.
    assert_eq!(
        calls
            .iter()
            .filter(|c| *c == "worktree list --porcelain")
            .count(),
        4,
        "{calls:#?}"
    );
}

/// The exit codes of the repro runs the project's ledger holds.
fn repro_runs(p: &ghfake::Fake) -> Vec<i64> {
    let out = p.devkit_here(&["run", "runs", "list", "--task", "repro", "--json"]);
    let runs: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    runs.as_array()
        .unwrap()
        .iter()
        .map(|r| r["exit_code"].as_i64().unwrap())
        .collect()
}

#[test]
fn a_pr_another_gate_refuses_runs_no_repro() {
    let p = project(
        &format!(
            "\n[tasks.verify]\nrun = [\"git\", \"version\"]\n{GATE}verify_task = \"verify\"\n"
        ),
        FIX,
    );
    let stderr = refusal(&p);
    assert!(
        stderr.contains("verify"),
        "the verify gate refused: {stderr}"
    );
    assert_eq!(repro_runs(&p), Vec::<i64>::new());
}

#[test]
fn a_repro_that_passes_with_the_fix_reverted_refuses_naming_the_item() {
    let p = project(GATE, &format!("{FIX}\n{ALWAYS}"));
    let stderr = refusal(&p);
    assert!(
        stderr.contains("2. The lib reads well"),
        "names the item: {stderr}"
    );
    assert!(
        stderr.contains("passes with the fix reverted"),
        "says why: {stderr}"
    );
    assert!(
        !stderr.contains("1. The lib is fixed"),
        "names only the failing item: {stderr}"
    );
}

#[test]
fn a_human_override_inside_a_harness_session_is_still_gated() {
    let p = project(GATE, &format!("{FIX}\n{ALWAYS}"));
    let out = p.issue_in_session("human", Some("S"), &[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "fix: the lib",
        "--pr-body",
        "Fixes the lib.",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("passes with the fix reverted"),
        "the repro gate refused: {stderr}"
    );
    assert!(!opened(&p), "{}", p.calls());
}

#[test]
fn a_selector_matching_no_test_refuses_naming_the_item() {
    let p = project(GATE, NONE);
    let stderr = refusal(&p);
    assert!(
        stderr.contains("1. The lib is tested"),
        "names the item: {stderr}"
    );
    assert!(stderr.contains("matches no test"), "says why: {stderr}");
}

#[test]
fn a_file_holding_test_and_source_changes_is_inconclusive() {
    let p = project(GATE, ALWAYS);
    write(
        &p,
        "src/mixed.rs",
        "fn f() -> u8 { 2 }\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn f_is_two() { assert_eq!(super::f(), 2); }\n}\n",
    );
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "a module and its test"]);
    p.serve_pr_at(&PR, git(&p, &["rev-parse", "HEAD"]).trim());
    let body = opened_body(&p);
    assert!(body.contains("INCONCLUSIVE"), "{body}");
    assert!(body.contains("`src/mixed.rs`"), "names the file: {body}");
}

#[test]
fn a_fix_updating_an_out_of_line_test_module_keeps_that_module() {
    let fake = project(GATE, "");
    git(&fake, &["checkout", "-q", "main"]);
    write(
        &fake,
        "src/shell.rs",
        "fn f() {}\n\n#[cfg(test)]\nmod tests;\n",
    );
    write(&fake, "src/shell/tests.rs", "// expects: lib: broken\n");
    git(&fake, &["add", "-A"]);
    git(&fake, &[
        "commit",
        "-q",
        "-m",
        "a module tested out of line",
    ]);
    git(&fake, &["checkout", "-q", "-"]);
    git(&fake, &["rebase", "-q", "main"]);
    write(&fake, "src/shell/tests.rs", "// expects the fix\n");
    write(&fake, "tests/old.txt", "lib: fixed\nexpects: lib: broken\n");
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "update the expectation"]);
    fake.serve_pr_at(&PR, git(&fake, &["rev-parse", "HEAD"]).trim());
    fake.serve_issue(
        "Why.\n\n## Done when\n\n- [ ] The lib is fixed (proof: repro tests/old.txt)\n",
    );
    let body = opened_body(&fake);
    assert!(body.contains("PASSED"), "{body}");
    assert!(
        !body.contains("`src/shell/tests.rs`"),
        "the test module is not reverted: {body}"
    );
}

#[test]
fn a_fix_updating_a_test_module_declared_in_an_inline_module_keeps_it() {
    let fake = project(GATE, "");
    git(&fake, &["checkout", "-q", "main"]);
    write(
        &fake,
        "src/lib.rs",
        "mod outer {\n    #[cfg(test)]\n    mod tests;\n}\n",
    );
    write(&fake, "src/outer/tests.rs", "// expects: lib: broken\n");
    git(&fake, &["add", "-A"]);
    git(&fake, &[
        "commit",
        "-q",
        "-m",
        "an inline module tested out of line",
    ]);
    git(&fake, &["checkout", "-q", "-"]);
    git(&fake, &["rebase", "-q", "main"]);
    write(&fake, "src/outer/tests.rs", "// expects the fix\n");
    write(&fake, "tests/old.txt", "lib: fixed\nexpects: lib: broken\n");
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "update the expectation"]);
    fake.serve_pr_at(&PR, git(&fake, &["rev-parse", "HEAD"]).trim());
    fake.serve_issue(
        "Why.\n\n## Done when\n\n- [ ] The lib is fixed (proof: repro tests/old.txt)\n",
    );
    let body = opened_body(&fake);
    assert!(body.contains("PASSED"), "{body}");
    assert!(
        !body.contains("`src/outer/tests.rs`"),
        "the test module is not reverted: {body}"
    );
}

#[test]
fn a_module_file_a_path_attribute_reaches_is_inconclusive_and_kept() {
    let fake = project(GATE, "");
    git(&fake, &["checkout", "-q", "main"]);
    write(
        &fake,
        "src/shell.rs",
        "fn f() {}\n\n#[cfg(test)]\n#[path = \"checks/shell_cases.rs\"]\nmod tests;\n",
    );
    write(
        &fake,
        "src/checks/shell_cases.rs",
        "// expects: lib: broken\n",
    );
    git(&fake, &["add", "-A"]);
    git(&fake, &[
        "commit",
        "-q",
        "-m",
        "a module tested through a path",
    ]);
    git(&fake, &["checkout", "-q", "-"]);
    git(&fake, &["rebase", "-q", "main"]);
    write(&fake, "src/checks/shell_cases.rs", "// expects the fix\n");
    write(&fake, "tests/old.txt", "lib: fixed\nexpects: lib: broken\n");
    git(&fake, &["add", "-A"]);
    git(&fake, &["commit", "-q", "-m", "update the expectation"]);
    fake.serve_pr_at(&PR, git(&fake, &["rev-parse", "HEAD"]).trim());
    fake.serve_issue(
        "Why.\n\n## Done when\n\n- [ ] The lib is fixed (proof: repro tests/old.txt)\n",
    );
    let body = opened_body(&fake);
    assert!(body.contains("INCONCLUSIVE"), "{body}");
    assert!(
        body.contains("`src/checks/shell_cases.rs`"),
        "names the file: {body}"
    );
    assert_eq!(repro_runs(&fake), Vec::<i64>::new(), "nothing is reverted");
}

#[test]
fn a_test_module_whose_end_is_unclear_is_inconclusive() {
    let p = project(GATE, FIX);
    let module = |expect: &str| {
        format!(
            "fn f() {{}}\n\n#[cfg(test)]\nmod tests {{\n    const FIXTURE: &str = r#\"\nfn x() \
             {{\n}}\n\"#;\n\n    #[test]\n    fn t() {{ assert_eq!(FIXTURE, \"{expect}\"); }}\n}}\n"
        )
    };
    git(&p, &["checkout", "-q", "main"]);
    write(&p, "src/fixture.rs", &module("old"));
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "a module with a fixture"]);
    git(&p, &["checkout", "-q", "-"]);
    git(&p, &["rebase", "-q", "main"]);
    write(&p, "src/fixture.rs", &module("new"));
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "update the expectation"]);
    p.serve_pr_at(&PR, git(&p, &["rev-parse", "HEAD"]).trim());
    let body = opened_body(&p);
    assert!(body.contains("INCONCLUSIVE"), "{body}");
    assert!(body.contains("`src/fixture.rs`"), "names the file: {body}");
}

#[test]
fn a_ticked_item_is_shown_as_done_and_not_checked() {
    let p = project(
        GATE,
        &format!("{FIX}\n- [x] The lib reads well (proof: repro tests/always.txt)"),
    );
    let body = opened_body(&p);
    assert!(
        body.contains("2. The lib reads well: already ticked in the issue"),
        "{body}"
    );
    assert_eq!(repro_runs(&p).len(), 2, "only item 1 ran");
}

#[test]
fn a_test_added_in_an_out_of_line_module_is_run_with_the_fix_reverted() {
    let p = project("\n[pr.gates]\nrepro_task = \"repro\"\n", "");
    git(&p, &["checkout", "-q", "main"]);
    write(
        &p,
        "src/shell.rs",
        "fn f() {}\n\n#[cfg(test)]\nmod tests;\n",
    );
    git(&p, &["add", "-A"]);
    git(&p, &[
        "commit",
        "-q",
        "-m",
        "a module to be tested out of line",
    ]);
    git(&p, &["checkout", "-q", "-"]);
    git(&p, &["rebase", "-q", "main"]);
    write(&p, "src/shell/tests.rs", "// passes either way\n");
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "a test that needs no fix"]);
    p.serve_pr_at(&PR, git(&p, &["rev-parse", "HEAD"]).trim());
    p.serve_issue(
        "Why.\n\n## Done when\n\n- [ ] The shell works (proof: repro src/shell/tests.rs)\n",
    );
    let stderr = refusal(&p);
    assert!(
        stderr.contains("1. The shell works") && stderr.contains("passes with the fix reverted"),
        "not reported PASSED for want of the test: {stderr}"
    );
}

#[test]
fn a_repro_task_that_starts_a_server_is_inconclusive_and_not_run() {
    let p = project(
        "\n[apps.api]\nbase_port = 39181\npath = \".\"\nlaunch = [\"git\", \"version\"]\n\n\
         [tasks.repro-up]\nsteps = [{ up = \"api\" }, { task = \"repro\" }]\n\n[pr.gates]\n\
         repro_task = \"repro-up\"\n",
        &format!("{FIX}\n{ALWAYS}"),
    );
    let body = opened_body(&p);
    assert!(
        body.contains("1. The lib is fixed: INCONCLUSIVE")
            && body.contains("2. The lib reads well: INCONCLUSIVE")
            && body.contains("`api`"),
        "{body}"
    );
    assert_eq!(repro_runs(&p), Vec::<i64>::new(), "nothing ran");
}

/// Where `devkit` keeps the repro check's throwaway worktrees.
fn repro_root(p: &ghfake::Fake) -> std::path::PathBuf {
    p.state().join("devkit").join("repro")
}

#[test]
fn the_throwaway_worktree_lives_in_the_state_dir_and_is_removed() {
    let p = project(GATE, FIX);
    opened_body(&p);
    let out = p.devkit_here(&["run", "runs", "list", "--task", "repro", "--json"]);
    let runs: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let root = repro_root(&p).canonicalize().unwrap();
    let worktrees: Vec<std::path::PathBuf> = runs
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["worktree"].as_str().unwrap().into())
        .collect();
    assert_eq!(worktrees.len(), 2, "{runs}");
    for worktree in worktrees {
        assert!(
            worktree.starts_with(&root),
            "{} in {}",
            worktree.display(),
            root.display()
        );
        assert!(!worktree.exists(), "{} is removed", worktree.display());
    }
}

#[test]
fn a_stale_throwaway_worktree_is_pruned_and_one_in_use_is_kept() {
    let p = project(GATE, FIX);
    let add = |name: &str| {
        let holder = repro_root(&p).join(name);
        std::fs::create_dir_all(&holder).unwrap();
        let lock = std::fs::File::create(holder.join("lock")).unwrap();
        let worktree = holder.join("repro");
        git(&p, &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ]);
        (holder, lock)
    };
    let (stale, _) = add("devkit-repro-stale");
    let (live, lock) = add("devkit-repro-live");
    lock.lock().unwrap();
    opened_body(&p);
    let listed = git(&p, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains("devkit-repro-stale"), "{listed}");
    assert!(!stale.exists(), "{}", stale.display());
    assert!(listed.contains("devkit-repro-live"), "{listed}");
    assert!(live.join("repro").exists());
}

#[test]
fn a_check_that_cannot_run_says_not_run_and_opens_the_pr() {
    let p = project(GATE, FIX);
    std::fs::create_dir_all(repro_root(&p).parent().unwrap()).unwrap();
    std::fs::write(repro_root(&p), "not a directory").unwrap();
    let body = opened_body(&p);
    assert!(
        body.contains("1. The lib is fixed: NOT RUN: the check could not run: creating "),
        "{body}"
    );
}

#[test]
fn a_fix_and_its_test_in_one_rust_file_are_checked_red_to_green() {
    let p = project(GATE, "");
    let calc = |answer: &str, test: &str| {
        format!(
            "// two: {answer}\nfn f() -> u8 {{ 2 }}\n\n#[cfg(test)]\nmod tests {{\n    // {test}\n}}\n"
        )
    };
    git(&p, &["checkout", "-q", "main"]);
    write(&p, "src/calc.rs", &calc("no", "old"));
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "a calculator"]);
    git(&p, &["checkout", "-q", "-"]);
    git(&p, &["rebase", "-q", "main"]);
    write(&p, "src/calc.rs", &calc("yes", "new"));
    write(&p, "tests/calc.txt", "two: yes\n");
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "fix the calculator"]);
    p.serve_pr_at(&PR, git(&p, &["rev-parse", "HEAD"]).trim());
    p.serve_issue("Why.\n\n## Done when\n\n- [ ] Two is right (proof: repro tests/calc.txt)\n");
    let body = opened_body(&p);
    assert!(body.contains("1. Two is right: PASSED"), "{body}");
    assert!(body.contains("`src/calc.rs`"), "{body}");
}

/// What `git` reports of the caller's worktree, index and worktree list.
fn snapshot(p: &ghfake::Fake) -> String {
    [
        &["status", "--porcelain=v1", "--untracked-files=all"][..],
        &["diff"],
        &["diff", "--cached"],
        &["worktree", "list", "--porcelain"],
        &["rev-parse", "HEAD"],
    ]
    .iter()
    .map(|args| git(p, args))
    .collect::<Vec<_>>()
    .join("\n--\n")
}

#[test]
fn the_callers_worktree_and_index_are_unchanged_pass_or_fail() {
    for items in [FIX, ALWAYS] {
        let p = project(GATE, items);
        write(&p, "src/lib.txt", "lib: being edited\n");
        write(&p, "staged.txt", "staged\n");
        git(&p, &["add", "staged.txt"]);
        write(&p, "untracked.txt", "untracked\n");
        let before = snapshot(&p);
        let _ = create(&p, "agent");
        assert_eq!(snapshot(&p), before, "{items}");
        assert_eq!(
            std::fs::read_to_string(p.project().join("src/lib.txt")).unwrap(),
            "lib: being edited\n"
        );
    }
}

#[test]
fn an_item_with_no_repro_marker_gets_an_advisory_line() {
    let p = project(GATE, &format!("{FIX}\n{UNMARKED}"));
    let body = opened_body(&p);
    assert!(
        body.contains("2. The docs say so: no `(proof: repro <selector>)` marker"),
        "{body}"
    );

    let p = project(GATE, UNMARKED);
    let body = opened_body(&p);
    assert!(body.contains("### Verification"), "{body}");
    assert!(
        body.contains("1. The docs say so: no `(proof: repro <selector>)` marker"),
        "{body}"
    );
}

#[test]
fn advise_reports_a_failed_check_and_opens_the_pr() {
    let p = project(
        &format!("{GATE}repro = \"advise\"\n"),
        &format!("{ALWAYS}\n{NONE}"),
    );
    let body = opened_body(&p);
    assert!(body.contains("1. The lib reads well"), "{body}");
    assert!(body.contains("passes with the fix reverted"), "{body}");
    assert!(body.contains("2. The lib is tested"), "{body}");
    assert!(body.contains("matches no test"), "{body}");
}

#[test]
fn a_human_is_shown_a_failed_check_and_never_refused() {
    let p = project(GATE, ALWAYS);
    let (ok, stderr) = create(&p, "human");
    assert!(ok, "{stderr}");
    assert!(
        body(&p).contains("passes with the fix reverted"),
        "{}",
        body(&p)
    );
}

#[test]
fn with_no_repro_task_or_with_the_gate_off_the_body_is_left_alone() {
    for gates in ["", &format!("{GATE}repro = \"off\"\n")] {
        let p = project(gates, ALWAYS);
        assert_eq!(opened_body(&p), "Fixes the lib.", "{gates}");
    }
}
