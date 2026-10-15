//! The todo lists as an agent reads them: open todos in full, finished ones
//! counted, under a heading per node.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
    hash::{DefaultHasher, Hash, Hasher},
};

use devkit_common::ui::printable;

use crate::{
    Holder, Status, StatusKind, Todo,
    graph::{Graph, Plan},
    names::Names,
};

/// What a listing says beside each todo: blocking, from the graph the lines
/// were drawn from, which ready todos earn a `ready` note, the work under
/// each synced ticket, and the name each todo shows under.
pub struct Notes<'a> {
    pub graph: &'a Graph<'a>,
    pub show_ready: &'a dyn Fn(&Todo) -> bool,
    pub work: &'a TicketWork,
    pub names: &'a Names,
}

/// The work a worktree on this machine does for each synced ticket, keyed
/// by the ticket todo's id: the count of open todos in its workspace, and,
/// when a listing expands tickets, those todos.
#[derive(Debug, Default)]
pub struct TicketWork {
    pub open: HashMap<String, usize>,
    pub expanded: HashMap<String, Vec<Todo>>,
}

impl TicketWork {
    /// No ticket work at all, for a listing that shows none.
    pub fn none() -> &'static TicketWork {
        static NONE: std::sync::LazyLock<TicketWork> =
            std::sync::LazyLock::new(TicketWork::default);
        &NONE
    }
}

/// The plan as a person reads it: a header, the critical path, then one block
/// per batch, and a `cycle:` line when some members never placed.
pub fn render_plan(root: &str, plan: &Plan<'_>, work: &TicketWork, names: &Names) -> String {
    let open = plan.batches.iter().flatten().count() + plan.cycle.len();
    let started = plan
        .batches
        .iter()
        .flatten()
        .filter(|todo| todo.status.kind() == StatusKind::InProgress)
        .count();
    let mut out = format!("{root}: {open} open, {started} in progress\n");
    if !plan.critical.is_empty() {
        let path = plan
            .critical
            .iter()
            .map(|todo| names.id(&todo.id))
            .collect::<Vec<_>>()
            .join(" -> ");
        let _ = writeln!(
            out,
            "critical path: {path} ({} batches minimum)",
            plan.critical.len()
        );
    }
    for (index, batch) in plan.batches.iter().enumerate() {
        let label = match index {
            0 => "  ready now",
            _ => "",
        };
        let _ = writeln!(out, "\nbatch {}{label}", index + 1);
        for todo in batch {
            let _ = writeln!(out, "  {}", plan_line(todo, plan, index, work, names));
            for under in work.expanded.get(&todo.id).into_iter().flatten() {
                let _ = writeln!(out, "    {}", work_line(under, None, names));
            }
        }
    }
    if !plan.cycle.is_empty() {
        let ids = plan
            .cycle
            .iter()
            .map(|todo| names.id(&todo.id))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "\ncycle: {ids}");
    }
    out
}

/// One plan row: the mark, the name, the text, the critical mark, and why
/// the todo sits in this batch.
fn plan_line(
    todo: &Todo,
    plan: &Plan<'_>,
    batch: usize,
    work: &TicketWork,
    names: &Names,
) -> String {
    let mark = match todo.status.kind() {
        StatusKind::InProgress => ">",
        _ => "o",
    };
    let critical = if plan.critical.iter().any(|on| on.id == todo.id) {
        "*"
    } else {
        ""
    };
    let note = match &todo.status {
        Status::InProgress { by } => format!("in progress: {}", printable(by.name())),
        Status::Pending if batch == 0 => "ready".to_string(),
        _ => {
            let members: std::collections::BTreeSet<&str> = plan
                .batches
                .iter()
                .flatten()
                .map(|member| member.id.as_str())
                .collect();
            let after = todo
                .blocked_by
                .iter()
                .filter(|id| members.contains(id.as_str()))
                .map(|id| names.id(id))
                .collect::<Vec<_>>()
                .join(", ");
            format!("after {after}")
        }
    };
    let note = match work.open.get(&todo.id) {
        Some(open) => format!("{note}, {open} open"),
        None => note,
    };
    format!(
        "{mark} {}  {}  {critical}  {note}",
        printable(&names.id(&todo.id)),
        printable(names.text(todo))
    )
}

/// The plan as Mermaid `flowchart LR` source, one subgraph per batch and
/// one for the cycle.
pub fn render_mermaid(plan: &Plan<'_>, names: &Names) -> String {
    let mut out = String::from("flowchart LR\n");
    for (index, batch) in plan.batches.iter().enumerate() {
        let number = index + 1;
        mermaid_subgraph(
            &mut out,
            &format!("b{number}[\"batch {number}\"]"),
            batch,
            names,
        );
    }
    if !plan.cycle.is_empty() {
        mermaid_subgraph(&mut out, "cycle[\"cycle\"]", &plan.cycle, names);
    }
    let members: Vec<&Todo> = plan
        .batches
        .iter()
        .flatten()
        .chain(&plan.cycle)
        .copied()
        .collect();
    for todo in &members {
        for blocker in &todo.blocked_by {
            if members.iter().any(|member| &member.id == blocker) {
                let _ = writeln!(
                    out,
                    "  t{} --> t{}",
                    crate::short_id(blocker),
                    crate::short_id(&todo.id)
                );
            }
        }
    }
    let critical: Vec<&str> = plan
        .critical
        .iter()
        .map(|todo| crate::short_id(&todo.id))
        .collect();
    if !critical.is_empty() {
        let _ = writeln!(out, "  classDef critical stroke-width:3px");
        let _ = writeln!(
            out,
            "  class {} critical",
            critical
                .iter()
                .map(|id| format!("t{id}"))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    out
}

fn mermaid_subgraph(out: &mut String, head: &str, todos: &[&Todo], names: &Names) {
    let _ = writeln!(out, "  subgraph {head}");
    for todo in todos {
        let text = printable(names.text(todo)).replace('"', "#quot;");
        let name = printable(&names.id(&todo.id)).replace('"', "#quot;");
        let id = crate::short_id(&todo.id);
        let _ = writeln!(out, "    t{id}[\"{name}: {text}\"]");
    }
    let _ = writeln!(out, "  end");
}

/// The lists for the injected context: each node in `nodes` order, open todos
/// in tree order, finished todos as one count line. A node with nothing open
/// is left out.
pub fn render_lists(
    nodes: &[String],
    todos: &[Todo],
    viewer: &Holder,
    notes: &Notes<'_>,
) -> String {
    render(nodes, todos, viewer, false, notes)
}

/// As [`render_lists`], with completed todos shown in full as `- [x]`, for a
/// person or agent asking for the listing.
pub fn render_full(nodes: &[String], todos: &[Todo], viewer: &Holder, notes: &Notes<'_>) -> String {
    render(nodes, todos, viewer, true, notes)
}

fn render(
    nodes: &[String],
    todos: &[Todo],
    viewer: &Holder,
    full: bool,
    notes: &Notes<'_>,
) -> String {
    let sections: Vec<String> = nodes
        .iter()
        .filter_map(|node| {
            let here: Vec<&Todo> = todos.iter().filter(|t| t.node() == node).collect();
            section(node, &here, viewer, full, notes)
        })
        .collect();
    sections.join("\n")
}

fn section(
    node: &str,
    todos: &[&Todo],
    viewer: &Holder,
    full: bool,
    notes: &Notes<'_>,
) -> Option<String> {
    let shown: Vec<&Todo> = todos
        .iter()
        .copied()
        .filter(|t| match t.status.kind() {
            StatusKind::Pending | StatusKind::InProgress => true,
            StatusKind::Completed => full,
            StatusKind::Cancelled => false,
        })
        .collect();
    let count = |kind| todos.iter().filter(|t| t.status.kind() == kind).count();
    // A full listing keeps a node whose todos were all cancelled, so a person
    // can still see what was abandoned there.
    if shown.is_empty() && !(full && count(StatusKind::Cancelled) > 0) {
        return None;
    }
    let mut out = format!("## {}\n", printable(node));
    for (todo, depth) in tree(&shown) {
        let _ = writeln!(out, "{}{}", "  ".repeat(depth), line(todo, viewer, notes));
        for under in notes.work.expanded.get(&todo.id).into_iter().flatten() {
            let _ = writeln!(
                out,
                "{}{}",
                "  ".repeat(depth + 1),
                work_line(under, Some(viewer), notes.names)
            );
        }
    }
    let mut finished = Vec::new();
    if !full && count(StatusKind::Completed) > 0 {
        finished.push(format!("{} done", count(StatusKind::Completed)));
    }
    if count(StatusKind::Cancelled) > 0 {
        finished.push(format!("{} cancelled", count(StatusKind::Cancelled)));
    }
    if !finished.is_empty() {
        let _ = writeln!(out, "{}", finished.join(", "));
    }
    Some(out)
}

/// A todo's checkbox mark and its holder note, `viewer` naming whose claim
/// reads as the reader's own.
fn mark_and_note(todo: &Todo, viewer: Option<&Holder>) -> (char, String) {
    match &todo.status {
        Status::InProgress { by } if Some(by) == viewer => (' ', ", in progress".to_string()),
        Status::InProgress { by } => (' ', format!(", in progress: {}", printable(by.name()))),
        Status::Completed { .. } => ('x', String::new()),
        Status::Pending | Status::Cancelled { .. } => (' ', String::new()),
    }
}

fn line(todo: &Todo, viewer: &Holder, notes: &Notes<'_>) -> String {
    let (mark, note) = mark_and_note(todo, Some(viewer));
    let blocking = blocking_note(todo, notes);
    let mut joined = match (note.is_empty(), blocking.is_empty()) {
        (true, true) => String::new(),
        (false, true) => note,
        (true, false) => format!(", {blocking}"),
        (false, false) => format!("{note}, {blocking}"),
    };
    if let Some(open) = notes.work.open.get(&todo.id) {
        let _ = write!(joined, ", {open} open");
    }
    format!(
        "- [{mark}] {} ({}{joined})",
        printable(notes.names.text(todo)),
        printable(&notes.names.id(&todo.id))
    )
}

/// A workspace todo listed under its ticket: its mark, text, id, holder, and
/// the node it sits on, `viewer` naming whose claim reads as the reader's own.
fn work_line(todo: &Todo, viewer: Option<&Holder>, names: &Names) -> String {
    let (mark, note) = mark_and_note(todo, viewer);
    format!(
        "- [{mark}] {} ({}{note}) on {}",
        printable(names.text(todo)),
        printable(&names.id(&todo.id)),
        printable(todo.node())
    )
}

/// `blocked by` while a blocker is open, `ready` once the last one closes
/// and the caller is offered the todo, and nothing for a todo with no edges.
fn blocking_note(todo: &Todo, notes: &Notes<'_>) -> String {
    if todo.blocked_by.is_empty() {
        return String::new();
    }
    let open = notes.graph.open_blockers(todo);
    if !open.is_empty() {
        let names = open
            .iter()
            .map(|blocker| {
                let id = notes.names.id(&blocker.id);
                if blocker.node() == todo.node() {
                    id.to_string()
                } else {
                    format!("{id} on {}", printable(blocker.node()))
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        return format!("blocked by {names}");
    }
    if notes.graph.is_ready(todo) && (notes.show_ready)(todo) {
        return "ready".to_string();
    }
    String::new()
}

/// Todos without an order follow the ordered ones, oldest first; the id
/// settles ties.
fn sort_key(t: &Todo) -> (bool, i64, Option<&str>, &str) {
    (
        t.order.is_none(),
        t.order.unwrap_or(0),
        t.entry.as_deref(),
        &t.id,
    )
}

/// Depth-first tree order. A parent outside `todos` leaves its child at the
/// top level, and todos caught in a parent cycle still show.
fn tree<'a>(todos: &[&'a Todo]) -> Vec<(&'a Todo, usize)> {
    let present: HashSet<&str> = todos.iter().map(|t| t.id.as_str()).collect();
    let parent_of = |t: &'a Todo| -> Option<&'a str> {
        t.parent
            .as_deref()
            .filter(|p| present.contains(p) && *p != t.id)
    };
    let mut children: HashMap<Option<&str>, Vec<&Todo>> = HashMap::new();
    for t in todos {
        children.entry(parent_of(t)).or_default().push(t);
    }
    for list in children.values_mut() {
        list.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk(&children, None, 0, &mut seen, &mut out);
    let mut stranded: Vec<&Todo> = todos
        .iter()
        .copied()
        .filter(|t| !seen.contains(t.id.as_str()))
        .collect();
    stranded.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    for t in stranded {
        if seen.insert(t.id.as_str()) {
            out.push((t, 0));
            walk(&children, Some(&t.id), 1, &mut seen, &mut out);
        }
    }
    out
}

fn walk<'a>(
    children: &HashMap<Option<&str>, Vec<&'a Todo>>,
    parent: Option<&str>,
    depth: usize,
    seen: &mut HashSet<&'a str>,
    out: &mut Vec<(&'a Todo, usize)>,
) {
    for t in children.get(&parent).into_iter().flatten() {
        if seen.insert(t.id.as_str()) {
            out.push((t, depth));
            walk(children, Some(&t.id), depth + 1, seen, out);
        }
    }
}

/// The line naming the pending todos other sessions left on `workspace`'s
/// session nodes, or `None` when there are none. A session never sees a
/// sibling's list, so this is how it learns there is one to pick up.
pub fn left_by_others(workspace: &str, pending: usize) -> Option<String> {
    let noun = if pending == 1 { "todo" } else { "todos" };
    (pending > 0).then(|| {
        format!(
            "Other sessions on `{workspace}` left {pending} pending {noun}; \
             `devkit todo list --subtree {workspace}` lists them."
        )
    })
}

/// What an agent is told ahead of its lists. It does not depend on the
/// backend.
pub fn guide(own_node: &str) -> String {
    format!(
        "Todo list, kept by devkit. Write your own todos to `{own_node}`.\n\
         - Track any work of three or more steps here before you start it.\n\
         - If you have a built-in task or plan tool, use it; devkit mirrors it here. \
         Otherwise use `devkit todo add \"<text>\"` (prints the id), `devkit todo start <id>`, \
         `devkit todo done <id>` and `devkit todo cancel <id>`.\n\
         - A sub-agent claims a todo with `devkit todo start <id>` before working on it.\n\
         - Ids are in parentheses.\n"
    )
}

/// A 16-hex-digit hash of rendered lists, to tell whether they changed since
/// the last injection.
pub fn digest(lists: &str) -> String {
    let mut hasher = DefaultHasher::new();
    lists.as_bytes().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use devkit_config::IdStyle;

    use super::*;
    use crate::{Holder, Status, Todo};

    fn todo(id: &str, text: &str, parent: Option<&str>, order: i64, status: Status) -> Todo {
        Todo {
            ticket: None,
            id: id.into(),
            description: text.into(),
            status,
            parent: parent.map(Into::into),
            order: Some(order),
            project: Some("r.main".into()),
            entry: None,
            modified: None,
            blocked_by: Vec::new(),
        }
    }

    fn nodes() -> Vec<String> {
        vec!["r.main".to_string()]
    }

    fn quiet<'a>(graph: &'a Graph<'a>) -> Notes<'a> {
        static SHORT: std::sync::LazyLock<Names> =
            std::sync::LazyLock::new(|| Names::plain(IdStyle::Short));
        Notes {
            graph,
            show_ready: &|_| false,
            work: TicketWork::none(),
            names: &SHORT,
        }
    }

    fn ip(by: &str) -> Status {
        Status::InProgress {
            by: Holder::new(by),
        }
    }

    #[test]
    fn open_todos_render_in_tree_order_under_their_node() {
        let todos = [
            todo("2", "two", Some("1"), 1024, Status::Pending),
            todo("1", "one", None, 1024, Status::Pending),
        ];
        assert_eq!(
            render_lists(
                &nodes(),
                &todos,
                &Holder::new("S"),
                &quiet(&Graph::new(&todos))
            ),
            "## r.main\n- [ ] one (1)\n  - [ ] two (2)\n"
        );
    }

    #[test]
    fn another_holders_claim_is_named() {
        let todos = [todo("3", "x", None, 1024, ip("S/a1"))];
        let out = render_lists(
            &nodes(),
            &todos,
            &Holder::new("S/a2"),
            &quiet(&Graph::new(&todos)),
        );
        assert!(out.contains("- [ ] x (3, in progress: a1)\n"), "{out}");
    }

    #[test]
    fn own_claim_reads_in_progress() {
        let todos = [todo("3", "x", None, 1024, ip("S/a1"))];
        let out = render_lists(
            &nodes(),
            &todos,
            &Holder::new("S/a1"),
            &quiet(&Graph::new(&todos)),
        );
        assert!(out.contains("- [ ] x (3, in progress)\n"), "{out}");
    }

    #[test]
    fn finished_todos_collapse_to_a_count() {
        let todos = [
            todo("1", "a", None, 1024, Status::Completed { by: None }),
            todo("2", "b", None, 2048, Status::Completed { by: None }),
            todo("3", "c", None, 3072, Status::Cancelled { by: None }),
            todo("4", "d", None, 4096, Status::Pending),
        ];
        assert_eq!(
            render_lists(
                &nodes(),
                &todos,
                &Holder::new("S"),
                &quiet(&Graph::new(&todos))
            ),
            "## r.main\n- [ ] d (4)\n2 done, 1 cancelled\n"
        );
    }

    #[test]
    fn a_node_with_nothing_open_is_left_out() {
        let todos = [todo("1", "a", None, 1024, Status::Completed { by: None })];
        assert_eq!(
            render_lists(
                &nodes(),
                &todos,
                &Holder::new("S"),
                &quiet(&Graph::new(&todos))
            ),
            ""
        );
    }

    #[test]
    fn nodes_render_in_the_order_given() {
        let mut deep = todo("1", "deep", None, 1024, Status::Pending);
        deep.project = Some("r.main.claude-s".into());
        let mut global = todo("2", "wide", None, 1024, Status::Pending);
        global.project = None;
        let nodes = ["r.main.claude-s", "r.main", "r", "global"].map(String::from);
        let listed = [global, deep];
        assert_eq!(
            render_lists(
                &nodes,
                &listed,
                &Holder::new("S"),
                &quiet(&Graph::new(&listed))
            ),
            "## r.main.claude-s\n- [ ] deep (1)\n\n## global\n- [ ] wide (2)\n"
        );
    }

    #[test]
    fn a_full_listing_shows_finished_todos() {
        let todos = [
            todo("1", "a", None, 1024, Status::Completed { by: None }),
            todo("2", "b", None, 2048, Status::Cancelled { by: None }),
        ];
        assert_eq!(
            render_full(
                &nodes(),
                &todos,
                &Holder::new("S"),
                &quiet(&Graph::new(&todos))
            ),
            "## r.main\n- [x] a (1)\n1 cancelled\n"
        );
    }

    #[test]
    fn a_full_listing_counts_a_node_holding_only_cancelled_todos() {
        let todos = [todo("1", "a", None, 1024, Status::Cancelled { by: None })];
        assert_eq!(
            render_full(
                &nodes(),
                &todos,
                &Holder::new("S"),
                &quiet(&Graph::new(&todos))
            ),
            "## r.main\n1 cancelled\n"
        );
        assert_eq!(
            render_lists(
                &nodes(),
                &todos,
                &Holder::new("S"),
                &quiet(&Graph::new(&todos))
            ),
            ""
        );
    }

    #[test]
    fn a_uuid_id_renders_short() {
        let todos = [todo(
            "96432cd6-082a-4d8c-a9cb-adef8823ff92",
            "x",
            None,
            1024,
            Status::Pending,
        )];
        assert_eq!(
            render_lists(
                &nodes(),
                &todos,
                &Holder::new("S"),
                &quiet(&Graph::new(&todos))
            ),
            "## r.main\n- [ ] x (96432cd6)\n"
        );
    }

    const UUID: &str = "96432cd6-082a-4d8c-a9cb-adef8823ff92";

    fn ticket_todos() -> Vec<Todo> {
        let mut issue = todo(
            "1234abcd-0000-4000-8000-000000000001",
            "#65 Issue 65",
            None,
            1024,
            Status::Pending,
        );
        issue.ticket = Some(crate::TicketRef {
            tracker: "github".into(),
            key: "o/r#65".into(),
            synced_status: None,
        });
        vec![issue, todo(UUID, "plain", None, 2048, Status::Pending)]
    }

    fn named(todos: &[Todo]) -> Names {
        Names::new(IdStyle::Name, todos, |_| "#65".to_string())
    }

    #[test]
    fn plan_rows_show_names() {
        let todos = ticket_todos();
        let graph = Graph::new(&todos);
        let plan = graph.plan(|_| true, None);
        let out = render_plan("r", &plan, TicketWork::none(), &named(&todos));
        assert!(out.contains("o #65  Issue 65"), "{out}");
        let word = crate::names::word_name(UUID).unwrap();
        assert!(out.contains(&format!("o {word}  plain")), "{out}");
        assert!(!out.contains("96432cd6"), "{out}");
    }

    #[test]
    fn list_lines_show_names_in_parentheses() {
        let todos = ticket_todos();
        let graph = Graph::new(&todos);
        let names = named(&todos);
        let notes = Notes {
            names: &names,
            ..quiet(&graph)
        };
        let out = render_lists(&nodes(), &todos, &Holder::new("S"), &notes);
        assert!(out.contains("- [ ] Issue 65 (#65)\n"), "{out}");
    }

    #[test]
    fn mermaid_keeps_hex_node_ids_and_labels_names() {
        let todos = ticket_todos();
        let graph = Graph::new(&todos);
        let plan = graph.plan(|_| true, None);
        let out = render_mermaid(&plan, &named(&todos));
        let word = crate::names::word_name(UUID).unwrap();
        assert!(
            out.contains(&format!("t96432cd6[\"{word}: plain\"]")),
            "{out}"
        );
        assert!(out.contains("t1234abcd[\"#65: Issue 65\"]"), "{out}");
    }

    #[test]
    fn the_guide_names_the_node() {
        assert!(guide("r.main.claude-s").contains("`r.main.claude-s`"));
    }

    #[test]
    fn digest_is_stable() {
        assert_eq!(digest("a"), digest("a"));
        assert_ne!(digest("a"), digest("b"));
        assert_eq!(digest("a").len(), 16);
    }
}
