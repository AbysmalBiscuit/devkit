//! Ecosystem knowledge: which invocations of which programs start a long-lived
//! dev server. Extended by pull request, never by config — a project adding a
//! refusal of its own writes `[harness.commands.<name>]`.

use super::norm::basename;

/// Frameworks whose server is the `dev` subcommand and nothing else.
const DEV_SUBCOMMAND: [&str; 4] = ["next", "nitro", "wrangler", "mintlify"];

/// Words that mean a vite invocation builds rather than serves. These are
/// tested wherever they appear in the argv because a flag's value can precede
/// the verb. The cost of this looseness falls the safe way: a flag value that
/// happens to equal one of these words makes the guard allow rather than deny.
const VITE_NON_SERVER: [&str; 3] = ["build", "preview", "optimize"];

/// Flags that make a verbless invocation print and exit rather than serve.
/// Any other flag leaves the verdict to the verb, so `vite --port 3000` is a
/// server.
const INFO_FLAGS: [&str; 4] = ["--version", "-v", "--help", "-h"];

/// Whether the guard has an opinion about this program at all.
pub fn is_known_program(prog: &str) -> bool {
    let p = basename(prog);
    DEV_SUBCOMMAND.contains(&p) || matches!(p, "uvicorn" | "flask" | "vite")
}

/// Whether this argv starts a dev server.
pub fn is_dev_server(argv: &[String]) -> bool {
    let Some(prog) = argv.first().map(|p| basename(p)) else {
        return false;
    };
    let rest: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
    let first_verb = rest.iter().find(|a| !a.starts_with('-'));

    if DEV_SUBCOMMAND.contains(&prog) {
        return first_verb == Some(&"dev");
    }
    match prog {
        "uvicorn" => true,
        "flask" => rest.contains(&"run"),
        "vite" => {
            if rest.iter().any(|a| INFO_FLAGS.contains(a)) {
                return false;
            }
            !rest.iter().any(|a| VITE_NON_SERVER.contains(a))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(cmd: &str) -> bool {
        let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        is_dev_server(&argv)
    }

    #[test]
    fn dev_subcommand_frameworks() {
        assert!(server("next dev"));
        assert!(server("nitro dev"));
        assert!(server("wrangler dev"));
        assert!(server("mintlify dev"));
        assert!(!server("next build"));
        assert!(!server("next"));
    }

    #[test]
    fn uvicorn_is_a_server_bare() {
        assert!(server("uvicorn app:app"));
        assert!(server("uvicorn"));
    }

    #[test]
    fn flask_needs_its_run_verb_anywhere() {
        assert!(server("flask run"));
        assert!(server("flask --app x run"));
        assert!(!server("flask shell"));
    }

    #[test]
    fn vite_serves_on_three_verbs_and_no_others() {
        assert!(server("vite"));
        assert!(server("vite dev"));
        assert!(server("vite serve"));
        assert!(!server("vite build"));
        assert!(!server("vite preview"));
        assert!(!server("vite optimize"));
        assert!(!server("vite --version"));
        assert!(!server("vite -h"));
        // A non-info flag and its value carry no non-server word, so the invocation serves.
        assert!(server("vite --port 3000"));
        assert!(!server("vite build --minify"));
        assert!(!server("vite --config vite.config.ts build"));
    }

    #[test]
    fn a_catalog_program_is_recognised_by_basename() {
        assert!(is_known_program("./node_modules/.bin/vite"));
        assert!(!is_known_program("cargo"));
    }
}
