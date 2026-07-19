//! Env-isolated end-to-end checks of `task::resolve_step` against a real
//! flock-backed registry: the require_live gate, the user-override waiver,
//! holder-scoped allocation, and fresh per-call resolution.

use std::collections::{BTreeMap, HashMap};

use devkit_ports::apps::App;
use devkit_ports::config::{Config, TaskConfig};
use devkit_ports::registry::{self, Role};
use devkit_ports::task;

const BASE: u16 = 47360;

fn catalog() -> HashMap<String, App> {
    let mut m = HashMap::new();
    m.insert(
        "api".to_string(),
        App {
            name: "api".into(),
            base_port: BASE,
            path: "apps/api".into(),
            launch: vec![],
            url_env: None,
            provides_url: false,
            static_env: HashMap::new(),
            prep_files: vec![],
            setup: vec![],
        },
    );
    m
}

fn build_task() -> TaskConfig {
    TaskConfig {
        run: vec!["git".into(), "version".into()],
        env: [(
            "BASE".to_string(),
            "http://localhost:{{ ports['api'] }}".to_string(),
        )]
        .into(),
        require_live: vec!["api".into()],
        ..TaskConfig::default()
    }
}

#[test]
fn gate_waiver_scoping_and_lazy_resolution() {
    // The registry path comes from process-global env, so every scenario runs
    // sequentially inside this single test.
    let tmp = std::env::temp_dir().join(format!("devkit-task-gate-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    unsafe {
        std::env::set_var("HOME", &tmp);
        std::env::set_var("XDG_STATE_HOME", &tmp);
    }
    let mine = tmp.join("wt-mine");
    let foreign = tmp.join("wt-foreign");
    std::fs::create_dir_all(&mine).unwrap();
    std::fs::create_dir_all(&foreign).unwrap();
    let mine_s = mine.to_str().unwrap();
    let foreign_s = foreign.to_str().unwrap();

    let mut cfg = Config::default();
    cfg.tasks.insert("build".into(), build_task());
    let cat = catalog();
    let none = BTreeMap::new();

    // Gate: no reservation at all → loud error, and no reservation minted.
    let err = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &none).unwrap_err();
    assert!(format!("{err:#}").contains("no live server"), "{err:#}");
    assert!(registry::snapshot().unwrap().entries.is_empty());

    // Waiver: overriding BASE removes the only reference to `api`, so the
    // gate is skipped and nothing is allocated.
    let user: BTreeMap<String, String> =
        [("BASE".to_string(), "https://preview".to_string())].into();
    let plan = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &user).unwrap();
    assert_eq!(plan.env["BASE"], "https://preview");
    assert!(registry::snapshot().unwrap().entries.is_empty());

    // Holder scoping: a foreign holder on BASE never satisfies my gate and
    // never leaks into my allocation.
    registry::with_lock(|d| Ok(d.alloc_one(foreign_s, "api", BASE, Role::Issue))).unwrap();
    let err = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &none).unwrap_err();
    assert!(format!("{err:#}").contains("no live server"), "{err:#}");

    let my_port =
        registry::with_lock(|d| Ok(d.alloc_one(mine_s, "api", BASE, Role::Issue))).unwrap();
    assert_eq!(my_port, BASE + 1);
    registry::record_pid(
        my_port,
        "api",
        mine_s,
        Role::Issue,
        std::process::id(),
        tmp.join("api.log"),
    )
    .unwrap();
    let plan = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &none).unwrap();
    assert_eq!(plan.env["BASE"], format!("http://localhost:{}", BASE + 1));

    // Laziness: after the server moves, a fresh resolve_step renders the new
    // port — nothing is cached from the earlier call.
    registry::release_ports(&[my_port]).unwrap();
    registry::release_ports(&[BASE]).unwrap(); // free the foreign row too
    let moved = registry::with_lock(|d| Ok(d.alloc_one(mine_s, "api", BASE, Role::Issue))).unwrap();
    assert_eq!(moved, BASE);
    registry::record_pid(
        moved,
        "api",
        mine_s,
        Role::Issue,
        std::process::id(),
        tmp.join("api.log"),
    )
    .unwrap();
    let plan = task::resolve_step(&cfg, &cat, &mine, mine_s, "build", &none).unwrap();
    assert_eq!(plan.env["BASE"], format!("http://localhost:{BASE}"));
}
