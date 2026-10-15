//! A GitHub repository's checkout synced into the todo store against a `gh`
//! that answers from canned GitHub responses, for the ticket sync and push
//! tests. A test file declares `ghfake` and `todoenv` beside this module.
#![allow(dead_code)]

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use devkit_todo::Todo;

use super::{ghfake, todoenv::Proj};

pub const STATUSES: &str = r#"
[ticket.sync.statuses]
"" = "pending"
"Todo" = "pending"
"In progress" = "in_progress"
"Done" = "completed"
"#;

pub const RULES: &str = "\n[[ticket.sync.rules]]\nassignee = \"me\"\n";

/// A checkout `proj` whose issues live in GitHub repository `o/r`, with
/// project 3 holding their status when `project`, and a fake `gh`.
pub struct Synced {
    pub proj: Proj,
    pub bin: tempfile::TempDir,
}

impl Synced {
    pub fn new(sync: &str, project: bool) -> Self {
        Self::in_proj(Proj::new(), sync, project)
    }

    pub fn in_proj(proj: Proj, sync: &str, project: bool) -> Self {
        let project = if project { "project = 3\n" } else { "" };
        std::fs::write(
            proj.path.join("devkit.toml"),
            format!(
                "[tracker]\nkind = \"github\"\n\n[forge]\nkind = \"github\"\nrepo = \"o/r\"\n\n\
                 [github]\nissues_repo = \"o/r\"\n{project}{sync}"
            ),
        )
        .unwrap();
        let bin = tempfile::tempdir().unwrap();
        ghfake::install_fake_gh(bin.path());
        Self { proj, bin }
    }

    pub fn serve(&self, file: &str, body: &str) {
        std::fs::write(self.bin.path().join(file), body).unwrap();
    }

    pub fn devkit(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        self.devkit_fed(args, env, "")
    }

    pub fn devkit_fed(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> std::process::Output {
        self.devkit_in(&self.proj.path, args, env, stdin)
    }

    pub fn devkit_in(
        &self,
        dir: &std::path::Path,
        args: &[&str],
        env: &[(&str, &str)],
        stdin: &str,
    ) -> std::process::Output {
        self.with_gh(env, |env| self.proj.devkit_in(dir, args, env, stdin))
    }

    /// `devkit` run in the checkout through `script`, so its stdin is a
    /// terminal, fed `input`. Its stderr arrives in the output's stdout.
    #[cfg(target_os = "linux")]
    pub fn devkit_on_a_terminal(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        input: &str,
    ) -> std::process::Output {
        let line = std::iter::once(env!("CARGO_BIN_EXE_devkit"))
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        self.with_gh(env, |env| {
            let script = ["-qec", line.as_str(), "/dev/null"];
            self.proj
                .spawn(&self.proj.path, "script", &script, env, input)
        })
    }

    /// `f` given `env` and the variables that point `gh` at the fake.
    fn with_gh<R>(&self, env: &[(&str, &str)], f: impl FnOnce(&[(&str, &str)]) -> R) -> R {
        let path = std::env::join_paths(std::iter::once(self.bin.path().to_path_buf()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
        ))
        .unwrap();
        let path = path.to_str().unwrap();
        let ghfake: PathBuf = self.bin.path().to_path_buf();
        let mut all = vec![
            ("PATH", path),
            ("GHFAKE_DIR", ghfake.to_str().unwrap()),
            ("GH_TOKEN", ""),
            ("GITHUB_TOKEN", ""),
        ];
        all.extend_from_slice(env);
        f(&all)
    }

    pub fn sync(&self, args: &[&str]) -> std::process::Output {
        let mut all = vec!["todo", "sync"];
        all.extend_from_slice(args);
        self.devkit(&all, &[])
    }

    pub fn calls(&self) -> String {
        std::fs::read_to_string(self.bin.path().join("gh.log")).unwrap_or_default()
    }

    /// The tracker writes `gh` was asked for.
    pub fn mutations(&self) -> Vec<String> {
        self.calls()
            .lines()
            .filter(|l| l.contains("query=mutation"))
            .map(String::from)
            .collect()
    }

    /// How many rule searches `gh` was asked for: one per ticket sync here.
    pub fn ticket_queries(&self) -> usize {
        self.calls()
            .lines()
            .filter(|l| l.contains("search(query"))
            .count()
    }

    /// The ticket sync's state file for this checkout's repository with
    /// extension `ext`: `last`, `failed` or `lock`.
    pub fn sync_state(&self, ext: &str) -> PathBuf {
        self.proj
            .state()
            .join("ticket-sync")
            .join(format!("proj.{ext}"))
    }

    pub fn todo_for(&self, key: &str) -> Todo {
        self.proj
            .todos()
            .into_iter()
            .find(|t| t.ticket.as_ref().is_some_and(|r| r.key == key))
            .unwrap_or_else(|| panic!("no todo for ticket {key}: {:?}", self.proj.todos()))
    }
}

/// An open issue as the sync query reads it, in project 3 at `option` when
/// one is given.
pub fn issue(number: u64, option: Option<&str>, blocked_by: &[u64]) -> String {
    let items = match option {
        Some(name) => {
            format!(r#"[{{"project":{{"id":"PVT_3"}},"fieldValueByName":{{"name":"{name}"}}}}]"#)
        }
        None => "[]".into(),
    };
    let blockers: Vec<String> = blocked_by
        .iter()
        .map(|n| format!(r#"{{"number":{n},"repository":{{"nameWithOwner":"o/r"}}}}"#))
        .collect();
    format!(
        r#"{{"number":{number},"title":"Issue {number}","state":"OPEN","stateReason":null,
            "parent":null,"blockedBy":{{"nodes":[{}]}},"projectItems":{{"nodes":{items}}}}}"#,
        blockers.join(",")
    )
}

/// One answer for every query a sync sends: the search, the issues by
/// number, and project 3's status field with `options`.
pub fn graphql(issues: &[String], options: &[&str]) -> String {
    let options: Vec<String> = options
        .iter()
        .enumerate()
        .map(|(i, n)| format!(r#"{{"id":"o{i}","name":"{n}"}}"#))
        .collect();
    let by_number: Vec<String> = issues
        .iter()
        .map(|i| {
            let v: serde_json::Value = serde_json::from_str(i).unwrap();
            format!(r#""i{}":{i}"#, v["number"])
        })
        .collect();
    format!(
        r#"{{"data":{{
            "search":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{}]}},
            "repository":{{{},"issue":{{"id":"I_65","projectItems":{{"nodes":[
                {{"id":"PVTI_3","project":{{"id":"PVT_3","number":3}},
                  "fieldValueByName":{{"name":"Todo","optionId":"o0"}}}}]}}}}}},
            "repositoryOwner":{{"projectV2":{{"id":"PVT_3",
                "field":{{"id":"PVTSSF_status","options":[{}]}}}}}}
        }}}}"#,
        issues.join(","),
        by_number.join(","),
        options.join(",")
    )
}

pub const OPTIONS: [&str; 3] = ["Todo", "In progress", "Done"];

/// Issue 65, at `Todo` and blocked by 66, and issue 66, outside the project.
pub fn two_issues() -> Synced {
    two_issues_in(Proj::new())
}

/// [`two_issues`] in `proj`.
pub fn two_issues_in(proj: Proj) -> Synced {
    let s = Synced::in_proj(proj, &format!("{RULES}{STATUSES}"), true);
    s.serve(
        "graphql.json",
        &graphql(
            &[issue(65, Some("Todo"), &[66]), issue(66, None, &[])],
            &OPTIONS,
        ),
    );
    s
}

/// Polls `done` until it holds, failing with `what` after 30 seconds.
pub fn poll(what: &str, done: impl Fn() -> bool) {
    poll_or(what, done, String::new);
}

/// [`poll`], adding `detail` to the failure.
pub fn poll_or(what: &str, done: impl Fn() -> bool, detail: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}{}",
            detail()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits long enough for a background sync a call may have started to reach
/// `gh`, for a test that asserts none did.
pub fn settle() {
    std::thread::sleep(Duration::from_secs(1));
}
