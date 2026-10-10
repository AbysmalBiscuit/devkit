//! A project with no `[forge]` table takes its forge and repositories from
//! `origin`.

#[path = "common/ghfake.rs"]
mod ghfake;

/// GitHub serves the same repository at `www.github.com`, so a clone URL copied
/// with that prefix names the project's repository and must default it.
#[test]
fn a_www_origin_supplies_the_pr_repository() {
    let fake = ghfake::Fake::with_origin("https://www.github.com/acme/widget.git");
    let out = fake.issue(&["checkout-pr", "17"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("not a github.com remote"),
        "the origin was refused: {stderr}"
    );
    let calls = fake.calls();
    assert!(
        calls
            .lines()
            .any(|c| c == "api --hostname github.com --method GET repos/acme/widget/pulls/17"),
        "PR #17 was not looked up in the origin's repository.\ngh calls:\n{calls}\nstderr: {stderr}"
    );
}

/// A GitHub Enterprise project names its host, and every `gh` call is pinned
/// to that host: without `--repo` or `--hostname`, `GH_HOST` would pick
/// another.
#[test]
fn an_enterprise_host_scopes_every_gh_call() {
    let fake = ghfake::Fake::with_origin_and(
        "[forge]\nkind = \"github\"\nhost = \"ghe.acme.test\"",
        "git@ghe.acme.test:acme/widget.git",
    );
    let out = fake.issue(&["checkout-pr", "17"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let calls = fake.calls();
    assert!(
        calls
            .lines()
            .any(|c| c == "api --hostname ghe.acme.test --method GET repos/acme/widget/pulls/17"),
        "PR #17 was not looked up on the enterprise host.\ngh calls:\n{calls}\nstderr: {stderr}"
    );
    assert!(
        !calls.contains("github.com/"),
        "a call escaped to github.com:\n{calls}"
    );
}

/// An `origin` on a host devkit does not recognize could be any forge, so
/// nothing is guessed: the PR command fails naming the key that settles it,
/// and `gh` is never asked about a repository on some other host.
#[test]
fn an_unknown_origin_host_fails_a_pr_command_without_calling_gh() {
    let fake = ghfake::Fake::with_origin("https://git.acme.test/acme/widget.git");
    let out = fake.issue(&["checkout-pr", "17"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "stderr: {stderr}");
    assert!(stderr.contains("[forge] kind"), "stderr: {stderr}");
    assert!(
        !fake.calls().lines().any(|c| c.starts_with("pr ")),
        "gh was called:\n{}",
        fake.calls()
    );
}

/// With no forge, `workspace status` still reports every worktree, holding each
/// one unfinished with the reason, rather than failing on the PR lookup.
#[test]
fn status_reports_worktrees_without_a_forge() {
    let fake = ghfake::Fake::with_origin("https://git.acme.test/acme/widget.git");
    let wts = tempfile::tempdir().unwrap();
    let wt = wts.path().join("eng-2");
    devkit_git::Git::fixture(fake.project())
        .args([
            "worktree",
            "add",
            "-q",
            "-b",
            "lev/eng-2-x",
            wt.to_str().unwrap(),
        ])
        .output()
        .expect("git worktree add");

    let out = fake.issue(&["status"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(stdout.contains("ENG-2"), "stdout: {stdout}");
    assert!(stdout.contains("no forge"), "stdout: {stdout}");
    assert!(fake.calls().is_empty(), "gh was called:\n{}", fake.calls());
}
