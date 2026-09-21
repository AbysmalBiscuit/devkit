//! The `pre-tool-use` rules stage: the rules governing the files this call is
//! about to write.
//!
//! What follows makes this safe to hang off a permission gate. Each property
//! below is the safety argument rather than a convention.
//!
//! Nothing here runs before the verdict is final. Loading config or parsing an
//! index earlier would put a fallible, slow step in front of a denial: a
//! malformed config under `?` exits non-zero, which the harness treats as a
//! non-blocking error, and a large parse can spend the manifest's timeout,
//! whose expiry also allows the call.
//!
//! `inject` returns `()`. With no `Result` and no `?` it has no path to an exit
//! code.
//!
//! It runs under `catch_unwind`, the profile `shell.rs` pins with a
//! `compile_error!` against an aborting panic strategy.
//!
//! It emits one write and flush before the log record, because the envelope has
//! to reach the harness ahead of anything that can stall on the log directory.
//!
//! The emitted object carries `additionalContext` and no permission decision. A
//! `permissionDecision` here would auto-approve writes the user would otherwise
//! be asked about.

use std::{
    collections::HashSet,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
};

use devkit_common::{git::Checkout, harness, harness::Harness, paths};
use devkit_rules::{
    context::{self, Subject},
    index,
    query::{self, Filter},
    render,
    vocab::{Severity, Task, canonical_language},
};
use serde_json::Value;

use super::shell::print_envelope;

/// The fired-set for one holder.
///
/// Hashing the complete raw holder id rather than sanitizing it: dropping
/// disallowed characters is lossy, and two holders differing only in what was
/// dropped would share one set.
pub fn fired_path(holder: &str) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    holder.hash(&mut hasher);
    paths::state_dir()
        .join("rules")
        .join(format!("{:016x}", hasher.finish()))
}

/// Forget everything a holder was shown. The release verbs call this, and they
/// already run when a session or a subagent ends; without it the directory
/// grows one file per session forever.
pub fn clear_for_holder(holder: &str) {
    let _ = std::fs::remove_file(fired_path(holder));
}

/// Record ids as injected for `holder`. `devkit rules context` calls this too,
/// so what a session-start block emitted is not emitted again by the first
/// write that happens to match it.
pub fn stamp_ids(holder: &str, ids: &[String]) {
    let path = fired_path(holder);
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    for id in ids {
        let _ = writeln!(file, "{id}");
    }
}

/// Ids already injected for `holder`.
///
/// A parent and its subagents append to this file concurrently, so a line that
/// does not look like an id is skipped rather than trusted or fatal: a torn
/// line costs one duplicate injection, which is not worth a lock.
fn already_fired(holder: &str) -> HashSet<String> {
    std::fs::read_to_string(fired_path(holder))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        // A file entry's id is its configured path, which may hold a space or
        // non-ASCII. Only a control character marks a torn write.
        .filter(|line| !line.is_empty() && !line.chars().any(char::is_control))
        .map(str::to_string)
        .collect()
}

/// The language a rule would be tagged with for this path, from its extension.
fn language_of(path: &str) -> Option<String> {
    let ext = Path::new(path).extension()?.to_str()?;
    Some(canonical_language(ext))
}

/// Inject the rules and files governing `targets`.
///
/// Every failure is silence. The caller's verdict has already been emitted, so
/// there is nothing this can report that would be worth the risk of reporting
/// it.
pub fn inject(
    payload: &Value,
    checkout: &Checkout,
    cwd: &Path,
    declared: Option<Harness>,
    targets: &[String],
    holder: &str,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(payload, checkout, cwd, declared, targets, holder);
    }));
}

fn run(
    payload: &Value,
    checkout: &Checkout,
    cwd: &Path,
    declared: Option<Harness>,
    targets: &[String],
    holder: &str,
) {
    // Every shipped manifest passes `--harness`, but the retired
    // `lockm hook pretooluse` spelling does not, and the shell path infers
    // rather than giving up. Match it.
    let harness_name = declared.unwrap_or_else(|| harness::infer_harness(payload));
    // `devkit_common::config::resolve_in` is the only permitted door to the
    // merged config: `tests/no_stray_config.rs` fails the build when anything
    // else calls `devkit_config::resolve`. `enforcement_enabled_in` reads raw
    // layer flags and never deserializes `Config`, so there is no read to
    // share.
    let Ok((project, provenance)) = devkit_common::config::resolve_in(checkout, None, cwd) else {
        return;
    };
    let settings = &project.rules;
    if !settings.enabled {
        return;
    }
    let Some(root) = checkout.root() else {
        return;
    };

    // git reports a symlinked working directory resolved, since it reads the
    // directory rather than the spelling used to reach it, so `root` is
    // `/private/var/...` where the payload's cwd is `/var/...`; `git.rs`'s own
    // `containment` documents the same hazard. A purely lexical strip would
    // drop every target on macOS.
    let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // A target that escapes the root after normalization is dropped: a rule for
    // the repository root must not fire for a write outside the repository.
    let relative: Vec<String> = targets
        .iter()
        .map(|t| resolve(payload, t))
        .filter_map(|abs| relativize_root(root, &root_canon, &abs))
        .collect();
    if relative.is_empty() {
        return;
    }

    let subject = Subject {
        root: root_canon.clone(),
        targets: relative.clone(),
        harness: Some(harness_slug(harness_name).to_string()),
    };

    let floor: Severity = settings.min_severity.parse().unwrap_or(Severity::Should);
    let fired = already_fired(holder);

    let index_path = match &settings.index {
        Some(p) => PathBuf::from(p),
        None => index::default_index_path(checkout.main_worktree().unwrap_or(root)),
    };

    // One matched set across every target, so a call touching two directories
    // gets both directories' rules.
    let mut chosen: Vec<devkit_rules::model::Rule> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(loaded) = index::load(&index_path) {
        for target in &relative {
            let filter = Filter {
                task: Some(Task::CodeGeneration),
                language: language_of(target),
                min_severity: Some(floor),
                paths: vec![target.clone()],
                ..Filter::default()
            };
            for rule in query::rank(&loaded, query::matching(&loaded, &filter), &[]) {
                if fired.contains(&rule.id) || !seen.insert(rule.id.clone()) {
                    continue;
                }
                chosen.push(rule.clone());
            }
        }
    }

    // A relative `path` anchors to the directory of the layer that declared the
    // entry. `[[context.files]]` is an array, and an array leaf replaces
    // wholesale rather than merging, so every surviving entry came from one
    // file and `origin` names it. Layer discovery keeps the spelling it was
    // reached with, so this resolves like `root_canon`: `subject.root` is
    // already canonical, and comparing it against an unresolved `layer_dir`
    // drops every entry when the project root is reached through a symlink.
    let layer_dir = provenance
        .origin
        .get("context.files")
        .and_then(|f| f.parent())
        .map(|d| std::fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf()))
        .unwrap_or_else(|| root_canon.clone());
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in &project.context.files {
        if fired.contains(&entry.path) || !context::fires(entry, &layer_dir, &subject) {
            continue;
        }
        let path = layer_dir.join(&entry.path);
        if let Some(body) = context::read_capped(&path, settings.max_file_bytes) {
            files.push((entry.path.clone(), body));
        }
    }

    // "How many rules and files one event may inject": the cap bounds files
    // too, not just rules.
    files.truncate(settings.per_event_limit);
    let budget = settings.per_event_limit.saturating_sub(files.len());
    chosen.truncate(budget);

    // Whatever the byte cap would still cut is dropped here, before
    // rendering, rather than rendered truncated and stamped anyway: the fired
    // set must never claim to have shown content that never fully rendered.
    let mut block;
    loop {
        let refs: Vec<&devkit_rules::model::Rule> = chosen.iter().collect();
        block = render::block(&refs, &files, settings.max_event_bytes);
        if !block.ends_with(render::TRUNCATION_NOTE) {
            break;
        }
        if files.pop().is_some() {
            continue;
        }
        if chosen.pop().is_some() {
            continue;
        }
        break;
    }
    if block.is_empty() {
        return;
    }
    let Some(envelope) = harness::warn_shell_json(harness_name, &block) else {
        return;
    };
    print_envelope(&envelope);
    let _ = std::io::stdout().flush();

    let mut ids: Vec<String> = chosen.into_iter().map(|r| r.id).collect();
    ids.extend(files.into_iter().map(|(path, _)| path));
    stamp_ids(holder, &ids);
}

/// A payload path as an absolute one. Mirrors `edit::resolve_against`, which is
/// private to that module and runs only inside `claim`, which returns before
/// reaching it when enforcement is off.
fn resolve(payload: &Value, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(|cwd| {
            let base = Path::new(cwd);
            // The payload's cwd is the spelling the session was started with,
            // which on macOS is the symlink rather than the resolved path the
            // checkout root carries.
            std::fs::canonicalize(base)
                .unwrap_or_else(|_| base.to_path_buf())
                .join(p)
        })
        .unwrap_or_else(|| p.to_path_buf())
}

/// `path`, canonicalized as far as an existing ancestor allows. A write
/// target is often a file that does not exist yet, and `canonicalize` refuses
/// a path that doesn't, so this walks up to the nearest ancestor that does
/// exist, canonicalizes only that much, and rejoins the rest unresolved.
fn canonicalize_prefix(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return path.to_path_buf();
        };
        tail.push(name);
        existing = parent;
    }
    let mut resolved = std::fs::canonicalize(existing).unwrap_or_else(|_| existing.to_path_buf());
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    resolved
}

/// `abs` relative to the repository, trying `root` as given before falling
/// back to a fully canonical comparison. This mirrors `git::containment`: a
/// raw absolute target usually matches the raw root as-is, and canonicalizing
/// `root` first breaks that on Windows, where `canonicalize` returns a
/// `\\?\`-prefixed path a harness never sends. The fallback resolves `abs`
/// through [`canonicalize_prefix`] rather than `canonicalize` directly,
/// because the target itself is often a file that does not exist yet; a raw
/// `root_canon` compared against a raw `abs` only ever rescues a target that
/// happened to already be spelled canonically.
fn relativize_root(root: &Path, root_canon: &Path, abs: &Path) -> Option<String> {
    query::relativize(root, abs)
        .or_else(|| query::relativize(root_canon, &canonicalize_prefix(abs)))
}

fn harness_slug(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::Cursor => "cursor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fired_path_is_stable_and_holder_specific() {
        assert_eq!(fired_path("S"), fired_path("S"));
        assert_ne!(fired_path("S"), fired_path("S/a1"));
    }

    /// nextest runs every test in its own process, so pointing `HOME` and
    /// `XDG_STATE_HOME` at a private tempdir here does not race any other
    /// test's fired-set.
    #[test]
    fn stamp_ids_is_visible_to_already_fired_until_cleared() {
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("XDG_STATE_HOME", home.path());
        }
        let holder = "unit-test-holder";
        assert!(already_fired(holder).is_empty());

        stamp_ids(holder, &["r-a".to_string(), "r-b".to_string()]);
        let fired = already_fired(holder);
        assert!(fired.contains("r-a"));
        assert!(fired.contains("r-b"));

        clear_for_holder(holder);
        assert!(already_fired(holder).is_empty());
    }

    #[test]
    fn language_of_reads_the_extension() {
        assert_eq!(language_of("src/a.rs"), Some("rust".to_string()));
        assert_eq!(language_of("README"), None);
    }

    #[test]
    fn resolve_falls_back_to_the_raw_path_with_no_cwd() {
        let payload = serde_json::json!({});
        assert_eq!(
            resolve(&payload, "relative/b.rs"),
            PathBuf::from("relative/b.rs")
        );
        assert_eq!(resolve(&payload, "/abs/a.rs"), PathBuf::from("/abs/a.rs"));
    }

    #[test]
    fn resolve_joins_a_relative_target_against_the_payloads_cwd() {
        let payload = serde_json::json!({ "cwd": "/repo" });
        assert_eq!(
            resolve(&payload, "src/a.rs"),
            Path::new("/repo").join("src/a.rs")
        );
    }

    #[test]
    fn harness_slug_matches_the_manifest_spelling() {
        assert_eq!(harness_slug(Harness::ClaudeCode), "claude-code");
        assert_eq!(harness_slug(Harness::Codex), "codex");
        assert_eq!(harness_slug(Harness::Cursor), "cursor");
    }

    /// Mirrors the Windows shape: `canonicalize` would prefix the root with
    /// `\\?\`, which an absolute target sent verbatim by a harness never
    /// carries, so the raw root has to be tried first.
    #[test]
    fn relativize_root_prefers_the_raw_root_over_the_canonical_one() {
        let root = Path::new("/repo");
        let root_canon = Path::new("/some/other/spelling/of/repo");
        assert_eq!(
            relativize_root(root, root_canon, Path::new("/repo/src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
    }

    /// The real macOS shape: `abs` is spelled through a different symlink
    /// than `root`, and neither `src/` nor `a.rs` exists yet, so `abs` cannot
    /// be canonicalized directly. `canonicalize_prefix` resolves it through
    /// the symlink, its nearest existing ancestor.
    #[cfg(unix)]
    #[test]
    fn relativize_root_resolves_a_nonexistent_target_reached_through_a_symlink() {
        let real = tempfile::tempdir().unwrap();
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("via-symlink");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let root = std::fs::canonicalize(real.path()).unwrap();
        let abs = link.join("src/a.rs");
        assert_eq!(
            relativize_root(&root, &root, &abs).as_deref(),
            Some("src/a.rs")
        );
    }

    /// Containment must not widen: a path genuinely outside the repository
    /// fails every attempt, even when `root` and `root_canon` differ (a
    /// symlinked project root) and the outside path does not exist either.
    #[cfg(unix)]
    #[test]
    fn relativize_root_drops_a_path_outside_the_repository() {
        let real = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("via-symlink");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();

        let root_canon = std::fs::canonicalize(real.path()).unwrap();
        let outside = other.path().join("elsewhere/b.rs");
        assert_eq!(relativize_root(&link, &root_canon, &outside), None);
    }
}
