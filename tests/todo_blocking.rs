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
