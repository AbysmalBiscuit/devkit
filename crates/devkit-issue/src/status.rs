use std::{collections::HashMap, path::Path};

use anyhow::Result;
use devkit_common::{
    forge::{self, ForgeKind, HeadLookup, PrBrief, PrLocator, PrLookup, Repo},
    record::{self, RecordState},
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
        /// Commits at the worktree's HEAD that a merged PR's head does not
        /// reach: work that exists only on the branch `issue end` deletes.
        /// `None` for a PR that has not merged, and for a merged one whose
        /// head could not be compared.
        #[serde(default)]
        ahead: Option<u32>,
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

/// Whether a worktree holds uncommitted changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tree {
    Clean,
    Dirty,
    /// The status could not be read, or has not been yet. Carries why.
    Unknown(String),
}

/// Whether a worktree may be removed. `Unknown` counts as held: it means an
/// input could not be read, and a removal on a guess can discard work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Finished,
    Held(Vec<String>),
    Unknown(Vec<String>),
}

impl Default for Verdict {
    /// A row nobody has judged yet.
    fn default() -> Self {
        Verdict::Unknown(Vec::new())
    }
}

impl Verdict {
    pub fn is_finished(&self) -> bool {
        matches!(self, Verdict::Finished)
    }

    /// Every reason the worktree is not finished, joined, or `None` when there
    /// is none to give.
    pub fn reason(&self) -> Option<String> {
        match self {
            Verdict::Finished => None,
            Verdict::Held(reasons) | Verdict::Unknown(reasons) if !reasons.is_empty() => {
                Some(reasons.join(", "))
            }
            Verdict::Held(_) | Verdict::Unknown(_) => None,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Verdict::Finished => "finished",
            Verdict::Held(_) => "held",
            Verdict::Unknown(_) => "unknown",
        }
    }
}

/// One issue worktree with its PR + tracker state and the finished verdict.
#[derive(Debug, Clone)]
pub struct IssueWorktree {
    pub worktree: String,
    pub branch: String,
    pub issue_id: IssueId,
    /// The setup record exists and cannot be read. The issue id then comes
    /// from the branch, which the record would have overruled.
    pub record_unreadable: bool,
    pub tree: Tree,
    /// The PR, tagged. `pr_number`, `pr_state` and `pr_url` below are derived
    /// from it for the serialized shape consumers already read.
    pub pr: PrStatus,
    /// The tracker's state for this issue, absent when the tracker has no row
    /// for it or there is no tracker.
    pub state: Option<State>,
    pub verdict: Verdict,
}

impl IssueWorktree {
    /// The row for the worktree at `path` on `branch`, before its tree, PR and
    /// tracker state are read.
    pub fn at(path: &Path, branch: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: path.to_string_lossy().into_owned(),
            branch: branch.to_string(),
            issue_id: worktree::issue_id_of(path, branch),
            record_unreadable: matches!(record::read_state(path), RecordState::Unusable),
            tree: Tree::Unknown("not checked".into()),
            pr: PrStatus::None,
            state: None,
            verdict: Verdict::default(),
        }
    }
}

impl Serialize for IssueWorktree {
    /// Emits `pr` alongside the three legacy fields, so an MCP consumer reading
    /// `pr_state` keeps working while a new one can read the candidates.
    /// `dirty` is true whenever the tree is not known to be clean.
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("IssueWorktree", 12)?;
        st.serialize_field("worktree", &self.worktree)?;
        st.serialize_field("branch", &self.branch)?;
        st.serialize_field("issue_id", &self.issue_id)?;
        st.serialize_field("dirty", &(self.tree != Tree::Clean))?;
        st.serialize_field("pr", &self.pr)?;
        st.serialize_field("pr_number", &self.pr.number())?;
        st.serialize_field("pr_state", self.pr.state_label())?;
        st.serialize_field("pr_url", &self.pr.url())?;
        st.serialize_field("state", &self.state)?;
        st.serialize_field("verdict", self.verdict.kind())?;
        st.serialize_field("finished", &self.verdict.is_finished())?;
        st.serialize_field("reason_not_finished", &self.verdict.reason())?;
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

    /// Head-branch lookups, tagged as the report's `PrStatus`, with each merged
    /// PR's head compared against the row checked out on its branch.
    fn of(lookups: HashMap<String, HeadLookup>, rows: &[IssueWorktree]) -> Prs {
        let worktree_on: HashMap<&str, &str> = rows
            .iter()
            .map(|r| (r.branch.as_str(), r.worktree.as_str()))
            .collect();
        Prs(lookups
            .into_iter()
            .map(|(branch, lookup)| {
                let mut status = pr_status_of(&lookup);
                if let (HeadLookup::Unique(pr), PrStatus::Unique { state, ahead, .. }) =
                    (&lookup, &mut status)
                    && state == "MERGED"
                    && let Some(wt) = worktree_on.get(branch.as_str())
                {
                    *ahead = ahead_of(Path::new(wt), &pr.head_ref_oid);
                }
                (branch, status)
            })
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
            ahead: None,
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
/// Rows carry an unchecked tree; the tree check is a separate step so callers
/// can drive it with a progress bar.
pub fn discover(start: &str, ids: &[String]) -> Result<Discovered> {
    let (_main, others) = worktree::discover(start)?;
    let mut rows = Vec::new();
    for wt in &others {
        let row = IssueWorktree::at(&wt.path, &wt.branch);
        // An issue id is case-insensitive in every tracker that has one, and
        // the record holds whichever spelling the tracker was given.
        let id = row.issue_id.to_string();
        if !ids.is_empty() && !ids.iter().any(|w| w.eq_ignore_ascii_case(&id)) {
            continue;
        }
        rows.push(row);
    }
    let issue_ids = rows
        .iter()
        .filter_map(|r| r.issue_id.tracker().map(str::to_owned))
        .collect();
    Ok(Discovered { rows, issue_ids })
}

/// Whether a worktree has uncommitted changes, untracked files included. The
/// one place a removal's dirty gate is decided: a status that could not be
/// read is `Unknown`, which holds the worktree like any other unknown.
pub fn tree_of(path: &Path) -> Tree {
    match Vcs::at(path).dirty(path, Changes::All) {
        Ok(true) => Tree::Dirty,
        Ok(false) => Tree::Clean,
        Err(e) => Tree::Unknown(format!("{e:#}")),
    }
}

/// `tree_of` for many worktrees, run on a bounded thread pool with order
/// preserved. Each check is an independent, I/O-bound `git status` walk, so
/// fanning them across cores turns N serial walks into roughly one walk's
/// latency. The batch form of [`tree_stream`]: results land in a slot per
/// input index, keeping the output aligned with `paths`.
pub fn tree_many(paths: &[String]) -> Vec<Tree> {
    let out = std::sync::Mutex::new(vec![Tree::Clean; paths.len()]);
    tree_stream(paths, |i, t| out.lock().unwrap()[i] = t);
    out.into_inner().unwrap()
}

/// `tree_of` for many worktrees, reporting each result as soon as it is
/// known. `report(i, tree)` is invoked exactly once per input index, from
/// worker threads on a bounded pool over contiguous chunks; callers that
/// want the batch form should keep using [`tree_many`].
pub fn tree_stream(paths: &[String], report: impl Fn(usize, Tree) + Send + Clone) {
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
                    report(ci * chunk + j, tree_of(Path::new(p)));
                }
            });
        }
    });
}

/// Whether the commit checked out at `path` is on some remote-tracking branch.
/// A repository that cannot answer reads as not pushed, since this stands in
/// for a merged PR before `issue end` deletes the branch.
pub fn pushed_of(path: &str) -> bool {
    let path = Path::new(path);
    Vcs::at(path).pushed(path).unwrap_or(false)
}

/// How many commits at `worktree`'s HEAD the commit `oid` does not reach, or
/// `None` when that cannot be worked out: an `oid` the local repository has
/// never fetched, or none at all from a forge that omitted it.
pub fn ahead_of(worktree: &Path, oid: &str) -> Option<u32> {
    if oid.is_empty() || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Vcs::at(worktree).ahead(worktree, oid).ok()
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
    recorded: impl Fn(&str, &str) -> Option<PrLocator>,
) -> (Vec<(String, PrLocator)>, Vec<String>) {
    let mut bound = Vec::new();
    let mut branches = Vec::new();
    for row in rows.iter().filter(|r| r.branch != "DETACHED") {
        match recorded(&row.worktree, &row.branch) {
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
            return Ok(Prs::of(unavailable_all(&branches, &reason), &d.rows));
        }
        (ForgeKind::Github | ForgeKind::Gitlab | ForgeKind::Forgejo, _) => {}
    }
    let repo = f.repos.prs()?;
    let forge = f.forge.as_ref();
    let (bound, branches) = partition_by_record(&d.rows, |worktree, branch| {
        devkit_common::record::read_on(Path::new(worktree), branch).and_then(|r| r.pr)
    });
    let recorded = recorded_lookups(bound, repo, forge);
    let batch = forge.prs_by_head(repo, &branches);
    let lookups = merge_lookups(recorded, batch);
    fetch_merged_heads(&lookups, &d.rows, forge);
    Ok(Prs::of(lookups, &d.rows))
}

/// Fetches, in one request, every merged PR head missing from the worktree's
/// repository, so [`Prs::of`] can compare it with HEAD. A branch finished
/// elsewhere and deleted on merge leaves its head reachable only from the
/// forge's PR ref. A failed fetch leaves those heads uncompared.
fn fetch_merged_heads(
    lookups: &HashMap<String, HeadLookup>,
    rows: &[IssueWorktree],
    forge: &dyn forge::Forge,
) {
    let mut repo = None;
    let mut refs = Vec::new();
    for row in rows {
        let Some(HeadLookup::Unique(pr)) = lookups.get(&row.branch) else {
            continue;
        };
        let worktree = Path::new(&row.worktree);
        if pr.state == "MERGED"
            && ahead_of(worktree, &pr.head_ref_oid).is_none()
            && let Some(head) = forge.head_ref(pr.number)
        {
            repo.get_or_insert(worktree);
            refs.push(head);
        }
    }
    if let Some(repo) = repo {
        let _ = Vcs::at(repo).fetch_commits(repo, "origin", &refs);
    }
}

/// Attach trees (in row order), the branch's PR, tracker state, and the
/// verdict. `tracker` is carried through to the report for link building and
/// to tell a blank state column from an unreachable tracker. `pr_only` is
/// [`verdict`]'s.
pub fn assemble(
    d: Discovered,
    trees: Vec<Tree>,
    prs: Prs,
    states: HashMap<String, State>,
    tracker: TrackerInfo,
    pr_only: bool,
) -> StatusReport {
    let mut rows = d.rows;
    for (i, wt) in rows.iter_mut().enumerate() {
        if let Some(tree) = trees.get(i) {
            wt.tree = tree.clone();
        }
        prs.apply(wt);
        if let Some(st) = wt.issue_id.tracker().and_then(|id| states.get(id)) {
            wt.state = Some(st.clone());
        }
        wt.verdict = verdict(wt, &tracker, pr_only);
    }
    let finished_count = rows.iter().filter(|r| r.verdict.is_finished()).count();
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

/// Whether the worktree may be removed, and every reason it may not. This is
/// the whole deletion gate: `issue end` removes exactly the rows it calls
/// finished.
///
/// A reason is `Unknown` when an input could not be read: the record, the
/// tree, the PR, the merged PR's head against HEAD, or the tracker's state.
/// One such reason makes the whole verdict `Unknown`.
///
/// The state gate has four shapes. A project that declared it has no tracker
/// has no state to wait for, so its verdict rests on the PR and a clean tree. A
/// tracker that answered gates on the issue having reached a completed state. A
/// tracker that is configured but did not answer holds the gate open, so an
/// unset key or an unreachable API never promotes a worktree to finished. So
/// does the no-tracker stand-in devkit falls back to, which is devkit having
/// found nothing to ask rather than the project saying there is nothing.
///
/// With `pr_only` both the state and issue-id gates are dropped (finished = PR
/// merged + clean), so repos whose branches carry no issue id still qualify.
pub fn verdict(wt: &IssueWorktree, tracker: &TrackerInfo, pr_only: bool) -> Verdict {
    let mut held: Vec<String> = Vec::new();
    let mut unknown = false;
    let mut doubt = |held: &mut Vec<String>, reason: String| {
        unknown = true;
        held.push(reason);
    };
    if wt.record_unreadable {
        doubt(&mut held, "issue record unreadable".into());
    } else if !pr_only && wt.issue_id == IssueId::Unknown {
        return Verdict::Held(vec!["not an issue worktree".into()]);
    }
    match &wt.pr {
        PrStatus::Unique { state, ahead, .. } if state == "MERGED" => match ahead {
            Some(0) => {}
            Some(n) => held.push(format!("{n} commit(s) past the merged PR")),
            None => doubt(&mut held, "HEAD not compared with the merged PR".into()),
        },
        PrStatus::Unique { .. } => held.push("PR not merged".into()),
        PrStatus::None => held.push("no PR".into()),
        PrStatus::Ambiguous { .. } => held.push("PR ambiguous".into()),
        PrStatus::Unknown { reason } => doubt(&mut held, format!("PR unknown: {reason}")),
        PrStatus::Untracked { pushed: true } => {}
        PrStatus::Untracked { pushed: false } => held.push("commits not on a remote".into()),
    }
    // A project that declared it has no tracker has no state to wait for. Every
    // other tracker gates on the issue's state and says so when it could not
    // read one, the fallback stand-in included, since it stands in for a
    // tracker devkit could not resolve. A worktree set up with no issue has no
    // state either.
    let nothing_to_wait_for =
        wt.issue_id == IssueId::NoIssue || (tracker.kind == TrackerKind::None && tracker.declared);
    if !pr_only && !nothing_to_wait_for {
        match wt.state.as_ref() {
            Some(s) if s.kind != StateKind::Completed => {
                held.push(format!("{} {}", label(tracker.kind), s.name))
            }
            Some(_) => {}
            None if tracker.ready => doubt(&mut held, "tracker state unknown".into()),
            None => doubt(&mut held, "no tracker key".into()),
        }
    }
    match &wt.tree {
        Tree::Clean => {}
        Tree::Dirty => held.push("dirty".into()),
        Tree::Unknown(why) => doubt(&mut held, format!("tree unknown: {why}")),
    }
    match (held.is_empty(), unknown) {
        (true, _) => Verdict::Finished,
        (false, true) => Verdict::Unknown(held),
        (false, false) => Verdict::Held(held),
    }
}

/// Discover worktrees, fetch PRs + tracker state concurrently, and compute the
/// verdict against a caller-supplied tracker. Silent, with no progress output
/// (the CLI re-orchestrates the same pieces with bars). This crate reads no
/// config, so the caller that loaded one resolves the tracker and repos and
/// injects them; tests inject a fake. `pr_only` is [`verdict`]'s.
pub fn gather_with(
    start: &str,
    ids: &[String],
    t: &Resolved,
    f: &forge::Resolved,
    pr_only: bool,
) -> Result<StatusReport> {
    let d = discover(start, ids)?;
    let info = TrackerInfo::of(t);
    let t = t.tracker.as_ref();
    if d.is_empty() {
        // No worktrees means no ids to look up and no rows to link, and no PR
        // repository is needed either.
        return Ok(assemble(
            d,
            Vec::new(),
            Prs::empty(),
            HashMap::new(),
            info,
            pr_only,
        ));
    }
    let paths = d.worktree_paths();
    let ids_v: Vec<String> = d.issue_ids().to_vec();
    let (trees, prs, states, link_base) = std::thread::scope(|s| {
        let dt = s.spawn(|| tree_many(&paths));
        let pt = s.spawn(|| fetch_prs(&d, f));
        // The state fetch and the link base share a thread: both go through the
        // tracker, and both can reach the network.
        let tt = s.spawn(|| (t.states(&ids_v), t.issue_url("")));
        let trees = dt.join().expect("tree thread panicked");
        let prs = pt.join().expect("prs thread panicked")?;
        let (states, link_base) = tt.join().expect("tracker thread panicked");
        Ok::<_, anyhow::Error>((trees, prs, states, link_base))
    })?;
    let info = TrackerInfo { link_base, ..info };
    Ok(assemble(d, trees, prs, states, info, pr_only))
}

/// Local-only status: discovery + tree checks, with no `gh`/tracker network.
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
    let trees = tree_many(&d.worktree_paths());
    Ok(assemble(
        d,
        trees,
        Prs::empty(),
        HashMap::new(),
        TrackerInfo::of(&t),
        false,
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use devkit_common::tracker::{StateKind, Tracker, TrackerKind, fake::FakeTracker};
    use devkit_git::Git;

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
            vec![Tree::Clean],
            Prs::for_test(vec![pr(10, "MERGED", "lev/eng-1-fix")]),
            t.states(&["ENG-1".into()]),
            tracker(TrackerKind::Linear, true),
            false,
        );
        assert_eq!(
            report.worktrees[0].state.as_ref().map(|s| s.kind),
            Some(StateKind::Completed)
        );
        assert!(report.worktrees[0].verdict.is_finished());
        assert_eq!(report.finished_count, 1);
    }

    #[test]
    fn an_open_issue_is_not_finished_and_says_why() {
        let mut st = done("In Progress");
        st.kind = StateKind::Started;
        let report = assemble(
            discovered("ENG-2", "lev/eng-2-wip"),
            vec![Tree::Clean],
            Prs::for_test(vec![pr(11, "MERGED", "lev/eng-2-wip")]),
            HashMap::from([("ENG-2".to_string(), st)]),
            tracker(TrackerKind::Linear, true),
            false,
        );
        assert!(!report.worktrees[0].verdict.is_finished());
        let why = report.worktrees[0].verdict.reason().unwrap();
        assert!(
            why.contains("In Progress"),
            "the reason names the state: {why}"
        );
    }

    #[test]
    fn with_no_tracker_a_merged_clean_worktree_is_finished_without_a_state() {
        let report = assemble(
            discovered("ENG-3", "lev/some-branch"),
            vec![Tree::Clean],
            Prs::for_test(vec![pr(12, "MERGED", "lev/some-branch")]),
            HashMap::new(),
            tracker(TrackerKind::None, false),
            false,
        );
        let row = &report.worktrees[0];
        assert!(row.state.is_none());
        assert!(
            row.verdict.is_finished(),
            "a project with no tracker still finishes on PR merged + clean"
        );
    }

    #[test]
    fn with_a_fallback_tracker_a_merged_clean_worktree_is_not_finished() {
        let report = assemble(
            discovered("ENG-6", "lev/fallback-branch"),
            vec![Tree::Clean],
            Prs::for_test(vec![pr(14, "MERGED", "lev/fallback-branch")]),
            HashMap::new(),
            fallback_none(),
            false,
        );
        let row = &report.worktrees[0];
        assert!(!row.verdict.is_finished());
        assert_eq!(
            row.verdict.reason().as_deref(),
            Some("no tracker key"),
            "landing on the no-tracker stand-in means devkit found no tracker to \
             ask, which holds the gate exactly as an unreadable one does"
        );
    }

    #[test]
    fn with_a_tracker_and_no_key_a_merged_clean_worktree_is_not_finished() {
        let report = assemble(
            discovered("ENG-4", "lev/other-branch"),
            vec![Tree::Clean],
            Prs::for_test(vec![pr(13, "MERGED", "lev/other-branch")]),
            HashMap::new(),
            tracker(TrackerKind::Linear, false),
            false,
        );
        let row = &report.worktrees[0];
        assert!(!row.verdict.is_finished());
        assert_eq!(
            row.verdict.reason().as_deref(),
            Some("no tracker key"),
            "a configured tracker that cannot answer holds the gate, so an unset \
             key never promotes a worktree to finished"
        );
    }

    // assemble zips trees onto rows in order, attaches the branch's PR,
    // applies tracker state, and computes the verdict.
    #[test]
    fn assemble_attaches_pr_tree_and_verdict() {
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
        let report = assemble(d, vec![Tree::Clean], prs, states, info, false);
        let row = &report.worktrees[0];
        assert_eq!(row.pr.number(), Some(7));
        assert_eq!(row.pr.state_label(), "MERGED");
        assert_eq!(row.tree, Tree::Clean);
        assert!(row.verdict.is_finished());
        assert_eq!(report.finished_count, 1);
        assert_eq!(
            report.tracker.link_base.as_deref(),
            Some("https://linear.app/acme/issue/")
        );
    }

    #[test]
    fn assemble_marks_dirty_from_trees() {
        let report = assemble(
            discovered("ENG-2", "lev/eng-2-bar"),
            vec![Tree::Dirty],
            Prs::for_test(vec![]),
            HashMap::new(),
            tracker(TrackerKind::None, false),
            false,
        );
        assert_eq!(report.worktrees[0].tree, Tree::Dirty);
        assert!(!report.worktrees[0].verdict.is_finished());
    }

    impl Discovered {
        fn for_test(rows: Vec<IssueWorktree>, issue_ids: Vec<String>) -> Self {
            Discovered { rows, issue_ids }
        }
    }
    impl Prs {
        /// Each PR as its head branch's status, a merged one with HEAD at its
        /// head.
        fn for_test(briefs: Vec<PrBrief>) -> Self {
            Prs(briefs
                .into_iter()
                .map(|b| {
                    let mut status = pr_status_of(&HeadLookup::Unique(b.clone()));
                    if let PrStatus::Unique { state, ahead, .. } = &mut status
                        && state == "MERGED"
                    {
                        *ahead = Some(0);
                    }
                    (b.head_ref_name, status)
                })
                .collect())
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
        let (bound, branches) = partition_by_record(&[a, b], |worktree, _| {
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
            partition_by_record(std::slice::from_ref(&row), |_, _| Some(recorded.clone()));
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
        let prs = Prs::of(merge_lookups(recorded, batch), &[]);
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
        let (bound, branches) = partition_by_record(&[d], |_, _| {
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

    fn reason_of(wt: &IssueWorktree, tracker: &TrackerInfo, pr_only: bool) -> Option<String> {
        verdict(wt, tracker, pr_only).reason()
    }

    /// A row whose merged PR, if any, has HEAD at its head.
    fn wt(issue_id: &str, pr_state: &str, dirty: bool, kind: Option<StateKind>) -> IssueWorktree {
        let pr = if pr_state == "NO_PR" {
            PrStatus::None
        } else {
            PrStatus::Unique {
                number: 1,
                state: pr_state.into(),
                url: "https://x/1".into(),
                is_draft: false,
                ahead: (pr_state == "MERGED").then_some(0),
            }
        };
        IssueWorktree {
            worktree: "/w".into(),
            branch: "b".into(),
            issue_id: issue_id.parse().unwrap(),
            record_unreadable: false,
            tree: if dirty { Tree::Dirty } else { Tree::Clean },
            pr,
            state: kind.map(|kind| State {
                kind,
                name: "Done".into(),
                color: None,
            }),
            verdict: Verdict::default(),
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
            reason_of(
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
            reason_of(
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
            reason_of(
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
            reason_of(
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
        assert!(reason_of(&wt("NONE", "MERGED", false, None), &linear, false).is_none());
        assert_eq!(
            reason_of(&wt("NONE", "NO_PR", true, None), &linear, false).as_deref(),
            Some("no PR, dirty")
        );
    }

    #[test]
    fn pr_only_unknown_still_gated_on_pr() {
        assert_eq!(
            reason_of(
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
            reason_of(
                &wt("UNKNOWN", "MERGED", false, Some(StateKind::Completed)),
                &linear,
                false
            )
            .as_deref(),
            Some("not an issue worktree")
        );
        // No PR + a tracker with no key, all reasons join with ", ".
        assert_eq!(
            reason_of(
                &wt("ENG-2", "NO_PR", false, None),
                &tracker(TrackerKind::Linear, false),
                false
            )
            .as_deref(),
            Some("no PR, no tracker key")
        );
        // Open PR + started state + dirty; the reason names the tracker.
        assert_eq!(
            reason_of(
                &wt("ENG-3", "OPEN", true, Some(StateKind::Started)),
                &linear,
                false
            )
            .as_deref(),
            Some("PR not merged, Linear Done, dirty")
        );
        // A ready tracker with no row for the issue.
        assert_eq!(
            reason_of(&wt("ENG-4", "MERGED", false, None), &linear, false).as_deref(),
            Some("tracker state unknown")
        );
    }

    #[test]
    fn the_reason_names_whichever_tracker_produced_the_state() {
        let row = wt("ENG-5", "MERGED", false, Some(StateKind::Started));
        assert_eq!(
            reason_of(&row, &tracker(TrackerKind::Github, true), false).as_deref(),
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
            ahead: Some(0),
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
        let reason = reason_of(&ambiguous, &linear, false).expect("must name a reason");
        assert!(reason.contains("PR ambiguous"), "{reason}");

        let unknown = IssueWorktree {
            pr: PrStatus::Unknown {
                reason: "recorded PR no longer resolves".into(),
            },
            ..wt("ENG-2", "NO_PR", false, Some(StateKind::Completed))
        };
        let v = verdict(&unknown, &linear, false);
        assert!(matches!(v, Verdict::Unknown(_)), "{v:?}");
        let reason = v.reason().expect("must name a reason");
        assert!(
            reason.contains("recorded PR no longer resolves"),
            "{reason}"
        );
    }

    /// A merged PR finishes the worktree only when HEAD holds nothing the PR's
    /// head lacks, since `issue end` deletes the branch those commits are on.
    #[test]
    fn a_merged_pr_finishes_only_a_worktree_with_nothing_past_its_head() {
        let linear = tracker(TrackerKind::Linear, true);
        let merged = |ahead| IssueWorktree {
            pr: PrStatus::Unique {
                number: 1,
                state: "MERGED".into(),
                url: "https://x/1".into(),
                is_draft: false,
                ahead,
            },
            ..wt("ENG-1", "NO_PR", false, Some(StateKind::Completed))
        };
        assert_eq!(verdict(&merged(Some(0)), &linear, false), Verdict::Finished);
        assert_eq!(
            verdict(&merged(Some(2)), &linear, false),
            Verdict::Held(vec!["2 commit(s) past the merged PR".into()])
        );
        assert!(matches!(
            verdict(&merged(None), &linear, false),
            Verdict::Unknown(_)
        ));
    }

    #[test]
    fn an_unreadable_record_or_tree_makes_the_verdict_unknown() {
        let linear = tracker(TrackerKind::Linear, true);
        let finished = wt("ENG-1", "MERGED", false, Some(StateKind::Completed));
        assert_eq!(verdict(&finished, &linear, false), Verdict::Finished);

        let record = IssueWorktree {
            record_unreadable: true,
            ..finished.clone()
        };
        assert_eq!(
            verdict(&record, &linear, false),
            Verdict::Unknown(vec!["issue record unreadable".into()])
        );

        let tree = IssueWorktree {
            tree: Tree::Unknown("git status timed out".into()),
            ..finished
        };
        assert_eq!(
            verdict(&tree, &linear, false),
            Verdict::Unknown(vec!["tree unknown: git status timed out".into()])
        );
    }

    #[test]
    fn ahead_of_counts_commits_the_oid_does_not_reach() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Git::fixture(dir.path())
                .args(args.iter().copied())
                .output()
                .unwrap()
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["commit", "-q", "--allow-empty", "-m", "one"]);
        let one = git(&["rev-parse", "HEAD"]).trim().to_string();
        git(&["commit", "-q", "--allow-empty", "-m", "two"]);

        assert_eq!(ahead_of(dir.path(), &one), Some(1));
        assert_eq!(
            ahead_of(dir.path(), "",),
            None,
            "a forge that reported no head commit"
        );
        assert_eq!(
            ahead_of(dir.path(), &"f".repeat(40)),
            None,
            "a head commit this clone never fetched"
        );
    }

    // tree_stream must report each index exactly once with the same result
    // tree_many computes. Every dir is a clean git repo except one, which
    // carries an untracked file, so exactly one index is dirty and the value
    // comparison catches wrong-value or wrong-index bugs.
    #[test]
    fn tree_stream_reports_every_index_once() {
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

        let got: Mutex<Vec<Option<Tree>>> = Mutex::new(vec![None; paths.len()]);
        tree_stream(&paths, |i, t| {
            let mut g = got.lock().unwrap();
            assert!(g[i].is_none(), "index {i} reported twice");
            g[i] = Some(t);
        });
        let got = got.into_inner().unwrap();
        let want = tree_many(&paths);
        let mut expected = vec![Tree::Clean; 7];
        expected[3] = Tree::Dirty;
        assert_eq!(
            want, expected,
            "only the repo with the untracked file is dirty"
        );
        assert_eq!(
            got.into_iter()
                .map(|o| o.expect("index missing"))
                .collect::<Vec<_>>(),
            want
        );
    }

    /// A path git cannot read is neither clean nor dirty. `--force` waives a
    /// dirty tree, and it must not waive one nobody could look at.
    #[test]
    fn tree_of_is_unknown_when_git_cannot_answer() {
        let dir = tempfile::tempdir().unwrap();
        // Not a git repository: `git status --porcelain` fails to run here.
        assert!(matches!(tree_of(dir.path()), Tree::Unknown(_)));
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
        assert_eq!(reason_of(&row(true), &none, false), None);
        assert_eq!(
            reason_of(&row(false), &none, false).as_deref(),
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
            ahead: None,
        });
    }

    #[test]
    fn a_draft_still_labels_as_open_for_serialized_consumers() {
        let status = PrStatus::Unique {
            number: 7,
            state: "OPEN".into(),
            url: "u7".into(),
            is_draft: true,
            ahead: None,
        };
        assert_eq!(status.state_label(), "OPEN");
    }
}
