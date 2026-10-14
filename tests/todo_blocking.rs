//! Blocking edges driven through the `devkit` binary.

#[path = "common/todoenv.rs"]
mod todoenv;

use todoenv::{Proj, stderr, stdout};

const S1: [(&str, &str); 1] = [("CLAUDE_CODE_SESSION_ID", "s1")];

fn add(p: &Proj, text: &str) -> String {
    let out = p.devkit(&["todo", "add", text], &S1);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out).trim().to_string()
}

#[test]
fn block_records_edges_and_list_reads_them_back() {
    let p = Proj::new();
    let (a, b, c) = (add(&p, "a"), add(&p, "b"), add(&p, "c"));
    let out = p.devkit(&["todo", "block", &c, "--by", &format!("{a},{b}")], &S1);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(p.todo(&c).blocked_by, [a, b]);
}

#[test]
fn add_with_blocked_by_records_the_edge() {
    let p = Proj::new();
    let blocker = add(&p, "blocker");
    let out = p.devkit(&["todo", "add", "blocked", "--blocked-by", &blocker], &S1);
    assert!(out.status.success(), "{}", stderr(&out));
    let id = stdout(&out).trim().to_string();
    assert_eq!(p.todo(&id).blocked_by, [blocker]);
}

#[test]
fn an_unknown_blocker_is_refused_and_nothing_is_written() {
    let p = Proj::new();
    let out = p.devkit(&["todo", "add", "x", "--blocked-by", "9"], &S1);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("no todo 9"), "{}", stderr(&out));
    assert!(p.todos().iter().all(|todo| todo.description != "x"));
}

#[test]
fn a_cycle_is_refused_naming_it() {
    let p = Proj::new();
    let (a, b) = (add(&p, "a"), add(&p, "b"));
    let self_edge = p.devkit(&["todo", "block", &a, "--by", &a], &S1);
    assert!(!self_edge.status.success());
    assert!(
        stderr(&self_edge).contains("1 -> 1"),
        "{}",
        stderr(&self_edge)
    );
    let first = p.devkit(&["todo", "block", &b, "--by", &a], &S1);
    assert!(first.status.success(), "{}", stderr(&first));
    let back = p.devkit(&["todo", "block", &a, "--by", &b], &S1);
    assert!(!back.status.success());
    assert!(stderr(&back).contains("1 -> 2 -> 1"), "{}", stderr(&back));
    assert!(p.todo(&a).blocked_by.is_empty());
}

#[test]
fn start_refuses_a_blocked_todo_unless_forced() {
    let p = Proj::new();
    let (blocker, blocked) = (add(&p, "blocker"), add(&p, "blocked"));
    p.devkit(&["todo", "block", &blocked, "--by", &blocker], &S1);
    let refused = p.devkit(&["todo", "start", &blocked], &S1);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("blocked by 1"),
        "{}",
        stderr(&refused)
    );
    assert!(p.todo(&blocked).status == devkit_todo::Status::Pending);
    let forced = p.devkit(&["todo", "start", &blocked, "--force"], &S1);
    assert!(forced.status.success(), "{}", stderr(&forced));
    assert!(matches!(
        p.todo(&blocked).status,
        devkit_todo::Status::InProgress { .. }
    ));
}

#[test]
fn done_prints_what_it_unblocked_once() {
    let p = Proj::new();
    let (a, b, c) = (add(&p, "a"), add(&p, "b"), add(&p, "c"));
    p.devkit(&["todo", "block", &c, "--by", &format!("{a},{b}")], &S1);
    let one = p.devkit(&["todo", "done", &a], &S1);
    assert!(one.status.success(), "{}", stderr(&one));
    assert!(!stdout(&one).contains("unblocked"), "{}", stdout(&one));
    let two = p.devkit(&["todo", "done", &b], &S1);
    assert_eq!(stdout(&two).trim(), "unblocked: 3");

    let p = Proj::new();
    let (a, b, c) = (add(&p, "a"), add(&p, "b"), add(&p, "c"));
    p.devkit(&["todo", "block", &c, "--by", &format!("{a},{b}")], &S1);
    let both = p.devkit(&["todo", "done", &a, &b], &S1);
    assert!(both.status.success(), "{}", stderr(&both));
    assert_eq!(
        stdout(&both).matches("unblocked: 3").count(),
        1,
        "{}",
        stdout(&both)
    );
}

#[test]
fn undone_blocks_the_dependent_again() {
    let p = Proj::new();
    let (a, b) = (add(&p, "a"), add(&p, "b"));
    p.devkit(&["todo", "block", &b, "--by", &a], &S1);
    p.devkit(&["todo", "done", &a], &S1);
    let undone = p.devkit(&["todo", "undone", &a], &S1);
    assert!(undone.status.success(), "{}", stderr(&undone));
    let start = p.devkit(&["todo", "start", &b], &S1);
    assert!(!start.status.success());
    assert!(
        stderr(&start).contains("blocked by 1"),
        "{}",
        stderr(&start)
    );
}

#[test]
fn ready_lists_the_frontier_critical_first() {
    let p = Proj::new();
    let (a, b, c) = (add(&p, "a"), add(&p, "b"), add(&p, "c"));
    p.devkit(&["todo", "block", &b, "--by", &a], &S1);
    p.devkit(&["todo", "block", &c, "--by", &b], &S1);
    let ready = p.devkit(&["todo", "ready"], &S1);
    assert!(ready.status.success(), "{}", stderr(&ready));
    assert_eq!(stdout(&ready), "1  a  *\n");
}

#[test]
fn plan_with_agents_caps_batches() {
    let p = Proj::new();
    let (a, b) = (add(&p, "a"), add(&p, "b"));
    let plan = p.devkit(&["todo", "plan", "--agents", "1"], &S1);
    assert!(plan.status.success(), "{}", stderr(&plan));
    let text = stdout(&plan);
    assert!(text.contains("batch 1  ready now"), "{text}");
    assert!(text.contains(&format!("o {a}  a")), "{text}");
    assert!(text.contains("batch 2"), "{text}");
    assert!(text.contains(&format!("o {b}  b")), "{text}");
}

#[test]
fn plan_mermaid_prints_flowchart_source() {
    let p = Proj::new();
    add(&p, "a");
    let out = p.devkit(&["todo", "plan", "--mermaid"], &S1);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.starts_with("flowchart LR\n"), "{text}");
    assert!(text.contains("subgraph b1[\"batch 1\"]"), "{text}");
}

#[test]
fn another_sessions_steps_are_not_offered() {
    let p = Proj::with_home_config(
        "[todo.roles.manager]\nscope = \"workspace\"\n[todo.roles.implementer]\nscope = \"agent\"\nparent = \"manager\"\n",
    );
    add(&p, "mine");
    let other = [("CLAUDE_CODE_SESSION_ID", "s2")];
    let added = p.devkit(&["todo", "add", "theirs"], &other);
    assert!(added.status.success(), "{}", stderr(&added));
    let selected = p.devkit(&["todo", "role", "manager"], &S1);
    assert!(selected.status.success(), "{}", stderr(&selected));
    let manager = S1;
    for verb in ["ready", "plan"] {
        let out = p.devkit(&["todo", verb], &manager);
        assert!(out.status.success(), "{verb}: {}", stderr(&out));
        assert!(!stdout(&out).contains("theirs"), "{verb}: {}", stdout(&out));
    }
}

#[test]
fn ready_outside_a_repository_lists_global_todos() {
    let p = Proj::new();
    let added = p.devkit_in(p.outside(), &["todo", "add", "far"], &S1, "");
    assert!(added.status.success(), "{}", stderr(&added));
    let ready = p.devkit_in(p.outside(), &["todo", "ready"], &S1, "");
    assert!(ready.status.success(), "{}", stderr(&ready));
    assert!(stdout(&ready).contains("far"), "{}", stdout(&ready));
}

#[test]
fn list_json_omits_a_blocker_outside_the_filter() {
    let p = Proj::new();
    let here = add(&p, "here");
    let there = p.devkit(&["todo", "add", "there", "--node", "proj.side"], &S1);
    assert!(there.status.success(), "{}", stderr(&there));
    let side = stdout(&there).trim().to_string();
    p.devkit(&["todo", "block", &here, "--by", &side], &S1);
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&p.devkit(&["todo", "list", "--json"], &S1))).unwrap();
    let rows = json.as_array().unwrap();
    assert_eq!(rows.len(), 1, "{json}");
    assert_eq!(rows[0]["description"], "here");
}

#[test]
fn list_shows_a_subagent_ready_on_its_own_node() {
    let p = Proj::with_home_config(
        "[todo.roles.manager]\nscope = \"workspace\"\n[todo.roles.implementer]\nscope = \"agent\"\nparent = \"manager\"\n",
    );
    let selected = p.devkit(&["todo", "role", "implementer"], &S1);
    assert!(selected.status.success(), "{}", stderr(&selected));
    let blocker = add(&p, "blocker");
    p.devkit(&["todo", "done", &blocker], &S1);
    let waiting = p.devkit(&["todo", "add", "waiting", "--blocked-by", &blocker], &S1);
    assert!(waiting.status.success(), "{}", stderr(&waiting));
    let listed = p.devkit(&["todo", "list"], &S1);
    assert!(listed.status.success(), "{}", stderr(&listed));
    assert!(stdout(&listed).contains("ready"), "{}", stdout(&listed));
}

#[test]
fn an_outside_wait_is_not_printed_as_a_cycle() {
    let p = Proj::new();
    let side = p.devkit(&["todo", "add", "side", "--node", "proj.side"], &S1);
    assert!(side.status.success(), "{}", stderr(&side));
    let blocker = stdout(&side).trim().to_string();
    let mine = add(&p, "mine");
    p.devkit(&["todo", "block", &mine, "--by", &blocker], &S1);
    let plan = p.devkit(&["todo", "plan"], &S1);
    assert!(plan.status.success(), "{}", stderr(&plan));
    let text = stdout(&plan);
    assert!(!text.contains("cycle:"), "{text}");
    assert!(!text.contains("mine"), "{text}");
    let ready = p.devkit(&["todo", "ready"], &S1);
    assert!(ready.status.success(), "{}", stderr(&ready));
    assert!(!stdout(&ready).contains("mine"), "{}", stdout(&ready));
}

#[test]
fn a_sessionless_scope_is_offered_in_neither_mode() {
    let p = Proj::with_home_config(
        "[todo.scopes.personal]\nnode = \"{repo}.{branch}.personal\"\nparent = \"workspace\"\n[todo.roles.manager]\nscope = \"workspace\"\n",
    );
    let selected = p.devkit(&["todo", "role", "manager"], &S1);
    assert!(selected.status.success(), "{}", stderr(&selected));
    let added = p.devkit(
        &["todo", "add", "private", "--node", "proj.main.personal"],
        &S1,
    );
    assert!(added.status.success(), "{}", stderr(&added));
    let ready = p.devkit(&["todo", "ready"], &S1);
    assert!(ready.status.success(), "{}", stderr(&ready));
    assert!(!stdout(&ready).contains("private"), "{}", stdout(&ready));

    let config = p.home_config();
    let existing = std::fs::read_to_string(&config).unwrap();
    std::fs::write(
        config,
        format!("{existing}\n[todo]\nready_outside_ticket = true\n"),
    )
    .unwrap();
    let elsewhere = p.devkit(&["todo", "add", "elsewhere", "--node", "proj.side"], &S1);
    assert!(elsewhere.status.success(), "{}", stderr(&elsewhere));
    let wide = p.devkit(&["todo", "ready"], &S1);
    assert!(wide.status.success(), "{}", stderr(&wide));
    assert!(stdout(&wide).contains("elsewhere"), "{}", stdout(&wide));
    assert!(!stdout(&wide).contains("private"), "{}", stdout(&wide));
}

#[test]
fn another_tickets_ready_todo_is_not_offered_by_default() {
    let p = Proj::new();
    add(&p, "mine");
    let added = p.devkit(&["todo", "add", "elsewhere", "--node", "proj.side"], &S1);
    assert!(added.status.success(), "{}", stderr(&added));
    let ready = p.devkit(&["todo", "ready"], &S1);
    assert!(!stdout(&ready).contains("elsewhere"), "{}", stdout(&ready));

    let config = p.home_config();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(config, "[todo]\nready_outside_ticket = true\n").unwrap();
    let wide = p.devkit(&["todo", "ready"], &S1);
    assert!(wide.status.success(), "{}", stderr(&wide));
    assert!(stdout(&wide).contains("elsewhere"), "{}", stdout(&wide));
}

#[test]
fn print_unblocked_false_silences_the_line() {
    let p = Proj::new();
    std::fs::write(
        p.path.join("devkit.toml"),
        "[todo]\nprint_unblocked = false\n",
    )
    .unwrap();
    let (a, b) = (add(&p, "a"), add(&p, "b"));
    p.devkit(&["todo", "block", &b, "--by", &a], &S1);
    let done = p.devkit(&["todo", "done", &a], &S1);
    assert!(done.status.success(), "{}", stderr(&done));
    assert!(!stdout(&done).contains("unblocked"), "{}", stdout(&done));
    assert!(p.todo(&b).blocked_by == [a]);
}

#[test]
fn list_outside_a_repository_marks_ready_only_what_ready_offers() {
    let p = Proj::new();
    let blocker = add(&p, "blocker");
    let waiting = p.devkit(&["todo", "add", "waiting", "--blocked-by", &blocker], &S1);
    assert!(waiting.status.success(), "{}", stderr(&waiting));
    p.devkit(&["todo", "done", &blocker], &S1);

    let ready = p.devkit_in(p.outside(), &["todo", "ready"], &S1, "");
    assert!(ready.status.success(), "{}", stderr(&ready));
    assert!(!stdout(&ready).contains("waiting"), "{}", stdout(&ready));
    let listed = p.devkit_in(p.outside(), &["todo", "list", "--all"], &S1, "");
    assert!(listed.status.success(), "{}", stderr(&listed));
    let text = stdout(&listed);
    let line = text
        .lines()
        .find(|line| line.contains("waiting"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(!line.contains("ready"), "{text}");
}
