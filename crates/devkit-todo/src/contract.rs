//! What every [`TodoStore`] owes, as cases a backend's tests run against its
//! own store through [`contract_tests!`](crate::contract_tests). Each case
//! starts from an empty store and uses only the ids `add` returns.

use crate::{
    Claimed, Edit, Filter, Holder, NewTodo, NodeMatch, Status, StatusChange, StatusKind, Todo,
    TodoStore,
    activity::{Activity, ActivityLog, ClaimEnd, Interval, Recorded},
};

/// One `#[test]` per contract case. `$make` builds a fresh store and a guard
/// that keeps its backing files alive for the test: `|| (guard, store)`.
///
/// `skip_unless $make` takes a `$make` returning `Option<(guard, store)>`;
/// each test returns early on `None`, for a backend whose program may be
/// absent.
#[macro_export]
macro_rules! contract_tests {
    (@cases $make:expr) => {
        $crate::contract_tests!(@each $make;
            add_then_list_round_trips,
            add_under_an_unknown_parent_is_refused,
            add_without_order_appends_after_the_last_sibling,
            descriptions_are_one_line,
            set_status_goes_through_transition,
            a_sub_agent_takes_over_its_sessions_claim,
            a_persons_claim_is_never_taken,
            undone_drops_the_holder,
            cancel_keeps_the_record,
            a_cancelled_todo_can_be_finished,
            release_all_returns_covered_claims_to_pending,
            purge_removes_the_record,
            unknown_ids_are_refused,
            get_returns_the_todo,
            move_reorder_and_relocate,
            filters_match_exact_and_subtree,
            apply_reports_each_status_change_it_made,
            claiming_then_completing_records_one_completed_interval,
            releasing_records_each_released_interval,
            a_handed_claim_closes_one_interval_and_opens_the_next,
        );
    };
    (@each $make:expr; $($case:ident),* $(,)?) => {
        $(
            #[test]
            fn $case() {
                let Some((_guard, store)) = ($make)() else {
                    return;
                };
                $crate::contract::$case(&store);
            }
        )*
    };
    (skip_unless $make:expr) => {
        $crate::contract_tests!(@cases $make);
    };
    ($make:expr) => {
        $crate::contract_tests!(@cases || Some(($make)()));
    };
}

/// A todo on `r.main` with no parent, placed after its last sibling.
fn new(description: &str) -> NewTodo {
    NewTodo {
        project: Some("r.main".into()),
        description: description.into(),
        parent: None,
        order: None,
    }
}

fn add(s: &impl TodoStore, description: &str) -> String {
    s.add(new(description)).unwrap()
}

fn get(s: &impl TodoStore, id: &str) -> Todo {
    s.get(id).unwrap().unwrap_or_else(|| panic!("no todo {id}"))
}

fn set(s: &impl TodoStore, id: &str, to: StatusKind, actor: &str) -> anyhow::Result<()> {
    s.apply(&Edit::SetStatus {
        id: id.into(),
        to,
        actor: Holder::new(actor),
    })
}

fn in_progress(by: &str) -> Status {
    Status::InProgress {
        by: Holder::new(by),
    }
}

fn claimant(err: &anyhow::Error) -> Option<String> {
    err.chain()
        .find_map(|e| e.downcast_ref::<Claimed>())
        .map(|c| c.by.to_string())
}

pub fn add_then_list_round_trips(s: &impl TodoStore) {
    let filed = add(s, "filed");
    let global = s
        .add(NewTodo {
            project: None,
            ..new("global")
        })
        .unwrap();
    let todos = s.list(&Filter::all()).unwrap();
    assert_eq!(todos.len(), 2, "{todos:?}");
    let filed = todos.iter().find(|t| t.id == filed).unwrap();
    assert_eq!(filed.description, "filed");
    assert_eq!(filed.project.as_deref(), Some("r.main"));
    assert_eq!(filed.status, Status::Pending);
    assert_eq!(filed.parent, None);
    assert!(
        filed.entry.is_some() && filed.modified.is_some(),
        "{filed:?}"
    );
    let global = todos.iter().find(|t| t.id == global).unwrap();
    assert_eq!(global.project, None);
}

pub fn add_under_an_unknown_parent_is_refused(s: &impl TodoStore) {
    let err = s
        .add(NewTodo {
            parent: Some("99999999".into()),
            ..new("orphan")
        })
        .unwrap_err();
    assert!(err.to_string().contains("no todo 99999999"), "{err:#}");
    assert!(s.list(&Filter::all()).unwrap().is_empty());
}

pub fn add_without_order_appends_after_the_last_sibling(s: &impl TodoStore) {
    let a = add(s, "a");
    let b = add(s, "b");
    let child = s
        .add(NewTodo {
            parent: Some(a.clone()),
            ..new("a1")
        })
        .unwrap();
    assert_eq!(get(s, &a).order, Some(1024));
    assert_eq!(get(s, &b).order, Some(2048));
    let child = get(s, &child);
    assert_eq!(child.order, Some(1024));
    assert_eq!(child.parent.as_deref(), Some(a.as_str()));
}

pub fn descriptions_are_one_line(s: &impl TodoStore) {
    let id = add(s, "a\n\tb  c");
    assert_eq!(get(s, &id).description, "a b c");
    s.apply(&Edit::Describe {
        id: id.clone(),
        description: "x\r\ny".into(),
    })
    .unwrap();
    assert_eq!(get(s, &id).description, "x y");
}

pub fn set_status_goes_through_transition(s: &impl TodoStore) {
    let id = add(s, "a");
    set(s, &id, StatusKind::InProgress, "S/a").unwrap();
    let err = set(s, &id, StatusKind::InProgress, "S/b").unwrap_err();
    assert_eq!(claimant(&err).as_deref(), Some("S/a"), "{err:#}");
    set(s, &id, StatusKind::InProgress, "S").unwrap();
    assert_eq!(get(s, &id).status, in_progress("S/a"));
    set(s, &id, StatusKind::Completed, "S").unwrap();
    assert_eq!(get(s, &id).status, Status::Completed {
        by: Some(Holder::new("S/a"))
    });
}

pub fn a_sub_agent_takes_over_its_sessions_claim(s: &impl TodoStore) {
    let id = add(s, "a");
    set(s, &id, StatusKind::InProgress, "S").unwrap();
    set(s, &id, StatusKind::InProgress, "S/a").unwrap();
    assert_eq!(get(s, &id).status, in_progress("S/a"));
}

pub fn a_persons_claim_is_never_taken(s: &impl TodoStore) {
    let id = add(s, "a");
    set(s, &id, StatusKind::InProgress, "human").unwrap();
    let err = set(s, &id, StatusKind::InProgress, "S").unwrap_err();
    assert_eq!(claimant(&err).as_deref(), Some("human"), "{err:#}");
    assert_eq!(get(s, &id).status, in_progress("human"));
}

pub fn undone_drops_the_holder(s: &impl TodoStore) {
    let id = add(s, "a");
    set(s, &id, StatusKind::InProgress, "S").unwrap();
    set(s, &id, StatusKind::Completed, "S").unwrap();
    set(s, &id, StatusKind::Pending, "S").unwrap();
    assert_eq!(get(s, &id).status, Status::Pending);
}

pub fn cancel_keeps_the_record(s: &impl TodoStore) {
    let id = add(s, "a");
    set(s, &id, StatusKind::Cancelled, "S").unwrap();
    let cancelled = Status::Cancelled {
        by: Some(Holder::new("S")),
    };
    assert_eq!(get(s, &id).status, cancelled);
    let listed = s.list(&Filter::all()).unwrap();
    assert_eq!(
        listed.iter().find(|t| t.id == id).map(|t| &t.status),
        Some(&cancelled)
    );
}

pub fn a_cancelled_todo_can_be_finished(s: &impl TodoStore) {
    let id = add(s, "a");
    set(s, &id, StatusKind::Cancelled, "S").unwrap();
    set(s, &id, StatusKind::Completed, "T").unwrap();
    assert_eq!(get(s, &id).status, Status::Completed {
        by: Some(Holder::new("T"))
    });
}

/// Several claims at once, as many as a bulk edit that might ask for
/// confirmation.
pub fn release_all_returns_covered_claims_to_pending(s: &impl TodoStore) {
    let own = add(s, "own");
    let subs: Vec<String> = ["a", "b", "c"]
        .map(|name| {
            let id = add(s, name);
            set(s, &id, StatusKind::InProgress, &format!("S/{name}")).unwrap();
            id
        })
        .into();
    let other = add(s, "other");
    set(s, &own, StatusKind::InProgress, "S").unwrap();
    set(s, &other, StatusKind::InProgress, "T").unwrap();
    s.apply(&Edit::ReleaseAll {
        holder: Holder::new("S"),
    })
    .unwrap();
    for id in subs.iter().chain([&own]) {
        assert_eq!(get(s, id).status, Status::Pending, "{id}");
    }
    assert_eq!(get(s, &other).status, in_progress("T"));
    s.apply(&Edit::ReleaseAll {
        holder: Holder::human(),
    })
    .unwrap();
    assert_eq!(get(s, &other).status, in_progress("T"));
}

pub fn purge_removes_the_record(s: &impl TodoStore) {
    let id = add(s, "secret");
    let cancelled = add(s, "cancelled secret");
    set(s, &cancelled, StatusKind::Cancelled, "S").unwrap();
    s.apply(&Edit::Purge(id.clone())).unwrap();
    s.apply(&Edit::Purge(cancelled.clone())).unwrap();
    assert_eq!(s.get(&id).unwrap(), None);
    assert_eq!(s.get(&cancelled).unwrap(), None);
    assert!(s.list(&Filter::all()).unwrap().is_empty());
    let err = s.apply(&Edit::Purge("99999999".into())).unwrap_err();
    assert!(err.to_string().contains("no todo 99999999"), "{err:#}");
}

pub fn unknown_ids_are_refused(s: &impl TodoStore) {
    let id = add(s, "a");
    let err = s
        .apply(&Edit::Describe {
            id: "99999999".into(),
            description: "x".into(),
        })
        .unwrap_err();
    assert!(err.to_string().contains("no todo 99999999"), "{err:#}");
    let err = set(s, "99999999", StatusKind::Completed, "S").unwrap_err();
    assert!(err.to_string().contains("no todo 99999999"), "{err:#}");
    assert_eq!(s.get("99999999").unwrap(), None);
    assert_eq!(s.get("not an id").unwrap(), None);
    assert_eq!(get(s, &id).description, "a");
}

pub fn get_returns_the_todo(s: &impl TodoStore) {
    add(s, "other");
    let id = add(s, "wanted");
    let listed = s
        .list(&Filter::all())
        .unwrap()
        .into_iter()
        .find(|t| t.id == id)
        .unwrap();
    assert_eq!(get(s, &id), listed);
}

pub fn move_reorder_and_relocate(s: &impl TodoStore) {
    let parent = add(s, "parent");
    let child = |text: &str| NewTodo {
        parent: Some(parent.clone()),
        ..new(text)
    };
    s.add(child("a")).unwrap();
    s.add(child("b")).unwrap();
    let loose = add(s, "loose");
    s.apply(&Edit::Move {
        id: loose.clone(),
        parent: Some(parent.clone()),
        order: None,
    })
    .unwrap();
    let moved = get(s, &loose);
    assert_eq!(moved.parent.as_deref(), Some(parent.as_str()));
    assert_eq!(moved.order, Some(3 * 1024));
    s.apply(&Edit::Reorder {
        id: loose.clone(),
        order: 7,
    })
    .unwrap();
    assert_eq!(get(s, &loose).order, Some(7));
    s.apply(&Edit::Move {
        id: loose.clone(),
        parent: None,
        order: Some(9),
    })
    .unwrap();
    let top = get(s, &loose);
    assert_eq!((top.parent, top.order), (None, Some(9)));
    s.apply(&Edit::Relocate {
        id: loose.clone(),
        project: Some("r.other".into()),
    })
    .unwrap();
    assert_eq!(get(s, &loose).project.as_deref(), Some("r.other"));
    s.apply(&Edit::Relocate {
        id: loose.clone(),
        project: None,
    })
    .unwrap();
    assert_eq!(get(s, &loose).project, None);
}

pub fn filters_match_exact_and_subtree(s: &impl TodoStore) {
    for project in ["r", "r.main", "r-web"] {
        s.add(NewTodo {
            project: Some(project.into()),
            ..new(project)
        })
        .unwrap();
    }
    let projects = |filter: Filter| {
        let mut out: Vec<String> = s
            .list(&filter)
            .unwrap()
            .into_iter()
            .filter_map(|t| t.project)
            .collect();
        out.sort();
        out
    };
    let subtree = Filter {
        nodes: vec![NodeMatch::Subtree("r".into())],
    };
    assert_eq!(projects(subtree), ["r", "r.main"]);
    assert_eq!(projects(Filter::exact(["r".to_string()])), ["r"]);
    assert_eq!(projects(Filter::all()), ["r", "r-web", "r.main"]);
    assert!(projects(Filter { nodes: Vec::new() }).is_empty());
}

/// Each change as `(todo, from, to)`, sorted by todo.
fn changed(changes: Vec<StatusChange>) -> Vec<(String, Status, Option<Status>)> {
    for change in &changes {
        assert_eq!(change.node, "r.main", "{change:?}");
    }
    let mut out: Vec<_> = changes
        .into_iter()
        .map(|c| (c.todo, c.from, c.to))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn claim_edit(id: &str, by: &str) -> Edit {
    Edit::SetStatus {
        id: id.into(),
        to: StatusKind::InProgress,
        actor: Holder::new(by),
    }
}

pub fn apply_reports_each_status_change_it_made(s: &impl TodoStore) {
    let (a, b) = (add(s, "a"), add(s, "b"));
    assert_eq!(changed(s.apply(&claim_edit(&a, "S")).unwrap()), [(
        a.clone(),
        Status::Pending,
        Some(in_progress("S"))
    )]);
    assert!(s.apply(&claim_edit(&a, "S")).unwrap().is_empty());
    let described = s.apply(&Edit::Describe {
        id: a.clone(),
        description: "a2".into(),
    });
    assert!(described.unwrap().is_empty());
    assert_eq!(changed(s.apply(&claim_edit(&a, "S/x")).unwrap()), [(
        a.clone(),
        in_progress("S"),
        Some(in_progress("S/x"))
    )]);
    s.apply(&claim_edit(&b, "S")).unwrap();
    let release = Edit::ReleaseAll {
        holder: Holder::new("S"),
    };
    let mut want = vec![
        (a.clone(), in_progress("S/x"), Some(Status::Pending)),
        (b.clone(), in_progress("S"), Some(Status::Pending)),
    ];
    want.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(changed(s.apply(&release).unwrap()), want);
    s.apply(&claim_edit(&a, "S")).unwrap();
    assert_eq!(changed(s.apply(&Edit::Purge(a.clone())).unwrap()), [(
        a,
        in_progress("S"),
        None
    )]);
}

/// `s` recording into a fresh log, and the guard that keeps the log alive.
fn recorded<S: TodoStore>(s: S) -> (tempfile::TempDir, Recorded<S>) {
    let dir = tempfile::tempdir().unwrap();
    let log = ActivityLog::at(dir.path().to_path_buf());
    (dir, Recorded::new(s, log))
}

fn claims<S>(r: &Recorded<S>) -> Vec<Interval> {
    let Activity { claims, .. } = r.log().read(std::time::SystemTime::now().into()).unwrap();
    claims
}

fn ended(claim: &Interval) -> (&str, &str, Option<ClaimEnd>) {
    assert!(claim.end.is_none_or(|end| end >= claim.start), "{claim:?}");
    (&claim.todo, &claim.holder, claim.outcome)
}

pub fn claiming_then_completing_records_one_completed_interval(s: &impl TodoStore) {
    let (_dir, r) = recorded(s);
    let id = add(&r, "a");
    set(&r, &id, StatusKind::InProgress, "S/a").unwrap();
    set(&r, &id, StatusKind::Completed, "S/a").unwrap();
    let claims = claims(&r);
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert_eq!(
        ended(&claims[0]),
        (id.as_str(), "S/a", Some(ClaimEnd::Completed))
    );
    assert!(claims[0].end.is_some());
    assert_eq!(claims[0].node, "r.main");
}

pub fn releasing_records_each_released_interval(s: &impl TodoStore) {
    let (_dir, r) = recorded(s);
    let (own, sub, other) = (add(&r, "own"), add(&r, "sub"), add(&r, "other"));
    set(&r, &own, StatusKind::InProgress, "S").unwrap();
    set(&r, &sub, StatusKind::InProgress, "S/a").unwrap();
    set(&r, &other, StatusKind::InProgress, "T").unwrap();
    r.apply(&Edit::ReleaseAll {
        holder: Holder::new("S"),
    })
    .unwrap();
    let claims = claims(&r);
    let outcome = |id: &str| {
        claims
            .iter()
            .find(|c| c.todo == id)
            .map(|c| ended(c).2)
            .unwrap_or_else(|| panic!("no interval for {id}: {claims:?}"))
    };
    assert_eq!(outcome(&own), Some(ClaimEnd::Released));
    assert_eq!(outcome(&sub), Some(ClaimEnd::Released));
    assert_eq!(outcome(&other), None);
}

pub fn a_handed_claim_closes_one_interval_and_opens_the_next(s: &impl TodoStore) {
    let (_dir, r) = recorded(s);
    let id = add(&r, "a");
    set(&r, &id, StatusKind::InProgress, "S").unwrap();
    set(&r, &id, StatusKind::InProgress, "S/a").unwrap();
    let claims = claims(&r);
    assert_eq!(claims.len(), 2, "{claims:?}");
    assert_eq!(
        ended(&claims[0]),
        (id.as_str(), "S", Some(ClaimEnd::Handed))
    );
    assert_eq!(ended(&claims[1]), (id.as_str(), "S/a", None));
    assert_eq!(claims[0].end, Some(claims[1].start));
}
