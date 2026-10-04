//! The todo lists as an agent reads them: open todos in full, finished ones
//! counted, under a heading per node.

use std::{
    collections::{HashMap, HashSet},
    fmt::Write,
    hash::{DefaultHasher, Hash, Hasher},
};

use devkit_common::ui::printable;

use crate::{Holder, Status, StatusKind, Todo};

/// The lists for the injected context: each node in `nodes` order, open todos
/// in tree order, finished todos as one count line. A node with nothing open
/// is left out.
pub fn render_lists(nodes: &[String], todos: &[Todo], viewer: &Holder) -> String {
    render(nodes, todos, viewer, false)
}

/// As [`render_lists`], with completed todos shown in full as `- [x]`, for a
/// person or agent asking for the listing.
pub fn render_full(nodes: &[String], todos: &[Todo], viewer: &Holder) -> String {
    render(nodes, todos, viewer, true)
}

fn render(nodes: &[String], todos: &[Todo], viewer: &Holder, full: bool) -> String {
    let sections: Vec<String> = nodes
        .iter()
        .filter_map(|node| {
            let here: Vec<&Todo> = todos.iter().filter(|t| t.node() == node).collect();
            section(node, &here, viewer, full)
        })
        .collect();
    sections.join("\n")
}

fn section(node: &str, todos: &[&Todo], viewer: &Holder, full: bool) -> Option<String> {
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
        let _ = writeln!(out, "{}{}", "  ".repeat(depth), line(todo, viewer));
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

fn line(todo: &Todo, viewer: &Holder) -> String {
    let (mark, note) = match &todo.status {
        Status::InProgress { by } if by == viewer => (' ', ", in progress".to_string()),
        Status::InProgress { by } => (' ', format!(", in progress: {}", printable(by.name()))),
        Status::Completed { .. } => ('x', String::new()),
        Status::Pending | Status::Cancelled { .. } => (' ', String::new()),
    };
    format!(
        "- [{mark}] {} ({}{note})",
        printable(&todo.description),
        printable(crate::short_id(&todo.id))
    )
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
    use super::*;
    use crate::{Holder, Status, Todo};

    fn todo(id: &str, text: &str, parent: Option<&str>, order: i64, status: Status) -> Todo {
        Todo {
            id: id.into(),
            description: text.into(),
            status,
            parent: parent.map(Into::into),
            order: Some(order),
            project: Some("r.main".into()),
            entry: None,
            modified: None,
        }
    }

    fn nodes() -> Vec<String> {
        vec!["r.main".to_string()]
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
            render_lists(&nodes(), &todos, &Holder::new("S")),
            "## r.main\n- [ ] one (1)\n  - [ ] two (2)\n"
        );
    }

    #[test]
    fn another_holders_claim_is_named() {
        let todos = [todo("3", "x", None, 1024, ip("S/a1"))];
        let out = render_lists(&nodes(), &todos, &Holder::new("S/a2"));
        assert!(out.contains("- [ ] x (3, in progress: a1)\n"), "{out}");
    }

    #[test]
    fn own_claim_reads_in_progress() {
        let todos = [todo("3", "x", None, 1024, ip("S/a1"))];
        let out = render_lists(&nodes(), &todos, &Holder::new("S/a1"));
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
            render_lists(&nodes(), &todos, &Holder::new("S")),
            "## r.main\n- [ ] d (4)\n2 done, 1 cancelled\n"
        );
    }

    #[test]
    fn a_node_with_nothing_open_is_left_out() {
        let todos = [todo("1", "a", None, 1024, Status::Completed { by: None })];
        assert_eq!(render_lists(&nodes(), &todos, &Holder::new("S")), "");
    }

    #[test]
    fn nodes_render_in_the_order_given() {
        let mut deep = todo("1", "deep", None, 1024, Status::Pending);
        deep.project = Some("r.main.claude-s".into());
        let mut global = todo("2", "wide", None, 1024, Status::Pending);
        global.project = None;
        let nodes = ["r.main.claude-s", "r.main", "r", "global"].map(String::from);
        assert_eq!(
            render_lists(&nodes, &[global, deep], &Holder::new("S")),
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
            render_full(&nodes(), &todos, &Holder::new("S")),
            "## r.main\n- [x] a (1)\n1 cancelled\n"
        );
    }

    #[test]
    fn a_full_listing_counts_a_node_holding_only_cancelled_todos() {
        let todos = [todo("1", "a", None, 1024, Status::Cancelled { by: None })];
        assert_eq!(
            render_full(&nodes(), &todos, &Holder::new("S")),
            "## r.main\n1 cancelled\n"
        );
        assert_eq!(render_lists(&nodes(), &todos, &Holder::new("S")), "");
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
            render_lists(&nodes(), &todos, &Holder::new("S")),
            "## r.main\n- [ ] x (96432cd6)\n"
        );
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
