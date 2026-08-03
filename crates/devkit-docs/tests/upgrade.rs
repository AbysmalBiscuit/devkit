mod common;

#[test]
fn a_nested_scoped_cache_migrates_and_its_worktree_still_works() {
    let base = common::unique_tmp("upgrade");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");

    // Build a 0.12.x-shaped cache by hand: nested scope, worktree, no origin.
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    let bare = old.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", &repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    devkit_common::cmd::capture(
        "git",
        &[
            "worktree",
            "add",
            "--detach",
            old.join("v1.0.0").to_str().unwrap(),
            "v1.0.0",
        ],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();

    // Capture what the worktree pointed at, so the assertion below is about
    // preservation rather than merely about the directory existing.
    let before_head = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "HEAD"],
        Some(old.join("v1.0.0").to_str().unwrap()),
    )
    .unwrap()
    .trim()
    .to_string();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(!done.is_empty());

    let new = cache.join("@scope~pkg");
    assert!(new.is_dir());
    assert!(!cache.join("@scope").exists());

    // A clean status alone would also pass for a worktree pointing at the wrong
    // commit, so assert the exact HEAD.
    let after_head = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "HEAD"],
        Some(new.join("v1.0.0").to_str().unwrap()),
    )
    .unwrap()
    .trim()
    .to_string();
    assert_eq!(
        after_head, before_head,
        "the migrated worktree must keep its commit"
    );

    let status = devkit_common::cmd::capture(
        "git",
        &["status", "--porcelain"],
        Some(new.join("v1.0.0").to_str().unwrap()),
    )
    .unwrap();
    assert!(status.trim().is_empty());

    // The exact origin, not merely "some origin": a wrong URL here would make
    // the mismatch guard reject the library forever after.
    let meta = devkit_docs::cache::read_meta(&new);
    assert_eq!(meta.origin.as_deref(), Some(repo.as_str()));

    // Idempotent.
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}

#[test]
fn a_worktree_whose_link_cannot_be_repaired_is_rebuilt_at_its_recorded_commit() {
    let base = common::unique_tmp("upgrade-broken");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    let bare = old.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", &repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    devkit_common::cmd::capture(
        "git",
        &[
            "worktree",
            "add",
            "--detach",
            old.join("v1.0.0").to_str().unwrap(),
            "v1.0.0",
        ],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();
    let head = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "HEAD"],
        Some(old.join("v1.0.0").to_str().unwrap()),
    )
    .unwrap()
    .trim()
    .to_string();

    // Break the worktree's link to its administrative directory beyond repair.
    std::fs::write(
        old.join("v1.0.0").join(".git"),
        "gitdir: /nonexistent/elsewhere",
    )
    .unwrap();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(
        done.iter().any(|l| l.contains("rebuilt")),
        "the rebuild must be reported: {done:?}"
    );

    // Rebuilt, at the same commit, and usable — not left registered-but-absent
    // for prune to trip over.
    let new = cache.join("@scope~pkg").join("v1.0.0");
    assert_eq!(
        devkit_common::cmd::capture("git", &["rev-parse", "HEAD"], Some(new.to_str().unwrap()))
            .unwrap()
            .trim(),
        head
    );
    assert!(
        devkit_docs::upgrade::run(&cache).unwrap().is_empty(),
        "still idempotent"
    );
}

#[test]
fn a_crash_between_rename_and_repair_is_finished_by_the_next_run() {
    let base = common::unique_tmp("upgrade-resume");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    let bare = old.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", &repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    devkit_common::cmd::capture(
        "git",
        &[
            "worktree",
            "add",
            "--detach",
            old.join("v1.0.0").to_str().unwrap(),
            "v1.0.0",
        ],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();
    let head = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "HEAD"],
        Some(old.join("v1.0.0").to_str().unwrap()),
    )
    .unwrap()
    .trim()
    .to_string();

    // Exactly the state a crash after `fs::rename` and before `worktree repair`
    // leaves: renamed, so it looks migrated, but every absolute link is stale.
    std::fs::rename(&old, cache.join("@scope~pkg")).unwrap();
    std::fs::remove_dir_all(cache.join("@scope")).ok();
    let moved = cache.join("@scope~pkg").join("v1.0.0");
    assert!(
        devkit_common::cmd::capture(
            "git",
            &["status", "--porcelain"],
            Some(moved.to_str().unwrap())
        )
        .is_err(),
        "the fixture must actually be broken, or this test proves nothing"
    );

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(
        !done.is_empty(),
        "phase 5 must repair a library that needs no rename"
    );
    assert_eq!(
        devkit_common::cmd::capture("git", &["rev-parse", "HEAD"], Some(moved.to_str().unwrap()))
            .unwrap()
            .trim(),
        head
    );
    assert!(
        devkit_docs::upgrade::run(&cache).unwrap().is_empty(),
        "and then settle"
    );
}

#[test]
fn a_crash_between_worktree_prune_and_worktree_add_is_recovered_from_the_journal() {
    let base = common::unique_tmp("upgrade-journal");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    std::fs::create_dir_all(&lib).unwrap();
    let bare = lib.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", &repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    let head = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "v1.0.0^{commit}"],
        Some(bare.to_str().unwrap()),
    )
    .unwrap()
    .trim()
    .to_string();

    // The state a crash after `worktree prune` and before `worktree add` leaves:
    // no directory, no admin entry, nothing on disk naming the checkout except
    // the journal the previous run wrote before it started mutating.
    let journal = cache
        .join("registry.locks")
        .join("@scope~pkg.migration.json");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        format!(r#"{{"worktrees":[{{"dirname":"v1.0.0","commit":"{head}"}}]}}"#),
    )
    .unwrap();
    assert!(!lib.join("v1.0.0").exists());

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(
        done.iter().any(|l| l.contains("v1.0.0")),
        "the recovery must be reported: {done:?}"
    );
    assert_eq!(
        devkit_common::cmd::capture(
            "git",
            &["rev-parse", "HEAD"],
            Some(lib.join("v1.0.0").to_str().unwrap()),
        )
        .unwrap()
        .trim(),
        head
    );
    assert!(
        !journal.exists(),
        "the journal is cleared once its worktrees are back"
    );
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}

#[test]
fn a_rename_whose_target_already_exists_migrates_nothing() {
    let base = common::unique_tmp("upgrade-collide");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");

    // Old nested form and the new encoded form both present.
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    devkit_common::cmd::capture(
        "git",
        &[
            "clone",
            "--bare",
            &repo,
            old.join("repo.git").to_str().unwrap(),
        ],
        None,
    )
    .unwrap();
    let new = cache.join("@scope~pkg");
    std::fs::create_dir_all(&new).unwrap();

    let err = devkit_docs::upgrade::run(&cache).unwrap_err().to_string();
    assert!(
        err.contains("@scope~pkg"),
        "the error must name the target: {err}"
    );
    assert!(
        err.contains("already exists"),
        "an occupied target is not a case-folding collision: {err}"
    );
    // Nothing moved: a refused migration leaves the cache exactly as it was.
    assert!(old.join("repo.git").is_dir());
}

#[test]
fn an_already_migrated_cache_is_left_alone() {
    let base = common::unique_tmp("upgrade-noop");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    std::fs::create_dir_all(&lib).unwrap();
    devkit_common::cmd::capture(
        "git",
        &[
            "clone",
            "--bare",
            &repo,
            lib.join("repo.git").to_str().unwrap(),
        ],
        None,
    )
    .unwrap();

    // Only the origin backfill runs, and only once.
    assert_eq!(devkit_docs::upgrade::run(&cache).unwrap().len(), 1);
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}

#[test]
fn a_stale_administrative_back_pointer_is_repaired_before_prune_can_read_it_as_abandoned() {
    let base = common::unique_tmp("upgrade-backptr");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    std::fs::create_dir_all(&lib).unwrap();
    let bare = lib.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", &repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    devkit_common::cmd::capture(
        "git",
        &[
            "worktree",
            "add",
            "--detach",
            lib.join("v1.0.0").to_str().unwrap(),
            "v1.0.0",
        ],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();

    // Only the administration's half of the link is stale. The checkout itself
    // still resolves, so a pass that checks one direction sees nothing wrong —
    // and leaves an entry `git worktree prune` reads as abandoned.
    let back = bare.join("worktrees/v1.0.0/gitdir");
    std::fs::write(&back, "/nonexistent/elsewhere/.git\n").unwrap();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(
        done.iter().any(|l| l.contains("repaired")),
        "the stale back-pointer must be repaired: {done:?}"
    );

    devkit_common::cmd::capture("git", &["worktree", "prune"], Some(bare.to_str().unwrap()))
        .unwrap();
    let listed = devkit_common::cmd::capture(
        "git",
        &["worktree", "list", "--porcelain"],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();
    assert!(
        listed.contains("v1.0.0"),
        "prune dropped the checkout, so the link was never repaired: {listed}"
    );
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}

#[test]
fn one_unrepairable_checkout_does_not_cost_its_healthy_sibling_its_registration() {
    let base = common::unique_tmp("upgrade-mixed");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let old = cache.join("@scope/pkg");
    std::fs::create_dir_all(&old).unwrap();
    let bare = old.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", &repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    let mut heads = Vec::new();
    for tag in ["v1.0.0", "v1.1.0"] {
        devkit_common::cmd::capture(
            "git",
            &[
                "worktree",
                "add",
                "--detach",
                old.join(tag).to_str().unwrap(),
                tag,
            ],
            Some(bare.to_str().unwrap()),
        )
        .unwrap();
        heads.push(
            devkit_common::cmd::capture(
                "git",
                &["rev-parse", "HEAD"],
                Some(old.join(tag).to_str().unwrap()),
            )
            .unwrap()
            .trim()
            .to_string(),
        );
    }
    // v1.1.0 needs a rebuild, which prunes. v1.0.0 only needs the rename
    // followed through, so pruning before repairing it would strand it too.
    std::fs::write(
        old.join("v1.1.0").join(".git"),
        "gitdir: /nonexistent/elsewhere",
    )
    .unwrap();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    let new = cache.join("@scope~pkg");
    assert!(
        done.iter()
            .any(|l| l.contains("repaired") && l.contains("v1.0.0")),
        "the healthy sibling must be repaired, not rebuilt: {done:?}"
    );
    assert!(
        done.iter()
            .any(|l| l.contains("rebuilt") && l.contains("v1.1.0")),
        "the broken checkout must be rebuilt: {done:?}"
    );
    for (tag, head) in ["v1.0.0", "v1.1.0"].iter().zip(&heads) {
        assert_eq!(
            devkit_common::cmd::capture(
                "git",
                &["rev-parse", "HEAD"],
                Some(new.join(tag).to_str().unwrap()),
            )
            .unwrap()
            .trim(),
            head,
            "{tag} moved off its commit"
        );
    }
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}

#[test]
fn a_recreated_checkout_clears_the_registration_its_removal_left_behind() {
    let base = common::unique_tmp("upgrade-stranded");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    std::fs::create_dir_all(&lib).unwrap();
    let bare = lib.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", &repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    devkit_common::cmd::capture(
        "git",
        &[
            "worktree",
            "add",
            "--detach",
            lib.join("v1.0.0").to_str().unwrap(),
            "v1.0.0",
        ],
        Some(bare.to_str().unwrap()),
    )
    .unwrap();
    let head = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "HEAD"],
        Some(lib.join("v1.0.0").to_str().unwrap()),
    )
    .unwrap()
    .trim()
    .to_string();

    // The state a crash between deleting a checkout outside git and clearing
    // its registration leaves: the directory is gone, the administrative entry
    // still claims that exact path, and the journal still wants it back.
    std::fs::remove_dir_all(lib.join("v1.0.0")).unwrap();
    assert!(bare.join("worktrees/v1.0.0").is_dir());
    let journal = cache
        .join("registry.locks")
        .join("@scope~pkg.migration.json");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        format!(r#"{{"worktrees":[{{"dirname":"v1.0.0","commit":"{head}"}}]}}"#),
    )
    .unwrap();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(
        done.iter().any(|l| l.contains("v1.0.0")),
        "the recovery must be reported: {done:?}"
    );
    assert_eq!(
        devkit_common::cmd::capture(
            "git",
            &["rev-parse", "HEAD"],
            Some(lib.join("v1.0.0").to_str().unwrap()),
        )
        .unwrap()
        .trim(),
        head
    );
    assert!(!journal.exists());
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}
