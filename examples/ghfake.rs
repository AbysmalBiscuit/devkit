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
    if args.starts_with("pr view") && args.contains("reviewRequests") {
        return Some(("review_requests.json", r#"{"reviewRequests":[]}"#));
    }
    if args.starts_with("pr view") && args.contains("reviews") {
        return Some(("reviews.json", r#"{"reviews":[]}"#));
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
    if joined.starts_with("pr view") {
        match std::fs::read_to_string(dir.join("pr_view.json")) {
            Ok(pr) => print!("{pr}"),
            // What `gh` says for a PR that does not exist.
            Err(_) => {
                eprintln!("no pull requests found");
                std::process::exit(1);
            }
        }
        return;
    }
    if succeeds_silently(&joined) {
        return;
    }
    std::process::exit(1);
}
