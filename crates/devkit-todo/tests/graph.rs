//! The blocking graph, built from todos directly: what is ready, how the
//! plan layers, and which edges close a cycle.

use std::num::NonZeroUsize;

use devkit_todo::{Holder, Status, Todo, graph::Graph};

fn pending(id: &str, blocked_by: &[&str]) -> Todo {
    todo(id, Status::Pending, blocked_by, None)
}

fn todo(id: &str, status: Status, blocked_by: &[&str], order: Option<i64>) -> Todo {
    Todo {
        id: id.into(),
        description: id.into(),
        status,
        parent: None,
        order,
        project: Some("r.main".into()),
        entry: None,
        modified: None,
        blocked_by: blocked_by.iter().map(|id| (*id).to_string()).collect(),
    }
}

fn ids(todos: &[&Todo]) -> Vec<String> {
    let mut out: Vec<String> = todos.iter().map(|todo| todo.id.clone()).collect();
    out.sort();
    out
}

fn batches(plan: &devkit_todo::graph::Plan<'_>) -> Vec<Vec<String>> {
    plan.batches.iter().map(|batch| ids(batch)).collect()
}

#[test]
fn a_todo_with_one_finished_blocker_is_ready_and_one_with_two_waits() {
    let todos = vec![
        todo("1", Status::Completed { by: None }, &[], None),
        pending("2", &[]),
        pending("4", &["1"]),
        pending("5", &["1", "2"]),
    ];
    let graph = Graph::new(&todos);
    assert!(graph.is_ready(&todos[2]));
    assert!(!graph.is_ready(&todos[3]));
    assert_eq!(ids(&graph.unblocked_by(&["1"])), ["4"]);
}

#[test]
fn in_progress_blockers_block_and_closed_or_missing_ones_do_not() {
    let todos = vec![
        todo(
            "1",
            Status::InProgress {
                by: Holder::new("S"),
            },
            &[],
            None,
        ),
        todo("2", Status::Cancelled { by: None }, &[], None),
        todo("3", Status::Completed { by: None }, &[], None),
        pending("4", &["1"]),
        pending("5", &["2"]),
        pending("6", &["3"]),
        pending("7", &["missing"]),
    ];
    let graph = Graph::new(&todos);
    assert!(graph.is_blocked(&todos[3]));
    assert!(!graph.is_blocked(&todos[4]));
    assert!(!graph.is_blocked(&todos[5]));
    assert!(!graph.is_blocked(&todos[6]));
    assert!(graph.is_ready(&todos[6]));
}

/// The spec's sample plan: A done; G in progress; B and H after A; C, D and
/// E after B; F after E; I after G and H; J after G; K after H; L after
/// F, I, J and K.
fn sample() -> Vec<Todo> {
    let done = Status::Completed { by: None };
    let working = Status::InProgress {
        by: Holder::new("S"),
    };
    vec![
        todo("A", done, &[], Some(1)),
        todo("G", working, &[], Some(2)),
        pending("B", &["A"]),
        pending("H", &["A"]),
        pending("C", &["B"]),
        pending("D", &["B"]),
        pending("E", &["B"]),
        pending("F", &["E"]),
        pending("I", &["G", "H"]),
        pending("J", &["G"]),
        pending("K", &["H"]),
        pending("L", &["F", "I", "J", "K"]),
    ]
}

#[test]
fn batches_layer_by_open_blockers() {
    let todos = sample();
    let plan = Graph::new(&todos).plan(|_| true, None);
    assert_eq!(batches(&plan), [
        vec!["B", "G", "H"],
        vec!["C", "D", "E", "I", "J", "K"],
        vec!["F"],
        vec!["L"],
    ]);
    assert_eq!(ids(&plan.critical), ["B", "E", "F", "L"]);
}

#[test]
fn a_cap_fills_batches_by_longest_chain() {
    let todos = sample();
    let plan = Graph::new(&todos).plan(|_| true, NonZeroUsize::new(3));
    assert_eq!(batches(&plan)[0], ["B", "G", "H"]);
    assert_eq!(batches(&plan)[1][0], "E");
}

#[test]
fn cycle_members_are_reported_not_dropped() {
    let todos = vec![
        pending("1", &["2"]),
        pending("2", &["1"]),
        pending("3", &[]),
    ];
    let graph = Graph::new(&todos);
    let plan = graph.plan(|_| true, None);
    assert_eq!(batches(&plan), [vec!["3"]]);
    assert_eq!(ids(&plan.cycle), ["1", "2"]);
    assert!(!graph.is_ready(&todos[0]));
    assert!(!graph.is_ready(&todos[1]));
}

#[test]
fn cycle_through_names_the_cycle() {
    let todos = vec![pending("1", &[]), pending("2", &["1"])];
    assert_eq!(
        devkit_todo::graph::cycle_through(&todos, "1", &["2".into()]),
        Some(vec!["1".into(), "2".into(), "1".into()])
    );
    assert_eq!(
        devkit_todo::graph::cycle_through(&todos, "1", &["1".into()]),
        Some(vec!["1".into(), "1".into()])
    );
    assert_eq!(
        devkit_todo::graph::cycle_through(&todos, "2", &["3".into()]),
        None
    );
}

#[test]
fn frontier_puts_the_critical_path_first() {
    let todos = sample();
    let plan = Graph::new(&todos).plan(|_| true, None);
    let frontier = plan.frontier();
    assert_eq!(ids(&frontier), ["B", "H"]);
    assert_eq!(frontier[0].id, "B");
}
