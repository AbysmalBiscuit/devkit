//! Reduce a lexed segment to the argv a shell would actually exec, plus the
//! doppler wrapper that reduction removed.

/// Wrappers that exec their remaining argv unchanged.
const PROCESS_WRAPPERS: [&str; 4] = ["nohup", "setsid", "exec", "time"];

/// Runner prefixes, longest first so `bun run` is tried before `bun`.
const RUNNERS: [&[&str]; 7] = [
    &["bun", "run"],
    &["pnpm", "exec"],
    &["uv", "run"],
    &["npm", "exec"],
    &["bunx"],
    &["npx"],
    &["uvx"],
];

/// Bounds how many `doppler run --command=…` wrappers get unwrapped inside
/// one another. Each level re-lexes and recursively normalizes its value, so
/// a payload that nests this wrapper inside itself grows the call stack by
/// one frame per level; unlike a panic, a stack overflow cannot be caught, so
/// the recursion has to stop on its own before that point regardless of how
/// long the input command string is. The cap sits far above any real doppler
/// wrapping and far below anything that could exhaust the stack.
const MAX_DOPPLER_COMMAND_DEPTH: usize = 8;

/// A doppler wrapper's identity, normalized so `-c dev` and `--config dev`
/// compare equal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Doppler {
    pub config: Option<String>,
    pub project: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Normalized {
    /// The program and its arguments, with every wrapper removed.
    pub argv: Vec<String>,
    /// The doppler wrapper that was removed, if any.
    pub doppler: Option<Doppler>,
}

/// The last path component of a program word. A launch through
/// `./node_modules/.bin/vite` is a `vite` launch; `run::assert_not_prd` already
/// matches `doppler` this way.
pub fn basename(prog: &str) -> &str {
    prog.rsplit(['/', '\\']).next().unwrap_or(prog)
}

/// Strip assignments and wrappers from a lexed segment. `None` when nothing
/// executable remains.
pub fn normalize(words: &[String]) -> Option<Normalized> {
    normalize_at_depth(words, 0)
}

fn normalize_at_depth(words: &[String], depth: usize) -> Option<Normalized> {
    let mut argv: Vec<String> = words.to_vec();
    let mut doppler = None;

    loop {
        let before = argv.len();
        strip_assignments(&mut argv);
        strip_process_wrappers(&mut argv);
        if let Some(d) = strip_doppler(&mut argv, depth) {
            doppler = Some(d);
        }
        strip_runner(&mut argv);
        if argv.len() == before {
            break;
        }
    }
    (!argv.is_empty()).then_some(Normalized { argv, doppler })
}

/// Skip a run of `-`-prefixed flag words at the front of `argv`, without
/// tracking which flags take a following value: a value-taking flag then
/// leaves its value word as the new first word, exactly as an unrecognized
/// bare flag would on its own — the guard finds no rule matching it either
/// way and allows the command through.
fn skip_leading_flags(argv: &mut Vec<String>) {
    while argv.first().is_some_and(|w| w.starts_with('-')) {
        argv.remove(0);
    }
}

fn strip_assignments(argv: &mut Vec<String>) {
    if argv.first().is_some_and(|w| basename(w) == "env") {
        argv.remove(0);
        skip_leading_flags(argv);
    }
    while argv
        .first()
        .is_some_and(|w| w.contains('=') && !w.starts_with('=') && !w.starts_with('-'))
    {
        argv.remove(0);
    }
}

fn strip_process_wrappers(argv: &mut Vec<String>) {
    while let Some(first) = argv.first().map(|w| basename(w).to_string()) {
        if PROCESS_WRAPPERS.contains(&first.as_str()) {
            argv.remove(0);
        } else if first == "timeout" {
            let mut duration_idx = 1;
            while argv.get(duration_idx).is_some_and(|w| w.starts_with('-')) {
                duration_idx += 1;
            }
            if argv.len() > duration_idx {
                argv.drain(..=duration_idx);
            } else {
                break;
            }
        } else {
            break;
        }
    }
}

fn strip_runner(argv: &mut Vec<String>) {
    for runner in RUNNERS {
        let Some((head, tail)) = runner.split_first() else {
            continue;
        };
        if argv.len() > runner.len()
            && basename(&argv[0]) == *head
            && &argv[1..runner.len()] == tail
        {
            argv.drain(..runner.len());
            skip_leading_flags(argv);
            return;
        }
    }
}

/// Parse `-c`/`--config` and `-p`/`--project` from a doppler wrapper's flag
/// words, in either `--flag value` or `--flag=value` form.
fn parse_doppler_flags(words: &[String]) -> Doppler {
    let mut d = Doppler::default();
    let mut i = 0;
    while i < words.len() {
        let word = words[i].as_str();
        let (key, inline) = match word.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (word, None),
        };
        let value = inline
            .map(str::to_string)
            .or_else(|| words.get(i + 1).cloned());
        match key {
            "-c" | "--config" => d.config = value,
            "-p" | "--project" => d.project = value,
            _ => {}
        }
        i += if inline.is_some() || !key.starts_with('-') {
            1
        } else {
            2
        };
    }
    d
}

/// Remove a `doppler run … --` wrapper, or a `doppler run … --command=<cmd>`
/// / `--command <cmd>` wrapper, and report its `(config, project)`. A
/// `--command` value is itself a shell command string; only its first segment
/// is normalized, so a value chaining several commands together reduces to
/// just the first of them. Past `MAX_DOPPLER_COMMAND_DEPTH` levels of nested
/// `--command` wrappers, the value is left unnormalized: the wrapper reaches
/// the guard as an ordinary word list, matches no rule, and is allowed. A
/// `doppler run` with neither form present is left in place: it names no
/// inner command, so there is nothing to unwrap.
fn strip_doppler(argv: &mut Vec<String>, depth: usize) -> Option<Doppler> {
    if argv.len() < 2 || basename(&argv[0]) != "doppler" || argv[1] != "run" {
        return None;
    }
    if let Some(sep) = argv.iter().position(|w| w == "--") {
        let d = parse_doppler_flags(&argv[2..sep]);
        argv.drain(..=sep);
        return Some(d);
    }

    let cmd_idx = argv
        .iter()
        .position(|w| w == "--command" || w.starts_with("--command="))?;
    if cmd_idx < 2 || depth >= MAX_DOPPLER_COMMAND_DEPTH {
        return None;
    }
    let value = match argv[cmd_idx].strip_prefix("--command=") {
        Some(v) => v.to_string(),
        None => argv.get(cmd_idx + 1)?.clone(),
    };
    let d = parse_doppler_flags(&argv[2..cmd_idx]);
    let inner = crate::guard::lex::segments(&value).into_iter().next()?;
    let normalized = normalize_at_depth(&inner, depth + 1)?;
    *argv = normalized.argv;
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &str) -> Vec<String> {
        let seg = crate::guard::lex::segments(cmd).remove(0);
        normalize(&seg).expect("a segment normalizes").argv
    }

    #[test]
    fn leading_assignments_are_stripped() {
        assert_eq!(argv("FOO=1 BAR=2 vite"), vec!["vite"]);
    }

    #[test]
    fn env_is_stripped_with_its_assignments() {
        assert_eq!(argv("env FOO=1 vite"), vec!["vite"]);
    }

    #[test]
    fn process_wrappers_are_stripped() {
        assert_eq!(argv("nohup bun run dev"), vec!["dev"]);
        assert_eq!(argv("timeout 30 vite"), vec!["vite"]);
        assert_eq!(argv("exec setsid vite dev"), vec!["vite", "dev"]);
    }

    #[test]
    fn runner_prefixes_are_stripped() {
        assert_eq!(argv("bunx vite"), vec!["vite"]);
        assert_eq!(argv("bun run vite dev"), vec!["vite", "dev"]);
        assert_eq!(argv("pnpm exec vite"), vec!["vite"]);
        assert_eq!(argv("uv run uvicorn app"), vec!["uvicorn", "app"]);
    }

    #[test]
    fn npm_exec_is_a_runner_but_npm_run_is_not() {
        assert_eq!(argv("npm exec vite"), vec!["vite"]);
        assert_eq!(argv("npm run dev"), vec!["npm", "run", "dev"]);
    }

    #[test]
    fn a_doppler_wrapper_is_stripped_and_recorded() {
        let seg = crate::guard::lex::segments("doppler run -c dev -- bun test").remove(0);
        let n = normalize(&seg).unwrap();
        assert_eq!(n.argv, vec!["bun", "test"]);
        assert_eq!(n.doppler.unwrap().config.as_deref(), Some("dev"));
    }

    #[test]
    fn doppler_config_spellings_normalize_together() {
        let short = crate::guard::lex::segments("doppler run -c dev -- x").remove(0);
        let long = crate::guard::lex::segments("doppler run --config dev -- x").remove(0);
        assert_eq!(
            normalize(&short).unwrap().doppler,
            normalize(&long).unwrap().doppler
        );
    }

    #[test]
    fn a_doppler_run_without_a_separator_is_left_alone() {
        assert_eq!(
            argv("doppler run -c dev"),
            vec!["doppler", "run", "-c", "dev"]
        );
    }

    #[test]
    fn the_command_word_compares_by_basename() {
        assert_eq!(basename("./node_modules/.bin/vite"), "vite");
        assert_eq!(basename("vite"), "vite");
    }

    #[test]
    fn a_segment_of_only_assignments_normalizes_to_nothing() {
        let seg = crate::guard::lex::segments("FOO=1").remove(0);
        assert!(normalize(&seg).is_none());
    }

    #[test]
    fn a_newly_exposed_env_is_stripped_in_the_same_normalization() {
        assert_eq!(argv("FOO=1 env BAR=2 vite"), vec!["vite"]);
    }

    #[test]
    fn flags_between_a_wrapper_and_its_command_are_skipped() {
        assert_eq!(argv("env -i FOO=1 vite"), vec!["vite"]);
        assert_eq!(argv("timeout --foreground 30 vite"), vec!["vite"]);
        assert_eq!(argv("npx --yes vite"), vec!["vite"]);
        assert_eq!(argv("bunx --bun vite"), vec!["vite"]);
    }

    #[test]
    fn a_doppler_command_flag_is_stripped_and_recorded() {
        let seg =
            crate::guard::lex::segments("doppler run -c dev --command=\"bun test\"").remove(0);
        let n = normalize(&seg).unwrap();
        assert_eq!(n.argv, vec!["bun", "test"]);
        assert_eq!(n.doppler.unwrap().config.as_deref(), Some("dev"));

        let seg = crate::guard::lex::segments("doppler run --command vite").remove(0);
        let n = normalize(&seg).unwrap();
        assert_eq!(n.argv, vec!["vite"]);
    }

    #[test]
    fn doppler_metadata_handles_equals_form_and_a_trailing_flag() {
        let eq = crate::guard::lex::segments("doppler run --config=dev -- vite").remove(0);
        assert_eq!(
            normalize(&eq).unwrap().doppler.unwrap().config.as_deref(),
            Some("dev")
        );

        let dangling = crate::guard::lex::segments("doppler run -c -- vite").remove(0);
        assert_eq!(normalize(&dangling).unwrap().doppler.unwrap().config, None);
    }

    /// Double-quotes `s` for embedding as one shell word, escaping the
    /// characters the lexer's double-quote handling unescapes on the way
    /// back in.
    fn dquote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
        out
    }

    /// A command string nesting `doppler run --command=…` inside itself
    /// `levels` deep around a plain `vite` at the center.
    fn nested_doppler_command(levels: usize) -> String {
        let mut cmd = "vite".to_string();
        for _ in 0..levels {
            cmd = format!("doppler run --command={}", dquote(&cmd));
        }
        cmd
    }

    #[test]
    fn doppler_command_nesting_past_the_depth_cap_terminates_unnormalized() {
        let seg =
            crate::guard::lex::segments(&nested_doppler_command(MAX_DOPPLER_COMMAND_DEPTH + 3))
                .remove(0);
        let n = normalize(&seg).expect("the outermost wrapper still normalizes");
        assert_eq!(n.argv[0], "doppler");
        assert_eq!(n.argv[1], "run");
        assert!(n.argv[2].starts_with("--command="));
    }
}
