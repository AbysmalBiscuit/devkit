//! A project with no `[github]` table takes its repositories from `origin`.

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
            .any(|c| c.starts_with("pr view 17") && c.ends_with("--repo github.com/acme/widget")),
        "PR #17 was not looked up in the origin's repository.\ngh calls:\n{calls}\nstderr: {stderr}"
    );
}
