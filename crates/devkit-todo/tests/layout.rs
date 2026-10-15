use devkit_config::{Config, todo::Template};
use devkit_todo::{
    Holder,
    layout::{Facts, Layout},
    node::{Harness, Place, SessionRef},
};

fn facts(holder: &str) -> Facts {
    Facts::new(
        &Place::Workspace {
            repo: "r".into(),
            branch: "main".into(),
        },
        Some(&SessionRef {
            harness: Harness::Codex,
            id: "S".into(),
        }),
        &Holder::new(holder),
    )
}

#[test]
fn default_templates_fill_and_read_caller_facts() {
    let config = Config::parse("").unwrap();
    let layout = Layout::new(&config.todo).unwrap();
    for (scope, node) in [
        ("global", "global"),
        ("repo", "r"),
        ("workspace", "r.main"),
        ("session", "r.main.codex-S"),
        ("agent", "r.main.codex-S.a1"),
    ] {
        assert_eq!(layout.fill(scope, &facts("S/a1")).unwrap(), node);
        let template = Template::parse(&config.todo.scopes[scope].node).unwrap();
        let values = template.read(node).unwrap();
        assert_eq!(template.fill(&values).as_deref(), Some(node));
    }
    let session = Template::parse(&config.todo.scopes["session"].node).unwrap();
    assert!(session.read("r.main.unknown-S").is_none());
    assert!(session.read("r.main.codex-").is_none());
    assert!(session.read("r.main.codex-S.a").is_none());
}

#[test]
fn missing_facts_walk_up_to_a_scope_that_fills() {
    let layout = Layout::new(&Config::parse("").unwrap().todo).unwrap();
    assert_eq!(layout.fill("agent", &facts("S")).unwrap(), "r.main.codex-S");
    let terminal = Facts::new(
        &Place::Workspace {
            repo: "r".into(),
            branch: "main".into(),
        },
        None,
        &Holder::human(),
    );
    assert_eq!(layout.fill("agent", &terminal).unwrap(), "r.main");
    let repo = Facts::new(&Place::Project { repo: "r".into() }, None, &Holder::human());
    assert_eq!(layout.fill("agent", &repo).unwrap(), "r");
    assert_eq!(
        layout
            .fill("agent", &Facts::new(&Place::Global, None, &Holder::human()))
            .unwrap(),
        "global"
    );
}

fn workspace(repo: &str, branch: &str, session: &str, holder: &str) -> Facts {
    Facts::new(
        &Place::Workspace {
            repo: repo.into(),
            branch: branch.into(),
        },
        Some(&SessionRef {
            harness: Harness::Claude,
            id: session.into(),
        }),
        &Holder::new(holder),
    )
}

#[test]
fn the_ticket_is_the_workspace_node() {
    let layout = Layout::new(&Config::parse("").unwrap().todo).unwrap();
    let facts = workspace("proj", "main", "s1", "s1");
    assert_eq!(
        layout.ticket_root(&facts, false).as_deref(),
        Some("proj.main")
    );
    assert_eq!(layout.ticket_root(&facts, true).as_deref(), Some("proj"));
}

#[test]
fn outside_a_repository_there_is_no_ticket() {
    let layout = Layout::new(&Config::parse("").unwrap().todo).unwrap();
    let facts = Facts::new(&Place::Global, None, &Holder::human());
    assert_eq!(layout.ticket_root(&facts, false), None);
    assert_eq!(layout.ticket_root(&facts, true), None);
}

#[test]
fn offered_covers_the_ticket_and_own_session_only() {
    let layout = Layout::new(&Config::parse("").unwrap().todo).unwrap();
    let facts = workspace("proj", "main", "s1", "s1");
    let root = layout.ticket_root(&facts, false).unwrap();
    for node in ["proj.main", "proj.main.claude-s1.a1"] {
        assert!(layout.offered(Some(node), &root, &facts, "main"), "{node}");
    }
    for node in ["proj.main.claude-s2", "proj", "proj.other", "global"] {
        assert!(!layout.offered(Some(node), &root, &facts, "main"), "{node}");
    }
    assert!(!layout.offered(None, &root, &facts, "main"));
}

#[test]
fn a_sessionless_child_scope_is_offered_in_neither_mode() {
    let config = Config::parse(
        "[todo.scopes.personal]\nnode = '{repo}.{branch}.personal'\nparent = 'workspace'\n[todo.roles.manager]\nscope = 'workspace'\n",
    )
    .unwrap();
    let layout = Layout::new(&config.todo).unwrap();
    let facts = workspace("proj", "main", "s1", "s1");
    let narrow = layout.ticket_root(&facts, false).unwrap();
    let wide = layout.ticket_root(&facts, true).unwrap();
    assert!(!layout.offered(Some("proj.main.personal"), &narrow, &facts, "main"));
    assert!(!layout.offered(Some("proj.main.personal"), &wide, &facts, "main"));
    assert!(layout.offered(Some("proj.main"), &narrow, &facts, "main"));
    assert!(layout.offered(Some("proj.main.claude-s1"), &narrow, &facts, "main"));
    assert!(!layout.offered(Some("proj.other"), &narrow, &facts, "main"));
    assert!(layout.offered(Some("proj"), &wide, &facts, "main"));
    assert!(layout.offered(Some("proj.other"), &wide, &facts, "main"));
    assert!(layout.offered(Some("proj.main.claude-s1"), &wide, &facts, "main"));
    assert!(!layout.offered(Some("proj.other.claude-s2"), &wide, &facts, "main"));
}

#[test]
fn offered_uses_the_callers_role_only() {
    let config = Config::parse(
        "[todo.scopes.personal]\nnode = '{repo}.{branch}.personal'\nparent = 'workspace'\n[todo.roles.manager]\nscope = 'workspace'\n[todo.roles.personal]\nscope = 'personal'\n",
    )
    .unwrap();
    let layout = Layout::new(&config.todo).unwrap();
    let facts = workspace("proj", "main", "s1", "s1");
    let root = layout.ticket_root(&facts, false).unwrap();
    assert!(!layout.offered(Some("proj.main.personal"), &root, &facts, "manager"));
    assert!(layout.offered(Some("proj.main.personal"), &root, &facts, "personal"));
    assert!(layout.offered(Some("proj.main.claude-s1"), &root, &facts, "manager"));
}

#[test]
fn sanitized_values_never_add_a_level() {
    let layout = Layout::new(&Config::parse("").unwrap().todo).unwrap();
    let facts = Facts::new(
        &Place::Workspace {
            repo: "r.x".into(),
            branch: "feat/a".into(),
        },
        Some(&SessionRef {
            harness: Harness::Claude,
            id: "S.x".into(),
        }),
        &Holder::new("S.x/a\\1"),
    );
    assert_eq!(
        layout.fill("agent", &facts).unwrap(),
        "r-x.feat-a.claude-S-x.a-1"
    );
}

#[test]
fn manager_visibility_includes_ancestors_and_only_own_session_descendants() {
    let config = Config::parse("[todo.roles.manager]\nscope = 'workspace'\n[todo.scopes.finding]\nnode = '{repo}.{branch}.notes.{harness}-{session}'\nparent = 'workspace'").unwrap();
    let layout = Layout::new(&config.todo).unwrap();
    let filter = layout.visible(&facts("S"), "manager").unwrap();
    for node in [
        "global",
        "r",
        "r.main",
        "r.main.codex-S",
        "r.main.codex-S.a1",
        "r.main.notes.codex-S",
    ] {
        assert!(filter.matches(Some(node)), "{node}");
    }
    for node in [
        "r.other",
        "r.main.codex-T",
        "r.main.codex-T.a1",
        "r.main.notes.codex-T",
        "r.main.personal",
        "r-web.main.codex-S",
    ] {
        assert!(!filter.matches(Some(node)), "{node}");
    }
}

#[test]
fn store_scans_require_anchors_for_open_templates() {
    let layout = Layout::new(&Config::parse("").unwrap().todo).unwrap();
    let todo = |node: &str| devkit_todo::Todo {
        ticket: None,
        id: node.into(),
        description: node.into(),
        status: devkit_todo::Status::Pending,
        parent: None,
        order: None,
        project: Some(node.into()),
        entry: None,
        modified: None,
        blocked_by: Vec::new(),
    };
    let todos = [
        "r",
        "r.main",
        "r.main.codex-S",
        "r.main.codex-S.a1",
        "ideas",
        "followup.main",
        "global",
        "devkit.r.main.codex-S",
        "r.main.personal.extra",
        "unfiled",
    ];
    let kept = layout.fence(todos.into_iter().map(todo).collect());
    assert_eq!(
        kept.iter().map(devkit_todo::Todo::node).collect::<Vec<_>>(),
        [
            "r",
            "r.main",
            "r.main.codex-S",
            "r.main.codex-S.a1",
            "global"
        ]
    );
    let literal = Layout::new(
        &Config::parse("[todo.scopes.fixed]\nnode = 'notes'\nparent = 'global'")
            .unwrap()
            .todo,
    )
    .unwrap();
    assert_eq!(literal.fence(vec![todo("notes")]).len(), 1);
}

#[test]
fn template_read_keeps_literal_prefixes_suffixes_and_closed_harness() {
    let template = Template::parse("repo-{repo}.job-{branch}-{harness}.{session}").unwrap();
    let values = template.read("repo-r.job-feat-codex.S").unwrap();
    assert_eq!(values["repo"], "r");
    assert_eq!(values["branch"], "feat");
    assert_eq!(values["harness"], "codex");
    assert_eq!(
        template.fill(&values).as_deref(),
        Some("repo-r.job-feat-codex.S")
    );
    assert!(template.read("repo-r.job-feat-other.S").is_none());
    assert!(template.read("repo-r.job--codex.S").is_none());
}

#[test]
fn a_ticket_todo_is_inside_the_fence_without_an_anchor() {
    let layout = Layout::new(&Config::parse("").unwrap().todo).unwrap();
    let todo = |id: &str, ticket: Option<&str>| devkit_todo::Todo {
        ticket: ticket.map(|key| devkit_todo::TicketRef {
            tracker: "github".into(),
            key: key.into(),
            synced_status: Some("Todo".into()),
        }),
        id: id.into(),
        description: id.into(),
        status: devkit_todo::Status::Pending,
        parent: None,
        order: None,
        project: Some("r".into()),
        entry: None,
        modified: None,
        blocked_by: Vec::new(),
    };
    let kept = layout.fence(vec![todo("ticket", Some("o/r#65")), todo("plain", None)]);
    assert_eq!(kept.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), [
        "ticket"
    ]);
}
