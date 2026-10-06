//! A `gh` stand-in for the integration tests, copied into place as `gh` (or
//! `gh.exe`) on a test's `PATH`.
//!
//! A binary rather than a shell script because `Command::new("gh")` resolves
//! `.exe` and nothing else on Windows, so a `.cmd` or `.bat` on `PATH` is never
//! found and every test driving `gh` would be Unix-only.
//!
//! `GHFAKE_DIR` names a directory of canned answers. Each verb reads one file
//! from it, falling back to an empty answer when the file is absent, and every
//! argument vector is appended to `gh.log` there.
//!
//! A `refuse_graphql` file there makes every GraphQL-backed verb fail with
//! HTTP 403, as the Claude Code cloud proxy does, while `gh api` REST calls
//! still answer. A `create_error.txt` there makes `gh pr create` and the REST
//! create fail with its contents on stderr.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

/// The file a verb answers from, and what to say when the test did not write
/// one. `None` means the verb answers nothing and only reports an exit status.
fn canned(args: &str) -> Option<(&'static str, &'static str)> {
    if args.starts_with("pr list") {
        return Some(("pr_list.json", "[]"));
    }
    if args.starts_with("issue create") {
        return Some(("issue_create.txt", "https://github.com/o/r/issues/42\n"));
    }
    if args.starts_with("pr create") {
        return Some(("pr_create.txt", ""));
    }
    None
}

/// Verbs that succeed silently. `auth token` is deliberately absent: it must
/// fail so a run resolves no bearer token and takes its `gh` fallback.
fn succeeds_silently(args: &str) -> bool {
    args.starts_with("pr ready") || args.starts_with("pr edit") || args.starts_with("pr checkout")
}

fn read_or(dir: &Path, file: &str, fallback: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_else(|_| fallback.to_string())
}

fn uses_graphql(args: &str) -> bool {
    args.starts_with("api graphql") || args.starts_with("pr ")
}

/// Exit 1 with `create_error.txt` on stderr, when the test wrote one.
fn fail_create(dir: &Path) {
    if let Ok(stderr) = std::fs::read_to_string(dir.join("create_error.txt")) {
        eprintln!("{stderr}");
        std::process::exit(1);
    }
}

/// Print `file`, or fail the way `gh api` reports a 404.
fn serve_or_404(dir: &Path, file: &str) {
    match std::fs::read_to_string(dir.join(file)) {
        Ok(body) => print!("{body}"),
        Err(_) => {
            print!(r#"{{"message":"Not Found","status":"404"}}"#);
            eprintln!("gh: Not Found (HTTP 404)");
            std::process::exit(1);
        }
    }
}

/// Answer `gh api [--hostname H] [--method M] PATH [-f k=v]...` from the
/// REST fixtures.
fn rest(dir: &Path, args: &[String]) {
    let method = args
        .iter()
        .position(|a| a == "--method" || a == "-X")
        .and_then(|i| args.get(i + 1))
        .map_or("GET", String::as_str);
    let Some(path) = args
        .iter()
        .find(|a| a.starts_with("repos/") || a.as_str() == "user")
    else {
        std::process::exit(1);
    };
    let path = path.split('?').next().unwrap_or_default();
    let segments: Vec<&str> = path.split('/').collect();
    match (method, segments.as_slice()) {
        ("GET", ["user"]) => print!(r#"{{"login":"LevValle"}}"#),
        ("GET", ["repos", _, _, "pulls"]) => print!("{}", read_or(dir, "rest_pulls.json", "[]")),
        ("POST", ["repos", _, _, "pulls"]) => {
            fail_create(dir);
            serve_or_404(dir, "rest_pull_create.json")
        }
        ("GET", ["repos", _, _, "pulls", n]) => serve_or_404(dir, &format!("rest_pull_{n}.json")),
        ("GET", ["repos", _, _, "pulls", _, "requested_reviewers"]) => print!(
            "{}",
            read_or(
                dir,
                "rest_requested_reviewers.json",
                r#"{"users":[],"teams":[]}"#
            )
        ),
        ("POST", ["repos", _, _, "pulls", _, "requested_reviewers"]) => print!("{{}}"),
        ("GET", ["repos", _, _, "pulls", _, "reviews"]) => {
            print!("{}", read_or(dir, "rest_reviews.json", "[]"))
        }
        _ => std::process::exit(1),
    }
}

fn main() {
    let dir = PathBuf::from(std::env::var("GHFAKE_DIR").expect("GHFAKE_DIR"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let joined = args.join(" ");

    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("gh.log"))
    {
        let _ = writeln!(log, "{joined}");
    }

    // `gh.log` joins with spaces and newlines, which a multi-line body also
    // contains, so the argument vector `pr create` was handed is kept whole.
    if joined.starts_with("pr create") {
        let _ = std::fs::write(dir.join("pr_create.args"), args.join("\0"));
    }

    if uses_graphql(&joined) && dir.join("refuse_graphql").exists() {
        eprintln!(
            "HTTP 403: GitHub GraphQL is not available from Claude Code sessions; use the \
             REST API (https://api.github.com/graphql)"
        );
        std::process::exit(1);
    }
    if joined.starts_with("pr create") {
        fail_create(&dir);
    }
    if let Some((file, fallback)) = canned(&joined) {
        print!("{}", read_or(&dir, file, fallback));
        return;
    }
    if joined.starts_with("api graphql") {
        let file = if joined.contains("query=mutation") {
            "graphql_mutation.json"
        } else {
            "graphql.json"
        };
        match std::fs::read_to_string(dir.join(file)) {
            Ok(answer) => print!("{answer}"),
            Err(_) => std::process::exit(1),
        }
        return;
    }
    if joined.starts_with("api ") {
        rest(&dir, &args);
        return;
    }
    if succeeds_silently(&joined) {
        return;
    }
    std::process::exit(1);
}
