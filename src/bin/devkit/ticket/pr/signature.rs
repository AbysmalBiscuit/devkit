//! Detached-signature checks for `pr waive-verify --signed`. devkit runs the
//! format's own tool and holds no crypto of its own.

use std::path::{Path, PathBuf};

use ambassador::{Delegate, delegatable_trait};
use anyhow::{Result, anyhow, bail};
use devkit_common::cmd;
use devkit_config::SignatureFormat;

/// The ssh signature namespace a waiver record is signed under, so a
/// signature made for git or another tool does not count as a waiver.
pub(crate) const SSH_NAMESPACE: &str = "devkit-waiver";

/// A check of a detached signature against a set of trusted keys.
#[delegatable_trait]
pub(crate) trait SignatureVerifier {
    /// The identity of the trusted key that made `signature` over `message`.
    /// Fails when no trusted key made it.
    fn verify(&self, message: &[u8], signature: &Path) -> Result<String>;
}

/// The verifier for each [`SignatureFormat`], holding its trusted keys.
#[derive(Delegate)]
#[delegate(SignatureVerifier)]
pub(crate) enum Verifier {
    Ssh(SshVerifier),
    Openpgp(GpgVerifier),
}

impl Verifier {
    pub(crate) fn new(format: SignatureFormat, trusted: PathBuf) -> Self {
        match format {
            SignatureFormat::Ssh => Self::Ssh(SshVerifier {
                allowed_signers: trusted,
            }),
            SignatureFormat::Openpgp => Self::Openpgp(GpgVerifier { keyring: trusted }),
        }
    }
}

/// `ssh-keygen -Y verify` against an `allowed_signers` file. The identity
/// is the principal the file lists for the signing key.
pub(crate) struct SshVerifier {
    allowed_signers: PathBuf,
}

impl SignatureVerifier for SshVerifier {
    fn verify(&self, message: &[u8], signature: &Path) -> Result<String> {
        let (allowed, signature) = (arg(&self.allowed_signers)?, arg(signature)?);
        let principals = cmd::capture(
            "ssh-keygen",
            &["-Y", "find-principals", "-f", allowed, "-s", signature],
            None,
        )
        .map_err(|e| refused(&e))?;
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
            match cmd::capture_stdin("ssh-keygen", &args, message) {
                Ok(_) => return Ok(principal.to_string()),
                Err(e) => last = refused(&e),
            }
        }
        Err(last)
    }
}

/// `gpgv` against a keyring. The identity is the signing key's user id and
/// primary key fingerprint.
pub(crate) struct GpgVerifier {
    keyring: PathBuf,
}

impl SignatureVerifier for GpgVerifier {
    fn verify(&self, message: &[u8], signature: &Path) -> Result<String> {
        let status = cmd::capture_stdin(
            "gpgv",
            &[
                "--status-fd",
                "1",
                "--keyring",
                arg(&self.keyring)?,
                arg(signature)?,
                "-",
            ],
            message,
        )
        .map_err(|e| refused(&e))?;
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

/// The verifier's own account of a failed check, without the argument list.
fn refused(e: &anyhow::Error) -> anyhow::Error {
    anyhow!(
        "{}",
        cmd::failed_stderr(e).map_or_else(|| format!("{e:#}"), str::to_string)
    )
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
