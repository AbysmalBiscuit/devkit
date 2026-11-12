//! The signers `pr waive-verify --signed` trusts, and the check of a
//! detached signature against them. The check runs the format's own tool
//! through [`SignatureTools`], so this crate spawns no process itself.

use std::path::{Path, PathBuf};

use ambassador::{Delegate, delegatable_trait};
use anyhow::{Result, anyhow, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The ssh signature namespace a waiver record is signed under, so a
/// signature made for git or another tool does not count as a waiver.
pub const SSH_NAMESPACE: &str = "devkit-waiver";

/// The IO a signature check needs.
pub trait SignatureTools {
    /// A trusted-key path as the config writes it, made absolute. Fails
    /// for a path that could depend on the caller, such as a relative one.
    fn path(&self, configured: &str) -> Result<PathBuf>;
    /// The executable to run for `tool`: `configured` when the config names
    /// one, else the tool from a location the caller cannot change.
    fn program(&self, tool: &str, configured: Option<&str>) -> Result<PathBuf>;
    /// Run `program` with `stdin` and an environment the caller cannot set,
    /// and return its stdout. A nonzero exit fails with the tool's stderr.
    fn run(&self, program: &Path, args: &[&str], stdin: &[u8]) -> Result<String>;
}

/// A check of a detached signature against a set of trusted keys.
#[delegatable_trait]
pub trait SignatureVerifier {
    /// The identity of the trusted key that made `signature` over `message`.
    /// Fails when no trusted key made it.
    fn verify(
        &self,
        tools: &dyn SignatureTools,
        message: &[u8],
        signature: &Path,
    ) -> Result<String>;
}

/// How a waiver record is signed, and the keys that devkit trusts for it.
///
/// ```
/// # use devkit_config::{Config, SignatureFormat};
/// # let cfg = Config::parse(r#"
/// [pr.signed_waivers]
/// format = "ssh"
/// trusted_keys = "~/.config/devkit/allowed_signers"
/// # "#).unwrap();
/// # let Some(SignatureFormat::Ssh(ssh)) = &cfg.pr.signed_waivers else { panic!() };
/// # assert_eq!(ssh.trusted_keys, "~/.config/devkit/allowed_signers");
/// # assert_eq!(<&str>::from(cfg.pr.signed_waivers.as_ref().unwrap()), "ssh");
/// # let pgp = Config::parse(
/// #     "[pr.signed_waivers]\nformat = \"openpgp\"\ntrusted_keys = \"/k.gpg\"\nprogram = \"/opt/gnupg/bin/gpgv\"\n",
/// # ).unwrap();
/// # let Some(SignatureFormat::Openpgp(gpg)) = &pgp.pr.signed_waivers else { panic!() };
/// # assert_eq!(gpg.program.as_deref(), Some("/opt/gnupg/bin/gpgv"));
/// # assert!(Config::parse("").unwrap().pr.signed_waivers.is_none());
/// # assert!(Config::parse("[pr.signed_waivers]\nformat = \"x509\"\ntrusted_keys = \"/k\"\n").is_err());
/// # assert!(Config::parse("[pr.signed_waivers]\nformat = \"ssh\"\n").is_err());
/// ```
#[derive(
    Debug, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize, Delegate, strum::IntoStaticStr,
)]
#[serde(tag = "format", rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
#[delegate(SignatureVerifier)]
pub enum SignatureFormat {
    /// An ssh signature from `ssh-keygen -Y sign -n devkit-waiver`,
    /// checked with `ssh-keygen -Y verify`.
    Ssh(SshVerifier),
    /// An OpenPGP detached signature from `gpg --detach-sign`, checked with
    /// `gpgv`.
    Openpgp(GpgVerifier),
}

/// `ssh-keygen -Y verify` against an `allowed_signers` file. The signer is
/// the principal that the file lists for the signing key.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SshVerifier {
    /// The `allowed_signers` file. Its principals must allow the
    /// `devkit-waiver` namespace. An absolute path, or `~/` for the account
    /// home.
    pub trusted_keys: String,
    /// The `ssh-keygen` to run, as an absolute path. When unset, devkit
    /// runs the one in the system directories (`/usr/bin`, `/bin`, or
    /// Windows' `System32\OpenSSH`), never one that `PATH` names.
    pub program: Option<String>,
}

impl SignatureVerifier for SshVerifier {
    fn verify(
        &self,
        tools: &dyn SignatureTools,
        message: &[u8],
        signature: &Path,
    ) -> Result<String> {
        let program = tools.program("ssh-keygen", self.program.as_deref())?;
        let allowed = tools.path(&self.trusted_keys)?;
        let (allowed, signature) = (arg(&allowed)?, arg(signature)?);
        let principals = tools.run(
            &program,
            &["-Y", "find-principals", "-f", allowed, "-s", signature],
            &[],
        )?;
        let mut last = anyhow!("no principal in the trusted keys holds the signing key");
        for principal in principals.lines().map(str::trim).filter(|p| !p.is_empty()) {
            let args = [
                "-Y",
                "verify",
                "-f",
                allowed,
                "-I",
                principal,
                "-n",
                SSH_NAMESPACE,
                "-s",
                signature,
            ];
            match tools.run(&program, &args, message) {
                Ok(_) => return Ok(principal.to_string()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }
}

/// `gpgv` against a keyring. The signer is the signing key's user id and
/// primary key fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GpgVerifier {
    /// The keyring that `gpgv` reads, such as the output of `gpg --export`.
    /// An absolute path, or `~/` for the account home.
    pub trusted_keys: String,
    /// The `gpgv` to run, as an absolute path. When unset, devkit runs the
    /// one in the system directories (`/usr/bin` or `/bin`), never one that
    /// `PATH` names.
    pub program: Option<String>,
}

impl SignatureVerifier for GpgVerifier {
    fn verify(
        &self,
        tools: &dyn SignatureTools,
        message: &[u8],
        signature: &Path,
    ) -> Result<String> {
        let program = tools.program("gpgv", self.program.as_deref())?;
        let keyring = tools.path(&self.trusted_keys)?;
        let status = tools.run(
            &program,
            &[
                "--status-fd",
                "1",
                "--keyring",
                arg(&keyring)?,
                arg(signature)?,
                "-",
            ],
            message,
        )?;
        signer_of(&status)
    }
}

/// The `<user id> (<fingerprint>)` that gpgv's status lines name for a good,
/// valid signature.
fn signer_of(status: &str) -> Result<String> {
    let field = |tag: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix("[GNUPG:] ")?.strip_prefix(tag))
    };
    let user = field("GOODSIG ")
        .and_then(|rest| rest.split_once(' '))
        .map(|(_, uid)| uid);
    let primary = field("VALIDSIG ").and_then(|rest| rest.split_whitespace().nth(9));
    match (user, primary) {
        (Some(user), Some(primary)) => Ok(format!("{user} ({primary})")),
        _ => bail!("gpgv reported no good, valid signature"),
    }
}

fn arg(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("`{}` is not valid UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gpg_signer_is_the_user_id_and_primary_fingerprint() {
        let status = "[GNUPG:] NEWSIG\n\
            [GNUPG:] GOODSIG CFC0392BB636F30D Release Bot <rb@example.com>\n\
            [GNUPG:] VALIDSIG 3388AA 2026-11-12 1794472281 0 4 0 22 10 00 3388PRIMARY\n";
        assert_eq!(
            signer_of(status).unwrap(),
            "Release Bot <rb@example.com> (3388PRIMARY)"
        );
        assert!(signer_of("[GNUPG:] GOODSIG CFC0 Release Bot <rb@example.com>\n").is_err());
    }
}
