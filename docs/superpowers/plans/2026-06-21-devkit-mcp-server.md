# devkit MCP server Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose devkit's port-allocation and file-lock facades to coding agents over a stdio MCP server using a meta-MCP tool shape (two tools backed by one action registry).

**Architecture:** A new `crates/devkit-mcp` library crate holds a hand-rolled, fully synchronous JSON-RPC 2.0 stdio loop, a single action registry (the source of truth for both `devkit_describe` and `devkit_call`), and per-binary handler modules that call the existing `devkit-ports`/`devkit-locks` facades. A thin `src/bin/devkit-mcp/main.rs` wires stdin/stdout to the loop. No tokio, no new async dependencies.

**Tech Stack:** Rust (edition 2024), `anyhow`, `serde`/`serde_json`, the existing `devkit-ports`/`devkit-locks`/`devkit-common` crates. Transport is newline-delimited JSON-RPC over stdio.

## Global Constraints

- **Exactly two MCP tools**: `devkit_describe` and `devkit_call`. Never one-tool-per-action. The action registry is the single source of truth that powers both.
- **Action naming**: `binary.action` (e.g. `ports.alloc`, `locks.acquire`). v1 surface is the 9 ports + locks actions only; `devrun`/`issue` are out of scope.
- **`root` is an explicit per-call argument** for every action that needs it (`locks.*` and `ports.alloc`); no CWD inference. Missing-where-required is a hard error.
- **`holder` is server-minted**: `$DEVKIT_SESSION` if set and non-empty, else `format!("mcp-{}", std::process::id())`. The agent may override per call.
- **Fully synchronous. No tokio, no async, no new runtime dependency.** Build the MCP crate with the `daemon` feature so the ports facades cooperate with a running `devkitd`.
- **`anyhow` everywhere** with `.context()` chains. Each binary installs `devkit_common::report::install_panic_hook`.
- **`Role` is mapped exhaustively** — no `_ => Issue` catch-alls.
- **Zero-warning policy**: `cargo clippy --workspace --all-targets -- -D warnings` must pass.
- **Merge gate**: `cargo test --workspace` must stay green. Format with `cargo fmt --all`.
- **Process-spawning tests wait for completion** (`wait_with_output`), never a fixed sleep.
- **Conventional Commits**; commit message footer is exactly `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` and no other trailers.

## Architecture decisions (resolved in the design + plan phase)

- **Ports cooperate with a live daemon for free.** `registry::{snapshot,alloc,release,prune}` already try `devkitd` under `#[cfg(feature="daemon")]` before the flock fallback and take explicit args. Handlers call them directly.
- **v1 locks use the direct-flock `*_with` store ops** (`store::{acquire_with,check_with,release_with,release_all_with,status_with,prune_with}` against `FlockStore::new()`), passing explicit `root` + `holder`. These bypass the daemon: under a live `devkitd`, lock *writes* hard-error with `DaemonHoldsLock` (reads are ungated). Full lock/daemon cooperation needs daemon-aware resolved-context facade variants, which are owned by the "Authoritative in-memory mode for the lock registry" work in `docs/next-steps.md` and are **deferred** here (see Task 5 + Unresolved questions).
- **Error mapping** (refines the spec): protocol-level failures (unparseable line, unknown JSON-RPC method) are JSON-RPC error responses; everything inside `tools/call` — unknown tool, unknown action, invalid args, facade errors, lock conflicts — is returned as a `tools/call` **result** with `isError` set appropriately. A lock conflict is `isError: false` (it is data the agent branches on); a facade/validation error is `isError: true`.

## File structure

```
crates/devkit-mcp/
  Cargo.toml                 # new member crate
  src/lib.rs                 # module decls, run(), dispatch(), mint_holder(), ServerCtx, MCP method handlers
  src/jsonrpc.rs             # JSON-RPC envelope types + NDJSON transport
  src/actions.rs             # Action struct, actions() registry, describe(), call()
  src/ports.rs               # 4 ports handlers + schemas
  src/locks.rs               # 5 locks handlers + schemas
src/bin/devkit-mcp/main.rs   # thin binary (root `devkit` package)
tests/mcp.rs                 # root integration tests: spawn the bin, drive the protocol
Cargo.toml                   # root: members, workspace deps, deps, daemon feature
.mcp.json                    # plugin MCP-server registration (Task 5)
README.md / AGENTS.md / docs/next-steps.md  # docs (Task 5)
```

---

### Task 1: Crate scaffold, workspace wiring, JSON-RPC envelope + transport, run loop

**Files:**
- Create: `crates/devkit-mcp/Cargo.toml`
- Create: `crates/devkit-mcp/src/jsonrpc.rs`
- Create: `crates/devkit-mcp/src/lib.rs`
- Create: `src/bin/devkit-mcp/main.rs`
- Modify: `Cargo.toml` (root) — `[workspace] members`, `[workspace.dependencies]`, `[dependencies]`, `[features] daemon`

**Interfaces:**
- Produces: `devkit_mcp::run(reader: &mut impl BufRead, writer: &mut impl Write) -> anyhow::Result<()>` — reads NDJSON JSON-RPC requests until EOF, writes one response per request (none for notifications). `jsonrpc::{Request, Response, RpcError, METHOD_NOT_FOUND, PARSE_ERROR, read_line_value, write_response}`.

- [ ] **Step 1: Create the crate manifest**

`crates/devkit-mcp/Cargo.toml`:

```toml
[package]
name = "devkit-mcp"
edition.workspace = true
version = "0.2.0" # x-release-please-version

[dependencies]
anyhow.workspace = true
serde = { workspace = true }
serde_json.workspace = true
devkit-common.workspace = true
devkit-ports.workspace = true
devkit-locks.workspace = true

[features]
daemon = ["devkit-ports/daemon", "devkit-locks/daemon"]
```

- [ ] **Step 2: Wire the workspace** — edit the root `Cargo.toml`:

In `[workspace] members`, add `"crates/devkit-mcp"`:
```toml
members = ["crates/devkit-common", "crates/devkit-ports", "crates/devkit-locks", "crates/devkit-mcp"]
```
In `[workspace.dependencies]`, add (next to the other path deps):
```toml
devkit-mcp = { path = "crates/devkit-mcp" }
```
In the root `[dependencies]`, add:
```toml
devkit-mcp.workspace = true
```
In `[features]`, extend `daemon` to propagate to the new crate:
```toml
daemon = ["devkit-ports/daemon", "devkit-locks/daemon", "devkit-mcp/daemon"]
```
Do **not** add a `[[bin]]` entry — the binary at `src/bin/devkit-mcp/main.rs` is auto-discovered (it is not feature-gated, unlike `devkitd`).

- [ ] **Step 3: Write the JSON-RPC transport with its unit tests**

`crates/devkit-mcp/src/jsonrpc.rs`:

```rust
use std::io::{BufRead, Write};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC 2.0 request or notification. A notification carries no `id`.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[allow(dead_code)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Response { jsonrpc: "2.0", id, result: Some(result), error: None }
    }
    pub fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError { code, message: message.into() }),
        }
    }
}

pub const METHOD_NOT_FOUND: i64 = -32601;
pub const PARSE_ERROR: i64 = -32700;

/// Read one line. Returns `Ok(None)` on clean EOF.
pub fn read_line_value(reader: &mut impl BufRead) -> Result<Option<String>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line))
}

/// Write one response as a single newline-delimited JSON line and flush.
pub fn write_response(writer: &mut impl Write, resp: &Response) -> Result<()> {
    let mut bytes = serde_json::to_vec(resp)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_response_omits_error_field() {
        let r = Response::ok(Value::from(1), serde_json::json!({"a": 1}));
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["a"], 1);
        assert!(v.get("error").is_none());
    }

    #[test]
    fn err_response_omits_result_field() {
        let r = Response::err(Value::from(2), METHOD_NOT_FOUND, "nope");
        let v: Value = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(v["error"]["message"], "nope");
        assert!(v.get("result").is_none());
    }
}
```

- [ ] **Step 4: Write the loop with its unit tests**

`crates/devkit-mcp/src/lib.rs`:

```rust
mod jsonrpc;

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::Value;

use jsonrpc::{Request, Response, METHOD_NOT_FOUND, PARSE_ERROR};

/// Run the stdio JSON-RPC loop until EOF.
pub fn run(reader: &mut impl BufRead, writer: &mut impl Write) -> Result<()> {
    while let Some(line) = jsonrpc::read_line_value(reader)? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                jsonrpc::write_response(
                    writer,
                    &Response::err(Value::Null, PARSE_ERROR, format!("parse error: {e}")),
                )?;
                continue;
            }
        };
        if let Some(resp) = dispatch(&req) {
            jsonrpc::write_response(writer, &resp)?;
        }
    }
    Ok(())
}

/// Returns `None` for notifications (no `id`) — they get no response.
fn dispatch(req: &Request) -> Option<Response> {
    let id = req.id.clone()?;
    Some(Response::err(id, METHOD_NOT_FOUND, format!("method not found: {}", req.method)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn drive(input: &str) -> Vec<Value> {
        let mut out = Vec::new();
        run(&mut input.as_bytes(), &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let resps = drive("{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"bogus\"}\n");
        assert_eq!(resps.len(), 1);
        assert_eq!(resps[0]["id"], 7);
        assert_eq!(resps[0]["error"]["code"], -32601);
    }

    #[test]
    fn notification_gets_no_response() {
        let resps = drive("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n");
        assert!(resps.is_empty());
    }

    #[test]
    fn unparseable_line_returns_parse_error() {
        let resps = drive("not json\n");
        assert_eq!(resps.len(), 1);
        assert_eq!(resps[0]["error"]["code"], -32700);
    }
}
```

- [ ] **Step 5: Write the thin binary**

`src/bin/devkit-mcp/main.rs`:

```rust
use std::io::{BufReader, Write};

use anyhow::Result;

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkit-mcp");
    devkit_common::paths::migrate_legacy_state();
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    devkit_mcp::run(&mut reader, &mut writer)?;
    writer.flush()?;
    Ok(())
}
```

- [ ] **Step 6: Build and test**

Run: `cargo build --workspace`
Expected: compiles, produces `target/debug/devkit-mcp`.
Run: `cargo test -p devkit-mcp`
Expected: the 5 unit tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-mcp/Cargo.toml crates/devkit-mcp/src/jsonrpc.rs crates/devkit-mcp/src/lib.rs src/bin/devkit-mcp/main.rs Cargo.toml Cargo.lock
git commit -m "feat(mcp): scaffold devkit-mcp crate with stdio json-rpc loop"
```

---

### Task 2: `initialize` + `tools/list`

**Files:**
- Modify: `crates/devkit-mcp/src/lib.rs` (extend `dispatch`, add `initialize_result`/`tools_list_result`)

**Interfaces:**
- Produces: JSON-RPC handling for `initialize` (returns `serverInfo`/`capabilities`), `notifications/initialized` (ignored), and `tools/list` (returns the two MCP tools and their input schemas).

- [ ] **Step 1: Write the failing tests** — append to the `tests` module in `crates/devkit-mcp/src/lib.rs`:

```rust
    #[test]
    fn initialize_returns_server_info() {
        let resps = drive(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
        );
        assert_eq!(resps[0]["result"]["serverInfo"]["name"], "devkit-mcp");
        assert!(resps[0]["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_returns_the_two_meta_tools() {
        let resps = drive("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n");
        let tools = resps[0]["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["devkit_describe", "devkit_call"]);
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
        assert_eq!(tools[1]["inputSchema"]["type"], "object");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p devkit-mcp initialize_returns_server_info tools_list_returns_the_two_meta_tools`
Expected: FAIL — `initialize`/`tools/list` currently return method-not-found.

- [ ] **Step 3: Extend `dispatch` and add the result builders** in `crates/devkit-mcp/src/lib.rs`:

Replace the `dispatch` fn with:

```rust
fn dispatch(req: &Request) -> Option<Response> {
    match req.method.as_str() {
        "initialize" => Some(Response::ok(req.id.clone()?, initialize_result())),
        "tools/list" => Some(Response::ok(req.id.clone()?, tools_list_result())),
        "notifications/initialized" => None,
        _ => Some(Response::err(
            req.id.clone()?,
            METHOD_NOT_FOUND,
            format!("method not found: {}", req.method),
        )),
    }
}

fn initialize_result() -> Value {
    serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "devkit-mcp", "version": env!("CARGO_PKG_VERSION") }
    })
}

fn tools_list_result() -> Value {
    serde_json::json!({
        "tools": [
            {
                "name": "devkit_describe",
                "description": "List devkit actions, or return the argument schema for one action. Call with no arguments to list every action; pass {\"action\": \"<name>\"} to get that action's argument schema.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "description": "Action name, e.g. \"locks.acquire\". Omit to list all actions." }
                    },
                    "additionalProperties": false
                }
            },
            {
                "name": "devkit_call",
                "description": "Invoke a devkit action. Call devkit_describe first to learn the action's argument schema.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "description": "Action name, e.g. \"locks.acquire\"." },
                        "args": { "type": "object", "description": "Arguments for the action, per its schema from devkit_describe." }
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }
            }
        ]
    })
}
```

- [ ] **Step 4: Run to verify all pass**

Run: `cargo test -p devkit-mcp`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-mcp/src/lib.rs
git commit -m "feat(mcp): handle initialize and tools/list"
```

---

### Task 3: Action registry, `devkit_describe`/`devkit_call`, and the 4 ports handlers

**Files:**
- Create: `crates/devkit-mcp/src/actions.rs`
- Create: `crates/devkit-mcp/src/ports.rs`
- Modify: `crates/devkit-mcp/src/lib.rs` (add `ServerCtx`, `mint_holder`, modules, `tools/call` dispatch, thread `ServerCtx` through `run`)
- Modify: `src/bin/devkit-mcp/main.rs` (mint holder, build `ServerCtx`)
- Create: `tests/mcp.rs` (root integration tests — ports)

**Interfaces:**
- Produces:
  - `devkit_mcp::ServerCtx { pub default_holder: String }` and `devkit_mcp::mint_holder() -> String`.
  - `devkit_mcp::run(reader, writer, ctx: &ServerCtx) -> Result<()>` (signature gains `ctx`).
  - `actions::Action { name: &'static str, summary: &'static str, schema: fn() -> Value, handler: fn(&ServerCtx, Value) -> Result<Value> }`, `actions::actions() -> Vec<Action>`, `actions::find(&str) -> Option<Action>`, `actions::describe(Value) -> Result<Value>`, `actions::call(&ServerCtx, Value) -> Result<Value>`.
  - `ports::actions() -> Vec<Action>` registering `ports.status`, `ports.alloc`, `ports.release`, `ports.prune`.
- Consumes: `devkit_ports::registry::{self, Role}`, `devkit_ports::load`.

- [ ] **Step 1: Add `ServerCtx`, `mint_holder`, modules, and `tools/call` to `lib.rs`**

In `crates/devkit-mcp/src/lib.rs`, add module declarations at the top (below `mod jsonrpc;`):

```rust
mod actions;
mod ports;
```

Add the context type and holder minting (after the imports):

```rust
/// Per-session server context. One stdio server process == one agent session,
/// so `default_holder` is stable for the process lifetime.
pub struct ServerCtx {
    pub default_holder: String,
}

/// `$DEVKIT_SESSION` if set and non-empty, else a stable per-process id.
pub fn mint_holder() -> String {
    std::env::var("DEVKIT_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("mcp-{}", std::process::id()))
}
```

Change `run` and `dispatch` to thread `ctx`:

```rust
pub fn run(reader: &mut impl BufRead, writer: &mut impl Write, ctx: &ServerCtx) -> Result<()> {
    while let Some(line) = jsonrpc::read_line_value(reader)? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                jsonrpc::write_response(
                    writer,
                    &Response::err(Value::Null, PARSE_ERROR, format!("parse error: {e}")),
                )?;
                continue;
            }
        };
        if let Some(resp) = dispatch(ctx, &req) {
            jsonrpc::write_response(writer, &resp)?;
        }
    }
    Ok(())
}

fn dispatch(ctx: &ServerCtx, req: &Request) -> Option<Response> {
    match req.method.as_str() {
        "initialize" => Some(Response::ok(req.id.clone()?, initialize_result())),
        "tools/list" => Some(Response::ok(req.id.clone()?, tools_list_result())),
        "tools/call" => Some(tools_call(ctx, req.id.clone()?, &req.params)),
        "notifications/initialized" => None,
        _ => Some(Response::err(
            req.id.clone()?,
            METHOD_NOT_FOUND,
            format!("method not found: {}", req.method),
        )),
    }
}

fn tools_call(ctx: &ServerCtx, id: Value, params: &Value) -> Response {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or_default();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let result: Result<Value> = match name {
        "devkit_describe" => actions::describe(arguments),
        "devkit_call" => actions::call(ctx, arguments),
        other => Err(anyhow::anyhow!("unknown tool: {other}")),
    };
    match result {
        Ok(v) => Response::ok(id, tool_result(&v, false)),
        Err(e) => Response::ok(id, tool_result(&Value::String(format!("{e:#}")), true)),
    }
}

fn tool_result(payload: &Value, is_error: bool) -> Value {
    let text = match payload.as_str() {
        Some(s) => s.to_string(),
        None => serde_json::to_string(payload).unwrap_or_else(|_| "null".to_string()),
    };
    serde_json::json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": is_error
    })
}
```

Update the existing `drive` test helper in `lib.rs` to pass a `ServerCtx`:

```rust
    fn drive(input: &str) -> Vec<Value> {
        let ctx = ServerCtx { default_holder: "test-session".to_string() };
        let mut out = Vec::new();
        run(&mut input.as_bytes(), &mut out, &ctx).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
```

- [ ] **Step 2: Update the binary to pass the context** — `src/bin/devkit-mcp/main.rs`:

```rust
use std::io::{BufReader, Write};

use anyhow::Result;

fn main() -> Result<()> {
    devkit_common::report::install_panic_hook("devkit-mcp");
    devkit_common::paths::migrate_legacy_state();
    let ctx = devkit_mcp::ServerCtx { default_holder: devkit_mcp::mint_holder() };
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    devkit_mcp::run(&mut reader, &mut writer, &ctx)?;
    writer.flush()?;
    Ok(())
}
```

- [ ] **Step 3: Write the action registry with unit tests** — `crates/devkit-mcp/src/actions.rs`:

```rust
use anyhow::{anyhow, Result};
use serde_json::Value;

use crate::ServerCtx;

/// One registered action. `schema` returns its argument JSON Schema; `handler`
/// validates + executes it. The registry is the single source of truth for both
/// `devkit_describe` and `devkit_call`.
pub struct Action {
    pub name: &'static str,
    pub summary: &'static str,
    pub schema: fn() -> Value,
    pub handler: fn(&ServerCtx, Value) -> Result<Value>,
}

/// All registered actions. Adding a binary's actions is one `extend` line.
pub fn actions() -> Vec<Action> {
    let mut v = Vec::new();
    v.extend(crate::ports::actions());
    v
}

pub fn find(name: &str) -> Option<Action> {
    actions().into_iter().find(|a| a.name == name)
}

/// `devkit_describe`: no `action` -> list `{action, summary}`; with `action` ->
/// that action's argument schema.
pub fn describe(args: Value) -> Result<Value> {
    match args.get("action").and_then(|v| v.as_str()) {
        None => {
            let list: Vec<Value> = actions()
                .iter()
                .map(|a| serde_json::json!({ "action": a.name, "summary": a.summary }))
                .collect();
            Ok(Value::Array(list))
        }
        Some(name) => {
            let a = find(name).ok_or_else(|| anyhow!("unknown action: {name}"))?;
            Ok((a.schema)())
        }
    }
}

/// `devkit_call`: look up the action, hand it its `args` object.
pub fn call(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let name = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing required field: action"))?;
    let a = find(name).ok_or_else(|| anyhow!("unknown action: {name}"))?;
    let action_args = args
        .get("args")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    (a.handler)(ctx, action_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_lists_the_ports_actions() {
        let list = describe(Value::Null).unwrap();
        let names: Vec<&str> =
            list.as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
        assert!(names.contains(&"ports.status"));
        assert!(names.contains(&"ports.alloc"));
        assert!(names.contains(&"ports.release"));
        assert!(names.contains(&"ports.prune"));
    }

    #[test]
    fn describe_returns_a_schema_for_each_action() {
        for a in actions() {
            let schema = describe(serde_json::json!({ "action": a.name })).unwrap();
            assert_eq!(schema["type"], "object", "{} schema", a.name);
        }
    }

    #[test]
    fn describe_unknown_action_errors() {
        assert!(describe(serde_json::json!({ "action": "nope.nope" })).is_err());
    }

    #[test]
    fn call_unknown_action_errors() {
        let ctx = ServerCtx { default_holder: "t".to_string() };
        assert!(call(&ctx, serde_json::json!({ "action": "nope.nope" })).is_err());
    }
}
```

- [ ] **Step 4: Write the ports handlers** — `crates/devkit-mcp/src/ports.rs`:

```rust
use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use devkit_ports::registry::{self, Role};

use crate::actions::Action;
use crate::ServerCtx;

pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "ports.status",
            summary: "Show the current port-allocation registry.",
            schema: status_schema,
            handler: status,
        },
        Action {
            name: "ports.alloc",
            summary: "Allocate ports for one or more apps under a holder.",
            schema: alloc_schema,
            handler: alloc,
        },
        Action {
            name: "ports.release",
            summary: "Release a holder's port reservations.",
            schema: release_schema,
            handler: release,
        },
        Action {
            name: "ports.prune",
            summary: "Drop dead port reservations whose process is gone.",
            schema: prune_schema,
            handler: prune,
        },
    ]
}

fn resolve_holder(ctx: &ServerCtx, given: Option<String>) -> String {
    given.unwrap_or_else(|| ctx.default_holder.clone())
}

fn status_schema() -> Value {
    serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn status(_ctx: &ServerCtx, _args: Value) -> Result<Value> {
    let data = registry::snapshot()?;
    Ok(serde_json::to_value(data)?)
}

#[derive(Deserialize)]
struct AllocArgs {
    root: String,
    apps: Vec<String>,
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    holder: Option<String>,
}

fn alloc_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root (holds devkit.toml)." },
            "apps": { "type": "array", "items": { "type": "string" }, "description": "App names from the devkit.toml catalog." },
            "role": { "type": "string", "enum": ["issue", "baseline"], "description": "Allocation role (default issue)." },
            "holder": { "type": "string", "description": "Override the session holder id." }
        },
        "required": ["root", "apps"],
        "additionalProperties": false
    })
}

fn alloc(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: AllocArgs = serde_json::from_value(args).context("invalid ports.alloc arguments")?;
    let holder = resolve_holder(ctx, a.holder);
    let role = a.role.unwrap_or(Role::Issue);
    let loaded = devkit_ports::load::load(None, std::path::Path::new(&a.root))
        .context("loading devkit.toml")?;
    let mut reqs = Vec::with_capacity(a.apps.len());
    for app in &a.apps {
        let base = loaded
            .catalog
            .get(app)
            .ok_or_else(|| anyhow!("unknown app `{app}`"))?
            .base_port;
        reqs.push((app.clone(), base));
    }
    let allocated = registry::alloc(&holder, &reqs, role)?;
    let map: serde_json::Map<String, Value> =
        allocated.into_iter().map(|(app, port)| (app, Value::from(port))).collect();
    Ok(Value::Object(map))
}

#[derive(Deserialize)]
struct ReleaseArgs {
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    holder: Option<String>,
}

fn release_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "role": { "type": "string", "enum": ["issue", "baseline"], "description": "Only release this role (default: all roles)." },
            "holder": { "type": "string", "description": "Override the session holder id." }
        },
        "additionalProperties": false
    })
}

fn release(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: ReleaseArgs = serde_json::from_value(args).context("invalid ports.release arguments")?;
    let holder = resolve_holder(ctx, a.holder);
    let freed = registry::release(&holder, a.role)?;
    Ok(serde_json::json!({ "freed": freed }))
}

fn prune_schema() -> Value {
    serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn prune(_ctx: &ServerCtx, _args: Value) -> Result<Value> {
    let freed = registry::prune()?;
    Ok(serde_json::json!({ "freed": freed }))
}
```

- [ ] **Step 5: Write the root integration test harness + ports tests** — `tests/mcp.rs`:

```rust
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{json, Value};

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
            call_req(1, "ports.alloc", json!({ "root": root, "apps": ["web"], "holder": "sess-1" })),
            call_req(2, "ports.status", json!({})),
            call_req(3, "ports.release", json!({ "holder": "sess-1" })),
        ],
    );
    let alloc = tool_json(&resps[0], false);
    let port = alloc["web"].as_u64().expect("web port");
    assert!(port >= 3000, "allocated port {port} >= base 3000");

    let status = tool_json(&resps[1], false);
    let entries = status["entries"].as_object().expect("entries map");
    assert!(entries.contains_key(&port.to_string()), "status shows the allocated port");

    let released = tool_json(&resps[2], false);
    assert_eq!(released["freed"].as_array().unwrap().len(), 1);
}

#[test]
fn ports_alloc_unknown_app_is_an_error() {
    let proj = project_with_config();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps =
        mcp(&proj, &state, &[call_req(1, "ports.alloc", json!({ "root": root, "apps": ["ghost"] }))]);
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("ghost"), "error names the app");
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p devkit-mcp`
Expected: unit tests pass (registry/describe + the Task 1/2 tests).
Run: `cargo test -p devkit --test mcp`
Expected: `ports_alloc_status_release_roundtrip` and `ports_alloc_unknown_app_is_an_error` pass.

- [ ] **Step 7: Commit**

```bash
git add crates/devkit-mcp/src/actions.rs crates/devkit-mcp/src/ports.rs crates/devkit-mcp/src/lib.rs src/bin/devkit-mcp/main.rs tests/mcp.rs
git commit -m "feat(mcp): add action registry, describe/call, and ports actions"
```

---

### Task 4: The 5 locks handlers

**Files:**
- Create: `crates/devkit-mcp/src/locks.rs`
- Modify: `crates/devkit-mcp/src/lib.rs` (add `mod locks;`)
- Modify: `crates/devkit-mcp/src/actions.rs` (register locks actions)
- Modify: `tests/mcp.rs` (locks integration tests)

**Interfaces:**
- Produces: `locks::actions() -> Vec<Action>` registering `locks.acquire`, `locks.check`, `locks.release`, `locks.status`, `locks.prune`.
- Consumes: `devkit_locks::normalize_under_root(abs: &Path, root: &Path) -> Result<String>`; `devkit_locks::store::{FlockStore, acquire_with, check_with, release_with, release_all_with, status_with, prune_with}`. `FlockStore::new()` is public. The `*_with` ops take already-normalized root-relative path keys and a `now: u64` unix-seconds value (compute locally — `devkit_locks`'s `now()` is private).

- [ ] **Step 1: Register the module and actions**

In `crates/devkit-mcp/src/lib.rs`, add `mod locks;` next to the other module declarations.

In `crates/devkit-mcp/src/actions.rs`, extend `actions()`:

```rust
pub fn actions() -> Vec<Action> {
    let mut v = Vec::new();
    v.extend(crate::ports::actions());
    v.extend(crate::locks::actions());
    v
}
```

- [ ] **Step 2: Write the locks handlers** — `crates/devkit-mcp/src/locks.rs`:

```rust
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;

use devkit_locks::normalize_under_root;
use devkit_locks::store::{
    acquire_with, check_with, prune_with, release_all_with, release_with, status_with, FlockStore,
};

use crate::actions::Action;
use crate::ServerCtx;

pub fn actions() -> Vec<Action> {
    vec![
        Action {
            name: "locks.acquire",
            summary: "Claim one or more paths for the session (all-or-nothing).",
            schema: acquire_schema,
            handler: acquire,
        },
        Action {
            name: "locks.check",
            summary: "Check whether paths are locked by another holder.",
            schema: check_schema,
            handler: check,
        },
        Action {
            name: "locks.release",
            summary: "Release locks the session holds.",
            schema: release_schema,
            handler: release,
        },
        Action {
            name: "locks.status",
            summary: "List held locks for the project (or all projects).",
            schema: status_schema,
            handler: status,
        },
        Action {
            name: "locks.prune",
            summary: "Drop expired or dead locks.",
            schema: prune_schema,
            handler: prune,
        },
    ]
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn resolve_holder(ctx: &ServerCtx, given: Option<String>) -> String {
    given.unwrap_or_else(|| ctx.default_holder.clone())
}

/// Express each input path as a root-relative key. Inputs may be absolute or
/// relative to `root`.
fn normalize(root: &str, paths: &[String]) -> Result<Vec<String>> {
    let root_path = Path::new(root);
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let pp = Path::new(p);
        let abs = if pp.is_absolute() { pp.to_path_buf() } else { root_path.join(pp) };
        out.push(
            normalize_under_root(&abs, root_path).with_context(|| format!("normalizing {p}"))?,
        );
    }
    Ok(out)
}

#[derive(Deserialize)]
struct AcquireArgs {
    root: String,
    paths: Vec<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default = "default_ttl")]
    ttl: u64,
    #[serde(default)]
    holder: Option<String>,
}

fn default_ttl() -> u64 {
    1800
}

fn acquire_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root." },
            "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths to lock (absolute or root-relative)." },
            "note": { "type": "string", "description": "Optional note shown to others who hit the lock." },
            "ttl": { "type": "integer", "minimum": 0, "description": "Lease seconds; 0 = no expiry. Default 1800." },
            "holder": { "type": "string", "description": "Override the session holder id." }
        },
        "required": ["root", "paths"],
        "additionalProperties": false
    })
}

fn acquire(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: AcquireArgs = serde_json::from_value(args).context("invalid locks.acquire arguments")?;
    let holder = resolve_holder(ctx, a.holder);
    let paths = normalize(&a.root, &a.paths)?;
    let outcome = acquire_with(
        &FlockStore::new(),
        &a.root,
        &holder,
        &paths,
        None,
        a.note.as_deref(),
        a.ttl,
        now(),
    )?;
    Ok(serde_json::to_value(outcome)?)
}

#[derive(Deserialize)]
struct CheckArgs {
    root: String,
    paths: Vec<String>,
    #[serde(default)]
    holder: Option<String>,
}

fn check_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root." },
            "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths to check." },
            "holder": { "type": "string", "description": "Override the session holder id (a path held by this holder is not a conflict)." }
        },
        "required": ["root", "paths"],
        "additionalProperties": false
    })
}

fn check(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: CheckArgs = serde_json::from_value(args).context("invalid locks.check arguments")?;
    let holder = resolve_holder(ctx, a.holder);
    let paths = normalize(&a.root, &a.paths)?;
    let conflicts = check_with(&FlockStore::new(), &a.root, &holder, &paths, now())?;
    Ok(serde_json::to_value(conflicts)?)
}

#[derive(Deserialize)]
struct ReleaseArgs {
    root: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    all: bool,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    holder: Option<String>,
}

fn release_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root." },
            "paths": { "type": "array", "items": { "type": "string" }, "description": "Paths to release (required unless all=true)." },
            "all": { "type": "boolean", "description": "Release every lock held by this holder in the project." },
            "force": { "type": "boolean", "description": "Release even locks held by another holder." },
            "holder": { "type": "string", "description": "Override the session holder id." }
        },
        "required": ["root"],
        "additionalProperties": false
    })
}

fn release(ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: ReleaseArgs = serde_json::from_value(args).context("invalid locks.release arguments")?;
    let holder = resolve_holder(ctx, a.holder);
    if a.all {
        let released = release_all_with(&FlockStore::new(), &a.root, &holder)?;
        return Ok(serde_json::json!({ "released": released, "refused": [] }));
    }
    if a.paths.is_empty() {
        bail!("locks.release requires `paths` unless `all` is true");
    }
    let paths = normalize(&a.root, &a.paths)?;
    let (released, refused) =
        release_with(&FlockStore::new(), &a.root, &holder, &paths, a.force)?;
    Ok(serde_json::json!({ "released": released, "refused": refused }))
}

#[derive(Deserialize)]
struct StatusArgs {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    all: bool,
}

fn status_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "root": { "type": "string", "description": "Absolute path to the project root (required unless all=true)." },
            "all": { "type": "boolean", "description": "List locks across all projects." }
        },
        "additionalProperties": false
    })
}

fn status(_ctx: &ServerCtx, args: Value) -> Result<Value> {
    let a: StatusArgs = serde_json::from_value(args).context("invalid locks.status arguments")?;
    let root = match (a.root, a.all) {
        (Some(r), _) => r,
        (None, true) => String::new(),
        (None, false) => bail!("locks.status requires `root` unless `all` is true"),
    };
    let entries = status_with(&FlockStore::new(), &root, a.all, now())?;
    Ok(serde_json::to_value(entries)?)
}

fn prune_schema() -> Value {
    serde_json::json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn prune(_ctx: &ServerCtx, _args: Value) -> Result<Value> {
    let pruned = prune_with(&FlockStore::new(), now())?;
    Ok(serde_json::json!({ "pruned": pruned }))
}
```

- [ ] **Step 3: Add locks integration tests** — append to `tests/mcp.rs`:

```rust
#[test]
fn locks_acquire_then_other_holder_sees_conflict() {
    let proj = project();
    let state = scratch("state");
    let root = proj.to_str().unwrap();
    let resps = mcp(
        &proj,
        &state,
        &[
            call_req(1, "locks.acquire", json!({ "root": root, "paths": ["src/a.rs"], "holder": "alice" })),
            call_req(2, "locks.check", json!({ "root": root, "paths": ["src/a.rs"], "holder": "bob" })),
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
            call_req(1, "locks.acquire", json!({ "root": root, "paths": ["x.txt"], "holder": "alice" })),
            call_req(2, "locks.release", json!({ "root": root, "paths": ["x.txt"], "holder": "alice" })),
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
    let resps =
        mcp(&proj, &state, &[call_req(1, "locks.release", json!({ "root": root, "holder": "alice" }))]);
    let payload = tool_json(&resps[0], true);
    assert!(payload.as_str().unwrap().contains("paths"));
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p devkit-mcp` — describe-returns-a-schema-for-each now covers all 9 actions.
Expected: PASS.
Run: `cargo test -p devkit --test mcp`
Expected: ports + locks tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/devkit-mcp/src/locks.rs crates/devkit-mcp/src/lib.rs crates/devkit-mcp/src/actions.rs tests/mcp.rs
git commit -m "feat(mcp): add file-lock actions"
```

---

### Task 5: Packaging, docs, and the full gate

**Files:**
- Create: `.mcp.json`
- Modify: `README.md`
- Modify: `AGENTS.md`
- Modify: `docs/next-steps.md`

**Interfaces:**
- Produces: an MCP-server registration consumable by the Claude Code plugin; user/contributor docs; an updated follow-ups list. No new code.

- [ ] **Step 1: Register the MCP server for the plugin** — create `.mcp.json` at the repo root:

```json
{
  "mcpServers": {
    "devkit": {
      "command": "devkit-mcp"
    }
  }
}
```

This assumes `devkit-mcp` is on `PATH` (installed via `cargo install --path .`). The Claude Code plugin packaged in `.claude-plugin/` discovers a root `.mcp.json`. Codex/Cursor MCP-server registration uses each host's own manifest field and is **verified live** (see next-steps) rather than guessed here.

- [ ] **Step 2: Document the server in `README.md`**

Add a section near the other binaries describing `devkit-mcp`:

```markdown
## devkit-mcp (MCP server)

`devkit-mcp` exposes devkit's port and file-lock coordination to MCP-capable
coding agents over stdio. It presents two tools:

- `devkit_describe` — list the available actions, or fetch one action's argument
  schema (`{"action": "locks.acquire"}`).
- `devkit_call` — invoke an action, e.g.
  `{"action": "locks.acquire", "args": {"root": "/path/to/repo", "paths": ["src/a.rs"]}}`.

v1 actions: `ports.{status,alloc,release,prune}` and
`locks.{acquire,check,release,status,prune}`. Pass `root` (the project path) on
every lock call and on `ports.alloc`; the session `holder` is minted from
`$DEVKIT_SESSION` (or a per-process id) and can be overridden per call.

Install with `cargo install --path .` (it builds alongside the other binaries),
then register it with your agent — the bundled plugin's `.mcp.json` points at the
`devkit-mcp` command.
```

- [ ] **Step 3: Note the crate in `AGENTS.md`**

In the layout table, add a row:

```markdown
| `crates/devkit-mcp` | lib: stdio MCP server (`jsonrpc`, action `registry`, `ports`/`locks` handlers) over the port + lock facades |
```

And add a `src/bin/devkit-mcp` row:

```markdown
| `src/bin/devkit-mcp` | meta-MCP stdio server exposing the port + lock facades to coding agents |
```

- [ ] **Step 4: Update `docs/next-steps.md`**

Under the `## MCP server for devkit` section, replace the "Deferred" framing with a status line and add the deferred follow-ups:

```markdown
## MCP server for devkit

v1 is implemented (`crates/devkit-mcp` + `src/bin/devkit-mcp`): a meta-MCP stdio
server (`devkit_describe` + `devkit_call`) exposing the 9 port + lock actions over
the library facades. Design: `docs/superpowers/specs/2026-06-21-devkit-mcp-server-design.md`.
Plan: `docs/superpowers/plans/2026-06-21-devkit-mcp-server.md`.

Deferred follow-ups:

- **Daemon-aware locks.** v1 lock writes go straight through `FlockStore` and will
  hard-error (`DaemonHoldsLock`) under a live `devkitd`. Full cooperation needs
  daemon-aware resolved-context lock facade variants — owned by the "Authoritative
  in-memory mode for the lock registry" section above. Until then, run lock actions
  without a daemon, or wire that work first.
- **`devrun` + `issue` actions.** Phase 2 of the surface (process supervision, the
  issue/PR lifecycle). Higher blast radius; add as new registry entries — the tool
  shape does not change.
- **Live MCP registration for Codex and Cursor.** Only the Claude Code `.mcp.json`
  is provided. Confirm the MCP-server registration field each host expects, install
  the plugin in Codex and Cursor, and confirm `devkit_describe`/`devkit_call` appear.
- **`initialize` protocol-version negotiation.** The server returns a fixed
  `protocolVersion`; confirm it against the versions the target hosts send and
  negotiate if a host requires it.
```

- [ ] **Step 5: Format, lint, and run the full gate**

Run: `cargo fmt --all`
Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero warnings.
Run: `cargo test --workspace`
Expected: all tests green (existing suite + the new `devkit-mcp` unit tests + `tests/mcp.rs`).

- [ ] **Step 6: Validate the Claude plugin manifest** — dispatch the `plugin-dev:plugin-validator` agent over `.claude-plugin/` + the new `.mcp.json` and confirm it reports no errors.

- [ ] **Step 7: Commit**

```bash
git add .mcp.json README.md AGENTS.md docs/next-steps.md
git commit -m "docs(mcp): document devkit-mcp, register server, update next-steps"
```

---

## Verification (whole-branch)

- `cargo test --workspace` green; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean.
- A fresh `cargo build --release` produces a `devkit-mcp` binary.
- Manual smoke (optional, documented in next-steps): pipe an `initialize` + `tools/list` + a `devkit_call` for `locks.status` into `target/release/devkit-mcp` with an isolated `XDG_STATE_HOME` and confirm well-formed JSON-RPC responses.

## Unresolved questions

1. **Daemon cooperation for locks in v1.** This plan ships v1 lock actions over direct flock, which **hard-errors under a live `devkitd`** (writes only; reads are fine). The alternative is to first build the daemon-aware resolved-context lock facade variants (tracked under the lock-registry in-memory work) and route the MCP handlers through them. Ship v1 direct-flock now (recommended — keeps scope tight, lock/daemon cooperation lands with the work that already owns it), or pull that work in first?
2. **MCP registration for Codex/Cursor.** Only the Claude Code `.mcp.json` is included; the Codex/Cursor registration format is left to live verification (mirroring how the packaging plan handled hooks). Acceptable, or should the plan pin those formats now?
3. **`protocolVersion`.** The server advertises a fixed `"2024-11-05"`. Fine as a baseline, or do you want version negotiation in v1?
