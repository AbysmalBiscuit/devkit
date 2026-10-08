use devkit_config::Config;
use devkit_todo::{
    Holder,
    layout::{Facts, Layout},
    node::{Harness, Place, SessionRef},
    roles::Roles,
};

fn layout() -> Layout {
    Layout::new(&Config::parse("[todo.roles.manager]\nscope = 'workspace'\n[todo.roles.worker]\nscope = 'agent'\nparent = 'manager'\nagent_types = ['implementer']\n[todo.roles.reviewer]\nscope = 'agent'\nparent = 'manager'").unwrap().todo).unwrap()
}

fn facts(holder: &str) -> Facts {
    Facts::new(
        &Place::Workspace {
            repo: "r".into(),
            branch: "main".into(),
        },
        Some(&SessionRef {
            harness: Harness::Codex,
            id: Holder::new(holder).session().to_string(),
        }),
        &Holder::new(holder),
    )
}

#[test]
fn exact_roles_survive_resume_independently_for_session_and_workers() {
    let dir = tempfile::tempdir().unwrap();
    let roles = Roles::at(dir.path().to_path_buf());
    let layout = layout();
    roles
        .record(&layout, &Holder::new("S"), "manager", None)
        .unwrap();
    roles
        .spawn(&layout, &Holder::new("S/a"), Some("implementer"), None)
        .unwrap();
    let resumed = Roles::at(dir.path().to_path_buf());
    let main = resumed
        .resolve(&layout, &facts("S"), &Holder::new("S"), None)
        .unwrap();
    assert_eq!(main.name, "manager");
    assert_eq!(main.node, "r.main");
    assert!(!main.hold_pending);
    let worker = resumed
        .resolve(&layout, &facts("S/a"), &Holder::new("S/a"), None)
        .unwrap();
    assert_eq!(worker.name, "worker");
    assert_eq!(worker.node, "r.main.codex-S.a");
    assert!(worker.hold_pending);
    assert_eq!(
        resumed
            .resolve(&layout, &facts("S/b"), &Holder::new("S/b"), None)
            .unwrap()
            .name,
        "subagent"
    );
    roles
        .record(&layout, &Holder::new("S/a"), "reviewer", None)
        .unwrap();
    assert_eq!(
        roles
            .resolve(
                &layout,
                &facts("S/a"),
                &Holder::new("S/a"),
                Some("implementer")
            )
            .unwrap()
            .name,
        "reviewer"
    );
    let default = roles
        .resolve(&layout, &facts("T"), &Holder::new("T"), None)
        .unwrap();
    assert_eq!(default.name, "main");
    assert!(default.hold_pending);
    assert!(
        !roles
            .resolve(&layout, &facts("T/a"), &Holder::new("T/a"), None)
            .unwrap()
            .hold_pending
    );
}

#[test]
fn first_write_suggests_child_roles_once_per_exact_holder() {
    let dir = tempfile::tempdir().unwrap();
    let roles = Roles::at(dir.path().to_path_buf());
    let layout = layout();
    roles
        .record(&layout, &Holder::new("S"), "manager", None)
        .unwrap();
    let text = roles
        .nudge(&layout, &facts("S/a"), &Holder::new("S/a"))
        .unwrap()
        .unwrap();
    assert!(text.contains("worker"));
    assert!(text.contains("reviewer"));
    assert!(text.contains("devkit todo role"));
    assert!(
        Roles::at(dir.path().to_path_buf())
            .nudge(&layout, &facts("S/a"), &Holder::new("S/a"))
            .unwrap()
            .is_none()
    );
    assert!(
        roles
            .nudge(&layout, &facts("S/b"), &Holder::new("S/b"))
            .unwrap()
            .is_some()
    );
    roles
        .spawn(&layout, &Holder::new("S/c"), Some("implementer"), None)
        .unwrap();
    assert!(
        roles
            .nudge(&layout, &facts("S/c"), &Holder::new("S/c"))
            .unwrap()
            .is_none()
    );
    let empty = Layout::new(&Config::parse("").unwrap().todo).unwrap();
    assert!(
        roles
            .nudge(&empty, &facts("T"), &Holder::new("T"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn deleted_role_warns_and_falls_back_to_saved_agent_type() {
    let dir = tempfile::tempdir().unwrap();
    let roles = Roles::at(dir.path().to_path_buf());
    let layout = layout();
    roles
        .spawn(&layout, &Holder::new("S/a"), Some("implementer"), None)
        .unwrap();
    roles
        .record(&layout, &Holder::new("S/a"), "reviewer", None)
        .unwrap();
    let replacement = Layout::new(
        &Config::parse("[todo.roles.worker]\nscope = 'agent'\nagent_types = ['implementer']")
            .unwrap()
            .todo,
    )
    .unwrap();
    let resolved = roles
        .resolve(&replacement, &facts("S/a"), &Holder::new("S/a"), None)
        .unwrap();
    assert_eq!(resolved.name, "worker");
    let warning = resolved.warning.unwrap();
    assert!(warning.contains("reviewer"));
    assert!(warning.contains("worker"));
}

#[test]
fn writes_prune_roles_recorded_more_than_thirty_days_ago() {
    let dir = tempfile::tempdir().unwrap();
    let roles = Roles::at(dir.path().to_path_buf());
    let layout = layout();
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    std::fs::write(
        dir.path().join("roles.json"),
        serde_json::json!({"version":1,"entries":{
            "old":{"role":"manager","at":now - chrono::Duration::days(31)},
            "recent":{"role":"manager","at":now - chrono::Duration::days(29)}
        }})
        .to_string(),
    )
    .unwrap();
    roles
        .record(&layout, &Holder::new("S"), "manager", None)
        .unwrap();
    assert_eq!(
        roles
            .resolve(&layout, &facts("old"), &Holder::new("old"), None)
            .unwrap()
            .name,
        "main"
    );
    assert_eq!(
        roles
            .resolve(&layout, &facts("recent"), &Holder::new("recent"), None)
            .unwrap()
            .name,
        "manager"
    );
}

#[test]
fn pending_default_follows_the_effective_scope_and_explicit_override() {
    let layout = layout();
    let global = Facts::new(
        &Place::Global,
        Some(&SessionRef {
            harness: Harness::Codex,
            id: "S".into(),
        }),
        &Holder::new("S"),
    );
    assert!(!layout.hold_pending("main", &global).unwrap());
    let explicit = Layout::new(
        &Config::parse("[todo.roles.main]\nscope = 'session'\nhold_pending = true")
            .unwrap()
            .todo,
    )
    .unwrap();
    assert!(explicit.hold_pending("main", &global).unwrap());
}

#[test]
fn concurrent_writes_keep_all_holders_and_nudge_only_one_writer() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout();
    Roles::at(dir.path().into())
        .record(&layout, &Holder::new("S"), "manager", None)
        .unwrap();
    std::thread::scope(|threads| {
        let jobs: Vec<_> = (0..8)
            .map(|index| {
                let layout = &layout;
                let path = dir.path();
                threads.spawn(move || {
                    let roles = Roles::at(path.into());
                    roles
                        .record(layout, &Holder::new(format!("S/a{index}")), "worker", None)
                        .unwrap();
                    roles
                        .nudge(layout, &facts("S/roleless"), &Holder::new("S/roleless"))
                        .unwrap()
                        .is_some()
                })
            })
            .collect();
        assert_eq!(
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .filter(|nudged| *nudged)
                .count(),
            1
        );
    });
    let roles = Roles::at(dir.path().into());
    for index in 0..8 {
        let holder = Holder::new(format!("S/a{index}"));
        assert_eq!(
            roles
                .resolve(&layout, &facts(&holder), &holder, None)
                .unwrap()
                .name,
            "worker"
        );
    }
}
