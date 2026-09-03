//! Reduce a lexed segment to the argv a shell would actually exec, plus the
//! doppler wrapper that reduction removed.

/// Wrappers that exec their remaining argv unchanged.
const PROCESS_WRAPPERS: [&str; 4] = ["nohup", "setsid", "exec", "time"];

/// Runner prefixes, longest first so `bun run` is tried before `bun`.
const RUNNERS: [&[&str]; 6] = [
    &["bun", "run"],
    &["pnpm", "exec"],
    &["uv", "run"],
    &["bunx"],
    &["npx"],
    &["uvx"],
];

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
    let mut argv: Vec<String> = words.to_vec();
    let mut doppler = None;

    loop {
        strip_assignments(&mut argv);
        let before = argv.len();
        strip_process_wrappers(&mut argv);
        if let Some(d) = strip_doppler(&mut argv) {
            doppler = Some(d);
        }
        strip_runner(&mut argv);
        if argv.len() == before {
            break;
        }
    }
    (!argv.is_empty()).then_some(Normalized { argv, doppler })
}

fn strip_assignments(argv: &mut Vec<String>) {
    if argv.first().is_some_and(|w| basename(w) == "env") {
        argv.remove(0);
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
        } else if first == "timeout" && argv.len() > 1 {
            argv.drain(..2);
        } else {
            break;
        }
    }
}

fn strip_runner(argv: &mut Vec<String>) {
    for runner in RUNNERS {
        let matches = argv.len() > runner.len()
            && argv
                .iter()
                .zip(runner)
                .enumerate()
                .all(|(i, (w, r))| if i == 0 { basename(w) == *r } else { w == r });
        if matches {
            argv.drain(..runner.len());
            return;
        }
    }
}

/// Remove a `doppler run … --` wrapper and report its `(config, project)`. A
/// `doppler run` with no `--` separator is left in place: it names no inner
/// command, so there is nothing to unwrap.
fn strip_doppler(argv: &mut Vec<String>) -> Option<Doppler> {
    if argv.len() < 2 || basename(&argv[0]) != "doppler" || argv[1] != "run" {
        return None;
    }
    let sep = argv.iter().position(|w| w == "--")?;
    let mut d = Doppler::default();
    let mut i = 2;
    while i < sep {
        let (flag, value) = (argv[i].as_str(), argv.get(i + 1).cloned());
        match flag {
            "-c" | "--config" => d.config = value,
            "-p" | "--project" => d.project = value,
            _ => {}
        }
        i += if flag.starts_with('-') { 2 } else { 1 };
    }
    argv.drain(..=sep);
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
}
