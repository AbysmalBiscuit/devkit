use anyhow::{Context, Result};
use devkit_common::progress::Steps;
use devkit_common::{linear, secrets, slack};
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
    }
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
}
