//! A per-machine key for digests of text that may hold secrets. A plain hash
//! of a low-entropy secret can be guessed offline; an HMAC under a key that
//! never leaves this machine cannot be guessed from the digest alone.

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

/// An HMAC-SHA-256 key read from a file only its owner can read.
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

    /// The hex HMAC of `text`, prefixed `hmac-sha256:`.
    pub fn digest(&self, text: &str) -> String {
        let tag = hmac::sign(&self.0, text.as_bytes());
        let mut out = String::from("hmac-sha256:");
        for b in tag.as_ref() {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }
}

/// Write fresh key bytes to `path` and return the bytes the file then holds.
/// The temporary file is readable only by its owner, and a concurrent writer
/// that renames first wins.
fn create(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; KEY_LEN];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the system random source failed"))?;
    let dir: PathBuf = path.parent().context("a key path has a parent")?.into();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir)
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

    #[test]
    fn a_key_is_created_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state").join("digest.key");
        let first = Key::at(&path).unwrap().digest("text");
        assert!(first.starts_with("hmac-sha256:"), "{first}");
        assert_eq!(Key::at(&path).unwrap().digest("text"), first);
        assert_ne!(Key::at(&path).unwrap().digest("other"), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn another_machines_key_gives_another_digest() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let digest = |dir: &Path| Key::at(&dir.join("k")).unwrap().digest("text");
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
