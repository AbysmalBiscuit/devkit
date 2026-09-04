//! A flock-guarded JSON document store.
//!
//! The port registry and the lock registry are the same machine over different
//! schemas: an exclusive advisory file lock guards a read-modify-write against a
//! JSON file, with schema-drift salvage and crash-safe atomic replacement. This
//! module is that machine, parameterized over the payload type.
//!
//! A payload implements [`Document`]; callers drive it through [`with_lock`],
//! passing the lock-file and data-file paths. Both files live in the same
//! directory, which is created on demand.

use anyhow::{Context, Result};
use fd_lock::RwLock;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::Path;

/// A JSON payload persisted under an advisory file lock.
///
/// Implementors own a `version` field and know how to recover as many entries
/// as still parse from a document whose top-level shape has drifted across a
/// schema change — discarding the whole file would orphan live state.
pub trait Document: Default + Serialize + DeserializeOwned {
    /// Stamp the current schema version into the document before it is persisted.
    fn stamp_version(&mut self);

    /// Best-effort recovery from a document that no longer deserializes whole.
    /// `None` means nothing is recoverable; the caller backs the file up and
    /// reinitialises. Implementations typically delegate to [`salvage_map`].
    fn salvage(raw: &str) -> Option<Self>;

    /// Noun used in salvage and corruption warnings, e.g. `"registry"`.
    fn label() -> &'static str;

    /// Number of entries currently held; used only for the salvage warning.
    fn len(&self) -> usize;

    /// True when the document holds no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Re-deserialize each value under the top-level object field `field`, keyed by
/// `parse_key`. Values that no longer parse, and keys that `parse_key` rejects,
/// are skipped. The building block for [`Document::salvage`]; returns `None`
/// only when `field` is absent or not an object.
pub fn salvage_map<K: Ord, V: DeserializeOwned>(
    raw: &str,
    field: &str,
    parse_key: impl Fn(&str) -> Option<K>,
) -> Option<BTreeMap<K, V>> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = v.get(field)?.as_object()?;
    let mut out = BTreeMap::new();
    for (k, val) in obj {
        if let (Some(key), Ok(entry)) = (parse_key(k), serde_json::from_value::<V>(val.clone())) {
            out.insert(key, entry);
        }
    }
    Some(out)
}

/// Load a document, salvaging on schema drift exactly as `with_lock` does on read.
/// A missing or empty file yields the default. Never takes a lock — intended for a
/// one-shot read by an owner that has its own exclusion (e.g. the daemon at startup).
pub fn load<D: Document>(path: &Path) -> D {
    read(path)
}

/// Read a file's contents, distinguishing "does not exist" from every other
/// failure. `Ok(None)` is the expected shape of a first run or an absent
/// candidate file. Any other error — permission denied, a directory sitting
/// where a file was expected, a transient I/O failure — comes back as `Err`,
/// because the caller cannot tell whether the content it would have read
/// still means what an absent file would mean.
pub fn read_strict(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Load a document strictly, for callers whose decision is destructive
/// enough that an unreadable input must never be mistaken for an absent one.
/// `NotFound` or a genuinely empty file still yields the default — that is
/// the legitimate first-run shape — but every other read or parse failure
/// returns `Err` instead of `load`'s silent fallback. Performs no salvage or
/// corruption recovery; a caller that needs those stays on `load`.
pub fn try_load<D: Document>(path: &Path) -> Result<D> {
    match read_strict(path)? {
        None => Ok(D::default()),
        Some(s) if s.trim().is_empty() => Ok(D::default()),
        Some(s) => serde_json::from_str(&s).with_context(|| {
            format!(
                "parsing {} at {}; delete the file (or restore it from a backup) to reinitialise it, \
                 then retry",
                D::label(),
                path.display()
            )
        }),
    }
}

/// Persist a document with a crash-safe atomic rename. Takes no lock and does not
/// stamp the version — a caller that mutated the document should call
/// `Document::stamp_version` first (as `with_lock` does).
pub fn save<D: Document>(path: &Path, data: &D) -> Result<()> {
    write(path, data)
}

/// Load a document, salvaging on schema drift and backing up on true corruption.
/// A missing or empty file yields the default. Never fails: an unreadable file
/// is renamed to `*.json.bak` and replaced by a fresh default.
fn read<D: Document>(path: &Path) -> D {
    let s = match fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return D::default(),
    };
    match serde_json::from_str::<D>(&s) {
        Ok(d) => d,
        // A parse failure usually means a schema change, not garbage. Recover
        // every entry we still understand rather than discarding live state.
        Err(_) => match D::salvage(&s) {
            Some(d) => {
                let n = d.len();
                eprintln!(
                    "warning: {} schema differs; salvaged {} entr{}",
                    D::label(),
                    n,
                    if n == 1 { "y" } else { "ies" }
                );
                d
            }
            None => {
                let _ = fs::rename(path, path.with_extension("json.bak"));
                eprintln!(
                    "warning: unreadable {}; backed up and reinitialised",
                    D::label()
                );
                D::default()
            }
        },
    }
}

/// Persist a document by writing a sibling temp file and renaming it over the
/// target — atomic on POSIX and Windows, so a crash mid-write can't truncate it.
fn write<D: Document>(path: &Path, data: &D) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(data)?)?;
    fs::rename(&tmp, path).with_context(|| format!("atomically replacing {}", D::label()))?;
    Ok(())
}

/// Acquire the exclusive advisory lock at `lock_path`, load the document at
/// `data_path` via `load`, run `f`, and persist the (version-stamped)
/// result. The parent directory is created on demand. Shared by `with_lock`
/// and `with_lock_strict`, which differ only in how the pre-`f` load can fail.
fn with_lock_via<D: Document, T>(
    lock_path: &Path,
    data_path: &Path,
    load: impl FnOnce(&Path) -> Result<D>,
    f: impl FnOnce(&mut D) -> Result<T>,
) -> Result<T> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let _ = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    let mut lock = RwLock::new(File::open(lock_path)?);
    let _guard = lock.write()?; // blocks until exclusive
    let mut data = load(data_path)?;
    let out = f(&mut data)?;
    data.stamp_version();
    write(data_path, &data)?;
    Ok(out)
}

/// Run `f` while holding the exclusive advisory lock at `lock_path`, against the
/// JSON document at `data_path`; persists the (version-stamped) result. The
/// parent directory is created on demand. Keep the work inside `f` minimal —
/// the lock is held for its whole duration.
pub fn with_lock<D: Document, T>(
    lock_path: &Path,
    data_path: &Path,
    f: impl FnOnce(&mut D) -> Result<T>,
) -> Result<T> {
    with_lock_via(lock_path, data_path, |p| Ok(read::<D>(p)), f)
}

/// As `with_lock`, but the pre-`f` load goes through `try_load`: an
/// unreadable or unparsable document aborts with `Err` — leaving both the
/// lock and the on-disk file untouched — rather than silently committing a
/// default document over it.
pub fn with_lock_strict<D: Document, T>(
    lock_path: &Path,
    data_path: &Path,
    f: impl FnOnce(&mut D) -> Result<T>,
) -> Result<T> {
    with_lock_via(lock_path, data_path, try_load::<D>, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::path::PathBuf;

    const VERSION: u32 = 3;

    #[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
    struct Doc {
        #[serde(default)]
        version: u32,
        #[serde(default)]
        items: BTreeMap<u16, String>,
    }

    impl Document for Doc {
        fn stamp_version(&mut self) {
            self.version = VERSION;
        }
        fn salvage(raw: &str) -> Option<Self> {
            Some(Doc {
                version: 0,
                items: salvage_map(raw, "items", |k| k.parse::<u16>().ok())?,
            })
        }
        fn label() -> &'static str {
            "test store"
        }
        fn len(&self) -> usize {
            self.items.len()
        }
    }

    /// A file path that does not exist yet — `tag` names the lock or the
    /// document, and the store creates whichever it is handed. The guard comes
    /// back with it: dropping the guard removes the directory around the file.
    fn scratch(tag: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(tag);
        (dir, path)
    }

    #[test]
    fn with_lock_persists_and_stamps_version() {
        let (_lock_guard, lock) = scratch("a.lock");
        let (_data_guard, data) = scratch("a.json");
        with_lock::<Doc, _>(&lock, &data, |d| {
            d.items.insert(8080, "api".into());
            Ok(())
        })
        .unwrap();
        let back = with_lock::<Doc, _>(&lock, &data, |d| Ok(d.clone())).unwrap();
        assert_eq!(back.items[&8080], "api");
        assert_eq!(back.version, VERSION);
    }

    #[test]
    fn read_missing_is_default() {
        let (_lock_guard, lock) = scratch("b.lock");
        let (_data_guard, data) = scratch("b.json");
        let out = with_lock::<Doc, _>(&lock, &data, |d| Ok(d.is_empty())).unwrap();
        assert!(out);
    }

    #[test]
    fn salvage_recovers_entries_from_drifted_schema() {
        // A string `version` forces whole-document deserialization to fail while
        // the per-entry values still parse.
        let raw = r#"{"version":"oops","items":{"8080":"api","9090":"web"}}"#;
        assert!(serde_json::from_str::<Doc>(raw).is_err());
        let d = Doc::salvage(raw).expect("items object present");
        assert_eq!(d.items[&8080], "api");
        assert_eq!(d.items[&9090], "web");
        assert_eq!(d.version, 0);
    }

    #[test]
    fn salvage_none_without_target_field() {
        assert!(Doc::salvage(r#"{"something":"else"}"#).is_none());
    }

    /// A crash between truncating a file and writing it leaves an empty one.
    /// That is the first-run shape, not corruption: both readers answer with
    /// the default, and `read` must not back the file up as unparseable.
    #[test]
    fn an_empty_document_reads_as_the_default_without_a_backup() {
        let (_guard, p) = scratch("empty.json");
        let backup = p.with_extension("json.bak");
        for body in ["", "  \n\t "] {
            fs::write(&p, body).unwrap();
            assert_eq!(load::<Doc>(&p), Doc::default(), "load {body:?}");
            assert!(
                !backup.exists(),
                "an empty file is not corruption: {body:?}"
            );
            assert_eq!(
                try_load::<Doc>(&p).unwrap(),
                Doc::default(),
                "try_load {body:?}"
            );
        }
    }

    #[test]
    fn is_empty_tracks_the_entry_count() {
        let mut d = Doc::default();
        assert!(d.is_empty());
        d.items.insert(8080, "api".into());
        assert!(!d.is_empty());
    }

    #[test]
    fn load_save_roundtrip_and_missing_default() {
        let (_guard, p) = scratch("loadsave.json");
        assert!(load::<Doc>(&p).is_empty(), "missing file loads as default");
        let mut d = Doc::default();
        d.items.insert(8080, "api".into());
        save(&p, &d).unwrap();
        assert_eq!(load::<Doc>(&p).items[&8080], "api");
    }
}
