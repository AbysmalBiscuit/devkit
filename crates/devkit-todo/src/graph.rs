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
        // A started todo is already running, so its own blockers no longer
        // gate it anywhere in the plan. Its dependents still wait on it.
        let waits: HashMap<&str, Vec<&Todo>> = members
            .iter()
            .map(|todo| {
                let waits = match todo.status.kind() {
                    StatusKind::InProgress => Vec::new(),
                    _ => self.open_blockers(todo),
                };
                (todo.id.as_str(), waits)
            })
            .collect();
        // An open blocker outside the plan never finishes here, so a member
        // waiting on it, directly or through another member, is not offered.
        let mut waiting_outside: Vec<&str> = members
            .iter()
            .filter(|todo| {
                waits[todo.id.as_str()]
                    .iter()
                    .any(|blocker| !is_member(&blocker.id))
            })
            .map(|todo| todo.id.as_str())
            .collect();
        let mut grew = true;
        while grew {
            grew = false;
            for todo in &members {
                if waiting_outside.contains(&todo.id.as_str()) {
                    continue;
                }
                let waits_outside = waits[todo.id.as_str()]
                    .iter()
                    .any(|blocker| waiting_outside.contains(&blocker.id.as_str()));
                if waits_outside {
                    waiting_outside.push(todo.id.as_str());
                    grew = true;
                }
            }
        }
        let outside = |todo: &Todo| waiting_outside.contains(&todo.id.as_str());
        let mut waits_on: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut dependents_of: HashMap<&str, Vec<&str>> = HashMap::new();
        for todo in &members {
            let member_waits: Vec<&str> = waits[todo.id.as_str()]
                .iter()
                .filter(|blocker| is_member(&blocker.id))
                .map(|blocker| blocker.id.as_str())
                .collect();
            for blocker in &member_waits {
                dependents_of
                    .entry(blocker)
                    .or_default()
                    .push(todo.id.as_str());
            }
            waits_on.insert(todo.id.as_str(), member_waits);
        }

        // Stored member-to-member edges, so starting a todo cannot hide a
        // cycle it sits on.
        let on_member_cycle = |id: &str| {
            let mut seen = Vec::new();
            let mut stack = vec![id];
            while let Some(current) = stack.pop() {
                if seen.contains(&current) {
                    continue;
                }
                seen.push(current);
                let Some(todo) = self.get(current) else {
                    continue;
                };
                for blocker in self.open_blockers(todo) {
                    if !is_member(&blocker.id) {
                        continue;
                    }
                    if blocker.id == id {
                        return true;
                    }
                    stack.push(&blocker.id);
                }
            }
            false
        };
        let mut cycled: Vec<&Todo> = members
            .iter()
            .copied()
            .filter(|todo| on_member_cycle(&todo.id))
            .collect();
        cycled.sort_by(|a, b| a.id.cmp(&b.id));

        let mut indegree: HashMap<&str, usize> = HashMap::new();
        for todo in &members {
            if cycled.iter().any(|on| on.id == todo.id) {
                continue;
            }
            indegree.insert(todo.id.as_str(), waits_on[todo.id.as_str()].len());
        }
        let mut placed: HashMap<&str, usize> = HashMap::new();
        let mut batches: Vec<Vec<&Todo>> = Vec::new();
        loop {
            let mut ready: Vec<&Todo> = members
                .iter()
                .copied()
                .filter(|todo| !placed.contains_key(todo.id.as_str()) && !outside(todo))
                .filter(|todo| indegree.get(todo.id.as_str()).copied().unwrap_or(1) == 0)
                .collect();
            if ready.is_empty() {
                break;
            }
            // Ranks come from the members still unplaced, so a later batch
            // prefers the longest chain that remains.
            let left: Vec<&Todo> = members
                .iter()
                .copied()
                .filter(|todo| !placed.contains_key(todo.id.as_str()))
                .collect();
            let ranks = ranks_of(&left, &dependents_of);
            ready.sort_by(|a, b| ranks[a.id.as_str()].cmp(&ranks[b.id.as_str()]));
            // Started todos wait on nothing, so they all land in batch 1 and
            // count against its cap without being moved out of it.
            let (mut batch, rest): (Vec<&Todo>, Vec<&Todo>) = ready
                .into_iter()
                .partition(|todo| todo.status.kind() == StatusKind::InProgress);
            let room = cap.map_or(usize::MAX, |cap| cap.get().saturating_sub(batch.len()));
            batch.extend(rest.into_iter().take(room));
            if batch.is_empty() {
                break;
            }
            let batch_index = batches.len();
            for todo in &batch {
                placed.insert(todo.id.as_str(), batch_index);
                for dependent in dependents_of.get(todo.id.as_str()).into_iter().flatten() {
                    if let Some(left) = indegree.get_mut(dependent) {
                        *left = left.saturating_sub(1);
                    }
                }
            }
            batches.push(batch);
        }

        let mut cycle = cycled;
        cycle.retain(|todo| !outside(todo));
        let acyclic: Vec<&Todo> = members
            .iter()
            .copied()
            .filter(|todo| placed.contains_key(todo.id.as_str()))
            .collect();
        let critical = critical_path(&acyclic, &dependents_of);
        Plan {
            batches,
            critical,
            cycle,
        }
    }
}

/// One member's rank: the longest remaining chain of dependents, then how
/// many members wait on it, then its order, then its id. A smaller rank is
/// scheduled first.
#[derive(Clone, Copy)]
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

/// Each member's rank: longest remaining chain of dependents, then how many
/// members wait on it. One pass over a reverse topological order.
fn ranks_of<'a>(
    members: &[&'a Todo],
    dependents_of: &HashMap<&str, Vec<&str>>,
) -> HashMap<&'a str, Rank<'a>> {
    let mut chain: HashMap<&str, usize> = HashMap::new();
    for todo in topo(members, dependents_of).iter().rev() {
        let below = dependents_of
            .get(todo.id.as_str())
            .into_iter()
            .flatten()
            .filter_map(|id| chain.get(id).copied())
            .max()
            .unwrap_or(0);
        chain.insert(todo.id.as_str(), below + 1);
    }
    members
        .iter()
        .map(|todo| {
            let dependents = dependents_of
                .get(todo.id.as_str())
                .map_or(0, |ids| ids.len());
            (todo.id.as_str(), Rank {
                chain: chain.get(todo.id.as_str()).copied().unwrap_or(1),
                dependents,
                order: todo.order.unwrap_or(i64::MAX),
                id: todo.id.as_str(),
            })
        })
        .collect()
}

/// Members in an order where every blocker precedes its dependents. A cycle
/// among them is omitted: the caller already placed only the acyclic ones,
/// or is ranking a batch whose members have no remaining waits.
fn topo<'a>(members: &[&'a Todo], dependents_of: &HashMap<&str, Vec<&str>>) -> Vec<&'a Todo> {
    let ids: HashMap<&str, &Todo> = members
        .iter()
        .map(|todo| (todo.id.as_str(), *todo))
        .collect();
    let mut indegree: HashMap<&str, usize> =
        members.iter().map(|todo| (todo.id.as_str(), 0)).collect();
    for todo in members {
        for dependent in dependents_of.get(todo.id.as_str()).into_iter().flatten() {
            if let Some(count) = indegree.get_mut(dependent) {
                *count += 1;
            }
        }
    }
    let mut ready: Vec<&str> = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(id, _)| *id)
        .collect();
    ready.sort_unstable();
    let mut order = Vec::new();
    while let Some(id) = ready.pop() {
        let Some(todo) = ids.get(id) else {
            continue;
        };
        order.push(*todo);
        let mut freed = Vec::new();
        for dependent in dependents_of.get(id).into_iter().flatten() {
            if let Some(count) = indegree.get_mut(dependent) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    freed.push(*dependent);
                }
            }
        }
        freed.sort_unstable();
        ready.extend(freed);
    }
    order
}

/// The longest chain of members. Ties go to the chain whose first member
/// ranks higher, then the next, through to the last.
fn critical_path<'a>(
    members: &[&'a Todo],
    dependents_of: &HashMap<&str, Vec<&str>>,
) -> Vec<&'a Todo> {
    if members.is_empty() {
        return Vec::new();
    }
    let ranks = ranks_of(members, dependents_of);
    let by_id: HashMap<&str, &Todo> = members
        .iter()
        .map(|todo| (todo.id.as_str(), *todo))
        .collect();
    let rank = |id: &str| ranks[id];
    // Prefer the chain that starts at the better rank. Compare rank sequences
    // with the preferred rank as greater, since `max_by` keeps the greater.
    let better = |a: &[&str], b: &[&str]| {
        a.len()
            .cmp(&b.len())
            .then_with(|| b.iter().map(|id| rank(id)).cmp(a.iter().map(|id| rank(id))))
    };
    let mut best_from: HashMap<&str, Vec<&str>> = HashMap::new();
    for todo in topo(members, dependents_of).iter().rev() {
        let mut best = vec![todo.id.as_str()];
        let next = dependents_of
            .get(todo.id.as_str())
            .into_iter()
            .flatten()
            .filter_map(|id| best_from.get(id))
            .max_by(|a, b| better(a, b));
        if let Some(next) = next {
            best.extend(next);
        }
        best_from.insert(todo.id.as_str(), best);
    }
    best_from
        .values()
        .max_by(|a, b| better(a, b))
        .map(|ids| ids.iter().map(|id| by_id[id]).collect())
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
