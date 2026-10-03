//! How a todo is written as a taskwarrior task and read back. Public because
//! any backend that keeps todos in a taskwarrior database writes the same
//! tasks.

use devkit_todo::{Status, Todo};
use serde::Deserialize;

/// Passed as `rc.` overrides on every call, so a taskrc that declares none of
/// them never folds `holder:` into a description. `subof` and `order` are
/// alacritree's, so its tab nests and orders devkit's todos; `holder` is
/// devkit's.
pub const UDAS: [(&str, &str); 6] = [
    ("uda.subof.type", "uuid"),
    ("uda.subof.label", "Sub of"),
    ("uda.order.type", "numeric"),
    ("uda.order.label", "Order"),
    ("uda.holder.type", "string"),
    ("uda.holder.label", "Holder"),
];

/// The project a global todo is filed under. A task with no project is never
/// a todo, so a person's own unfiled tasks stay out of every list.
pub const GLOBAL_PROJECT: &str = devkit_todo::node::GLOBAL;

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
    /// The todo this task is, or `None` for a task that is not one: no
    /// project, or a status other than pending, completed or deleted.
    ///
    /// A pending task started with no `holder` was started outside devkit,
    /// by `task start` or alacritree, so it reads as held by a person and no
    /// agent takes it over.
    pub fn into_todo(self) -> Option<Todo> {
        let project = self.project?;
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
            project: (project != GLOBAL_PROJECT).then_some(project),
            entry: self.entry.as_deref().and_then(rfc3339),
            modified: self.modified.as_deref().and_then(rfc3339),
        })
    }
}

/// The words after `<uuid>` that store `status`. `started` keeps an existing
/// start time, so a claim handed down to a sub-agent keeps its start.
pub fn status_args(status: &Status, started: bool) -> Vec<String> {
    let holder = |by: &devkit_todo::Holder| format!("holder:{by}");
    match status {
        Status::Pending => ["modify", "status:pending", "start:", "end:", "holder:"]
            .map(String::from)
            .to_vec(),
        Status::InProgress { by } => ["modify", "status:pending"]
            .map(String::from)
            .into_iter()
            .chain((!started).then(|| "start:now".to_string()))
            .chain([holder(by)])
            .collect(),
        Status::Completed { by } => std::iter::once("done".to_string())
            .chain(by.as_ref().map(holder))
            .collect(),
        Status::Cancelled { by } => std::iter::once("delete".to_string())
            .chain(by.as_ref().map(holder))
            .collect(),
    }
}

/// The `project:` word for a todo's node, `None` being the global list.
pub fn project_arg(project: Option<&str>) -> String {
    format!("project:{}", project.unwrap_or(GLOBAL_PROJECT))
}

/// A description as a `task` argument. Taskwarrior reads a backslash as an
/// escape and drops it, even after `--`, so each one is doubled.
pub fn escaped(description: &str) -> String {
    description.replace('\\', r"\\")
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
            "project": "r.main",
        });
        task.as_object_mut()
            .unwrap()
            .extend(json.as_object().unwrap().clone());
        serde_json::from_value(task).unwrap()
    }

    fn status_of(json: serde_json::Value) -> Option<Status> {
        exported(json).into_todo().map(|t| t.status)
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
    fn global_project_reads_as_none_and_no_project_is_not_a_todo() {
        let global = exported(serde_json::json!({"project": "global"}));
        assert_eq!(global.into_todo().unwrap().project, None);
        let mut unfiled = exported(serde_json::json!({}));
        unfiled.project = None;
        assert_eq!(unfiled.into_todo(), None);
    }

    #[test]
    fn fractional_order_rounds() {
        let task = exported(serde_json::json!({"order": 1024.4}));
        assert_eq!(task.into_todo().unwrap().order, Some(1024));
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
            task.into_todo().unwrap().entry.as_deref(),
            Some("2026-10-03T12:00:00Z")
        );
    }

    #[test]
    fn status_args_cover_every_status() {
        let s = || Some(Holder::new("S"));
        assert_eq!(status_args(&Status::Pending, true), [
            "modify",
            "status:pending",
            "start:",
            "end:",
            "holder:"
        ]);
        let claimed = Status::InProgress {
            by: Holder::new("S"),
        };
        assert_eq!(status_args(&claimed, false), [
            "modify",
            "status:pending",
            "start:now",
            "holder:S"
        ]);
        assert_eq!(status_args(&claimed, true), [
            "modify",
            "status:pending",
            "holder:S"
        ]);
        assert_eq!(status_args(&Status::Completed { by: s() }, false), [
            "done", "holder:S"
        ]);
        assert_eq!(status_args(&Status::Cancelled { by: s() }, false), [
            "delete", "holder:S"
        ]);
        assert_eq!(status_args(&Status::Completed { by: None }, false), [
            "done"
        ]);
        assert_eq!(status_args(&Status::Cancelled { by: None }, false), [
            "delete"
        ]);
    }

    #[test]
    fn project_words_name_the_node() {
        assert_eq!(project_arg(None), "project:global");
        assert_eq!(project_arg(Some("r.main")), "project:r.main");
    }

    #[test]
    fn backslashes_double() {
        assert_eq!(escaped(r"a\b"), r"a\\b");
        assert_eq!(escaped("plain"), "plain");
    }
}
