//! Subagent runs, claim intervals and sessions. Hooks and store edits append
//! events to an [`ActivityStore`], without a lock, and
//! [`ActivityStore::read`] pairs them into records. A run closes at its stop,
//! else its session's end, else as [`RunEnd::Lost`] once its agent has been
//! silent past [`BACKSTOP`]. A session that has been silent as long reads as
//! [`SessionState::Silent`]: a harness may reclaim a session without ever
//! sending its end.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::{Edit, Filter, Holder, NewTodo, Status, StatusChange, Todo, TodoStore};

/// How long an agent may go without firing a hook before its open run counts
/// as lost, and a session before it counts as silent, the lifetime of a
/// hook's file lock.
pub const BACKSTOP: TimeDelta = TimeDelta::minutes(30);

/// The agent id a session's main agent is marked seen under, which no
/// subagent's id is.
pub const MAIN_AGENT: &str = "";

/// The file the local log marks [`MAIN_AGENT`] seen in, a name no
/// [`segment`] spells.
const MAIN_FILE: &str = "@main";

/// The label a run without an agent type reports under.
const SUBAGENT: &str = "subagent";

const EVENTS: &str = "events.jsonl";
const SEEN: &str = "seen";

/// One line of the log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub what: What,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum What {
    SubagentStart {
        session: String,
        agent: String,
        /// Absent where the harness sent none, as for a Claude Code fork.
        agent_type: Option<String>,
    },
    SubagentStop {
        session: String,
        agent: String,
    },
    SessionEnd {
        session: String,
    },
    Claim {
        todo: String,
        node: String,
        holder: Holder,
    },
    Unclaim {
        todo: String,
        holder: Holder,
        outcome: ClaimEnd,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEnd {
    Stopped,
    SessionEnded,
    Lost,
}

impl RunEnd {
    /// The text report's spelling, the same as serde's snake_case.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::SessionEnded => "session_ended",
            Self::Lost => "lost",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimEnd {
    Completed,
    Cancelled,
    /// Returned to pending: stopped, or released when its holder ended.
    Released,
    /// Taken over by another holder, whose interval starts as this one ends.
    Handed,
}

/// One subagent run. `end` and `outcome` are `None` while it runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub session: String,
    pub agent: String,
    pub agent_type: Option<String>,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub outcome: Option<RunEnd>,
}

impl Run {
    /// The agent type, or [`SUBAGENT`] for a run that has none.
    pub fn label(&self) -> &str {
        self.agent_type.as_deref().unwrap_or(SUBAGENT)
    }
}

/// One stretch of a todo in progress by one holder. `end` and `outcome` are
/// `None` while it is held.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interval {
    pub todo: String,
    pub node: String,
    pub holder: Holder,
    pub start: DateTime<Utc>,
    pub end: Option<DateTime<Utc>>,
    pub outcome: Option<ClaimEnd>,
}

/// What the log knows of one session: when it first and last showed signs
/// of life, and when it sent its end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub session: String,
    pub first: DateTime<Utc>,
    /// Its latest hook, subagent start or stop, or claim.
    pub last_seen: Option<DateTime<Utc>>,
    /// Its latest end.
    pub ended: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionState {
    /// It sent its end, and nothing since.
    Ended {
        at: DateTime<Utc>,
    },
    /// Nothing from it for longer than [`BACKSTOP`]: reclaimed without an
    /// end, or idle.
    Silent {
        since: DateTime<Utc>,
    },
    Active,
}

impl Session {
    /// Its state as of `now`.
    pub fn state(&self, now: DateTime<Utc>) -> SessionState {
        match (self.last_seen, self.ended) {
            (seen, Some(at)) if seen.is_none_or(|seen| seen <= at) => SessionState::Ended { at },
            (Some(since), _) if now - since > BACKSTOP => SessionState::Silent { since },
            _ => SessionState::Active,
        }
    }

    /// Its latest sign of life or end.
    pub fn last(&self) -> DateTime<Utc> {
        self.last_seen.max(self.ended).unwrap_or(self.first)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub runs: Vec<Run>,
    pub claims: Vec<Interval>,
    /// Every session the log has a record of, in order of first sight.
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// The time the records were judged as of, by the store's clock.
    #[serde(default)]
    pub as_of: DateTime<Utc>,
}

/// This machine's clock. Debug builds shift it by
/// `DEVKIT_TEST_CLOCK_SKEW_SECS` seconds, so a test can stand in for a
/// machine whose clock is off.
pub fn local_now() -> SystemTime {
    let now = SystemTime::now();
    #[cfg(debug_assertions)]
    if let Some(skew) = std::env::var("DEVKIT_TEST_CLOCK_SKEW_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
    {
        let by = std::time::Duration::from_secs(skew.unsigned_abs());
        return if skew < 0 { now - by } else { now + by };
    }
    now
}

/// Where activity events are kept.
///
/// The `_now` methods and [`ActivityStore::seen`] take the time from the
/// store's own clock: this machine's for a local log, the database's for one
/// many machines share, so machines whose clocks disagree still record and
/// judge runs on one timeline.
#[ambassador::delegatable_trait]
pub trait ActivityStore {
    /// Appends `event`. Concurrent appends never interleave.
    fn record(&self, event: &::devkit_todo::activity::Event) -> ::anyhow::Result<()>;
    /// Appends every one of `events`, in order. A store that can write them
    /// in one go does, so a long batch costs one round trip.
    fn record_all(&self, events: &[::devkit_todo::activity::Event]) -> ::anyhow::Result<()> {
        events.iter().try_for_each(|event| self.record(event))
    }
    /// Appends `what`, stamped now by the store's clock.
    fn record_now(&self, what: &::devkit_todo::activity::What) -> ::anyhow::Result<()>;
    /// Notes that `agent` of `session`, or its main agent as
    /// [`MAIN_AGENT`], fired a hook now, by the store's clock.
    fn seen(&self, session: &str, agent: &str) -> ::anyhow::Result<()>;
    /// Notes that `agent` of `session` fired a hook at `when`.
    fn seen_at(
        &self,
        session: &str,
        agent: &str,
        when: ::std::time::SystemTime,
    ) -> ::anyhow::Result<()>;
    /// Drops the last-hook marks of `agent`, or of every agent of `session`
    /// when `None`, once their runs have closed.
    fn forget(&self, session: &str, agent: ::std::option::Option<&str>) -> ::anyhow::Result<()>;
    /// Every run and interval recorded, as of `now`.
    fn read(
        &self,
        now: ::chrono::DateTime<::chrono::Utc>,
    ) -> ::anyhow::Result<::devkit_todo::activity::Activity>;
    /// Every run and interval recorded, as of now by the store's clock.
    fn read_now(&self) -> ::anyhow::Result<::devkit_todo::activity::Activity>;
}

impl Activity {
    /// Pairs `events` into records as of `now`. An open run whose agent last
    /// fired a hook, by `seen`, more than [`BACKSTOP`] before `now` closes as
    /// lost at that hook, or at its start when it was never seen.
    pub fn of(mut events: Vec<Event>, seen: Vec<Seen>, now: DateTime<Utc>) -> Self {
        events.sort_by_key(|e| e.at);
        let sessions = sessions(&events, &seen);
        let mut activity = derive(events);
        let marks: HashMap<(&str, &str), DateTime<Utc>> = seen
            .iter()
            .map(|s| ((s.session.as_str(), s.agent.as_str()), s.at))
            .collect();
        for run in activity.runs.iter_mut().filter(|r| r.end.is_none()) {
            let last = marks
                .get(&(run.session.as_str(), run.agent.as_str()))
                .map_or(run.start, |seen| (*seen).max(run.start));
            if now - last > BACKSTOP {
                run.end = Some(last);
                run.outcome = Some(RunEnd::Lost);
            }
        }
        activity.sessions = sessions;
        activity.as_of = now;
        activity
    }

    /// The state of the session `holder` belongs to, `None` when the log has
    /// no record of it.
    pub fn session_state(&self, holder: &Holder) -> Option<SessionState> {
        let session = holder.session();
        self.sessions
            .iter()
            .find(|s| s.session == *session)
            .map(|s| s.state(self.as_of))
    }
}

/// Every session in time-ordered `events` and `seen`, in order of first
/// sight. Its own hooks, subagent starts and stops, and claims are signs of
/// life; an unclaim is not, since another holder may have made it.
fn sessions(events: &[Event], seen: &[Seen]) -> Vec<Session> {
    let mut sessions: Vec<Session> = Vec::new();
    let mut note = |session: &str, at: DateTime<Utc>, end: bool| {
        let i = match sessions.iter().position(|s| s.session == session) {
            Some(i) => i,
            None => {
                sessions.push(Session {
                    session: session.to_string(),
                    first: at,
                    last_seen: None,
                    ended: None,
                });
                sessions.len() - 1
            }
        };
        let s = &mut sessions[i];
        s.first = s.first.min(at);
        let field = if end { &mut s.ended } else { &mut s.last_seen };
        *field = (*field).max(Some(at));
    };
    for Event { at, what } in events {
        match what {
            What::SubagentStart { session, .. } | What::SubagentStop { session, .. } => {
                note(session, *at, false)
            }
            What::SessionEnd { session } => note(session, *at, true),
            What::Claim { holder, .. } if !holder.is_human() => note(&holder.session(), *at, false),
            What::Claim { .. } | What::Unclaim { .. } => {}
        }
    }
    for mark in seen {
        note(&mark.session, mark.at, false);
    }
    sessions.sort_by_key(|s| s.first);
    sessions
}

/// The records a shared database keeps under one root, as its
/// `activity_read` function returns them: every event as a log line, and the
/// last hook of each agent still marked seen, as of `now`.
#[derive(Debug, Deserialize)]
pub struct Snapshot {
    pub now: DateTime<Utc>,
    pub events: Vec<serde_json::Value>,
    pub seen: Vec<Seen>,
}

/// When an agent last fired a hook.
#[derive(Clone, Debug, Deserialize)]
pub struct Seen {
    pub session: String,
    pub agent: String,
    pub at: DateTime<Utc>,
}

impl From<Snapshot> for Activity {
    /// An event that does not parse is skipped.
    fn from(snapshot: Snapshot) -> Self {
        let events = snapshot
            .events
            .into_iter()
            .filter_map(|line| serde_json::from_value(line).ok())
            .collect();
        Self::of(events, snapshot.seen, snapshot.now)
    }
}

/// The log in one directory: `events.jsonl`, and under `seen/` one empty file
/// per running agent, and per session's main agent, whose modification time
/// is that agent's last hook.
pub struct ActivityLog {
    dir: PathBuf,
}

impl ActivityLog {
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The log in devkit's todo state directory.
    pub fn open() -> Self {
        Self::at(crate::state_dir().join("activity"))
    }

    fn seen_path(&self, session: &str, agent: Option<&str>) -> PathBuf {
        let dir = self.dir.join(SEEN).join(segment(session));
        match agent {
            Some(MAIN_AGENT) => dir.join(MAIN_FILE),
            Some(agent) => dir.join(segment(agent)),
            None => dir,
        }
    }

    /// Every mark under `seen/`. One whose name does not decode, or that
    /// vanishes while being read, is skipped.
    fn marks(&self) -> Result<Vec<Seen>> {
        let root = self.dir.join(SEEN);
        let sessions = match fs::read_dir(&root) {
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            sessions => sessions.with_context(|| format!("reading {}", root.display()))?,
        };
        let mut marks = Vec::new();
        for session_dir in sessions.flatten() {
            let Some(session) = unsegment(&session_dir.file_name().to_string_lossy()) else {
                continue;
            };
            let Ok(agents) = fs::read_dir(session_dir.path()) else {
                continue;
            };
            for file in agents.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                let agent = match name.as_str() {
                    MAIN_FILE => Some(MAIN_AGENT.to_string()),
                    name => unsegment(name),
                };
                let modified = file.metadata().and_then(|m| m.modified());
                if let (Some(agent), Ok(modified)) = (agent, modified) {
                    marks.push(Seen {
                        session: session.clone(),
                        agent,
                        at: modified.into(),
                    });
                }
            }
        }
        Ok(marks)
    }
}

impl ActivityStore for ActivityLog {
    /// Appends `event` as one line, in a single write so concurrent appends
    /// never interleave.
    fn record(&self, event: &Event) -> Result<()> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        let path = self.dir.join(EVENTS);
        open_creating(&path, OpenOptions::new().create(true).append(true))
            .and_then(|mut file| file.write_all(&line))
            .with_context(|| format!("appending to {}", path.display()))
    }

    fn record_now(&self, what: &What) -> Result<()> {
        self.record(&Event {
            at: local_now().into(),
            what: what.clone(),
        })
    }

    fn seen(&self, session: &str, agent: &str) -> Result<()> {
        self.seen_at(session, agent, local_now())
    }

    fn read_now(&self) -> Result<Activity> {
        self.read(local_now().into())
    }

    fn seen_at(&self, session: &str, agent: &str, when: SystemTime) -> Result<()> {
        let path = self.seen_path(session, Some(agent));
        open_creating(
            &path,
            OpenOptions::new().create(true).write(true).truncate(false),
        )
        .and_then(|file| file.set_modified(when))
        .with_context(|| format!("marking {} seen", path.display()))
    }

    fn forget(&self, session: &str, agent: Option<&str>) -> Result<()> {
        let path = self.seen_path(session, agent);
        let removed = match agent {
            Some(_) => fs::remove_file(&path),
            None => fs::remove_dir_all(&path),
        };
        match removed {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", path.display()))
            }
            _ => Ok(()),
        }
    }

    /// A line that does not parse is skipped.
    fn read(&self, now: DateTime<Utc>) -> Result<Activity> {
        let path = self.dir.join(EVENTS);
        let text = match fs::read_to_string(&path) {
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            text => text.with_context(|| format!("reading {}", path.display()))?,
        };
        let events = text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Ok(Activity::of(events, self.marks()?, now))
    }
}

/// `path` opened with `options`, its parent directories created when missing.
fn open_creating(path: &Path, options: &OpenOptions) -> std::io::Result<File> {
    match options.open(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            options.open(path)
        }
        file => file,
    }
}

/// `id` as one path segment: every byte outside `[A-Za-z0-9_-]` as `%XX`, so
/// distinct ids never share a file and none escapes the directory.
pub fn segment(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The id [`segment`] spelled as `name`, `None` for a name it never spells.
fn unsegment(name: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(name.len());
    let mut rest = name.bytes();
    while let Some(b) = rest.next() {
        match b {
            b'%' => {
                let hex = [rest.next()?, rest.next()?];
                bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' => bytes.push(b),
            _ => return None,
        }
    }
    String::from_utf8(bytes).ok()
}

/// Pairs time-ordered events into records. A stop or an unclaim with nothing
/// open to close is dropped.
fn derive(events: Vec<Event>) -> Activity {
    let mut activity = Activity::default();
    let mut runs: HashMap<(String, String), usize> = HashMap::new();
    let mut claims: HashMap<String, usize> = HashMap::new();
    for Event { at, what } in events {
        match what {
            What::SubagentStart {
                session,
                agent,
                agent_type,
            } => {
                let key = (session.clone(), agent.clone());
                if runs.contains_key(&key) {
                    continue;
                }
                runs.insert(key, activity.runs.len());
                activity.runs.push(Run {
                    session,
                    agent,
                    agent_type,
                    start: at,
                    end: None,
                    outcome: None,
                });
            }
            What::SubagentStop { session, agent } => {
                if let Some(i) = runs.remove(&(session, agent)) {
                    close_run(&mut activity.runs[i], at, RunEnd::Stopped);
                }
            }
            What::SessionEnd { session } => {
                runs.retain(|(s, _), i| {
                    let ends = *s == session;
                    if ends {
                        close_run(&mut activity.runs[*i], at, RunEnd::SessionEnded);
                    }
                    !ends
                });
            }
            What::Claim { todo, node, holder } => {
                if let Some(i) = claims.remove(&todo) {
                    close_claim(&mut activity.claims[i], at, ClaimEnd::Handed);
                }
                claims.insert(todo.clone(), activity.claims.len());
                activity.claims.push(Interval {
                    todo,
                    node,
                    holder,
                    start: at,
                    end: None,
                    outcome: None,
                });
            }
            What::Unclaim { todo, outcome, .. } => {
                if let Some(i) = claims.remove(&todo) {
                    close_claim(&mut activity.claims[i], at, outcome);
                }
            }
        }
    }
    activity
}

fn close_run(run: &mut Run, at: DateTime<Utc>, outcome: RunEnd) {
    run.end = Some(at);
    run.outcome = Some(outcome);
}

fn close_claim(claim: &mut Interval, at: DateTime<Utc>, outcome: ClaimEnd) {
    claim.end = Some(at);
    claim.outcome = Some(outcome);
}

/// `at` as RFC 3339 UTC to the second, the form reports print.
pub fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A store whose claim changes are recorded in an [`ActivityStore`]. A
/// failure to record never fails the edit.
pub struct Recorded<S, L> {
    store: S,
    log: L,
}

impl<S, L> Recorded<S, L> {
    pub fn new(store: S, log: L) -> Self {
        Self { store, log }
    }

    pub fn inner(&self) -> &S {
        &self.store
    }

    pub fn log(&self) -> &L {
        &self.log
    }
}

fn unclaim(change: &StatusChange, holder: &Holder, outcome: ClaimEnd) -> What {
    What::Unclaim {
        todo: change.todo.clone(),
        holder: holder.clone(),
        outcome,
    }
}

fn claim(change: &StatusChange, holder: &Holder) -> What {
    What::Claim {
        todo: change.todo.clone(),
        node: change.node.clone(),
        holder: holder.clone(),
    }
}

/// The claim events of one status change.
fn claim_events(change: &StatusChange) -> Vec<What> {
    match (&change.from, &change.to) {
        (Status::InProgress { by }, Some(Status::InProgress { by: to })) if by != to => {
            vec![unclaim(change, by, ClaimEnd::Handed), claim(change, to)]
        }
        (Status::InProgress { .. }, Some(Status::InProgress { .. })) => Vec::new(),
        (Status::InProgress { by }, Some(Status::Completed { .. })) => {
            vec![unclaim(change, by, ClaimEnd::Completed)]
        }
        (Status::InProgress { by }, Some(Status::Cancelled { .. }) | None) => {
            vec![unclaim(change, by, ClaimEnd::Cancelled)]
        }
        (Status::InProgress { by }, Some(Status::Pending)) => {
            vec![unclaim(change, by, ClaimEnd::Released)]
        }
        (_, Some(Status::InProgress { by })) => vec![claim(change, by)],
        _ => Vec::new(),
    }
}

impl<S: TodoStore, L: ActivityStore> TodoStore for Recorded<S, L> {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        self.store.list(filter)
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        self.store.get(id)
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        self.store.add(todo)
    }

    /// Logs each claim change at the time the store made it, so events from
    /// processes that append out of order still read in order.
    fn apply(&self, edit: &Edit) -> Result<Vec<StatusChange>> {
        let changes = self.store.apply(edit)?;
        let events: Vec<Event> = changes
            .iter()
            .flat_map(|change| {
                claim_events(change).into_iter().map(|what| Event {
                    at: change.at,
                    what,
                })
            })
            .collect();
        if !events.is_empty() {
            let _ = self.log.record_all(&events);
        }
        Ok(changes)
    }
}
