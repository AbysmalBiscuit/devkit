//! `issue.status` over the real JSON-RPC surface, against a project whose
//! `devkit.toml` names a tracker.

use serde_json::{Value, json};
use std::path::Path;

fn git(args: &[&str], cwd: &Path) {
    devkit_common::git::Git::fixture(cwd)
        .args(args.iter().copied())
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
}

/// A one-commit repo with no remote, whose `devkit.toml` forces `kind` — or
/// omits the `[tracker]` table entirely when `kind` is `None`, leaving the
/// choice to detection.
fn fixture(kind: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(&["init", "-q", "-b", "main"], dir.path());
    let table = match kind {
        Some(k) => format!("\n[tracker]\nkind = \"{k}\"\n"),
        None => String::new(),
    };
    std::fs::write(
        dir.path().join("devkit.toml"),
        format!(
            "[defaults]\n\
             worktree_root = \"wts\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n\
             {table}"
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("f"), "x").unwrap();
    git(&["add", "."], dir.path());
    git(&["commit", "-qm", "init"], dir.path());
    dir
}

/// One `tools/call` round trip, returning the action's decoded payload.
fn call(action: &str, args: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "devkit_call", "arguments": { "action": action, "args": args } }
    });
    let ctx = devkit_mcp::ServerCtx {
        default_holder: "test-session".to_string(),
        own_worktree: None,
    };
    let mut out = Vec::new();
    devkit_mcp::run(&mut format!("{req}\n").as_bytes(), &mut out, &ctx).unwrap();
    let resp: Value = serde_json::from_str(String::from_utf8(out).unwrap().trim()).unwrap();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(resp["result"]["isError"], false, "action failed: {text}");
    serde_json::from_str(text).unwrap()
}

/// Two repos alike but for the kind their config names, and the report follows
/// the config both times. Detection cannot tell the two apart — same shape, same
/// environment — so only the config can account for the difference.
#[test]
fn the_status_action_reports_the_configured_tracker_kind() {
    // A `DEVKIT_CONFIG` in the environment is the sole config layer, which would
    // hide the fixture's own file. This test binary holds one test, so clearing
    // it races no other thread.
    unsafe { std::env::remove_var("DEVKIT_CONFIG") };

    for kind in ["linear", "none"] {
        let dir = fixture(Some(kind));
        // No worktree matches the filter, so nothing is fetched over the network.
        let report = call(
            "issue.status",
            json!({ "root": dir.path().to_str().unwrap(), "ids": ["NOPE-1"] }),
        );
        assert!(report["worktrees"].as_array().unwrap().is_empty());
        assert_eq!(
            report["tracker"]["kind"],
            kind,
            "in {}",
            dir.path().display()
        );
        assert_eq!(
            report["tracker"]["declared"],
            true,
            "a config named this tracker, in {}",
            dir.path().display()
        );
    }

    // No `[tracker]` table: whichever kind detection lands on, nobody declared
    // it, and the report has to say so — `declared` is what keeps `issue end`
    // from reading a detected `none` as "no issue state to wait for". The
    // assertion holds whether or not this machine has a LINEAR_API_KEY.
    let dir = fixture(None);
    let report = call(
        "issue.status",
        json!({ "root": dir.path().to_str().unwrap(), "ids": ["NOPE-1"] }),
    );
    assert_eq!(
        report["tracker"]["declared"],
        false,
        "detection produced this tracker, in {}",
        dir.path().display()
    );
}
