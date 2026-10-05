use devkit_config::Config;

#[test]
fn defaults_and_custom_entries_share_one_tree() {
    let config = Config::parse(
        r#"
[todo.scopes.finding]
node = "{repo}.{branch}.findings"
parent = "workspace"
[todo.roles.manager]
scope = "workspace"
[todo.roles.implementer]
scope = "agent"
parent = "manager"
agent_types = ["implementer"]
hold_pending = true
"#,
    )
    .unwrap();
    let todo = serde_json::to_value(config.todo).unwrap();
    assert_eq!(todo["scopes"]["global"]["node"], "global");
    assert_eq!(todo["scopes"]["repo"]["node"], "{repo}");
    assert_eq!(todo["scopes"]["workspace"]["parent"], "repo");
    assert_eq!(
        todo["scopes"]["session"]["node"],
        "{repo}.{branch}.{harness}-{session}"
    );
    assert_eq!(
        todo["scopes"]["agent"]["node"],
        "{repo}.{branch}.{harness}-{session}.{agent}"
    );
    assert_eq!(todo["scopes"]["finding"]["parent"], "workspace");
    assert_eq!(todo["roles"]["main"]["scope"], "session");
    assert_eq!(todo["roles"]["subagent"]["scope"], "session");
    assert_eq!(todo["roles"]["implementer"]["scope"], "agent");
}

macro_rules! invalid_template {
    ($name:ident, $template:expr, $message:expr) => {
        #[test]
        fn $name() {
            let toml = format!(
                "[todo.scopes.custom]\nnode = {:?}\nparent = \"workspace\"\n",
                $template
            );
            let error = Config::parse(&toml).unwrap_err();
            assert!(format!("{error:#}").contains($message), "{error:#}");
        }
    };
}

invalid_template!(
    unknown_placeholder,
    "{repo}.{unknown}",
    "unknown placeholder"
);
invalid_template!(repeated_placeholder, "{repo}.{repo}", "used twice");
invalid_template!(
    multiple_open_values_in_a_segment,
    "{repo}.{branch}-{session}",
    "open placeholder"
);
invalid_template!(unanchorable_template, "{branch}.{session}", "{repo}");
invalid_template!(empty_segment, "{repo}..jobs", "empty segment");
invalid_template!(unclosed_placeholder, "{repo}.{branch", "brace");
invalid_template!(unopened_placeholder, "{repo}.branch}", "brace");

macro_rules! invalid_layout {
    ($name:ident, $toml:expr, $message:expr) => {
        #[test]
        fn $name() {
            let error = Config::parse($toml).unwrap_err();
            assert!(format!("{error:#}").contains($message), "{error:#}");
        }
    };
}

invalid_layout!(
    missing_scope_parent,
    "[todo.scopes.workspace]\nnode = '{repo}.{branch}'\nparent = 'missing'",
    "missing parent"
);
invalid_layout!(
    scope_cycle,
    "[todo.scopes.repo]\nnode = '{repo}'\nparent = 'workspace'",
    "cycle"
);
invalid_layout!(
    second_scope_root,
    "[todo.scopes.custom]\nnode = 'extra'",
    "one root"
);
invalid_layout!(
    root_with_placeholder,
    "[todo.scopes.global]\nnode = '{repo}'",
    "root"
);
invalid_layout!(
    missing_role_scope,
    "[todo.roles.manager]\nscope = 'missing'",
    "missing scope"
);
invalid_layout!(
    missing_role_parent,
    "[todo.roles.worker]\nscope = 'agent'\nparent = 'missing'",
    "missing parent"
);
invalid_layout!(
    role_cycle,
    "[todo.roles.main]\nscope = 'session'\nparent = 'subagent'\n[todo.roles.subagent]\nscope = 'session'\nparent = 'main'",
    "cycle"
);
invalid_layout!(
    duplicate_agent_types,
    "[todo.roles.main]\nscope = 'session'\nagent_types = ['worker']\n[todo.roles.subagent]\nscope = 'session'\nagent_types = ['worker']",
    "agent_type"
);

#[test]
fn removed_project_names_scopes_replacement() {
    let error = Config::parse("[todo]\nproject = 'agents'").unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("[todo] project"), "{message}");
    assert!(message.contains("[todo.scopes]"), "{message}");
}

#[test]
fn postgres_root_defaults_to_devkit() {
    let config = Config::parse("").unwrap();
    assert_eq!(
        serde_json::to_value(config.todo.postgres).unwrap()["root"],
        "devkit"
    );
    let config = Config::parse("[todo.postgres]\nroot = 'factory'").unwrap();
    assert_eq!(
        serde_json::to_value(config.todo.postgres).unwrap()["root"],
        "factory"
    );
}

#[test]
fn workflow_maps_merge_by_key_across_layers() {
    let layers = [
        ("home.toml", "[todo.roles.manager]\nscope = 'workspace'\n[todo.roles.worker]\nscope = 'agent'\nparent = 'manager'\nagent_types = ['worker']\n[todo.scopes.finding]\nnode = '{repo}.{branch}.findings'\nparent = 'workspace'"),
        ("devkit.toml", "[todo.roles.worker]\nhold_pending = true\n[todo.roles.main]\nscope = 'workspace'\n[todo.scopes.finding]\nnode = '{repo}.{branch}.notes'"),
    ].map(|(path, text)| (std::path::PathBuf::from(path), toml::from_str(text).unwrap()));
    let (merged, ..) = devkit_config::merge_layers(&layers);
    let config = Config::parse(&toml::to_string(&merged).unwrap()).unwrap();
    assert_eq!(config.todo.roles["manager"].scope, "workspace");
    assert_eq!(config.todo.roles["worker"].agent_types, ["worker"]);
    assert_eq!(config.todo.roles["worker"].hold_pending, Some(true));
    assert_eq!(config.todo.roles["main"].scope, "workspace");
    assert_eq!(config.todo.roles["subagent"].scope, "session");
    assert_eq!(config.todo.scopes["finding"].node, "{repo}.{branch}.notes");
    assert_eq!(
        config.todo.scopes["finding"].parent.as_deref(),
        Some("workspace")
    );
}
