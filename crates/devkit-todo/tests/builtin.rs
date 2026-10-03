use std::{fs, path::Path, thread};

use devkit_todo::{
    BuiltinStore, Claimed, Edit, Filter, Holder, NewTodo, Status, StatusKind, Todo, TodoStore,
};

fn new(description: &str) -> NewTodo {
    NewTodo {
        project: Some("r.main".into()),
        description: description.into(),
        parent: None,
        order: None,
    }
}

fn all(store: &BuiltinStore) -> Vec<Todo> {
    store.list(&Filter::all()).unwrap()
}

fn get(store: &BuiltinStore, id: &str) -> Todo {
    all(store).into_iter().find(|t| t.id == id).unwrap()
}

fn start(store: &BuiltinStore, id: &str, actor: &str) -> anyhow::Result<()> {
    store.apply(&Edit::SetStatus {
        id: id.into(),
        to: StatusKind::InProgress,
        actor: Holder::new(actor),
    })
}

fn todos_file(dir: &Path) -> std::path::PathBuf {
    dir.join("todos.json")
}

#[test]
fn ids_are_sequential_and_sort_numerically() {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    for i in 1..=10 {
        assert_eq!(store.add(new(&format!("t{i}"))).unwrap(), i.to_string());
    }
    let ids: Vec<String> = all(&store).into_iter().map(|t| t.id).collect();
    let want: Vec<String> = (1..=10).map(|i| i.to_string()).collect();
    assert_eq!(ids, want);
}

#[test]
fn add_without_order_appends_after_the_last_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    store.add(new("a")).unwrap();
    store.add(new("b")).unwrap();
    let child = store
        .add(NewTodo {
            parent: Some("1".into()),
            ..new("a1")
        })
        .unwrap();
    assert_eq!(get(&store, "1").order, Some(1024));
    assert_eq!(get(&store, "2").order, Some(2048));
    assert_eq!(get(&store, &child).order, Some(1024));
}

#[test]
fn descriptions_are_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    let id = store.add(new("a\n\tb  c")).unwrap();
    assert_eq!(get(&store, &id).description, "a b c");
    store
        .apply(&Edit::Describe {
            id: id.clone(),
            description: "x\r\ny".into(),
        })
        .unwrap();
    assert_eq!(get(&store, &id).description, "x y");
}

#[test]
fn set_status_goes_through_transition() {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    store.add(new("a")).unwrap();
    start(&store, "1", "S/a1").unwrap();
    let err = start(&store, "1", "S/a2").unwrap_err();
    assert_eq!(
        err.downcast_ref::<Claimed>().map(|c| c.by.to_string()),
        Some("S/a1".to_string())
    );
    assert_eq!(get(&store, "1").status, Status::InProgress {
        by: Holder::new("S/a1")
    });
}

#[test]
fn release_all_returns_covered_claims_to_pending() {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    store.add(new("a")).unwrap();
    store.add(new("b")).unwrap();
    start(&store, "1", "S/a1").unwrap();
    start(&store, "2", "T").unwrap();
    store
        .apply(&Edit::ReleaseAll {
            holder: Holder::new("S"),
        })
        .unwrap();
    assert_eq!(get(&store, "1").status, Status::Pending);
    assert_eq!(get(&store, "2").status, Status::InProgress {
        by: Holder::new("T")
    });
    store
        .apply(&Edit::ReleaseAll {
            holder: Holder::human(),
        })
        .unwrap();
    assert_eq!(get(&store, "2").status.kind(), StatusKind::InProgress);
}

#[test]
fn purge_removes_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    store.add(new("secret")).unwrap();
    store.apply(&Edit::Purge("1".into())).unwrap();
    assert!(all(&store).is_empty());
    let err = store.apply(&Edit::Purge("7".into())).unwrap_err();
    assert_eq!(err.to_string(), "no todo 7");
}

#[test]
fn unknown_ids_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    store.add(new("a")).unwrap();
    let before = fs::read(todos_file(dir.path())).unwrap();
    let err = store
        .apply(&Edit::Describe {
            id: "99".into(),
            description: "x".into(),
        })
        .unwrap_err();
    assert!(err.to_string().contains("no todo 99"), "{err:#}");
    assert_eq!(fs::read(todos_file(dir.path())).unwrap(), before);
}

#[test]
fn a_corrupt_file_aborts_and_stays() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(todos_file(dir.path()), "{not json").unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    assert!(store.add(new("a")).is_err());
    assert!(store.list(&Filter::all()).is_err());
    assert_eq!(
        fs::read_to_string(todos_file(dir.path())).unwrap(),
        "{not json"
    );
}

#[test]
fn concurrent_adds_all_land() {
    let dir = tempfile::tempdir().unwrap();
    let handles: Vec<_> = (0..8)
        .map(|t| {
            let path = dir.path().to_path_buf();
            thread::spawn(move || {
                let store = BuiltinStore::at(path);
                (0..25)
                    .map(|i| store.add(new(&format!("{t}-{i}"))).unwrap())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut ids: Vec<String> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 200);
    assert_eq!(all(&BuiltinStore::at(dir.path().to_path_buf())).len(), 200);
}
