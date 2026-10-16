//! The git processes a command starts, read from git's own trace2 event
//! files. Setting [`GitTrace::VAR`] to [`GitTrace::dir`] on one command makes
//! every git it starts, directly or through a child, write a file there.

use std::{collections::BTreeMap, path::Path};

pub struct GitTrace {
    dir: tempfile::TempDir,
}

impl GitTrace {
    pub const VAR: &str = "GIT_TRACE2_EVENT";

    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("trace dir"),
        }
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The arguments of every git process started since the last call, with
    /// any `-C <dir>` dropped so one question asked of two spellings of a
    /// directory reads as the same call. A git that another git started, as
    /// an alias does, is left out. Reading clears the directory.
    pub fn take(&self) -> Vec<String> {
        let mut calls = Vec::new();
        for entry in std::fs::read_dir(self.dir()).expect("read trace dir") {
            let path = entry.expect("trace entry").path();
            let text = std::fs::read_to_string(&path).expect("read trace file");
            std::fs::remove_file(&path).expect("clear trace file");
            let start = text.lines().find_map(|line| {
                let event: serde_json::Value = serde_json::from_str(line).ok()?;
                (event["event"] == "start").then_some(event)
            });
            let Some(start) = start else { continue };
            if start["sid"].as_str().is_some_and(|sid| sid.contains('/')) {
                continue;
            }
            let mut argv: Vec<&str> = start["argv"]
                .as_array()
                .expect("argv")
                .iter()
                .skip(1)
                .map(|a| a.as_str().expect("argv entry"))
                .collect();
            if let Some(at) = argv.iter().position(|a| *a == "-C") {
                argv.drain(at..(at + 2).min(argv.len()));
            }
            calls.push(argv.join(" "));
        }
        calls.sort();
        calls
    }

    /// The calls in `calls` made more than once, with how many times.
    pub fn repeated(calls: &[String]) -> BTreeMap<&str, usize> {
        let mut counts = BTreeMap::new();
        for call in calls {
            *counts.entry(call.as_str()).or_insert(0) += 1;
        }
        counts.retain(|_, n| *n > 1);
        counts
    }
}
