//! Where the baseline comparison starts. Every consumer of `baseline_ref`
//! resolves through here, so a project that declares nothing still gets the
//! remote's default branch.

mod locks;

use crate::issue::checkout::with_cleanup;
use crate::issue::setup::{backfill_includes, prep_apps, run_after_worktree_create};
use anyhow::{Context, Result};
use devkit_common::git::Git;
use devkit_common::progress::Steps;
use devkit_common::record::RecordState;
use devkit_common::worktree::{BASELINE_MARKER, BaselineState};
use devkit_config::{Config, expand_tilde};
use devkit_ports::apps::App;
use devkit_ports::registry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// The ref a worktree's baseline is measured against: the configured
/// `baseline_ref`, else the remote's default branch.
pub fn target(cfg: &Config, repo: &Path) -> Result<String> {
    if !cfg.defaults.baseline_ref.is_empty() {
        return Ok(cfg.defaults.baseline_ref.clone());
    }
    devkit_common::git::default_remote_branch(repo).context(
        "no baseline target: set `defaults.baseline_ref`, \
         or run `git remote set-head origin -a` so origin/HEAD names one",
    )
}

/// The commit this worktree forked from: the merge base of its HEAD and
/// `target`. Local refs only, so no fetch is needed — extending a branch does
/// not move its merge base with another branch, and the value changes only
/// when the worktree is rebased.
pub fn pin(worktree: &Path, target: &str) -> Result<String> {
    let out = devkit_common::git::Git::at(worktree)
        .args(["merge-base", "HEAD", target])
        .output()
        .with_context(|| format!("resolving the fork point between HEAD and `{target}`"))?;
    let sha = out.trim();
    anyhow::ensure!(
        !sha.is_empty(),
        "`git merge-base HEAD {target}` named no commit"
    );
    Ok(sha.to_string())
}

/// One app's prep fingerprint at the sha the baseline was built from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppMark {
    pub fingerprint: String,
}

/// The contents of a baseline worktree's marker file: the sha it was built at,
/// and each app's prep fingerprint at that build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub sha: String,
    #[serde(default)]
    pub apps: BTreeMap<String, AppMark>,
}

/// The result of reading a baseline's marker. `Absent` and `Unusable` are
/// both "no marker to trust", but only `Absent` means a fresh baseline may be
/// built here — `Unusable` means something occupies the path already and
/// must be dealt with before a rebuild can proceed.
pub enum MarkerState {
    Ok(Marker),
    Unusable,
    Absent,
}

/// Written last, after every bootstrap step, so its presence is what makes a
/// baseline complete: a directory without one is an interrupted bootstrap
/// whatever its HEAD says. It also carries identity, which lets a stray
/// directory be told from a real baseline, and each app's prep fingerprint.
pub fn write_marker(dir: &Path, m: &Marker) -> Result<()> {
    let p = dir.join(BASELINE_MARKER);
    let parent = p.parent().expect("marker path has a parent");
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let body = toml::to_string(m).context("serializing baseline marker")?;
    // Rename rather than write in place: a crash partway through a write would
    // otherwise leave a file that parses as neither a marker nor its absence.
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("renaming into {}", p.display()))
}

/// Reads a baseline's marker. A missing file is `Absent`: nothing was ever
/// built here, so the directory is free to build fresh. Any other read
/// failure (permission denied, the path occupied by something unreadable as
/// a file) is `Unusable`, not `Absent` — an I/O error does not prove there is
/// no baseline, only that this call could not confirm one, and treating that
/// as a clean slate risks building over a baseline other worktrees still
/// reference. A file that reads but does not parse as TOML is `Unusable` for
/// the same reason: something occupies the path and cannot be trusted.
pub fn read_marker(dir: &Path) -> MarkerState {
    match std::fs::read_to_string(dir.join(BASELINE_MARKER)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MarkerState::Absent,
        Err(_) => MarkerState::Unusable,
        Ok(body) => match toml::from_str(&body) {
            Ok(m) => MarkerState::Ok(m),
            Err(_) => MarkerState::Unusable,
        },
    }
}

/// Candidate directories tried for one sha before giving up. Two different
/// shas sharing a 12-character prefix is already rare; this many sharing one
/// is far beyond plausible, so hitting the bound signals a bug (or a
/// deliberately doctored marker) rather than a baseline directory to build.
const MAX_SLOT_CANDIDATES: u32 = 64;

/// Which directory serves `sha`, and what state it is in.
#[must_use]
pub enum Slot {
    /// A complete baseline for this exact sha is already here.
    Reuse(PathBuf, Marker),
    /// Something occupies the path but its marker cannot be trusted.
    Rebuild(PathBuf),
    /// Nothing occupies the path: build fresh here.
    Create(PathBuf),
    /// No candidate resolved within `MAX_SLOT_CANDIDATES` tries.
    Exhausted(String),
}

/// Directory-name form of a sha. Twelve hex characters is 48 bits against a
/// few dozen directories, and it leaves Windows path headroom a 40-character
/// name would spend.
///
/// Takes the first 12 *characters*, not bytes: slicing a `&str` by raw byte
/// index panics when that index falls inside a multi-byte character. Real
/// shas are hex ASCII where the two coincide, but this function accepts any
/// `&str`, so it walks char boundaries instead of assuming one.
pub fn short(sha: &str) -> &str {
    match sha.char_indices().nth(12) {
        Some((idx, _)) => &sha[..idx],
        None => sha,
    }
}

/// Which directory serves `sha`, and in what state. An interrupted bootstrap
/// leaves a registered worktree with no marker; classifying that as occupied
/// would strand it, since the baseline would move to `_2`, prune reports
/// rather than removes it, and the worktree filter would stop recognizing it.
///
/// Twelve-character names collide across unrelated shas, so a marker naming
/// a different sha does not mean rebuild — it means this sha belongs in the
/// next candidate, `<short>_2` and onward. The walk is bounded
/// (`MAX_SLOT_CANDIDATES`) so a run of collisions reports `Exhausted` instead
/// of looping forever.
pub fn slot(baseline_dir: &Path, sha: &str) -> Slot {
    let base = short(sha);
    for n in 1..=MAX_SLOT_CANDIDATES {
        let name = if n == 1 {
            base.to_string()
        } else {
            format!("{base}_{n}")
        };
        let path = baseline_dir.join(&name);
        match read_marker(&path) {
            MarkerState::Ok(m) if m.sha == sha => return Slot::Reuse(path, m),
            MarkerState::Ok(_) => continue,
            MarkerState::Unusable => return Slot::Rebuild(path),
            MarkerState::Absent => {
                return match std::fs::metadata(&path) {
                    Ok(_) => Slot::Rebuild(path),
                    Err(_) => Slot::Create(path),
                };
            }
        }
    }
    Slot::Exhausted(format!(
        "no free or reusable baseline slot for `{sha}` after {MAX_SLOT_CANDIDATES} \
         candidates under `{}`",
        baseline_dir.display()
    ))
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn eat(mut h: u64, bytes: &[u8]) -> u64 {
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// What an app was prepped from. A bare "this app is prepped" flag would go
/// stale: a project that adds a key to an app's env file would give issue
/// worktrees the new key and baselines the old one forever, and
/// `issue sync-includes` no longer reaches baselines.
///
/// FNV-1a rather than `DefaultHasher`, whose output is explicitly not stable
/// between Rust releases. This value is stored in the marker and compared on a
/// later run, possibly under a different toolchain.
pub fn fingerprint(app: &App, includes: &[String]) -> String {
    let mut h = FNV_OFFSET;
    for f in &app.prep_files {
        h = eat(h, f.path.as_bytes());
        h = eat(h, b"\0");
        h = eat(h, f.content.as_bytes());
        h = eat(h, b"\0");
        h = eat(h, &[u8::from(f.overwrite)]);
    }
    for cmd in &app.setup {
        for part in cmd {
            h = eat(h, part.as_bytes());
            h = eat(h, b"\0");
        }
        h = eat(h, b"\x01");
    }
    for i in includes {
        h = eat(h, i.as_bytes());
        h = eat(h, b"\0");
    }
    format!("{h:016x}")
}

/// A baseline is one shared tree, so it renders with one stable identity rather
/// than borrowing the identity of whichever worktree happened to create it.
/// Keying on the sha keeps two baselines from sharing per-issue resources.
/// This is the render context every baseline bootstrap uses for `prep_files`
/// and, extended with `worktree`, for `after_worktree_create`; it carries
/// every key those templates are documented to see (`prefix`, `issue`,
/// `slug`, `branch`, `role`, plus `apps` and `sha` for the bootstrap itself)
/// so a project's template renders in a baseline exactly as it does in an
/// issue worktree: `template::render` is strict, and a key missing here is a
/// hard bootstrap failure there.
fn bootstrap_context(sha: &str, apps: &[String], prefix: &str) -> serde_json::Value {
    let id = format!("baseline-{}", short(sha));
    serde_json::json!({
        "prefix": prefix,
        "issue": id,
        "slug": id,
        "branch": id,
        "role": "baseline",
        "sha": sha,
        "apps": apps,
    })
}

/// The directory the baselines of this project live in.
pub fn dir(cfg: &Config) -> Result<PathBuf> {
    anyhow::ensure!(
        !cfg.defaults.baseline_dir.is_empty(),
        "a baseline needs `defaults.worktree_root` or `defaults.baseline_dir`"
    );
    Ok(expand_tilde(&cfg.defaults.baseline_dir))
}

/// [`dir`] with the second misconfiguration that makes a slot under it unsafe
/// to name already refused. The unset directory is reported first: a config
/// naming no baseline directory is the problem to fix, whatever the sha.
fn baselines_root(cfg: &Config, sha: &str) -> Result<PathBuf> {
    let root = dir(cfg)?;
    // An empty sha names the baseline root itself as its slot, and a `Rebuild`
    // there would take the whole directory — every other baseline with it.
    anyhow::ensure!(!sha.is_empty(), "a baseline needs a fork-point commit");
    Ok(root)
}

/// The directory [`ensure`] would serve `sha` from, resolved without building
/// or creating anything — the existing slot when one holds this sha, else the
/// candidate a build would take. A path that does not exist yet is the honest
/// answer for a fork point with no baseline.
pub fn planned_path(cfg: &Config, sha: &str) -> Result<PathBuf> {
    let root = baselines_root(cfg, sha)?;
    match slot(&root, sha) {
        Slot::Reuse(p, _) | Slot::Rebuild(p) | Slot::Create(p) => Ok(p),
        Slot::Exhausted(why) => anyhow::bail!(why),
    }
}

/// The baseline directory for `sha`, created if needed. Reuse preps only what
/// has drifted; a new or interrupted tree is built from scratch.
///
/// Runs before the caller resolves ports, so a long bootstrap cannot outlive a
/// reservation taken alongside it.
///
/// Takes the slot lock and never the directory lock, which is the half of the
/// fixed order (directory, then slot) that belongs here.
pub fn ensure(
    cfg: &Config,
    catalog: &HashMap<String, App>,
    primary: &Path,
    sha: &str,
    apps: &[String],
    steps: &Steps,
) -> Result<PathBuf> {
    let root = baselines_root(cfg, sha)?;
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;

    // The slot is resolved twice: once to name the lock, once inside it, since
    // another process may have finished a bootstrap while this one waited.
    let name = match slot(&root, sha) {
        Slot::Reuse(p, _) | Slot::Rebuild(p) | Slot::Create(p) => slot_name(&p)?,
        Slot::Exhausted(why) => anyhow::bail!(why),
    };
    locks::with_slot(&root, &name, || {
        let ctx = bootstrap_context(sha, apps, &cfg.defaults.branch_prefix);
        let vars = &cfg.templates.variables;
        let includes = &cfg.defaults.worktree_include;
        let primary_s = primary
            .to_str()
            .context("primary checkout path not UTF-8")?;
        let env = [(locks::REENTRY_VAR, name.as_str())];

        let (path, mut marker, built_here) = match slot(&root, sha) {
            Slot::Reuse(path, marker) => (path, marker, false),
            Slot::Rebuild(path) => {
                let path_s = baseline_path_str(&path)?;
                // Always `--force`: the tree may hold rendered prep files and
                // include copies that a plain remove would refuse over.
                let _ = Git::at(primary)
                    .args(["worktree", "remove", "--force", path_s])
                    .timeout(devkit_common::git::SLOW_TIMEOUT)
                    .output();
                let _ = std::fs::remove_dir_all(&path);
                create(primary, &path, sha, steps)?;
                (path, fresh_marker(sha), true)
            }
            Slot::Create(path) => {
                create(primary, &path, sha, steps)?;
                (path, fresh_marker(sha), true)
            }
            Slot::Exhausted(why) => anyhow::bail!(why),
        };

        let mut build = || -> Result<()> {
            backfill_includes(primary_s, &path, includes, steps);
            let stale: Vec<String> = apps
                .iter()
                .filter(|a| {
                    catalog.get(*a).is_some_and(|app| {
                        marker.apps.get(*a).map(|m| m.fingerprint.as_str())
                            != Some(fingerprint(app, includes).as_str())
                    })
                })
                .cloned()
                .collect();
            if !stale.is_empty() {
                let branch = format!("baseline-{}", short(sha));
                steps.during_result("Preparing apps…", || {
                    prep_apps(&path, &branch, &stale, catalog, &ctx, vars)
                })?;
            }
            let mut hook_ctx = ctx.clone();
            if let Some(obj) = hook_ctx.as_object_mut() {
                obj.insert(
                    "worktree".into(),
                    serde_json::Value::String(path.to_string_lossy().into_owned()),
                );
            }
            run_after_worktree_create(
                &path,
                &cfg.hooks.after_worktree_create,
                &hook_ctx,
                vars,
                &env,
                steps,
            );
            for a in apps {
                if let Some(app) = catalog.get(a) {
                    marker.apps.insert(
                        a.clone(),
                        AppMark {
                            fingerprint: fingerprint(app, includes),
                        },
                    );
                }
            }
            // Last, so an interrupted bootstrap leaves no marker and the probe
            // table classifies the tree as `Rebuild` rather than complete.
            write_marker(&path, &marker)
        };

        // A tree this call built gets removed on failure, so a half-built
        // baseline never survives to be reused. A reused one is left standing:
        // its marker still describes what is actually prepped, other worktrees
        // and their running servers depend on the directory, and the drift that
        // failed here is retried on the next run.
        if built_here {
            with_cleanup(&path, primary_s, build)?;
        } else {
            build()?;
        }
        Ok(path)
    })
}

fn fresh_marker(sha: &str) -> Marker {
    Marker {
        sha: sha.to_string(),
        apps: BTreeMap::new(),
    }
}

/// Detached so the baseline never occupies a branch name and never shows up in
/// a session manager's branch list.
fn create(primary: &Path, path: &Path, sha: &str, steps: &Steps) -> Result<()> {
    let path_s = baseline_path_str(path)?;
    // A directory removed by hand leaves a registration behind; `worktree add`
    // refuses over it until the registration is pruned.
    let _ = Git::at(primary).args(["worktree", "prune"]).output();
    steps.during_result("Creating baseline worktree…", || {
        Git::at(primary)
            .args(["worktree", "add", "--detach", path_s, sha])
            .timeout(devkit_common::git::SLOW_TIMEOUT)
            .output()
    })?;
    Ok(())
}

fn baseline_path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("baseline path not UTF-8: {}", path.display()))
}

/// The lock key for a slot: its directory name, which is what `locks::with_slot`
/// takes.
fn slot_name(path: &Path) -> Result<String> {
    Ok(path
        .file_name()
        .with_context(|| format!("baseline slot has no directory name: {}", path.display()))?
        .to_string_lossy()
        .into_owned())
}

/// Which worktrees name which baseline, as of one scan.
pub struct References {
    /// Baseline path → the worktrees naming it.
    pub by_baseline: BTreeMap<PathBuf, Vec<PathBuf>>,
    /// Worktrees this scan could not read a baseline out of: the record exists
    /// and does not parse, or the tree could not even be classified. Which
    /// baseline each names is unknown, so no baseline is provably unreferenced
    /// while any of these exist.
    pub unreadable: Vec<PathBuf>,
}

impl References {
    /// The worktrees naming `baseline`, from every entry that is the same
    /// directory as it. The map is keyed by the path each record spells, and
    /// two records can spell one directory differently — a symlinked parent, or
    /// a `baseline_dir` a per-worktree config layer resolves elsewhere — so an
    /// entry is matched by identity, the comparison the worktrees inside it
    /// already get. A lookup by exact key reads a reference spelled another way
    /// as no reference at all, and every caller acts on that answer: one
    /// exempts the baseline from the cross-worktree gate, one deletes it, and
    /// one shows an operator a baseline nothing appears to need.
    ///
    /// Several entries can be the same directory, so this unions them rather
    /// than taking the first match.
    fn naming(&self, baseline: &Path) -> Vec<&Path> {
        self.by_baseline
            .iter()
            .filter(|(spelling, _)| devkit_common::git::same_path(spelling, baseline))
            .flat_map(|(_, holders)| holders.iter().map(PathBuf::as_path))
            .collect()
    }

    /// The worktrees this scan could not read, as a list to print.
    fn unreadable_names(&self) -> String {
        self.unreadable
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Why this scan can prove nothing, naming the worktrees to repair. A scan
    /// that stayed silent would leave a user with one corrupt
    /// `.devkit/issue.toml` watching baselines never get reclaimed, with
    /// nothing to point at.
    fn unreadable_note(&self) -> Option<String> {
        if self.unreadable.is_empty() {
            return None;
        }
        Some(format!(
            "no baseline can be reclaimed while these worktrees cannot be read: {}",
            self.unreadable_names()
        ))
    }
}

/// Which worktrees name each baseline. Derived rather than stored: a registry
/// would keep a phantom reference alive after a plain `git worktree remove`,
/// and the fix for that is this scan with a file to maintain beside it.
///
/// The primary checkout is scanned alongside the linked worktrees: `devrun up
/// --role baseline` run from there writes a record naming the baseline, and a
/// scan that skipped it would let a sibling `issue end` reclaim a baseline the
/// primary checkout is still serving from.
///
/// A worktree that could not be classified counts as unreadable rather than as
/// a baseline. Folding it in with the baselines would drop it from the scan
/// before its record was ever read, and a worktree whose reference nobody can
/// see is exactly what the unreadable list exists to stop.
pub fn referencers(repo: &str) -> Result<References> {
    let trees = devkit_common::worktree::discover_all(repo)?;
    let mut by_baseline: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut unreadable = trees.undecidable;
    for path in std::iter::once(trees.main).chain(trees.linked.into_iter().map(|w| w.path)) {
        match devkit_common::record::read_state(&path) {
            RecordState::Ok(r) => {
                if let Some(b) = r.baseline {
                    by_baseline
                        .entry(PathBuf::from(b.path))
                        .or_default()
                        .push(path);
                }
            }
            RecordState::Unusable => unreadable.push(path),
            RecordState::Absent => {}
        }
    }
    Ok(References {
        by_baseline,
        unreadable,
    })
}

/// Whether any live port row is held by `baseline`.
///
/// Holders are compared by identity, the way `References::naming` compares
/// them: a sweep enumerates the baseline directory, so it meets whatever
/// spelling that directory hands it, while a row was written from the spelling
/// the worktree's config resolved. A text compare reads two spellings of one
/// directory as two holders and lets a tree with running servers past the one
/// refusal that exists to protect it.
pub fn live_rows_hold(baseline: &Path, ports: &registry::Data) -> bool {
    ports.entries.values().any(|e| {
        e.pid.is_some_and(registry::pid_alive)
            && devkit_common::git::same_path(Path::new(&e.holder), baseline)
    })
}

/// The ports one holder owns, for `run::bring_down_ports`.
pub fn rows_for_holder(holder: &str, ports: &registry::Data) -> Vec<u16> {
    ports
        .entries
        .iter()
        .filter(|(_, e)| e.holder == holder)
        .map(|(p, _)| *p)
        .collect()
}

/// Whether any worktree other than `worktree` names `baseline` in this scan.
/// The two callers of it — a repin releasing what it abandons, and `down`
/// deciding whether a baseline is foreign — ask the same question, and both
/// must read a scan they cannot prove complete as shared.
///
/// Both sides are compared by identity, never by text: the referencer paths
/// come from `git worktree list` while a caller's own path comes from a
/// `rev-parse`, and the baseline each record spells is whatever that worktree's
/// config resolved. Any of those can spell one directory two ways.
pub fn shared_with_others(refs: &References, baseline: &Path, worktree: &Path) -> bool {
    refs.naming(baseline)
        .iter()
        .any(|w| !devkit_common::git::same_path(w, worktree))
}

/// Run `f` while holding `baseline`'s slot lock, which is what serializes a
/// decision about who references it against the `up` that would change the
/// answer.
pub fn with_slot_lock<T>(baseline: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let root = baseline.parent().context("baseline path has no parent")?;
    locks::with_slot(root, &slot_name(baseline)?, f)
}

/// Hand the rows held by `previous` to `down`, when `worktree` is the only
/// worktree still naming it. A worktree whose pin moves would otherwise leave
/// servers under a holder it no longer reaches: unreachable without `--holder`
/// and a terminal, unreachable from MCP, and enough to make prune refuse the
/// directory forever.
///
/// A baseline several worktrees share is nobody's to stop. Stopping it here
/// would take another worktree's servers down from a run that has no terminal
/// to confirm with, which is exactly what the cross-worktree gate on `devrun
/// down` exists to prevent.
///
/// The referencer scan and `down` both run under `previous`'s slot lock, which
/// narrows the time-of-check race the reuse path makes reachable: between the
/// two, another worktree's `up` adopts this baseline and — `up` being
/// idempotent for a live pid — reports these very servers as its own running
/// baseline, moments before they are stopped. The window is narrowed rather
/// than closed, because `up` writes its record after `ensure` has returned and
/// released this lock; closing it needs that pin write to move inside `ensure`.
pub fn release_abandoned(
    repo: &str,
    worktree: &Path,
    previous: &Path,
    down: impl FnOnce(&[u16]) -> Result<()>,
) -> Result<()> {
    with_slot_lock(previous, || {
        // A pin is a plain path in a file a person can edit, and these rows are
        // stopped by pid. A path carrying no baseline marker names servers this
        // run has no claim on, and neither does one whose marker cannot be
        // read: an unproven claim is no claim.
        match devkit_common::worktree::baseline_state(previous) {
            BaselineState::Yes => {}
            BaselineState::No => return Ok(()),
            BaselineState::Unknown => {
                eprintln!(
                    "warning: leaving the servers under {} running: its {} can be neither \
                     read nor ruled out, so this run cannot tell the tree is a baseline",
                    previous.display(),
                    BASELINE_MARKER
                );
                return Ok(());
            }
        }
        let refs = referencers(repo)?;
        if !refs.unreadable.is_empty() {
            eprintln!(
                "warning: leaving the servers under {} running: these worktrees could not \
                 be read, and any of them could name it: {}",
                previous.display(),
                refs.unreadable_names()
            );
            return Ok(());
        }
        if shared_with_others(&refs, previous, worktree) {
            return Ok(());
        }
        let rows = rows_for_holder(&previous.to_string_lossy(), &registry::snapshot()?);
        if rows.is_empty() {
            return Ok(());
        }
        down(&rows)
    })
}

/// Remove `baseline` when nothing references it any more. The caller's own
/// worktree must already be gone: counting while it still exists makes two
/// concurrent `issue end` runs each see the other and each decline.
pub fn drop_reference(
    repo: &str,
    baseline: &Path,
    ports: &registry::Data,
    force: bool,
) -> Result<bool> {
    // The locks are named from the path's parent while `git worktree remove`
    // resolves the same path against the repository, so a relative path locks
    // a file under the calling process's working directory and guards neither
    // the tree nor the callers of it. Config resolves `baseline_dir` to an
    // absolute path, which leaves a hand-edited record as the only source.
    anyhow::ensure!(
        baseline.is_absolute(),
        "baseline path must be absolute: {}",
        baseline.display()
    );
    let root = baseline.parent().context("baseline path has no parent")?;
    locks::with_dir(root, || {
        let refs = referencers(repo)?;
        if let Some(note) = refs.unreadable_note() {
            eprintln!("warning: {note}");
        }
        remove_if_unreferenced(
            repo,
            baseline,
            &refs,
            &registrations(repo)?,
            Gates::reference(force),
            ports,
            Sweep::Remove,
        )
    })
}

/// Whether a sweep acts on what it decides. Both modes reach their verdict
/// through the same gates on the same inputs, which is as far as the agreement
/// goes: `Report` cannot know whether the removal itself will succeed, nor what
/// the tree, the registry or the referencing worktrees look like by the time a
/// real sweep runs.
#[derive(Clone, Copy, PartialEq)]
enum Sweep {
    Remove,
    Report,
}

/// The waivable half of the removal policy. The two refusals are waived
/// separately: an operator whose baseline carries an edit — a lockfile a
/// bootstrap step rewrote is enough — must not have to switch off the
/// running-servers gate to get past it.
#[derive(Clone, Copy)]
pub struct Gates {
    /// Waives the running-servers refusal, and nothing else.
    force: bool,
    /// Whether a tree somebody has edited is a refusal. An operator's sweep
    /// refuses one unless told to discard it: the edit is the only thing in a
    /// baseline a rebuild would not bring back. `drop_reference` does not
    /// refuse: it has no waiver to offer, and it runs after the worktree that
    /// referenced the baseline is already gone.
    refuse_edits: bool,
}

impl Gates {
    /// The operator's sweep: `--force` waives running servers, `--discard-edits`
    /// waives a tree somebody edited, and neither waives the other.
    pub fn sweep(force: bool, discard_edits: bool) -> Self {
        Gates {
            force,
            refuse_edits: !discard_edits,
        }
    }

    fn reference(force: bool) -> Self {
        Gates {
            force,
            refuse_edits: false,
        }
    }
}

/// How a baseline is reclaimed, once every gate has passed.
#[derive(Debug)]
enum Removal {
    /// git has a registration for the tree, so git takes it down.
    Worktree,
    /// A marked tree git has no registration for. `git worktree remove` cannot
    /// take it — it is not a working tree as far as this repository knows — so
    /// the directory itself is all there is to reclaim.
    Orphan,
}

/// Whether this scan proves nothing references `baseline`. Nothing is provably
/// unreferenced while some worktree's own pin cannot be read, so an unreadable
/// scan answers no for every baseline; the caller prints the note naming the
/// worktrees to repair.
fn unreferenced(refs: &References, baseline: &Path) -> bool {
    refs.unreadable.is_empty() && refs.naming(baseline).is_empty()
}

/// Every gate between a baseline and its removal, reached without removing
/// anything: `Ok(None)` leaves it alone with nothing to say, `Err` is a refusal
/// to report, and `Ok(Some(_))` says it goes and by which mechanism.
///
/// This is the whole verdict and both sweep modes reach it here, so neither can
/// gate on something the other does not. What a dry run still cannot know is
/// whether the removal it reports would succeed, and whether any of these
/// answers still holds by the time a real sweep asks.
fn decide(
    baseline: &Path,
    refs: &References,
    registered: &[devkit_common::git::Worktree],
    gates: Gates,
    ports: &registry::Data,
) -> Result<Option<Removal>> {
    if !unreferenced(refs, baseline) {
        return Ok(None);
    }
    // The tree can be gone already: whoever held the slot lock before this call
    // may have been the last referencer too. There is nothing left to remove
    // and nothing went wrong.
    if !baseline.exists() {
        return Ok(None);
    }
    // Neither caller arrives with this tree proven to be devkit's: a sweep
    // enumerates whatever the baseline directory holds, and `drop_reference` is
    // handed a path out of a record a person can edit. `git worktree remove
    // --force` accepts a sibling linked worktree of the same repository and
    // takes its uncommitted work with it, so the marker is what decides whether
    // this path is ours to delete — and a marker that cannot be read decides
    // nothing. The two refusals are worded apart because they are different
    // problems: one path is somebody else's, the other is unreadable and may
    // well be a baseline.
    match devkit_common::worktree::baseline_state(baseline) {
        BaselineState::Yes => {}
        BaselineState::No => anyhow::bail!(
            "{} is not a baseline worktree; refusing to remove it",
            baseline.display()
        ),
        BaselineState::Unknown => anyhow::bail!(
            "cannot tell whether {} is a baseline worktree: its {} can be neither \
             read nor ruled out; refusing to remove it",
            baseline.display(),
            BASELINE_MARKER
        ),
    }
    // Both sides resolve or the guard refuses. A side that fell back to its
    // unresolved spelling would be compared against a resolved one, and on
    // Windows the two can never match at all — `canonicalize` returns a
    // `\\?\`-prefixed path and `current_dir` does not — so a fallback here
    // silently disarms the one check standing between the sweep and the
    // caller's own directory.
    let here = std::fs::canonicalize(baseline)
        .with_context(|| format!("resolving {}", baseline.display()))?;
    let cwd = std::env::current_dir().context("resolving the current directory")?;
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("resolving the current directory {}", cwd.display()))?;
    if cwd == here || cwd.starts_with(&here) {
        anyhow::bail!("cd out of {} before removing it", baseline.display());
    }
    // A live server in the tree is the one thing worth refusing for even when
    // nobody has touched it.
    if !gates.force && live_rows_hold(baseline, ports) {
        anyhow::bail!(
            "{} still has running servers; stop them or pass --force",
            baseline.display()
        );
    }
    match registration(registered, baseline) {
        None => return orphan_removal(baseline).map(Some),
        // The single `--force` the removal below passes does not override a
        // lock, and a repeated `-f -f` that would is not devkit's to pass on
        // somebody's behalf. So the lock is a refusal here rather than a
        // failure the real sweep walks into and reports on every run.
        Some(w) if w.locked => anyhow::bail!(
            "{} is a locked worktree; `git worktree unlock` it first",
            baseline.display()
        ),
        Some(_) => {}
    }
    // Modified *tracked* files are a person's edit. Untracked files are not the
    // same signal and are not counted: a baseline carries rendered prep files
    // and installed dependencies by design, so a check that counted them would
    // call every baseline dirty. This is also a different decision from the
    // `--force` the `git worktree remove` always carries, which exists only so
    // git stops refusing the removal over those same untracked files.
    if gates.refuse_edits && has_local_edits(baseline)? {
        anyhow::bail!(
            "{} has modified tracked files; commit or discard them, or pass --discard-edits",
            baseline.display()
        );
    }
    Ok(Some(Removal::Worktree))
}

/// Every worktree git has a registration for, baselines included:
/// `worktree::discover_all` drops those, and telling a registered tree from an
/// orphaned one is the whole question here. Read once per sweep: it is one git
/// process, and the baseline directory lock a sweep holds is one `issue end`
/// waits on unbounded.
fn registrations(repo: &str) -> Result<Vec<devkit_common::git::Worktree>> {
    devkit_common::git::worktrees(Path::new(repo))
}

/// git's registration for `baseline`, matched by identity for the same reason
/// `References::naming` is: git spells a path the way it was registered, a
/// sweep spells it the way the directory read it back.
fn registration<'a>(
    registrations: &'a [devkit_common::git::Worktree],
    baseline: &Path,
) -> Option<&'a devkit_common::git::Worktree> {
    registrations
        .iter()
        .find(|w| devkit_common::git::same_path(&w.path, baseline))
}

/// A marked tree this repository has no registration for, which is the state a
/// baseline is left in when its registration goes and its files stay. Reclaimed
/// as a plain directory — but only once nothing stands behind its `.git`: a
/// tree whose `.git` resolves is some repository's checkout, and a sweep run
/// from a different repository has no claim on it. The guard exists because
/// this deletion is the one that has no registration to check the tree against
/// — a `Slot::Rebuild` deletes a tree this repository registered moments
/// earlier and needs nothing of the sort — so the unproven case refuses with
/// the live one.
fn orphan_removal(baseline: &Path) -> Result<Removal> {
    match gitdir_state(baseline) {
        GitdirState::Absent => Ok(Removal::Orphan),
        GitdirState::Resolves => anyhow::bail!(
            "{} carries a baseline marker but this repository has no worktree registration \
             for it, and its git directory resolves; remove it from the repository that \
             owns it",
            baseline.display()
        ),
        GitdirState::Unknown => anyhow::bail!(
            "cannot tell whether {} belongs to another repository: its .git can be \
             neither read nor ruled out; refusing to remove it",
            baseline.display()
        ),
    }
}

/// What stands behind a directory's `.git`.
///
/// Three-valued for the reason [`devkit_common::worktree::BaselineState`] and
/// [`MarkerState`] are: only `Absent` reaches a deletion, so a two-valued
/// answer would have to fold every unanticipated read failure into one of the
/// other two, and folding it into "nothing here" deletes somebody's checkout.
enum GitdirState {
    /// A `.git` directory (a repository lives here), or a `.git` file naming a
    /// git directory that is there.
    Resolves,
    /// No `.git` at all, or one naming a git directory that is not there —
    /// which is what a baseline whose registration went looks like.
    Absent,
    /// The question could not be answered: `.git` could not be stat'd or read,
    /// it names a path this build cannot represent, or the named path could not
    /// be stat'd.
    Unknown,
}

/// Classify `dir`'s `.git`. Every failure that is not "the thing is not there"
/// is [`GitdirState::Unknown`]: the error set is open — permissions, a broken
/// mount, a file this process may unlink but not read — and no member of it
/// proves the directory is unowned.
fn gitdir_state(dir: &Path) -> GitdirState {
    let dot = dir.join(".git");
    let md = match std::fs::metadata(&dot) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return GitdirState::Absent,
        Err(_) => return GitdirState::Unknown,
    };
    if md.is_dir() {
        return GitdirState::Resolves;
    }
    let Ok(body) = std::fs::read(&dot) else {
        return GitdirState::Unknown;
    };
    let Some(named) = body
        .split(|b| *b == b'\n')
        .find_map(|line| line.strip_prefix(b"gitdir:"))
    else {
        // A `.git` file that names no git directory is not a pointer at
        // anything, whatever else it holds.
        return GitdirState::Absent;
    };
    // A path this build cannot spell is a path it cannot check.
    let Ok(named) = std::str::from_utf8(named) else {
        return GitdirState::Unknown;
    };
    // git strips only the trailing newline when it reads this file back, so
    // every other space belongs to the directory name. Rather than reproduce
    // git's parse on a path that decides a deletion, anything still carrying
    // whitespace at either end after the separator is left unproven.
    let named = named.trim_end_matches(['\r', '\n']);
    let named = named.strip_prefix(' ').unwrap_or(named);
    if named != named.trim() {
        return GitdirState::Unknown;
    }
    let named = Path::new(named);
    let target = if named.is_absolute() {
        named.to_path_buf()
    } else {
        dir.join(named)
    };
    // `Path::exists` folds a permission error into `false`, which is the whole
    // bug this function exists to avoid.
    match std::fs::symlink_metadata(&target) {
        Ok(_) => GitdirState::Resolves,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => GitdirState::Absent,
        Err(_) => GitdirState::Unknown,
    }
}

/// Whether the tree carries modified tracked files.
fn has_local_edits(baseline: &Path) -> Result<bool> {
    let out = Git::at(baseline)
        .args(["status", "--porcelain", "--untracked-files=no"])
        .timeout(devkit_common::git::SLOW_TIMEOUT)
        .output()
        .with_context(|| format!("reading the state of {}", baseline.display()))?;
    Ok(!out.trim().is_empty())
}

/// The body of [`drop_reference`], without the directory lock. A sweep already
/// holds that lock and calls this directly: `flock` blocks on a second open of
/// the same file even within one process, so a locked function must never call
/// another locked function.
///
/// `Ok(true)` means the baseline was removed, or under [`Sweep::Report`] that it
/// would have been; `Err` is a refusal either way.
fn remove_if_unreferenced(
    repo: &str,
    baseline: &Path,
    refs: &References,
    registered: &[devkit_common::git::Worktree],
    gates: Gates,
    ports: &registry::Data,
    sweep: Sweep,
) -> Result<bool> {
    let root = baseline.parent().context("baseline path has no parent")?;
    let name = slot_name(baseline)?;
    // Both modes resolve these, and before any lock. Handing `git worktree
    // remove --force` a path that lost its non-UTF-8 bytes would aim a
    // destructive command somewhere other than the baseline — and a dry run
    // that skipped the resolution would promise a removal the real sweep then
    // refuses on the same input.
    let path_s = baseline_path_str(baseline)?;
    // A dry run takes no lock. The wait for a slot lock is unbounded, so a
    // read-only pass would queue behind a bootstrap while holding the directory
    // lock, and it would leave a lock file behind for every slot it merely
    // looked at — lock files are never unlinked.
    if sweep == Sweep::Report {
        return Ok(decide(baseline, refs, registered, gates, ports)?.is_some());
    }
    locks::with_slot(root, &name, || {
        let Some(removal) = decide(baseline, refs, registered, gates, ports)? else {
            return Ok(false);
        };
        // The caller's scan predates this lock and `up` writes its pin after
        // `ensure` has released the same lock, so a baseline built while this
        // call waited has a referencer that scan cannot show. Re-read for the
        // tree actually about to go, rather than for every candidate.
        if !unreferenced(&referencers(repo)?, baseline) {
            return Ok(false);
        }
        match removal {
            Removal::Worktree => {
                // Always `--force`: a baseline holds include copies and rendered
                // prep files, and any untracked file would otherwise refuse the
                // removal.
                Git::at(Path::new(repo))
                    .args(["worktree", "remove", "--force", path_s])
                    .timeout(devkit_common::git::SLOW_TIMEOUT)
                    .output()?;
                Ok(true)
            }
            Removal::Orphan => {
                std::fs::remove_dir_all(baseline)
                    .with_context(|| format!("removing {}", baseline.display()))?;
                Ok(true)
            }
        }
    })
}

/// The directories under `baseline_dir` a sweep or a listing considers, sorted.
/// The lock directory sits among them and is not one, and a `baseline_dir` that
/// does not exist holds none.
fn slots_in(baseline_dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(baseline_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", baseline_dir.display()));
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("reading {}", baseline_dir.display()))?
            .path();
        if path.is_dir() && path.file_name().is_some_and(|n| n != locks::DIR) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// What a directory under `baseline_dir` is, as far as a listing can tell —
/// which is also what decides how (and whether) a sweep reclaims it.
pub enum SlotState {
    /// A marked tree git has a worktree registration for.
    Registered,
    /// Marked, but this repository has no registration for it: a sweep reclaims
    /// the directory itself.
    Orphaned,
    /// No marker, so devkit cannot prove it created this directory and a sweep
    /// leaves it where it is.
    Unmarked,
    /// A marker that can be neither read nor ruled out.
    Unreadable,
}

impl std::fmt::Display for SlotState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SlotState::Registered => "registered",
            SlotState::Orphaned => "orphaned",
            SlotState::Unmarked => "unmarked",
            SlotState::Unreadable => "unreadable",
        })
    }
}

/// One baseline directory as an operator sees it.
pub struct Listed {
    pub path: PathBuf,
    /// The fork point its marker names, absent when there is no marker to trust.
    pub sha: Option<String>,
    pub state: SlotState,
    pub referencers: Vec<PathBuf>,
    pub bytes: u64,
}

/// The baseline directory as an operator sees it.
pub struct Listing {
    pub baselines: Vec<Listed>,
    /// Why nothing here can be reclaimed at all, when some worktree's own pin
    /// cannot be read. Without it the referencer column reads as "free to
    /// prune" for every row while a sweep refuses all of them.
    pub note: Option<String>,
}

/// Enumerate via `read_dir` rather than `git worktree list`: a directory git no
/// longer knows about is exactly what an operator needs to see, and the git
/// list would render it invisible.
pub fn list(baseline_dir: &Path, repo: &str) -> Result<Listing> {
    let refs = referencers(repo)?;
    let registered = registrations(repo)?;
    let mut baselines = Vec::new();
    for path in slots_in(baseline_dir)? {
        let (sha, state) = match read_marker(&path) {
            MarkerState::Ok(m) => {
                let state = if registration(&registered, &path).is_some() {
                    SlotState::Registered
                } else {
                    SlotState::Orphaned
                };
                (Some(m.sha), state)
            }
            MarkerState::Unusable => (None, SlotState::Unreadable),
            MarkerState::Absent => (None, SlotState::Unmarked),
        };
        baselines.push(Listed {
            referencers: refs
                .naming(&path)
                .into_iter()
                .map(Path::to_path_buf)
                .collect(),
            bytes: dir_size(&path),
            path,
            sha,
            state,
        });
    }
    Ok(Listing {
        baselines,
        note: refs.unreadable_note(),
    })
}

/// Baselines nothing references, and the disk reclaiming them would free.
/// `list` sizes every slot with `dir_size` — a parallel walk of a full
/// repository checkout, prep files and all — because its table shows a size
/// column for every row; `doctor` only needs the reclaimable total, so this
/// sizes just the slots that qualify. The common case, no orphans, walks
/// nothing.
pub fn orphaned(baseline_dir: &Path, repo: &str) -> Result<(usize, u64)> {
    let refs = referencers(repo)?;
    let mut count = 0;
    let mut bytes = 0;
    for path in slots_in(baseline_dir)? {
        if !refs.naming(&path).is_empty() {
            continue;
        }
        count += 1;
        bytes += dir_size(&path);
    }
    Ok((count, bytes))
}

/// What a sweep took, what it deliberately left where it was, and what it
/// refused.
#[derive(Default)]
pub struct Pruned {
    pub removed: Vec<PathBuf>,
    pub reported: Vec<PathBuf>,
    /// Baselines this sweep could not take. Each one's reason is warned about
    /// as it happens; the list is what tells the caller the run was incomplete.
    pub refused: Vec<PathBuf>,
}

/// Remove every unreferenced baseline in one pass under a single directory
/// lock. Calls `remove_if_unreferenced` directly rather than `drop_reference`,
/// which takes that same lock: two open file descriptions on one lock file
/// block each other even inside one process.
///
/// A dry run puts every slot through the same gates on the same inputs and
/// stops before carrying the verdict out, so `removed` names what a real sweep
/// would take of the world as it stands, and the refusals it would print are
/// printed here too.
pub fn prune_all(
    baseline_dir: &Path,
    repo: &str,
    ports: &registry::Data,
    dry_run: bool,
    gates: Gates,
) -> Result<Pruned> {
    // Taking the directory lock would create a directory this project has never
    // needed, and there is nothing under it to sweep.
    if !baseline_dir.exists() {
        return Ok(Pruned::default());
    }
    let sweep = if dry_run {
        Sweep::Report
    } else {
        Sweep::Remove
    };
    locks::with_dir(baseline_dir, || {
        let refs = referencers(repo)?;
        // Both scans are taken once for the sweep, not once per candidate: each
        // is a git process, and they run under the directory lock `issue end`
        // waits on unbounded. Freshness where it matters is the slot lock's
        // job.
        let registered = registrations(repo)?;
        // Printed once for the sweep rather than once per slot: the note names
        // the worktrees to repair, and it is the same list for every baseline.
        if let Some(note) = refs.unreadable_note() {
            eprintln!("warning: {note}");
        }
        let mut removed = Vec::new();
        let mut reported = Vec::new();
        let mut refused = Vec::new();
        for path in slots_in(baseline_dir)? {
            // No marker means devkit cannot prove it created this tree, so it
            // is named and left alone rather than deleted.
            if matches!(read_marker(&path), MarkerState::Absent) {
                reported.push(path);
                continue;
            }
            match remove_if_unreferenced(repo, &path, &refs, &registered, gates, ports, sweep) {
                Ok(true) => removed.push(path),
                Ok(false) => {}
                // One stuck baseline must not abandon the sweep.
                Err(e) => {
                    eprintln!("warning: {}: {e:#}", path.display());
                    refused.push(path);
                }
            }
        }
        Ok(Pruned {
            removed,
            reported,
            refused,
        })
    })
}

/// Total bytes under `path`. Walked in parallel because a baseline holds a full
/// dependency tree; `jwalk_parallelism` is evaluated here, on the thread that
/// builds the walk, since inside `pool::install` it would see itself as nested
/// and silently go serial.
///
/// jwalk's `skip_hidden` default is true and would fail quietly here: a
/// baseline's `.venv`, `.next`, `.turbo` and its own `.devkit` are dotfiles, so
/// the default reports a fraction of the tree, or nothing at all. Symlinks stay
/// unfollowed so a link into a tree already counted is not counted twice.
fn dir_size(path: &Path) -> u64 {
    let parallelism = devkit_common::pool::jwalk_parallelism();
    jwalk::WalkDir::new(path)
        .skip_hidden(false)
        .parallelism(parallelism)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Point a worktree's record at a baseline, leaving its other fields alone.
///
/// Writes a record when there is none. A worktree made by hand rather than by
/// `issue setup` still holds a reference, and skipping the write there would
/// let prune reclaim a baseline that worktree is serving from.
pub fn write_pin(worktree: &Path, sha: &str, path: &Path) -> Result<()> {
    let mut rec = match devkit_common::record::read(worktree) {
        Some(rec) => rec,
        None => {
            let branch = devkit_common::git::branch(worktree)?;
            devkit_common::record::IssueRecord {
                issue: branch.clone(),
                slug: branch,
                apps: vec![],
                summary: None,
                pr: None,
                baseline: None,
            }
        }
    };
    rec.baseline = Some(devkit_common::record::BaselinePin {
        sha: sha.to_string(),
        path: path.to_string_lossy().into_owned(),
    });
    devkit_common::record::write(worktree, &rec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(prep: Vec<devkit_config::PrepFile>, setup: Vec<Vec<String>>) -> App {
        App {
            name: "api".into(),
            base_port: 3000,
            path: "apps/api".into(),
            launch: vec!["run".into()],
            url: None,
            url_env: None,
            provides_url: false,
            static_env: Default::default(),
            prep_files: prep,
            setup,
        }
    }

    fn prep(path: &str, content: &str) -> devkit_config::PrepFile {
        devkit_config::PrepFile {
            path: path.into(),
            content: content.into(),
            overwrite: false,
        }
    }

    /// The FNV-1a offset basis, which an app with nothing to hash must produce.
    /// This pins the algorithm: a fingerprint is stored in the marker and compared
    /// on a later run, possibly under a different toolchain, so a hash that shifts
    /// between Rust releases would either re-prep every baseline forever or stop
    /// noticing a real change.
    #[test]
    fn an_app_with_nothing_to_hash_is_the_fnv_offset_basis() {
        assert_eq!(
            fingerprint(&app_with(vec![], vec![]), &[]),
            "cbf29ce484222325"
        );
    }

    #[test]
    fn a_fingerprint_moves_when_prep_content_changes() {
        let before = fingerprint(&app_with(vec![prep(".env", "A=1")], vec![]), &[]);
        let after = fingerprint(&app_with(vec![prep(".env", "A=2")], vec![]), &[]);
        assert_ne!(before, after);
    }

    #[test]
    fn a_fingerprint_moves_when_includes_change() {
        let app = app_with(vec![], vec![]);
        assert_ne!(
            fingerprint(&app, &[".env".into()]),
            fingerprint(&app, &[".env.local".into()])
        );
    }

    /// Without a separator between fields, `["ab", "c"]` and `["a", "bc"]` hash
    /// identically and a changed setup command goes unnoticed.
    #[test]
    fn field_boundaries_are_part_of_the_hash() {
        let a = app_with(vec![], vec![vec!["ab".into(), "c".into()]]);
        let b = app_with(vec![], vec![vec!["a".into(), "bc".into()]]);
        assert_ne!(fingerprint(&a, &[]), fingerprint(&b, &[]));
    }

    #[test]
    fn the_synthetic_identity_is_stable_per_sha() {
        let ctx = bootstrap_context("d13d90b724bf8a3c", &["api".to_string()], "lev/");
        assert_eq!(ctx["prefix"], "lev/");
        assert_eq!(ctx["issue"], "baseline-d13d90b724bf");
        assert_eq!(ctx["slug"], "baseline-d13d90b724bf");
        assert_eq!(ctx["branch"], "baseline-d13d90b724bf");
        assert_eq!(ctx["role"], "baseline");
        assert_eq!(ctx["sha"], "d13d90b724bf8a3c");
    }

    /// A `prep_files` template naming `{{ issue }}` must render inside a baseline
    /// rather than hard-failing: `template::render` is strict and `prep_apps`
    /// propagates a render error with `?`.
    #[test]
    fn a_prep_template_naming_the_issue_renders_in_a_baseline() {
        let ctx = bootstrap_context("d13d90b724bf8a3c", &["api".to_string()], "lev/");
        let got = devkit_common::template::render(
            "ISSUE={{ issue }} ROLE={{ role }}",
            &ctx,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(got, "ISSUE=baseline-d13d90b724bf ROLE=baseline");
    }

    fn fixture_git(cwd: &Path, args: &[&str]) -> String {
        devkit_common::git::Git::fixture(cwd)
            .args(args.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
    }

    /// A primary checkout with one commit, and the sha of that commit.
    fn primary_with_one_commit(at: &Path) -> String {
        std::fs::create_dir_all(at).unwrap();
        fixture_git(at, &["init", "-q", "-b", "main"]);
        std::fs::write(at.join("f"), "x").unwrap();
        fixture_git(at, &["add", "."]);
        fixture_git(at, &["commit", "-qm", "one"]);
        fixture_git(at, &["rev-parse", "HEAD"]).trim().to_string()
    }

    fn cfg_rooted_at(baseline_dir: &Path) -> Config {
        let mut cfg = Config::default();
        cfg.defaults.baseline_dir = baseline_dir.to_str().unwrap().to_string();
        cfg
    }

    #[test]
    fn an_unset_baseline_dir_names_the_two_keys_that_set_it() {
        let err = ensure(
            &Config::default(),
            &HashMap::new(),
            Path::new("/nonexistent"),
            SHA,
            &[],
            &Steps::persistent(),
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("worktree_root"), "{msg}");
        assert!(msg.contains("baseline_dir"), "{msg}");
    }

    #[test]
    fn a_bootstrap_creates_a_detached_worktree_and_reuses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("repo");
        let sha = primary_with_one_commit(&primary);
        let root = tmp.path().join("baselines");
        let cfg = cfg_rooted_at(&root);
        let catalog = HashMap::new();

        let path = ensure(&cfg, &catalog, &primary, &sha, &[], &Steps::persistent()).unwrap();
        assert_eq!(path, root.join(short(&sha)));
        assert!(matches!(read_marker(&path), MarkerState::Ok(m) if m.sha == sha));
        assert!(
            !devkit_common::git::Git::fixture(&path)
                .args(["symbolic-ref", "-q", "HEAD"])
                .success()
                .unwrap(),
            "a baseline must occupy no branch name"
        );

        let again = ensure(&cfg, &catalog, &primary, &sha, &[], &Steps::persistent()).unwrap();
        assert_eq!(again, path, "a complete baseline is reused, not rebuilt");
    }

    /// A `prep_files` entry naming `{{ prefix }}` must render inside a baseline
    /// bootstrap rather than hard-failing it: `template::render` is strict, and
    /// `prefix` is a documented `prep_files` context key.
    #[test]
    fn a_prep_template_naming_the_prefix_renders_in_a_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("repo");
        let sha = primary_with_one_commit(&primary);
        let root = tmp.path().join("baselines");
        let mut cfg = cfg_rooted_at(&root);
        cfg.defaults.branch_prefix = "lev/".into();
        let catalog = HashMap::from([(
            "api".to_string(),
            app_with(vec![prep(".env", "PREFIX={{ prefix }}")], vec![]),
        )]);

        let path = ensure(
            &cfg,
            &catalog,
            &primary,
            &sha,
            &["api".to_string()],
            &Steps::persistent(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(path.join("apps/api/.env")).unwrap(),
            "PREFIX=lev/"
        );
    }

    /// `after_worktree_create` is documented to render over `worktree`, and its
    /// shipped default (`zoxide add {{ worktree }}`) depends on it. `git init`
    /// stands in for that default: it renders `{{ worktree }}` and leaves a
    /// directory behind only if the argument rendered.
    #[test]
    fn the_after_worktree_create_hook_sees_the_baseline_directory_as_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("repo");
        let sha = primary_with_one_commit(&primary);
        let root = tmp.path().join("baselines");
        let mut cfg = cfg_rooted_at(&root);
        cfg.hooks.after_worktree_create = vec![vec![
            "git".to_string(),
            "init".to_string(),
            "{{ worktree }}/artifact".to_string(),
        ]];

        let path = ensure(
            &cfg,
            &HashMap::new(),
            &primary,
            &sha,
            &[],
            &Steps::persistent(),
        )
        .unwrap();

        assert!(
            path.join("artifact").exists(),
            "the hook never received `worktree`"
        );
    }

    /// An interrupted bootstrap leaves a tree with no marker. The next run owns
    /// that path: it replaces the tree rather than moving the baseline to a
    /// collision slot and stranding the remains.
    #[test]
    fn a_markerless_tree_is_replaced_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("repo");
        let sha = primary_with_one_commit(&primary);
        let root = tmp.path().join("baselines");
        let stale = root.join(short(&sha));
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("leftover"), "x").unwrap();

        let path = ensure(
            &cfg_rooted_at(&root),
            &HashMap::new(),
            &primary,
            &sha,
            &[],
            &Steps::persistent(),
        )
        .unwrap();
        assert_eq!(path, stale);
        assert!(!path.join("leftover").exists(), "the stale tree survived");
        assert!(matches!(read_marker(&path), MarkerState::Ok(_)));
    }

    /// The marker is written after every prep step, so a bootstrap that fails
    /// partway can never be mistaken for a finished baseline.
    #[test]
    fn a_failed_bootstrap_leaves_nothing_to_mistake_for_a_baseline() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("repo");
        let sha = primary_with_one_commit(&primary);
        let root = tmp.path().join("baselines");
        let catalog = HashMap::from([(
            "api".to_string(),
            app_with(vec![], vec![vec!["devkit-no-such-program-xyz".into()]]),
        )]);

        let err = ensure(
            &cfg_rooted_at(&root),
            &catalog,
            &primary,
            &sha,
            &["api".to_string()],
            &Steps::persistent(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("api"), "{err:#}");
        assert!(
            !root.join(short(&sha)).exists(),
            "a failed bootstrap left a tree behind"
        );
    }

    /// An empty sha resolves its slot to the baseline root itself, where a
    /// rebuild would remove every other baseline along with it.
    #[test]
    fn an_empty_sha_is_refused_before_any_slot_is_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("baselines");
        let sibling = root.join("d13d90b724bf");
        std::fs::create_dir_all(&sibling).unwrap();

        assert!(
            ensure(
                &cfg_rooted_at(&root),
                &HashMap::new(),
                tmp.path(),
                "",
                &[],
                &Steps::persistent(),
            )
            .is_err()
        );
        assert!(sibling.exists(), "an unrelated baseline was removed");
    }

    /// A tree this run did not build is left standing when prep fails: its
    /// marker still describes what is prepped, and other worktrees — and any
    /// server running out of it — depend on the directory.
    #[test]
    fn a_reused_baseline_survives_a_failed_re_prep() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("repo");
        let sha = primary_with_one_commit(&primary);
        let root = tmp.path().join("baselines");
        let cfg = cfg_rooted_at(&root);
        let apps = ["api".to_string()];
        let catalog_of = |app: App| HashMap::from([("api".to_string(), app)]);

        let path = ensure(
            &cfg,
            &catalog_of(app_with(vec![prep(".env", "A=1")], vec![])),
            &primary,
            &sha,
            &apps,
            &Steps::persistent(),
        )
        .unwrap();
        let before = match read_marker(&path) {
            MarkerState::Ok(m) => m,
            _ => panic!("the first bootstrap must leave a readable marker"),
        };

        // Drifted prep, so the app is stale, plus a setup command that cannot run.
        let broken = app_with(
            vec![prep(".env", "A=2")],
            vec![vec!["devkit-no-such-program-xyz".into()]],
        );
        assert!(
            ensure(
                &cfg,
                &catalog_of(broken),
                &primary,
                &sha,
                &apps,
                &Steps::persistent()
            )
            .is_err()
        );
        assert!(path.exists(), "the reused baseline was removed");
        assert!(
            matches!(read_marker(&path), MarkerState::Ok(m) if m == before),
            "a failed re-prep must not claim the new fingerprint"
        );
    }

    /// The fingerprint is what makes reuse safe: an app whose prep drifted is
    /// prepped again, and one whose prep is unchanged is left alone.
    #[test]
    fn only_an_app_whose_fingerprint_moved_is_prepped_again() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("repo");
        let sha = primary_with_one_commit(&primary);
        let root = tmp.path().join("baselines");
        let cfg = cfg_rooted_at(&root);
        let apps = ["api".to_string()];
        let overwriting = |content: &str| devkit_config::PrepFile {
            path: ".env".into(),
            content: content.into(),
            overwrite: true,
        };
        let catalog_with = |content: &str| {
            HashMap::from([(
                "api".to_string(),
                app_with(vec![overwriting(content)], vec![]),
            )])
        };

        let path = ensure(
            &cfg,
            &catalog_with("A=1"),
            &primary,
            &sha,
            &apps,
            &Steps::persistent(),
        )
        .unwrap();
        let env = path.join("apps/api/.env");
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "A=1");

        // An edit that only a re-prep would undo: an unchanged fingerprint must
        // leave it standing.
        std::fs::write(&env, "EDITED").unwrap();
        ensure(
            &cfg,
            &catalog_with("A=1"),
            &primary,
            &sha,
            &apps,
            &Steps::persistent(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "EDITED");

        ensure(
            &cfg,
            &catalog_with("A=2"),
            &primary,
            &sha,
            &apps,
            &Steps::persistent(),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&env).unwrap(), "A=2");
    }

    #[test]
    fn a_marker_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut apps = std::collections::BTreeMap::new();
        apps.insert(
            "api".to_string(),
            AppMark {
                fingerprint: "9f2c".into(),
            },
        );
        let m = Marker {
            sha: "d13d90b724bf".into(),
            apps,
        };
        write_marker(dir.path(), &m).unwrap();
        assert!(matches!(read_marker(dir.path()), MarkerState::Ok(got) if got == m));
    }

    #[test]
    fn an_absent_marker_is_absent_and_a_corrupt_one_is_unusable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(read_marker(dir.path()), MarkerState::Absent));
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            dir.path().join(devkit_common::worktree::BASELINE_MARKER),
            "sha = ",
        )
        .unwrap();
        assert!(matches!(read_marker(dir.path()), MarkerState::Unusable));
    }

    /// A non-`NotFound` read error (here, the marker path occupied by a
    /// directory instead of a file) must not read as `Absent` — that would
    /// tell a caller a live baseline it cannot read is a clean slate to
    /// build fresh over.
    #[test]
    fn an_unreadable_marker_is_unusable_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join(devkit_common::worktree::BASELINE_MARKER);
        std::fs::create_dir_all(&marker_path).unwrap();

        let err = std::fs::read_to_string(&marker_path).unwrap_err();
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "test assumes reading a directory as a file yields a non-NotFound error on this platform"
        );

        assert!(matches!(read_marker(dir.path()), MarkerState::Unusable));
    }

    const SHA: &str = "d13d90b724bf8a3c0000000000000000000000ab";
    const OTHER: &str = "0123456789ab0000000000000000000000000000";

    fn place(root: &std::path::Path, name: &str, sha: &str) {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        write_marker(
            &d,
            &Marker {
                sha: sha.into(),
                apps: Default::default(),
            },
        )
        .unwrap();
    }

    #[test]
    fn an_empty_dir_creates_at_the_short_sha() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            matches!(slot(root.path(), SHA), Slot::Create(p) if p == root.path().join("d13d90b724bf"))
        );
    }

    #[test]
    fn a_matching_marker_is_reused() {
        let root = tempfile::tempdir().unwrap();
        place(root.path(), "d13d90b724bf", SHA);
        assert!(matches!(slot(root.path(), SHA), Slot::Reuse(..)));
    }

    #[test]
    fn a_colliding_marker_moves_to_the_next_candidate() {
        let root = tempfile::tempdir().unwrap();
        place(root.path(), "d13d90b724bf", OTHER);
        assert!(matches!(slot(root.path(), SHA), Slot::Create(p) if p.ends_with("d13d90b724bf_2")));
    }

    #[test]
    fn a_markerless_directory_is_rebuilt_in_place() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("d13d90b724bf")).unwrap();
        assert!(matches!(slot(root.path(), SHA), Slot::Rebuild(p) if p.ends_with("d13d90b724bf")));
    }

    #[test]
    fn a_corrupt_marker_is_rebuilt_in_place() {
        let root = tempfile::tempdir().unwrap();
        let d = root.path().join("d13d90b724bf");
        std::fs::create_dir_all(d.join(".devkit")).unwrap();
        std::fs::write(d.join(devkit_common::worktree::BASELINE_MARKER), "sha = ").unwrap();
        assert!(matches!(slot(root.path(), SHA), Slot::Rebuild(_)));
    }

    /// Every candidate up to the bound collides with a genuinely different
    /// sha (not an empty or corrupt marker), so the only way out is the
    /// bound itself — proving the walk terminates instead of looping forever.
    #[test]
    fn exhausting_the_candidate_bound_reports_instead_of_looping() {
        let root = tempfile::tempdir().unwrap();
        for n in 1..=MAX_SLOT_CANDIDATES {
            let name = if n == 1 {
                "d13d90b724bf".to_string()
            } else {
                format!("d13d90b724bf_{n}")
            };
            let other = format!("d13d90b724bf{n:028x}");
            assert_ne!(other, SHA, "constructed collision must not equal SHA");
            place(root.path(), &name, &other);
        }
        assert!(matches!(slot(root.path(), SHA), Slot::Exhausted(_)));
    }

    #[test]
    fn short_truncates_to_twelve_chars() {
        assert_eq!(short(SHA), "d13d90b724bf");
    }

    #[test]
    fn short_of_a_short_input_returns_it_whole() {
        assert_eq!(short("abcd"), "abcd");
    }

    #[test]
    fn short_of_an_empty_input_returns_empty() {
        assert_eq!(short(""), "");
    }

    /// `short` must not panic when the 12th byte would split a multi-byte
    /// UTF-8 character — real shas are hex ASCII, but the function takes any
    /// `&str` and a caller could pass something else.
    #[test]
    fn short_does_not_panic_on_a_multibyte_boundary() {
        let s = "1234567890€23";
        let _ = short(s);
    }

    #[test]
    fn a_configured_ref_wins_over_detection() {
        let mut cfg = devkit_config::Config::default();
        cfg.defaults.baseline_ref = "origin/release".into();
        // Detection would fail in a non-repo path; the configured ref means it is
        // never consulted.
        let got = target(&cfg, std::path::Path::new("/nonexistent")).unwrap();
        assert_eq!(got, "origin/release");
    }

    #[test]
    fn an_undetectable_target_names_both_fixes() {
        let tmp = tempfile::tempdir().unwrap();
        devkit_common::git::Git::fixture(tmp.path())
            .args(["init", "-q"])
            .output()
            .unwrap();
        let err = target(&devkit_config::Config::default(), tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("baseline_ref"), "{msg}");
        assert!(msg.contains("git remote set-head"), "{msg}");
    }

    /// Two commits on main, a branch cut from the first, then main advances:
    /// the merge base stays at the fork point.
    #[test]
    fn the_pin_is_the_fork_point_not_the_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        let fork = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-qb", "feat"]);
        std::fs::write(p.join("b"), "2").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "two"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(p.join("c"), "3").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "three"]);
        git(&["checkout", "-q", "feat"]);

        let got = pin(p, "main").unwrap();
        assert_eq!(got, fork);
        assert_eq!(got.len(), 40);
    }

    /// Rebasing `feat` onto a `main` that has advanced moves the merge base to
    /// `main`'s new tip: this is what re-resolving after a rebase relies on.
    #[test]
    fn a_rebase_moves_the_pin_to_the_new_tip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        let first_fork = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-qb", "feat"]);
        std::fs::write(p.join("b"), "2").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "two"]);
        git(&["checkout", "-q", "main"]);
        std::fs::write(p.join("c"), "3").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "three"]);
        let new_tip = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["checkout", "-q", "feat"]);

        assert_eq!(pin(p, "main").unwrap(), first_fork);

        git(&["rebase", "main"]);

        let rebased = pin(p, "main").unwrap();
        assert_eq!(rebased, new_tip);
        assert_ne!(rebased, first_fork);
    }

    #[test]
    fn unrelated_histories_error_naming_both_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path();
        let git = |args: &[&str]| {
            devkit_common::git::Git::fixture(p)
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(p.join("a"), "1").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "one"]);
        git(&["checkout", "-q", "--orphan", "lonely"]);
        git(&["commit", "-qm", "orphan", "--allow-empty"]);

        let err = pin(p, "main").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("HEAD"), "{msg}");
        assert!(msg.contains("main"), "{msg}");
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        repo: String,
        baseline: PathBuf,
        a: PathBuf,
        b: PathBuf,
    }

    /// One commit, one baseline at it, two issue worktrees whose records both
    /// name that baseline.
    fn two_worktrees_sharing_one_baseline() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("main");
        let sha = primary_with_one_commit(&repo);

        let baseline = tmp.path().join("_baselines").join(short(&sha));
        std::fs::create_dir_all(baseline.parent().unwrap()).unwrap();
        fixture_git(
            &repo,
            &["worktree", "add", "--detach", baseline.to_str().unwrap()],
        );
        write_marker(&baseline, &fresh_marker(&sha)).unwrap();

        let make = |name: &str| {
            let wt = tmp.path().join(name);
            fixture_git(
                &repo,
                &["worktree", "add", "-b", name, wt.to_str().unwrap()],
            );
            devkit_common::record::write(
                &wt,
                &devkit_common::record::IssueRecord {
                    issue: name.to_string(),
                    slug: name.to_string(),
                    apps: vec![],
                    summary: None,
                    pr: None,
                    baseline: Some(devkit_common::record::BaselinePin {
                        sha: sha.clone(),
                        path: baseline.to_string_lossy().into_owned(),
                    }),
                },
            )
            .unwrap();
            wt
        };
        let a = make("a");
        let b = make("b");
        Fixture {
            _tmp: tmp,
            repo: repo.to_string_lossy().into_owned(),
            baseline,
            a,
            b,
        }
    }

    fn remove_worktree(repo: &str, wt: &Path) {
        fixture_git(
            Path::new(repo),
            &["worktree", "remove", "--force", wt.to_str().unwrap()],
        );
    }

    /// Leave a directory's baseline marker unresolvable — a symlink loop here,
    /// a permission failure in the field — so it can be neither read nor ruled
    /// out.
    #[cfg(unix)]
    fn make_unclassifiable(dir: &Path) {
        let marker = dir.join(BASELINE_MARKER);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&marker);
        std::os::unix::fs::symlink(&marker, &marker).unwrap();
    }

    fn corrupt_record(wt: &Path) {
        std::fs::write(wt.join(".devkit").join("issue.toml"), "issue = ").unwrap();
    }

    /// A worktree whose baseline marker cannot be resolved is filtered out of
    /// the listings with the baselines, so its record is never read. Counting
    /// it as unreadable is what keeps a reference nobody can see from being
    /// counted as no reference at all.
    #[cfg(unix)]
    #[test]
    fn a_worktree_that_cannot_be_classified_is_unreadable() {
        let f = two_worktrees_sharing_one_baseline();
        make_unclassifiable(&f.b);

        let refs = referencers(&f.repo).unwrap();
        assert!(
            refs.unreadable
                .iter()
                .any(|p| devkit_common::git::same_path(p, &f.b)),
            "{:?}",
            refs.unreadable
        );
        assert!(
            !drop_reference(&f.repo, &f.baseline, &registry::Data::default(), false).unwrap(),
            "a baseline was reclaimed on a scan that could not read every worktree"
        );
    }

    /// Repin `worktree` at `baseline` spelled through a symlinked parent: the
    /// same directory, a different key in the scan.
    #[cfg(unix)]
    fn pin_through_a_link(tmp: &Path, worktree: &Path, baseline: &Path) -> PathBuf {
        let link = tmp.join("link");
        if !link.exists() {
            std::os::unix::fs::symlink(tmp, &link).unwrap();
        }
        let spelled = link.join(baseline.strip_prefix(tmp).unwrap());
        let mut rec = devkit_common::record::read(worktree).unwrap();
        rec.baseline = Some(devkit_common::record::BaselinePin {
            sha: "d13d90b724bf8a3c".into(),
            path: spelled.to_string_lossy().into_owned(),
        });
        devkit_common::record::write(worktree, &rec).unwrap();
        spelled
    }

    /// The removal target and the records that reference it are spelled by
    /// whatever resolved each of them, so one directory can occupy two keys in
    /// the scan. Looked up by exact key, a reference spelled another way reads
    /// as no reference, and `git worktree remove --force` runs on a baseline a
    /// live worktree is still using.
    #[cfg(unix)]
    #[test]
    fn a_reference_spelled_another_way_still_holds_the_baseline() {
        let f = two_worktrees_sharing_one_baseline();
        let tmp = f.baseline.parent().unwrap().parent().unwrap().to_path_buf();
        remove_worktree(&f.repo, &f.a);
        let spelled = pin_through_a_link(&tmp, &f.b, &f.baseline);
        assert_ne!(spelled, f.baseline, "the fixture must spell it differently");

        assert!(
            !drop_reference(&f.repo, &f.baseline, &registry::Data::default(), false).unwrap(),
            "removed a baseline b still references"
        );
        assert!(f.baseline.exists(), "the tree is gone");
    }

    #[test]
    fn a_corrupt_record_counts_as_a_referencer() {
        let f = two_worktrees_sharing_one_baseline();
        corrupt_record(&f.b);
        remove_worktree(&f.repo, &f.a);
        let ports = registry::Data::default();
        assert!(!drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
        assert!(f.baseline.exists(), "cannot-tell must not delete");
    }

    /// The refusal is silent to the user unless it says which record it could
    /// not read: one corrupt `.devkit/issue.toml` otherwise stops every
    /// baseline from ever being reclaimed with nothing to point at.
    #[test]
    fn the_refusal_names_the_worktrees_it_could_not_read() {
        let refs = References {
            by_baseline: BTreeMap::new(),
            unreadable: vec![PathBuf::from("/w/a"), PathBuf::from("/w/b")],
        };
        let note = refs
            .unreadable_note()
            .expect("a note for unreadable records");
        assert!(note.contains("/w/a"), "{note}");
        assert!(note.contains("/w/b"), "{note}");
        assert!(
            References {
                by_baseline: BTreeMap::new(),
                unreadable: vec![],
            }
            .unreadable_note()
            .is_none()
        );
    }

    #[test]
    fn the_last_referencer_removes_the_baseline() {
        let f = two_worktrees_sharing_one_baseline();
        let ports = registry::Data::default();
        remove_worktree(&f.repo, &f.a);
        assert!(!drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
        assert!(f.baseline.exists());
        remove_worktree(&f.repo, &f.b);
        assert!(drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
        assert!(!f.baseline.exists());
    }

    /// Two callers that both find themselves holding the last reference must
    /// produce one remover and no leak. The barrier forces the interleaving
    /// worth testing: both worktrees are gone before either counts, so both see
    /// zero referencers and both try to remove. Without it the threads can
    /// serialize, one declining because the other's worktree still stands,
    /// which never exercises the contention.
    #[test]
    fn two_concurrent_ends_leave_no_baseline_behind() {
        let f = two_worktrees_sharing_one_baseline();
        let gate = std::sync::Barrier::new(2);
        let removed: Vec<bool> = std::thread::scope(|s| {
            let handles: Vec<_> = [f.a.clone(), f.b.clone()]
                .into_iter()
                .map(|wt| {
                    let repo = f.repo.clone();
                    let baseline = f.baseline.clone();
                    let gate = &gate;
                    s.spawn(move || {
                        remove_worktree(&repo, &wt);
                        gate.wait();
                        let ports = registry::Data::default();
                        drop_reference(&repo, &baseline, &ports, false).unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(
            removed.iter().filter(|r| **r).count(),
            1,
            "exactly one remover"
        );
        assert!(!f.baseline.exists(), "baseline leaked");
    }

    /// The removal target comes from a record inside the worktree being
    /// removed, so a wrong or hand-edited pin aims `git worktree remove
    /// --force` at whatever it names. A sibling issue worktree of the same
    /// repository is a valid removal target for git and would go, uncommitted
    /// work included, so identity is checked before anything is deleted.
    #[test]
    fn a_pin_aimed_at_a_sibling_worktree_is_refused() {
        let f = two_worktrees_sharing_one_baseline();
        std::fs::write(f.b.join("f"), "uncommitted work").unwrap();
        remove_worktree(&f.repo, &f.a);
        let ports = registry::Data::default();

        let err = drop_reference(&f.repo, &f.b, &ports, false).unwrap_err();
        assert!(format!("{err:#}").contains("not a baseline"), "{err:#}");
        assert!(f.b.exists(), "a sibling worktree must survive");
        assert_eq!(
            std::fs::read_to_string(f.b.join("f")).unwrap(),
            "uncommitted work"
        );
    }

    /// The identity check decides whether `git worktree remove --force` runs,
    /// so a marker that can be neither read nor ruled out must refuse: the tree
    /// goes with its uncommitted work either way. The refusal names the
    /// classification failure, because "this is somebody else's tree" and "this
    /// tree cannot be read" send an operator to different places.
    #[cfg(unix)]
    #[test]
    fn a_baseline_that_cannot_be_classified_is_not_removed() {
        let f = two_worktrees_sharing_one_baseline();
        remove_worktree(&f.repo, &f.a);
        remove_worktree(&f.repo, &f.b);
        make_unclassifiable(&f.baseline);
        // Built by hand, empty: the removal must be refused by the identity
        // check rather than by a scan that already knows it cannot prove
        // anything. A real scan cannot supply that state — a tree it cannot
        // classify is one of the worktrees it reports as unreadable — so the
        // decision is driven directly.
        let refs = References {
            by_baseline: BTreeMap::new(),
            unreadable: vec![],
        };

        let err = decide(
            &f.baseline,
            &refs,
            &registrations(&f.repo).unwrap(),
            Gates::sweep(true, true),
            &registry::Data::default(),
        )
        .unwrap_err();

        let msg = format!("{err:#}");
        assert!(msg.contains("cannot tell"), "{msg}");
        assert!(msg.contains(BASELINE_MARKER), "{msg}");
        assert!(
            !msg.contains("is not a baseline"),
            "an unreadable marker is not the same as a missing one: {msg}"
        );
        assert!(
            f.baseline.exists(),
            "force removed a tree it could not read"
        );
    }

    /// Whether the pin names a baseline decides before anything else runs, so
    /// an unclassifiable one is left alone rather than carried into the scan.
    /// The unreadable repository is what makes that ordering observable: a run
    /// that got as far as counting referencers would fail on it.
    #[cfg(unix)]
    #[test]
    fn an_unclassifiable_pin_target_keeps_its_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let previous = tmp.path().join("_baselines").join("d13d90b724bf");
        make_unclassifiable(&previous);
        let mut stopped = false;

        release_abandoned(
            tmp.path().join("not-a-repository").to_str().unwrap(),
            &tmp.path().join("wt"),
            &previous,
            |_| {
                stopped = true;
                Ok(())
            },
        )
        .unwrap();

        assert!(
            !stopped,
            "servers stopped under a path this run cannot read"
        );
    }

    /// A relative path locks under whatever directory the process was started
    /// in while `git worktree remove` resolves it against the repository, so
    /// the lock guards neither the caller nor the tree. Config resolves
    /// `baseline_dir` to an absolute path, which leaves a hand-edited record as
    /// the only source of one.
    #[test]
    fn a_relative_pin_path_is_refused_before_any_lock() {
        let f = two_worktrees_sharing_one_baseline();
        let ports = registry::Data::default();

        let err = drop_reference(&f.repo, Path::new("_baselines"), &ports, false).unwrap_err();
        assert!(format!("{err:#}").contains("absolute"), "{err:#}");
        assert!(
            !Path::new(".locks").exists(),
            "a lock file was created under the process working directory"
        );
    }

    /// The primary checkout is a worktree like any other and can name a
    /// baseline in its own record — `devrun up --role baseline` run from there
    /// writes exactly that. A scan that skipped it would let a sibling
    /// `issue end` reclaim a baseline still in use.
    #[test]
    fn a_record_in_the_primary_checkout_counts_as_a_referencer() {
        let f = two_worktrees_sharing_one_baseline();
        let repo = PathBuf::from(&f.repo);
        write_pin(&repo, "d13d90b724bf8a3c", &f.baseline).unwrap();
        remove_worktree(&f.repo, &f.a);
        remove_worktree(&f.repo, &f.b);
        let ports = registry::Data::default();

        assert_eq!(
            referencers(&f.repo).unwrap().by_baseline[&f.baseline].len(),
            1,
            "the primary checkout's record is the one remaining reference"
        );
        assert!(!drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
        assert!(f.baseline.exists(), "a referenced baseline was removed");
    }

    #[test]
    fn a_live_row_refuses_without_force() {
        let f = two_worktrees_sharing_one_baseline();
        remove_worktree(&f.repo, &f.a);
        remove_worktree(&f.repo, &f.b);
        let mut ports = registry::Data::default();
        ports.entries.insert(
            3000,
            registry::Entry {
                app: "api".into(),
                holder: f.baseline.to_string_lossy().into_owned(),
                role: registry::Role::Baseline,
                pid: Some(std::process::id()),
                logfile: None,
                ts: registry::now(),
            },
        );
        let err = drop_reference(&f.repo, &f.baseline, &ports, false).unwrap_err();
        assert!(format!("{err:#}").contains("running servers"), "{err:#}");
        assert!(
            drop_reference(&f.repo, &f.baseline, &ports, true).unwrap(),
            "force removes"
        );
    }

    /// A worktree made by hand has no record. It still references the baseline
    /// it runs against, so the pin creates the record rather than declining.
    #[test]
    fn pinning_a_worktree_with_no_record_creates_one() {
        let f = two_worktrees_sharing_one_baseline();
        let bare = f.baseline.parent().unwrap().parent().unwrap().join("c");
        fixture_git(
            Path::new(&f.repo),
            &["worktree", "add", "-b", "c", bare.to_str().unwrap()],
        );
        assert!(
            devkit_common::record::read(&bare).is_none(),
            "fixture must start recordless"
        );

        write_pin(&bare, "d13d90b724bf8a3c", &f.baseline).unwrap();
        let rec = devkit_common::record::read(&bare).unwrap();
        assert_eq!(rec.baseline.unwrap().path, f.baseline.to_string_lossy());
        assert_eq!(rec.issue, "c", "identity falls back to the branch");

        let refs = referencers(&f.repo).unwrap();
        assert!(
            refs.by_baseline[&f.baseline].contains(&bare),
            "the new record counts"
        );
    }

    /// A baseline holds rendered prep files and include copies that git does
    /// not track, so a plain `worktree remove` would refuse over them.
    #[test]
    fn untracked_prep_output_does_not_block_removal() {
        let f = two_worktrees_sharing_one_baseline();
        std::fs::write(f.baseline.join(".env.local"), "A=1").unwrap();
        remove_worktree(&f.repo, &f.a);
        remove_worktree(&f.repo, &f.b);
        let ports = registry::Data::default();
        assert!(drop_reference(&f.repo, &f.baseline, &ports, false).unwrap());
    }

    /// `git worktree remove --force` deletes whatever path it is handed, so a
    /// path that cannot round-trip through `&str` must stop the call rather
    /// than reach it as something else — and it must stop a dry run too, or the
    /// sweep reports a removal it then refuses on the same input.
    #[test]
    #[cfg(unix)]
    fn a_non_utf8_baseline_path_is_refused_before_the_removal() {
        use std::os::unix::ffi::OsStrExt;

        let f = two_worktrees_sharing_one_baseline();
        let root = f.baseline.parent().unwrap();
        let odd = root.join(std::ffi::OsStr::from_bytes(b"base\xffline"));
        std::fs::create_dir_all(&odd).unwrap();
        let ports = registry::Data::default();

        let err = drop_reference(&f.repo, &odd, &ports, false).unwrap_err();
        assert!(format!("{err:#}").contains("not UTF-8"), "{err:#}");
        assert!(odd.exists(), "the refused path must survive");

        for sweep in [Sweep::Report, Sweep::Remove] {
            let err = remove_if_unreferenced(
                &f.repo,
                &odd,
                &referencers(&f.repo).unwrap(),
                &registrations(&f.repo).unwrap(),
                Gates::sweep(true, true),
                &ports,
                sweep,
            )
            .unwrap_err();
            assert!(format!("{err:#}").contains("not UTF-8"), "{err:#}");
        }
    }

    /// A baseline's biggest directories are dotted — `.venv`, `.next`,
    /// `.turbo`, and the `.devkit` marker itself — and jwalk skips hidden
    /// entries by default, so a size that ignored them would report a fraction
    /// of the tree or nothing at all.
    #[test]
    fn a_size_counts_hidden_files_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("visible"), vec![b'x'; 100]).unwrap();
        assert_eq!(dir_size(dir.path()), 100);

        std::fs::create_dir_all(dir.path().join(".venv").join("lib")).unwrap();
        std::fs::write(
            dir.path().join(".venv").join("lib").join("f"),
            vec![b'x'; 500],
        )
        .unwrap();
        std::fs::write(dir.path().join(".dotfile"), vec![b'x'; 7]).unwrap();

        assert_eq!(dir_size(dir.path()), 607, "hidden entries were skipped");
    }

    /// The lock directory lives among the slots, and a baseline directory that
    /// was never created is an empty listing rather than a failure.
    #[test]
    fn slots_skip_the_lock_directory_and_survive_an_absent_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("_baselines");
        assert!(slots_in(&root).unwrap().is_empty());

        std::fs::create_dir_all(root.join(locks::DIR)).unwrap();
        std::fs::create_dir_all(root.join("bbbbbbbbbbbb")).unwrap();
        std::fs::create_dir_all(root.join("aaaaaaaaaaaa")).unwrap();
        std::fs::write(root.join("loose-file"), "x").unwrap();

        assert_eq!(
            slots_in(&root).unwrap(),
            vec![root.join("aaaaaaaaaaaa"), root.join("bbbbbbbbbbbb")]
        );
    }

    /// A marked tree this repository has no registration for is reclaimed as a
    /// plain directory, since `git worktree remove` cannot take it — but not
    /// while its git directory still resolves. That tree is some repository's
    /// checkout, and a sweep run from a different one has no claim on it.
    #[test]
    fn an_orphan_standing_on_a_live_gitdir_is_refused() {
        let f = two_worktrees_sharing_one_baseline();
        remove_worktree(&f.repo, &f.a);
        remove_worktree(&f.repo, &f.b);
        let root = f.baseline.parent().unwrap();
        let stranger = root.join("aaaaaaaaaaaa");
        write_marker(&stranger, &fresh_marker("abc")).unwrap();
        let admin = root.join("somebody-elses-admin-dir");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(
            stranger.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();

        let err =
            drop_reference(&f.repo, &stranger, &registry::Data::default(), false).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("repository that owns it"), "{msg}");
        assert!(stranger.exists(), "the refused tree was removed");
    }

    /// The mark of a tree some repository still owns. An abandoned baseline
    /// keeps its `.git` file while the git directory it names is gone, and that
    /// is the tree a sweep reclaims as a plain directory.
    #[test]
    fn a_gitdir_resolves_only_while_something_stands_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        assert!(
            matches!(gitdir_state(&tree), GitdirState::Absent),
            "no .git"
        );

        let admin = dir.path().join("admin");
        std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        assert!(
            matches!(gitdir_state(&tree), GitdirState::Absent),
            "the target does not exist"
        );

        std::fs::create_dir_all(&admin).unwrap();
        assert!(
            matches!(gitdir_state(&tree), GitdirState::Resolves),
            "the target exists"
        );

        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert!(
            matches!(gitdir_state(&repo), GitdirState::Resolves),
            "a .git directory is a repository"
        );
    }

    /// git keeps every space in a directory name and strips only the trailing
    /// newline when it reads a `.git` file back, so a `gitdir:` whose target
    /// ends in a space names a directory git resolves. Trimming it here would
    /// name a different directory, find nothing there, and delete somebody's
    /// tree as abandoned; the unproven spelling refuses instead, in both sweep
    /// modes.
    #[test]
    fn a_gitdir_padded_with_whitespace_is_refused_in_both_modes() {
        let f = two_worktrees_sharing_one_baseline();
        let root = f.baseline.parent().unwrap();
        let stranger = root.join("bbbbbbbbbbbb");
        write_marker(&stranger, &fresh_marker("abc")).unwrap();
        std::fs::write(
            stranger.join(".git"),
            format!("gitdir: {}/admin \n", root.display()),
        )
        .unwrap();

        assert!(matches!(gitdir_state(&stranger), GitdirState::Unknown));
        for sweep in [Sweep::Report, Sweep::Remove] {
            let err = remove_if_unreferenced(
                &f.repo,
                &stranger,
                &referencers(&f.repo).unwrap(),
                &registrations(&f.repo).unwrap(),
                Gates::sweep(true, true),
                &registry::Data::default(),
                sweep,
            )
            .unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains("neither read nor ruled out"), "{msg}");
            assert!(stranger.exists(), "the refused tree was removed");
        }
    }

    /// `Absent` is the only answer that reaches `remove_dir_all`, so every
    /// failure to classify must land somewhere else. A `gitdir:` naming a path
    /// this build cannot spell is one such failure: the tree it points at
    /// cannot be checked, and a repository cloned under a directory whose name
    /// carries a non-UTF-8 byte is enough to produce it.
    #[test]
    fn a_gitdir_this_build_cannot_spell_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join(".git"), b"gitdir: /somewhere/\xff/.git\n").unwrap();

        assert!(matches!(gitdir_state(&tree), GitdirState::Unknown));
    }

    /// A `.git` this process may unlink but not read, and a `gitdir:` target it
    /// cannot stat. Both are ordinary field states — another user's checkout,
    /// a private parent directory — and both used to read as "nothing here".
    #[cfg(unix)]
    #[test]
    fn a_dot_git_that_cannot_be_read_or_stated_is_unknown() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let unreadable = dir.path().join("unreadable");
        std::fs::create_dir_all(&unreadable).unwrap();
        let dot = unreadable.join(".git");
        std::fs::write(&dot, "gitdir: /wherever\n").unwrap();
        std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root, and some CI containers, ignore file permissions, which leaves
        // nothing to observe either way.
        if std::fs::read(&dot).is_err() {
            assert!(matches!(gitdir_state(&unreadable), GitdirState::Unknown));
        }
        std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o644)).unwrap();

        // A symlink loop rather than a mode, so this half holds as root too.
        let looped = dir.path().join("looped");
        std::fs::create_dir_all(&looped).unwrap();
        let loop_dir = dir.path().join("ring");
        std::os::unix::fs::symlink(&loop_dir, &loop_dir).unwrap();
        std::fs::write(
            looped.join(".git"),
            format!("gitdir: {}/admin\n", loop_dir.display()),
        )
        .unwrap();
        assert!(matches!(gitdir_state(&looped), GitdirState::Unknown));

        let itself = dir.path().join("itself");
        std::fs::create_dir_all(&itself).unwrap();
        let dot = itself.join(".git");
        std::os::unix::fs::symlink(&dot, &dot).unwrap();
        assert!(matches!(gitdir_state(&itself), GitdirState::Unknown));
    }
}
