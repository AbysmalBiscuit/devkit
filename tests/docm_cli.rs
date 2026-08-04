//! End-to-end `docm` tests: each drives the installed binary with `HOME` and
//! `XDG_DATA_HOME` redirected into a temporary tree, so the manifest and the
//! cache under test are the ones production code computes for itself.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const READY: Duration = Duration::from_secs(60);
const CONTENTION: Duration = Duration::from_secs(30);

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repo on branch `main` with tags v1.0.0 and v1.1.0.
fn fixture_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "tag.gpgsign", "false"]);
    std::fs::write(dir.join("src/lib.rs"), "// v1").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "v1"]);
    git(dir, &["tag", "v1.0.0"]);
    std::fs::write(dir.join("src/lib.rs"), "// v2").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "v2"]);
    git(dir, &["tag", "v1.1.0"]);
}

struct Env {
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
    project: PathBuf,
    upstream: String,
}

impl Env {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("docm-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let env = Env {
            home: root.join("home"),
            data: root.join("data"),
            project: root.join("project"),
            upstream: root.join("upstream").to_string_lossy().into_owned(),
            root,
        };
        std::fs::create_dir_all(env.home.join(".config/devkit")).unwrap();
        std::fs::create_dir_all(&env.project).unwrap();
        std::fs::create_dir_all(&env.upstream).unwrap();
        fixture_repo(Path::new(&env.upstream));
        env
    }

    fn global(&self) -> PathBuf {
        self.home.join(".config/devkit/docs.toml")
    }

    fn cache(&self) -> PathBuf {
        self.data.join("devkit/docs")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_docm"));
        command
            .args(args)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.data)
            // Without this, `docs_root` computes its legacy path from the
            // caller's cache home and moves a real store into the temp tree.
            .env("XDG_CACHE_HOME", self.root.join("xdg-cache"))
            .env_remove(devkit_docs::barrier::VAR)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn docm(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn spawn(&self, args: &[&str], barrier: &Path) -> Child {
        self.command(args)
            .env(devkit_docs::barrier::VAR, barrier)
            .spawn()
            .unwrap()
    }

    /// `add` of the fixture repo under `name`, pinned to `git_ref`.
    fn add(&self, name: &str, git_ref: &str) -> Output {
        self.docm(&[
            "add",
            name,
            "--eco",
            "git",
            "--repo",
            &self.upstream,
            "--ref",
            git_ref,
        ])
    }

    fn add_project(&self, name: &str, git_ref: &str) -> Output {
        self.docm(&[
            "add",
            name,
            "--eco",
            "git",
            "--repo",
            &self.upstream,
            "--ref",
            git_ref,
            "--project",
        ])
    }

    /// A hand-maintained `devkit.toml`: comments, an inline comment, and keys
    /// in an order no serializer would produce. A rollback that rewrites the
    /// file instead of restoring it disturbs all three visibly.
    fn write_devkit_toml(&self, registered: &str) -> PathBuf {
        let path = self.project.join("devkit.toml");
        std::fs::write(
            &path,
            format!(
                "# keep me\n[defaults]\napps_dir = 'apps' # inline\n\n\
                 [[docs.libs]]\nref = \"v1.0.0\"\nrepo = {:?}\nname = {registered:?}\n\
                 ecosystem = \"git\"\n",
                self.upstream
            ),
        )
        .unwrap();
        path
    }

    /// A `devkit.toml` whose entry carries a key docm does not model and a
    /// comment *inside* the entry table. Both sit where a re-serialization of
    /// the entry destroys them.
    fn write_devkit_toml_with_extras(&self, registered: &str) -> PathBuf {
        let path = self.project.join("devkit.toml");
        std::fs::write(
            &path,
            format!(
                "# keep me\n[defaults]\napps_dir = 'apps' # inline\n\n\
                 [[docs.libs]]\nname = {registered:?}\necosystem = \"git\"\n\
                 repo = {:?}\n# pinned until the codegen rewrite lands\n\
                 ref = \"v1.0.0\"\nnotes = \"stale\"\nowner = \"platform-team\"\n",
                self.upstream
            ),
        )
        .unwrap();
        path
    }

    /// A cargo workspace whose lockfile pins `package`, so resolution goes
    /// through the importer rather than a `--ref` pin.
    fn write_cargo_project(&self, package: &str, version: &str) {
        std::fs::write(
            self.project.join("Cargo.toml"),
            format!(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
                 [dependencies]\n{package} = \"1\"\n"
            ),
        )
        .unwrap();
        std::fs::write(
            self.project.join("Cargo.lock"),
            format!(
                "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\
                 dependencies = [\"{package}\"]\n\n\
                 [[package]]\nname = \"{package}\"\nversion = \"{version}\"\n\
                 source = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
            ),
        )
        .unwrap();
    }

    /// An npm workspace whose lockfile pins `package` in the `node_modules`
    /// slot of the same name, carrying `row` as that slot's entry.
    fn write_npm_project(&self, package: &str, spec: &str, row: &str) {
        let manifest = format!(r#"{{"name":"app","dependencies":{{"{package}":"{spec}"}}}}"#);
        std::fs::write(self.project.join("package.json"), &manifest).unwrap();
        std::fs::write(
            self.project.join("package-lock.json"),
            format!(
                r#"{{"lockfileVersion":3,"packages":{{"":{manifest},"node_modules/{package}":{row}}}}}"#
            ),
        )
        .unwrap();
    }

    /// A migration record naming a checkout at a commit the bare clone does
    /// not have — what an interrupted rebuild leaves behind once a fetch with
    /// `--prune-tags` has dropped the tag the checkout was pinned to.
    fn write_unsatisfiable_journal(&self, lib: &str) -> PathBuf {
        let path = self
            .cache()
            .join("registry.locks")
            .join(format!("{lib}.migration.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(r#"{{"worktrees":[{{"dirname":"v0.9.0","commit":"{GONE_COMMIT}"}}]}}"#),
        )
        .unwrap();
        path
    }
}

/// A well-formed object id no fixture repository contains.
const GONE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_ran(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed:\n{}\n{}",
        stdout(output),
        stderr(output)
    );
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn wait_for(path: &Path, timeout: Duration, message: &str) {
    let deadline = Instant::now() + timeout;
    while !path.try_exists().unwrap() {
        assert!(Instant::now() <= deadline, "{message}");
        std::thread::yield_now();
    }
}

fn wait(child: Child, label: &str) -> Output {
    let output = child.wait_with_output().unwrap();
    assert_ran(&output, label);
    output
}

#[test]
fn add_materializes_the_checkout_and_reports_ref_commit_and_path() {
    let env = Env::new("add-reports");
    let added = env.add("up", "v1.0.0");
    assert_ran(&added, "docm add up");

    let out = stdout(&added);
    assert!(out.contains("registered up (git)"), "{out}");
    assert!(out.contains("ref       v1.0.0"), "{out}");
    assert!(
        out.contains(&format!("manifest  {}", env.global().display())),
        "{out}"
    );
    let checkout = env.cache().join("up/v1.0.0");
    assert!(
        out.contains(&format!("path      {}", checkout.display())),
        "{out}"
    );
    assert!(
        checkout.join("src/lib.rs").is_file(),
        "add must materialize the checkout it reports"
    );
}

#[test]
fn a_failed_repin_leaves_the_previous_entry_intact() {
    let env = Env::new("repin-rollback");
    assert_ran(&env.add("up", "v1.0.0"), "docm add up v1.0.0");
    let before = read(&env.global());

    let failed = env.add("up", "does-not-exist");
    assert!(
        !failed.status.success(),
        "an unresolvable ref must fail the add"
    );

    assert_eq!(
        read(&env.global()),
        before,
        "a failed re-pin must restore the previous entry byte for byte"
    );
    let path = env.docm(&["path", "up"]);
    assert_ran(&path, "docm path up");
    assert_eq!(
        stdout(&path).trim(),
        env.cache().join("up/v1.0.0").to_string_lossy(),
        "the restored entry must still resolve to the old pin"
    );
}

#[test]
fn a_failed_add_of_a_new_library_leaves_the_manifest_byte_identical() {
    let env = Env::new("add-rollback");
    assert_ran(&env.add("keep", "v1.0.0"), "docm add keep");
    let before = read(&env.global());

    let failed = env.add("new", "does-not-exist");
    assert!(
        !failed.status.success(),
        "an unresolvable ref must fail the add"
    );

    assert_eq!(read(&env.global()), before);
    assert!(
        !before.contains("\"new\""),
        "the failed entry must not survive: {before}"
    );
}

/// The project manifest is hand-maintained and repo-committed, so a rollback
/// there has to put the file back rather than rewrite it.
#[test]
fn a_failed_project_add_leaves_the_devkit_toml_byte_identical() {
    let env = Env::new("project-add-rollback");
    let devkit_toml = env.write_devkit_toml("keep");
    let before = read(&devkit_toml);

    let failed = env.add_project("new", "does-not-exist");

    assert!(
        !failed.status.success(),
        "an unresolvable ref must fail the add:\n{}",
        stdout(&failed)
    );
    assert_eq!(read(&devkit_toml), before);
}

/// The re-pin arm restores an entry the file already carried, so it must leave
/// the rest of the file — comments, formatting, unrelated tables — alone.
#[test]
fn a_failed_project_repin_restores_the_previous_entry() {
    let env = Env::new("project-repin-rollback");
    let devkit_toml = env.write_devkit_toml("keep");
    let before = read(&devkit_toml);

    let failed = env.add_project("keep", "does-not-exist");

    assert!(
        !failed.status.success(),
        "an unresolvable ref must fail the add:\n{}",
        stdout(&failed)
    );
    let after = read(&devkit_toml);
    assert!(
        !after.contains("does-not-exist"),
        "the failed pin must not survive:\n{after}"
    );
    assert!(
        after.contains("# keep me") && after.contains("# inline"),
        "hand-written content outside the entry must survive:\n{after}"
    );
    let path = env.docm(&["path", "keep"]);
    assert_ran(&path, "docm path keep");
    assert_eq!(
        stdout(&path).trim(),
        env.cache().join("keep/v1.0.0").to_string_lossy(),
        "the restored entry must still resolve to the old pin"
    );
    assert_eq!(read(&devkit_toml), before);
}

#[test]
fn rm_project_removes_only_the_named_entry() {
    let env = Env::new("project-rm");
    let devkit_toml = env.write_devkit_toml("keep");

    let removed = env.docm(&["rm", "keep", "--project"]);

    assert_ran(&removed, "docm rm keep --project");
    let after = read(&devkit_toml);
    assert!(!after.contains("keep\""), "{after}");
    assert!(
        after.contains("# keep me") && after.contains("apps_dir"),
        "removal must not disturb the rest of the file:\n{after}"
    );
}

#[test]
fn add_of_a_ref_less_git_entry_pins_the_default_branch_globally() {
    let env = Env::new("add-infers");
    let added = env.docm(&["add", "up", "--eco", "git", "--repo", &env.upstream]);
    assert_ran(&added, "docm add up (no --ref)");

    assert!(
        stdout(&added).contains("ref       main (inferred default branch; moves on `docm sync`)"),
        "{}",
        stdout(&added)
    );
    assert!(
        read(&env.global()).contains("ref = \"main\""),
        "the inferred branch must be written into the manifest as an explicit ref: {}",
        read(&env.global())
    );
    assert!(env.cache().join("up/main/src/lib.rs").is_file());
}

#[test]
fn add_project_refuses_to_infer_a_default_branch() {
    let env = Env::new("add-project-refuses");
    let devkit_toml = env.project.join("devkit.toml");
    std::fs::write(&devkit_toml, "[defaults]\napps_dir = 'apps'\n").unwrap();
    let before = read(&devkit_toml);

    let refused = env.docm(&[
        "add",
        "up",
        "--eco",
        "git",
        "--repo",
        &env.upstream,
        "--project",
    ]);

    assert!(!refused.status.success(), "{}", stdout(&refused));
    assert!(
        stderr(&refused).contains("--project needs an explicit --ref for a git URL entry"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(
        read(&devkit_toml),
        before,
        "the devkit.toml must be untouched"
    );
}

#[test]
fn sync_records_a_missing_ref_in_the_global_manifest() {
    let env = Env::new("sync-backfills");
    std::fs::write(
        env.global(),
        format!(
            "[[libs]]\nname = \"up\"\necosystem = \"git\"\nrepo = {:?}\n",
            env.upstream
        ),
    )
    .unwrap();

    let synced = env.docm(&["sync"]);
    assert_ran(&synced, "docm sync");

    assert!(
        read(&env.global()).contains("ref = \"main\""),
        "sync must record the inferred default branch: {}",
        read(&env.global())
    );
    assert!(env.cache().join("up/main/src/lib.rs").is_file());
}

#[test]
fn sync_refuses_to_write_an_inferred_ref_into_a_devkit_toml() {
    let env = Env::new("sync-project-refuses");
    let devkit_toml = env.project.join("devkit.toml");
    std::fs::write(
        &devkit_toml,
        format!(
            "[[docs.libs]]\nname = \"up\"\necosystem = \"git\"\nrepo = {:?}\n",
            env.upstream
        ),
    )
    .unwrap();
    let before = read(&devkit_toml);

    let refused = env.docm(&["sync"]);

    assert!(!refused.status.success(), "{}", stdout(&refused));
    assert!(
        stderr(&refused)
            .contains("docm will not write an inferred default branch into a devkit.toml"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(read(&devkit_toml), before);
    // The pin must not land anywhere: writing it to the global manifest instead
    // gives a project entry a machine-specific pin the project cannot see.
    let global = std::fs::read_to_string(env.global()).unwrap_or_default();
    assert!(!global.contains("ref ="), "{global}");
}

#[test]
fn info_and_list_report_the_ref_commit_and_clone_origin() {
    let env = Env::new("info-list");
    assert_ran(&env.add("up", "v1.0.0"), "docm add up");

    let info = env.docm(&["info", "up"]);
    assert_ran(&info, "docm info up");
    let out = stdout(&info);
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "v1.0.0^{commit}"])
            .current_dir(&env.upstream)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let head = head.trim();
    for line in [
        format!("repo     {}", env.upstream),
        "ref      v1.0.0".to_string(),
        format!("commit   {head}"),
        "status   ok".to_string(),
    ] {
        assert!(out.contains(&line), "missing `{line}` in:\n{out}");
    }

    let listed = env.docm(&["list", "--json"]);
    assert_ran(&listed, "docm list --json");
    let items: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
    let lib = &items[0];
    assert_eq!(lib["origin"], serde_json::json!(env.upstream));
    assert_eq!(lib["checkouts"][0]["worktree"], serde_json::json!("v1.0.0"));
    assert_eq!(lib["checkouts"][0]["ref"], serde_json::json!("v1.0.0"));
    assert_eq!(lib["checkouts"][0]["commit"], serde_json::json!(head));
}

/// `status ok` is a claim agents act on, so it must be paid for: a checkout
/// whose source no longer matches its commit fails the command instead.
#[test]
fn info_fails_instead_of_reporting_ok_for_a_dirty_checkout() {
    let env = Env::new("info-dirty");
    assert_ran(&env.add("up", "v1.0.0"), "docm add up");
    std::fs::write(env.cache().join("up/v1.0.0/src/lib.rs"), "// local edit").unwrap();

    let info = env.docm(&["info", "up"]);

    assert!(
        !info.status.success(),
        "info reported on a modified checkout:\n{}",
        stdout(&info)
    );
    assert!(
        stderr(&info).contains("has local modifications"),
        "{}",
        stderr(&info)
    );
    assert!(!stdout(&info).contains("status   ok"), "{}", stdout(&info));
}

/// `rm` must take the same library lock `add` holds for its whole transaction.
/// The adder pauses between reading the manifest and writing its entry, which
/// is the only window where a locked and an unlocked `rm` differ: an unlocked
/// one reads the manifest without `up`, deletes nothing, and lets the add
/// commit afterwards, leaving `up` registered by a command that removed it.
#[test]
fn rm_blocks_until_a_concurrent_add_of_the_same_library_completes() {
    let env = Env::new("rm-add-race");
    assert_ran(&env.add("keep", "v1.0.0"), "docm add keep");
    let barrier = env.root.join("barrier");
    // The manifest write and the resolution have rendezvous of their own,
    // released upfront: this test is about the one between add's manifest read
    // and its manifest write.
    std::fs::write(barrier.with_extension("commit"), "").unwrap();
    std::fs::write(barrier.with_extension("manifest-go"), "").unwrap();

    let adder = env.spawn(
        &[
            "add",
            "up",
            "--eco",
            "git",
            "--repo",
            &env.upstream,
            "--ref",
            "v1.0.0",
        ],
        &barrier,
    );
    wait_for(
        &barrier.with_extension("ready"),
        READY,
        "the adder never reached the barrier",
    );

    let remover = env.spawn(&["rm", "up"], &barrier);
    // Proof that the remover is *blocked on `up`'s library lock*, not merely
    // running: `.contended.up` is written from inside the acquisition of that
    // lock after a non-blocking attempt fails, which can only happen while the
    // adder holds it. Waiting on the remover merely starting would pass
    // against a remover that takes no lock at all, and an unscoped signal
    // would also be satisfied by contention on the manifest lock.
    wait_for(
        &barrier.with_extension("contended.up"),
        CONTENTION,
        "the remover never contended for the library lock — it is not taking one",
    );

    std::fs::write(barrier.with_extension("go"), "").unwrap();
    wait(adder, "docm add up");
    wait(remover, "docm rm up");

    let manifest = read(&env.global());
    assert!(
        !manifest.contains("\"up\""),
        "rm ran after the add committed, so `up` must be gone — finding it means rm read \
         the manifest before the add committed and wrote back a stale copy:\n{manifest}"
    );
    assert!(
        manifest.contains("\"keep\""),
        "the unrelated entry must survive a lost update:\n{manifest}"
    );
}

/// `rm` and `prune` are the recovery commands for a cache the migration
/// cannot finish, so a migration that fails hard takes the recovery with it.
/// A record naming a commit the repository no longer has is dropped instead:
/// the checkout it describes is rebuildable by re-resolving, while a cache
/// with no working CLI is not.
#[test]
fn an_unsatisfiable_migration_record_does_not_disable_the_cli() {
    let env = Env::new("journal-unsatisfiable");
    assert_ran(&env.add("up", "v1.0.0"), "docm add up");
    let journal = env.write_unsatisfiable_journal("up");

    let listed = env.docm(&["list"]);

    assert_ran(&listed, "docm list");
    assert!(
        stderr(&listed).contains(&journal.display().to_string()),
        "the dropped record must name the file it came from: {}",
        stderr(&listed)
    );
    assert!(
        stderr(&listed).contains(GONE_COMMIT),
        "the dropped record must name the commit that is gone: {}",
        stderr(&listed)
    );
    assert!(
        !journal.exists(),
        "a record nothing can satisfy must not survive the run that dropped it"
    );
    assert_ran(&env.docm(&["prune"]), "docm prune");
    assert_ran(&env.docm(&["rm", "up"]), "docm rm up");
}

/// `completions` writes a script to stdout and never reads the cache, and a
/// shell rc sources it on every new shell — so it must not be gated behind a
/// cache migration that can warn or fail.
#[test]
fn completions_neither_migrates_the_cache_nor_writes_to_stderr() {
    let env = Env::new("completions-no-migration");
    assert_ran(&env.add("up", "v1.0.0"), "docm add up");
    let journal = env.write_unsatisfiable_journal("up");

    let completions = env.docm(&["completions", "bash"]);

    assert_ran(&completions, "docm completions bash");
    assert!(
        stderr(&completions).is_empty(),
        "a shell sources this at startup: {}",
        stderr(&completions)
    );
    assert!(
        stdout(&completions).contains("docm"),
        "{}",
        stdout(&completions)
    );
    assert!(
        journal.exists(),
        "completions must not touch the cache at all"
    );
}

/// A `devkit.toml` is repo-committed and hand-maintained, so a re-pin that
/// succeeds has to leave everything it did not change: keys docm does not
/// model, and the comments and ordering around them.
#[test]
fn a_successful_project_repin_keeps_unmodeled_keys_and_inner_comments() {
    let env = Env::new("project-repin-keeps");
    let devkit_toml = env.write_devkit_toml_with_extras("keep");

    let repinned = env.add_project("keep", "v1.1.0");

    assert_ran(&repinned, "docm add keep --project --ref v1.1.0");
    let after = read(&devkit_toml);
    assert!(
        after.contains("ref = \"v1.1.0\""),
        "the re-pin must land:\n{after}"
    );
    assert!(
        after.contains("owner = \"platform-team\""),
        "a key docm does not model must survive its own re-pin:\n{after}"
    );
    assert!(
        after.contains("# pinned until the codegen rewrite lands"),
        "a comment inside the entry must survive:\n{after}"
    );
    assert!(
        after.contains("# keep me") && after.contains("# inline"),
        "content outside the entry must survive:\n{after}"
    );
    assert!(
        !after.contains("stale"),
        "a key docm models is docm's to drop when the registration omits it:\n{after}"
    );
}

/// stdout carries the answer and everything that explains it; stderr carries
/// only what needs attention. Where the version came from is provenance for a
/// correct answer, so a fully successful lockfile resolution says nothing on
/// stderr — the channel readers are told to treat as a stop signal.
#[test]
fn a_lockfile_resolution_reports_its_provenance_on_stdout_only() {
    let env = Env::new("lockfile-provenance");
    env.write_cargo_project("up", "1.0.0");

    let added = env.docm(&["add", "up", "--eco", "rust", "--repo", &env.upstream]);

    assert_ran(&added, "docm add up");
    assert!(
        stderr(&added).is_empty(),
        "a successful add wrote to stderr: {}",
        stderr(&added)
    );
    assert!(
        stdout(&added).contains("source    the root workspace installs it (Cargo.lock)"),
        "provenance belongs with the result:\n{}",
        stdout(&added)
    );

    let path = env.docm(&["path", "up"]);
    assert_ran(&path, "docm path up");
    assert!(
        stderr(&path).is_empty(),
        "a successful path lookup wrote to stderr: {}",
        stderr(&path)
    );
    assert_eq!(
        stdout(&path).trim(),
        env.cache().join("up/v1.0.0").to_string_lossy(),
        "path prints exactly the checkout"
    );

    let info = env.docm(&["info", "up"]);
    assert_ran(&info, "docm info up");
    assert!(stderr(&info).is_empty(), "{}", stderr(&info));
    assert!(
        stdout(&info).contains("source   the root workspace installs it (Cargo.lock)"),
        "{}",
        stdout(&info)
    );
}

/// An npm alias fills `node_modules/<key>` with a different package, so the
/// version recorded there is the alias target's release number. Serving that
/// number as the queried library's version answers with an unrelated
/// repository's tree; the CLI refuses instead, naming the package actually
/// installed and the pin that overrides it.
#[test]
fn an_npm_alias_is_refused_rather_than_resolved_to_the_wrong_repo() {
    let env = Env::new("npm-alias");
    env.write_npm_project(
        "up",
        "npm:up-fork@^1.0.0",
        r#"{"name":"up-fork","version":"1.0.0","resolved":"https://registry.npmjs.org/up-fork/-/up-fork-1.0.0.tgz","integrity":"sha512-x"}"#,
    );

    let aliased = env.docm(&["add", "up", "--eco", "js", "--repo", &env.upstream]);

    assert!(
        !aliased.status.success(),
        "an alias resolved instead of failing:\n{}",
        stdout(&aliased)
    );
    let error = stderr(&aliased);
    assert!(error.contains("up-fork"), "{error}");
    assert!(error.contains("node_modules/up"), "{error}");
    assert!(error.contains("--ref"), "{error}");
    assert!(
        !stdout(&aliased).contains("1.0.0"),
        "a refused alias still reported a version:\n{}",
        stdout(&aliased)
    );

    env.write_npm_project(
        "up",
        "^1.0.0",
        r#"{"version":"1.0.0","resolved":"https://registry.npmjs.org/up/-/up-1.0.0.tgz","integrity":"sha512-x"}"#,
    );

    let plain = env.docm(&["add", "up", "--eco", "js", "--repo", &env.upstream]);

    assert_ran(&plain, "docm add up");
    assert!(
        stdout(&plain).contains(
            "source    the root workspace installs it (node_modules/up; package-lock.json)"
        ),
        "{}",
        stdout(&plain)
    );
}

/// A cache carrying a library whose name docm reserves fails every command,
/// including the `docm rm` that would unregister it. The error therefore has
/// to describe the recovery that works — a hand edit of the manifest and a
/// hand deletion of the cache directory — and this test performs exactly the
/// recovery the error names.
#[test]
fn a_reserved_library_name_names_a_recovery_that_works() {
    let env = Env::new("reserved-name");
    assert_ran(&env.add("other", "v1.0.0"), "docm add other");

    let manifest = env.global();
    let registered = read(&manifest);
    std::fs::write(
        &manifest,
        format!(
            "{registered}\n[[libs]]\nname = \"manifest\"\necosystem = \"git\"\n\
             repo = {:?}\nref = \"v1.0.0\"\n",
            env.upstream
        ),
    )
    .unwrap();
    copy_tree(&env.cache().join("other"), &env.cache().join("manifest"));

    let bricked = env.docm(&["list"]);
    assert!(!bricked.status.success(), "{}", stdout(&bricked));
    let error = stderr(&bricked);
    assert!(error.contains("`manifest`"), "{error}");
    assert!(error.contains("docm rm"), "{error}");
    assert!(error.contains("docs.toml"), "{error}");
    assert!(error.contains("<cache>/manifest"), "{error}");

    assert!(
        !env.docm(&["rm", "manifest"]).status.success(),
        "docm rm unregistered a reserved name"
    );

    std::fs::write(&manifest, &registered).unwrap();
    std::fs::remove_dir_all(env.cache().join("manifest")).unwrap();

    let recovered = env.docm(&["list"]);
    assert_ran(&recovered, "docm list after the recovery the error names");
    assert!(
        stdout(&recovered).contains("other"),
        "{}",
        stdout(&recovered)
    );
}
