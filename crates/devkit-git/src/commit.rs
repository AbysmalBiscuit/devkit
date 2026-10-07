//! [`VersionControl::commit`](devkit_vcs::VersionControl::commit) for git.
//!
//! A patch is committed from a private index built from HEAD, so the shared
//! index, which other sessions stage into, is held locked and rewritten only
//! once the new HEAD is confirmed. What it held is replayed on top of the
//! commit with `merge-tree`, and anything that cannot replay exactly is
//! refused before HEAD moves. Every commit hook runs through a wrapper that
//! also watches the reference transaction, so a hook that changes the
//! committed tree aborts the commit instead of publishing it.

use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Output,
};

use anyhow::{Context, Result, bail, ensure};
use devkit_vcs::Selection;

use super::{Git, REDIRECTING_VARS, SLOW_TIMEOUT};

/// The hidden `devkit` verb a commit hook wrapper runs:
/// `devkit <verb> <state> <hook> [args]...`.
pub const HOOK_VERB: &str = "git-commit-hook";

/// Operations git is in the middle of, under the git directory. Committing
/// beside one would leave it to resume against a HEAD it did not make.
const OPERATION_PATHS: [&str; 6] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

/// The author of the throwaway commits `merge-tree` takes as input. They are
/// never referenced, so no real identity, configured or not, is needed.
const THROWAWAY_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "devkit commit"),
    ("GIT_AUTHOR_EMAIL", "devkit-commit@localhost"),
    ("GIT_COMMITTER_NAME", "devkit commit"),
    ("GIT_COMMITTER_EMAIL", "devkit-commit@localhost"),
];

pub(crate) fn commit(dir: &Path, selection: &Selection<'_>, message: &str) -> Result<String> {
    match selection {
        Selection::Paths(paths) => commit_paths(dir, paths, message),
        Selection::Patch(patch) => commit_patch(dir, patch, message),
        Selection::Amend => report(
            Git::at(dir)
                .args(["commit", "--amend", "--only", "-m", message])
                .timeout(SLOW_TIMEOUT)
                .wait()?,
        ),
    }
}

/// stdout of a `git commit` that succeeded, else an error carrying its
/// output, hook output included.
fn report(out: Output) -> Result<String> {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    ensure!(
        out.status.success(),
        "git commit failed ({}); HEAD is unchanged:\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim_end(),
        if stdout.trim().is_empty() {
            String::new()
        } else {
            format!("\n{}", stdout.trim_end())
        }
    );
    Ok(stdout)
}

/// `git commit --only`, which commits the paths from a temporary index and
/// then updates just those entries in the shared one. A path-limited commit
/// can name only a path git knows, so an untracked one is first recorded as
/// intended, without content, and forgotten again if the commit fails.
fn commit_paths(dir: &Path, paths: &[PathBuf], message: &str) -> Result<String> {
    ensure!(!paths.is_empty(), "name at least one path to commit");
    let paths = paths
        .iter()
        .map(|p| super::backend::utf8(p))
        .collect::<Result<Vec<_>>>()?;
    let untracked = Git::at(dir)
        .args([
            "--literal-pathspecs",
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .args(paths.iter().copied())
        .output()?;
    let untracked: Vec<&str> = untracked.split('\0').filter(|p| !p.is_empty()).collect();
    let committed = (|| {
        if !untracked.is_empty() {
            Git::at(dir)
                .args(["--literal-pathspecs", "add", "--intent-to-add", "--"])
                .args(untracked.iter().copied())
                .output()?;
        }
        report(
            Git::at(dir)
                .args([
                    "--literal-pathspecs",
                    "commit",
                    "--only",
                    "-m",
                    message,
                    "--",
                ])
                .args(paths.iter().copied())
                .timeout(SLOW_TIMEOUT)
                .wait()?,
        )
    })();
    if committed.is_err() && !untracked.is_empty() {
        Git::at(dir)
            .args([
                "--literal-pathspecs",
                "rm",
                "--cached",
                "-q",
                "--ignore-unmatch",
                "--",
            ])
            .args(untracked.iter().copied())
            .output()
            .context("forgetting the paths recorded for the refused commit")?;
    }
    committed
}

/// Locations under the git directory, absolute.
fn git_paths<const N: usize>(top: &Path, names: [&str; N]) -> Result<[PathBuf; N]> {
    let mut git = Git::at(top).args(["rev-parse", "--path-format=absolute"]);
    for name in names {
        git = git.args(["--git-path", name]);
    }
    let out = git.output()?;
    let found: Vec<PathBuf> = out.lines().map(PathBuf::from).collect();
    found
        .try_into()
        .map_err(|found| anyhow::anyhow!("`git rev-parse --git-path` answered {found:?}"))
}

/// The index lock this commit holds and the scratch directory beside the
/// index. Both are removed on the way out unless `retain` is set, which marks
/// an outcome that needs a person to look before anything retries.
struct Held {
    lock: PathBuf,
    owns_lock: bool,
    scratch: Option<PathBuf>,
    retain: bool,
}

impl Held {
    fn recovery(&self) -> String {
        format!(
            "recovery files retained at {}; index lock: {}. Inspect HEAD before retrying.",
            self.scratch
                .as_deref()
                .map_or_else(|| "(none)".into(), |s| s.display().to_string()),
            self.lock.display()
        )
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.retain {
            return;
        }
        if self.owns_lock {
            let _ = fs::remove_file(&self.lock);
        }
        if let Some(scratch) = &self.scratch {
            let _ = fs::remove_dir_all(scratch);
        }
    }
}

/// A git call against one of the private indexes, with `core.hooksPath`
/// aimed at `hooks` so that writing the index runs no hook of the
/// repository's.
fn private(top: &Path, index: &Path, hooks: &Path) -> Result<Git> {
    Ok(Git::at(top)
        .args(["-c", &format!("core.hooksPath={}", slashed(hooks)?)])
        .env("GIT_INDEX_FILE", index))
}

fn text(git: Git) -> Result<String> {
    Ok(git.output()?.trim().to_string())
}

fn commit_patch(dir: &Path, patch: &Path, message: &str) -> Result<String> {
    let patch = super::backend::utf8(patch)?.to_string();
    ensure!(Path::new(&patch).is_file(), "patch {patch}: no such file");
    let top = PathBuf::from(text(Git::at(dir).args(["rev-parse", "--show-toplevel"]))?);
    let [index, hooks_dir] = git_paths(&top, ["index", "hooks"])?;
    let lock = index.with_file_name(format!(
        "{}.lock",
        index
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("index")
    ));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
    {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "{}: another git operation holds the index lock",
                lock.display()
            )
        }
        Err(e) => return Err(e).with_context(|| format!("taking {}", lock.display())),
    }
    let mut held = Held {
        lock,
        owns_lock: true,
        scratch: None,
        retain: false,
    };
    let result = commit_patch_locked(&top, &index, &hooks_dir, &patch, message, &mut held);
    match result {
        Err(e) if held.retain => Err(e.context(held.recovery())),
        other => other,
    }
}

fn commit_patch_locked(
    top: &Path,
    index: &Path,
    hooks_dir: &Path,
    patch: &str,
    message: &str,
    held: &mut Held,
) -> Result<String> {
    let operations = git_paths(top, OPERATION_PATHS)?;
    for (name, path) in OPERATION_PATHS.iter().zip(&operations) {
        ensure!(
            !path.exists(),
            "{name}: finish the git operation in progress first"
        );
    }
    ensure!(
        Git::at(top)
            .args(["ls-files", "--unmerged", "-z"])
            .output()?
            .is_empty(),
        "the index holds unresolved merge entries"
    );

    let scratch = index
        .parent()
        .context("the index has no parent directory")?
        .join(format!(
            "devkit-commit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
    fs::create_dir(&scratch).with_context(|| format!("creating {}", scratch.display()))?;
    held.scratch = Some(scratch.clone());
    let original = scratch.join("original.index");
    let prior = scratch.join("prior.index");
    let candidate = scratch.join("candidate.index");
    let replacement = scratch.join("replacement.index");
    let no_hooks = scratch.join("no-hooks");
    fs::create_dir(&no_hooks)?;

    let old_head = head_commit(top)?;
    let target = head_ref(top)?;
    ensure!(
        old_head.is_some() || target != "HEAD",
        "HEAD is neither a commit nor an unborn branch"
    );

    if index.exists() {
        fs::copy(index, &original).with_context(|| format!("copying {}", index.display()))?;
    } else {
        private(top, &original, &no_hooks)?
            .args(["read-tree", "--empty"])
            .output()?;
    }
    fs::copy(&original, &prior)?;
    let prior_tree = text(private(top, &prior, &no_hooks)?.args(["write-tree"]))?;
    private(top, &candidate, &no_hooks)?
        .args(["read-tree", old_head.as_deref().unwrap_or("--empty")])
        .output()?;
    let base_tree = text(private(top, &candidate, &no_hooks)?.args(["write-tree"]))?;
    private(top, &candidate, &no_hooks)?
        .args(["apply", "--cached", "--whitespace=nowarn", "--", patch])
        .output()
        .context("the patch does not apply to HEAD")?;
    let selected_tree = text(private(top, &candidate, &no_hooks)?.args(["write-tree"]))?;
    ensure!(
        selected_tree != base_tree,
        "the patch changes nothing relative to HEAD"
    );
    check_merge_drivers(
        top,
        &base_tree,
        &selected_tree,
        &prior_tree,
        &scratch.join("attributes.index"),
    )?;

    let [merge_base, ours, theirs] =
        [&base_tree, &selected_tree, &prior_tree].map(|tree| wrap_tree(top, tree));
    let merge_base = format!("--merge-base={}", merge_base?);
    let merged = Git::at(top)
        .args([
            "-c",
            "merge.renames=false",
            "-c",
            "merge.renormalize=false",
            "merge-tree",
            "--write-tree",
            &merge_base,
            &ours?,
            &theirs?,
        ])
        .wait()?;
    match merged.status.code() {
        Some(0) => {}
        Some(1) => bail!(
            "the patch overlaps staged changes; HEAD and the shared index are unchanged\n{}{}",
            String::from_utf8_lossy(&merged.stderr),
            String::from_utf8_lossy(&merged.stdout)
        ),
        _ => bail!(
            "git merge-tree could not run ({}); HEAD and the shared index are unchanged\n{}",
            merged.status,
            String::from_utf8_lossy(&merged.stderr).trim()
        ),
    }
    let merged_tree = String::from_utf8_lossy(&merged.stdout)
        .lines()
        .next()
        .context("git merge-tree named no tree")?
        .to_string();
    fs::copy(&original, &replacement)?;
    private(top, &replacement, &no_hooks)?
        .args(["read-tree", "-i", "-m", &prior_tree, &merged_tree])
        .output()?;

    let state = HookState {
        hooks: hooks_dir.to_path_buf(),
        old_head: old_head.clone(),
        target,
        selected_tree,
        proposed: scratch.join("proposed"),
    };
    let wrappers = write_wrappers(&scratch, &state)?;
    held.retain = true;
    let out = private(top, &candidate, &wrappers)?
        .args(["commit", "-m", message])
        .timeout(SLOW_TIMEOUT)
        .wait()?;
    let proposed = fs::read_to_string(&state.proposed).ok();
    let new_head = head_commit(top)?;
    if proposed.is_none() || new_head != proposed {
        if proposed.is_some() || new_head != old_head || out.status.success() {
            bail!(
                "HEAD could not be confirmed after git commit. Do not retry the commit.\n{}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        held.retain = false;
        return report(out);
    }

    install(&replacement, &held.lock, index).with_context(|| {
        format!(
            "commit {} was created, but the index could not be installed. \
             Original index: {}; replacement index: {}. \
             Do not retry the commit; recover the index first.",
            new_head.as_deref().unwrap_or_default(),
            original.display(),
            replacement.display()
        )
    })?;
    held.owns_lock = false;
    held.retain = false;
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        report.push_str(&format!(
            "commit {} was created and the staged changes kept, but git exited {}; \
             do not retry the commit.\n{}",
            new_head.as_deref().unwrap_or_default(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(report)
}

/// The commit HEAD names, or `None` on an unborn branch.
fn head_commit(dir: &Path) -> Result<Option<String>> {
    let out = Git::at(dir)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .wait()?;
    Ok(out
        .status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

/// The ref HEAD points at, or `HEAD` itself when it is detached.
fn head_ref(dir: &Path) -> Result<String> {
    let out = Git::at(dir).args(["symbolic-ref", "-q", "HEAD"]).wait()?;
    Ok(if out.status.success() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        "HEAD".to_string()
    })
}

/// Moves `replacement` into place through the lock this commit holds, the
/// way git itself replaces an index.
fn install(replacement: &Path, lock: &Path, index: &Path) -> Result<()> {
    fs::copy(replacement, lock)?;
    // Windows flushes only a handle opened for writing.
    fs::OpenOptions::new().write(true).open(lock)?.sync_all()?;
    fs::rename(lock, index)?;
    Ok(())
}

/// A commit holding `tree`, never referenced, because `merge-tree` takes
/// commits rather than trees before git 2.45.
fn wrap_tree(top: &Path, tree: &str) -> Result<String> {
    let mut git = Git::at(top);
    for (key, value) in THROWAWAY_IDENTITY {
        git = git.env(key, value);
    }
    text(git.args([
        "commit-tree",
        "--no-gpg-sign",
        "-m",
        "devkit commit merge input",
        tree,
    ]))
}

/// Refuse when a path both sides change would merge through anything but
/// git's own text or binary driver: a custom or `union` driver may not
/// reproduce the staged hunks exactly.
fn check_merge_drivers(
    top: &Path,
    base: &str,
    selected: &str,
    prior: &str,
    no_index: &Path,
) -> Result<()> {
    let changed = |tree: &str| -> Result<BTreeSet<String>> {
        let out = Git::at(top)
            .args([
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                "-r",
                "-z",
                base,
                tree,
            ])
            .wait()?;
        ensure!(
            out.status.success(),
            "git diff-tree failed ({})",
            out.status
        );
        let paths = String::from_utf8(out.stdout).context("a changed path is not UTF-8")?;
        Ok(paths
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect())
    };
    let overlap: Vec<String> = changed(selected)?
        .intersection(&changed(prior)?)
        .cloned()
        .collect();
    if overlap.is_empty() {
        return Ok(());
    }
    // An index that does not exist makes the working tree's attributes the
    // only ones read, as a merge into it would.
    let attributes = Git::at(top)
        .env("GIT_INDEX_FILE", no_index)
        .args(["--literal-pathspecs", "check-attr", "-z", "merge", "--"])
        .args(overlap.iter().map(String::as_str))
        .output()?;
    let fields: Vec<&str> = attributes.split('\0').collect();
    for [path, _, value] in fields.as_chunks::<3>().0 {
        if matches!(*value, "set" | "unset") {
            continue;
        }
        let driver = if *value == "unspecified" {
            Git::at(top)
                .args(["config", "--get", "merge.default"])
                .wait()?
                .stdout
        } else {
            value.as_bytes().to_vec()
        };
        let driver = String::from_utf8_lossy(&driver).trim().to_string();
        let driver = if driver.is_empty() {
            "text".to_string()
        } else {
            driver
        };
        let custom = Git::at(top)
            .args(["config", "--get", &format!("merge.{driver}.driver")])
            .success()?;
        ensure!(
            matches!(driver.as_str(), "text" | "binary") && !custom,
            "{path}: merge driver `{driver}` cannot keep the staged hunks exact"
        );
    }
    Ok(())
}

/// A path as a shell script and git config read it: forward slashes, which
/// git for Windows also accepts.
fn slashed(path: &Path) -> Result<String> {
    Ok(super::backend::utf8(path)?.replace('\\', "/"))
}

/// What a hook wrapper needs to know about the commit it guards.
struct HookState {
    /// The repository's own hooks directory.
    hooks: PathBuf,
    old_head: Option<String>,
    /// The ref the commit moves: HEAD's branch, or `HEAD` when detached.
    target: String,
    /// The tree the commit must record.
    selected_tree: String,
    /// Where the wrapper writes the commit it saw prepared.
    proposed: PathBuf,
}

impl HookState {
    fn write(&self, path: &Path) -> Result<()> {
        let fields = [
            slashed(&self.hooks)?,
            self.old_head.clone().unwrap_or_default(),
            self.target.clone(),
            self.selected_tree.clone(),
            slashed(&self.proposed)?,
        ];
        write_synced(path, fields.join("\n").as_bytes())
    }

    fn read(path: &Path) -> Result<Self> {
        let body =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let fields: Vec<&str> = body.split('\n').collect();
        let [hooks, old_head, target, selected_tree, proposed] = fields[..] else {
            bail!("{}: not a commit hook state", path.display())
        };
        Ok(Self {
            hooks: hooks.into(),
            old_head: (!old_head.is_empty()).then(|| old_head.to_string()),
            target: target.to_string(),
            selected_tree: selected_tree.to_string(),
            proposed: proposed.into(),
        })
    }
}

fn write_synced(path: &Path, data: &[u8]) -> Result<()> {
    let mut file = fs::File::create(path).with_context(|| format!("writing {}", path.display()))?;
    file.write_all(data)?;
    file.sync_all()?;
    Ok(())
}

/// A wrapper for every hook the repository has, plus `reference-transaction`,
/// each running this executable's [`HOOK_VERB`].
fn write_wrappers(scratch: &Path, state: &HookState) -> Result<PathBuf> {
    let state_path = scratch.join("hook-state");
    state.write(&state_path)?;
    let wrappers = scratch.join("hooks");
    fs::create_dir(&wrappers)?;
    let mut names = BTreeSet::from(["reference-transaction".to_string()]);
    if let Ok(entries) = fs::read_dir(&state.hooks) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                names.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    let exe = std::env::current_exe().context("locating the devkit executable")?;
    for name in names {
        let script = format!(
            "#!/bin/sh\nexec {} {HOOK_VERB} {} {} \"$@\"\n",
            quote(&slashed(&exe)?),
            quote(&slashed(&state_path)?),
            quote(&name)
        );
        let path = wrappers.join(&name);
        fs::write(&path, script)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(wrappers)
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The body of [`HOOK_VERB`]: run the repository's own `hook`, then, once the
/// commit's reference transaction is prepared, confirm it moves the expected
/// ref from the expected commit to one recording the selected tree. Returns
/// the exit status for the hook.
pub fn run_hook(state_path: &Path, hook: &str, args: &[String]) -> Result<i32> {
    let state = HookState::read(state_path)?;
    let payload = if hook == "reference-transaction" {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        Some(buf)
    } else {
        None
    };
    let cwd = std::env::current_dir()?;
    let mut git = Git::at(&cwd);
    for var in REDIRECTING_VARS {
        if let Some(value) = std::env::var_os(var) {
            git = git.env(var, value);
        }
    }
    let mut git = git.args([
        "-c",
        &format!("core.hooksPath={}", slashed(&state.hooks)?),
        "hook",
        "run",
        "--ignore-missing",
    ]);
    let stdin_file = state_path.with_extension(format!("stdin-{}", std::process::id()));
    if let Some(payload) = &payload {
        fs::write(&stdin_file, payload)?;
        git = git.args(["--to-stdin", &slashed(&stdin_file)?]);
    }
    let out = git
        .args([hook, "--"])
        .args(args.iter().map(String::as_str))
        .timeout(SLOW_TIMEOUT)
        .wait();
    let _ = fs::remove_file(&stdin_file);
    let out = out?;
    std::io::stdout().write_all(&out.stdout)?;
    std::io::stderr().write_all(&out.stderr)?;
    let code = out.status.code().unwrap_or(1);
    if code != 0 || hook != "reference-transaction" || args != ["prepared"] {
        return Ok(code);
    }
    let payload = String::from_utf8_lossy(payload.as_deref().unwrap_or_default()).into_owned();
    verify_transaction(&cwd, &state, &payload)?;
    Ok(0)
}

fn verify_transaction(cwd: &Path, state: &HookState, payload: &str) -> Result<()> {
    ensure!(
        head_ref(cwd)? == state.target,
        "HEAD now names another ref; commit refused"
    );
    let mut matched = false;
    for line in payload.lines() {
        let mut fields = line.splitn(3, ' ');
        let (Some(old), Some(new), Some(name)) = (fields.next(), fields.next(), fields.next())
        else {
            bail!("unreadable reference transaction line {line:?}");
        };
        if name != state.target && name != "HEAD" {
            continue;
        }
        matched = true;
        let expected_old = state
            .old_head
            .clone()
            .unwrap_or_else(|| "0".repeat(new.len()));
        ensure!(
            old == expected_old,
            "HEAD moved while the commit was prepared; commit refused"
        );
        let tree = text(Git::at(cwd).args(["rev-parse", &format!("{new}^{{tree}}")]))?;
        ensure!(
            tree == state.selected_tree,
            "a commit hook changed the committed tree; commit refused"
        );
        write_synced(&state.proposed, new.as_bytes())?;
    }
    if !matched {
        let proposed = fs::read_to_string(&state.proposed).ok();
        let head = text(Git::at(cwd).args(["rev-parse", "HEAD"]))?;
        ensure!(
            proposed.as_deref() == Some(head.as_str()),
            "a reference update does not move the expected HEAD; commit refused"
        );
    }
    Ok(())
}
