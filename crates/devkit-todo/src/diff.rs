//! Turning a resent list into edits. Plan tools such as Codex's
//! `update_plan` send the whole list on every call, so each call is compared
//! with the previous one.

use crate::{ORDER_GAP, StatusKind};

/// One step of a list as a plan tool sent it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub text: String,
    pub status: StatusKind,
}

/// A step of the previous list and the todo it became.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirrored {
    pub text: String,
    pub todo: String,
    pub status: StatusKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    /// The new list's step at `index` has no todo yet.
    Add {
        index: usize,
    },
    Cancel {
        todo: String,
    },
    Status {
        todo: String,
        to: StatusKind,
    },
    Reorder {
        todo: String,
        order: i64,
    },
}

/// For each step of `next`, the index of the `previous` entry it continues:
/// the first one not yet paired that has the same text.
pub fn pair(previous: &[Mirrored], next: &[Step]) -> Vec<Option<usize>> {
    let mut paired = vec![false; previous.len()];
    next.iter()
        .map(|step| {
            let found = (0..previous.len()).find(|&i| !paired[i] && previous[i].text == step.text);
            if let Some(i) = found {
                paired[i] = true;
            }
            found
        })
        .collect()
}

/// The changes that take `previous`, in its order, to `next`: adds, then status
/// changes, then cancels, then reorders.
///
/// Matching is positional within equal text, so two steps with the same text
/// stay distinct, and a renamed step reads as a cancel and an add. A step
/// whose position changed is reordered to `index * ORDER_GAP`.
pub fn diff(previous: &[Mirrored], next: &[Step]) -> Vec<Change> {
    let pairs = pair(previous, next);
    let mut paired = vec![false; previous.len()];
    pairs.iter().flatten().for_each(|&i| paired[i] = true);
    let adds = pairs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.is_none())
        .map(|(index, _)| Change::Add { index });
    let statuses = pairs.iter().zip(next).filter_map(|(p, step)| {
        let prev = &previous[(*p)?];
        (prev.status != step.status).then(|| Change::Status {
            todo: prev.todo.clone(),
            to: step.status,
        })
    });
    let cancels = previous
        .iter()
        .zip(&paired)
        .filter(|(_, paired)| !**paired)
        .map(|(prev, _)| Change::Cancel {
            todo: prev.todo.clone(),
        });
    let reorders = pairs.iter().enumerate().filter_map(|(index, p)| {
        let from = (*p)?;
        (from != index).then(|| Change::Reorder {
            todo: previous[from].todo.clone(),
            order: index as i64 * ORDER_GAP,
        })
    });
    adds.chain(statuses)
        .chain(cancels)
        .chain(reorders)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StatusKind::{Completed as C, InProgress as I, Pending as P};

    fn prev(rows: &[(&str, &str, StatusKind)]) -> Vec<Mirrored> {
        rows.iter()
            .map(|(text, id, status)| Mirrored {
                text: text.to_string(),
                todo: id.to_string(),
                status: *status,
            })
            .collect()
    }

    fn steps(rows: &[(&str, StatusKind)]) -> Vec<Step> {
        rows.iter()
            .map(|(text, status)| Step {
                text: text.to_string(),
                status: *status,
            })
            .collect()
    }

    #[test]
    fn repeated_steps_stay_distinct() {
        let previous = prev(&[
            ("run tests", "1", P),
            ("build", "2", P),
            ("run tests", "3", P),
        ]);
        assert_eq!(
            diff(&previous, &steps(&[("run tests", C), ("run tests", P)])),
            [
                Change::Status {
                    todo: "1".into(),
                    to: C
                },
                Change::Cancel { todo: "2".into() },
                Change::Reorder {
                    todo: "3".into(),
                    order: 1024
                },
            ]
        );
    }

    #[test]
    fn new_steps_are_added_at_their_index() {
        assert_eq!(diff(&[], &steps(&[("a", P), ("b", I)])), [
            Change::Add { index: 0 },
            Change::Add { index: 1 }
        ]);
    }

    #[test]
    fn a_rename_is_a_cancel_and_an_add() {
        let previous = prev(&[("old", "1", P)]);
        assert_eq!(diff(&previous, &steps(&[("new", P)])), [
            Change::Add { index: 0 },
            Change::Cancel { todo: "1".into() }
        ]);
    }

    #[test]
    fn an_unchanged_list_changes_nothing() {
        let previous = prev(&[("a", "1", P), ("b", "2", C)]);
        assert!(diff(&previous, &steps(&[("a", P), ("b", C)])).is_empty());
    }
}
