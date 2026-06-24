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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackIdentity {
    pub team: String,
    pub user: String,
    pub url: String,
}

/// Validate `token` via `auth.test`, returning the bot/user identity. The ureq
/// error is preserved as the top-level error (no `.context`) so a caller can
/// downcast it to tell an unreachable host from a rejected token.
pub fn validate(token: &str) -> Result<SlackIdentity> {
    let resp: serde_json::Value = ureq::post("https://slack.com/api/auth.test")
        .set("Authorization", &format!("Bearer {token}"))
        .call()?
        .into_json()?;
    parse_identity(&resp)
}

fn parse_identity(resp: &serde_json::Value) -> Result<SlackIdentity> {
    if resp.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        bail!("Slack token rejected: {err}");
    }
    Ok(SlackIdentity {
        team: resp["team"].as_str().unwrap_or("").to_string(),
        user: resp["user"].as_str().unwrap_or("").to_string(),
        url: resp["url"].as_str().unwrap_or("").to_string(),
    })
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

    #[test]
    fn slack_identity_parsed() {
        let v = serde_json::json!({
            "ok": true, "team": "Adaptyv", "user": "devkit",
            "url": "https://adaptyv.slack.com/"
        });
        let id = parse_identity(&v).unwrap();
        assert_eq!(id.team, "Adaptyv");
        assert_eq!(id.user, "devkit");
        assert_eq!(id.url, "https://adaptyv.slack.com/");
    }

    #[test]
    fn slack_not_ok_surfaces_error() {
        let v = serde_json::json!({ "ok": false, "error": "invalid_auth" });
        let e = parse_identity(&v).unwrap_err();
        assert!(e.to_string().contains("invalid_auth"));
    }
}
