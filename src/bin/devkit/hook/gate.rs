//! The write gate: every write `pre-tool-use` judges, from a structured edit
//! or a shell command, is claimed here.
//!
//! The gate fails closed. Once [`enabled`] says it applies, a registry error,
//! a registry that misses the deadline, and a panic each deny. A harness
//! allows the call when a hook times out or exits non-zero, so neither may be
//! how the gate answers: the deadline is in-process, and [`guarded`] turns a
//! panic into a denial.

use std::{
    cell::Cell,
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};

use devkit_common::vcs::Checkout;
use devkit_config::PolicyAction;
use devkit_locks::{
    Live, Registry, WriteResolver,
    model::{Conflict, WriteDecision},
};
use pabal::AnyHarness;

use super::{payload, print_envelope, writes};

// A panic denies through `catch_unwind`, which catches nothing under an
// aborting panic strategy. Nothing else ties the compile profile to this
// file, so the dependency is stated where it is relied on.
#[cfg(panic = "abort")]
compile_error!(
    "the write gate denies a panic through catch_unwind; the release profile must unwind"
);

pub const PREFIX: &str = "devkit write-harness:";
/// Far longer than a healthy registry takes, and below the manifest timeout,
/// which allows the call when it fires. Keep the manifest at this plus the
/// record deadline plus a second or two of process startup.
const DEADLINE: Duration = Duration::from_secs(2);
const NOTE: &str = "write-harness";
/// The backstop for a session killed before its release hook runs.
const TTL_SECS: u64 = 1800;
const PANIC_REASON: &str =
    "devkit write-harness: internal failure while evaluating a write (fail-closed)";

/// Whether writes from `harness` at `cwd` go through the gate.
pub fn enabled(harness: AnyHarness, checkout: &Checkout, cwd: &Path) -> bool {
    let claims = match harness {
        AnyHarness::ClaudeCode | AnyHarness::Codex => true,
        // Cursor's Tab completions edit through a post-only hook, so its
        // claims would cover a fraction of what it writes.
        AnyHarness::Cursor => false,
        // Antigravity sends no session-end event to release claims on.
        AnyHarness::Antigravity => false,
    };
    claims && devkit_common::harness::writes_enabled(checkout, cwd)
}

/// What a write asks of the registry. A relative path is resolved against the
/// session's working directory, never the hook process's.
#[derive(Debug, Default)]
pub struct Claims {
    /// Write targets, in order, without repeats.
    pub paths: Vec<String>,
    /// Directories to claim whole, each bounding a write that reaches paths
    /// under it no one can list. In order, without repeats or one another's
    /// subdirectories.
    pub trees: Vec<String>,
    /// Directories to check without claiming, in order, without repeats.
    pub scopes: Vec<ScopeCheck>,
}

/// A directory the registry is asked about, and which question to ask of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeCheck {
    /// A writer rewrites an unenumerated set of files under this directory, so
    /// any claim overlapping it conflicts.
    Tree { dir: String, whole_checkout: bool },
    /// A name was created fresh under this directory. Nobody else can produce
    /// that name, so only a claim covering the directory's children conflicts.
    Fresh { dir: String },
    /// Only this path's metadata changes, so a claim on it or on a directory
    /// above it conflicts, and a claim below it does not.
    Covering { path: String },
}

impl Claims {
    pub fn paths(paths: Vec<String>) -> Self {
        Self {
            paths,
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.trees.is_empty() && self.scopes.is_empty()
    }

    pub fn path(&mut self, path: &str) {
        if !self.paths.iter().any(|p| p == path) {
            self.paths.push(path.to_string());
        }
    }

    pub fn scope(&mut self, check: ScopeCheck) {
        if !self.scopes.contains(&check) {
            self.scopes.push(check);
        }
    }

    pub fn tree(&mut self, dir: &str) {
        let under = |inner: &str, outer: &str| {
            inner == outer
                || inner
                    .strip_prefix(outer)
                    .is_some_and(|rest| rest.starts_with('/'))
        };
        if self.trees.iter().any(|t| under(dir, t)) {
            return;
        }
        self.trees.retain(|t| !under(t, dir));
        self.trees.push(dir.to_string());
    }

    fn resolved_against(&self, cwd: &Path) -> Self {
        let abs = |p: &str| {
            let path = Path::new(p);
            if path.is_absolute() {
                p.to_string()
            } else {
                cwd.join(path).to_string_lossy().into_owned()
            }
        };
        Self {
            paths: self.paths.iter().map(|p| abs(p)).collect(),
            trees: self.trees.iter().map(|d| abs(d)).collect(),
            scopes: self
                .scopes
                .iter()
                .map(|check| match check {
                    ScopeCheck::Tree {
                        dir,
                        whole_checkout,
                    } => ScopeCheck::Tree {
                        dir: abs(dir),
                        whole_checkout: *whole_checkout,
                    },
                    ScopeCheck::Fresh { dir } => ScopeCheck::Fresh { dir: abs(dir) },
                    ScopeCheck::Covering { path } => ScopeCheck::Covering { path: abs(path) },
                })
                .collect(),
        }
    }
}

/// What the gate found: blocks deny the call, warnings ride along with it.
#[derive(Debug, Default)]
pub struct WriteVerdict {
    pub blocks: Vec<String>,
    pub warnings: Vec<String>,
}

impl WriteVerdict {
    fn deny(reason: String) -> Self {
        Self {
            blocks: vec![reason],
            warnings: Vec::new(),
        }
    }
}

/// The gate over one registry. [`WriteGate::live`] is the registry every
/// session shares.
pub struct WriteGate<R = Live> {
    registry: Arc<R>,
    deadline: Duration,
}

impl WriteGate {
    pub fn live() -> Self {
        Self::new(Live, DEADLINE)
    }
}

impl<R: Registry + Send + Sync + 'static> WriteGate<R> {
    pub fn new(registry: R, deadline: Duration) -> Self {
        Self {
            registry: Arc::new(registry),
            deadline,
        }
    }

    /// Check every scope, then claim every path and every tree, for `holder`.
    /// A scope conflict stops before any claim; a claim conflict leaves the
    /// claims already made to the normal release lifecycle. A tree no claim
    /// can stand for is an unresolved write, which `unresolved` decides.
    pub fn decide(
        &self,
        claims: &Claims,
        holder: &str,
        checkout: &Checkout,
        cwd: &Path,
        unresolved: PolicyAction,
    ) -> WriteVerdict {
        let claims = claims.resolved_against(cwd);
        let holder = holder.to_string();
        let resolver = WriteResolver::with_registry(checkout.clone(), Arc::clone(&self.registry));
        match with_deadline(self.deadline, move || {
            enforce(resolver, &claims, &holder, unresolved)
        }) {
            Ok(Ok(verdict)) => verdict,
            Ok(Err(e)) => {
                WriteVerdict::deny(format!("{PREFIX} registry error (fail-closed): {e:#}"))
            }
            Err(StageError::Panicked) => WriteVerdict::deny(format!(
                "{PREFIX} internal failure while claiming write targets (fail-closed)"
            )),
            Err(StageError::TimedOut) => WriteVerdict::deny(format!(
                "{PREFIX} the lock registry did not answer within {:?} (fail-closed). Retry; if it \
                 persists, check `lockm status` and `devkit doctor`.",
                self.deadline
            )),
        }
    }
}

fn enforce<R: Registry>(
    mut resolver: WriteResolver<R>,
    claims: &Claims,
    holder: &str,
    unresolved: PolicyAction,
) -> anyhow::Result<WriteVerdict> {
    let mut verdict = WriteVerdict::default();
    let mut conflicts = Vec::new();
    for check in &claims.scopes {
        conflicts.extend(match check {
            ScopeCheck::Tree {
                dir,
                whole_checkout,
            } => resolver.check_scope(dir, *whole_checkout, holder)?,
            ScopeCheck::Fresh { dir } => resolver.check_covering(dir, holder)?,
            ScopeCheck::Covering { path } => resolver.check_covering(path, holder)?,
        });
    }
    if !conflicts.is_empty() {
        verdict.blocks.push(conflict_message(&conflicts));
        return Ok(verdict);
    }
    for path in &claims.paths {
        if let WriteDecision::Denied(c) =
            resolver.decide_write(path, holder, Some(NOTE), TTL_SECS)?
        {
            conflicts.extend(c);
        }
    }
    for dir in &claims.trees {
        match resolver.claim_tree(dir, holder, Some(NOTE), TTL_SECS)? {
            Some(WriteDecision::Denied(c)) => conflicts.extend(c),
            Some(WriteDecision::Acquired | WriteDecision::AllowedByOwnership) => {}
            None => writes::file_finding(
                unresolved,
                format!(
                    "{PREFIX} a write reaches paths under `{dir}` that could not be listed, and \
                     devkit claims such a set only in a directory below a checkout root. {}",
                    writes::unresolved_fix(unresolved)
                ),
                &mut verdict.blocks,
                &mut verdict.warnings,
            ),
        }
    }
    if !conflicts.is_empty() {
        verdict.blocks.push(conflict_message(&conflicts));
    }
    Ok(verdict)
}

/// The deny reason naming every holder in the way.
fn conflict_message(conflicts: &[Conflict]) -> String {
    let who = conflicts
        .iter()
        .map(|c| format!("{} (held by {})", c.path, c.held_by))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{PREFIX} {who} is locked by another agent; edit a different file or wait for it to finish"
    )
}

#[derive(Debug)]
enum StageError {
    TimedOut,
    Panicked,
}

/// Run `work` on its own thread and wait at most `deadline`. On a timeout the
/// thread is left running; the hook process exits once its verdict is out,
/// which ends it.
fn with_deadline<T: Send + 'static>(
    deadline: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, StageError> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    match rx.recv_timeout(deadline) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(StageError::TimedOut),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(StageError::Panicked),
    }
}

/// Armed once the gate applies to a call, so that a panic denies it, and
/// disarmed once the call's verdict is on stdout, since a second envelope
/// after it would make stdout unparseable and lose the verdict.
pub struct Armed(Cell<Option<AnyHarness>>);

impl Armed {
    pub fn arm(&self, harness: AnyHarness) {
        self.0.set(Some(harness));
    }

    pub fn disarm(&self) {
        self.0.set(None);
    }
}

/// A panic [`guarded`] caught, and whether it denied the call for it.
pub struct Panicked {
    pub denied: bool,
}

/// Run a `pre-tool-use` path so that a panic while the gate is armed prints a
/// denial. Any other panic is the caller's to answer.
pub fn guarded<T>(work: impl FnOnce(&Armed) -> T) -> Result<T, Panicked> {
    let armed = Armed(Cell::new(None));
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&armed))).map_err(|_| {
        let armed = armed.0.get();
        if let Some(harness) = armed {
            print_envelope(&payload::deny(harness, PANIC_REASON));
        }
        Panicked {
            denied: armed.is_some(),
        }
    })
}

/// A git project with a registry of its own, for running either path's write
/// stage in-process.
#[cfg(test)]
pub(super) mod fixture {
    use std::{
        path::Path,
        sync::{Arc, Mutex},
    };

    use devkit_common::vcs::Checkout;
    use devkit_locks::{
        model::Data,
        store::{self, MemoryStore},
    };

    use super::{DEADLINE, WriteGate};

    pub struct Project {
        dir: tempfile::TempDir,
        registry_dir: tempfile::TempDir,
        state: Arc<Mutex<Data>>,
        root: String,
    }

    impl Project {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            devkit_git::Git::fixture(dir.path())
                .args(["init", "-q", "-b", "main"])
                .output()
                .unwrap();
            let root = devkit_locks::find_root_from(dir.path())
                .to_string_lossy()
                .into_owned();
            Self {
                dir,
                registry_dir: tempfile::tempdir().unwrap(),
                state: Arc::default(),
                root,
            }
        }

        pub fn path(&self) -> &Path {
            self.dir.path()
        }

        pub fn checkout(&self) -> Checkout {
            Checkout::at(self.path())
        }

        fn store(&self) -> MemoryStore {
            MemoryStore::new(
                Arc::clone(&self.state),
                self.registry_dir.path().join("locks.json"),
            )
        }

        pub fn gate(&self) -> WriteGate<MemoryStore> {
            WriteGate::new(self.store(), DEADLINE)
        }

        /// Claim `path`, relative to the project root, the way another
        /// session's `lockm acquire` would have.
        pub fn hold(&self, holder: &str, path: &str) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let out = store::acquire_with(
                &self.store(),
                &self.root,
                holder,
                &[path.to_string()],
                None,
                None,
                1800,
                now,
            )
            .unwrap();
            assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
        }

        /// `(root-relative path, holder)` for every row, sorted.
        pub fn rows(&self) -> Vec<(String, String)> {
            let mut rows: Vec<_> = self
                .state
                .lock()
                .unwrap()
                .locks
                .values()
                .map(|row| (row.path.clone(), row.holder.clone()))
                .collect();
            rows.sort();
            rows
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_denial_names_the_path_and_its_holder() {
        let reason = conflict_message(&[Conflict {
            path: "src/a.rs".into(),
            held_by: "S/b2".into(),
            age_secs: 5,
            note: None,
        }]);
        assert!(reason.contains("src/a.rs (held by S/b2)"), "{reason}");
    }

    #[test]
    fn a_relative_claim_resolves_against_the_sessions_cwd() {
        let mut claims = Claims::paths(vec!["src/a.rs".into(), "/tmp/b.rs".into()]);
        claims.tree("gen");
        claims.scope(ScopeCheck::Fresh { dir: "out".into() });
        let got = claims.resolved_against(Path::new("/repo"));
        // Compared as paths: `join` writes the platform's separator, and
        // `Path` equality treats both separators as one on Windows.
        let expected = |p: &str| Path::new("/repo").join(p);
        assert_eq!(Path::new(&got.paths[0]), expected("src/a.rs"));
        assert_eq!(got.paths[1], "/tmp/b.rs");
        assert_eq!(Path::new(&got.trees[0]), expected("gen"));
        let ScopeCheck::Fresh { dir } = &got.scopes[0] else {
            panic!("{:?}", got.scopes)
        };
        assert_eq!(Path::new(dir), expected("out"));
    }

    #[test]
    fn the_deadline_returns_before_slow_work_finishes() {
        let start = Instant::now();
        let r = with_deadline(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_secs(5))
        });
        assert!(matches!(r, Err(StageError::TimedOut)));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(matches!(with_deadline(Duration::from_secs(5), || 7), Ok(7)));
        assert!(matches!(
            with_deadline(Duration::from_secs(5), || -> u8 { panic!("boom") }),
            Err(StageError::Panicked)
        ));
    }

    #[test]
    fn a_panic_denies_only_while_the_gate_is_armed() {
        let before = guarded(|_| -> () { panic!("before the gate") });
        assert!(matches!(before, Err(Panicked { denied: false })));
        let after = guarded(|armed| -> () {
            armed.arm(AnyHarness::ClaudeCode);
            panic!("inside the gate")
        });
        assert!(matches!(after, Err(Panicked { denied: true })));
        let answered = guarded(|armed| -> () {
            armed.arm(AnyHarness::ClaudeCode);
            armed.disarm();
            panic!("after the verdict")
        });
        assert!(matches!(answered, Err(Panicked { denied: false })));
        assert!(matches!(guarded(|_| 7), Ok(7)));
    }
}
