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

/// Run `f` while holding the exclusive advisory lock at `lock_path`, against the
/// JSON document at `data_path`; persists the (version-stamped) result. The
/// parent directory is created on demand. Keep the work inside `f` minimal —
/// the lock is held for its whole duration.
pub fn with_lock<D: Document, T>(
    lock_path: &Path,
    data_path: &Path,
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
    let mut data = read::<D>(data_path);
    let out = f(&mut data)?;
    data.stamp_version();
    write(data_path, &data)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

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

    fn scratch(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("devkit-store-{}-{}", std::process::id(), tag))
    }

    #[test]
    fn with_lock_persists_and_stamps_version() {
        let lock = scratch("a.lock");
        let data = scratch("a.json");
        let _ = fs::remove_file(&data);
        with_lock::<Doc, _>(&lock, &data, |d| {
            d.items.insert(8080, "api".into());
            Ok(())
        })
        .unwrap();
        let back = with_lock::<Doc, _>(&lock, &data, |d| Ok(d.clone())).unwrap();
        assert_eq!(back.items[&8080], "api");
        assert_eq!(back.version, VERSION);
        let _ = fs::remove_file(&data);
        let _ = fs::remove_file(&lock);
    }

    #[test]
    fn read_missing_is_default() {
        let lock = scratch("b.lock");
        let data = scratch("b.json");
        let _ = fs::remove_file(&data);
        let out = with_lock::<Doc, _>(&lock, &data, |d| Ok(d.is_empty())).unwrap();
        assert!(out);
        let _ = fs::remove_file(&data);
        let _ = fs::remove_file(&lock);
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
}
