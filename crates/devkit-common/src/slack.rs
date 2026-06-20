use anyhow::{Result, bail};

/// Post a message to a Slack channel/user id via chat.postMessage.
pub fn post_message(token: &str, channel: &str, text: &str) -> Result<()> {
    let resp: serde_json::Value = ureq::post("https://slack.com/api/chat.postMessage")
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(ureq::json!({ "channel": channel, "text": text }))?
        .into_json()?;
    check_response(&resp)
}

/// Slack returns `{ "ok": true }` or `{ "ok": false, "error": "..." }`.
fn check_response(resp: &serde_json::Value) -> Result<()> {
    if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(());
    }
    let err = resp
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");
    bail!("Slack chat.postMessage failed: {err}");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ok_true_passes() {
        assert!(check_response(&serde_json::json!({ "ok": true })).is_ok());
    }
    #[test]
    fn ok_false_surfaces_error() {
        let e = check_response(&serde_json::json!({ "ok": false, "error": "channel_not_found" }))
            .unwrap_err();
        assert!(e.to_string().contains("channel_not_found"));
    }
}
