//! Derive a dev-server signature from an app's `launch` argv, and test whether a
//! running process's command line matches it. Config-driven — no hardcoded
//! framework list beyond the runtime launchers we strip.

/// Runtime launchers stripped from the front of the server command.
const RUNTIMES: &[&str] = &[
    "bun", "bunx", "node", "uv", "uvx", "run", "python", "python3", "poetry", "pipenv",
];

/// The signature tokens for a launch command: the framework word, plus an
/// optional bare subcommand (`dev`/`run`). Empty when nothing meaningful remains.
///
/// Drops the doppler wrapper (everything up to and including the last `--`),
/// then flags, the `{port}` placeholder, and leading runtime launchers.
pub fn signature(launch: &[String]) -> Vec<String> {
    let cmd: &[String] = match launch.iter().rposition(|t| t == "--") {
        Some(i) => &launch[i + 1..],
        None => launch,
    };
    let mut words = cmd.iter().filter(|t| !t.starts_with('-') && *t != "{port}");
    let framework = words.find(|t| !RUNTIMES.contains(&t.as_str()));
    let framework = match framework {
        Some(f) => f.clone(),
        None => return Vec::new(),
    };
    let mut sig = vec![framework];
    if let Some(next) = words.next() {
        // A bare subcommand like `dev`/`run`; exclude path/version-ish tokens.
        if !next.contains('/') && !next.contains(':') && !next.contains('.') {
            sig.push(next.clone());
        }
    }
    sig
}

/// True if every signature token appears as a whole argv word (or path leaf).
pub fn argv_matches(argv: &str, sig: &[String]) -> bool {
    !sig.is_empty() && sig.iter().all(|tok| word_present(argv, tok))
}

fn word_present(argv: &str, tok: &str) -> bool {
    let leaf = format!("/{tok}");
    argv.split_whitespace()
        .any(|w| w == tok || w.ends_with(&leaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn nitro_signature_keeps_framework_and_dev() {
        let launch = v(&[
            "doppler",
            "run",
            "-p",
            "api-foundry",
            "-c",
            "dev_local",
            "--",
            "bun",
            "nitro",
            "dev",
            "--port",
            "{port}",
        ]);
        assert_eq!(signature(&launch), v(&["nitro", "dev"]));
    }

    #[test]
    fn uvicorn_signature_is_framework_only() {
        let launch = v(&[
            "doppler",
            "run",
            "-p",
            "plate-api",
            "-c",
            "dev_local",
            "--",
            "uv",
            "run",
            "uvicorn",
            "server.main:create_app",
            "--factory",
            "--reload",
            "--port",
            "{port}",
        ]);
        assert_eq!(signature(&launch), v(&["uvicorn"]));
    }

    #[test]
    fn vite_signature_drops_port_placeholder() {
        let launch = v(&[
            "doppler",
            "run",
            "-p",
            "monorepo",
            "-c",
            "dev_local",
            "--",
            "bun",
            "vite",
            "--port",
            "{port}",
        ]);
        assert_eq!(signature(&launch), v(&["vite"]));
    }

    #[test]
    fn flask_signature_is_framework_only() {
        let launch = v(&[
            "doppler",
            "run",
            "-p",
            "ada-printer",
            "-c",
            "dev_local",
            "--",
            "uv",
            "run",
            "flask",
            "--app",
            "src/server.py",
            "run",
            "--port",
            "{port}",
        ]);
        assert_eq!(signature(&launch), v(&["flask"]));
    }

    #[test]
    fn matches_bun_and_node_forms() {
        let sig = v(&["nitro", "dev"]);
        assert!(argv_matches("bun nitro dev --port 9200", &sig));
        assert!(argv_matches(
            "node /home/u/app/node_modules/.bin/nitro dev --port 9200",
            &sig
        ));
    }

    #[test]
    fn does_not_match_unrelated_or_partial() {
        let sig = v(&["vite"]);
        assert!(!argv_matches("bun vitest run", &sig)); // vitest != vite
        assert!(!argv_matches("doppler run -- bun test", &sig));
    }
}
