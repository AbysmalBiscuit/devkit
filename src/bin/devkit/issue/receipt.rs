//! Render receipts: an empty file per title and per body `issue render` or
//! `issue pr render` produced in an agent session, named by the digest of its
//! text. The pre-tool-use hook allows an issue-writing MCP call only when its
//! text has an issue receipt.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use devkit_common::{caller::HARNESS_SESSION_VARS, gitignore, harness_log::redact, vcs::Checkout};

/// What a receipt vouches for. Each kind has its own store, so a PR render
/// never vouches for an issue write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Issue,
    Pr,
}

impl Kind {
    pub(crate) const ALL: [Self; 2] = [Self::Issue, Self::Pr];

    fn dir(self) -> &'static str {
        match self {
            Self::Issue => "issue-receipts",
            Self::Pr => "pr-receipts",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Field {
    Title,
    Body,
}

impl Field {
    fn prefix(self) -> &'static str {
        match self {
            Self::Title => "title-",
            Self::Body => "body-",
        }
    }
}

/// Age past which a render deletes a session's receipts, for sessions
/// that never fired `session-end`.
pub(crate) const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The text a receipt vouches for: LF line endings, no trailing whitespace on
/// any line, none at either end. An MCP client that reflows line endings or
/// trims a trailing newline still matches.
pub(crate) fn normalize(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// SHA-256 of the normalized text as bare lowercase hex. The digest's
/// `sha256:` prefix is dropped because a colon is not a legal NTFS filename
/// character.
pub(crate) fn hex(text: &str) -> String {
    let digest = redact::digest(&normalize(text));
    digest
        .strip_prefix("sha256:")
        .unwrap_or(&digest)
        .to_string()
}

/// Whether `id` is safe as one path component: non-empty, and only ASCII
/// letters, digits, `-` and `_`.
pub(crate) fn valid_session(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Every distinct, non-empty harness session id in the environment. Two
/// harness variables can hold different ids, and the hook may be handed
/// either.
pub(crate) fn sessions_from_env() -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for var in HARNESS_SESSION_VARS {
        if let Ok(id) = std::env::var(var)
            && !id.is_empty()
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

/// Where receipts for a directory in `checkout` live: the repository's main
/// worktree, else the checkout root. Every worktree of one repository shares
/// the store, because a harness reports the directory its session started in,
/// not the worktree its shell `cd`ed into to render.
pub(crate) fn store_root(checkout: &Checkout) -> Option<PathBuf> {
    checkout
        .main_worktree()
        .or_else(|| checkout.root())
        .map(Path::to_path_buf)
}

fn receipts_root(checkout: &Path, kind: Kind) -> PathBuf {
    checkout.join(".devkit").join(kind.dir())
}

pub(crate) fn session_dir(checkout: &Path, kind: Kind, session: &str) -> PathBuf {
    receipts_root(checkout, kind).join(session)
}

fn receipt_path(checkout: &Path, kind: Kind, session: &str, field: Field, text: &str) -> PathBuf {
    session_dir(checkout, kind, session).join(format!("{}{}", field.prefix(), hex(text)))
}

/// Refuse an invalid session id, and a store that is not a plain directory.
/// A `.devkit` that is a file would read as "no receipt" on Windows, which
/// reports a path through a file as not found. A symlinked `.devkit` or
/// receipts directory, as a commit can carry, would point the stale sweep's
/// deletes and the receipt writes outside the checkout.
fn check_store(checkout: &Path, kind: Kind, session: &str) -> Result<()> {
    if !valid_session(session) {
        bail!("session id `{session}` is not usable as a directory name");
    }
    check_dirs(checkout, kind)
}

fn check_dirs(checkout: &Path, kind: Kind) -> Result<()> {
    let devkit = checkout.join(".devkit");
    for dir in [devkit.clone(), receipts_root(checkout, kind)] {
        let Ok(meta) = std::fs::symlink_metadata(&dir) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            bail!(
                "{} is a symlink: devkit keeps render receipts only in a real directory inside the checkout",
                dir.display()
            );
        }
        if dir == devkit && meta.is_file() {
            bail!(
                "{} is a file, not the directory devkit keeps its render receipts in",
                devkit.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn write(
    checkout: &Path,
    kind: Kind,
    session: &str,
    title: &str,
    body: &str,
) -> Result<()> {
    check_store(checkout, kind, session)?;
    let dir = session_dir(checkout, kind, session);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    gitignore::write_self_ignore(&checkout.join(".devkit"));
    for (field, text) in [(Field::Title, title), (Field::Body, body)] {
        let path = receipt_path(checkout, kind, session, field, text);
        std::fs::File::create(&path).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn has(
    checkout: &Path,
    kind: Kind,
    session: &str,
    field: Field,
    text: &str,
) -> Result<bool> {
    check_store(checkout, kind, session)?;
    let path = receipt_path(checkout, kind, session, field, text);
    path.try_exists()
        .with_context(|| format!("reading {}", path.display()))
}

/// Record a rendered `title` and `body` for every agent session in the
/// environment, in the store of the checkout holding `start`. Outside a
/// session nothing is written, and stderr says so.
pub(crate) fn record(start: &Path, kind: Kind, title: &str, body: &str) -> Result<()> {
    let sessions = sessions_from_env();
    if let Some(bad) = sessions.iter().find(|s| !valid_session(s)) {
        bail!("session id `{bad}` is not usable as a directory name: no receipt written");
    }
    if sessions.is_empty() {
        eprintln!("no agent session: no receipt written");
        return Ok(());
    }
    let checkout = store_root(&Checkout::at(start))
        .with_context(|| format!("not inside a git checkout: {}", start.display()))?;
    check_dirs(&checkout, kind)?;
    let _ = sweep_stale(&checkout, kind, STALE_AFTER);
    for session in &sessions {
        write(&checkout, kind, session, title, body)?;
    }
    Ok(())
}

/// Delete one session's receipts of every kind. An invalid id or a session
/// with none is a no-op. Every kind is attempted, except one whose store is a
/// symlink, which is skipped; the first failure is returned.
pub(crate) fn clear_session(checkout: &Path, session: &str) -> Result<()> {
    if !valid_session(session) {
        return Ok(());
    }
    let mut first_err = None;
    for kind in Kind::ALL {
        if let Err(e) = check_dirs(checkout, kind) {
            first_err.get_or_insert(e);
            continue;
        }
        let dir = session_dir(checkout, kind, session);
        match std::fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                first_err.get_or_insert_with(|| {
                    anyhow::Error::new(e).context(format!("removing {}", dir.display()))
                });
            }
            _ => {}
        }
    }
    first_err.map_or(Ok(()), Err)
}

/// Delete every session directory last written to more than `older_than` ago.
/// One that cannot be read or removed is left for the next sweep.
pub(crate) fn sweep_stale(checkout: &Path, kind: Kind, older_than: Duration) -> Result<()> {
    let root = receipts_root(checkout, kind);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", root.display())),
    };
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|age| age > older_than));
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;

    fn backdate(dir: &Path, by: Duration) {
        #[cfg(windows)]
        let handle = {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_WRITE_ATTRIBUTES, and FILE_FLAG_BACKUP_SEMANTICS to open a
            // directory at all.
            std::fs::OpenOptions::new()
                .access_mode(0x100)
                .custom_flags(0x0200_0000)
                .open(dir)
                .unwrap()
        };
        #[cfg(not(windows))]
        let handle = std::fs::File::open(dir).unwrap();
        handle.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn normalize_folds_line_endings_and_edge_whitespace() {
        assert_eq!(normalize("a  \r\nb\t\r\n\r\n"), "a\nb");
        assert_eq!(normalize("  x "), "x");
    }

    #[test]
    fn hex_is_colon_free_lowercase_sha256() {
        let h = hex("x");
        assert_eq!(h.len(), 64);
        assert!(!h.contains(':'));
        assert!(h.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "{h}");
        assert_eq!(hex("a\r\n"), hex("a"));
    }

    #[test]
    fn session_ids_are_path_safe() {
        for ok in ["f18b31aa-9b8c", "a_B-9"] {
            assert!(valid_session(ok), "{ok}");
        }
        for bad in ["", "..", "../x", "a/b", "a\\b", "a:b", "é"] {
            assert!(!valid_session(bad), "{bad}");
        }
    }

    #[test]
    fn a_written_pair_is_found_field_by_field() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, Kind::Issue, "S", "T", "B").unwrap();
        assert!(has(d, Kind::Issue, "S", Field::Title, "T").unwrap());
        assert!(has(d, Kind::Issue, "S", Field::Body, "B ").unwrap());
        assert!(!has(d, Kind::Issue, "S", Field::Body, "T").unwrap());
        assert!(!has(d, Kind::Issue, "S2", Field::Title, "T").unwrap());
        assert!(!has(d, Kind::Pr, "S", Field::Title, "T").unwrap());
        assert!(d.join(".devkit/.gitignore").exists());
    }

    #[test]
    fn write_refuses_an_invalid_session() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write(dir.path(), Kind::Issue, "../x", "T", "B").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn clear_session_removes_only_that_session() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, Kind::Issue, "S1", "T", "B").unwrap();
        write(d, Kind::Pr, "S1", "T", "B").unwrap();
        write(d, Kind::Issue, "S2", "T", "B").unwrap();
        clear_session(d, "S1").unwrap();
        assert!(!session_dir(d, Kind::Issue, "S1").exists());
        assert!(!session_dir(d, Kind::Pr, "S1").exists());
        assert!(session_dir(d, Kind::Issue, "S2").exists());
        clear_session(d, "..").unwrap();
        assert!(session_dir(d, Kind::Issue, "S2").exists());
    }

    #[cfg(unix)]
    #[test]
    fn clear_session_reports_a_symlinked_kind_and_clears_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, Kind::Issue, "S1", "T", "B").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("S1")).unwrap();
        std::os::unix::fs::symlink(outside.path(), receipts_root(d, Kind::Pr)).unwrap();
        let err = clear_session(d, "S1").unwrap_err();
        assert!(format!("{err:#}").contains("symlink"), "{err:#}");
        assert!(outside.path().join("S1").exists());
        assert!(!session_dir(d, Kind::Issue, "S1").exists());
    }

    #[test]
    fn sweep_drops_only_old_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(d, Kind::Issue, "old", "T", "B").unwrap();
        write(d, Kind::Issue, "fresh", "T", "B").unwrap();
        backdate(
            &session_dir(d, Kind::Issue, "old"),
            Duration::from_secs(8 * 24 * 60 * 60),
        );
        sweep_stale(d, Kind::Issue, STALE_AFTER).unwrap();
        assert!(!session_dir(d, Kind::Issue, "old").exists());
        assert!(session_dir(d, Kind::Issue, "fresh").exists());
    }

    #[test]
    fn a_devkit_file_fails_the_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".devkit"), "").unwrap();
        let err = write(dir.path(), Kind::Issue, "S", "T", "B").unwrap_err();
        assert!(format!("{err:#}").contains(".devkit"), "{err:#}");
    }
}
