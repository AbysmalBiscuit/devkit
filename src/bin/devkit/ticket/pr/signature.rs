//! Detached-signature checks for `pr waive-verify --signed`. devkit runs the
//! format's own tool and holds no crypto of its own.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use devkit_common::cmd;
use devkit_config::SignatureFormat;

/// The ssh signature namespace a waiver record is signed under, so a
/// signature made for git or another tool does not count as a waiver.
pub(crate) const SSH_NAMESPACE: &str = "devkit-waiver";

/// Check that `signature` signs `message` with a key in `trusted`, the
/// `allowed_signers` file or keyring that `format` reads.
pub(crate) fn verify(
    format: SignatureFormat,
    trusted: &Path,
    message: &Path,
    signature: &Path,
) -> Result<()> {
    let checked = match format {
        SignatureFormat::Ssh => verify_ssh(trusted, message, signature),
        SignatureFormat::Openpgp => cmd::capture(
            "gpgv",
            &[
                "--keyring",
                &arg(trusted)?,
                &arg(signature)?,
                &arg(message)?,
            ],
            None,
        )
        .map(drop),
    };
    checked.map_err(|e| {
        let why = cmd::failed_stderr(&e).map_or_else(|| format!("{e:#}"), str::to_string);
        anyhow!(
            "the signature `{}` on `{}` does not verify against `{}`: {why}",
            signature.display(),
            message.display(),
            trusted.display()
        )
    })
}

/// `ssh-keygen -Y verify` needs the signer's principal, so each principal
/// that `trusted` lists for the signing key is tried in turn.
fn verify_ssh(trusted: &Path, message: &Path, signature: &Path) -> Result<()> {
    let (trusted, signature) = (arg(trusted)?, arg(signature)?);
    let principals = cmd::capture(
        "ssh-keygen",
        &["-Y", "find-principals", "-f", &trusted, "-s", &signature],
        None,
    )?;
    let mut last = anyhow!("no principal in the trusted keys holds the signing key");
    for principal in principals.lines().map(str::trim).filter(|p| !p.is_empty()) {
        let stdin = std::fs::File::open(message)
            .with_context(|| format!("opening {}", message.display()))?;
        match cmd::capture_stdin(
            "ssh-keygen",
            &[
                "-Y",
                "verify",
                "-f",
                &trusted,
                "-I",
                principal,
                "-n",
                SSH_NAMESPACE,
                "-s",
                &signature,
            ],
            stdin,
        ) {
            Ok(_) => return Ok(()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn arg(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("`{}` is not valid UTF-8", path.display()))
}
