//! `issue.status` over the real JSON-RPC surface, against a project whose
//! `devkit.toml` names a tracker.

use std::path::Path;

use devkit_locks::ident::Identity;
use serde_json::{Value, json};

fn git(args: &[&str], cwd: &Path) {
    devkit_git::Git::fixture(cwd)
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
        default_holder: Identity::Resolved("test-session".into()),
        own_worktree: None,
        enabled: true,
    };
    let mut out = Vec::new();
    devkit_mcp::run(&mut format!("{req}\n").as_bytes(), &mut out, &ctx).unwrap();
    let resp: Value = serde_json::from_str(String::from_utf8(out).unwrap().trim()).unwrap();
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(resp["result"]["isError"], false, "action failed: {text}");
    serde_json::from_str(text).unwrap()
}

/// A `DEVKIT_CONFIG` in the environment is the sole config layer, which would
/// hide each fixture's own file. Every test calls this before touching
/// anything else, and `Once` holds the others until the removal is done, so
/// no thread reads the environment while it changes.
fn clear_devkit_config() {
    static CLEAR: std::sync::Once = std::sync::Once::new();
    CLEAR.call_once(|| unsafe { std::env::remove_var("DEVKIT_CONFIG") });
}

/// A `doppler.yaml` devkit cannot parse says nothing about `devkit.toml`, so
/// the tracker that file declares still decides the report. Doppler's
/// single-project form writes `setup` as a mapping where the app catalog
/// expects a list.
#[test]
fn an_unparseable_doppler_yaml_keeps_the_declared_tracker() {
    clear_devkit_config();

    let dir = fixture(None);
    std::fs::write(
        dir.path().join("devkit.toml"),
        "[config]\n\
         root = true\n\
         [defaults]\n\
         worktree_root = \"wts\"\n\
         branch_prefix = \"lev/\"\n\
         baseline_ref = \"origin/main\"\n\
         doppler_yaml = \"doppler.yaml\"\n\
         \n\
         [tracker]\n\
         kind = \"none\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("doppler.yaml"),
        "setup:\n  project: api\n  config: dev\n  path: apps/api\n",
    )
    .unwrap();

    let report = call(
        "issue.status",
        json!({ "root": dir.path().to_str().unwrap(), "ids": ["NOPE-1"] }),
    );

    assert_eq!(report["tracker"]["kind"], "none");
    assert_eq!(report["tracker"]["declared"], true);
}

/// Two repos alike but for the kind their config names, and the report follows
/// the config both times. Detection cannot tell the two apart — same shape,
/// same environment — so only the config can account for the difference.
#[test]
fn the_status_action_reports_the_configured_tracker_kind() {
    clear_devkit_config();

    for kind in ["linear", "none"] {
        let dir = fixture(Some(kind));
        // No worktree matches the filter, so nothing is fetched over the
        // network.
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
    // it, and the report has to say so — `declared` is what keeps `workspace
    // end` from reading a detected `none` as "no issue state to wait for".
    // The assertion holds whether or not this machine has a LINEAR_API_KEY.
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
