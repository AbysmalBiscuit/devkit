mod common;

use common::{add_by_hand, exported, private, task};
use devkit_todo::{
    Claimed, Edit, Filter, Holder, NewTodo, NodeMatch, Status, StatusKind, TodoStore,
};
use devkit_todo_taskwarrior::{TaskwarriorNotFound, TaskwarriorStore};

devkit_todo::contract_tests!(skip_unless common::private);

fn new(project: Option<&str>, description: &str) -> NewTodo {
    NewTodo {
        project: project.map(Into::into),
        description: description.into(),
        parent: None,
        order: None,
    }
}

fn start(store: &TaskwarriorStore, id: &str, actor: &str) -> anyhow::Result<()> {
    store.apply(&Edit::SetStatus {
        id: id.into(),
        to: StatusKind::InProgress,
        actor: Holder::new(actor),
    })
}

#[test]
fn attribute_syntax_in_a_description_is_verbatim() {
    let Some((dir, store)) = private() else {
        return;
    };
    let texts = [
        "fix project:x +tag due:tomorrow",
        "-- -lead",
        "-lead",
        r"open C:\Users\me\x.log on \\.\pipe\a and a\",
    ];
    for text in texts {
        let id = store.add(new(Some("r.main"), text)).unwrap();
        let todo = store.get(&id).unwrap().unwrap();
        assert_eq!(todo.description, text);
        assert_eq!(todo.project.as_deref(), Some("r.main"));
        let raw = exported(dir.path(), &id);
        assert_eq!(raw["tags"], serde_json::Value::Null, "{raw}");
        assert_eq!(raw["due"], serde_json::Value::Null, "{raw}");
        let replaced = format!("now {text}");
        store
            .apply(&Edit::Describe {
                id: id.clone(),
                description: replaced.clone(),
            })
            .unwrap();
        assert_eq!(store.get(&id).unwrap().unwrap().description, replaced);
        assert_eq!(exported(dir.path(), &id)["project"], "devkit.r.main");
    }
}

#[test]
fn a_hand_started_task_is_held_by_human() {
    let Some((dir, store)) = private() else {
        return;
    };
    let uuid = add_by_hand(dir.path(), &["project:devkit.r.main", "--", "t"]);
    task(dir.path(), &[&uuid, "start"]);
    assert_eq!(
        store.get(&uuid).unwrap().unwrap().status,
        Status::InProgress {
            by: Holder::human()
        }
    );
    let err = start(&store, &uuid, "S").unwrap_err();
    assert_eq!(
        err.chain()
            .find_map(|e| e.downcast_ref::<Claimed>())
            .map(|c| c.by.to_string())
            .as_deref(),
        Some("human"),
        "{err:#}"
    );
}

#[test]
fn unfiled_and_unrelated_tasks_stay_out() {
    let Some((dir, store)) = private() else {
        return;
    };
    let personal = add_by_hand(dir.path(), &["--", "personal"]);
    task(dir.path(), &[&personal, "start"]);
    let chores = add_by_hand(dir.path(), &["project:home", "--", "chores"]);
    task(dir.path(), &[&chores, "start"]);
    let todo = store.add(new(Some("r.main"), "todo")).unwrap();
    start(&store, &todo, "S").unwrap();

    let listed: Vec<String> = store
        .list(&Filter::all())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert!(!listed.contains(&personal), "{listed:?}");
    let subtree = Filter {
        nodes: vec![NodeMatch::Subtree("r".into())],
    };
    let in_r: Vec<String> = store
        .list(&subtree)
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(in_r, std::slice::from_ref(&todo));

    assert_eq!(store.get(&personal).unwrap(), None);
    let err = start(&store, &personal, "S").unwrap_err();
    assert!(err.to_string().contains("no todo"), "{err:#}");
    let err = start(&store, &chores, "S").unwrap_err();
    assert!(err.to_string().contains("no todo"), "{err:#}");

    store
        .apply(&Edit::ReleaseAll {
            holder: Holder::new("S"),
        })
        .unwrap();
    assert_eq!(store.get(&todo).unwrap().unwrap().status, Status::Pending);
    for uuid in [&personal, &chores] {
        let raw = exported(dir.path(), uuid);
        assert!(raw["start"].is_string(), "{raw}");
        assert_eq!(raw["holder"], serde_json::Value::Null, "{raw}");
    }
}

#[test]
fn a_short_id_resolves_and_an_ambiguous_one_fails() {
    let Some((dir, store)) = private() else {
        return;
    };
    let id = store.add(new(Some("r"), "one")).unwrap();
    assert_eq!(store.get(&id[..8]).unwrap(), store.get(&id).unwrap());
    assert_eq!(store.get(&id[..7]).unwrap(), None);
    start(&store, &id[..8], "S").unwrap();
    assert_eq!(
        store.get(&id).unwrap().unwrap().status.kind(),
        StatusKind::InProgress
    );

    let twins = r#"[
        {"uuid":"abcdef12-0000-4000-8000-000000000001","description":"a","status":"pending","project":"devkit.r","entry":"20261003T120000Z"},
        {"uuid":"abcdef12-0000-4000-8000-000000000002","description":"b","status":"pending","project":"devkit.r","entry":"20261003T120000Z"}
    ]"#;
    let file = dir.path().join("twins.json");
    std::fs::write(&file, twins).unwrap();
    task(dir.path(), &["import", file.to_str().unwrap()]);
    let err = store.get("abcdef12").unwrap_err();
    assert_eq!(
        err.to_string(),
        "todo id abcdef12 is ambiguous: abcdef12-0000-4000-8000-000000000001, \
         abcdef12-0000-4000-8000-000000000002"
    );
    assert!(start(&store, "abcdef12", "S").is_err());
    assert!(
        store
            .get("abcdef12-0000-4000-8000-000000000002")
            .unwrap()
            .is_some()
    );
}

#[test]
fn a_missing_program_names_the_config_key() {
    let err = TaskwarriorStore::new("/nonexistent/task")
        .list(&Filter::all())
        .unwrap_err();
    assert!(
        err.downcast_ref::<TaskwarriorNotFound>().is_some(),
        "{err:#}"
    );
    assert_eq!(
        err.to_string(),
        "taskwarrior not found: /nonexistent/task ([todo.taskwarrior] path)"
    );
}

#[test]
fn todos_live_under_the_root_project() {
    let Some((dir, store)) = private() else {
        return;
    };
    let global = store.add(new(None, "wide")).unwrap();
    assert_eq!(exported(dir.path(), &global)["project"], "devkit");
    assert_eq!(store.get(&global).unwrap().unwrap().project, None);
    let filed = store.add(new(Some("r.main"), "narrow")).unwrap();
    assert_eq!(exported(dir.path(), &filed)["project"], "devkit.r.main");
    assert_eq!(
        store.get(&filed).unwrap().unwrap().project.as_deref(),
        Some("r.main")
    );
}

/// Tasks a person keeps outside the root, even on a project named like a
/// node, are never todos.
#[test]
fn tasks_outside_the_root_are_not_todos() {
    let Some((dir, store)) = private() else {
        return;
    };
    let chores = add_by_hand(dir.path(), &["project:home", "--", "chores"]);
    let lookalike = add_by_hand(dir.path(), &["project:proj.main", "--", "lookalike"]);
    let todo = store.add(new(Some("proj.main"), "todo")).unwrap();
    for filter in [
        Filter::all(),
        Filter::exact(["proj.main".to_string()]),
        Filter {
            nodes: vec![NodeMatch::Subtree("proj".into())],
        },
    ] {
        let listed: Vec<String> = store
            .list(&filter)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(listed, std::slice::from_ref(&todo), "{filter:?}");
    }
    for outside in [&chores, &lookalike] {
        assert_eq!(store.get(outside).unwrap(), None);
        assert_eq!(store.get(&outside[..8]).unwrap(), None);
        let err = start(&store, outside, "S").unwrap_err();
        assert!(err.to_string().contains("no todo"), "{err:#}");
        assert_eq!(
            exported(dir.path(), outside)["start"],
            serde_json::Value::Null
        );
    }
}

#[test]
fn a_configured_root_scopes_the_todos() {
    let Some((dir, store)) = private() else {
        return;
    };
    let store = store.with_root("agents");
    let id = store.add(new(Some("r"), "one")).unwrap();
    assert_eq!(exported(dir.path(), &id)["project"], "agents.r");
    let default_root = add_by_hand(dir.path(), &["project:devkit.r", "--", "other root"]);
    let listed: Vec<String> = store
        .list(&Filter::all())
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(listed, [id]);
    assert_eq!(store.get(&default_root).unwrap(), None);
}

#[test]
fn claims_are_stored_as_start_and_holder() {
    let Some((dir, store)) = private() else {
        return;
    };
    let id = store.add(new(Some("r"), "one")).unwrap();
    start(&store, &id, "S").unwrap();
    let raw = exported(dir.path(), &id);
    assert_eq!(
        (raw["status"].as_str(), raw["holder"].as_str()),
        (Some("pending"), Some("S"))
    );
    assert!(raw["start"].is_string(), "{raw}");
    task(dir.path(), &[&id, "modify", "start:20200101T000000Z"]);
    start(&store, &id, "S/a").unwrap();
    let raw = exported(dir.path(), &id);
    assert_eq!(raw["holder"], "S/a");
    assert_eq!(
        raw["start"], "20200101T000000Z",
        "a handed-down claim keeps its start"
    );
}
