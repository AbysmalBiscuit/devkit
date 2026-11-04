//! Which done todos `devkit todo prune` removes.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Days, NaiveDate, NaiveTime, TimeDelta, TimeZone, Utc};
use devkit_config::TicketSyncConfig;

use crate::{Edit, StatusChange, Todo, TodoStore, ticket::synced_kind};

/// How many todos one [`Edit::Prune`] names, so one request stays small on a
/// backend that sends it over HTTP.
const BATCH: usize = 1000;

/// The instants a done todo's last change must fall between. Both bounds are
/// inclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl Window {
    fn covers(&self, at: DateTime<Utc>) -> bool {
        self.from.is_none_or(|from| from <= at) && self.to.is_none_or(|to| at <= to)
    }
}

/// The first instant of `date` in `zone`. A midnight that a clock change
/// skips gives way to the first hour that exists.
pub fn day_start<Tz: TimeZone>(date: NaiveDate, zone: &Tz) -> DateTime<Utc> {
    let midnight = date.and_time(NaiveTime::MIN);
    (0..)
        .find_map(|hours| {
            zone.from_local_datetime(&(midnight + TimeDelta::hours(hours)))
                .earliest()
        })
        .expect("a clock change skips only a few hours")
        .to_utc()
}

/// The last instant of `date` in `zone`.
pub fn day_end<Tz: TimeZone>(date: NaiveDate, zone: &Tz) -> DateTime<Utc> {
    let next = date
        .checked_add_days(Days::new(1))
        .unwrap_or(NaiveDate::MAX);
    day_start(next, zone) - TimeDelta::nanoseconds(1)
}

/// The todos of `scoped` to remove: each one done, with a last change inside
/// `window`, that no open todo in `all` waits on or nests under, however deep.
/// A todo with no readable last change stays. `all` is the whole store, since
/// the open todo that needs one may sit outside the scope.
///
/// A ticket todo goes only when `sync` maps its ticket's last synced status
/// to done, since a sync would bring a ticket still open back as open.
pub fn prunable(
    all: &[Todo],
    scoped: Vec<Todo>,
    window: Window,
    sync: &TicketSyncConfig,
) -> Vec<Todo> {
    let needed = needed_by_open(all);
    scoped
        .into_iter()
        .filter(|todo| !todo.status.kind().is_open() && !needed.contains(todo.id.as_str()))
        .filter(|todo| match &todo.ticket {
            None => true,
            Some(ticket) => ticket
                .synced_status
                .as_deref()
                .and_then(|name| synced_kind(name, sync))
                .is_some_and(|kind| !kind.is_open()),
        })
        .filter(|todo| {
            todo.modified
                .as_deref()
                .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                .is_some_and(|at| window.covers(at.to_utc()))
        })
        .collect()
}

/// The ids each open todo in `all` waits on, and each of its ancestors.
fn needed_by_open(all: &[Todo]) -> HashSet<&str> {
    let by_id: HashMap<&str, &Todo> = all.iter().map(|todo| (todo.id.as_str(), todo)).collect();
    let mut needed = HashSet::new();
    let mut walked = HashSet::new();
    for todo in all.iter().filter(|todo| todo.status.kind().is_open()) {
        needed.extend(todo.blocked_by.iter().map(String::as_str));
        let mut parent = todo.parent.as_deref();
        while let Some(id) = parent {
            if !walked.insert(id) {
                break;
            }
            needed.insert(id);
            parent = by_id.get(id).and_then(|todo| todo.parent.as_deref());
        }
    }
    needed
}

/// Removes `todos` from `store` in batches, and hands `removed` the changes of
/// each batch as it lands, so a later failed batch loses no earlier report.
/// Returns how many todos it removed.
pub fn remove(
    store: &impl TodoStore,
    todos: &[Todo],
    mut removed: impl FnMut(&[StatusChange]) -> anyhow::Result<()>,
) -> anyhow::Result<usize> {
    let mut count = 0;
    for batch in todos.chunks(BATCH) {
        let changes = store.apply(&Edit::Prune {
            ids: batch.iter().map(|todo| todo.id.clone()).collect(),
        })?;
        removed(&changes)?;
        count += changes.len();
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;
    use crate::{Holder, Status};

    fn date() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 1).unwrap()
    }

    #[test]
    fn a_day_runs_from_local_midnight_to_the_next() {
        let tokyo = FixedOffset::east_opt(9 * 3600).unwrap();
        assert_eq!(
            day_start(date(), &tokyo).to_rfc3339(),
            "2026-09-30T15:00:00+00:00"
        );
        assert_eq!(
            day_end(date(), &tokyo) + TimeDelta::nanoseconds(1),
            "2026-10-01T15:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    fn todo(id: &str, status: Status, parent: Option<&str>) -> Todo {
        Todo {
            id: id.into(),
            description: id.into(),
            status,
            parent: parent.map(Into::into),
            order: None,
            project: None,
            entry: None,
            modified: Some("2026-10-01T00:00:00Z".into()),
            blocked_by: Vec::new(),
            ticket: None,
        }
    }

    fn done() -> Status {
        Status::Completed {
            by: Some(Holder::human()),
        }
    }

    #[test]
    fn a_parent_cycle_ends_the_walk() {
        let all = [
            todo("open", Status::Pending, Some("a")),
            todo("a", done(), Some("b")),
            todo("b", done(), Some("a")),
        ];
        assert!(prunable(&all, all.to_vec(), Window::default(), &Default::default()).is_empty());
    }

    /// Removes every todo of its first prune, and fails every later one.
    struct FailsAfterOneBatch(std::cell::Cell<usize>);

    impl TodoStore for FailsAfterOneBatch {
        fn list(&self, _: &crate::Filter) -> anyhow::Result<Vec<Todo>> {
            Ok(Vec::new())
        }

        fn get(&self, _: &str) -> anyhow::Result<Option<Todo>> {
            Ok(None)
        }

        fn add(&self, _: crate::NewTodo) -> anyhow::Result<String> {
            anyhow::bail!("unused")
        }

        fn apply(&self, edit: &Edit) -> anyhow::Result<Vec<StatusChange>> {
            let Edit::Prune { ids } = edit else {
                anyhow::bail!("unused");
            };
            self.0.set(self.0.get() + 1);
            if self.0.get() > 1 {
                anyhow::bail!("store went away");
            }
            Ok(ids
                .iter()
                .map(|id| StatusChange::of(&todo(id, done(), None), None))
                .collect())
        }
    }

    #[test]
    fn a_failed_batch_still_reports_the_batches_before_it() {
        let todos: Vec<Todo> = (0..BATCH + 1)
            .map(|n| todo(&n.to_string(), done(), None))
            .collect();
        let mut reported = Vec::new();

        let err = remove(&FailsAfterOneBatch(Default::default()), &todos, |changes| {
            reported.extend(changes.iter().map(|c| c.todo.clone()));
            Ok(())
        })
        .unwrap_err();

        assert!(err.to_string().contains("went away"), "{err:#}");
        assert_eq!(reported.len(), BATCH);
        assert_eq!(reported[0], "0");
    }

    #[test]
    fn an_unreadable_last_change_stays() {
        let mut unread = todo("unread", done(), None);
        unread.modified = Some("yesterday".into());
        let mut unset = todo("unset", done(), None);
        unset.modified = None;
        let all = [unread, unset];
        assert!(prunable(&all, all.to_vec(), Window::default(), &Default::default()).is_empty());
    }
}
