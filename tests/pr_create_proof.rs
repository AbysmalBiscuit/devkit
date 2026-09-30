//! `issue pr create` refuses an agent's proof that skips an item the issue is
//! done when, before anything is pushed.

#[path = "common/ghfake.rs"]
mod ghfake;

/// A GitHub-tracked project whose `pr_body` is the `proof` arg and whose
/// `defaults.pr_proof_variable` names it, with no PR yet and an `origin` that
/// records what was pushed.
struct Project {
    fake: ghfake::Fake,
    origin: tempfile::TempDir,
}

const CONFIG: &str = r#"pr_proof_variable = "proof"

[tracker]
kind = "github"

[templates]
pr_body = "{{ proof }}"

[templates.variables]
proof = { default = "", required = "agents" }
"#;

const ISSUE: &str = "\
Why this matters.

## Done when

- [ ] A gap is refused (proof: test)
- [ ] A covered run opens the PR (proof: test)
- [ ] Acceptance criteria parse (proof: test)

## Out of scope

- Everything else
";

fn project() -> Project {
    let fake = ghfake::Fake::without_pr(CONFIG);
    let origin = tempfile::tempdir().expect("origin dir");
    devkit_git::Git::fixture(origin.path())
        .args(["init", "-q", "--bare"])
        .output()
        .expect("git init --bare");
    let url = origin.path().to_str().expect("utf-8 origin path");
    devkit_git::Git::fixture(fake.project())
        .args(["remote", "add", "origin", url])
        .output()
        .expect("git remote add");
    fake.create_opens(&ghfake::Pr {
        number: 9,
        state: "OPEN",
        is_draft: true,
        author: "LevValle",
    });
    Project { fake, origin }
}

impl Project {
    fn create(&self, caller: &str, proof: &str) -> (bool, String) {
        let arg = format!("proof={proof}");
        let out = self.fake.issue_as(caller, &[
            "pr",
            "create",
            "--pr-title",
            "feat: a thing",
            "--arg",
            &arg,
        ]);
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn pushed(&self) -> bool {
        devkit_git::Git::fixture(self.origin.path())
            .args(["rev-parse", "--verify", "-q", "refs/heads/lev/eng-1-fix"])
            .success()
            .expect("git rev-parse")
    }

    fn opened(&self) -> bool {
        self.fake.calls().contains("pr create")
    }
}

#[test]
fn a_proof_that_skips_an_item_is_refused_before_the_push() {
    for section in [
        "## Done when",
        "## ACCEPTANCE CRITERIA",
        "**Acceptance criteria:**",
    ] {
        let p = project();
        p.fake.record_issue("7");
        p.fake.serve_issue(&ISSUE.replace("## Done when", section));

        let (ok, stderr) = p.create("agent", "1. test `gap`\n3. test `ac`");

        assert!(!ok, "{section}: a skipped item must refuse: {stderr}");
        assert!(
            stderr.contains("2. A covered run opens the PR (proof: test)"),
            "{section}: names the missing item: {stderr}"
        );
        assert!(
            !stderr.contains("1. A gap is refused"),
            "{section}: lists only what is missing: {stderr}"
        );
        assert!(!p.pushed(), "{section}: nothing is pushed");
        assert!(
            !p.opened(),
            "{section}: nothing is opened: {}",
            p.fake.calls()
        );
    }
}

#[test]
fn a_proof_covering_every_item_opens_the_pr() {
    let p = project();
    p.fake.record_issue("7");
    p.fake.serve_issue(ISSUE);

    let proof = "1. test `gap`\n2. test `opens`\n3. test `ac`";
    let (ok, stderr) = p.create("agent", proof);

    assert!(ok, "every item is covered: {stderr}");
    assert!(p.pushed(), "the branch is pushed");
    assert!(
        p.fake.calls().contains(&format!("--body {proof}")),
        "the proof is the body: {}",
        p.fake.calls()
    );
}

#[test]
fn an_issue_without_the_section_is_not_gated() {
    let p = project();
    p.fake.record_issue("7");
    p.fake.serve_issue("Just prose, no items.\n");

    let (ok, stderr) = p.create("agent", "covered no item by number");
    assert!(ok, "{stderr}");
    assert!(p.opened());
}

#[test]
fn a_run_outside_an_issue_worktree_is_not_gated() {
    let p = project();
    p.fake.serve_issue(ISSUE);

    let (ok, stderr) = p.create("agent", "covered no item by number");
    assert!(ok, "{stderr}");
    assert!(
        !p.fake.calls().contains("api graphql"),
        "with no issue there is nothing to look up: {}",
        p.fake.calls()
    );
}

#[test]
fn a_human_caller_is_not_gated() {
    let p = project();
    p.fake.record_issue("7");
    p.fake.serve_issue(ISSUE);

    let (ok, stderr) = p.create("human", "covered no item by number");
    assert!(ok, "{stderr}");
    assert!(p.opened());
}

/// `pr checkout` records `UNKNOWN` when the PR names no issue, which leaves
/// nothing to ask the tracker about.
#[test]
fn a_checkout_worktree_with_no_issue_is_not_gated() {
    let p = project();
    p.fake.record_issue("UNKNOWN");
    p.fake.serve_issue(ISSUE);

    let (ok, stderr) = p.create("agent", "covered no item by number");
    assert!(ok, "{stderr}");
    assert!(
        !p.fake.calls().contains("api graphql"),
        "{}",
        p.fake.calls()
    );
}
