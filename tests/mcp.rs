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

/// A real (empty) git repo so `git worktree list` resolves with only the main
/// worktree — `issue.status` then returns empty without needing `gh`.
fn git_repo() -> PathBuf {
    let p = scratch("repo");
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&p)
        .status()
        .expect("spawn git init")
        .success();
    assert!(ok, "git init failed");
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

#[test]
fn devrun_status_lists_tracked_servers_for_root() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            // Reserve a port so there is something to report.
            call_req(1, "ports.alloc", json!({ "root": root, "apps": ["web"] })),
            call_req(2, "devrun.status", json!({ "root": root })),
            call_req(3, "devrun.status", json!({ "all": true })),
        ],
    );
    tool_json(&resps[0], false);

    let rows = tool_json(&resps[1], false);
    let arr = rows.as_array().expect("status returns an array");
    assert_eq!(arr.len(), 1, "one tracked server for this root");
    assert_eq!(arr[0]["app"], "web");
    // Nothing is listening, no pid → crashed.
    assert_eq!(arr[0]["state"], "crashed");

    let all = tool_json(&resps[2], false);
    assert!(!all.as_array().unwrap().is_empty(), "all view is non-empty");
}

#[test]
fn devrun_status_without_root_or_all_is_an_error() {
    let proj = project();
    let state = scratch("state");
    let resps = mcp(&proj, &state, &[call_req(1, "devrun.status", json!({}))]);
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("root"));
}

#[test]
fn devrun_logs_unknown_app_is_an_error() {
    let proj = project();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(
            1,
            "devrun.logs",
            json!({ "root": root, "app": "ghost" }),
        )],
    );
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("ghost"));
}

#[test]
fn devrun_down_releases_reserved_ports() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            call_req(1, "ports.alloc", json!({ "root": root, "apps": ["web"] })),
            call_req(2, "devrun.down", json!({ "root": root })),
            call_req(3, "devrun.status", json!({ "root": root })),
        ],
    );
    tool_json(&resps[0], false);

    let down = tool_json(&resps[1], false);
    assert_eq!(down["freed"].as_array().unwrap().len(), 1);
    assert_eq!(down["stopped"], 0, "no pid was recorded");

    let rows = tool_json(&resps[2], false);
    assert!(
        rows.as_array().unwrap().is_empty(),
        "nothing tracked after down"
    );
}

#[test]
fn devrun_up_unknown_app_is_an_error() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(
            1,
            "devrun.up",
            json!({ "root": root, "apps": ["ghost"] }),
        )],
    );
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("ghost"));
}

#[test]
fn devrun_up_requires_at_least_one_app() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(
            1,
            "devrun.up",
            json!({ "root": root, "apps": [] }),
        )],
    );
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("at least one app"));
}

#[test]
fn issue_actions_are_described() {
    let proj = project();
    let state = scratch("state");
    let resps = mcp(
        &proj,
        &state,
        &[json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "devkit_describe", "arguments": {} }
        })],
    );
    let text = resps[0]["result"]["content"][0]["text"].as_str().unwrap();
    let list: Value = serde_json::from_str(text).unwrap();
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"issue.status"), "issue.status is described");
    assert!(names.contains(&"issue.prs"), "issue.prs is described");
}

#[test]
fn issue_status_empty_for_repo_with_no_worktrees() {
    let proj = git_repo();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[call_req(1, "issue.status", json!({ "root": root }))],
    );
    let report = tool_json(&resps[0], false);
    assert!(
        report["worktrees"].as_array().unwrap().is_empty(),
        "no non-main worktrees → empty list"
    );
    assert_eq!(report["finished_count"], 0);
}

/// The MCP lifecycle a host drives on connect: `initialize` →
/// `notifications/initialized` → `tools/list`. The notification carries no `id`
/// and must draw no response; `initialize` must echo the protocol version and
/// server identity; `tools/list` must expose exactly the two meta tools.
#[test]
fn handshake_lifecycle_initialize_notification_tools_list() {
    let proj = project();
    let state = scratch("state");
    let resps = mcp(
        &proj,
        &state,
        &[
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "test-client", "version": "0" }
                }
            }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        ],
    );

    // Three requests, but the notification (no `id`) draws no response.
    assert_eq!(resps.len(), 2, "notification must not produce a response");

    let init = &resps[0];
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init["result"]["serverInfo"]["name"], "devkit-mcp");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "advertises the tools capability"
    );

    let list = &resps[1];
    assert_eq!(list["id"], 2);
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(names, ["devkit_describe", "devkit_call"]);
}
