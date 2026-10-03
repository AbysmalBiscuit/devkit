//! Which todo a harness's own task or plan entry became, kept apart from the
//! todos so capture works whatever store holds them. A crash between adding
//! a todo and recording it here leaves a todo whose later native updates are
//! skipped.

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use devkit_common::store;
use serde::{Deserialize, Serialize};

use crate::{Holder, node::Harness};

const VERSION: u32 = 1;
const SEP: char = '\u{1f}';

#[derive(Debug, Default, Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    version: u32,
    /// `harness SEP holder SEP native id` to todo id.
    #[serde(default)]
    ids: BTreeMap<String, String>,
    /// `harness SEP holder` to the last list a list-replace tool sent, as
    /// (step text, todo id) in order.
    #[serde(default)]
    snapshots: BTreeMap<String, Vec<(String, String)>>,
}

impl store::Document for Doc {
    fn stamp_version(&mut self) {
        self.version = VERSION;
    }

    /// Required by the trait, never called: the map loads strictly.
    fn salvage(raw: &str) -> Option<Self> {
        Some(Self {
            version: 0,
            ids: store::salvage_map(raw, "ids", |k| Some(k.to_string()))?,
            snapshots: store::salvage_map(raw, "snapshots", |k| Some(k.to_string()))
                .unwrap_or_default(),
        })
    }

    fn label() -> &'static str {
        "native todo map"
    }

    fn len(&self) -> usize {
        self.ids.len()
    }
}

fn holder_key(harness: Harness, holder: &str) -> String {
    format!("{}{SEP}{holder}", harness.prefix())
}

fn id_key(harness: Harness, holder: &str, native: &str) -> String {
    format!("{}{SEP}{native}", holder_key(harness, holder))
}

pub struct NativeMap {
    dir: PathBuf,
}

impl NativeMap {
    /// Keeps `dir/native.json`, guarded by `dir/native.lock`.
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn with_doc<T>(&self, f: impl FnOnce(&mut Doc) -> Result<T>) -> Result<T> {
        store::with_lock_strict(
            &self.dir.join("native.lock"),
            &self.dir.join("native.json"),
            f,
        )
    }

    /// A native id already mapped for `holder` is remapped, so a resumed
    /// session that numbers from 1 again never edits the previous run's todos.
    pub fn record(
        &self,
        harness: Harness,
        holder: &Holder,
        native: &str,
        todo: &str,
    ) -> Result<()> {
        self.with_doc(|doc| {
            doc.ids
                .insert(id_key(harness, holder, native), todo.to_string());
            Ok(())
        })
    }

    /// The holder's own mapping first, then its session's (the text before
    /// the first `/`).
    pub fn lookup(
        &self,
        harness: Harness,
        holder: &Holder,
        native: &str,
    ) -> Result<Option<String>> {
        let session = holder.split('/').next().unwrap_or(holder);
        self.with_doc(|doc| {
            Ok([&**holder, session]
                .into_iter()
                .find_map(|h| doc.ids.get(&id_key(harness, h, native)).cloned()))
        })
    }

    pub fn snapshot(&self, harness: Harness, holder: &Holder) -> Result<Vec<(String, String)>> {
        self.with_doc(|doc| {
            Ok(doc
                .snapshots
                .get(&holder_key(harness, holder))
                .cloned()
                .unwrap_or_default())
        })
    }

    pub fn set_snapshot(
        &self,
        harness: Harness,
        holder: &Holder,
        steps: Vec<(String, String)>,
    ) -> Result<()> {
        self.with_doc(|doc| {
            doc.snapshots.insert(holder_key(harness, holder), steps);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_falls_back_to_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let map = NativeMap::at(dir.path().to_path_buf());
        map.record(Harness::Claude, &Holder::new("S"), "1", "4")
            .unwrap();
        assert_eq!(
            map.lookup(Harness::Claude, &Holder::new("S/a1"), "1")
                .unwrap(),
            Some("4".to_string())
        );
        assert_eq!(
            map.lookup(Harness::Codex, &Holder::new("S"), "1").unwrap(),
            None
        );
        assert_eq!(
            map.lookup(Harness::Claude, &Holder::new("T"), "1").unwrap(),
            None
        );
    }

    #[test]
    fn the_holders_own_mapping_wins_over_the_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let map = NativeMap::at(dir.path().to_path_buf());
        map.record(Harness::Claude, &Holder::new("S"), "1", "4")
            .unwrap();
        map.record(Harness::Claude, &Holder::new("S/a1"), "1", "7")
            .unwrap();
        assert_eq!(
            map.lookup(Harness::Claude, &Holder::new("S/a1"), "1")
                .unwrap(),
            Some("7".to_string())
        );
    }

    #[test]
    fn a_reused_native_id_replaces_the_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let map = NativeMap::at(dir.path().to_path_buf());
        map.record(Harness::Claude, &Holder::new("S"), "1", "4")
            .unwrap();
        map.record(Harness::Claude, &Holder::new("S"), "1", "9")
            .unwrap();
        assert_eq!(
            map.lookup(Harness::Claude, &Holder::new("S"), "1").unwrap(),
            Some("9".to_string())
        );
    }

    #[test]
    fn snapshots_are_kept_per_holder() {
        let dir = tempfile::tempdir().unwrap();
        let map = NativeMap::at(dir.path().to_path_buf());
        let steps = vec![("a".to_string(), "1".to_string())];
        map.set_snapshot(Harness::Codex, &Holder::new("S"), steps.clone())
            .unwrap();
        assert_eq!(
            map.snapshot(Harness::Codex, &Holder::new("S")).unwrap(),
            steps
        );
        assert!(
            map.snapshot(Harness::Codex, &Holder::new("S/a1"))
                .unwrap()
                .is_empty()
        );
    }
}
