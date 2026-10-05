//! How todos map to taskchampion tasks shared with Taskwarrior.

use devkit_todo::{Status, Todo};
use serde::Deserialize;

/// The root project devkit keeps its todos under unless configured
/// otherwise.
pub const DEFAULT_ROOT: &str = "devkit";

/// The taskwarrior project a node's todos are filed under: `root` itself for
/// the global list, `<root>.<node>` for any other node.
pub fn project_of(root: &str, node: Option<&str>) -> String {
    match node.filter(|n| *n != devkit_todo::node::GLOBAL) {
        Some(node) => format!("{root}.{node}"),
        None => root.to_string(),
    }
}

/// The node `project` files a todo on under `root`: `Some(None)` for the
/// global list, and `None` for a project outside `root`, whose tasks are not
/// todos.
pub fn node_of(root: &str, project: &str) -> Option<Option<String>> {
    if project == root {
        return Some(None);
    }
    let node = project.strip_prefix(root)?.strip_prefix('.')?;
    (!node.is_empty()).then(|| Some(node.to_string()))
}

/// A task as `task export` prints it.
#[derive(Debug, Deserialize)]
pub struct Exported {
    pub uuid: String,
    pub description: String,
    pub status: String,
    pub start: Option<String>,
    pub holder: Option<String>,
    pub subof: Option<String>,
    #[serde(default, deserialize_with = "integer_order")]
    pub order: Option<i64>,
    pub project: Option<String>,
    pub entry: Option<String>,
    pub modified: Option<String>,
}

/// Numeric UDAs export as JSON numbers that may carry a fraction.
fn integer_order<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.map(|n| n.round() as i64))
}

impl Exported {
    /// The todo this task is under `root`, or `None` for a task that is not
    /// one: a project outside `root`, no project at all, or a status other
    /// than pending, completed or deleted.
    ///
    /// A pending task started with no `holder` was started outside devkit,
    /// by `task start` or alacritree, so it reads as held by a person and no
    /// agent takes it over.
    pub fn into_todo(self, root: &str) -> Option<Todo> {
        let project = node_of(root, self.project.as_deref()?)?;
        let holder = self
            .holder
            .filter(|h| !h.is_empty())
            .map(devkit_todo::Holder::new);
        let status = match self.status.as_str() {
            "pending" if self.start.is_some() => Status::InProgress {
                by: holder.unwrap_or_else(devkit_todo::Holder::human),
            },
            "pending" => Status::Pending,
            "completed" => Status::Completed { by: holder },
            "deleted" => Status::Cancelled { by: holder },
            _ => return None,
        };
        Some(Todo {
            id: self.uuid,
            description: self.description,
            status,
            parent: self.subof,
            order: self.order,
            project,
            entry: self.entry.as_deref().and_then(rfc3339),
            modified: self.modified.as_deref().and_then(rfc3339),
        })
    }
}

/// Taskwarrior's `20261003T120000Z` as RFC 3339, `2026-10-03T12:00:00Z`.
pub fn rfc3339(taskwarrior_date: &str) -> Option<String> {
    chrono::NaiveDateTime::parse_from_str(taskwarrior_date, "%Y%m%dT%H%M%SZ")
        .ok()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

#[cfg(test)]
mod tests {
    use devkit_todo::Holder;

    use super::*;

    fn exported(json: serde_json::Value) -> Exported {
        let mut task = serde_json::json!({
            "uuid": "96432cd6-082a-4d8c-a9cb-adef8823ff92",
            "description": "d",
            "status": "pending",
            "project": "devkit.r.main",
        });
        task.as_object_mut()
            .unwrap()
            .extend(json.as_object().unwrap().clone());
        serde_json::from_value(task).unwrap()
    }

    fn status_of(json: serde_json::Value) -> Option<Status> {
        exported(json).into_todo(DEFAULT_ROOT).map(|t| t.status)
    }

    #[test]
    fn pending_with_start_and_holder_is_in_progress() {
        assert_eq!(
            status_of(serde_json::json!({"start": "20261003T120000Z", "holder": "S/a"})),
            Some(Status::InProgress {
                by: Holder::new("S/a")
            })
        );
    }

    #[test]
    fn pending_with_start_and_no_holder_is_held_by_human() {
        assert_eq!(
            status_of(serde_json::json!({"start": "20261003T120000Z"})),
            Some(Status::InProgress {
                by: Holder::human()
            })
        );
        assert_eq!(
            status_of(serde_json::json!({"holder": "S"})),
            Some(Status::Pending)
        );
    }

    #[test]
    fn completed_and_deleted_carry_their_holder() {
        assert_eq!(
            status_of(serde_json::json!({"status": "completed", "holder": "S"})),
            Some(Status::Completed {
                by: Some(Holder::new("S"))
            })
        );
        assert_eq!(
            status_of(serde_json::json!({"status": "deleted"})),
            Some(Status::Cancelled { by: None })
        );
    }

    #[test]
    fn recurring_and_unknown_statuses_are_not_todos() {
        assert_eq!(status_of(serde_json::json!({"status": "recurring"})), None);
        assert_eq!(status_of(serde_json::json!({"status": "waiting"})), None);
    }

    #[test]
    fn the_root_reads_as_global_and_no_project_is_not_a_todo() {
        let global = exported(serde_json::json!({"project": "devkit"}));
        assert_eq!(global.into_todo(DEFAULT_ROOT).unwrap().project, None);
        let mut unfiled = exported(serde_json::json!({}));
        unfiled.project = None;
        assert_eq!(unfiled.into_todo(DEFAULT_ROOT), None);
    }

    #[test]
    fn fractional_order_rounds() {
        let task = exported(serde_json::json!({"order": 1024.4}));
        assert_eq!(task.into_todo(DEFAULT_ROOT).unwrap().order, Some(1024));
    }

    #[test]
    fn dates_convert_to_rfc3339() {
        assert_eq!(
            rfc3339("20261003T120000Z").as_deref(),
            Some("2026-10-03T12:00:00Z")
        );
        assert_eq!(rfc3339("yesterday"), None);
        let task = exported(serde_json::json!({"entry": "20261003T120000Z"}));
        assert_eq!(
            task.into_todo(DEFAULT_ROOT).unwrap().entry.as_deref(),
            Some("2026-10-03T12:00:00Z")
        );
    }

    #[test]
    fn only_projects_under_the_root_name_a_node() {
        assert_eq!(node_of("devkit", "devkit"), Some(None));
        assert_eq!(
            node_of("devkit", "devkit.r.main"),
            Some(Some("r.main".into()))
        );
        assert_eq!(node_of("devkit", "r.main"), None);
        assert_eq!(node_of("devkit", "devkit-web.r"), None);
        assert_eq!(node_of("devkit", "devkitx"), None);
        let home = exported(serde_json::json!({"project": "home"}));
        assert_eq!(home.into_todo(DEFAULT_ROOT), None);
    }
}
