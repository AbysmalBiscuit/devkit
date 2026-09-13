#![allow(dead_code)]

use crate::{
    analyzer::Stdin,
    model::{Language, Value},
};

pub(crate) enum Exec {
    Source {
        language: Language,
        source: Value,
        script_args: Vec<Value>,
    },
    ScriptFile {
        script: Value,
    },
    Unsupported {
        language: &'static str,
    },
    Plain,
}

fn program(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

fn is_python(name: &str) -> bool {
    name == "py"
        || name
            .strip_prefix("python")
            .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

pub(crate) fn is_interpreter(name: &str) -> bool {
    let name = program(name);
    is_python(&name)
        || matches!(
            name.as_str(),
            "node"
                | "bun"
                | "deno"
                | "tsx"
                | "ts-node"
                | "bash"
                | "sh"
                | "dash"
                | "ksh"
                | "zsh"
                | "fish"
                | "pwsh"
                | "powershell"
                | "eval"
                | "perl"
                | "ruby"
                | "php"
                | "lua"
                | "nu"
                | "awk"
                | "gawk"
                | "mawk"
                | "osascript"
                | "rscript"
                | "julia"
                | "cmd"
        )
}

fn stdin_source(stdin: &Stdin) -> Option<Value> {
    match stdin {
        Stdin::Source { value, .. } => Some(value.clone()),
        Stdin::None => None,
    }
}

pub(crate) fn classify(name: &str, args: &[Value], stdin: &Stdin) -> Exec {
    let name = program(name);
    match name.as_str() {
        n if is_python(n) => python(args, stdin),
        "node" => eval_style(
            args,
            stdin,
            Language::JavaScript,
            &["-e", "--eval", "-p", "--print"],
            &["-r", "--require", "--import", "--loader"],
            1,
        ),
        "tsx" | "ts-node" => eval_style(
            args,
            stdin,
            Language::TypeScript,
            &["-e", "--eval", "-p", "--print"],
            &["-r", "--require"],
            1,
        ),
        "bun" => eval_style(
            args,
            stdin,
            Language::TypeScript,
            &["-e", "--eval", "-p", "--print"],
            &["--cwd", "-r", "--preload"],
            1,
        ),
        "deno" => match args.first().and_then(Value::known) {
            Some("eval") => Exec::Source {
                language: Language::TypeScript,
                source: args.get(1).cloned().unwrap_or(Value::Unknown),
                script_args: Vec::new(),
            },
            Some("run") => args
                .iter()
                .skip(1)
                .find(|a| a.known().is_none_or(|t| !t.starts_with('-')))
                .map_or(Exec::Plain, |s| Exec::ScriptFile { script: s.clone() }),
            _ => Exec::Plain,
        },
        "bash" | "sh" | "dash" | "ksh" => shell(args, stdin, Language::Bash),
        "fish" => shell(args, stdin, Language::Fish),
        "zsh" => match shell(args, stdin, Language::Bash) {
            Exec::Source { .. } => Exec::Unsupported { language: "zsh" },
            other => other,
        },
        "pwsh" | "powershell" => powershell(args, stdin),
        "eval" => {
            let words: Option<Vec<&str>> = args.iter().map(Value::known).collect();
            Exec::Source {
                language: Language::Bash,
                source: words.map_or(Value::Unknown, |w| Value::Known(w.join(" "))),
                script_args: Vec::new(),
            }
        }
        "perl" => {
            let in_place = args
                .iter()
                .filter_map(Value::known)
                .take_while(|a| a.starts_with('-'))
                .any(|a| !a.starts_with("--") && a[1..].contains('i'));
            if in_place {
                Exec::Plain
            } else {
                unsupported_or_script(args, "perl", &["-e", "-E"])
            }
        }
        "ruby" => unsupported_or_script(args, "ruby", &["-e"]),
        "php" => unsupported_or_script(args, "php", &["-r"]),
        "lua" => unsupported_or_script(args, "lua", &["-e"]),
        "rscript" => unsupported_or_script(args, "R", &["-e"]),
        "julia" => unsupported_or_script(args, "julia", &["-e", "--eval"]),
        "osascript" => unsupported_or_script(args, "AppleScript", &["-e"]),
        "nu" => unsupported_or_script(args, "nushell", &["-c", "--commands"]),
        "cmd" => Exec::Unsupported { language: "cmd" },
        "awk" | "gawk" | "mawk" => {
            let mut words = args.iter();
            while let Some(w) = words.next() {
                match w.known() {
                    Some("-f") => {
                        return words
                            .next()
                            .map_or(Exec::Plain, |s| Exec::ScriptFile { script: s.clone() });
                    }
                    Some("-v" | "-F") => {
                        words.next();
                    }
                    Some(t) if t.starts_with('-') => {}
                    _ => return Exec::Unsupported { language: "awk" },
                }
            }
            Exec::Plain
        }
        _ => Exec::Plain,
    }
}

fn python(args: &[Value], stdin: &Stdin) -> Exec {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            return Exec::ScriptFile {
                script: Value::Unknown,
            };
        };
        match text {
            "-c" => {
                return Exec::Source {
                    language: Language::Python,
                    source: args.get(i + 1).cloned().unwrap_or(Value::Unknown),
                    script_args: std::iter::once(Value::Known("-c".into()))
                        .chain(args.iter().skip(i + 2).cloned())
                        .collect(),
                };
            }
            "-m" => return Exec::Plain,
            "-" => {
                return Exec::Source {
                    language: Language::Python,
                    source: stdin_source(stdin).unwrap_or(Value::Unknown),
                    script_args: args[i..].to_vec(),
                };
            }
            "-W" | "-X" | "--check-hash-based-pycs" => i += 2,
            t if t.starts_with('-') => i += 1,
            _ => {
                return Exec::ScriptFile {
                    script: arg.clone(),
                };
            }
        }
    }
    match stdin_source(stdin) {
        Some(source) => Exec::Source {
            language: Language::Python,
            source,
            script_args: vec![Value::Known(String::new())],
        },
        None => Exec::Plain,
    }
}

/// `node`, `bun`, `tsx`: an evaluation flag carries source; the words after it
/// are the script's arguments, preceded by `offset` runtime entries of unknown
/// value in `process.argv`.
fn eval_style(
    args: &[Value],
    stdin: &Stdin,
    language: Language,
    eval_flags: &[&str],
    value_flags: &[&str],
    offset: usize,
) -> Exec {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            return Exec::ScriptFile {
                script: Value::Unknown,
            };
        };
        if eval_flags.contains(&text) {
            return Exec::Source {
                language,
                source: args.get(i + 1).cloned().unwrap_or(Value::Unknown),
                script_args: std::iter::repeat_n(Value::Unknown, offset)
                    .chain(args.iter().skip(i + 2).cloned())
                    .collect(),
            };
        }
        match text {
            "-" => {
                return Exec::Source {
                    language,
                    source: stdin_source(stdin).unwrap_or(Value::Unknown),
                    script_args: std::iter::repeat_n(Value::Unknown, offset)
                        .chain(args.iter().skip(i + 1).cloned())
                        .collect(),
                };
            }
            t if value_flags.contains(&t) => i += 2,
            t if t.starts_with('-') => i += 1,
            t if has_script_extension(t) => {
                return Exec::ScriptFile {
                    script: arg.clone(),
                };
            }
            _ => return Exec::Plain,
        }
    }
    Exec::Plain
}

fn has_script_extension(word: &str) -> bool {
    [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts", ".tsx", ".jsx"]
        .iter()
        .any(|e| word.ends_with(e))
}

fn shell(args: &[Value], stdin: &Stdin, language: Language) -> Exec {
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            return Exec::ScriptFile {
                script: Value::Unknown,
            };
        };
        if text.starts_with('-') && !text.starts_with("--") && text[1..].contains('c') {
            return Exec::Source {
                language,
                source: args.get(i + 1).cloned().unwrap_or(Value::Unknown),
                script_args: args.iter().skip(i + 2).cloned().collect(),
            };
        }
        match text {
            "-s" => {
                return Exec::Source {
                    language,
                    source: stdin_source(stdin).unwrap_or(Value::Unknown),
                    script_args: args[i + 1..].to_vec(),
                };
            }
            "-o" | "+o" | "--init-file" | "--rcfile" => i += 2,
            t if t.starts_with('-') || t.starts_with('+') => i += 1,
            _ => {
                return Exec::ScriptFile {
                    script: arg.clone(),
                };
            }
        }
    }
    match stdin_source(stdin) {
        Some(source) => Exec::Source {
            language,
            source,
            script_args: Vec::new(),
        },
        None => Exec::Plain,
    }
}

/// PowerShell parameters are case-insensitive and accept unambiguous
/// prefixes; only the ones that change how source is supplied are matched.
fn powershell(args: &[Value], stdin: &Stdin) -> Exec {
    const VALUE_PARAMS: &[&str] = &[
        "-executionpolicy",
        "-ep",
        "-workingdirectory",
        "-wd",
        "-outputformat",
        "-of",
        "-inputformat",
        "-if",
        "-windowstyle",
        "-w",
        "-configurationname",
        "-settingsfile",
    ];
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            return Exec::Source {
                language: Language::PowerShell,
                source: Value::Unknown,
                script_args: Vec::new(),
            };
        };
        let lower = text.to_ascii_lowercase();
        if lower == "-c" || (lower.len() >= 2 && "-command".starts_with(&lower) && lower.len() > 2)
        {
            let rest = &args[i + 1..];
            if rest.first().and_then(Value::known) == Some("-") {
                return Exec::Source {
                    language: Language::PowerShell,
                    source: stdin_source(stdin).unwrap_or(Value::Unknown),
                    script_args: Vec::new(),
                };
            }
            let words: Option<Vec<&str>> = rest.iter().map(Value::known).collect();
            return Exec::Source {
                language: Language::PowerShell,
                source: words.map_or(Value::Unknown, |w| Value::Known(w.join(" "))),
                script_args: Vec::new(),
            };
        }
        if lower == "-f" || (lower.len() > 2 && "-file".starts_with(&lower)) {
            return Exec::ScriptFile {
                script: args.get(i + 1).cloned().unwrap_or(Value::Unknown),
            };
        }
        if lower == "-e"
            || lower == "-ec"
            || (lower.len() > 3 && "-encodedcommand".starts_with(&lower))
        {
            return Exec::Source {
                language: Language::PowerShell,
                source: Value::Unknown,
                script_args: Vec::new(),
            };
        }
        if VALUE_PARAMS.contains(&lower.as_str()) {
            i += 2;
        } else if lower.starts_with('-') {
            i += 1;
        } else {
            return Exec::ScriptFile {
                script: arg.clone(),
            };
        }
    }
    Exec::Plain
}

fn unsupported_or_script(args: &[Value], language: &'static str, eval_flags: &[&str]) -> Exec {
    for arg in args {
        match arg.known() {
            Some(t) if eval_flags.contains(&t) => return Exec::Unsupported { language },
            Some(t) if t.starts_with('-') => {}
            Some(_) | None => {
                return Exec::ScriptFile {
                    script: arg.clone(),
                };
            }
        }
    }
    Exec::Plain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::UncertaintyKind,
        testutil::{bash, targets},
    };

    fn k(s: &str) -> Value {
        Value::Known(s.into())
    }

    fn args(words: &[&str]) -> Vec<Value> {
        words.iter().map(|w| k(w)).collect()
    }

    #[test]
    fn python_c_and_stdin_carry_their_arguments() {
        match classify("python3", &args(&["-c", "print(1)", "a.txt"]), &Stdin::None) {
            Exec::Source {
                language: Language::Python,
                source,
                script_args,
            } => {
                assert_eq!(source, k("print(1)"));
                assert_eq!(script_args, args(&["-c", "a.txt"]));
            }
            _ => panic!("expected python source"),
        }
        let stdin = Stdin::Source {
            value: k("print(2)"),
            span: 0..0,
        };
        match classify("python3", &args(&["-", "src/a.ts"]), &stdin) {
            Exec::Source {
                source,
                script_args,
                ..
            } => {
                assert_eq!(source, k("print(2)"));
                assert_eq!(script_args, args(&["-", "src/a.ts"]));
            }
            _ => panic!("expected python source"),
        }
    }

    #[test]
    fn a_script_path_is_a_script_file_and_a_module_is_plain() {
        assert!(matches!(
            classify("python3", &args(&["-u", "tools/gen.py", "x"]), &Stdin::None),
            Exec::ScriptFile { .. }
        ));
        assert!(matches!(
            classify("python3", &args(&["-m", "pytest"]), &Stdin::None),
            Exec::Plain
        ));
        assert!(matches!(
            classify("bash", &args(&["deploy.sh"]), &Stdin::None),
            Exec::ScriptFile { .. }
        ));
        assert!(matches!(
            classify("pwsh", &args(&["-File", "a.ps1"]), &Stdin::None),
            Exec::ScriptFile { .. }
        ));
    }

    #[test]
    fn shell_c_binds_dollar_zero_from_the_first_trailing_word() {
        match classify(
            "sh",
            &args(&["-ec", "echo $1", "sh", "out.txt"]),
            &Stdin::None,
        ) {
            Exec::Source {
                language: Language::Bash,
                script_args,
                ..
            } => assert_eq!(script_args, args(&["sh", "out.txt"])),
            _ => panic!("expected bash source"),
        }
    }

    #[test]
    fn powershell_command_joins_the_remaining_words() {
        match classify(
            "pwsh",
            &args(&["-NoProfile", "-Command", "Set-Content", "a.txt", "x"]),
            &Stdin::None,
        ) {
            Exec::Source {
                language: Language::PowerShell,
                source,
                ..
            } => assert_eq!(source, k("Set-Content a.txt x")),
            _ => panic!("expected powershell source"),
        }
    }

    #[test]
    fn node_eval_binds_process_argv() {
        match classify("node", &args(&["-e", "x", "a.txt"]), &Stdin::None) {
            Exec::Source {
                language: Language::JavaScript,
                script_args,
                ..
            } => {
                assert_eq!(script_args, vec![Value::Unknown, k("a.txt")]);
            }
            _ => panic!("expected javascript source"),
        }
    }

    #[test]
    fn bun_eval_binds_process_argv_with_its_own_offset() {
        // process.argv probes: node ["/usr/bin/node","a","b"]; bun
        // ["/home/lev/.bun/bin/bun","a","b"].
        match classify("bun", &args(&["-e", "x", "a.txt"]), &Stdin::None) {
            Exec::Source {
                language: Language::TypeScript,
                script_args,
                ..
            } => {
                assert_eq!(script_args, vec![Value::Unknown, k("a.txt")]);
            }
            _ => panic!("expected typescript source"),
        }
    }

    #[test]
    fn languages_without_an_adapter_are_unsupported() {
        for (name, a) in [
            ("perl", vec!["-e", "print 1"]),
            ("ruby", vec!["-e", "1"]),
            ("nu", vec!["-c", "ls"]),
            ("awk", vec!["{print $1}", "f"]),
        ] {
            assert!(
                matches!(
                    classify(name, &args(&a), &Stdin::None),
                    Exec::Unsupported { .. }
                ),
                "{name}"
            );
        }
        assert!(matches!(
            classify(
                "perl",
                &args(&["-pi", "-e", "s/a/b/", "f.txt"]),
                &Stdin::None
            ),
            Exec::Plain
        ));
    }

    #[test]
    fn source_from_an_unknown_producer_is_unknown() {
        let stdin = Stdin::Source {
            value: Value::Unknown,
            span: 0..0,
        };
        match classify("python3", &args(&["-"]), &stdin) {
            Exec::Source { source, .. } => assert_eq!(source, Value::Unknown),
            _ => panic!("expected python source"),
        }
    }

    #[test]
    fn nested_shell_source_is_analyzed_with_its_positional_arguments() {
        let a = bash("sh -c 'echo x > \"$1\"' sh out.txt");
        assert_eq!(targets(&a), ["/repo/out.txt"]);
        assert!(a.file_effects[0].location.embedded.is_some());
    }

    #[test]
    fn eval_of_known_words_is_bash_source() {
        assert_eq!(targets(&bash("eval 'echo x > e.txt'")), ["/repo/e.txt"]);
    }

    #[test]
    fn a_piped_script_of_unknown_content_is_an_unresolved_write() {
        let a = bash("curl -s https://example.invalid/x | python3 -");
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::UnresolvedWrite),
            "{a:?}"
        );
    }

    #[test]
    fn nesting_past_the_depth_limit_is_reported() {
        let a = bash(&format!("{} echo hi > deep.txt", "eval ".repeat(12)));
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::LimitExhausted(crate::model::Limit::Depth)),
            "{a:?}"
        );
    }
}
