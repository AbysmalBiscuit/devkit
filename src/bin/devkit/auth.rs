use anyhow::{Context, Result};
use devkit_common::github::{self, TokenSource};
use devkit_common::progress::Steps;
use devkit_common::tracker::linear;
use devkit_common::{secrets, slack};
use std::io::{IsTerminal, Read};
use std::path::Path;

use crate::Provider;

fn store_linear(path: &Path, token: &str, id: &linear::LinearIdentity) -> Result<()> {
    secrets::store_at(path, "linear_api_key", token)?;
    secrets::store_at(path, "linear_workspace", &id.workspace_url_key)?;
    Ok(())
}

fn store_slack(path: &Path, token: &str) -> Result<()> {
    secrets::store_at(path, "slack_token", token)
}

pub fn run(provider: Provider, token: Option<String>) -> Result<()> {
    if let Provider::Github = provider {
        return run_github();
    }
    let token = acquire(provider, token)?;
    let path = secrets::secrets_path();
    let steps = Steps::new();
    match provider {
        Provider::Linear => {
            let id = steps
                .during("Validating Linear API key…", || linear::validate(&token))
                .context("validating Linear API key")?;
            store_linear(&path, &token, &id)?;
            println!(
                "✓ linear: workspace \"{}\" ({})",
                id.workspace_url_key, id.viewer_email
            );
        }
        Provider::Slack => {
            let id = steps
                .during("Validating Slack token…", || slack::validate(&token))
                .context("validating Slack token")?;
            store_slack(&path, &token)?;
            println!("✓ slack: team \"{}\" (user {})", id.team, id.user);
        }
        Provider::Github => unreachable!("handled above"),
    }
    println!("  saved to {}", path.display());
    Ok(())
}

fn acquire(provider: Provider, token: Option<String>) -> Result<String> {
    if let Some(t) = token {
        return Ok(t.trim().to_string());
    }
    if !std::io::stdin().is_terminal() {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("reading token from stdin")?;
        return Ok(buf.trim().to_string());
    }
    eprintln!("{}", hint(provider));
    let entered = rpassword::prompt_password(format!("Paste your {} token: ", provider.label()))
        .context("reading token")?;
    Ok(entered.trim().to_string())
}

fn hint(provider: Provider) -> &'static str {
    match provider {
        Provider::Linear => "Create a Personal API Key at https://linear.app/settings/api",
        Provider::Slack => "Create a bot token on your Slack app's OAuth & Permissions page",
        Provider::Github => "run: gh auth login",
    }
}

/// One `gh` account on one host, as `gh auth status --json hosts` reports it.
struct GhHost {
    login: String,
    host: String,
    active: bool,
}

/// Parse the `hosts` map from `gh auth status --json hosts` (the inner object
/// keyed by host, not the `{"hosts": …}` envelope). Anything malformed — a
/// missing map, a host whose value is not an array, an entry with no `login`
/// — degrades to leaving that entry out. This is a diagnostic, not a gate: it
/// never errors and never fails the command.
fn parse_gh_hosts(hosts: &serde_json::Value) -> Vec<GhHost> {
    let Some(map) = hosts.as_object() else {
        return Vec::new();
    };
    map.iter()
        .flat_map(|(host, accounts)| {
            let accounts = accounts.as_array().cloned().unwrap_or_default();
            let host = host.clone();
            accounts.into_iter().filter_map(move |a| {
                Some(GhHost {
                    login: a.get("login")?.as_str()?.to_string(),
                    host: host.clone(),
                    active: a.get("active").and_then(|v| v.as_bool()).unwrap_or(false),
                })
            })
        })
        .collect()
}

fn source_label(source: TokenSource) -> String {
    match source {
        TokenSource::Env(var) => var.to_string(),
        TokenSource::Gh => "gh auth token".to_string(),
        TokenSource::None => "none".to_string(),
    }
}

/// Report devkit's GitHub identity: which token it would send, and who that
/// token belongs to. `gh`'s active account is listed separately, below the
/// identity line — it is what `gh` itself would use, which is the same
/// account only when the token came from `gh auth token` (`TokenSource::Gh`).
/// With `GH_TOKEN`/`GITHUB_TOKEN` set, devkit's identity and gh's active
/// account can differ, and reporting the wrong one is worse than reporting
/// neither.
fn github_report(token_source: TokenSource, viewer: Option<&str>, hosts: &[GhHost]) -> String {
    let mut out = String::new();
    match (token_source, viewer) {
        (TokenSource::None, _) => {
            out.push_str("✗ github: no token found\n");
            out.push_str("  set GH_TOKEN or GITHUB_TOKEN, or run: gh auth login\n");
        }
        (source, Some(login)) => {
            out.push_str(&format!(
                "✓ github: {login}  (token from {})\n",
                source_label(source)
            ));
        }
        (source, None) => {
            out.push_str(&format!(
                "⚠ github: token from {} but could not resolve the identity\n",
                source_label(source)
            ));
        }
    }
    if !hosts.is_empty() {
        out.push_str("\ngh accounts on this machine:\n");
        for h in hosts {
            let marker = if h.active { "*" } else { " " };
            out.push_str(&format!("  {marker} {} ({})\n", h.login, h.host));
        }
        if !matches!(token_source, TokenSource::Gh) {
            out.push_str(
                "  (devkit's identity above comes from the token, not gh's active account)\n",
            );
        }
    }
    out
}

fn gh_auth_status_hosts() -> serde_json::Value {
    devkit_common::cmd::capture("gh", &["auth", "status", "--json", "hosts"], None)
        .ok()
        .and_then(|out| serde_json::from_str(&out).ok())
        .unwrap_or_default()
}

fn run_github() -> Result<()> {
    let source = github::token_source();
    let viewer = match source {
        TokenSource::None => None,
        TokenSource::Env(_) | TokenSource::Gh => github::rest_get("/user")
            .ok()
            .and_then(|v| v.get("login").and_then(|l| l.as_str()).map(String::from)),
    };
    let hosts_resp = gh_auth_status_hosts();
    let hosts = parse_gh_hosts(&hosts_resp["hosts"]);
    print!("{}", github_report(source, viewer.as_deref(), &hosts));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_store_persists_key_and_workspace() {
        let p = std::env::temp_dir()
            .join(format!("devkit-auth-{}", std::process::id()))
            .join("secrets.toml");
        let _ = std::fs::remove_file(&p);
        let id = linear::LinearIdentity {
            workspace_url_key: "adaptyv".into(),
            org_name: "Adaptyv".into(),
            viewer_email: "me@x.io".into(),
        };
        store_linear(&p, "lin_secret", &id).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("lin_secret"));
        assert!(body.contains("adaptyv"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn the_identity_comes_from_the_token_not_the_active_gh_account() {
        // resolve_token reads GH_TOKEN, then GITHUB_TOKEN, and only then falls back
        // to `gh auth token`. With either variable set, the active gh account is
        // not the identity devkit uses, and reporting it as such would mislead
        // precisely the user who most needs the answer.
        let out = github_report(
            TokenSource::Env("GH_TOKEN"),
            Some("ci-bot"),
            &[GhHost {
                login: "a-human".into(),
                host: "github.com".into(),
                active: true,
            }],
        );
        assert!(out.contains("ci-bot"), "{out}");
        assert!(out.contains("GH_TOKEN"), "{out}");
        // The gh accounts are secondary diagnostics, below the identity line.
        assert!(
            out.find("ci-bot").unwrap() < out.find("a-human").unwrap(),
            "{out}"
        );
    }

    #[test]
    fn no_token_prints_the_login_instruction() {
        let out = github_report(TokenSource::None, None, &[]);
        assert!(out.contains("gh auth login"), "{out}");
        assert!(
            out.contains("GH_TOKEN") && out.contains("GITHUB_TOKEN"),
            "{out}"
        );
    }

    #[test]
    fn a_malformed_or_missing_hosts_payload_degrades() {
        assert!(parse_gh_hosts(&serde_json::json!({})).is_empty());
        assert!(parse_gh_hosts(&serde_json::json!({"github.com": "nonsense"})).is_empty());
    }
}
