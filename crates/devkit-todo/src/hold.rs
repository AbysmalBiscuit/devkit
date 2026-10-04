//! What holds an agent at its stop: the todos still open for it, and where
//! the fingerprint of the list it was last reminded of is kept.

use std::path::PathBuf;

use crate::{Holder, Status, StatusKind, Todo, render, state_dir};

/// The todos `holder` still owes: those it has in progress itself, on any
/// node, plus the pending todos on `pending_node`. A claim a sub-agent of
/// `holder` holds is the sub-agent's, not `holder`'s. Ordered by node, then
/// by order among siblings.
pub fn open_for<'a>(
    todos: &'a [Todo],
    holder: &Holder,
    pending_node: Option<&str>,
) -> Vec<&'a Todo> {
    let mut open: Vec<&Todo> = todos
        .iter()
        .filter(|t| match &t.status {
            Status::InProgress { by } => by == holder,
            Status::Pending => pending_node == Some(t.node()),
            Status::Completed { .. } | Status::Cancelled { .. } => false,
        })
        .collect();
    open.sort_by(|a, b| a.node().cmp(b.node()).then(a.order.cmp(&b.order)));
    open
}

/// One `<id> <status>` line per todo, sorted by id, so the same todos in the
/// same states give the same fingerprint in any order.
pub fn fingerprint(open: &[&Todo]) -> String {
    let mut sorted = open.to_vec();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    sorted
        .iter()
        .map(|t| {
            let status = match t.status.kind() {
                StatusKind::Pending => "pending",
                StatusKind::InProgress => "in_progress",
                StatusKind::Completed => "completed",
                StatusKind::Cancelled => "cancelled",
            };
            format!("{} {status}\n", t.id)
        })
        .collect()
}

/// The directory holding the fingerprints of `session` and its sub-agents,
/// removed whole when the session ends.
pub fn hold_dir(session: &Holder) -> PathBuf {
    state_dir().join("holds").join(render::digest(session))
}

/// The fingerprint of the open todos `holder` was last reminded of.
pub fn hold_path(holder: &Holder) -> PathBuf {
    hold_dir(&holder.session()).join(render::digest(holder))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(id: &str, node: &str, order: i64, status: Status) -> Todo {
        Todo {
            id: id.into(),
            description: format!("todo {id}"),
            status,
            parent: None,
            order: Some(order),
            project: Some(node.into()),
            entry: None,
            modified: None,
        }
    }

    fn by(holder: &str) -> Status {
        Status::InProgress {
            by: Holder::new(holder),
        }
    }

    fn todos() -> Vec<Todo> {
        vec![
            todo("p-other", "M", 1, Status::Pending),
            todo("ip-s-other", "M", 2, by("S")),
            todo("ip-a1", "N", 3, by("S/a1")),
            todo("done", "N", 4, Status::Completed { by: None }),
            todo("p-n", "N", 5, Status::Pending),
        ]
    }

    fn ids<'a>(open: &[&'a Todo]) -> Vec<&'a str> {
        open.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn counts_own_claims_on_any_node_and_pending_on_its_node() {
        let todos = todos();
        let open = open_for(&todos, &Holder::new("S"), Some("N"));
        assert_eq!(ids(&open), ["ip-s-other", "p-n"]);
    }

    #[test]
    fn a_sub_agent_counts_only_its_claims() {
        let todos = todos();
        let open = open_for(&todos, &Holder::new("S/a1"), None);
        assert_eq!(ids(&open), ["ip-a1"]);
    }

    #[test]
    fn no_pending_node_counts_no_pending() {
        let todos = todos();
        let open = open_for(&todos, &Holder::new("S"), None);
        assert_eq!(ids(&open), ["ip-s-other"]);
    }

    #[test]
    fn open_todos_order_by_node_then_order() {
        let todos = vec![
            todo("n2", "N", 2, Status::Pending),
            todo("m", "M", 9, by("S")),
            todo("n1", "N", 1, Status::Pending),
        ];
        let open = open_for(&todos, &Holder::new("S"), Some("N"));
        assert_eq!(ids(&open), ["m", "n1", "n2"]);
    }

    #[test]
    fn fingerprint_ignores_order_and_tracks_status() {
        let a = todo("a", "N", 1, Status::Pending);
        let b = todo("b", "N", 2, by("S"));
        assert_eq!(fingerprint(&[&a, &b]), fingerprint(&[&b, &a]));
        assert_eq!(fingerprint(&[&a, &b]), "a pending\nb in_progress\n");
        let a_started = todo("a", "N", 1, by("S"));
        assert_ne!(fingerprint(&[&a, &b]), fingerprint(&[&a_started, &b]));
    }

    #[test]
    fn a_sessions_and_its_sub_agents_holds_share_one_dir() {
        let session = Holder::new("S");
        let sub = Holder::new("S/a1");
        assert_eq!(hold_path(&sub).parent(), Some(hold_dir(&session).as_path()));
        assert_eq!(
            hold_path(&session).parent(),
            Some(hold_dir(&session).as_path())
        );
        assert_ne!(hold_path(&sub), hold_path(&session));
        assert_ne!(hold_dir(&session), hold_dir(&Holder::new("T")));
    }
}
