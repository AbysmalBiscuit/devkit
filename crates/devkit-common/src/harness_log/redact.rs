//! Redaction over a logged command.
//!
//! **Best effort.** This matches the environment variable names devkit itself
//! resolves credentials from and the token shapes of the services it talks to.
//! It will miss a novel format, a credential passed under a name devkit does
//! not know, and anything a command reads from a file. `redacted` is a
//! reduction in exposure, not a guarantee, and a corpus produced under it is
//! not safe to hand to a third party on the strength of this alone.
//!
//! The substitute names the kind, so the command's structure survives for the
//! analyzer corpus: `GH_TOKEN=<redacted:github-token> gh pr list` still parses
//! into the same invocation the original did.

use devkit_config::Fidelity;

/// Variable names devkit resolves a credential from. The first three are what
/// `secrets` stores; `GH_TOKEN` and `GITHUB_TOKEN` are resolved elsewhere and
/// are the two most likely to be typed into a command.
const SECRET_VARS: [&str; 5] = [
    "LINEAR_API_KEY",
    "LINEAR_WORKSPACE",
    "SLACK_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
];

/// Token prefixes, paired with the kind the substitute names.
const TOKEN_PREFIXES: [(&str, &str); 8] = [
    ("github_pat_", "github-token"),
    ("ghp_", "github-token"),
    ("gho_", "github-token"),
    ("ghs_", "github-token"),
    ("xoxb-", "slack-token"),
    ("xoxp-", "slack-token"),
    ("lin_api_", "linear-key"),
    ("sk-", "api-key"),
];

/// Apply `mode` to a command. Returns the text and whether anything was
/// substituted, so a record can say that redaction fired rather than leaving a
/// reader to guess from the text.
pub fn apply(command: &str, mode: Fidelity) -> (String, bool) {
    match mode {
        Fidelity::Full => (command.to_string(), false),
        Fidelity::Hashed => (digest(command), true),
        Fidelity::Redacted => redact(command),
    }
}

/// A stable digest and nothing else. Stable across runs and machines, so a
/// reader can count distinct commands without holding any of their text.
pub fn digest(text: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    let mut out = String::from("sha256:");
    for b in d.as_ref() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn substitute(kind: &str) -> String {
    format!("<redacted:{kind}>")
}

/// Walk the text once, rewriting each run of non-whitespace that either
/// assigns a known variable or opens with a known token prefix. Whitespace is
/// preserved exactly, so a command with nothing to redact comes back
/// byte-identical.
fn redact(command: &str) -> (String, bool) {
    let mut out = String::with_capacity(command.len());
    let mut hit = false;
    let mut rest = command;
    while !rest.is_empty() {
        let space = rest
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(rest.len());
        out.push_str(&rest[..space]);
        rest = &rest[space..];
        if rest.is_empty() {
            break;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        match redact_word(word) {
            Some(replacement) => {
                out.push_str(&replacement);
                hit = true;
            }
            None => out.push_str(word),
        }
        rest = tail;
    }
    (out, hit)
}

/// One whitespace-delimited word, or `None` when it carries nothing known.
fn redact_word(word: &str) -> Option<String> {
    // `NAME=value`, keeping the name so the assignment still reads as one.
    if let Some((name, value)) = word.split_once('=')
        && !value.is_empty()
        && SECRET_VARS.contains(&name)
    {
        let kind = kind_of(&name.to_ascii_lowercase());
        return Some(format!("{name}={}", substitute(kind)));
    }
    // A bare token, however it was passed: an argument, a value after `=`
    // under a name devkit does not know, a quoted literal.
    let trimmed = word.trim_start_matches(['"', '\'', '=']);
    let (kind, _) = TOKEN_PREFIXES
        .iter()
        .find(|(prefix, _)| trimmed.starts_with(prefix))
        .map(|(p, k)| (*k, *p))?;
    let head = &word[..word.len() - trimmed.len()];
    let tail_quote = trimmed
        .rfind(['"', '\''])
        .filter(|i| *i + 1 == trimmed.len())
        .map_or("", |i| &trimmed[i..]);
    Some(format!("{head}{}{tail_quote}", substitute(kind)))
}

fn kind_of(name: &str) -> &'static str {
    match name {
        "gh_token" | "github_token" => "github-token",
        "slack_token" => "slack-token",
        "linear_api_key" => "linear-key",
        _ => "secret",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_with_no_secret_comes_through_byte_identical() {
        let input = "cargo nextest run --workspace --no-fail-fast";
        let (out, hit) = apply(input, Fidelity::Redacted);
        assert_eq!(out, input, "redaction must not disturb ordinary commands");
        assert!(!hit);
    }

    /// The case that matters most after the one above: whitespace is preserved
    /// exactly, so a heredoc or an indented script survives unchanged.
    #[test]
    fn whitespace_and_newlines_survive_untouched() {
        let input = "python3 - <<'EOF'\n  import os\n\n  print(os.getcwd())\nEOF\n";
        let (out, hit) = apply(input, Fidelity::Redacted);
        assert_eq!(out, input);
        assert!(!hit);
    }

    #[test]
    fn a_known_env_assignment_is_substituted_by_kind() {
        let (out, hit) = apply(
            "GH_TOKEN=ghp_abcdefghijklmnop gh pr list",
            Fidelity::Redacted,
        );
        assert!(hit);
        assert!(!out.contains("ghp_abcdefghijklmnop"));
        assert!(
            out.contains("GH_TOKEN="),
            "the command's structure must survive"
        );
        assert!(
            out.contains("github-token"),
            "the substitute names the kind"
        );
        assert!(out.contains("gh pr list"));
    }

    #[test]
    fn every_known_variable_name_is_matched() {
        for name in SECRET_VARS {
            let (out, hit) = apply(
                &format!("{name}=whatever-value-here cmd"),
                Fidelity::Redacted,
            );
            assert!(hit, "{name}");
            assert!(!out.contains("whatever-value-here"), "{name}: {out}");
            assert!(out.contains(name), "{name}: {out}");
        }
    }

    /// A token passed under a name devkit does not know is still a token.
    #[test]
    fn every_known_token_shape_is_matched_wherever_it_sits() {
        for (prefix, kind) in TOKEN_PREFIXES {
            let secret = format!("{prefix}AbC123dEf456");
            let (out, hit) = apply(
                &format!("curl -H \"x: {secret}\" https://x"),
                Fidelity::Redacted,
            );
            assert!(hit, "{prefix}");
            assert!(!out.contains(&secret), "{prefix}: {out}");
            assert!(out.contains(kind), "{prefix}: {out}");
            assert!(out.contains("https://x"), "{prefix}: {out}");
        }
    }

    #[test]
    fn a_bare_assignment_with_no_value_is_left_alone() {
        let input = "GH_TOKEN= gh pr list";
        assert_eq!(apply(input, Fidelity::Redacted).0, input);
    }

    #[test]
    fn hashed_keeps_nothing_but_a_stable_digest() {
        let (a, _) = apply("echo hi", Fidelity::Hashed);
        let (b, _) = apply("echo hi", Fidelity::Hashed);
        let (c, _) = apply("echo ho", Fidelity::Hashed);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(!a.contains("echo"));
    }

    /// Pinned against the published SHA-256 vector, so a digest that silently
    /// changed algorithm would not quietly re-key an accumulated corpus.
    #[test]
    fn the_digest_is_sha256_of_the_utf8_bytes() {
        assert_eq!(
            digest("abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn full_is_a_passthrough() {
        let input = "GH_TOKEN=ghp_secret gh pr list";
        let (out, hit) = apply(input, Fidelity::Full);
        assert_eq!(out, input);
        assert!(!hit);
    }
}
