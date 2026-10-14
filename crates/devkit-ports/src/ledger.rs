//! The run ledger: one record per `devrun task` run, kept in the state
//! directory where an agent's edit tool cannot reach it.
//!
//! devrun is the program that spawns the task, so the exit code it records is
//! the one the operating system reported, not one read back from a command
//! line. Each record is its own file, written whole and renamed into place, so
//! concurrent runs never interleave and a reader never sees half a record.

use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use anyhow::{Context, Result};
use devkit_common::{
    paths,
    vcs::{self, Changes, VersionControl},
};
use serde::{Deserialize, Serialize};

/// How many trailing output lines a record keeps.
pub const TAIL_LINES: usize = 100;
/// A line longer than this is cut, so one unbroken stream cannot grow a
/// record without bound.
const MAX_LINE_BYTES: usize = 4096;

/// One recorded run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    /// The task name as invoked.
    pub task: String,
    /// The template args passed with `--arg` and `--arg-file`.
    #[serde(default)]
    pub args: BTreeMap<String, String>,
    /// The repository's primary checkout, which runs are listed by.
    pub repo: PathBuf,
    /// The worktree the run started in.
    pub worktree: PathBuf,
    /// The full commit id checked out when the run started; `None` when it
    /// could not be read, as on a repository with no commits.
    pub head: Option<String>,
    /// The tree had no modified, staged or untracked files when the run
    /// started and when it ended, the head had not moved, the task ran as
    /// the committed config defines it ([`Run::committed_definition`]), and
    /// no `--env` or `--env-file` was given.
    pub clean: bool,
    /// The task's definition is the one the repository's committed
    /// `devkit.toml` layers alone give at HEAD: no local, home, `--config`
    /// or `$DEVKIT_CONFIG` layer changed it. `false` when that could not be
    /// determined.
    #[serde(default)]
    pub committed_definition: bool,
    /// The names of the variables `--env` and `--env-file` set. Their values
    /// may be secrets, so they are never stored.
    #[serde(default)]
    pub env: Vec<String>,
    /// The `--env-file` given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_file: Option<PathBuf>,
    /// Each command step devrun executed, in order, with the argv it ran.
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The exit code of the program devrun ran, or of a sequence's first
    /// failing step. `None` when it was killed by a signal or never ran.
    pub exit_code: Option<i32>,
    pub started_at: String,
    pub ended_at: String,
    /// The last lines of output devrun captured. A stream attached to a
    /// terminal is passed straight through, so `None` means nothing was
    /// captured.
    pub tail: Option<Vec<String>>,
    /// The record's file. Filled when read, never stored.
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Run {
    /// Whether this run proves `task` exited 0 on a clean tree at `sha`.
    pub fn proves(&self, task: &str, sha: &str) -> bool {
        self.task == task
            && self.exit_code == Some(0)
            && self.clean
            && self.head.as_deref() == Some(sha)
    }
}

/// One command step of a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// The command task the step ran.
    pub task: String,
    /// The argv it resolved to.
    pub argv: Vec<String>,
}

/// The directory holding the records.
#[derive(Debug, Clone)]
pub struct Ledger {
    dir: PathBuf,
}

impl Ledger {
    pub fn live() -> Self {
        Self::at(paths::runs_dir())
    }

    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Write `run` as a new record and return its path.
    pub fn record(&self, run: &Run) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        let stamp = now.format("%Y%m%dT%H%M%S%.6fZ");
        let name = format!("{stamp}-{}.json", std::process::id());
        let path = self.dir.join(name);
        let mut tmp = tempfile::NamedTempFile::new_in(&self.dir)
            .with_context(|| format!("creating a record in {}", self.dir.display()))?;
        serde_json::to_writer_pretty(&mut tmp, run)?;
        tmp.write_all(b"\n")?;
        tmp.persist(&path)
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    /// Every readable record, oldest first. A record that does not parse is
    /// skipped: it can prove nothing.
    pub fn runs(&self) -> Result<Vec<Run>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.dir.display())),
        };
        let mut runs: Vec<Run> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .filter_map(|p| {
                let text = std::fs::read_to_string(&p).ok()?;
                let mut run: Run = serde_json::from_str(&text).ok()?;
                run.path = Some(p);
                Some(run)
            })
            .collect();
        runs.sort_by(|a, b| (&a.started_at, &a.path).cmp(&(&b.started_at, &b.path)));
        Ok(runs)
    }

    /// The runs started in `repo`'s checkouts, oldest first.
    pub fn runs_in(&self, repo: &Path) -> Result<Vec<Run>> {
        Ok(self
            .runs()?
            .into_iter()
            .filter(|r| paths::same_path(&r.repo, repo))
            .collect())
    }

    /// The latest run in `repo`'s checkouts proving `task` exited 0 on a
    /// clean tree at exactly `sha`. A run at an ancestor, a prefix of the id,
    /// a dirty tree or another repository proves nothing.
    pub fn passed(&self, repo: &Path, task: &str, sha: &str) -> Result<Option<Run>> {
        Ok(self
            .runs_in(repo)?
            .into_iter()
            .rev()
            .find(|r| r.proves(task, sha)))
    }

    /// Whether `path` names something inside the ledger, however it is
    /// spelled. A relative `path` is taken from `cwd`.
    pub fn contains(&self, path: &Path, cwd: &Path) -> bool {
        let abs = devkit_config::normalize_lexically(&cwd.join(path));
        let dir = devkit_config::normalize_lexically(&self.dir);
        let (abs_real, dir_real) = (resolve_existing(&abs), resolve_existing(&dir));
        [&abs, &abs_real]
            .iter()
            .any(|p| p.starts_with(&dir) || p.starts_with(&dir_real))
    }
}

/// `path` with its longest existing prefix resolved through symlinks, so a
/// link into the ledger is caught by where it points.
fn resolve_existing(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(cur) {
            return rest.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The config the repository's committed `devkit.toml` layers alone give at
/// `start`: each tracked `devkit.toml` inside `worktree_root` that applies
/// there, read as HEAD holds it. Local, home, ignored, explicit and
/// out-of-tree layers are left out.
pub fn committed_config(worktree_root: &Path, start: &Path) -> Result<devkit_config::Config> {
    let root = std::fs::canonicalize(worktree_root)
        .with_context(|| format!("resolving {}", worktree_root.display()))?;
    let vcs = vcs::Vcs::at(&root);
    let mut tables = Vec::new();
    for layer in devkit_config::project_layers(start, None)? {
        if layer.path.file_name() != Some(std::ffi::OsStr::new(devkit_config::CONFIG_FILE)) {
            continue;
        }
        let Ok(path) = std::fs::canonicalize(&layer.path) else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        let rel: Vec<_> = rel.iter().map(|c| c.to_string_lossy()).collect();
        let Some(body) = vcs.committed_file(&root, &rel.join("/"))? else {
            continue;
        };
        let body = String::from_utf8(body)
            .with_context(|| format!("reading the committed {}", path.display()))?;
        let table: toml::Table = toml::from_str(&body)
            .with_context(|| format!("parsing the committed {}", path.display()))?;
        tables.push((path, table));
    }
    let (merged, ..) = devkit_config::merge_layers(&tables);
    toml::Value::Table(merged)
        .try_into()
        .context("deserializing the committed config")
}

/// What the start of a run captured about the tree it ran on.
pub struct Start {
    task: String,
    args: BTreeMap<String, String>,
    repo: PathBuf,
    worktree: PathBuf,
    head: Option<String>,
    clean: bool,
    committed_definition: bool,
    env: Vec<String>,
    env_file: Option<PathBuf>,
    started_at: String,
}

/// The commit at `root` and whether its tree is clean. An unreadable answer
/// reads as no commit and not clean, so it can never prove a pass.
fn tree_state(root: &Path) -> (Option<String>, bool) {
    let vcs = vcs::Vcs::at(root);
    let head = vcs.revision(root).ok().filter(|s| !s.is_empty());
    let clean = matches!(vcs.dirty(root, Changes::All), Ok(false));
    (head, clean)
}

impl Start {
    /// Capture the tree state for a run of `task` at worktree `root`.
    /// `committed_definition` says whether the task runs as the committed
    /// config defines it; `env` names the variables `--env` and `env_file`
    /// set.
    pub fn capture(
        root: &Path,
        task: &str,
        args: &BTreeMap<String, String>,
        committed_definition: bool,
        env: Vec<String>,
        env_file: Option<PathBuf>,
    ) -> Self {
        let (head, clean) = tree_state(root);
        let repo = vcs::primary_checkout(root).unwrap_or_else(|_| root.to_path_buf());
        Self {
            task: task.to_string(),
            args: args.clone(),
            repo: std::fs::canonicalize(&repo).unwrap_or(repo),
            worktree: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            head,
            clean,
            committed_definition,
            env,
            env_file,
            started_at: devkit_common::harness_log::now_rfc3339(),
        }
    }

    /// The finished run, re-reading the tree so a run that changed it, or
    /// whose head moved under it, is not clean.
    pub fn finish(self, exit_code: Option<i32>, tail: &Tail, steps: Vec<Step>) -> Run {
        let (head_after, clean_after) = tree_state(&self.worktree);
        Run {
            clean: self.clean
                && self.committed_definition
                && self.env.is_empty()
                && self.env_file.is_none()
                && clean_after
                && head_after == self.head,
            committed_definition: self.committed_definition,
            env: self.env,
            env_file: self.env_file,
            steps,
            task: self.task,
            args: self.args,
            repo: self.repo,
            worktree: self.worktree,
            head: self.head,
            exit_code,
            started_at: self.started_at,
            ended_at: devkit_common::harness_log::now_rfc3339(),
            tail: tail.lines(),
            path: None,
        }
    }
}

/// The last [`TAIL_LINES`] lines of the output streams devrun captured,
/// interleaved in the order they arrived.
#[derive(Debug, Default)]
pub struct Tail {
    inner: Mutex<TailInner>,
}

#[derive(Debug, Default)]
struct TailInner {
    lines: VecDeque<String>,
    captured: bool,
}

impl Tail {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn push(&self, bytes: &[u8]) {
        let bytes = &bytes[..bytes.len().min(MAX_LINE_BYTES)];
        let line = String::from_utf8_lossy(bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.lines.len() == TAIL_LINES {
            inner.lines.pop_front();
        }
        inner.lines.push_back(line);
    }

    fn mark_captured(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .captured = true;
    }

    /// The captured lines, or `None` when no stream was captured.
    pub fn lines(&self) -> Option<Vec<String>> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .captured
            .then(|| inner.lines.iter().cloned().collect())
    }

    /// Copy `from` to `to` byte for byte, keeping its lines in the tail.
    pub fn pump(&self, mut from: impl Read, mut to: impl Write) {
        self.mark_captured();
        let mut buf = [0u8; 8192];
        let mut partial = Vec::new();
        loop {
            let n = match from.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let _ = to.write_all(&buf[..n]);
            let _ = to.flush();
            partial.extend_from_slice(&buf[..n]);
            while let Some(i) = partial.iter().position(|&b| b == b'\n') {
                self.push(&partial[..i]);
                partial.drain(..=i);
            }
            if partial.len() > MAX_LINE_BYTES {
                self.push(&partial);
                partial.clear();
            }
        }
        if !partial.is_empty() {
            self.push(&partial);
        }
    }
}

/// A file a task's output stream is pointed at, followed by devrun while the
/// task runs. A writer the task left behind keeps writing to it unseen.
///
/// On Unix the file is unlinked as soon as both handles are open, so a devrun
/// killed mid-run leaves no file behind. Elsewhere an open file cannot be
/// removed, so it is removed once followed.
pub struct Capture {
    writer: std::fs::File,
    reader: std::fs::File,
    #[cfg(not(unix))]
    _named: tempfile::NamedTempFile,
}

/// How long a follower waits for more output when it has read all there is.
const FOLLOW_POLL: Duration = Duration::from_millis(20);

impl Capture {
    /// A fresh capture file, or `None` when the state directory cannot hold
    /// one: the task then inherits devrun's stream and nothing is captured.
    pub fn open() -> Option<Self> {
        let dir = paths::task_output_dir();
        std::fs::create_dir_all(&dir).ok()?;
        let named = tempfile::NamedTempFile::new_in(&dir).ok()?;
        // Separate opens, so the reader keeps its own offset.
        let writer = named.as_file().try_clone().ok()?;
        let reader = named.reopen().ok()?;
        #[cfg(unix)]
        named.close().ok()?;
        Some(Self {
            writer,
            reader,
            #[cfg(not(unix))]
            _named: named,
        })
    }

    /// A handle for the task's stream to write through.
    pub fn writer(&self) -> Result<std::fs::File> {
        self.writer
            .try_clone()
            .context("sharing the output capture file")
    }

    /// Echo the file to the stream `to` makes, keeping its lines in `tail`,
    /// until `exited` is set and the file is read to its end.
    pub fn follow<W: Write>(
        self,
        tail: &Arc<Tail>,
        exited: &Arc<AtomicBool>,
        to: impl FnOnce() -> W + Send + 'static,
    ) -> JoinHandle<()> {
        let (tail, exited) = (Arc::clone(tail), Arc::clone(exited));
        std::thread::spawn(move || {
            let Self { reader, .. } = self;
            tail.pump(Follow { reader, exited }, to());
        })
    }
}

/// A reader that waits at the end of a growing file until the writer is done.
struct Follow {
    reader: std::fs::File,
    exited: Arc<AtomicBool>,
}

impl Read for Follow {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            // Read the flag before the file, so an empty read after the task
            // exited means everything it wrote has been seen.
            let done = self.exited.load(Ordering::Acquire);
            match self.reader.read(buf)? {
                0 if !done => std::thread::sleep(FOLLOW_POLL),
                n => return Ok(n),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(task: &str, head: &str, clean: bool, exit_code: Option<i32>) -> Run {
        Run {
            task: task.into(),
            args: BTreeMap::new(),
            repo: "/r".into(),
            worktree: "/r".into(),
            head: Some(head.into()),
            clean,
            committed_definition: clean,
            env: Vec::new(),
            env_file: None,
            steps: Vec::new(),
            exit_code,
            started_at: "2026-01-01T00:00:00.000Z".into(),
            ended_at: "2026-01-01T00:00:01.000Z".into(),
            tail: None,
            path: None,
        }
    }

    #[test]
    fn only_a_clean_zero_exit_at_the_exact_sha_proves_a_pass() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert!(run("test", sha, true, Some(0)).proves("test", sha));
        assert!(!run("test", sha, false, Some(0)).proves("test", sha));
        assert!(!run("test", sha, true, Some(1)).proves("test", sha));
        assert!(!run("test", sha, true, None).proves("test", sha));
        assert!(!run("lint", sha, true, Some(0)).proves("test", sha));
        assert!(!run("test", sha, true, Some(0)).proves("test", &sha[..12]));
    }

    #[test]
    fn the_tail_keeps_the_last_lines_and_cuts_long_ones() {
        let tail = Tail::default();
        let mut out = Vec::new();
        let input: String = (0..TAIL_LINES + 5).map(|i| format!("line {i}\n")).collect();
        tail.pump(
            format!("{input}{}", "x".repeat(MAX_LINE_BYTES * 2)).as_bytes(),
            &mut out,
        );
        assert_eq!(
            out.len(),
            input.len() + MAX_LINE_BYTES * 2,
            "output passes through whole"
        );
        let lines = tail.lines().unwrap();
        assert_eq!(lines.len(), TAIL_LINES);
        assert!(
            lines.contains(&format!("line {}", TAIL_LINES + 4)),
            "{lines:?}"
        );
        assert!(!lines.contains(&"line 0".to_string()));
        assert!(lines.iter().all(|l| l.len() <= MAX_LINE_BYTES));
        assert!(lines.last().unwrap().starts_with('x'));
    }

    #[test]
    fn an_uncaptured_tail_is_none() {
        assert_eq!(Tail::default().lines(), None);
    }

    #[test]
    fn contains_sees_through_relative_and_dotted_spellings() {
        let tmp = tempfile::tempdir().unwrap();
        let ledger = Ledger::at(tmp.path().join("state/runs"));
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        assert!(ledger.contains(Path::new("../state/runs/x.json"), &cwd));
        assert!(ledger.contains(&tmp.path().join("repo/../state/./runs/new/x.json"), &cwd));
        assert!(!ledger.contains(Path::new("runs/x.json"), &cwd));
        assert!(!ledger.contains(&tmp.path().join("state/runs-other/x.json"), &cwd));
    }

    #[cfg(unix)]
    #[test]
    fn contains_follows_a_symlink_into_the_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state/runs");
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = tmp.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        std::os::unix::fs::symlink(&dir, cwd.join("link")).unwrap();
        assert!(Ledger::at(&dir).contains(Path::new("link/x.json"), &cwd));
    }
}
