//! Hex digests of text, under a policy each caller names.
//!
//! The two policies give different guarantees. A [`Policy::Shared`] digest is
//! the same on every machine, so a reader anywhere can count or match texts,
//! but anyone can hash a guess and compare it. A [`Policy::Machine`] digest
//! needs this machine's key, so a guess at a low-entropy secret cannot be
//! checked against the digest offline.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};

const KEY_LEN: usize = 32;

/// How [`of`] digests its text.
pub enum Policy<'a> {
    /// SHA-256, prefixed `sha256:`. Stable across runs and machines. Use it
    /// only for text whose guesses are not a secret.
    Shared,
    /// HMAC-SHA-256 under this machine's key, prefixed `hmac-sha256:`. Use it
    /// for text that may hold a secret.
    Machine(&'a Key),
}

/// The hex digest of `text` under `policy`.
pub fn of(text: &str, policy: Policy<'_>) -> String {
    let (prefix, bytes) = match policy {
        Policy::Shared => (
            "sha256:",
            ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
                .as_ref()
                .to_vec(),
        ),
        Policy::Machine(key) => (
            "hmac-sha256:",
            hmac::sign(&key.0, text.as_bytes()).as_ref().to_vec(),
        ),
    };
    let mut out = String::from(prefix);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// An HMAC-SHA-256 key read from a file only its owner can read, created by
/// [`crate::sys::create_private`].
pub struct Key(hmac::Key);

impl Key {
    /// The key in devkit's state directory.
    pub fn live() -> Result<Self> {
        Self::at(&crate::paths::state_dir()?.join("digest.key"))
    }

    /// The key at `path`, created with fresh random bytes when no file is
    /// there. A file of the wrong length is an error, never replaced.
    pub fn at(path: &Path) -> Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => create(path)?,
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        anyhow::ensure!(
            bytes.len() == KEY_LEN,
            "{} holds {} bytes, not a {KEY_LEN}-byte key",
            path.display(),
            bytes.len()
        );
        Ok(Self(hmac::Key::new(hmac::HMAC_SHA256, &bytes)))
    }
}

/// Write fresh key bytes to `path` and return the bytes the file then holds.
/// The temporary file is private from creation, and a concurrent writer that
/// renames first wins.
fn create(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; KEY_LEN];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system random source failed"))?;
    let dir: PathBuf = path.parent().context("a key path has a parent")?.into();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut tmp = tempfile::Builder::new()
        .make_in(&dir, crate::sys::create_private)
        .with_context(|| format!("creating a key in {}", dir.display()))?;
    tmp.write_all(&bytes)?;
    match tmp.persist_noclobber(path) {
        Ok(_) => Ok(bytes),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::read(path).with_context(|| format!("reading {}", path.display()))
        }
        Err(e) => Err(e.error).with_context(|| format!("writing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned against the published SHA-256 vector, so a shared digest that
    /// changed algorithm would not quietly re-key an accumulated corpus.
    #[test]
    fn the_shared_digest_is_sha256_of_the_utf8_bytes() {
        assert_eq!(
            of("abc", Policy::Shared),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_key_is_created_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("digest.key");
        let digest = |text| of(text, Policy::Machine(&Key::at(&path).unwrap()));
        let first = digest("text");
        assert!(first.starts_with("hmac-sha256:"), "{first}");
        assert_eq!(digest("text"), first);
        assert_ne!(digest("other"), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// A key file under a directory everyone can read grants access to its
    /// owner alone, with nothing inherited from the directory.
    #[cfg(windows)]
    #[test]
    fn a_key_under_a_directory_everyone_reads_is_private() {
        let icacls = |args: &[&std::ffi::OsStr]| {
            let out = std::process::Command::new("icacls")
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let dir = tempfile::tempdir().unwrap();
        icacls(&[
            dir.path().as_os_str(),
            "/grant".as_ref(),
            "*S-1-1-0:(OI)(CI)R".as_ref(),
        ]);
        let path = dir.path().join("digest.key");
        Key::at(&path).unwrap();
        let listing = icacls(&[path.as_os_str()]);
        let aces: Vec<&str> = listing
            .lines()
            .take_while(|l| !l.trim().is_empty())
            .map(|l| l.trim().trim_start_matches(path.to_str().unwrap()).trim())
            .collect();
        assert_eq!(aces.len(), 1, "{listing}");
        assert!(!aces[0].contains("(I)"), "{listing}");
    }

    #[test]
    fn another_machines_key_gives_another_digest() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let digest = |dir: &Path| of("text", Policy::Machine(&Key::at(&dir.join("k")).unwrap()));
        assert_ne!(digest(a.path()), digest(b.path()));
    }

    #[test]
    fn a_key_file_of_the_wrong_length_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("k");
        std::fs::write(&path, b"short").unwrap();
        assert!(Key::at(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
    }
}
