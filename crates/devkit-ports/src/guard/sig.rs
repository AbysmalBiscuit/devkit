//! Reduce a config argv to the fixed prefix a human would retype, and test a
//! typed command against it.

use super::norm::basename;

/// Programs whose every invocation looks alike from the outside. A one-token
/// signature naming one of these would match anything they run. `curl` earns
/// its place the same way `python -m {{ module }}` does: the identity of the
/// call sits entirely in an argument (the URL), not in the program name.
const GENERIC: [&str; 12] = [
    "python", "python3", "node", "bun", "deno", "docker", "cargo", "go", "uv", "sh", "bash", "curl",
];

/// The part of a config argv that a typed command can be expected to reproduce:
/// the command word plus its leading positionals.
///
/// Truncation stops at the first minijinja template *or* the first flag,
/// whichever comes first. Stopping at the template alone assumes it sits last,
/// and it usually does not.
///
/// Two rejections keep that from over-firing. A bare positional surviving after
/// the cut means the launch carries a verb the signature does not, so matching
/// the prefix would deny every sibling verb: `["docker", "compose"]` out of
/// `docker compose -p x up` would deny `docker compose down`. And a lone
/// generic interpreter matches everything it runs, so `["python"]` out of
/// `python -m {{ module }}` would deny `python -m pytest`.
pub fn signature(config_argv: &[String]) -> Option<Vec<String>> {
    let cut = config_argv
        .iter()
        .position(|w| is_template(w) || w.starts_with('-'))
        .unwrap_or(config_argv.len());
    let sig = &config_argv[..cut];
    if sig.is_empty() {
        return None;
    }
    let bare_after = config_argv[cut..]
        .iter()
        .any(|w| !w.starts_with('-') && !is_template(w));
    if bare_after {
        return None;
    }
    if sig.len() == 1 && GENERIC.contains(&basename(&sig[0])) {
        return None;
    }
    Some(sig.to_vec())
}

/// Whether a word carries minijinja that renders to something the typed command
/// cannot be expected to reproduce.
fn is_template(word: &str) -> bool {
    word.contains("{{") || word.contains("{%") || word.contains("ports[")
}

/// Whether `typed` starts with `sig`. The command word compares by basename;
/// every later word compares exactly.
pub fn matches(sig: &[String], typed: &[String]) -> bool {
    if sig.is_empty() || typed.len() < sig.len() {
        return false;
    }
    basename(&typed[0]) == basename(&sig[0]) && sig[1..] == typed[1..sig.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    fn sig(words: &[&str]) -> Option<Vec<String>> {
        signature(&v(words))
    }

    #[test]
    fn a_trailing_template_is_dropped() {
        assert_eq!(
            sig(&["nitro", "dev", "--port", "{{ port }}"]),
            Some(v(&["nitro", "dev"]))
        );
    }

    #[test]
    fn truncation_stops_at_the_first_flag() {
        assert_eq!(
            sig(&["uvicorn", "app:app", "--reload"]),
            Some(v(&["uvicorn", "app:app"]))
        );
    }

    #[test]
    fn a_bare_positional_after_the_cut_rejects_the_signature() {
        // `["docker", "compose"]` is a prefix of `docker compose down`, so
        // matching on it would deny every sibling verb.
        assert_eq!(sig(&["docker", "compose", "-p", "{{ p }}", "up"]), None);
    }

    #[test]
    fn a_generic_interpreter_alone_rejects_the_signature() {
        assert_eq!(sig(&["python", "-m", "{{ module }}"]), None);
        assert_eq!(sig(&["node", "--enable-source-maps", "{{ entry }}"]), None);
    }

    #[test]
    fn a_one_token_signature_survives_when_the_token_is_specific() {
        assert_eq!(sig(&["dev"]), Some(v(&["dev"])));
        // An app's `bun run dev -- --port {{ port }}`, after runner stripping.
        assert_eq!(
            sig(&["dev", "--", "--port", "{{ port }}"]),
            Some(v(&["dev"]))
        );
    }

    #[test]
    fn a_catalog_program_still_reduces_and_is_ranked_later() {
        // The catalog, not this signature, decides whether `vite build` is a
        // server. Reduction only has to avoid panicking on it.
        assert_eq!(sig(&["vite", "--port", "{{ port }}"]), Some(v(&["vite"])));
    }

    #[test]
    fn a_port_lookup_counts_as_a_template() {
        assert_eq!(sig(&["curl", "ports['api']"]), None);
    }

    #[test]
    fn a_typed_command_matches_a_prefix_signature() {
        assert!(matches(&v(&["nitro", "dev"]), &v(&["nitro", "dev"])));
        assert!(matches(
            &v(&["nitro", "dev"]),
            &v(&["nitro", "dev", "--port", "3000"])
        ));
        assert!(!matches(&v(&["nitro", "dev"]), &v(&["nitro", "build"])));
        assert!(!matches(&v(&["nitro", "dev"]), &v(&["nitro"])));
    }

    #[test]
    fn the_command_word_matches_by_basename() {
        assert!(matches(&v(&["vite"]), &v(&["./node_modules/.bin/vite"])));
    }
}
