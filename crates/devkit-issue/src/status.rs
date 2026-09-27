use std::{collections::HashMap, path::Path};

use anyhow::Result;
use devkit_common::{
    forge::{self, ForgeKind, HeadLookup, PrBrief, PrLocator, PrLookup, Repo},
    git::Git,
    tracker::{Resolved, State, StateKind, TrackerKind},
    vcs::{Changes, Vcs, VersionControl},
    worktree::{self, IssueId},
};
use serde::{Deserialize, Serialize};

/// A worktree's pull request, as the report knows it. The row carries the tag
/// rather than a state string plus two nullable fields, because an ambiguous
/// answer has candidates to name and a string has nowhere to put them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrStatus {
    /// No PR for this branch, from a transport that answered.
    None,
    Unique {
        number: u64,
        state: String,
        url: String,
        /// A draft's `state` is `OPEN`, so `state_label` cannot express this
        /// and deliberately does not try: consumers read `pr_state` and
        /// a changed string there breaks them.
        is_draft: bool,
    },
    /// Several PRs share this head branch. The verdict stays closed: `issue
    /// end` reads it to decide whether a worktree may be deleted, and a
    /// stranger's merged PR must not authorize that.
    Ambiguous {
        candidates: Vec<devkit_common::tracker::PrRef>,
    },
    /// The PR could not be identified: no token, a failed request, no forge
    /// found, or a recorded PR that no longer resolves.
    Unknown { reason: String },
    /// The project declared it has no forge, so there is no PR to wait for.
    /// What stands in for "merged" is the branch's commits being on a remote,
    /// since `issue end` deletes the branch.
    Untracked { pushed: bool },
}

impl PrStatus {
    pub fn number(&self) -> Option<u64> {
        match self {
            PrStatus::Unique { number, .. } => Some(*number),
            PrStatus::None
            | PrStatus::Ambiguous { .. }
            | PrStatus::Unknown { .. }
            | PrStatus::Untracked { .. } => None,
        }
    }

    pub fn url(&self) -> Option<&str> {
        match self {
            PrStatus::Unique { url, .. } => Some(url),
            PrStatus::None
            | PrStatus::Ambiguous { .. }
            | PrStatus::Unknown { .. }
            | PrStatus::Untracked { .. } => None,
        }
    }

    /// The `PR` column's state word, and the value the serialized `pr_state`
    /// field keeps carrying for consumers written against it.
    pub fn state_label(&self) -> &str {
        match self {
            PrStatus::Unique { state, .. } => state,
            PrStatus::None => "NO_PR",
            PrStatus::Ambiguous { .. } => "AMBIGUOUS",
            PrStatus::Unknown { .. } => "UNKNOWN",
            PrStatus::Untracked { .. } => "NO_FORGE",
        }
    }
}

/// One issue worktree with its PR + tracker state and the finished verdict.
#[derive(Debug, Clone)]
pub struct IssueWorktree {
    pub worktree: String,
    pub branch: String,
    pub issue_id: IssueId,
    pub dirty: bool,
    /// The PR, tagged. `pr_number`, `pr_state` and `pr_url` below are derived
    /// from it for the serialized shape consumers already read.
    pub pr: PrStatus,
    /// The tracker's state for this issue, absent when the tracker has no row
    /// for it or there is no tracker.
    pub state: Option<State>,
    pub finished: bool,
    pub reason_not_finished: Option<String>,
}

impl Serialize for IssueWorktree {
    /// Emits `pr` alongside the three legacy fields, so an MCP consumer reading
    /// `pr_state` keeps working while a new one can read the candidates.
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("IssueWorktree", 11)?;
        st.serialize_field("worktree", &self.worktree)?;
        st.serialize_field("branch", &self.branch)?;
        st.serialize_field("issue_id", &self.issue_id)?;
        st.serialize_field("dirty", &self.dirty)?;
        st.serialize_field("pr", &self.pr)?;
        st.serialize_field("pr_number", &self.pr.number())?;
        st.serialize_field("pr_state", self.pr.state_label())?;
        st.serialize_field("pr_url", &self.pr.url())?;
        st.serialize_field("state", &self.state)?;
        st.serialize_field("finished", &self.finished)?;
        st.serialize_field("reason_not_finished", &self.reason_not_finished)?;
        st.end()
    }
}

/// Which tracker produced this report and whether it could answer.
#[derive(Debug, Clone, Serialize)]
pub struct TrackerInfo {
    pub kind: TrackerKind,
    /// Configured and able to authenticate. False means the state column is
    /// blank because there is nothing to ask, not because the issue is unknown.
    pub ready: bool,
    /// Whether this tracker is the project's own answer rather than the
    /// stand-in devkit falls back to when nothing resolves. Only meaningful for
    /// `TrackerKind::None`, where it separates a project that has no tracker
    /// from devkit having found none.
    pub declared: bool,
    /// Why this tracker and not another. For an undeclared `TrackerKind::None`
    /// it is the only account of what devkit tried, and so the only thing that
    /// separates a project which named no tracker from one whose named tracker
    /// could not be built.
    pub reason: String,
    /// The tracker's issue URL built with an empty id, so
    /// `format!("{link_base}{id}")` is that issue's URL.
    pub link_base: Option<String>,
}

impl TrackerInfo {
    /// The report's tracker row for a resolved tracker. `link_base` starts
    /// absent: it costs a round trip, so callers fill it once they have asked.
    pub fn of(r: &Resolved) -> TrackerInfo {
        TrackerInfo {
            kind: r.tracker.kind(),
            ready: r.tracker.ready(),
            declared: r.declared,
            reason: r.reason.clone(),
            link_base: None,
        }
    }
}

/// The full status snapshot for a set of worktrees.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub worktrees: Vec<IssueWorktree>,
    pub finished_count: usize,
    pub tracker: TrackerInfo,
}

/// Local-only discovery: worktrees + dirty placeholders + issue ids. The slow
/// network fetches consume this. Fast — no `gh`/tracker.
pub struct Discovered {
    rows: Vec<IssueWorktree>,
    issue_ids: Vec<String>,
}

impl Discovered {
    /// Assemble a `Discovered` from pre-built rows, bypassing filesystem
    /// discovery. A seam for tests of callers that re-orchestrate the gather
    /// (e.g. the CLI's live table); real callers use [`discover`].
    #[doc(hidden)]
    pub fn from_parts(rows: Vec<IssueWorktree>, issue_ids: Vec<String>) -> Discovered {
        Discovered { rows, issue_ids }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn worktree_paths(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.worktree.clone()).collect()
    }

    pub fn issue_ids(&self) -> &[String] {
        &self.issue_ids
    }

    /// The discovered worktree rows, with dirty/PR/state still unfilled. Lets a
    /// single-worktree caller (`issue info`) pick its target without paying the
    /// per-worktree enrichment cost of a full gather.
    pub fn rows(&self) -> &[IssueWorktree] {
        &self.rows
    }
}

/// Each worktree branch's PR status, keyed by branch name.
pub struct Prs(HashMap<String, PrStatus>);

impl Prs {
    /// An empty PR list, built without any network call. Used when there are
    /// no worktrees and by tests of code that consumes a `Prs`.
    pub fn empty() -> Prs {
        Prs(HashMap::new())
    }

    /// Head-branch lookups, tagged as the report's `PrStatus`.
    pub fn from_lookups(lookups: HashMap<String, HeadLookup>) -> Prs {
        Prs(lookups
            .into_iter()
            .map(|(b, l)| (b, pr_status_of(&l)))
            .collect())
    }

    /// Overlay `row`'s branch's status onto `row.pr`, leaving the row
    /// untouched when the branch is detached or has no entry (an empty `Prs`
    /// never queried it). Same rule `assemble` applies per row, exposed so a
    /// single-worktree caller can enrich one row.
    pub fn apply(&self, row: &mut IssueWorktree) {
        if row.branch == "DETACHED" {
            return;
        }
        if let Some(status) = self.0.get(&row.branch) {
            row.pr = status.clone();
        }
    }
}

/// Tag a head-branch lookup as the report's `PrStatus`.
fn pr_status_of(lookup: &HeadLookup) -> PrStatus {
    match lookup {
        HeadLookup::Unique(pr) => PrStatus::Unique {
            number: pr.number,
            state: pr.state.clone(),
            url: pr.url.clone(),
            is_draft: pr.is_draft,
        },
        HeadLookup::NoMatch => PrStatus::None,
        HeadLookup::Ambiguous(candidates) => PrStatus::Ambiguous {
            candidates: candidates
                .iter()
                .map(|p| devkit_common::tracker::PrRef {
                    url: p.url.clone(),
                    number: p.number,
                })
                .collect(),
        },
        HeadLookup::Unavailable(reason) => PrStatus::Unknown {
            reason: reason.clone(),
        },
    }
}

/// Discover worktrees and their issue ids, filtered to `ids` when non-empty.
/// Rows carry `dirty = false` placeholders; the dirty check is a separate step
/// so callers can drive it with a progress bar.
pub fn discover(start: &str, ids: &[String]) -> Result<Discovered> {
    let (_main, others) = worktree::discover(start)?;
    let mut rows = Vec::new();
    for wt in &others {
        let iid = worktree::issue_id_of(&wt.path, &wt.branch);
        // An issue id is case-insensitive in every tracker that has one, and
        // the record holds whichever spelling the tracker was given.
        if !ids.is_empty() && !ids.iter().any(|w| w.eq_ignore_ascii_case(&iid.to_string())) {
            continue;
        }
        rows.push(IssueWorktree {
            worktree: wt.path.to_string_lossy().into_owned(),
            branch: wt.branch.clone(),
            issue_id: iid,
            dirty: false,
            pr: PrStatus::None,
            state: None,
            finished: false,
            reason_not_finished: None,
        });
    }
    let issue_ids = rows
        .iter()
        .filter_map(|r| r.issue_id.tracker().map(str::to_owned))
        .collect();
    Ok(Discovered { rows, issue_ids })
}

/// True when a worktree has uncommitted changes. A `git status` that fails to
/// run — a timeout, or a path git cannot read as a repository — is reported
/// dirty rather than clean, since a wrong "clean" here would let a caller
/// discard real work.
pub fn dirty_of(path: &str) -> bool {
    let path = Path::new(path);
    Vcs::at(path).dirty(path, Changes::All).unwrap_or(true)
}

/// `dirty_of` for many worktrees, run on a bounded thread pool with order
/// preserved. Each check is an independent, I/O-bound `git status` walk, so
/// fanning them across cores turns N serial walks into roughly one walk's
/// latency. The batch form of [`dirty_stream`]: results land in a slot per
/// input index, keeping the output aligned with `paths`.
pub fn dirty_many(paths: &[String]) -> Vec<bool> {
    let out = std::sync::Mutex::new(vec![false; paths.len()]);
    dirty_stream(paths, |i, d| out.lock().unwrap()[i] = d);
    out.into_inner().unwrap()
}

/// `dirty_of` for many worktrees, reporting each result as soon as it is
/// known. `report(i, dirty)` is invoked exactly once per input index, from
/// worker threads on a bounded pool over contiguous chunks; callers that
/// want the batch form should keep using [`dirty_many`].
pub fn dirty_stream(paths: &[String], report: impl Fn(usize, bool) + Send + Clone) {
    if paths.is_empty() {
        return;
    }
    let width = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
        .min(paths.len());
    let chunk = paths.len().div_ceil(width);
    std::thread::scope(|s| {
        for (ci, c) in paths.chunks(chunk).enumerate() {
            let report = report.clone();
            s.spawn(move || {
                for (j, p) in c.iter().enumerate() {
                    report(ci * chunk + j, dirty_of(p));
                }
            });
        }
    });
}

/// Whether the commit checked out at `path` is on some remote-tracking branch.
/// A `git` that cannot answer reads as not pushed, since this stands in for a
/// merged PR before `issue end` deletes the branch.
pub fn pushed_of(path: &str) -> bool {
    Git::at(Path::new(path))
        .args(["branch", "--remotes", "--contains", "HEAD"])
        .output()
        .is_ok_and(|out| !out.trim().is_empty())
}

/// Every branch marked `Unavailable` with the same `reason`: the whole batch
/// request could not be made at all (no token, transport failure).
fn unavailable_all(branches: &[String], reason: &str) -> HashMap<String, HeadLookup> {
    branches
        .iter()
        .map(|b| (b.clone(), HeadLookup::Unavailable(reason.to_string())))
        .collect()
}

/// Split worktree branches into ones a worktree's record already binds to a
/// PR (paired with that locator) and the rest, to be looked up by head branch
/// name in one batch. A bound branch never reaches the batch: the record is
/// authoritative over branch matching in any repository, so a superseded PR
/// sharing the same head branch can never win by riding along in that batch.
fn partition_by_record(
    rows: &[IssueWorktree],
    recorded: impl Fn(&str) -> Option<PrLocator>,
) -> (Vec<(String, PrLocator)>, Vec<String>) {
    let mut bound = Vec::new();
    let mut branches = Vec::new();
    for row in rows.iter().filter(|r| r.branch != "DETACHED") {
        match recorded(&row.worktree) {
            Some(loc) => bound.push((row.branch.clone(), loc)),
            None => branches.push(row.branch.clone()),
        }
    }
    (bound, branches)
}

/// The outcome of resolving one recorded locator into the shape a head lookup
/// already has a variant for: `Unavailable` covers both a transport failure
/// and a PR that no longer resolves, since neither may fall back to branch
/// matching and both close the finished verdict the same way.
fn recorded_result(found: Result<Option<PrBrief>>, n: u64) -> HeadLookup {
    match found {
        Ok(Some(pr)) => HeadLookup::Unique(pr),
        Ok(None) => HeadLookup::Unavailable(format!("recorded PR #{n} no longer resolves")),
        Err(e) => HeadLookup::Unavailable(format!("{e:#}")),
    }
}

/// Pair each recorded branch with its batched answer. An answer list shorter
/// than the targets leaves those branches `Unavailable` rather than unkeyed: a
/// branch with no entry at all reads as "no PR", which opens the finished
/// verdict instead of closing it.
fn recorded_answers(
    pending: Vec<(String, u64)>,
    answers: Vec<PrLookup>,
) -> HashMap<String, HeadLookup> {
    let mut answers: Vec<Option<PrLookup>> = answers.into_iter().map(Some).collect();
    answers.resize_with(pending.len(), || None);
    pending
        .into_iter()
        .zip(answers)
        .map(|((branch, n), answer)| {
            let found = answer.unwrap_or_else(|| {
                Err(anyhow::anyhow!(
                    "the batched lookup did not answer for #{n}"
                ))
            });
            (branch, recorded_result(found, n))
        })
        .collect()
}

/// Every recorded locator's exact PR, in one batched read, keyed by the
/// worktree branch each was recorded for. A locator naming no repository means
/// `default_repo`; one whose slug will not validate is `Unavailable` on its own
/// without keeping the rest of the batch from being asked.
fn recorded_lookups(
    bound: Vec<(String, PrLocator)>,
    default_repo: &Repo,
    forge: &dyn forge::Forge,
) -> HashMap<String, HeadLookup> {
    if bound.is_empty() {
        return HashMap::new();
    }
    let mut out = HashMap::new();
    let mut targets = Vec::new();
    let mut pending = Vec::new();
    for (branch, loc) in bound {
        match loc.resolve_or(default_repo) {
            Ok(repo) => {
                targets.push((repo, loc.number));
                pending.push((branch, loc.number));
            }
            Err(e) => {
                out.insert(branch, HeadLookup::Unavailable(format!("{e:#}")));
            }
        }
    }
    let answers = match forge.prs(&targets) {
        Ok(found) => found,
        Err(e) => {
            let reason = format!("{e:#}");
            pending
                .iter()
                .map(|_| Err(anyhow::anyhow!(reason.clone())))
                .collect()
        }
    };
    out.extend(recorded_answers(pending, answers));
    out
}

/// Overlay the recorded lookups on the branch batch. A key present in both
/// keeps the record: the record is authoritative over branch matching, and a
/// branch match overwriting it is the inversion this ordering forbids.
fn merge_lookups(
    recorded: HashMap<String, HeadLookup>,
    batch: HashMap<String, HeadLookup>,
) -> HashMap<String, HeadLookup> {
    let mut merged = batch;
    merged.extend(recorded);
    merged
}

/// The PR status of every worktree branch. With a forge, that is at most two
/// round trips: one resolving every recorded locator by number, one matching
/// every other branch by head name. Fails soft: a request that cannot be made
/// at all (no token, transport error) marks every branch it covered `Unknown`
/// rather than aborting the caller's report.
///
/// A project that declared no forge gets each branch's push state instead,
/// and one where devkit found no forge gets `Unknown` naming why.
pub fn fetch_prs(d: &Discovered, f: &forge::Resolved) -> Result<Prs> {
    let rows = d.rows.iter().filter(|r| r.branch != "DETACHED");
    match (f.forge.kind(), f.declared) {
        (ForgeKind::None, true) => {
            return Ok(Prs(rows
                .map(|r| {
                    let pushed = pushed_of(&r.worktree);
                    (r.branch.clone(), PrStatus::Untracked { pushed })
                })
                .collect()));
        }
        (ForgeKind::None, false) => {
            let branches: Vec<String> = rows.map(|r| r.branch.clone()).collect();
            let reason = format!("no forge: {}", f.reason);
            return Ok(Prs::from_lookups(unavailable_all(&branches, &reason)));
        }
        (ForgeKind::Github | ForgeKind::Gitlab | ForgeKind::Forgejo, _) => {}
    }
    let repo = f.repos.prs()?;
    let forge = f.forge.as_ref();
    let (bound, branches) = partition_by_record(&d.rows, |worktree| {
        devkit_common::record::read(Path::new(worktree)).and_then(|r| r.pr)
    });
    let recorded = recorded_lookups(bound, repo, forge);
    let batch = forge.prs_by_head(repo, &branches);
    Ok(Prs::from_lookups(merge_lookups(recorded, batch)))
}

/// Attach dirty flags (in row order), the branch's PR, tracker state, and the
/// finished verdict. `tracker` is carried through to the report for link
/// building and to tell a blank state column from an unreachable tracker.
pub fn assemble(
    d: Discovered,
    dirty: Vec<bool>,
    prs: Prs,
    states: HashMap<String, State>,
    tracker: TrackerInfo,
) -> StatusReport {
    let mut rows = d.rows;
    let mut finished_count = 0;
    for (i, wt) in rows.iter_mut().enumerate() {
        wt.dirty = dirty.get(i).copied().unwrap_or(false);
        prs.apply(wt);
        if let Some(st) = wt.issue_id.tracker().and_then(|id| states.get(id)) {
            wt.state = Some(st.clone());
        }
        let reason = reason_not_finished(wt, &tracker, false);
        wt.finished = reason.is_none();
        if wt.finished {
            finished_count += 1;
        }
        wt.reason_not_finished = reason;
    }
    StatusReport {
        worktrees: rows,
        finished_count,
        tracker,
    }
}

/// How a tracker names itself in user-facing text. `TrackerKind::None` answers
/// with the neutral word: a tracker-less project has no state gate, so nothing
/// that names a state ever reaches it.
pub fn label(kind: TrackerKind) -> &'static str {
    match kind {
        TrackerKind::Linear => "Linear",
        TrackerKind::Github => "GitHub",
        TrackerKind::None => "tracker",
    }
}

/// None when finished; otherwise a short reason it is not.
///
/// The state gate has four shapes. A project that declared it has no tracker
/// has no state to wait for, so its verdict rests on the PR and a clean tree. A
/// tracker that answered gates on the issue having reached a completed state. A
/// tracker that is configured but did not answer holds the gate open, so an
/// unset key or an unreachable API never promotes a worktree to finished — and
/// so does the no-tracker stand-in devkit falls back to, which is devkit having
/// found nothing to ask rather than the project saying there is nothing.
///
/// With `pr_only` both the state and issue-id gates are dropped (finished = PR
/// merged + clean), so repos whose branches carry no issue id still qualify.
pub fn reason_not_finished(
    wt: &IssueWorktree,
    tracker: &TrackerInfo,
    pr_only: bool,
) -> Option<String> {
    if !pr_only && wt.issue_id == IssueId::Unknown {
        return Some("not an issue worktree".into());
    }
    let mut bits: Vec<String> = Vec::new();
    match &wt.pr {
        PrStatus::Unique { state, .. } if state == "MERGED" => {}
        PrStatus::Unique { .. } => bits.push("PR not merged".into()),
        PrStatus::None => bits.push("no PR".into()),
        PrStatus::Ambiguous { .. } => bits.push("PR ambiguous".into()),
        PrStatus::Unknown { reason } => bits.push(format!("PR unknown: {reason}")),
        PrStatus::Untracked { pushed: true } => {}
        PrStatus::Untracked { pushed: false } => bits.push("commits not on a remote".into()),
    }
    // A project that declared it has no tracker has no state to wait for; every
    // other tracker gates on the issue's state and says so when it could not
    // read one — the fallback stand-in included, since it stands in for a
    // tracker devkit could not resolve. A worktree set up with no issue has no
    // state either.
    let nothing_to_wait_for =
        wt.issue_id == IssueId::NoIssue || (tracker.kind == TrackerKind::None && tracker.declared);
    if !pr_only && !nothing_to_wait_for {
        match wt.state.as_ref() {
            Some(s) if s.kind != StateKind::Completed => {
                bits.push(format!("{} {}", label(tracker.kind), s.name))
            }
            Some(_) => {}
            None if tracker.ready => bits.push("tracker state unknown".into()),
            None => bits.push("no tracker key".into()),
        }
    }
    if wt.dirty {
        bits.push("dirty".into());
    }
    if bits.is_empty() {
        None
    } else {
        Some(bits.join(", "))
    }
}

/// Discover worktrees, fetch PRs + tracker state concurrently, and compute the
/// finished verdict against a caller-supplied tracker. Silent — no progress
/// output (the CLI re-orchestrates the same pieces with bars). This crate reads
/// no config, so the caller that loaded one resolves the tracker and repos and
/// injects them; tests inject a fake.
pub fn gather_with(
    start: &str,
    ids: &[String],
    t: &Resolved,
    f: &forge::Resolved,
) -> Result<StatusReport> {
    let d = discover(start, ids)?;
    let info = TrackerInfo::of(t);
    let t = t.tracker.as_ref();
    if d.is_empty() {
        // No worktrees means no ids to look up and no rows to link, and no PR
        // repository is needed either.
        return Ok(assemble(d, Vec::new(), Prs::empty(), HashMap::new(), info));
    }
    let paths = d.worktree_paths();
    let ids_v: Vec<String> = d.issue_ids().to_vec();
    let (dirty, prs, states, link_base) = std::thread::scope(|s| {
        let dt = s.spawn(|| dirty_many(&paths));
        let pt = s.spawn(|| fetch_prs(&d, f));
        // The state fetch and the link base share a thread: both go through the
        // tracker, and both can reach the network.
        let tt = s.spawn(|| (t.states(&ids_v), t.issue_url("")));
        let dirty = dt.join().expect("dirty thread panicked");
        let prs = pt.join().expect("prs thread panicked")?;
        let (states, link_base) = tt.join().expect("tracker thread panicked");
        Ok::<_, anyhow::Error>((dirty, prs, states, link_base))
    })?;
    Ok(assemble(d, dirty, prs, states, TrackerInfo {
        link_base,
        ..info
    }))
}

/// Local-only status: discovery + dirty checks, with no `gh`/tracker network.
/// PRs stay `NO_PR` and the state stays unknown; callers (e.g. `issue info
/// --cache-only`) overlay cached data themselves.
///
/// This crate reads no config, so the tracker is detected from `start` rather
/// than named by one, and its GitHub repositories come from the `origin` remote
/// alone. Detection never yields a declared tracker, which keeps the state gate
/// closed on every row until a caller overlays real data.
pub fn gather_local(start: &str, ids: &[String]) -> Result<StatusReport> {
    let d = discover(start, ids)?;
    let f = forge::resolve(
        &devkit_config::ForgeConfig::default(),
        &devkit_config::GithubConfig::default(),
        start,
        None,
    );
    let t = devkit_common::tracker::resolve(None, Path::new(start), &f.repos);
    let dirty = dirty_many(&d.worktree_paths());
    Ok(assemble(
        d,
        dirty,
        Prs::empty(),
        HashMap::new(),
        TrackerInfo::of(&t),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use devkit_common::{
        git::Git,
        tracker::{StateKind, Tracker, TrackerKind, fake::FakeTracker},
    };

    use super::*;

    fn done(name: &str) -> State {
        State {
            kind: StateKind::Completed,
            name: name.into(),
            color: None,
        }
    }

    fn tracker(kind: TrackerKind, ready: bool) -> TrackerInfo {
        TrackerInfo {
            kind,
            ready,
            declared: true,
            reason: format!("[tracker] kind = {:?}", kind.as_str()),
            link_base: None,
        }
    }

    /// The stand-in devkit falls back to when nothing resolved: kind `None`,
    /// but the project never asked for it.
    fn fallback_none() -> TrackerInfo {
        TrackerInfo {
            kind: TrackerKind::None,
            ready: false,
            declared: false,
            reason: "detected: no LINEAR_API_KEY and no GitHub origin remote".into(),
            link_base: None,
        }
    }

    /// One discovered row on `branch`, shaped the way `discover` emits it.
    fn discovered(id: &str, branch: &str) -> Discovered {
        let mut row = wt(id, "NO_PR", false, None);
        row.branch = branch.to_string();
        Discovered::for_test(vec![row], vec![id.to_string()])
    }

    #[test]
    fn a_merged_clean_worktree_with_a_completed_issue_is_finished() {
        let t = FakeTracker::with_states([("ENG-1", done("Done"))]);
        let report = assemble(
            discovered("ENG-1", "lev/eng-1-fix"),
            vec![false],
            Prs::for_test(vec![pr(10, "MERGED", "lev/eng-1-fix")]),
            t.states(&["ENG-1".into()]),
            tracker(TrackerKind::Linear, true),
        );
        assert_eq!(
            report.worktrees[0].state.as_ref().map(|s| s.kind),
            Some(StateKind::Completed)
        );
        assert!(report.worktrees[0].finished);
        assert_eq!(report.finished_count, 1);
    }

    #[test]
    fn an_open_issue_is_not_finished_and_says_why() {
        let mut st = done("In Progress");
        st.kind = StateKind::Started;
        let report = assemble(
            discovered("ENG-2", "lev/eng-2-wip"),
            vec![false],
            Prs::for_test(vec![pr(11, "MERGED", "lev/eng-2-wip")]),
            HashMap::from([("ENG-2".to_string(), st)]),
            tracker(TrackerKind::Linear, true),
        );
        assert!(!report.worktrees[0].finished);
        let why = report.worktrees[0].reason_not_finished.as_deref().unwrap();
        assert!(
            why.contains("In Progress"),
            "the reason names the state: {why}"
        );
    }

    #[test]
    fn with_no_tracker_a_merged_clean_worktree_is_finished_without_a_state() {
        let report = assemble(
            discovered("ENG-3", "lev/some-branch"),
            vec![false],
            Prs::for_test(vec![pr(12, "MERGED", "lev/some-branch")]),
            HashMap::new(),
            tracker(TrackerKind::None, false),
        );
        let row = &report.worktrees[0];
        assert!(row.state.is_none());
        assert!(
            row.finished,
            "a project with no tracker still finishes on PR merged + clean"
        );
    }

    #[test]
    fn with_a_fallback_tracker_a_merged_clean_worktree_is_not_finished() {
        let report = assemble(
            discovered("ENG-6", "lev/fallback-branch"),
            vec![false],
            Prs::for_test(vec![pr(14, "MERGED", "lev/fallback-branch")]),
            HashMap::new(),
            fallback_none(),
        );
        let row = &report.worktrees[0];
        assert!(!row.finished);
        assert_eq!(
            row.reason_not_finished.as_deref(),
            Some("no tracker key"),
            "landing on the no-tracker stand-in means devkit found no tracker to \
             ask, which holds the gate exactly as an unreadable one does"
        );
    }

    #[test]
    fn with_a_tracker_and_no_key_a_merged_clean_worktree_is_not_finished() {
        let report = assemble(
            discovered("ENG-4", "lev/other-branch"),
            vec![false],
            Prs::for_test(vec![pr(13, "MERGED", "lev/other-branch")]),
            HashMap::new(),
            tracker(TrackerKind::Linear, false),
        );
        let row = &report.worktrees[0];
        assert!(!row.finished);
        assert_eq!(
            row.reason_not_finished.as_deref(),
            Some("no tracker key"),
            "a configured tracker that cannot answer holds the gate, so an unset \
             key never promotes a worktree to finished"
        );
    }

    // assemble zips dirty flags onto rows in order, attaches the branch's PR,
    // applies tracker state, and computes the finished verdict — the same
    // result the old monolithic gather produced.
    #[test]
    fn assemble_attaches_pr_dirty_and_verdict() {
        let d = discovered("ENG-1", "lev/eng-1-foo");
        let prs = Prs::for_test(vec![pr(7, "MERGED", "lev/eng-1-foo")]);
        let states = HashMap::from([("ENG-1".to_string(), done("Done"))]);
        let info = TrackerInfo {
            kind: TrackerKind::Linear,
            ready: true,
            declared: true,
            reason: "[tracker] kind = \"linear\"".into(),
            link_base: Some("https://linear.app/acme/issue/".into()),
        };
        let report = assemble(d, vec![false], prs, states, info);
        let row = &report.worktrees[0];
        assert_eq!(row.pr.number(), Some(7));
        assert_eq!(row.pr.state_label(), "MERGED");
        assert!(!row.dirty);
        assert!(row.finished);
        assert_eq!(report.finished_count, 1);
        assert_eq!(
            report.tracker.link_base.as_deref(),
            Some("https://linear.app/acme/issue/")
        );
    }

    #[test]
    fn assemble_marks_dirty_from_flags() {
        let report = assemble(
            discovered("ENG-2", "lev/eng-2-bar"),
            vec![true],
            Prs::for_test(vec![]),
            HashMap::new(),
            tracker(TrackerKind::None, false),
        );
        assert!(report.worktrees[0].dirty);
        assert!(!report.worktrees[0].finished);
    }

    impl Discovered {
        fn for_test(rows: Vec<IssueWorktree>, issue_ids: Vec<String>) -> Self {
            Discovered { rows, issue_ids }
        }
    }
    impl Prs {
        fn for_test(briefs: Vec<PrBrief>) -> Self {
            Prs::from_lookups(
                briefs
                    .into_iter()
                    .map(|b| (b.head_ref_name.clone(), HeadLookup::Unique(b)))
                    .collect(),
            )
        }
    }

    fn pr(n: u64, state: &str, head: &str) -> PrBrief {
        PrBrief {
            number: n,
            state: state.into(),
            url: format!("https://x/{n}"),
            title: String::new(),
            head_ref_name: head.into(),
            head_ref_oid: format!("oid{n}"),
            head_repo_owner: None,
            is_draft: false,
            author_login: None,
        }
    }

    #[test]
    fn a_recorded_branch_is_excluded_from_the_batch() {
        // The record is authoritative over branch matching in any repository,
        // not only one outside `pr_repo`: a second PR sharing the recorded
        // row's branch name would win a plain branch lookup, but the recorded
        // branch never reaches the batch that lookup runs against.
        let mut a = wt("ENG-1", "NO_PR", false, None);
        a.worktree = "/w/a".into();
        a.branch = "feat/x".into();
        let mut b = wt("ENG-2", "NO_PR", false, None);
        b.worktree = "/w/b".into();
        b.branch = "feat/y".into();

        let loc = PrLocator {
            repo: None,
            number: 12,
        };
        let (bound, branches) = partition_by_record(&[a, b], |worktree| {
            (worktree == "/w/a").then(|| loc.clone())
        });
        assert_eq!(bound, vec![("feat/x".to_string(), loc)]);
        assert_eq!(branches, vec!["feat/y".to_string()]);
    }

    #[test]
    fn a_recorded_pr_wins_over_a_second_pr_sharing_its_branch() {
        // Two forks propose `feat/x`: this worktree's work is #12 in its own
        // fork, and #11 is a stranger's PR carrying the same branch name. A
        // branch lookup answers #11 — the record is what makes the row report
        // #12 instead.
        let mut row = wt("ENG-1", "NO_PR", false, None);
        row.worktree = "/w/a".into();
        row.branch = "feat/x".into();

        let recorded = PrLocator {
            repo: Some("me/fork".into()),
            number: 12,
        };
        let (bound, branches) =
            partition_by_record(std::slice::from_ref(&row), |_| Some(recorded.clone()));
        assert_eq!(bound, vec![("feat/x".to_string(), recorded)]);
        assert!(
            branches.is_empty(),
            "a bound branch never reaches the batch"
        );

        // What branch matching would report: the stranger's PR.
        let batch = HashMap::from([(
            "feat/x".to_string(),
            HeadLookup::Unique(pr(11, "OPEN", "feat/x")),
        )]);
        let recorded = HashMap::from([(
            "feat/x".to_string(),
            recorded_result(Ok(Some(pr(12, "MERGED", "feat/x"))), 12),
        )]);
        let prs = Prs::from_lookups(merge_lookups(recorded, batch));
        prs.apply(&mut row);
        assert_eq!(row.pr.number(), Some(12), "got {:?}", row.pr);
    }

    #[test]
    fn a_batched_answer_is_keyed_by_the_branch_it_was_recorded_for() {
        let pending = vec![("feat/x".to_string(), 12), ("feat/y".to_string(), 13)];
        let got = recorded_answers(pending.clone(), vec![
            Ok(Some(pr(12, "MERGED", "someone/else"))),
            Ok(None),
        ]);
        assert!(matches!(&got["feat/x"], HeadLookup::Unique(p) if p.number == 12));
        assert!(
            matches!(&got["feat/y"], HeadLookup::Unavailable(r) if r.contains("13")),
            "got {:?}",
            got["feat/y"]
        );

        // An answer list shorter than the targets leaves the rest unknown; a
        // branch with no entry at all would read as "no PR" and open the
        // finished verdict.
        let short = recorded_answers(pending, vec![Ok(Some(pr(12, "MERGED", "feat/x")))]);
        assert_eq!(short.len(), 2);
        assert!(matches!(short["feat/y"], HeadLookup::Unavailable(_)));
    }

    #[test]
    fn a_detached_row_never_reaches_either_list() {
        let mut d = wt("UNKNOWN", "NO_PR", false, None);
        d.branch = "DETACHED".into();
        let (bound, branches) = partition_by_record(&[d], |_| {
            Some(PrLocator {
                repo: None,
                number: 1,
            })
        });
        assert!(bound.is_empty());
        assert!(branches.is_empty());
    }

    #[test]
    fn recorded_result_maps_found_missing_and_failed_to_head_lookup() {
        let brief = pr(12, "OPEN", "feat/y");
        assert!(matches!(
            recorded_result(Ok(Some(brief.clone())), 12),
            HeadLookup::Unique(p) if p.number == 12
        ));
        assert!(matches!(
            recorded_result(Ok(None), 3),
            HeadLookup::Unavailable(reason) if reason.contains('3')
        ));
        assert!(matches!(
            recorded_result(Err(anyhow::anyhow!("boom")), 3),
            HeadLookup::Unavailable(reason) if reason.contains("boom")
        ));
    }

    fn pr_ref(n: u64) -> devkit_common::tracker::PrRef {
        devkit_common::tracker::PrRef {
            url: format!("https://github.com/o/r/pull/{n}"),
            number: n,
        }
    }

    fn wt(issue_id: &str, pr_state: &str, dirty: bool, kind: Option<StateKind>) -> IssueWorktree {
        let pr = if pr_state == "NO_PR" {
            PrStatus::None
        } else {
            PrStatus::Unique {
                number: 1,
                state: pr_state.into(),
                url: "https://x/1".into(),
                is_draft: false,
            }
        };
        IssueWorktree {
            worktree: "/w".into(),
            branch: "b".into(),
            issue_id: issue_id.parse().unwrap(),
            dirty,
            pr,
            state: kind.map(|kind| State {
                kind,
                name: "Done".into(),
                color: None,
            }),
            finished: false,
            reason_not_finished: None,
        }
    }

    #[test]
    fn apply_overlays_pr_and_skips_detached() {
        let prs = Prs::for_test(vec![pr(7, "MERGED", "lev/eng-1-foo")]);
        let mut row = wt("ENG-1", "NO_PR", false, None);
        row.branch = "lev/eng-1-foo".into();
        prs.apply(&mut row);
        assert_eq!(row.pr.number(), Some(7));
        assert_eq!(row.pr.state_label(), "MERGED");

        let mut detached = wt("UNKNOWN", "NO_PR", false, None);
        detached.branch = "DETACHED".into();
        prs.apply(&mut detached);
        assert_eq!(detached.pr.number(), None);
        assert_eq!(detached.pr.state_label(), "NO_PR");
    }

    #[test]
    fn finished_when_merged_done_clean() {
        assert!(
            reason_not_finished(
                &wt("ENG-1", "MERGED", false, Some(StateKind::Completed)),
                &tracker(TrackerKind::Linear, true),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn not_finished_when_dirty() {
        assert_eq!(
            reason_not_finished(
                &wt("ENG-1", "MERGED", true, Some(StateKind::Completed)),
                &tracker(TrackerKind::Linear, true),
                false
            )
            .as_deref(),
            Some("dirty")
        );
    }

    #[test]
    fn pr_only_ignores_tracker_state() {
        // No state, no tracker, but pr_only drops the state gate.
        assert!(
            reason_not_finished(
                &wt("ENG-1", "MERGED", false, None),
                &tracker(TrackerKind::None, false),
                true
            )
            .is_none()
        );
    }

    #[test]
    fn pr_only_allows_unknown_issue_id() {
        // A repo without issue-id branch names has UNKNOWN issue ids; with
        // pr_only a merged + clean worktree is still finished.
        assert!(
            reason_not_finished(
                &wt("UNKNOWN", "MERGED", false, None),
                &tracker(TrackerKind::None, false),
                true
            )
            .is_none()
        );
    }

    /// A worktree set up with no issue has no tracker state to wait for, so a
    /// merged PR and a clean tree finish it without `--pr-only`.
    #[test]
    fn an_issueless_worktree_skips_the_tracker_gate() {
        let linear = tracker(TrackerKind::Linear, true);
        assert!(reason_not_finished(&wt("NONE", "MERGED", false, None), &linear, false).is_none());
        assert_eq!(
            reason_not_finished(&wt("NONE", "NO_PR", true, None), &linear, false).as_deref(),
            Some("no PR, dirty")
        );
    }

    #[test]
    fn pr_only_unknown_still_gated_on_pr() {
        assert_eq!(
            reason_not_finished(
                &wt("UNKNOWN", "NO_PR", false, None),
                &tracker(TrackerKind::None, false),
                true
            )
            .as_deref(),
            Some("no PR")
        );
    }

    #[test]
    fn verdict_combinations() {
        let linear = tracker(TrackerKind::Linear, true);
        // Unknown id is never an issue worktree.
        assert_eq!(
            reason_not_finished(
                &wt("UNKNOWN", "MERGED", false, Some(StateKind::Completed)),
                &linear,
                false
            )
            .as_deref(),
            Some("not an issue worktree")
        );
        // No PR + a tracker with no key, all reasons join with ", ".
        assert_eq!(
            reason_not_finished(
                &wt("ENG-2", "NO_PR", false, None),
                &tracker(TrackerKind::Linear, false),
                false
            )
            .as_deref(),
            Some("no PR, no tracker key")
        );
        // Open PR + started state + dirty; the reason names the tracker.
        assert_eq!(
            reason_not_finished(
                &wt("ENG-3", "OPEN", true, Some(StateKind::Started)),
                &linear,
                false
            )
            .as_deref(),
            Some("PR not merged, Linear Done, dirty")
        );
        // A ready tracker with no row for the issue.
        assert_eq!(
            reason_not_finished(&wt("ENG-4", "MERGED", false, None), &linear, false).as_deref(),
            Some("tracker state unknown")
        );
    }

    #[test]
    fn the_reason_names_whichever_tracker_produced_the_state() {
        let row = wt("ENG-5", "MERGED", false, Some(StateKind::Started));
        assert_eq!(
            reason_not_finished(&row, &tracker(TrackerKind::Github, true), false).as_deref(),
            Some("GitHub Done")
        );
    }

    #[test]
    fn prs_empty_leaves_row_untouched() {
        let mut r = wt("ENG-1", "NO_PR", false, None);
        Prs::empty().apply(&mut r);
        assert_eq!(r.pr.number(), None);
        assert_eq!(r.pr.state_label(), "NO_PR");
    }

    #[test]
    fn legacy_fields_derive_from_the_tag() {
        let u = PrStatus::Unique {
            number: 12,
            state: "MERGED".into(),
            url: "https://github.com/o/r/pull/12".into(),
            is_draft: false,
        };
        assert_eq!(u.number(), Some(12));
        assert_eq!(u.state_label(), "MERGED");
        assert_eq!(u.url(), Some("https://github.com/o/r/pull/12"));

        assert_eq!(PrStatus::None.state_label(), "NO_PR");
        assert_eq!(PrStatus::None.number(), None);

        // The shape that used to render as `AMBIGUOUS #0`: a state string with
        // no number, formatted with unwrap_or(0), printing a PR that
        // does not exist in the column a human reads before deleting a
        // worktree.
        let a = PrStatus::Ambiguous {
            candidates: vec![pr_ref(7), pr_ref(8)],
        };
        assert_eq!(a.state_label(), "AMBIGUOUS");
        assert_eq!(a.number(), None);
        assert_eq!(a.url(), None);
    }

    // The safety gate `issue end` reads before deleting a worktree: neither an
    // ambiguous nor an unresolved PR may read as finished, and each names why.
    #[test]
    fn ambiguous_and_unknown_prs_are_never_finished() {
        let linear = tracker(TrackerKind::Linear, true);
        let ambiguous = IssueWorktree {
            pr: PrStatus::Ambiguous {
                candidates: vec![pr_ref(7), pr_ref(8)],
            },
            ..wt("ENG-1", "NO_PR", false, Some(StateKind::Completed))
        };
        let reason = reason_not_finished(&ambiguous, &linear, false).expect("must name a reason");
        assert!(reason.contains("PR ambiguous"), "{reason}");

        let unknown = IssueWorktree {
            pr: PrStatus::Unknown {
                reason: "recorded PR no longer resolves".into(),
            },
            ..wt("ENG-2", "NO_PR", false, Some(StateKind::Completed))
        };
        let reason = reason_not_finished(&unknown, &linear, false).expect("must name a reason");
        assert!(
            reason.contains("recorded PR no longer resolves"),
            "{reason}"
        );
    }

    // dirty_stream must report each index exactly once with the same result
    // dirty_many computes. Every dir is a clean git repo except one, which
    // carries an untracked file, so exactly one index is true and the value
    // comparison catches wrong-value or wrong-index bugs.
    #[test]
    fn dirty_stream_reports_every_index_once() {
        use std::sync::Mutex;
        let base = tempfile::tempdir().unwrap();
        let paths: Vec<String> = (0..7)
            .map(|i| {
                let p = base.path().join(format!("d{i}"));
                std::fs::create_dir_all(&p).unwrap();
                Git::fixture(&p)
                    .args(["init", "-q", "-b", "main"])
                    .output()
                    .unwrap();
                p.to_string_lossy().into_owned()
            })
            .collect();
        // A repo with an untracked file: `git status --porcelain` is non-empty.
        std::fs::write(std::path::Path::new(&paths[3]).join("f"), "x").unwrap();

        let got: Mutex<Vec<Option<bool>>> = Mutex::new(vec![None; paths.len()]);
        dirty_stream(&paths, |i, d| {
            let mut g = got.lock().unwrap();
            assert!(g[i].is_none(), "index {i} reported twice");
            g[i] = Some(d);
        });
        let got = got.into_inner().unwrap();
        let want = dirty_many(&paths);
        assert_eq!(
            want,
            vec![false, false, false, true, false, false, false],
            "only the repo with the untracked file is dirty"
        );
        assert_eq!(
            got.into_iter()
                .map(|o| o.expect("index missing"))
                .collect::<Vec<_>>(),
            want
        );
    }

    /// `dirty_of` cannot distinguish a wedged git from a clean worktree — both
    /// surface as `Err` from `git status --porcelain` — so a path git cannot
    /// read is reported dirty rather than clean: `issue end` uses this to
    /// decide whether removing a worktree is safe, and a wrong "clean" would
    /// discard real work, while a wrong "dirty" only asks for `--force`.
    #[test]
    fn dirty_of_reports_dirty_when_git_cannot_answer() {
        let dir = tempfile::tempdir().unwrap();
        // Not a git repository: `git status --porcelain` fails to run here.
        assert!(dirty_of(dir.path().to_str().unwrap()));
    }

    /// A project that declared no forge has no PR to wait for, so its commits
    /// being on a remote is what `issue end` needs before deleting the branch.
    #[test]
    fn a_project_with_no_forge_finishes_once_its_commits_are_pushed() {
        let none = tracker(TrackerKind::None, false);
        let row = |pushed| IssueWorktree {
            pr: PrStatus::Untracked { pushed },
            ..wt("ENG-1", "NO_PR", false, None)
        };
        assert_eq!(reason_not_finished(&row(true), &none, false), None);
        assert_eq!(
            reason_not_finished(&row(false), &none, false).as_deref(),
            Some("commits not on a remote")
        );
        assert_eq!(row(true).pr.state_label(), "NO_FORGE");
    }

    fn scratch_forge(kind: &str) -> (tempfile::TempDir, forge::Resolved) {
        let dir = tempfile::tempdir().unwrap();
        Git::fixture(dir.path())
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        Git::fixture(dir.path())
            .args(["commit", "-q", "--allow-empty", "-m", "init"])
            .output()
            .unwrap();
        let cfg: devkit_config::Config =
            devkit_config::Config::parse(&format!("[forge]\nkind = \"{kind}\"\n")).unwrap();
        let path = dir.path().to_string_lossy().into_owned();
        let f = forge::resolve(&cfg.forge, &cfg.github, &path, None);
        (dir, f)
    }

    /// Declared none reads each branch's push state; a commit only local
    /// reads as not pushed. Detection finding no forge holds the gate with
    /// the reason instead.
    #[test]
    fn fetch_prs_without_a_forge_reads_push_state_or_holds_the_gate() {
        let (dir, declared) = scratch_forge("none");
        let mut row = wt("ENG-1", "NO_PR", false, None);
        row.worktree = dir.path().to_string_lossy().into_owned();
        row.branch = "main".into();
        let d = Discovered::for_test(vec![row.clone()], vec!["ENG-1".into()]);
        let mut got = row.clone();
        fetch_prs(&d, &declared).unwrap().apply(&mut got);
        assert_eq!(got.pr, PrStatus::Untracked { pushed: false });

        let detected = forge::resolve(
            &devkit_config::ForgeConfig::default(),
            &devkit_config::GithubConfig::default(),
            &row.worktree,
            None,
        );
        let mut got = row;
        fetch_prs(&d, &detected).unwrap().apply(&mut got);
        assert!(
            matches!(&got.pr, PrStatus::Unknown { reason } if reason.contains("no forge")),
            "{:?}",
            got.pr
        );
    }

    #[test]
    fn pr_status_of_carries_the_draft_flag() {
        let pr = PrBrief {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            title: String::new(),
            head_ref_name: "feat/x".into(),
            head_ref_oid: "abc123".into(),
            head_repo_owner: None,
            is_draft: true,
            author_login: None,
        };
        let status = pr_status_of(&HeadLookup::Unique(pr));
        assert_eq!(status, PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            is_draft: true,
        });
    }

    #[test]
    fn a_draft_still_labels_as_open_for_serialized_consumers() {
        let status = PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            is_draft: true,
        };
        assert_eq!(status.state_label(), "OPEN");
    }
}
