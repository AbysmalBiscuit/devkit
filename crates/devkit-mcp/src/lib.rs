mod jsonrpc;

use std::io::{BufRead, Write};

use anyhow::Result;
use serde_json::Value;

use jsonrpc::{METHOD_NOT_FOUND, PARSE_ERROR, Request, Response};

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

    #[test]
    fn initialize_returns_server_info() {
        let resps =
            drive("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n");
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
}
