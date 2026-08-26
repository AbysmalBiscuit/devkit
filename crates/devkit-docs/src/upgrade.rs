//! Bring a cache written by an older docm up to the current layout: a library
//! stored as the nested `<scope>/<pkg>` path becomes the `<scope>~<pkg>`
//! encoding, worktree links follow the move, and every library records the
//! origin its bare clone came from.
//!
//! `run` is a preflight-then-act pass. Nothing moves until every rename is
//! known to be safe, and every state a crash can leave is one the next run
//! finishes, so re-running is the whole recovery story.

use crate::{cache, locks, names};
use anyhow::{Context, Result, bail};
use devkit_common::git::Git;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const JOURNAL_SUFFIX: &str = ".migration.json";

/// The checkouts a library must end up holding, and the commits they sit at.
/// Rebuilding a checkout drops both its directory and the administrative entry
/// that names it, so the record has to outlive the process doing the rebuild.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Journal {
    #[serde(default)]
    worktrees: Vec<JournalEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalEntry {
    dirname: String,
    commit: String,
}

/// Migrate `cache_root` in place, returning one line per action taken. An
/// already-current cache with nothing to backfill yields an empty vector.
pub fn run(cache_root: &Path) -> Result<Vec<String>> {
    if !cache_root.is_dir() {
        return Ok(Vec::new());
    }
    let survey = survey(cache_root)?;
    let renames = plan_renames(cache_root, &survey)?;

    // Capturing before journalling keeps a refused run from leaving a record
    // at a target name no later pass visits, which a re-added library would
    // then read as its own.
    let mut captured = Vec::new();
    for rename in &renames {
        let source = cache_root.join(&rename.scope).join(&rename.member);
        captured.push(capture_commits(&source.join("repo.git"))?);
    }
    // Every commit is on disk before the first directory moves: a rebuild
    // destroys both the checkout and the administrative entry naming it, and
    // an interrupted run leaves nothing else to recover them from.
    for (rename, commits) in renames.iter().zip(&captured) {
        if !commits.is_empty() {
            write_journal(cache_root, &rename.target, commits)?;
        }
    }

    let mut lines = Vec::new();
    for rename in &renames {
        lines.extend(locks::with_lib_dir(cache_root, &rename.target, || {
            apply_rename(cache_root, rename)
        })?);
    }
    // Each library is surveyed and healed independently of every other, so
    // stopping at the first failure would cost a reader one run per broken
    // library to clear a cache. Every failure is collected and reported
    // together, and the libraries that are fine are still attended to.
    let mut problems = Vec::new();
    for dirname in &survey.libraries {
        match attend(cache_root, dirname) {
            Ok(attended) => lines.extend(attended),
            Err(error) => problems.push(format!("{dirname}: {error:#}")),
        }
    }
    if !problems.is_empty() {
        bail!("the docs cache is not usable:\n  {}", problems.join("\n  "));
    }
    Ok(lines)
}

fn attend(cache_root: &Path, dirname: &str) -> Result<Vec<String>> {
    if !needs_attention(cache_root, dirname)? {
        return Ok(Vec::new());
    }
    locks::with_lib_dir(cache_root, dirname, || {
        heal_and_backfill(cache_root, dirname)
    })
}

#[derive(Debug, Default)]
struct Survey {
    /// Cache-root directories that already hold a `repo.git`.
    libraries: Vec<String>,
    /// Cache-root directories whose children hold a `repo.git`.
    scopes: Vec<Scope>,
    /// Every cache-root directory name, for detecting rename targets that a
    /// case-folding filesystem would land on an existing entry.
    entries: Vec<String>,
}

#[derive(Debug)]
struct Scope {
    dir: String,
    members: Vec<String>,
}

/// A checkout `heal` has to put back, and whether a directory is in the way.
#[derive(Debug)]
struct Pending {
    checkout: String,
    commit: String,
    verb: &'static str,
    occupied: bool,
}

#[derive(Debug)]
struct Rename {
    scope: String,
    member: String,
    target: String,
}

fn survey(cache_root: &Path) -> Result<Survey> {
    let mut survey = Survey::default();
    let entries = std::fs::read_dir(cache_root)
        .with_context(|| format!("reading {}", cache_root.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", cache_root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dirname = entry.file_name().to_string_lossy().into_owned();
        survey.entries.push(dirname.clone());
        if locks::is_control(&dirname) {
            continue;
        }
        if path.join("repo.git").is_dir() {
            survey.libraries.push(dirname);
            continue;
        }
        let members = scope_members(&path)?;
        if !members.is_empty() {
            survey.scopes.push(Scope {
                dir: dirname,
                members,
            });
        }
    }
    survey.libraries.sort();
    survey.scopes.sort_by(|a, b| a.dir.cmp(&b.dir));
    Ok(survey)
}

fn scope_members(dir: &Path) -> Result<Vec<String>> {
    let mut members = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        if entry.path().join("repo.git").is_dir() {
            members.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    members.sort();
    Ok(members)
}

/// Every rename the cache needs, or an error listing every reason it cannot be
/// migrated. Nothing here touches the filesystem: a refused run leaves the
/// cache exactly as it was rather than half-renamed.
fn plan_renames(cache_root: &Path, survey: &Survey) -> Result<Vec<Rename>> {
    let mut renames = Vec::new();
    let mut problems = Vec::new();
    let mut claimed: BTreeMap<String, String> = survey
        .entries
        .iter()
        .map(|entry| (names::fold_key(entry), entry.clone()))
        .collect();
    for scope in &survey.scopes {
        for member in &scope.members {
            let source = format!("{}/{member}", scope.dir);
            let target = match names::lib_dir(&source) {
                Ok(target) => target,
                Err(error) => {
                    problems.push(format!("{source}: {error}"));
                    continue;
                }
            };
            if cache_root.join(&target).exists() {
                problems.push(format!("{source}: `{target}` already exists"));
                continue;
            }
            if let Some(existing) = claimed.insert(names::fold_key(&target), target.clone()) {
                problems.push(format!(
                    "{source}: `{target}` and `{existing}` differ only by case, which a \
                     case-folding filesystem cannot keep apart"
                ));
                continue;
            }
            renames.push(Rename {
                scope: scope.dir.clone(),
                member: member.clone(),
                target,
            });
        }
    }
    if !problems.is_empty() {
        bail!(
            "this docs cache cannot be migrated to the current layout:\n  {}\nmove or delete \
             the listed directories, then run docm again",
            problems.join("\n  ")
        );
    }
    Ok(renames)
}

fn apply_rename(cache_root: &Path, rename: &Rename) -> Result<Vec<String>> {
    let scope_dir = cache_root.join(&rename.scope);
    let source = scope_dir.join(&rename.member);
    let target = cache_root.join(&rename.target);
    let mut lines = Vec::new();
    // Another process planning the same migration may have held this lock
    // first and already moved the library. The plan is then satisfied rather
    // than violated, and healing it is still this run's job.
    if source.exists() || !target.is_dir() {
        std::fs::rename(&source, &target)
            .with_context(|| format!("moving {} to {}", source.display(), target.display()))?;
        lines.push(format!(
            "migrated {}/{} to {}",
            rename.scope, rename.member, rename.target
        ));
    }
    lines.extend(heal_and_backfill(cache_root, &rename.target)?);
    // A scope whose other members were not libraries keeps its directory.
    let _ = std::fs::remove_dir(&scope_dir);
    Ok(lines)
}

fn heal_and_backfill(cache_root: &Path, dirname: &str) -> Result<Vec<String>> {
    let mut lines = heal(cache_root, dirname)?;
    lines.extend(backfill_origin(cache_root, dirname)?);
    Ok(lines)
}

/// Whether a library is worth taking its lock for. A read-only probe, so the
/// steady state costs no lock, no subprocess, and no write.
fn needs_attention(cache_root: &Path, dirname: &str) -> Result<bool> {
    if journal_path(cache_root, dirname).exists() {
        return Ok(true);
    }
    let lib_dir = cache_root.join(dirname);
    if cache::read_meta(&lib_dir)?.origin.is_none() {
        return Ok(true);
    }
    let bare = lib_dir.join("repo.git");
    Ok(checkouts(cache_root, dirname)
        .iter()
        .any(|(_, path)| is_worktree(path) && !links_ok(&bare, path)))
}

/// A checkout git can still use: present, a worktree, and linked to its
/// administrative entry in both directions.
fn resolves(bare: &Path, checkout: &Path) -> bool {
    is_worktree(checkout) && links_ok(bare, checkout)
}

/// A journal is spent only once every checkout it names is back and usable.
/// Clearing it on mere presence would discard the record of a directory an
/// interrupted removal left behind as a husk, which no other pass would fix.
fn journal_satisfied(lib_dir: &Path, bare: &Path, journal: &[JournalEntry]) -> bool {
    journal
        .iter()
        .all(|entry| resolves(bare, &lib_dir.join(&entry.dirname)))
}

/// Make every checkout of one library resolve again: repair the ones whose
/// links a rename invalidated, rebuild the ones repair cannot fix, and
/// recreate the ones an interrupted rebuild left with no trace but a journal.
///
/// A checkout whose commit the repository no longer has is given up on rather
/// than bailed on. The migration runs before every command, so a hard failure
/// here takes `rm` and `prune` — the commands that clear a broken cache — down
/// with it, while the abandoned checkout is rebuildable by re-resolving.
fn heal(cache_root: &Path, dirname: &str) -> Result<Vec<String>> {
    let lib_dir = cache_root.join(dirname);
    let bare = lib_dir.join("repo.git");
    let bare_str = bare.to_string_lossy().into_owned();
    let journal_file = journal_path(cache_root, dirname);
    let admin = capture_commits(&bare)?;

    let mut lines = Vec::new();
    let mut journal = Vec::new();
    let mut unreachable = Vec::new();
    for entry in read_journal(&journal_file)? {
        if has_commit(&bare_str, &entry.commit)? {
            journal.push(entry);
        } else {
            unreachable.push(entry);
        }
    }
    for entry in &unreachable {
        lines.push(abandoned(
            dirname,
            &entry.dirname,
            &entry.commit,
            &journal_file,
        ));
    }

    let mut pending: Vec<Pending> = Vec::new();
    for (checkout, path) in checkouts(cache_root, dirname) {
        if resolves(&bare, &path) || !is_worktree(&path) {
            continue;
        }
        let commit = expected_commit(&admin, &journal, &checkout).with_context(|| {
            format!(
                "{} is not a usable worktree and nothing on disk records the commit it held; \
                 delete it and run docm again to re-resolve the library",
                path.display()
            )
        })?;
        if !has_commit(&bare_str, &commit)? {
            lines.push(abandoned(dirname, &checkout, &commit, &journal_file));
            continue;
        }
        let path_str = path.to_string_lossy().into_owned();
        // Repair has to precede any prune: while a worktree's administrative
        // back-pointer is stale, prune reads it as abandoned and deletes the
        // entry that records its commit.
        let _ = Git::at(&bare)
            .args(["worktree", "repair", path_str.as_str()])
            .output();
        match head_at(&path) {
            Ok(head) if head == commit => lines.push(format!("repaired {dirname}/{checkout}")),
            _ => pending.push(Pending {
                checkout,
                commit,
                verb: "rebuilt",
                occupied: true,
            }),
        }
    }
    for entry in &journal {
        let path = lib_dir.join(&entry.dirname);
        if resolves(&bare, &path) || pending.iter().any(|p| p.checkout == entry.dirname) {
            continue;
        }
        // A husk in the way of its own recreation has to be cleared first; a
        // checkout that is simply absent does not.
        let occupied = path.exists();
        pending.push(Pending {
            checkout: entry.dirname.clone(),
            commit: entry.commit.clone(),
            verb: if occupied { "rebuilt" } else { "restored" },
            occupied,
        });
    }

    if pending.is_empty() {
        if journal_satisfied(&lib_dir, &bare, &journal) {
            clear_journal(cache_root, dirname)?;
        }
        return Ok(lines);
    }

    let mut record = admin;
    for step in &pending {
        record.insert(step.checkout.clone(), step.commit.clone());
    }
    write_journal(cache_root, dirname, &record)?;

    for step in pending.iter().filter(|step| step.occupied) {
        let path = lib_dir.join(&step.checkout);
        let path_str = path.to_string_lossy().into_owned();
        if Git::at(&bare)
            .args(["worktree", "remove", "--force", path_str.as_str()])
            .output()
            .is_err()
        {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("deleting {}", path.display()));
                }
            }
        }
    }
    // A directory removed outside git leaves its administrative entry behind,
    // and `worktree add` refuses a name that is still registered.
    Git::at(&bare)
        .args(["worktree", "prune"])
        .output()
        .with_context(|| {
            format!(
                "pruning worktrees of {dirname}, listed by {}",
                journal_file.display()
            )
        })?;
    for step in &pending {
        let Pending {
            checkout, commit, ..
        } = step;
        let path_str = lib_dir.join(checkout).to_string_lossy().into_owned();
        Git::at(&bare)
            .args(["worktree", "add", "--detach", path_str.as_str(), commit])
            .output()
            .with_context(|| {
                format!(
                    "recreating {dirname}/{checkout} at {commit}, recorded in {}; delete that file \
                 to abandon the record and re-resolve the library instead",
                    journal_file.display()
                )
            })?;
        lines.push(format!("{} {dirname}/{checkout} at {commit}", step.verb));
    }
    if journal_satisfied(&lib_dir, &bare, &journal) {
        clear_journal(cache_root, dirname)?;
    }
    Ok(lines)
}

/// What one abandoned checkout is reported as: which commit is gone, which
/// file recorded it, and what puts the checkout back.
fn abandoned(dirname: &str, checkout: &str, commit: &str, journal_file: &Path) -> String {
    format!(
        "{dirname}/{checkout} was recorded at {commit}, which repo.git no longer has; dropping \
         that record from {} — re-resolve the library to rebuild the checkout",
        journal_file.display()
    )
}

/// Whether the bare repository holds `commit` locally. A fetch that prunes a
/// deleted tag can leave a record pointing at objects git has since collected,
/// and `worktree add` cannot recreate what is gone. `GIT_NO_LAZY_FETCH` keeps
/// the probe from asking the remote for an object it is about to give up on.
///
/// A `git` that will not spawn is an error rather than a `false`: the caller
/// gives up on the checkouts this answers `false` for, and losing the record
/// of a commit the repository still has is not recoverable.
fn has_commit(bare: &str, commit: &str) -> Result<bool> {
    let spec = format!("{commit}^{{commit}}");
    Git::at(Path::new(bare))
        .args(["cat-file", "-e", spec.as_str()])
        .env("GIT_NO_LAZY_FETCH", "1")
        .success()
        .with_context(|| format!("spawning git to look for {commit} in {bare}"))
}

fn backfill_origin(cache_root: &Path, dirname: &str) -> Result<Vec<String>> {
    let lib_dir = cache_root.join(dirname);
    let mut meta = cache::read_meta(&lib_dir)?;
    if meta.origin.is_some() {
        return Ok(Vec::new());
    }
    let bare = lib_dir.join("repo.git");
    let origin = Git::at(&bare)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty());
    let Some(origin) = origin else {
        return Ok(vec![format!(
            "{dirname} records no origin and its clone has no remote.origin.url; \
             `docm rm` and re-add it to clone it again"
        )]);
    };
    meta.origin = Some(origin.clone());
    cache::write_meta(&lib_dir, &meta)?;
    Ok(vec![format!("recorded origin {origin} for {dirname}")])
}

fn checkouts(cache_root: &Path, dirname: &str) -> Vec<(String, PathBuf)> {
    cache::LibCache::from_dir(cache_root, dirname).version_worktrees()
}

/// A checkout git owns, as opposed to a directory that merely sits in the
/// library. A `.git` *directory* is an unrelated repository someone parked
/// here, and is none of this pass's business.
fn is_worktree(path: &Path) -> bool {
    path.join(".git").is_file()
}

/// Whether a checkout and its administrative entry still point at each other.
fn links_ok(bare: &Path, checkout: &Path) -> bool {
    let git_file = checkout.join(".git");
    let Ok(contents) = std::fs::read_to_string(&git_file) else {
        return false;
    };
    let Some(recorded) = contents.trim().strip_prefix("gitdir:") else {
        return false;
    };
    let (Ok(admin), Ok(bare)) = (
        std::fs::canonicalize(recorded.trim()),
        std::fs::canonicalize(bare),
    ) else {
        return false;
    };
    if !admin.starts_with(bare.join("worktrees")) {
        return false;
    }
    let Ok(back) = std::fs::read_to_string(admin.join("gitdir")) else {
        return false;
    };
    match (
        std::fs::canonicalize(back.trim()),
        std::fs::canonicalize(&git_file),
    ) {
        (Ok(back), Ok(git_file)) => back == git_file,
        _ => false,
    }
}

fn head_at(checkout: &Path) -> Result<String> {
    Ok(Git::at(checkout)
        .args(["rev-parse", "HEAD"])
        .output()?
        .trim()
        .to_string())
}

fn expected_commit(
    admin: &BTreeMap<String, String>,
    journal: &[JournalEntry],
    checkout: &str,
) -> Option<String> {
    admin.get(checkout).cloned().or_else(|| {
        journal
            .iter()
            .find(|entry| entry.dirname == checkout)
            .map(|entry| entry.commit.clone())
    })
}

/// Checkout dirname → commit, read from the bare repository's worktree
/// administration. `git -C <checkout> rev-parse HEAD` needs the checkout's own
/// `.git` link to be intact, and the checkouts whose commit most needs
/// recording are exactly the ones whose link is broken.
fn capture_commits(bare: &Path) -> Result<BTreeMap<String, String>> {
    let lib_dir = bare
        .parent()
        .with_context(|| format!("{} has no library directory", bare.display()))?;
    let admin_root = bare.join("worktrees");
    let mut commits = BTreeMap::new();
    // A library with no checkouts has no administration directory. Any other
    // failure to list it is not evidence that it holds nothing: the commits it
    // records are the only ones a rebuild can put a checkout back at.
    let entries = match std::fs::read_dir(&admin_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(commits),
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", admin_root.display()));
        }
    };
    let mut listed: Option<BTreeMap<String, String>> = None;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", admin_root.display()))?;
        let admin = entry.path();
        let Some(checkout) = admin_checkout_name(&admin, lib_dir) else {
            continue;
        };
        let commit = match admin_head(&admin) {
            Some(commit) => commit,
            None => {
                let listed = match listed {
                    Some(ref listed) => listed,
                    None => listed.insert(list_worktrees(bare)?),
                };
                listed.get(&checkout).cloned().with_context(|| {
                    format!(
                        "{} registers a worktree named `{checkout}` but records no commit for \
                         it; delete that directory and run docm again",
                        admin.display()
                    )
                })?
            }
        };
        commits.insert(checkout, commit);
    }
    Ok(commits)
}

/// The checkout directory an administrative entry belongs to. Git names the
/// entry after the checkout's basename but disambiguates a collision, so the
/// recorded back-pointer wins when it names a directory that is really there —
/// and the entry's own name carries the answer when that back-pointer is
/// itself the broken thing.
fn admin_checkout_name(admin: &Path, lib_dir: &Path) -> Option<String> {
    let recorded = std::fs::read_to_string(admin.join("gitdir"))
        .ok()
        .and_then(|recorded| {
            let parent = Path::new(recorded.trim()).parent()?.to_path_buf();
            Some(parent.file_name()?.to_string_lossy().into_owned())
        });
    if let Some(recorded) = recorded
        && lib_dir.join(&recorded).is_dir()
    {
        return Some(recorded);
    }
    admin
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn admin_head(admin: &Path) -> Option<String> {
    real_commit(&std::fs::read_to_string(admin.join("HEAD")).ok()?)
}

/// A detached object id, rejecting the null id git reports for an entry it
/// cannot resolve — recording that as a commit would journal a checkout no
/// `worktree add` can ever recreate.
fn real_commit(value: &str) -> Option<String> {
    let value = value.trim();
    let hex = value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit());
    (hex && value.bytes().any(|byte| byte != b'0')).then(|| value.to_string())
}

fn list_worktrees(bare: &Path) -> Result<BTreeMap<String, String>> {
    let output = Git::at(bare)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .with_context(|| format!("listing worktrees of {}", bare.display()))?;
    let mut listed = BTreeMap::new();
    let mut checkout: Option<String> = None;
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            checkout = Path::new(path.trim())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
        } else if let Some(commit) = line.strip_prefix("HEAD ")
            && let Some(checkout) = checkout.take()
            && let Some(commit) = real_commit(commit)
        {
            listed.insert(checkout, commit);
        }
    }
    Ok(listed)
}

fn journal_path(cache_root: &Path, dirname: &str) -> PathBuf {
    locks::control_dir(cache_root).join(format!("{dirname}{JOURNAL_SUFFIX}"))
}

/// The checkouts a previous run recorded, or none when no run left a journal.
/// A journal that exists but cannot be read is an error: an empty one leaves
/// `heal` with nothing pending, and a journal with nothing pending is deleted.
fn read_journal(path: &Path) -> Result<Vec<JournalEntry>> {
    let raw = devkit_common::store::read_strict(path).with_context(|| {
        format!(
            "{} exists but cannot be read as a migration record; inspect it, then delete it to \
             continue",
            path.display()
        )
    })?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let journal: Journal = serde_json::from_str(&raw).with_context(|| {
        format!(
            "{} is not readable as a migration record; inspect it, then delete it to continue",
            path.display()
        )
    })?;
    Ok(journal.worktrees)
}

fn write_journal(
    cache_root: &Path,
    dirname: &str,
    commits: &BTreeMap<String, String>,
) -> Result<()> {
    let path = journal_path(cache_root, dirname);
    let parent = locks::control_dir(cache_root);
    std::fs::create_dir_all(&parent).with_context(|| format!("creating {}", parent.display()))?;
    let journal = Journal {
        worktrees: commits
            .iter()
            .map(|(dirname, commit)| JournalEntry {
                dirname: dirname.clone(),
                commit: commit.clone(),
            })
            .collect(),
    };
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(&journal)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn clear_journal(cache_root: &Path, dirname: &str) -> Result<()> {
    let path = journal_path(cache_root, dirname);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("deleting {}", path.display())),
    }
}
