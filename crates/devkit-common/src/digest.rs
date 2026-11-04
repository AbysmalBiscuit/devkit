//! The SHA-256 hex digest of text, the same on every machine.
//!
//! Anyone can hash a guess and compare it with a digest, so a digest of text
//! that may hold a secret stays in local state and never leaves the machine.

/// The SHA-256 of `text`'s UTF-8 bytes, as `sha256:<hex>`.
pub fn sha256(text: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    let mut out = String::from("sha256:");
    for b in d.as_ref() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned against the published SHA-256 vector, so a digest that changed
    /// algorithm would not quietly re-key receipts and hashed log records.
    #[test]
    fn the_digest_is_sha256_of_the_utf8_bytes() {
        assert_eq!(
            sha256("abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
