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
    let id = req.id.clone()?;
    Some(Response::err(
        id,
        METHOD_NOT_FOUND,
        format!("method not found: {}", req.method),
    ))
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
