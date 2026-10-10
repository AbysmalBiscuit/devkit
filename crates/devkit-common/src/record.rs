use std::path::Path;

use anyhow::{Context, Result, bail};
use devkit_config::IssueEvent;
use serde::{Deserialize, Serialize};

/// Per-worktree record written by `workspace setup`, carrying the setup-time
/// context that is otherwise unavailable later: the authoritative issue id, and
/// the slug, apps and summary path that `pr review` and `workspace end` need.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueRecord {
    pub issue: String,
    pub slug: String,
    /// The branch `workspace setup` bound to the issue: the one it created, or
    /// with `--here` the one already checked out. Absent on records written
    /// before it existed and on those `pr checkout` writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub apps: Vec<String>,
    /// The summary file `workspace setup --summary` wrote, so `workspace end`
    /// removes the file that actually exists rather than re-deriving a path
    /// from a template that may have changed since. Absent on records
    /// written before the summary existed, and on setups that asked for
    /// none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The pull request this worktree's work belongs to, written by
    /// `pr checkout` and by `pr review request` whenever either resolves
    /// one. The locator identifies both repository and number, so a PR outside
    /// `pr_repo` is still findable. Absent on records written before it existed
    /// and on a `workspace setup` worktree whose PR does not exist yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<crate::forge::PrLocator>,
    /// The baseline this worktree compares against, written whenever `devrun
    /// up` resolves one. Absent on records written before baselines were
    /// per-worktree, and on a worktree that has never run one. The path is
    /// stored alongside the sha because it is what `workspace end` deletes: a
    /// sha alone would resolve somewhere new the moment `baseline_dir`
    /// changed, orphaning every existing baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselinePin>,
    /// Which command created the worktree. Absent on records written before
    /// it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<RecordOrigin>,
    /// The `[ticket.events]` already claimed for this worktree, each fired at
    /// most once. `None` on a record written before events existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<IssueEvent>>,
}

/// The command that created a worktree and wrote its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordOrigin {
    /// `workspace setup`: the worktree where the issue's work happens.
    Setup,
    /// `pr checkout`: a reviewer's worktree on an existing PR.
    Checkout,
}

/// One worktree's pin on a baseline: which sha it was built at, and where it
/// lives, so `workspace end` can remove that exact directory without
/// re-deriving it from a `baseline_dir` that may have since changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselinePin {
    pub sha: String,
    pub path: String,
}

/// The result of reading a worktree's record. `Absent` and `Unusable` are both
/// "no record to trust", but only `Absent` means this worktree references no
/// baseline — `Unusable` means a record exists and cannot be confirmed, which
/// must not be counted as "references nothing" by a baseline referencer scan.
pub enum RecordState {
    Ok(Box<IssueRecord>),
    Unusable,
    Absent,
}

/// `<worktree>/.devkit/issue.toml`, the one spelling of the record path, so a
/// caller reporting a record it refused to touch names the same file the
/// readers and the writer use.
pub fn path(worktree: &Path) -> std::path::PathBuf {
    worktree.join(".devkit").join("issue.toml")
}

/// Write the record under `<worktree>/.devkit/`, creating the directory.
///
/// Writes to a temporary sibling and renames into place: other processes scan
/// this file to count a baseline's referencers, and a rename is atomic where a
/// plain write is not, so a concurrent reader never observes a half-written
/// file.
pub fn write(worktree: &Path, rec: &IssueRecord) -> Result<()> {
    let p = path(worktree);
    let dir = p.parent().expect("path has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::gitignore::write_self_ignore(dir);
    let body = toml::to_string(rec).context("serializing issue record")?;
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("renaming into {}", p.display()))
}

/// Read, change and write the record under an exclusive lock on
/// `<worktree>/.devkit/issue.lock`, so concurrent updaters each see the
/// others' writes. `f` gets `None` when the record is absent or unreadable,
/// and the record is written when `f` leaves it `Some` and changed.
///
/// A record that exists and cannot be read is never overwritten: replacing it
/// would discard the issue id, slug, apps, summary path and PR it still holds.
pub fn update<T>(worktree: &Path, f: impl FnOnce(&mut Option<IssueRecord>) -> T) -> Result<T> {
    let p = path(worktree);
    let dir = p.parent().expect("path has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::gitignore::write_self_ignore(dir);
    crate::store::with_file_lock(&dir.join("issue.lock"), || {
        let (before, unreadable) = match read_state(worktree) {
            RecordState::Ok(rec) => (Some(*rec), false),
            RecordState::Unusable => (None, true),
            RecordState::Absent => (None, false),
        };
        let mut rec = before.clone();
        let out = f(&mut rec);
        if let Some(after) = rec.filter(|r| Some(r) != before.as_ref()) {
            if unreadable {
                bail!(
                    "{} can be neither read nor ruled out; repair or remove it",
                    p.display()
                );
            }
            write(worktree, &after)?;
        }
        Ok(out)
    })
}

/// Claim `event` for the worktree: add it to the record's `events` and return
/// `true`, or return `false` when it was already claimed or there is no
/// readable record to hold the claim.
pub fn claim(worktree: &Path, event: IssueEvent) -> Result<bool> {
    if !path(worktree).exists() {
        return Ok(false);
    }
    update(worktree, |rec| rec.as_mut().is_some_and(|r| r.claim(event)))
}

impl IssueRecord {
    /// The tracker issue this record names, `None` for a worktree with no
    /// issue or an unknown one.
    pub fn tracker_issue(&self) -> Option<String> {
        self.issue
            .parse::<crate::worktree::IssueId>()
            .ok()?
            .tracker()
            .map(String::from)
    }

    /// Whether the record still binds a checkout on `branch`: it names that
    /// branch, or none at all, as a `pr checkout` record and one written
    /// before branches were recorded do. A checkout switched to another
    /// branch carries other work, which the issue must not be credited with.
    pub fn binds(&self, branch: &str) -> bool {
        self.branch.as_deref().is_none_or(|b| b == branch)
    }

    /// Add `event` to `events`, returning `false` when it was already there.
    pub fn claim(&mut self, event: IssueEvent) -> bool {
        let events = self.events.get_or_insert_with(Vec::new);
        let fresh = !events.contains(&event);
        if fresh {
            events.push(event);
        }
        fresh
    }
}

/// Read the record from `<worktree>/.devkit/issue.toml`, or `None` if absent or
/// unparseable.
pub fn read(worktree: &Path) -> Option<IssueRecord> {
    let body = std::fs::read_to_string(path(worktree)).ok()?;
    toml::from_str(&body).ok()
}

/// `read` for a checkout on `branch`: `None` also when the record was bound
/// to another branch. Every reader that acts for the checked-out work, rather
/// than for the worktree as a directory, reads through this.
pub fn read_on(worktree: &Path, branch: &str) -> Option<IssueRecord> {
    read(worktree).filter(|r| r.binds(branch))
}

/// `read`, but distinguishing a record that is absent from one that exists and
/// cannot be trusted. A baseline referencer count needs the distinction:
/// counting an unreadable or corrupt record as "no reference" would let a
/// scan delete a baseline still in use by a worktree whose record it could
/// not confirm.
pub fn read_state(worktree: &Path) -> RecordState {
    match std::fs::read_to_string(path(worktree)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RecordState::Absent,
        Err(_) => RecordState::Unusable,
        Ok(body) => match toml::from_str(&body) {
            Ok(r) => RecordState::Ok(Box::new(r)),
            Err(_) => RecordState::Unusable,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let rec = IssueRecord {
            issue: "ABC-123".into(),
            slug: "fix-login".into(),
            apps: vec!["web".into(), "api".into()],
            summary: Some("/w/ISSUE_SUMMARY_ABC-123.md".into()),
            pr: Some(crate::forge::PrLocator {
                repo: Some("acme/web".into()),
                number: 42,
            }),
            baseline: Some(BaselinePin {
                sha: "d13d90b724bf".into(),
                path: "/b/d13d".into(),
            }),
            ..Default::default()
        };
        write(dir.path(), &rec).unwrap();
        assert_eq!(read(dir.path()), Some(rec));
    }

    #[test]
    fn a_record_written_before_summaries_existed_still_reads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            dir.path().join(".devkit").join("issue.toml"),
            "issue = \"ABC-1\"\nslug = \"fix\"\napps = []\n",
        )
        .unwrap();
        let got = read(dir.path()).expect("legacy record parses");
        assert_eq!(got.issue, "ABC-1");
        assert!(got.summary.is_none());
        assert!(got.pr.is_none());
        assert!(got.baseline.is_none());
    }

    #[test]
    fn read_missing_is_none() {
        let scratch = tempfile::tempdir().unwrap();
        assert_eq!(read(&scratch.path().join("absent")), None);
    }

    #[test]
    fn an_old_record_with_no_pr_field_still_deserializes() {
        let rec: IssueRecord = toml::from_str("issue = 'ENG-1'\nslug = 'x'\napps = []\n").unwrap();
        assert_eq!(rec.pr, None);
    }

    #[test]
    fn a_record_without_a_baseline_still_reads() {
        let rec: IssueRecord = toml::from_str("issue = 'ENG-1'\nslug = 'x'\napps = []\n").unwrap();
        assert_eq!(rec.baseline, None);
    }

    #[test]
    fn a_baseline_pin_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let rec = IssueRecord {
            issue: "ENG-1".into(),
            slug: "x".into(),
            apps: vec![],
            summary: None,
            pr: None,
            baseline: Some(BaselinePin {
                sha: "d13d90b724bf".into(),
                path: "/b/d13d".into(),
            }),
            ..Default::default()
        };
        write(dir.path(), &rec).unwrap();
        assert_eq!(read(dir.path()), Some(rec));
    }

    #[test]
    fn a_corrupt_record_is_unusable_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(read_state(dir.path()), RecordState::Absent));
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(dir.path().join(".devkit").join("issue.toml"), "issue = ").unwrap();
        assert!(matches!(read_state(dir.path()), RecordState::Unusable));
    }

    /// An unreadable record must never read as an absent one: a
    /// referencer-count scan treats `Absent` as "this worktree references
    /// no baseline", and a worktree whose record exists but could not be
    /// confirmed is the one case where that answer is actively dangerous. A
    /// directory in place of the file is the portable way to force a
    /// non-`NotFound` I/O error — the concrete kind (`IsADirectory` on
    /// Linux, `PermissionDenied` on Windows) varies by platform, so the
    /// assertion only rules out `NotFound`.
    #[test]
    fn an_unreadable_record_is_unusable_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        std::fs::create_dir_all(&p).unwrap();
        let err = std::fs::read_to_string(&p).unwrap_err();
        assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
        assert!(matches!(read_state(dir.path()), RecordState::Unusable));
    }

    #[test]
    fn a_record_without_events_reads_as_legacy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(
            path(dir.path()),
            "issue = \"65\"\nslug = \"x\"\napps = []\n",
        )
        .unwrap();
        let rec = read(dir.path()).unwrap();
        assert_eq!((rec.events, rec.origin), (None, None));
    }

    #[test]
    fn events_and_origin_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let rec = IssueRecord {
            issue: "65".into(),
            origin: Some(RecordOrigin::Checkout),
            events: Some(vec![IssueEvent::PrOpen]),
            ..Default::default()
        };
        write(dir.path(), &rec).unwrap();
        let body = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(
            body.contains("origin = \"checkout\"") && body.contains("\"pr_open\""),
            "{body}"
        );
        assert_eq!(read(dir.path()), Some(rec));
    }

    #[test]
    fn claim_adds_an_event_once() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &IssueRecord {
            issue: "65".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(claim(dir.path(), IssueEvent::Start).unwrap());
        assert!(!claim(dir.path(), IssueEvent::Start).unwrap());
        assert_eq!(
            read(dir.path()).unwrap().events,
            Some(vec![IssueEvent::Start])
        );
    }

    #[test]
    fn concurrent_claims_have_one_winner_and_keep_a_concurrent_pin() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &IssueRecord {
            issue: "65".into(),
            ..Default::default()
        })
        .unwrap();
        let wins: usize = std::thread::scope(|s| {
            let claims: Vec<_> = (0..8)
                .map(|_| s.spawn(|| claim(dir.path(), IssueEvent::Start).unwrap()))
                .collect();
            let pin = s.spawn(|| {
                update(dir.path(), |r| {
                    r.as_mut().unwrap().baseline = Some(BaselinePin {
                        sha: "abc".into(),
                        path: "/b".into(),
                    });
                })
                .unwrap()
            });
            pin.join().unwrap();
            claims
                .into_iter()
                .map(|h| usize::from(h.join().unwrap()))
                .sum()
        });
        assert_eq!(wins, 1);
        let rec = read(dir.path()).unwrap();
        assert_eq!(rec.events, Some(vec![IssueEvent::Start]));
        assert_eq!(rec.baseline.unwrap().sha, "abc");
    }

    #[test]
    fn claiming_without_a_record_claims_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!claim(dir.path(), IssueEvent::Start).unwrap());
        assert!(read(dir.path()).is_none());
    }

    #[test]
    fn claiming_on_a_corrupt_record_claims_nothing_and_leaves_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".devkit")).unwrap();
        std::fs::write(path(dir.path()), "not toml").unwrap();
        assert!(!claim(dir.path(), IssueEvent::Start).unwrap());
        assert_eq!(
            std::fs::read_to_string(path(dir.path())).unwrap(),
            "not toml"
        );
    }

    /// The rename is what makes a write atomic: a reader sees either the whole
    /// previous record or the whole new one, never a truncated file. Asserting
    /// the temp file is gone is what pins the rename — a plain `fs::write`
    /// would also pass a round-trip assertion.
    #[test]
    fn a_write_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let rec = IssueRecord {
            issue: "ENG-1".into(),
            slug: "x".into(),
            apps: vec![],
            summary: None,
            pr: None,
            baseline: None,
            ..Default::default()
        };
        write(dir.path(), &rec).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join(".devkit"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
        assert_eq!(read(dir.path()), Some(rec));
    }
}
