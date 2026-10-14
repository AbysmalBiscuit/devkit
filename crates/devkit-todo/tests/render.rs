//! Plan and Mermaid text, rendered from todos built directly.

use devkit_todo::{
    Holder, Status, Todo,
    graph::Graph,
    render::{self, Notes},
};

fn todo(id: &str, text: &str, status: Status, blocked_by: &[&str]) -> Todo {
    Todo {
        id: id.into(),
        description: text.into(),
        status,
        parent: None,
        order: Some(id.parse().unwrap_or(0)),
        project: Some("r.main".into()),
        entry: None,
        modified: None,
        blocked_by: blocked_by.iter().map(|id| (*id).to_string()).collect(),
    }
}

fn working() -> Status {
    Status::InProgress {
        by: Holder::new("S"),
    }
}

#[test]
fn plan_text_names_batches_critical_path_and_cycle() {
    let todos = vec![
        todo("1", "first", working(), &[]),
        todo("2", "second", Status::Pending, &["1"]),
        todo("3", "looped", Status::Pending, &["4"]),
        todo("4", "back", Status::Pending, &["3"]),
    ];
    let plan = Graph::new(&todos).plan(|_| true, None);
    let text = render::render_plan("r.main", &plan);
    assert_eq!(
        text,
        "\
r.main: 4 open, 1 in progress
critical path: 1 -> 2 (2 batches minimum)

batch 1  ready now
  > 1  first  *  in progress: S

batch 2
  o 2  second  *  after 1

cycle: 3, 4
"
    );
}

#[test]
fn mermaid_has_one_subgraph_per_batch_and_escapes_quotes() {
    let todos = vec![
        todo("1", "say \"hi\"", Status::Pending, &[]),
        todo("2", "then", Status::Pending, &["1"]),
    ];
    let plan = Graph::new(&todos).plan(|_| true, None);
    let text = render::render_mermaid(&plan);
    assert_eq!(
        text,
        "\
flowchart LR
  subgraph b1[\"batch 1\"]
    t1[\"1: say #quot;hi#quot;\"]
  end
  subgraph b2[\"batch 2\"]
    t2[\"2: then\"]
  end
  t1 --> t2
  classDef critical stroke-width:3px
  class t1,t2 critical
"
    );
}

fn shown(todos: &[Todo], show: &dyn Fn(&Todo) -> bool) -> String {
    let graph = Graph::new(todos);
    let notes = Notes {
        graph: &graph,
        show_ready: show,
    };
    render::render_lists(&["r.main".into()], todos, &Holder::new("S"), &notes)
}

#[test]
fn blocked_and_ready_notes_render_in_the_parentheses() {
    let todos = vec![
        todo("1", "first", Status::Pending, &[]),
        todo("2", "second", Status::Pending, &["1"]),
    ];
    let text = shown(&todos, &|_| true);
    assert!(text.contains("- [ ] first (1)\n"), "{text}");
    assert!(text.contains("- [ ] second (2, blocked by 1)"), "{text}");
}

#[test]
fn a_blocker_off_the_listing_is_named_with_its_node() {
    let mut blocker = todo("9", "elsewhere", Status::Pending, &[]);
    blocker.project = Some("r.other".into());
    let listed = vec![todo("2", "second", Status::Pending, &["9"])];
    let all = vec![listed[0].clone(), blocker];
    let graph = Graph::new(&all);
    let notes = Notes {
        graph: &graph,
        show_ready: &|_| false,
    };
    let text = render::render_lists(&["r.main".into()], &listed, &Holder::new("S"), &notes);
    assert!(
        text.contains("- [ ] second (2, blocked by 9 on r.other)"),
        "{text}"
    );
}

#[test]
fn todos_without_blockers_render_unchanged() {
    let todos = vec![todo("1", "first", Status::Pending, &[])];
    let text = shown(&todos, &|_| false);
    assert_eq!(text, "## r.main\n- [ ] first (1)\n");
}

#[test]
fn plan_escapes_control_characters_in_the_holder() {
    let todos = vec![todo(
        "1",
        "first",
        Status::InProgress {
            by: Holder::new("S\u{1b}]0;owned\u{7}\u{202e}"),
        },
        &[],
    )];
    let plan = Graph::new(&todos).plan(|_| true, None);
    let text = render::render_plan("r.main", &plan);
    assert!(
        text.contains("in progress: S\\u{1b}]0;owned\\u{7}\\u{202e}\n"),
        "{text}"
    );
}
