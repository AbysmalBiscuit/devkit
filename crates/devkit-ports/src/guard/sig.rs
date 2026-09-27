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
    // Words after `--` go to whatever the command hands them to, not a verb.
    let (options, _) = split_trailing(&config_argv[cut..]);
    let bare_after = options
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

/// Stands in for config text whose rendered width is unknown, so [`signature`]
/// truncates there. It is spelled as minijinja because [`is_template`] is what
/// does the truncating; any spelling without `{{` would silently stop working.
pub const OPAQUE: &str = "{{ opaque }}";

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

/// Whether `typed`, already matching `sig`, asks for nothing `config_argv`
/// does not do: each word it adds before `--` appears in the config's, and its
/// words after `--` equal the config's. A config with nothing or a template
/// past its signature leaves the rest open.
pub fn within(config_argv: &[String], sig: &[String], typed: &[String]) -> bool {
    let tail = &config_argv[sig.len()..];
    if tail.is_empty() || tail.iter().any(|w| is_template(w)) {
        return true;
    }
    let (options, trailing) = split_trailing(tail);
    let (typed_options, typed_trailing) = split_trailing(&typed[sig.len()..]);
    typed_options.iter().all(|w| options.contains(w))
        && typed_trailing.is_none_or(|t| trailing == Some(t))
}

/// How many words past `sig` in `config_argv` the typed command leaves out.
pub fn omitted(config_argv: &[String], sig: &[String], typed: &[String]) -> usize {
    let typed_rest = &typed[sig.len()..];
    config_argv[sig.len()..]
        .iter()
        .filter(|w| !typed_rest.contains(w))
        .count()
}

/// `words` split at the first `--` into what comes before it and, when there
/// is one, what comes after it.
fn split_trailing(words: &[String]) -> (&[String], Option<&[String]>) {
    match words.iter().position(|w| w == "--") {
        Some(i) => (&words[..i], Some(&words[i + 1..])),
        None => (words, None),
    }
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

    #[test]
    fn bare_words_after_a_double_dash_keep_the_signature() {
        assert_eq!(
            sig(&["cargo", "clippy", "--workspace", "--", "-D", "warnings"]),
            Some(v(&["cargo", "clippy"]))
        );
    }

    fn is_within(config: &[&str], typed: &[&str]) -> bool {
        let config = v(config);
        within(&config, &signature(&config).unwrap(), &v(typed))
    }

    #[test]
    fn a_typed_command_is_within_a_config_that_does_everything_it_asks() {
        let build = ["cargo", "build", "--workspace", "--locked"];
        assert!(is_within(&build, &["cargo", "build"]));
        assert!(is_within(&build, &[
            "cargo",
            "build",
            "--locked",
            "--workspace"
        ]));
        assert!(!is_within(&build, &["cargo", "build", "-p", "x"]));
        assert!(!is_within(&build, &["cargo", "build", "--release"]));
    }

    #[test]
    fn words_after_a_double_dash_compare_as_one_group() {
        let lint = ["cargo", "clippy", "--workspace", "--", "-D", "warnings"];
        assert!(is_within(&lint, &[
            "cargo", "clippy", "--", "-D", "warnings"
        ]));
        assert!(is_within(&lint, &["cargo", "clippy", "--workspace"]));
        assert!(!is_within(&lint, &["cargo", "clippy", "--", "-D"]));
        assert!(!is_within(&lint, &[
            "cargo", "clippy", "--", "warnings", "-D"
        ]));
        assert!(!is_within(&lint, &["cargo", "clippy", "-D", "warnings"]));
    }

    #[test]
    fn a_config_with_nothing_or_a_template_past_its_signature_leaves_the_rest_open() {
        assert!(is_within(&["vite"], &["vite", "build"]));
        assert!(is_within(&["nitro", "dev", "--port", "{{ port }}"], &[
            "nitro", "dev", "--host", "0.0.0.0"
        ]));
    }
}
