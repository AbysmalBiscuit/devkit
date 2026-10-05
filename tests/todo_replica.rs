#[path = "common/todoenv.rs"]
mod todoenv;

use devkit_todo::{Filter, TodoStore};
use devkit_todo_taskchampion::TaskchampionStore;
use todoenv::{Proj, stderr};

fn add_at(p: &Proj, data: &std::path::Path, env: &[(&str, &str)]) {
    let out = p.devkit(&["todo", "add", "shared todo"], env);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        data.join("taskchampion.sqlite3").exists(),
        "{}",
        data.display()
    );
    let todos = TaskchampionStore::at(data.to_path_buf())
        .list(&Filter::all())
        .unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].description, "shared todo");
}

#[test]
fn home_taskrc_wins_over_xdg_and_expands_tilde() {
    let p = Proj::with_home_config("[todo]\nbackend = \"taskchampion\"\n");
    let home = p.state().parent().unwrap().to_path_buf();
    let xdg = home.join("xdg");
    std::fs::create_dir_all(xdg.join("task")).unwrap();
    std::fs::write(xdg.join("task/taskrc"), "data.location=~/wrong\n").unwrap();
    std::fs::write(home.join(".taskrc"), "data.location=~/tasks\n").unwrap();
    add_at(&p, &home.join("tasks"), &[(
        "XDG_CONFIG_HOME",
        xdg.to_str().unwrap(),
    )]);
    assert!(!home.join("wrong").exists());
}

#[test]
fn xdg_taskrc_is_discovered_without_home_taskrc() {
    for custom in [false, true] {
        let p = Proj::with_home_config("[todo]\nbackend = \"taskchampion\"\n");
        let home = p.state().parent().unwrap().to_path_buf();
        let xdg = home.join(if custom { "xdg" } else { ".config" });
        std::fs::create_dir_all(xdg.join("task")).unwrap();
        std::fs::write(xdg.join("task/taskrc"), "data.location=~/tasks\n").unwrap();
        let env = if custom {
            vec![("XDG_CONFIG_HOME", xdg.to_str().unwrap())]
        } else {
            vec![]
        };
        add_at(&p, &home.join("tasks"), &env);
    }
}

#[test]
fn explicit_data_dir_wins_without_reading_taskrc() {
    let p = Proj::new();
    let home = p.state().parent().unwrap().to_path_buf();
    let taskrc = home.join("broken.taskrc");
    std::fs::write(&taskrc, "malformed\n").unwrap();
    let p = p.home_config_of(
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\ndata_dir = \"~/configured\"\n",
    );
    add_at(&p, &home.join("configured"), &[
        ("TASKDATA", "~/wrong"),
        ("TASKRC", taskrc.to_str().unwrap()),
    ]);
    assert!(!home.join("wrong").exists());
    let out = p.devkit(&["todo", "list"], &[("TASKRC", taskrc.to_str().unwrap())]);
    assert!(!stderr(&out).contains("malformed"), "{}", stderr(&out));
}

#[test]
fn taskdata_wins_without_reading_taskrc() {
    let p = Proj::with_home_config("[todo]\nbackend = \"taskchampion\"\n");
    let home = p.state().parent().unwrap().to_path_buf();
    std::fs::write(home.join(".taskrc"), "malformed\n").unwrap();
    add_at(&p, &home.join("tasks"), &[("TASKDATA", "~/tasks")]);
    let out = p.devkit(&["todo", "list"], &[("TASKDATA", "~/tasks")]);
    assert!(!stderr(&out).contains("malformed"), "{}", stderr(&out));
}

#[test]
fn invalid_taskrc_warns_and_discards_partial_data_location() {
    for invalid in ["malformed", "include missing.rc", "include taskrc"] {
        let p = Proj::with_home_config("[todo]\nbackend = \"taskchampion\"\n");
        let home = p.state().parent().unwrap().to_path_buf();
        let taskrc = home.join("taskrc");
        std::fs::write(&taskrc, format!("data.location=~/wrong\n{invalid}\n")).unwrap();
        let out = p.devkit(&["todo", "add", "after invalid taskrc"], &[(
            "TASKRC",
            taskrc.to_str().unwrap(),
        )]);
        assert!(out.status.success(), "{}", stderr(&out));
        assert!(
            stderr(&out).contains("using the default replica"),
            "{}",
            stderr(&out)
        );
        assert!(
            stderr(&out).contains(taskrc.to_str().unwrap()),
            "{}",
            stderr(&out)
        );
        assert!(!home.join("wrong").exists());
        let todos = TaskchampionStore::at(p.state().join("todo/taskchampion"))
            .list(&Filter::all())
            .unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].description, "after invalid taskrc");
    }
}

#[test]
fn later_assignments_override_includes_and_empty_unsets_data_location() {
    for value in ["~/tasks", ""] {
        let p = Proj::with_home_config("[todo]\nbackend = \"taskchampion\"\n");
        let home = p.state().parent().unwrap().to_path_buf();
        std::fs::write(home.join("included.rc"), "data.location=~/wrong\n").unwrap();
        std::fs::write(
            home.join(".taskrc"),
            format!("include included.rc\ndata.location={value}\n"),
        )
        .unwrap();
        let expected = if value.is_empty() {
            p.state().join("todo/taskchampion")
        } else {
            home.join("tasks")
        };
        add_at(&p, &expected, &[]);
        assert!(!home.join("wrong").exists());
    }
}

#[test]
fn doctor_names_the_replica_and_all_provenance_sources() {
    let p = Proj::with_home_config("[todo]\nbackend = \"taskchampion\"\n");
    let home = p.state().parent().unwrap().to_path_buf();
    let check = |expected: &std::path::Path, origin: &str, env: &[(&str, &str)]| {
        let out = p.devkit(&["doctor", "--json"], env);
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
        let row = rows
            .iter()
            .find(|row| row["key"] == "todo_replica")
            .expect("doctor's replica row");
        assert_eq!(row["dir"], expected.to_str().unwrap());
        assert_eq!(row["origin"], origin);
        let detail = row["detail"].as_str().unwrap();
        assert!(detail.contains(expected.to_str().unwrap()), "{detail}");
        assert!(detail.contains(origin), "{detail}");
    };
    check(&p.state().join("todo/taskchampion"), "default", &[]);
    std::fs::write(home.join(".taskrc"), "data.location=~/taskrc-tasks\n").unwrap();
    check(&home.join("taskrc-tasks"), "taskrc", &[]);
    check(&home.join("env-tasks"), "TASKDATA", &[(
        "TASKDATA",
        "~/env-tasks",
    )]);
    std::fs::write(
        p.home_config(),
        "[todo]\nbackend = \"taskchampion\"\n[todo.taskchampion]\ndata_dir = \"~/config-tasks\"\n",
    )
    .unwrap();
    check(&home.join("config-tasks"), "config", &[(
        "TASKDATA",
        "~/env-tasks",
    )]);
}

#[test]
fn taskrc_includes_select_the_replica() {
    let p = Proj::with_home_config("[todo]\nbackend = \"taskchampion\"\n");
    let home = p.state().parent().unwrap().to_path_buf();
    std::fs::write(home.join(".taskrc"), "data.location=~/wrong\n").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let taskrc = dir.path().join("taskrc");
    std::fs::write(&taskrc, "data.location=/ignored\ninclude included.rc\n").unwrap();
    std::fs::write(
        dir.path().join("included.rc"),
        "include $EXTRA_TASKRC\nsync.server.url=https://ignored.invalid\n",
    )
    .unwrap();
    let extra = dir.path().join("extra.rc");
    std::fs::write(
        &extra,
        "# Replica shared with Taskwarrior\ndata.location=$REPLICA_ROOT/tasks # comment\n",
    )
    .unwrap();
    let data = dir.path().join("tasks");
    let out = p.devkit(&["todo", "add", "from the included taskrc"], &[
        ("TASKRC", taskrc.to_str().unwrap()),
        ("REPLICA_ROOT", dir.path().to_str().unwrap()),
        ("EXTRA_TASKRC", extra.to_str().unwrap()),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        data.join("taskchampion.sqlite3").exists(),
        "{}",
        data.display()
    );
    let todos = TaskchampionStore::at(data).list(&Filter::all()).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].description, "from the included taskrc");
    assert!(!stderr(&out).contains("sync"), "{}", stderr(&out));
}

#[test]
fn taskdata_selects_the_replica() {
    let p = Proj::with_home_config("[todo]\nbackend = \"taskchampion\"\n");
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("tasks");
    let out = p.devkit(&["todo", "add", "in the shared replica"], &[(
        "TASKDATA",
        data.to_str().unwrap(),
    )]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        data.join("taskchampion.sqlite3").exists(),
        "{}",
        data.display()
    );
    let todos = TaskchampionStore::at(data).list(&Filter::all()).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].description, "in the shared replica");
}

#[test]
fn the_removed_backend_names_taskchampion() {
    let p = Proj::with_home_config("[todo]\nbackend = \"taskwarrior\"\n");
    let out = p.devkit(&["todo", "list"], &[]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("taskchampion"), "{}", stderr(&out));

    let p = Proj::new();
    let out = p.devkit(&["todo", "list"], &[("DEVKIT_TODO_BACKEND", "taskwarrior")]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("DEVKIT_TODO_BACKEND"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("taskchampion"), "{}", stderr(&out));
}
