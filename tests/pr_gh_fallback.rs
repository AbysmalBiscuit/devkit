//! A PR fetched by number, one a command just opened, recorded, or was handed,
//! is read through `gh api` when no GitHub token resolves.

#[path = "common/ghfake.rs"]
mod ghfake;

const PR_7: ghfake::Pr = ghfake::Pr {
    number: 7,
    state: "OPEN",
    is_draft: true,
    author: "LevValle",
};

#[test]
fn pr_create_verifies_the_pr_it_opened() {
    let fake = ghfake::Fake::without_pr("");
    fake.create_opens(&PR_7);

    let out = fake.issue(&["pr", "create", "--no-push", "--pr-title", "t"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert!(
        fake.calls().contains("--method GET repos/o/r/pulls/7"),
        "{}",
        fake.calls()
    );
}

#[test]
fn pr_create_reuses_the_recorded_pr() {
    // `gh pr list` knows no PR for the branch, so only the record finds #7.
    let fake = ghfake::Fake::without_pr("");
    fake.serve_pr(&PR_7);
    let record = fake.project().join(".devkit");
    std::fs::create_dir_all(&record).unwrap();
    std::fs::write(
        record.join("issue.toml"),
        "issue = \"ENG-1\"\nslug = \"fix\"\napps = []\n\n[pr]\nrepo = \"o/r\"\nnumber = 7\n",
    )
    .unwrap();

    let out = fake.issue(&["pr", "create", "--no-push"]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert!(!fake.calls().contains("pr create"), "{}", fake.calls());
}

#[test]
fn pr_checkout_records_the_pr_it_checked_out() {
    let fake = ghfake::Fake::without_pr("");
    fake.serve_pr(&PR_7);
    let origin = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(origin.path())
        .args(["init", "-q", "--bare"])
        .output()
        .unwrap();
    let git = || devkit_git::Git::fixture(fake.project());
    git()
        .args(["remote", "add", "origin"])
        .args([origin.path().to_str().unwrap()])
        .output()
        .unwrap();
    git()
        .args(["push", "-q", "origin", "HEAD:main"])
        .output()
        .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let worktree = scratch.path().join("pr-7");

    let out = fake.issue(&["pr", "checkout", "#7", worktree.to_str().unwrap()]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    let record = std::fs::read_to_string(worktree.join(".devkit").join("issue.toml")).unwrap();
    assert!(record.contains("number = 7"), "{record}");
}
