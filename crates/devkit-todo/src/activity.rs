//! Subagent runs and claim intervals. Hooks and store edits append events to
//! one log, without a lock, and [`ActivityLog::read`] pairs them into records.
//! A run closes at its stop, else its session's end, else as
//! [`RunEnd::Lost`] once its agent has been silent past [`BACKSTOP`].

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::{Edit, Filter, Holder, NewTodo, Status, Todo, TodoStore, transition};

/// How long an agent may go without firing a hook before its open run counts
/// as lost, the lifetime of a hook's file lock.
pub const BACKSTOP: TimeDelta = TimeDelta::minutes(30);

/// The label a run without an agent type reports under.
pub const SUBAGENT: &str = "subagent";

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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub runs: Vec<Run>,
    pub claims: Vec<Interval>,
}

/// The log in one directory: `events.jsonl`, and under `seen/` one empty file
/// per running agent whose modification time is that agent's last hook.
pub struct ActivityLog {
    dir: PathBuf,
}

impl ActivityLog {
    pub fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The log beside the todo state, whichever backend holds the todos.
    pub fn open() -> Self {
        Self::at(crate::state_dir().join("activity"))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends `event` as one line, in a single write so concurrent appends
    /// never interleave.
    pub fn record(&self, event: &Event) -> Result<()> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        let path = self.dir.join(EVENTS);
        let open = || OpenOptions::new().create(true).append(true).open(&path);
        let mut file = match open() {
            Err(e) if e.kind() == ErrorKind::NotFound => {
                fs::create_dir_all(&self.dir)?;
                open()?
            }
            file => file?,
        };
        file.write_all(&line)?;
        Ok(())
    }

    /// Notes that `agent` of `session` fired a hook now.
    pub fn seen(&self, session: &str, agent: &str) -> Result<()> {
        self.seen_at(session, agent, SystemTime::now())
    }

    pub fn seen_at(&self, session: &str, agent: &str, when: SystemTime) -> Result<()> {
        let path = self.seen_path(session, Some(agent));
        let open = || {
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path)
        };
        let file = match open() {
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                open()?
            }
            file => file?,
        };
        file.set_modified(when)?;
        Ok(())
    }

    /// Drops the last-hook marks of `agent`, or of every agent of `session`
    /// when `None`, once their runs have closed.
    pub fn forget(&self, session: &str, agent: Option<&str>) -> Result<()> {
        let path = self.seen_path(session, agent);
        let removed = match agent {
            Some(_) => fs::remove_file(&path),
            None => fs::remove_dir_all(&path),
        };
        match removed {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    fn seen_path(&self, session: &str, agent: Option<&str>) -> PathBuf {
        let dir = self.dir.join(SEEN).join(segment(session));
        match agent {
            Some(agent) => dir.join(segment(agent)),
            None => dir,
        }
    }

    fn last_seen(&self, session: &str, agent: &str) -> Option<DateTime<Utc>> {
        let modified = fs::metadata(self.seen_path(session, Some(agent)))
            .and_then(|m| m.modified())
            .ok()?;
        Some(modified.into())
    }

    /// Every run and interval in the log as of `now`. A line that does not
    /// parse is skipped.
    pub fn read(&self, now: DateTime<Utc>) -> Result<Activity> {
        let text = match fs::read_to_string(self.dir.join(EVENTS)) {
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            text => text?,
        };
        let mut events: Vec<Event> = text
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        events.sort_by_key(|e| e.at);
        let mut activity = derive(events);
        for run in activity.runs.iter_mut().filter(|r| r.end.is_none()) {
            let last = self
                .last_seen(&run.session, &run.agent)
                .map_or(run.start, |seen| seen.max(run.start));
            if now - last > BACKSTOP {
                run.end = Some(last);
                run.outcome = Some(RunEnd::Lost);
            }
        }
        Ok(activity)
    }
}

/// `id` as one path segment: every byte outside `[A-Za-z0-9_-]` as `%XX`, so
/// distinct ids never share a file and none escapes the directory.
fn segment(id: &str) -> String {
    id.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
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

/// A store whose claim changes are recorded in an [`ActivityLog`]. A failure
/// to record never fails the edit.
pub struct Recorded<S> {
    store: S,
    log: ActivityLog,
}

impl<S> Recorded<S> {
    pub fn new(store: S, log: ActivityLog) -> Self {
        Self { store, log }
    }

    pub fn inner(&self) -> &S {
        &self.store
    }

    pub fn log(&self) -> &ActivityLog {
        &self.log
    }
}

impl<S: TodoStore> Recorded<S> {
    /// The claim events `edit` produces if it applies, judged from the store
    /// before it does. A read that fails records nothing.
    fn claim_changes(&self, edit: &Edit) -> Vec<What> {
        match edit {
            Edit::SetStatus { id, to, actor } => {
                let Ok(Some(todo)) = self.store.get(id) else {
                    return Vec::new();
                };
                match transition(&todo.status, *to, actor) {
                    Ok(Some(next)) => moved(&todo, &next),
                    _ => Vec::new(),
                }
            }
            Edit::ReleaseAll { holder } if !holder.is_human() => self
                .store
                .list(&Filter::all())
                .unwrap_or_default()
                .iter()
                .filter_map(|todo| match &todo.status {
                    Status::InProgress { by } if holder.covers(by) => {
                        Some(unclaim(todo, by, ClaimEnd::Released))
                    }
                    _ => None,
                })
                .collect(),
            Edit::Purge(id) => match self.store.get(id) {
                Ok(Some(todo)) => match &todo.status {
                    Status::InProgress { by } => vec![unclaim(&todo, by, ClaimEnd::Cancelled)],
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
}

fn unclaim(todo: &Todo, holder: &Holder, outcome: ClaimEnd) -> What {
    What::Unclaim {
        todo: todo.id.clone(),
        holder: holder.clone(),
        outcome,
    }
}

fn claim(todo: &Todo, holder: &Holder) -> What {
    What::Claim {
        todo: todo.id.clone(),
        node: todo.node().to_string(),
        holder: holder.clone(),
    }
}

/// The claim events of `todo` moving to `next`.
fn moved(todo: &Todo, next: &Status) -> Vec<What> {
    match (&todo.status, next) {
        (Status::InProgress { by }, Status::InProgress { by: to }) if by != to => {
            vec![unclaim(todo, by, ClaimEnd::Handed), claim(todo, to)]
        }
        (Status::InProgress { .. }, Status::InProgress { .. }) => Vec::new(),
        (Status::InProgress { by }, Status::Completed { .. }) => {
            vec![unclaim(todo, by, ClaimEnd::Completed)]
        }
        (Status::InProgress { by }, Status::Cancelled { .. }) => {
            vec![unclaim(todo, by, ClaimEnd::Cancelled)]
        }
        (Status::InProgress { by }, Status::Pending) => vec![unclaim(todo, by, ClaimEnd::Released)],
        (_, Status::InProgress { by }) => vec![claim(todo, by)],
        _ => Vec::new(),
    }
}

impl<S: TodoStore> TodoStore for Recorded<S> {
    fn list(&self, filter: &Filter) -> Result<Vec<Todo>> {
        self.store.list(filter)
    }

    fn get(&self, id: &str) -> Result<Option<Todo>> {
        self.store.get(id)
    }

    fn add(&self, todo: NewTodo) -> Result<String> {
        self.store.add(todo)
    }

    fn apply(&self, edit: &Edit) -> Result<()> {
        let changes = self.claim_changes(edit);
        self.store.apply(edit)?;
        let at = DateTime::<Utc>::from(SystemTime::now());
        for what in changes {
            let _ = self.log.record(&Event { at, what });
        }
        Ok(())
    }
}
