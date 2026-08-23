//! `issue.status` over the real JSON-RPC surface, against a project whose
//! `devkit.toml` names a tracker.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(args: &[&str], cwd: &Path) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git {args:?} failed");
}

/// A one-commit repo with no remote, whose `devkit.toml` forces `kind`.
fn fixture(kind: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("devkit-mcp-tracker-{kind}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&["init", "-q", "-b", "main"], &dir);
    git(&["config", "user.email", "t@t"], &dir);
    git(&["config", "user.name", "t"], &dir);
    std::fs::write(
        dir.join("devkit.toml"),
        format!(
            "[defaults]\n\
             worktree_root = \"wts\"\n\
             branch_prefix = \"lev/\"\n\
             baseline_ref = \"origin/main\"\n\
             baseline_path = \"baseline\"\n\
             \n\
             [tracker]\n\
             kind = \"{kind}\"\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("f"), "x").unwrap();
    git(&["add", "."], &dir);
    git(&["commit", "-qm", "init"], &dir);
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
        let dir = fixture(kind);
        // No worktree matches the filter, so nothing is fetched over the network.
        let report = call(
            "issue.status",
            json!({ "root": dir.to_str().unwrap(), "ids": ["NOPE-1"] }),
        );
        assert!(report["worktrees"].as_array().unwrap().is_empty());
        assert_eq!(report["tracker"]["kind"], kind, "in {}", dir.display());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
