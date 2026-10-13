//! The blocking graph, computed once from listed todos. Backends store the
//! edges; every caller reads readiness, batches and cycles from here.

use std::{cmp::Ordering, collections::HashMap, num::NonZeroUsize};

use anyhow::Context;

use crate::{StatusKind, Todo, TodoStore};

/// The blocking graph over `todos`.
pub struct Graph<'a> {
    todos: &'a [Todo],
    index: HashMap<&'a str, usize>,
}

impl<'a> Graph<'a> {
    pub fn new(todos: &'a [Todo]) -> Self {
        let index = todos
            .iter()
            .enumerate()
            .map(|(i, todo)| (todo.id.as_str(), i))
            .collect();
        Self { todos, index }
    }

    fn get(&self, id: &str) -> Option<&'a Todo> {
        self.index.get(id).map(|i| &self.todos[*i])
    }

    /// Blockers present in the graph and still open, in id order.
    pub fn open_blockers(&self, todo: &Todo) -> Vec<&'a Todo> {
        let mut blockers: Vec<&Todo> = todo
            .blocked_by
            .iter()
            .filter_map(|id| self.get(id))
            .filter(|blocker| is_open(&blocker.status.kind()))
            .collect();
        blockers.sort_by(|a, b| a.id.cmp(&b.id));
        blockers
    }

    pub fn is_blocked(&self, todo: &Todo) -> bool {
        !self.open_blockers(todo).is_empty()
    }

    /// Pending, not blocked, and not on a cycle.
    pub fn is_ready(&self, todo: &Todo) -> bool {
        todo.status.kind() == StatusKind::Pending
            && !self.is_blocked(todo)
            && !self.on_cycle(&todo.id)
    }

    fn on_cycle(&self, id: &str) -> bool {
        let Some(start) = self.get(id) else {
            return false;
        };
        let mut seen = Vec::new();
        let mut stack = vec![start.id.as_str()];
        while let Some(current) = stack.pop() {
            if seen.contains(&current) {
                continue;
            }
            seen.push(current);
            let Some(todo) = self.get(current) else {
                continue;
            };
            for blocker in self.open_blockers(todo) {
                if blocker.id == id {
                    return true;
                }
                stack.push(&blocker.id);
            }
        }
        false
    }

    /// Ready todos listing any of `closed` as a blocker, each once, in id
    /// order.
    pub fn unblocked_by(&self, closed: &[&str]) -> Vec<&'a Todo> {
        let mut ready: Vec<&Todo> = self
            .todos
            .iter()
            .filter(|todo| self.is_ready(todo))
            .filter(|todo| {
                todo.blocked_by
                    .iter()
                    .any(|id| closed.contains(&id.as_str()))
            })
            .collect();
        ready.sort_by(|a, b| a.id.cmp(&b.id));
        ready
    }

    /// The plan of the open todos `offered` accepts.
    pub fn plan(&self, offered: impl Fn(&Todo) -> bool, cap: Option<NonZeroUsize>) -> Plan<'a> {
        let members: Vec<&Todo> = self
            .todos
            .iter()
            .filter(|todo| is_open(&todo.status.kind()) && offered(todo))
            .collect();
        let is_member = |id: &str| members.iter().any(|todo| todo.id == id);
        let mut blocked_of: HashMap<&str, Vec<&Todo>> = HashMap::new();
        let mut waits_on: HashMap<&str, Vec<&Todo>> = HashMap::new();
        for todo in &members {
            for blocker in self
                .open_blockers(todo)
                .into_iter()
                .filter(|blocker| is_member(&blocker.id))
            {
                blocked_of
                    .entry(blocker.id.as_str())
                    .or_default()
                    .push(todo);
            }
            waits_on.insert(
                todo.id.as_str(),
                self.open_blockers(todo)
                    .into_iter()
                    .filter(|blocker| is_member(&blocker.id))
                    .collect(),
            );
        }
        let waits = |todo: &Todo| waits_on.get(todo.id.as_str()).cloned().unwrap_or_default();
        let blocked = |todo: &Todo| {
            blocked_of
                .get(todo.id.as_str())
                .cloned()
                .unwrap_or_default()
        };
        let mut ranks: HashMap<&str, Rank> = HashMap::new();
        for todo in &members {
            let dependents = members
                .iter()
                .filter(|other| other.blocked_by.iter().any(|id| id == &todo.id))
                .count();
            ranks.insert(todo.id.as_str(), Rank {
                chain: chain_length(todo, &blocked),
                dependents,
                order: todo.order.unwrap_or(i64::MAX),
                id: todo.id.as_str(),
            });
        }
        let rank = |todo: &Todo| ranks.get(todo.id.as_str()).unwrap().clone();

        let mut placed: HashMap<&str, usize> = HashMap::new();
        let mut batches: Vec<Vec<&Todo>> = Vec::new();
        loop {
            let ready: Vec<&Todo> = members
                .iter()
                .copied()
                .filter(|todo| !placed.contains_key(todo.id.as_str()))
                .filter(|todo| {
                    waits(todo)
                        .iter()
                        .all(|blocker| placed.contains_key(blocker.id.as_str()))
                })
                .collect();
            if ready.is_empty() {
                break;
            }
            let batch_index = batches.len();
            let mut batch = Vec::new();
            if batch_index == 0 {
                let mut started: Vec<&Todo> = ready
                    .iter()
                    .copied()
                    .filter(|todo| todo.status.kind() == StatusKind::InProgress)
                    .collect();
                started.sort_by(|a, b| rank(a).cmp(&rank(b)));
                batch.extend(started);
            }
            let mut waiting: Vec<&Todo> = ready
                .iter()
                .copied()
                .filter(|todo| todo.status.kind() != StatusKind::InProgress)
                .collect();
            waiting.sort_by(|a, b| rank(a).cmp(&rank(b)));
            let room = cap.map_or(usize::MAX, |cap| cap.get().saturating_sub(batch.len()));
            batch.extend(waiting.into_iter().take(room));
            for todo in &batch {
                placed.insert(&todo.id, batch_index);
            }
            batches.push(batch);
        }

        let critical = longest_chain(&members, &blocked, rank);
        let mut cycle: Vec<&Todo> = members
            .iter()
            .copied()
            .filter(|todo| !placed.contains_key(todo.id.as_str()))
            .collect();
        cycle.sort_by(|a, b| a.id.cmp(&b.id));
        Plan {
            batches,
            critical,
            cycle,
        }
    }
}

/// One batch's ranking: the longest remaining chain, then how many todos
/// wait on it, then its order, then its id.
#[derive(Clone)]
struct Rank<'a> {
    chain: usize,
    dependents: usize,
    order: i64,
    id: &'a str,
}

impl Ord for Rank<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.chain
            .cmp(&other.chain)
            .reverse()
            .then(self.dependents.cmp(&other.dependents).reverse())
            .then(self.order.cmp(&other.order))
            .then(self.id.cmp(other.id))
    }
}

impl PartialOrd for Rank<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Rank<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Rank<'_> {}

/// How many members sit on the longest chain of open blockers ending at
/// `todo`, counting `todo`.
fn chain_length<'a>(todo: &'a Todo, blockers: &impl Fn(&Todo) -> Vec<&'a Todo>) -> usize {
    let mut seen = Vec::new();
    fn walk<'a>(
        todo: &'a Todo,
        blockers: &impl Fn(&Todo) -> Vec<&'a Todo>,
        seen: &mut Vec<&'a str>,
    ) -> usize {
        if seen.contains(&todo.id.as_str()) {
            return 0;
        }
        seen.push(&todo.id);
        let below = blockers(todo)
            .into_iter()
            .map(|blocker| walk(blocker, blockers, seen))
            .max()
            .unwrap_or(0);
        seen.pop();
        below + 1
    }
    walk(todo, blockers, &mut seen)
}

/// The longest chain of members. Ties go to the chain whose first member
/// ranks higher, then the next, through to the last.
fn longest_chain<'a>(
    members: &[&'a Todo],
    next: &impl Fn(&Todo) -> Vec<&'a Todo>,
    rank: impl Fn(&Todo) -> Rank<'a>,
) -> Vec<&'a Todo> {
    fn walk<'a>(
        todo: &'a Todo,
        next: &impl Fn(&Todo) -> Vec<&'a Todo>,
        rank: &impl Fn(&Todo) -> Rank<'a>,
        seen: &[&'a Todo],
    ) -> Vec<&'a Todo> {
        let mut chain = vec![todo];
        let rest = next(todo)
            .into_iter()
            .filter(|step| !seen.iter().any(|have| have.id == step.id))
            .map(|step| {
                let mut longer = seen.to_vec();
                longer.push(todo);
                walk(step, next, rank, &longer)
            })
            .max_by(|a, b| {
                a.len().cmp(&b.len()).then_with(|| {
                    a.iter()
                        .map(|todo| rank(todo))
                        .cmp(b.iter().map(|todo| rank(todo)))
                })
            });
        if let Some(rest) = rest {
            chain.extend(rest);
        }
        chain
    }
    members
        .iter()
        .map(|start| walk(start, next, &rank, &[]))
        .max_by(|a, b| {
            a.len().cmp(&b.len()).then_with(|| {
                a.iter()
                    .map(|todo| rank(todo))
                    .cmp(b.iter().map(|todo| rank(todo)))
            })
        })
        .unwrap_or_default()
}

/// The open todos of a plan, in batches, and the cycle Kahn's algorithm
/// never placed.
pub struct Plan<'a> {
    pub batches: Vec<Vec<&'a Todo>>,
    pub critical: Vec<&'a Todo>,
    pub cycle: Vec<&'a Todo>,
}

impl<'a> Plan<'a> {
    /// Batch 1 without the todos already in progress.
    pub fn frontier(&self) -> Vec<&'a Todo> {
        self.batches
            .first()
            .map(|batch| {
                batch
                    .iter()
                    .copied()
                    .filter(|todo| todo.status.kind() != StatusKind::InProgress)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Whether a blocker of this kind still holds its dependents.
fn is_open(kind: &StatusKind) -> bool {
    match kind {
        StatusKind::Pending | StatusKind::InProgress => true,
        StatusKind::Completed | StatusKind::Cancelled => false,
    }
}

/// The ids of the cycle adding `by` to `id` would close, starting and
/// ending at `id`, or `None` when the edge stays acyclic.
pub fn cycle_through(todos: &[Todo], id: &str, by: &[String]) -> Option<Vec<String>> {
    let graph = Graph::new(todos);
    for blocker in by {
        if blocker == id {
            return Some(vec![id.to_string(), id.to_string()]);
        }
        if let Some(path) = reaches(&graph, blocker, id) {
            let mut cycle = vec![id.to_string()];
            cycle.extend(path);
            return Some(cycle);
        }
    }
    None
}

/// The ids from `from` to `target` along stored edges, `from` first and
/// `target` last, or `None` when no such path exists.
fn reaches(graph: &Graph<'_>, from: &str, target: &str) -> Option<Vec<String>> {
    let mut stack = vec![(from.to_string(), vec![from.to_string()])];
    let mut seen = Vec::new();
    while let Some((current, path)) = stack.pop() {
        if seen.contains(&current) {
            continue;
        }
        seen.push(current.clone());
        let Some(todo) = graph.get(&current) else {
            continue;
        };
        for next in &todo.blocked_by {
            if next == target {
                let mut found = path.clone();
                found.push(next.clone());
                return Some(found);
            }
            if graph.get(next).is_some() && !path.iter().any(|seen| seen == next) {
                let mut longer = path.clone();
                longer.push(next.clone());
                stack.push((next.clone(), longer));
            }
        }
    }
    None
}

/// `todos` plus each blocker the list lacks, fetched with `get`. A blocker
/// the store does not hold is skipped, and only one level is fetched.
pub fn with_blockers(store: &impl TodoStore, mut todos: Vec<Todo>) -> anyhow::Result<Vec<Todo>> {
    let mut missing = Vec::new();
    for todo in &todos {
        for blocker in &todo.blocked_by {
            if todos.iter().any(|have| &have.id == blocker) || missing.contains(blocker) {
                continue;
            }
            missing.push(blocker.clone());
        }
    }
    for id in missing {
        if let Some(blocker) = store
            .get(&id)
            .with_context(|| format!("reading todo {id}"))?
        {
            todos.push(blocker);
        }
    }
    Ok(todos)
}
