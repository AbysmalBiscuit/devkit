use std::{fs, path::Path, thread};

use devkit_todo::{Filter, NewTodo, Todo, TodoStore};
use devkit_todo_builtin::BuiltinStore;

devkit_todo::contract_tests!(|| {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    (dir, store)
});

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
fn a_refused_edit_leaves_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = BuiltinStore::at(dir.path().to_path_buf());
    store.add(new("a")).unwrap();
    let before = fs::read(todos_file(dir.path())).unwrap();
    store
        .apply(&devkit_todo::Edit::Purge("99".into()))
        .unwrap_err();
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
