use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Value, json};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("devkit-mcp-{}-{}-{n}", std::process::id(), tag));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A project dir with a `.git` marker so `find_root_from`/normalization resolve.
fn project() -> PathBuf {
    let p = scratch("proj");
    std::fs::create_dir_all(p.join(".git")).unwrap();
    p
}

const MINIMAL_CONFIG: &str = r#"[defaults]
worktree_root = "."
branch_prefix = "test/"
baseline_ref = "origin/main"
baseline_path = "."
doppler_config = "dev_local"
doppler_yaml = "nonexistent.yaml"

[apps.web]
base_port = 3000
launch = []
path = "apps/web"
"#;

/// A project dir that also carries a minimal devkit.toml with one app `web`.
fn project_with_config() -> PathBuf {
    let p = project();
    std::fs::write(p.join("devkit.toml"), MINIMAL_CONFIG).unwrap();
    p
}

/// Spawn the server, feed the requests as NDJSON, return parsed responses in order.
fn mcp(project: &Path, state: &Path, requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_devkit-mcp"))
        .current_dir(project)
        .env("XDG_STATE_HOME", state)
        .env("HOME", state)
        .env_remove("DEVKIT_SESSION")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn devkit-mcp");
    {
        let mut stdin = child.stdin.take().unwrap();
        for r in requests {
            writeln!(stdin, "{}", serde_json::to_string(r).unwrap()).unwrap();
        }
    } // drop stdin -> EOF
    let out = child.wait_with_output().expect("wait devkit-mcp");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call_req(id: u32, action: &str, args: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "devkit_call", "arguments": { "action": action, "args": args } }
    })
}

/// Parse the JSON payload out of a tools/call result, asserting isError == expected.
fn tool_json(resp: &Value, expect_error: bool) -> Value {
    assert_eq!(resp["result"]["isError"], expect_error, "isError on {resp}");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

#[test]
fn ports_alloc_status_release_roundtrip() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            call_req(1, "ports.alloc", json!({ "root": root, "apps": ["web"] })),
            call_req(2, "ports.status", json!({})),
            call_req(3, "ports.release", json!({ "root": root })),
        ],
    );
    let alloc = tool_json(&resps[0], false);
    let port = alloc["web"].as_u64().expect("web port");
    assert!(port >= 3000, "allocated port {port} >= base 3000");

    let status = tool_json(&resps[1], false);
    let entries = status["entries"].as_object().expect("entries map");
    assert!(
        entries.contains_key(&port.to_string()),
        "status shows the allocated port"
    );

    let released = tool_json(&resps[2], false);
    assert_eq!(released["freed"].as_array().unwrap().len(), 1);
}

#[test]
fn ports_alloc_unknown_app_is_an_error() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(
            1,
            "ports.alloc",
            json!({ "root": root, "apps": ["ghost"] }),
        )],
    );
    let payload = tool_json(&resps[0], true);
    assert!(
        payload.as_str().unwrap().contains("ghost"),
        "error names the app"
    );
}

#[test]
fn locks_acquire_then_other_holder_sees_conflict() {
    let proj = project();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            call_req(
                1,
                "locks.acquire",
                json!({ "root": root, "paths": ["src/a.rs"], "holder": "alice" }),
            ),
            call_req(
                2,
                "locks.check",
                json!({ "root": root, "paths": ["src/a.rs"], "holder": "bob" }),
            ),
        ],
    );
    let acq = tool_json(&resps[0], false);
    assert_eq!(acq["acquired"].as_array().unwrap().len(), 1);
    assert!(acq["conflicts"].as_array().unwrap().is_empty());

    let conflicts = tool_json(&resps[1], false);
    let arr = conflicts.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["held_by"], "alice");
}

#[test]
fn locks_acquire_then_release_clears_the_lock() {
    let proj = project();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            call_req(
                1,
                "locks.acquire",
                json!({ "root": root, "paths": ["x.txt"], "holder": "alice" }),
            ),
            call_req(
                2,
                "locks.release",
                json!({ "root": root, "paths": ["x.txt"], "holder": "alice" }),
            ),
            call_req(3, "locks.status", json!({ "root": root })),
        ],
    );
    tool_json(&resps[0], false);
    let released = tool_json(&resps[1], false);
    assert_eq!(released["released"].as_array().unwrap().len(), 1);
    let status = tool_json(&resps[2], false);
    assert!(status.as_array().unwrap().is_empty(), "no locks remain");
}

#[test]
fn locks_release_without_paths_or_all_is_an_error() {
    let proj = project();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(
            1,
            "locks.release",
            json!({ "root": root, "holder": "alice" }),
        )],
    );
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("paths"));
}

#[test]
fn locks_acquire_on_held_path_returns_conflicts_not_error() {
    let proj = project();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            call_req(
                1,
                "locks.acquire",
                json!({ "root": root, "paths": ["shared.rs"], "holder": "alice" }),
            ),
            call_req(
                2,
                "locks.acquire",
                json!({ "root": root, "paths": ["shared.rs"], "holder": "bob" }),
            ),
        ],
    );
    tool_json(&resps[0], false);
    // A conflict is data the agent branches on, not an error: isError stays false.
    let outcome = tool_json(&resps[1], false);
    assert!(
        outcome["acquired"].as_array().unwrap().is_empty(),
        "all-or-nothing: nothing acquired when a path conflicts"
    );
    let conflicts = outcome["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["held_by"], "alice");
}
