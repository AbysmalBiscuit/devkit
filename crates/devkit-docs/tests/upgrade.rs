use std::path::PathBuf;
use std::process::Command;

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
    let meta = devkit_docs::cache::read_meta(&new).unwrap();
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

/// Clone a bare repo for `lib` and add `checkout` as a detached worktree.
fn seed_library(repo: &str, lib: &std::path::Path, checkouts: &[&str]) -> Vec<String> {
    std::fs::create_dir_all(lib).unwrap();
    let bare = lib.join("repo.git");
    devkit_common::cmd::capture(
        "git",
        &["clone", "--bare", repo, bare.to_str().unwrap()],
        None,
    )
    .unwrap();
    checkouts
        .iter()
        .map(|checkout| {
            devkit_common::cmd::capture(
                "git",
                &[
                    "worktree",
                    "add",
                    "--detach",
                    lib.join(checkout).to_str().unwrap(),
                    checkout,
                ],
                Some(bare.to_str().unwrap()),
            )
            .unwrap();
            devkit_common::cmd::capture(
                "git",
                &["rev-parse", "HEAD"],
                Some(lib.join(checkout).to_str().unwrap()),
            )
            .unwrap()
            .trim()
            .to_string()
        })
        .collect()
}

fn write_journal(cache: &std::path::Path, lib: &str, checkout: &str, commit: &str) -> PathBuf {
    let path = cache
        .join("registry.locks")
        .join(format!("{lib}.migration.json"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(r#"{{"worktrees":[{{"dirname":"{checkout}","commit":"{commit}"}}]}}"#),
    )
    .unwrap();
    path
}

#[test]
fn a_husk_left_by_an_interrupted_removal_is_rebuilt_rather_than_forgotten() {
    let base = common::unique_tmp("upgrade-husk");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    let head = seed_library(&repo, &lib, &["v1.0.0"]).remove(0);
    let journal = write_journal(&cache, "@scope~pkg", "v1.0.0", &head);

    // What a crash inside `worktree remove` leaves: the directory is still
    // there, so it is not missing, but its gitfile is gone, so it is not a
    // worktree either. It falls between "repair it" and "restore it".
    std::fs::remove_file(lib.join("v1.0.0").join(".git")).unwrap();

    let done = devkit_docs::upgrade::run(&cache).unwrap();
    assert!(
        done.iter().any(|l| l.contains("v1.0.0")),
        "the husk must be healed, not skipped: {done:?}"
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
        "the journal is cleared only once its checkout is back and resolving"
    );
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}

/// A journal `read_to_string` rejects is not a journal with nothing in it: an
/// empty one leaves `heal` with nothing pending, and a spent journal is
/// deleted. Deleting needs write permission on the directory rather than on
/// the file, so the delete lands in exactly the case the read failed.
#[test]
fn a_journal_that_cannot_be_read_survives_the_heal_pass() {
    let base = common::unique_tmp("upgrade-unreadable-journal");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    let head = seed_library(&repo, &lib, &["v1.0.0"]).remove(0);
    let journal = write_journal(&cache, "@scope~pkg", "v1.0.0", &head);
    // Bytes no `read_to_string` accepts. Every platform reports this as a read
    // failure that is not `NotFound`, it survives the suite running as root,
    // and the file stays an ordinary file — so a fail-soft read still reaches
    // the delete and the assertion below is about the read, not the delete.
    std::fs::write(&journal, [0xff, 0xfe, 0x00, 0xff]).unwrap();

    let result = devkit_docs::upgrade::run(&cache);

    assert!(
        result.is_err(),
        "a journal that cannot be read must not pass as one with nothing pending"
    );
    assert!(
        journal.is_file(),
        "the record of which checkouts an interrupted removal left behind was deleted"
    );
}

/// `git` failing to spawn is not the repository having lost a commit. Routing
/// a spawn failure to `abandoned` drops the checkout's record from the journal,
/// permanently giving up on a checkout git could still have rebuilt.
#[test]
fn a_git_that_cannot_spawn_is_not_a_commit_the_repository_lost() {
    if let Some(cache) = std::env::var_os("DEVKIT_DOCS_TEST_NO_GIT_CACHE") {
        let cache = PathBuf::from(cache);
        let journal = cache.join("registry.locks/@scope~pkg.migration.json");
        assert!(
            journal.is_file(),
            "the fixture journal did not survive setup"
        );

        let result = devkit_docs::upgrade::run(&cache);

        assert!(
            result.is_err(),
            "a `git` that cannot be spawned must not read as a commit repo.git no longer has"
        );
        assert!(
            journal.is_file(),
            "the checkout's commit record was dropped because git could not be spawned"
        );
        return;
    }

    let base = common::unique_tmp("upgrade-no-git");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let lib = cache.join("@scope~pkg");
    seed_library(&repo, &lib, &[]);
    let head = devkit_common::cmd::capture(
        "git",
        &["rev-parse", "v1.0.0^{commit}"],
        Some(lib.join("repo.git").to_str().unwrap()),
    )
    .unwrap()
    .trim()
    .to_string();
    // Nothing on disk names the checkout but the journal, so dropping its
    // record is unrecoverable rather than merely wasteful.
    let journal = write_journal(&cache, "@scope~pkg", "v1.0.0", &head);
    assert!(!lib.join("v1.0.0").exists());

    // A PATH with no `git` on it is the portable way to make the spawn fail:
    // it needs no permission bits and behaves the same on every platform. It
    // has to be a child process, because PATH is per-process state and the
    // rest of the suite runs git concurrently.
    let no_git = base.join("empty-path");
    std::fs::create_dir_all(&no_git).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .env("DEVKIT_DOCS_TEST_NO_GIT_CACHE", &cache)
        .env("PATH", &no_git)
        .args([
            "--exact",
            "a_git_that_cannot_spawn_is_not_a_commit_the_repository_lost",
            "--nocapture",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        journal.is_file(),
        "the checkout's commit record was dropped because git could not be spawned"
    );
}

/// A worktree administration `read_dir` cannot list is not one with no
/// checkouts in it. An empty map leaves `heal` with no commit for a checkout
/// it must rebuild — and a stale journal entry filling that gap rebuilds it at
/// the wrong commit.
#[test]
fn a_worktree_administration_that_cannot_be_listed_is_not_an_empty_one() {
    let base = common::unique_tmp("upgrade-unreadable-admin");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let old = cache.join("@scope").join("pkg");
    seed_library(&repo, &old, &["v1.0.0"]);
    // A file where the administration directory belongs: `read_dir` fails on
    // every platform, including for root, which `chmod 000` does not.
    let admin_root = old.join("repo.git").join("worktrees");
    std::fs::remove_dir_all(&admin_root).unwrap();
    std::fs::write(&admin_root, "not a directory\n").unwrap();

    let error = devkit_docs::upgrade::run(&cache).unwrap_err();

    let report = format!("{error:#}");
    assert!(
        report.contains(&admin_root.display().to_string()),
        "the error must name the administration it could not read: {report}"
    );
    assert!(
        old.join("repo.git").is_dir(),
        "a run that cannot read the administration recording every commit must move nothing"
    );
    assert!(
        !cache.join("@scope~pkg").exists(),
        "the library was migrated on the strength of an administration that could not be read"
    );
}

#[test]
fn a_rename_another_process_already_applied_is_not_an_error() {
    let base = common::unique_tmp("upgrade-raced");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    let old = cache.join("@scope/pkg");
    let head = seed_library(&repo, &old, &["v1.0.0"]).remove(0);
    let new = cache.join("@scope~pkg");
    let journal = cache
        .join("registry.locks")
        .join("@scope~pkg.migration.json");

    // Hold the target lock, then start a run: it surveys and journals before it
    // reaches the lock, so it plans a rename it will apply against a source
    // another process has meanwhile moved.
    let mut racer = None;
    devkit_docs::locks::with_lib_dir(&cache, "@scope~pkg", || {
        let racing_cache = cache.clone();
        racer = Some(std::thread::spawn(move || {
            devkit_docs::upgrade::run(&racing_cache)
        }));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !journal.try_exists().unwrap() {
            assert!(
                std::time::Instant::now() <= deadline,
                "the run never reached its journal write"
            );
            std::thread::yield_now();
        }
        std::fs::rename(&old, &new).unwrap();
        let _ = std::fs::remove_dir(cache.join("@scope"));
        Ok(())
    })
    .unwrap();

    let done = racer.unwrap().join().unwrap().unwrap();
    assert!(
        !done.iter().any(|l| l.contains("migrated")),
        "the rename was another process's to report: {done:?}"
    );
    assert_eq!(
        devkit_common::cmd::capture(
            "git",
            &["rev-parse", "HEAD"],
            Some(new.join("v1.0.0").to_str().unwrap()),
        )
        .unwrap()
        .trim(),
        head
    );
    assert!(devkit_docs::upgrade::run(&cache).unwrap().is_empty());
}

#[test]
fn two_sources_that_fold_onto_one_target_migrate_nothing() {
    let base = common::unique_tmp("upgrade-fold");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    seed_library(&repo, &cache.join("@Scope/pkg"), &[]);
    if cache.join("@scope").is_dir() {
        // The volume folds case, so the two scopes are one directory and the
        // collision this guards cannot be staged here.
        return;
    }
    seed_library(&repo, &cache.join("@scope/pkg"), &[]);

    let err = devkit_docs::upgrade::run(&cache).unwrap_err().to_string();
    assert!(
        err.contains("differ only by case"),
        "the fold collision must be named: {err}"
    );
    assert!(cache.join("@Scope/pkg/repo.git").is_dir());
    assert!(cache.join("@scope/pkg/repo.git").is_dir());
}

#[test]
fn a_capture_failure_refuses_the_run_before_any_library_is_touched() {
    let base = common::unique_tmp("upgrade-capture");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");
    seed_library(&repo, &cache.join("@scope/aaa"), &["v1.0.0"]);
    let zzz = cache.join("@scope/zzz");
    seed_library(&repo, &zzz, &[]);

    // An administrative entry naming a checkout it records no commit for. It
    // sorts after `aaa`, whose capture has already succeeded by then.
    let ghost = zzz.join("repo.git/worktrees/ghost");
    std::fs::create_dir_all(&ghost).unwrap();
    std::fs::write(ghost.join("gitdir"), "/nonexistent/ghost/.git\n").unwrap();

    let err = devkit_docs::upgrade::run(&cache).unwrap_err().to_string();
    assert!(
        err.contains("ghost"),
        "the error must name the entry: {err}"
    );
    assert!(
        cache.join("@scope/aaa/repo.git").is_dir(),
        "a refused run moves nothing, including the libraries that planned cleanly"
    );
    assert!(
        !cache
            .join("registry.locks/@scope~aaa.migration.json")
            .exists(),
        "no journal outlives a run that never renamed anything"
    );
}

/// Every sidecar is read independently of every other, so a run that stops at
/// the first unreadable one costs the reader a round per library to recover a
/// cache. Each failure is named in one run instead, and a library the run can
/// still read is not held back by a sibling it cannot.
#[test]
fn every_unreadable_sidecar_is_named_in_one_run() {
    let base = common::unique_tmp("upgrade-aggregate");
    let repo = common::fixture_repo(&base.join("src"));
    let cache = base.join("cache");

    for lib in ["alpha", "beta", "gamma"] {
        seed_library(&repo, &cache.join(lib), &["v1.0.0"]);
        std::fs::write(
            cache.join(lib).join("meta.toml"),
            "tag_pattern = \"name-dash-v\"\n",
        )
        .unwrap();
    }
    let readable = cache.join("delta");
    seed_library(&repo, &readable, &["v1.0.0"]);

    let error = format!("{:#}", devkit_docs::upgrade::run(&cache).unwrap_err());

    for lib in ["alpha", "beta", "gamma"] {
        assert!(error.contains(lib), "{lib} is missing from:\n{error}");
        assert!(
            error.contains(&cache.join(lib).join("meta.toml").display().to_string()),
            "{lib}'s sidecar path is missing from:\n{error}"
        );
    }
    assert!(error.contains("name-dash-v"), "{error}");

    // The readable library's origin is backfilled in the same run rather than
    // waiting for the unreadable ones to be cleared.
    assert!(
        devkit_docs::cache::read_meta(&readable)
            .unwrap()
            .origin
            .is_some(),
        "a readable library was skipped over a sibling's failure"
    );

    for lib in ["alpha", "beta", "gamma"] {
        std::fs::remove_file(cache.join(lib).join("meta.toml")).unwrap();
    }
    devkit_docs::upgrade::run(&cache).unwrap();
}
