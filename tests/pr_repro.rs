//! `devkit pr repro`: the repro gate's check on its own, printing each
//! marked item's verdict and both runs' exit codes, and touching neither
//! the forge nor the caller's worktree and index.

#[path = "common/ghfake.rs"]
mod ghfake;

/// A "test runner" that passes when a line of the test file the selector
/// names appears under `src/`, and exits 128 when the file does not exist.
const TASKS: &str = r#"
[tracker]
kind = "github"

[tasks.repro]
run = ["git", "grep", "--no-index", "-q", "-f", "{{ selector }}", "--", "src"]
"#;

const GATE: &str = "\n[pr.gates]\nrepro_task = \"repro\"\nrepro_no_tests_exit = 128\n";

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
/// reading `broken`, and whose branch fixes it and adds tests. The recorded
/// issue's `Done when` items are `items`.
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
    fake.record_issue("7");
    fake.serve_issue(&format!("Why.\n\n## Done when\n\n{items}\n"));
    fake
}

fn repro(p: &ghfake::Fake, extra: &[&str]) -> (bool, String, String) {
    let out = p.devkit_here_with_env(&[&["pr", "repro"][..], extra].concat(), &[(
        "DEVKIT_CALLER",
        "agent",
    )]);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
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

const FIX: &str = "- [ ] The lib is fixed (proof: repro tests/fix.txt)";
const ALWAYS: &str = "- [ ] The lib reads well (proof: repro tests/always.txt)";
const NONE: &str = "- [ ] The lib is tested (proof: repro tests/none.txt)";
const HEAD_FAIL: &str = "- [ ] The lib stays broken (proof: repro tests/head.txt)";

#[test]
fn each_marked_item_prints_its_verdict_and_both_exit_codes() {
    let p = project(GATE, &format!("{FIX}\n{ALWAYS}\n{NONE}\n{HEAD_FAIL}"));
    write(&p, "tests/head.txt", "missing\n");
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "a test that fails at HEAD"]);
    write(&p, "src/lib.txt", "lib: being edited\n");
    write(&p, "staged.txt", "staged\n");
    git(&p, &["add", "staged.txt"]);
    write(&p, "untracked.txt", "untracked\n");
    let before = snapshot(&p);
    let (ok, stdout, stderr) = repro(&p, &[]);
    assert!(!ok, "require refuses a failed item: {stdout}{stderr}");
    let head = git(&p, &["rev-parse", "HEAD"]).trim().to_string();
    for want in [
        "1. The lib is fixed: PASSED",
        "exited 1",
        "exited 0",
        "2. The lib reads well: FAILED",
        "3. The lib is tested: FAILED",
        "4. The lib stays broken: FAILS AT HEAD",
        &format!("At HEAD `{head}`"),
    ] {
        assert!(stdout.contains(want), "{want}: {stdout}\n{stderr}");
    }
    let codes = repro_runs(&p);
    assert_eq!(codes.len(), 8, "both runs of each item: {codes:?}");
    assert_eq!(snapshot(&p), before, "the caller's worktree and index stay");
    assert_eq!(
        std::fs::read_to_string(p.project().join("src/lib.txt")).unwrap(),
        "lib: being edited\n"
    );
    let calls = p.calls();
    assert!(
        !calls.contains("pr create") && !calls.contains("pr edit") && !calls.contains("graphql"),
        "the forge is not written: {calls}"
    );
}

#[test]
fn it_exits_nonzero_exactly_when_require_would_refuse() {
    let passing = project(GATE, FIX);
    let (ok, stdout, stderr) = repro(&passing, &[]);
    assert!(ok, "a red-to-green item passes: {stdout}{stderr}");
    assert!(stdout.contains("PASSED"), "{stdout}");

    for (gates, items, caller, refused) in [
        (GATE, ALWAYS, "agent", true),
        (GATE, NONE, "agent", true),
        (GATE, HEAD_FAIL, "agent", false),
        (GATE, FIX, "human", false),
        (
            &format!("{GATE}repro = \"advise\"\n"),
            ALWAYS,
            "agent",
            false,
        ),
        (&format!("{GATE}repro = \"off\"\n"), ALWAYS, "agent", false),
    ] {
        let p = project(gates, items);
        if items == HEAD_FAIL {
            write(&p, "tests/head.txt", "missing\n");
            git(&p, &["add", "-A"]);
            git(&p, &["commit", "-q", "-m", "a test that fails at HEAD"]);
        }
        let out = p.devkit_here_with_env(&["pr", "repro"], &[("DEVKIT_CALLER", caller)]);
        assert_eq!(
            !out.status.success(),
            refused,
            "{caller} {gates:?} {items}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

const UNMARKED: &str = "- [ ] The docs say so (proof: review)";
const NO_SELECTOR: &str = "- [ ] The lib is proven (proof: repro)";

#[test]
fn json_prints_one_object_per_item_with_the_same_verdicts() {
    let p = project(GATE, &format!("{FIX}\n{ALWAYS}\n{NONE}\n{UNMARKED}"));
    let (_ok, stdout, stderr) = repro(&p, &["--json"]);
    let items: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}\n{stderr}")))
        .collect();
    assert_eq!(items.len(), 4, "{stdout}");
    let verdict = |n: u64| {
        items
            .iter()
            .find(|i| i["number"] == n)
            .unwrap_or_else(|| panic!("item {n}: {stdout}"))
    };
    assert_eq!(verdict(1)["verdict"], "PASSED");
    assert_eq!(verdict(1)["reverted_exit"], 1);
    assert_eq!(verdict(1)["head_exit"], 0);
    assert_eq!(verdict(2)["verdict"], "FAILED");
    assert_eq!(verdict(2)["head_exit"], 0);
    assert_eq!(verdict(2)["reverted_exit"], 0);
    assert_eq!(verdict(3)["verdict"], "FAILED");
    assert_eq!(verdict(3)["head_exit"], 128);
    assert!(verdict(4).get("verdict").is_none(), "{}", verdict(4));
    assert!(verdict(4).get("head_exit").is_none(), "{}", verdict(4));
    assert!(verdict(4).get("reverted_exit").is_none(), "{}", verdict(4));
    assert!(stdout.contains("1. The lib is fixed"), "{stdout}");

    let mixed = project(GATE, FIX);
    write(
        &mixed,
        "src/mixed.rs",
        "fn f() -> u8 { 2 }\n\n#[cfg(test)]\nmod tests {\n}\n",
    );
    git(&mixed, &["add", "-A"]);
    git(&mixed, &["commit", "-q", "-m", "a module and its test"]);
    let (ok, stdout, stderr) = repro(&mixed, &["--json"]);
    assert!(ok, "inconclusive does not refuse: {stderr}");
    let item: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap())
        .unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(item["verdict"], "INCONCLUSIVE", "{stdout}");
    assert!(item.get("head_exit").is_none(), "{item}");
    assert!(item.get("reverted_exit").is_none(), "{item}");

    let blank = project(GATE, NO_SELECTOR);
    let (ok, stdout, stderr) = repro(&blank, &["--json"]);
    assert!(ok, "a marker with no selector does not refuse: {stderr}");
    let item: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap())
        .unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(item["verdict"], "INCONCLUSIVE", "{stdout}");
}

#[test]
fn a_blank_flag_and_a_worktree_with_untracked_files_are_refused_cleanly() {
    let p = project(GATE, FIX);
    write(&p, "notes.txt", "left behind by the caller\n");
    let before = snapshot(&p);

    let out = p.devkit_here_with_env(&["pr", "repro", "--json", ""], &[(
        "DEVKIT_CALLER",
        "agent",
    )]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("unexpected argument"),
        "a blank argument is a usage error: {text}"
    );
    assert_eq!(repro_runs(&p), Vec::<i64>::new(), "nothing ran");

    let out = p.devkit_here_with_env(&["pr", "repro", "--json", "  "], &[(
        "DEVKIT_CALLER",
        "agent",
    )]);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let (ok, stdout, stderr) = repro(&p, &["--json"]);
    assert!(ok, "untracked files do not stop a passing check: {stderr}");
    assert!(stdout.contains("\"verdict\":\"PASSED\""), "{stdout}");
    assert_eq!(snapshot(&p), before, "the untracked file stays");
    assert!(
        std::fs::read_to_string(p.project().join("notes.txt"))
            .unwrap()
            .contains("left behind")
    );
}

#[test]
fn a_missing_issue_or_merge_base_names_what_is_missing() {
    let no_issue = project(GATE, FIX);
    std::fs::remove_file(no_issue.project().join(".devkit/issue.toml")).unwrap();
    let (ok, stdout, stderr) = repro(&no_issue, &[]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("no repro check"), "{stdout}");
    assert!(no_issue.calls().is_empty(), "{}", no_issue.calls());

    let no_base = project(GATE, FIX);
    git(&no_base, &["branch", "-D", "main"]);
    let (ok, stdout, stderr) = repro(&no_base, &[]);
    assert!(!ok, "{stdout}{stderr}");
    assert!(
        stderr.contains("neither `origin/main` nor `main` is a commit here"),
        "{stderr}"
    );
    assert_eq!(repro_runs(&no_base), Vec::<i64>::new());
}

/// A task whose shell kills itself, committed before the branch so the
/// reverted run uses it too. A task the branch itself changes is restored
/// with the fix, and the reverted run would not be the killed one.
#[cfg(unix)]
const KILL: &str = r#"
[tracker]
kind = "github"

[tasks.repro]
run = ["sh", "-c", "kill -TERM $$; echo {{ selector }}"]
"#;

#[cfg(unix)]
#[test]
fn a_killed_selector_prints_signal_not_an_exit_code() {
    let p = project("\n[pr.gates]\nrepro_task = \"repro\"\n", FIX);
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config)
        .unwrap()
        .replace(TASKS, KILL);
    std::fs::write(&config, toml).unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "--amend", "--no-edit"]);
    git(&p, &["branch", "-f", "main", "HEAD"]);
    write(&p, "src/lib.txt", "lib: fixed again\n");
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "fix the lib again"]);
    let (ok, stdout, stderr) = repro(&p, &["--json"]);
    assert!(ok, "a signal does not refuse: {stdout}{stderr}");
    let item: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap())
        .unwrap_or_else(|e| panic!("{e}: {stdout}{stderr}"));
    assert_eq!(item["head_exit"], "signal", "{item}");
    assert_eq!(item["reverted_exit"], "signal", "{item}");
    assert!(stdout.contains("killed by a signal"), "{stdout}");
}

#[test]
fn json_omits_both_exits_when_the_selector_did_not_run() {
    let unmarked = project(GATE, UNMARKED);
    let (ok, stdout, stderr) = repro(&unmarked, &["--json"]);
    assert!(ok, "{stderr}");
    let item: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap())
        .unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert!(item.get("head_exit").is_none(), "{item}");
    assert!(item.get("reverted_exit").is_none(), "{item}");

    let mixed = project(GATE, FIX);
    write(
        &mixed,
        "src/mixed.rs",
        "fn f() -> u8 { 2 }\n\n#[cfg(test)]\nmod tests {\n}\n",
    );
    git(&mixed, &["add", "-A"]);
    git(&mixed, &["commit", "-q", "-m", "a module and its test"]);
    let (ok, stdout, stderr) = repro(&mixed, &["--json"]);
    assert!(ok, "{stderr}");
    let item: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap())
        .unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(item["verdict"], "INCONCLUSIVE", "{item}");
    assert!(item.get("head_exit").is_none(), "{item}");
    assert!(item.get("reverted_exit").is_none(), "{item}");
}

#[test]
fn base_selects_the_merge_base_the_check_reverts_to() {
    let p = project(GATE, FIX);
    git(&p, &["branch", "release", "main"]);
    write(&p, "src/lib.txt", "lib: broken again\n");
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "break it after release"]);
    git(&p, &["branch", "-f", "main", "HEAD"]);
    write(&p, "src/lib.txt", "lib: fixed\n");
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "fix it again"]);
    git(&p, &["branch", "-f", "main", "HEAD"]);

    let (ok, stdout, stderr) = repro(&p, &["--base", "release"]);
    assert!(ok, "the fix is red against release: {stdout}{stderr}");
    assert!(stdout.contains("PASSED"), "{stdout}");
    assert!(stdout.contains("restored to the merge base"), "{stdout}");

    let (ok, stdout, stderr) = repro(&p, &[]);
    assert!(ok, "against main the source matches HEAD: {stderr}");
    assert!(
        stdout.contains("no non-test path"),
        "the default base is main, not release: {stdout}"
    );

    let blank = p.devkit_here_with_env(&["pr", "repro", "--base", ""], &[(
        "DEVKIT_CALLER",
        "agent",
    )]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&blank.stdout),
        String::from_utf8_lossy(&blank.stderr)
    );
    assert!(!blank.status.success(), "{text}");
    assert!(
        text.contains("neither `origin/` nor `` is a commit here"),
        "{text}"
    );

    let missing = p.devkit_here_with_env(&["pr", "repro", "--base", "no-such"], &[(
        "DEVKIT_CALLER",
        "agent",
    )]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&missing.stdout),
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(!missing.status.success(), "{text}");
    assert!(text.contains("pass --base"), "{text}");
}

/// A task that exits 143, the number a SIGTERM death is stored as.
/// It has a real exit code, so it must not read as a signal.
const EXIT_143: &str = r#"
[tracker]
kind = "github"

[tasks.repro]
run = ["sh", "-c", "echo {{ selector }}; exit 143"]
"#;

#[test]
fn a_real_exit_above_128_stays_that_code_and_refuses_when_it_names_no_test() {
    let p = project(
        "\n[pr.gates]\nrepro_task = \"repro\"\nrepro_no_tests_exit = 143\n",
        FIX,
    );
    let config = p.project().join("devkit.toml");
    let toml = std::fs::read_to_string(&config)
        .unwrap()
        .replace(TASKS, EXIT_143);
    std::fs::write(&config, toml).unwrap();
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "--amend", "--no-edit"]);
    git(&p, &["branch", "-f", "main", "HEAD"]);
    write(&p, "src/lib.txt", "lib: fixed again\n");
    git(&p, &["add", "-A"]);
    git(&p, &["commit", "-q", "-m", "fix the lib again"]);

    let (ok, stdout, stderr) = repro(&p, &["--json"]);
    assert!(
        !ok,
        "143 is the no-test exit, so require refuses: {stdout}{stderr}"
    );
    let item: serde_json::Value = serde_json::from_str(stdout.lines().next().unwrap())
        .unwrap_or_else(|e| panic!("{e}: {stdout}{stderr}"));
    assert_eq!(item["verdict"], "FAILED", "{item}");
    assert_eq!(item["head_exit"], 143, "{item}");
    assert_eq!(item["reverted_exit"], 143, "{item}");
    assert!(!stdout.contains("signal"), "{stdout}");
}

fn caller_repro(p: &ghfake::Fake, caller: &str) -> (bool, String, String) {
    let out = p.devkit_here_with_env(&["pr", "repro"], &[("DEVKIT_CALLER", caller)]);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_human_exits_zero_when_the_check_cannot_read_its_settings() {
    let no_base = project(GATE, FIX);
    git(&no_base, &["branch", "-D", "main"]);
    let (ok, stdout, stderr) = caller_repro(&no_base, "human");
    let text = format!("{stdout}{stderr}");
    assert!(ok, "a human with no base exits zero: {text}");
    assert!(
        text.contains("neither `origin/main` nor `main` is a commit here"),
        "{text}"
    );

    let (ok, stdout, stderr) = caller_repro(&no_base, "agent");
    assert!(!ok, "an agent with no base still fails: {stdout}{stderr}");

    let broken = project(GATE, FIX);
    let config = broken.project().join("devkit.toml");
    let good = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, "this is not toml\n").unwrap();
    git(&broken, &["add", "-A"]);
    git(&broken, &["commit", "-q", "--amend", "--no-edit"]);
    git(&broken, &["branch", "-f", "main", "HEAD"]);
    std::fs::write(&config, good).unwrap();
    git(&broken, &["add", "-A"]);
    git(&broken, &[
        "commit",
        "-q",
        "-m",
        "restore a readable config at HEAD",
    ]);
    let (ok, stdout, stderr) = caller_repro(&broken, "human");
    let text = format!("{stdout}{stderr}");
    assert!(ok, "a human with an unreadable config exits zero: {text}");
    assert!(text.contains("reading devkit.toml"), "{text}");

    let (ok, stdout, stderr) = caller_repro(&broken, "agent");
    assert!(
        !ok,
        "an agent with an unreadable config still fails: {stdout}{stderr}"
    );
    assert!(
        format!("{stdout}{stderr}").contains("reading devkit.toml"),
        "{stdout}{stderr}"
    );
}
