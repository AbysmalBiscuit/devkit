//! `pr create --attach` hands each file to `gh pr create`, and refuses
//! before the push whatever gh could not upload.

#[path = "common/ghfake.rs"]
mod ghfake;

const PR_9: ghfake::Pr = ghfake::Pr {
    number: 9,
    state: "OPEN",
    is_draft: true,
    author: "LevValle",
};

/// A bare repository to serve as `origin`, so a run that pushes leaves a ref
/// the test can see.
fn bare_origin() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("origin dir");
    devkit_git::Git::fixture(dir.path())
        .args(["init", "-q", "--bare"])
        .output()
        .expect("git init --bare");
    dir
}

/// Every ref `origin` holds, one per line.
fn pushed(origin: &tempfile::TempDir) -> String {
    devkit_git::Git::fixture(origin.path())
        .args(["for-each-ref"])
        .output()
        .expect("git for-each-ref")
}

#[test]
fn each_attachment_reaches_gh_as_its_own_flag() {
    let fake = ghfake::Fake::without_pr("");
    fake.create_opens(&PR_9);
    std::fs::write(fake.project().join("after.png"), "png").unwrap();
    std::fs::write(fake.project().join("demo.mp4"), "mp4").unwrap();

    let out = fake.issue(&[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "t",
        "--attach",
        "after.png#The login error state",
        "--attach",
        "demo.mp4",
    ]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    let create = fake
        .calls()
        .lines()
        .find(|c| c.starts_with("pr create"))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no gh pr create: {}", fake.calls()));
    assert!(
        create.contains("--attach after.png#The login error state --attach demo.mp4"),
        "{create}"
    );
}

#[test]
fn a_forge_without_attachments_refuses_before_the_push() {
    for kind in ["gitlab", "forgejo", "none"] {
        let origin = bare_origin();
        let fake = ghfake::Fake::with_origin_and(
            &format!("[forge]\nkind = \"{kind}\""),
            &origin.path().to_string_lossy(),
        );
        std::fs::write(fake.project().join("after.png"), "png").unwrap();

        let out = fake.issue(&["pr", "create", "--pr-title", "t", "--attach", "after.png"]);

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{kind}: {stderr}");
        assert!(
            stderr.contains("--attach") && stderr.contains(kind),
            "{kind}: the refusal names the flag and the forge: {stderr}"
        );
        assert_eq!(pushed(&origin), "", "{kind}: nothing is pushed");
    }
}

#[test]
fn a_missing_file_is_refused_before_the_push() {
    let origin = bare_origin();
    let fake = ghfake::Fake::with_origin_and(
        "[forge]\nkind = \"github\"\nrepo = \"o/r\"",
        &origin.path().to_string_lossy(),
    );
    fake.create_opens(&PR_9);

    let out = fake.issue(&[
        "pr",
        "create",
        "--pr-title",
        "t",
        "--attach",
        "missing.png#alt text",
    ]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("missing.png") && !stderr.contains("missing.png#"),
        "the refusal names the file, not the alt text: {stderr}"
    );
    assert_eq!(pushed(&origin), "", "nothing is pushed");
    assert!(!fake.calls().contains("pr create"), "{}", fake.calls());
}

/// Agents rerun `pr create` to push fixes, so appending to a PR that already
/// exists would stack another copy of each file in its body on every run.
#[test]
fn a_reused_pr_is_refused_before_it_is_touched() {
    let fake = ghfake::Fake::new("", &ghfake::Pr { number: 7, ..PR_9 });
    std::fs::write(fake.project().join("after.png"), "png").unwrap();

    let out = fake.issue(&[
        "pr",
        "create",
        "--no-push",
        "--to",
        "lev",
        "--attach",
        "after.png",
    ]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("#7") && stderr.contains("gh pr edit 7 --attach"),
        "the refusal names the PR and the way to attach to it: {stderr}"
    );
    let calls = fake.calls();
    assert!(
        !calls.contains("pr edit") && !calls.contains("pr create"),
        "{calls}"
    );
}

/// gh reads a whole argument that exists as a path before splitting off alt
/// text, since `#` is legal in a filename.
#[test]
fn a_hash_inside_an_existing_filename_is_not_alt_text() {
    let fake = ghfake::Fake::without_pr("");
    fake.create_opens(&PR_9);
    std::fs::write(fake.project().join("issue#206.png"), "png").unwrap();

    let out = fake.issue(&[
        "pr",
        "create",
        "--no-push",
        "--pr-title",
        "t",
        "--attach",
        "issue#206.png",
    ]);

    assert!(out.status.success(), "{out:?}\n{}", fake.calls());
    assert!(
        fake.calls().contains("--attach issue#206.png"),
        "{}",
        fake.calls()
    );
}
