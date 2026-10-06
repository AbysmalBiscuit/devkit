//! The single-PR operations reach GitHub over REST, so they work where GitHub
//! refuses GraphQL, as the Claude Code cloud proxy does.

#[path = "common/ghfake.rs"]
mod ghfake;

/// The REST create, as the fake `gh` logs it.
const REST_CREATE: &str = "--method POST repos/o/r/pulls -f";

fn ready_pr(number: u64) -> ghfake::Pr {
    ghfake::Pr {
        number,
        state: "OPEN",
        is_draft: false,
        author: "LevValle",
    }
}

/// A bare `origin` the project pushes to.
fn with_origin(fake: &ghfake::Fake) -> tempfile::TempDir {
    let origin = tempfile::tempdir().unwrap();
    devkit_git::Git::fixture(origin.path())
        .args(["init", "-q", "--bare"])
        .output()
        .unwrap();
    devkit_git::Git::fixture(fake.project())
        .args(["remote", "add", "origin"])
        .args([origin.path().to_str().unwrap()])
        .output()
        .unwrap();
    origin
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn pr_create_opens_a_ready_pr_over_rest_when_graphql_is_refused() {
    let fake = ghfake::Fake::without_pr("[templates]\npr_body = \"Closes {{ issue }}\"");
    fake.record_issue("ENG-1");
    fake.refuse_graphql();
    fake.create_opens(&ready_pr(7));
    let origin = with_origin(&fake);

    let out = fake.issue(&["pr", "create", "--ready", "--pr-title", "fix it"]);

    let calls = fake.calls();
    assert!(out.status.success(), "{out:?}\n{calls}");
    assert_eq!(stdout(&out), "https://github.com/o/r/pull/7\n");
    let create = calls
        .lines()
        .find(|l| l.contains(REST_CREATE))
        .unwrap_or_else(|| panic!("no REST create: {calls}"));
    for field in [
        "-f title=fix it",
        "-f head=lev/eng-1-fix",
        "-f base=main",
        "-f body=Closes ENG-1",
        "-F draft=false",
    ] {
        assert!(create.contains(field), "{field} missing: {create}");
    }
    let pushed = devkit_git::Git::fixture(origin.path())
        .args(["rev-parse", "refs/heads/lev/eng-1-fix"])
        .output()
        .unwrap();
    assert_eq!(pushed.trim(), fake.head(), "the branch was pushed");
}

#[test]
fn pr_create_reports_the_existing_pr_when_graphql_is_refused() {
    let fake = ghfake::Fake::new("", &ready_pr(7));
    fake.refuse_graphql();

    let out = fake.issue(&["pr", "create", "--no-push", "--pr-title", "t"]);

    let calls = fake.calls();
    assert!(out.status.success(), "{out:?}\n{calls}");
    assert_eq!(stdout(&out), "https://github.com/o/r/pull/7\n");
    assert!(
        calls.contains("pulls?head=o%3Alev%2Feng-1-fix&state=all"),
        "no REST lookup by owner-qualified head: {calls}"
    );
    assert!(
        !calls.contains(REST_CREATE),
        "a second PR was opened: {calls}"
    );
}

#[test]
fn pr_create_still_opens_through_gh_when_graphql_answers() {
    let fake = ghfake::Fake::without_pr("");
    fake.create_opens(&ready_pr(7));

    let out = fake.issue(&["pr", "create", "--no-push", "--pr-title", "t"]);

    let calls = fake.calls();
    assert!(out.status.success(), "{out:?}\n{calls}");
    assert_eq!(stdout(&out), "https://github.com/o/r/pull/7\n");
    assert!(calls.contains("pr create --base main --title t"), "{calls}");
    assert!(!calls.contains(REST_CREATE), "{calls}");
}

#[test]
fn a_create_failure_naming_graphql_in_the_title_is_not_sent_to_rest() {
    let fake = ghfake::Fake::without_pr("");
    fake.create_fails("pull request create failed: base branch not found");

    let out = fake.issue(&["pr", "create", "--no-push", "--pr-title", "GraphQL 403"]);

    let calls = fake.calls();
    assert!(!out.status.success(), "{out:?}\n{calls}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("base branch not found"),
        "{out:?}"
    );
    assert!(!calls.contains(REST_CREATE), "{calls}");
}

#[test]
fn a_rest_create_failure_naming_a_404_in_the_body_is_reported() {
    let fake =
        ghfake::Fake::without_pr("[templates]\npr_body = \"Fails with (HTTP 404) in {{ issue }}\"");
    fake.record_issue("ENG-1");
    fake.refuse_graphql();
    fake.create_fails("gh: Validation Failed (HTTP 422)");

    let out = fake.issue(&["pr", "create", "--no-push", "--ready", "--pr-title", "t"]);

    let calls = fake.calls();
    assert!(calls.contains(REST_CREATE), "{calls}");
    assert!(!out.status.success(), "{out:?}\n{calls}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Validation Failed (HTTP 422)"),
        "{out:?}"
    );
}

#[test]
fn review_request_adds_reviewers_over_rest_when_graphql_is_refused() {
    let fake = ghfake::Fake::new("", &ghfake::Pr {
        author: "someone-else",
        ..ready_pr(7)
    });
    fake.refuse_graphql();

    let out = fake.issue(&["review", "request", "--no-push", "--to", "lev"]);

    let calls = fake.calls();
    assert!(out.status.success(), "{out:?}\n{calls}");
    assert!(
        calls.contains(
            "--method POST repos/o/r/pulls/7/requested_reviewers -f reviewers[]=LevValle"
        ),
        "{calls}"
    );
}

#[test]
fn doctor_reaches_github_through_gh_when_graphql_is_refused() {
    let fake = ghfake::Fake::without_pr("");
    fake.refuse_graphql();

    let out = fake.devkit_here(&["doctor"]);

    let text = stdout(&out);
    let row = text
        .lines()
        .find(|l| l.contains("github on github.com"))
        .unwrap_or_else(|| panic!("no forge row: {text}\n{}", fake.calls()));
    assert!(row.contains("LevValle"), "{row}\n{}", fake.calls());
    assert!(
        fake.calls().contains("--method GET user"),
        "{}",
        fake.calls()
    );
}
