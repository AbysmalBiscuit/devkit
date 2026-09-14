#![allow(dead_code)]

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Word},
    context::PathStyle,
    model::Value,
    paths,
};

pub(crate) fn basename(prog: &str) -> &str {
    prog.rsplit(['/', '\\']).next().unwrap_or(prog)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CwdChange {
    Inherit,
    To(String),
    Unknown,
}

impl CwdChange {
    pub(crate) fn apply(&self, inherited: Option<String>) -> Option<String> {
        match self {
            Self::Inherit => inherited,
            Self::To(dir) => Some(dir.clone()),
            Self::Unknown => None,
        }
    }
}

pub(crate) struct Unwrapped {
    pub(crate) argv: Vec<Word>,
    pub(crate) wrappers: Vec<Vec<Word>>,
    pub(crate) cwd: CwdChange,
}

struct Wrapper {
    prefix: &'static [&'static str],
    value_flags: &'static [&'static str],
    cwd_flags: &'static [&'static str],
    positional: usize,
}

const WRAPPERS: &[Wrapper] = &[
    Wrapper {
        prefix: &["nohup"],
        value_flags: &[],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["setsid"],
        value_flags: &[],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["exec"],
        value_flags: &["-a"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["time"],
        value_flags: &["-f", "-o"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["command"],
        value_flags: &[],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["builtin"],
        value_flags: &[],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["nice"],
        value_flags: &["-n", "--adjustment"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["stdbuf"],
        value_flags: &["-i", "-o", "-e"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["sudo"],
        value_flags: &["-u", "-g", "-C", "-h", "-p"],
        cwd_flags: &["-D", "--chdir"],
        positional: 0,
    },
    Wrapper {
        prefix: &["timeout"],
        value_flags: &["-k", "--kill-after", "-s", "--signal"],
        cwd_flags: &[],
        positional: 1,
    },
    Wrapper {
        prefix: &["env"],
        value_flags: &["-u", "--unset", "-S", "--split-string"],
        cwd_flags: &["-C", "--chdir"],
        positional: 0,
    },
    Wrapper {
        prefix: &["uv", "run"],
        value_flags: &[
            "--with",
            "--with-requirements",
            "--with-editable",
            "--python",
            "-p",
            "--project",
            "--env-file",
            "--group",
            "--extra",
            "--package",
            "--index",
            "--index-url",
            "--extra-index-url",
            "--script",
        ],
        cwd_flags: &["--directory"],
        positional: 0,
    },
    Wrapper {
        prefix: &["uv", "tool", "run"],
        value_flags: &["--with", "--from", "--python", "-p"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["uvx"],
        value_flags: &["--with", "--from", "--python", "-p"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["pipx", "run"],
        value_flags: &["--spec", "--python"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["poetry", "run"],
        value_flags: &[],
        cwd_flags: &["-C", "--directory"],
        positional: 0,
    },
    Wrapper {
        prefix: &["pdm", "run"],
        value_flags: &[],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["hatch", "run"],
        value_flags: &[],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["bun", "run"],
        value_flags: &["--cwd"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["bun", "x"],
        value_flags: &[],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["bunx"],
        value_flags: &["-p", "--package"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["pnpm", "exec"],
        value_flags: &["--filter", "-F"],
        cwd_flags: &["-C", "--dir"],
        positional: 0,
    },
    Wrapper {
        prefix: &["pnpm", "dlx"],
        value_flags: &["--package"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["npm", "exec"],
        value_flags: &["-p", "--package", "-w", "--workspace"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["npx"],
        value_flags: &["-p", "--package"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["yarn", "dlx"],
        value_flags: &["-p", "--package"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["mise", "exec"],
        value_flags: &["-C", "--cd"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["mise", "x"],
        value_flags: &["-C", "--cd"],
        cwd_flags: &[],
        positional: 0,
    },
    Wrapper {
        prefix: &["direnv", "exec"],
        value_flags: &[],
        cwd_flags: &[],
        positional: 1,
    },
];

fn program_name(word: &Word) -> Option<String> {
    let name = basename(word.value.known()?);
    Some(name.strip_suffix(".exe").unwrap_or(name).to_string())
}

fn is_assignment(word: &Word) -> bool {
    word.value.known().is_some_and(|w| {
        w.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        })
    })
}

pub(crate) fn unwrap(a: &mut Analyzer<'_>, raw: &RawInvocation, frame: &Frame) -> Unwrapped {
    let mut argv = raw.words.clone();
    let mut wrappers = Vec::new();
    let mut cwd = CwdChange::Inherit;
    let mut current_cwd = raw.cwd.clone();

    loop {
        while argv.first().is_some_and(is_assignment) {
            argv.remove(0);
        }
        let Some(first) = argv.first().and_then(program_name) else {
            break;
        };
        if first == "doppler" && argv.get(1).and_then(|w| w.value.known()) == Some("run") {
            match strip_doppler(&argv) {
                Some((consumed, inner)) => {
                    wrappers.push(argv[..consumed].to_vec());
                    argv = inner;
                    continue;
                }
                None => break,
            }
        }
        if first == "xargs" {
            let (consumed, replace) = skip_options(&argv, 1, &[
                "-I",
                "-i",
                "-n",
                "-P",
                "-d",
                "-L",
                "-a",
                "-s",
                "-E",
                "--max-args",
                "--max-procs",
                "--delimiter",
                "--arg-file",
            ]);
            wrappers.push(argv[..consumed].to_vec());
            argv = argv[consumed..].to_vec();
            if argv.is_empty() {
                argv.push(literal("echo"));
            }
            if replace {
                for word in argv.iter_mut().skip(1) {
                    word.value = Value::Unknown;
                }
            } else {
                argv.push(Word {
                    value: Value::Unknown,
                    typed: "<input>".into(),
                    span: 0..0,
                });
            }
            continue;
        }
        if first == "find" {
            run_find_exec(a, raw, frame, &argv, current_cwd.clone());
            break;
        }
        let Some(wrapper) = WRAPPERS.iter().find(|wrapper| {
            argv.len() > wrapper.prefix.len()
                && wrapper.prefix.iter().enumerate().all(|(i, prefix)| {
                    if i == 0 {
                        program_name(&argv[0]).as_deref() == Some(*prefix)
                    } else {
                        argv[i].value.known() == Some(*prefix)
                    }
                })
        }) else {
            break;
        };
        let mut i = wrapper.prefix.len();
        while let Some(word) = argv.get(i) {
            let Some(text) = word.value.known() else {
                break;
            };
            if text == "--" {
                i += 1;
                break;
            }
            if !text.starts_with('-') {
                if first == "env" && is_assignment(word) {
                    i += 1;
                    continue;
                }
                break;
            }
            let (flag, inline) = match text.split_once('=') {
                Some((flag, value)) => (flag, Some(value.to_string())),
                None => (text, None),
            };
            if wrapper.cwd_flags.contains(&flag) {
                let value = match inline {
                    Some(value) => Value::Known(value),
                    None => {
                        i += 1;
                        argv.get(i).map_or(Value::Unknown, |w| w.value.clone())
                    }
                };
                cwd = match paths::resolve(&value, current_cwd.as_deref(), a.ctx.path_style) {
                    crate::model::Target::Path(dir) => CwdChange::To(dir),
                    crate::model::Target::Unresolved | crate::model::Target::Ephemeral => {
                        CwdChange::Unknown
                    }
                };
                current_cwd = cwd.apply(current_cwd.clone());
            } else if wrapper.value_flags.contains(&flag) && inline.is_none() {
                i += 1;
            }
            i += 1;
        }
        if first == "direnv"
            && let Some(dir) = argv.get(i)
        {
            cwd = match paths::resolve(&dir.value, current_cwd.as_deref(), a.ctx.path_style) {
                crate::model::Target::Path(path) => CwdChange::To(path),
                crate::model::Target::Unresolved | crate::model::Target::Ephemeral => {
                    CwdChange::Unknown
                }
            };
            current_cwd = cwd.apply(current_cwd.clone());
        }
        i += wrapper.positional;
        if i >= argv.len() {
            break;
        }
        wrappers.push(argv[..i].to_vec());
        argv = argv[i..].to_vec();
    }
    Unwrapped {
        argv,
        wrappers,
        cwd,
    }
}

fn literal(s: &str) -> Word {
    Word {
        value: Value::Known(s.into()),
        typed: s.into(),
        span: 0..0,
    }
}

fn skip_options(argv: &[Word], start: usize, value_flags: &[&str]) -> (usize, bool) {
    let mut i = start;
    let mut replace = false;
    while let Some(text) = argv.get(i).and_then(|w| w.value.known()) {
        if !text.starts_with('-') {
            break;
        }
        if text == "--" {
            return (i + 1, replace);
        }
        replace |= text.starts_with("-I") || text.starts_with("-i");
        if value_flags.contains(&text) {
            i += 1;
        }
        i += 1;
    }
    (i, replace)
}

fn strip_doppler(argv: &[Word]) -> Option<(usize, Vec<Word>)> {
    if let Some(sep) = argv.iter().position(|w| w.value.known() == Some("--")) {
        return Some((sep + 1, argv[sep + 1..].to_vec()));
    }
    let idx = argv.iter().position(|w| {
        w.value
            .known()
            .is_some_and(|text| text == "--command" || text.starts_with("--command="))
    })?;
    let text = argv[idx].value.known()?;
    let (value, consumed) = match text.strip_prefix("--command=") {
        Some(value) => (value.to_string(), idx + 1),
        None => (argv.get(idx + 1)?.value.known()?.to_string(), idx + 2),
    };
    let inner = crate::bash::simple_argv(&value)?;
    Some((
        consumed,
        inner.into_iter().map(|word| literal(&word)).collect(),
    ))
}

pub(crate) fn doppler_flags(words: &[Value]) -> (Option<String>, Option<String>) {
    let (mut config, mut project) = (None, None);
    let mut i = 0;
    while i < words.len() {
        let Some(text) = words[i].known() else {
            i += 1;
            continue;
        };
        let (key, inline) = match text.split_once('=') {
            Some((key, value)) => (key, Some(value.to_string())),
            None => (text, None),
        };
        if text == "--" {
            break;
        }
        let value = inline.clone().or_else(|| {
            words
                .get(i + 1)
                .filter(|v| v.known() != Some("--"))
                .and_then(|v| v.known())
                .map(str::to_string)
        });
        match key {
            "-c" | "--config" => config = value,
            "-p" | "--project" => project = value,
            _ => {}
        }
        i += if inline.is_some()
            || !key.starts_with('-')
            || !matches!(key, "-c" | "--config" | "-p" | "--project")
        {
            1
        } else {
            2
        };
    }
    (config, project)
}

fn run_find_exec(
    a: &mut Analyzer<'_>,
    raw: &RawInvocation,
    frame: &Frame,
    argv: &[Word],
    cwd: Option<String>,
) {
    let mut i = 1;
    while i < argv.len() {
        if matches!(
            argv[i].value.known(),
            Some("-exec" | "-execdir" | "-ok" | "-okdir")
        ) {
            let start = i + 1;
            let end = argv[start..]
                .iter()
                .position(|w| matches!(w.value.known(), Some(";" | "+")))
                .map_or(argv.len(), |p| start + p);
            let words = argv[start..end]
                .iter()
                .map(|w| {
                    if w.value.known() == Some("{}") {
                        Word {
                            value: Value::Unknown,
                            ..w.clone()
                        }
                    } else {
                        w.clone()
                    }
                })
                .collect();
            a.invocation(
                RawInvocation {
                    words,
                    stdin: crate::analyzer::Stdin::None,
                    cwd: cwd.clone(),
                    language: raw.language,
                    location: raw.location.clone(),
                },
                frame,
            );
            i = end;
        }
        i += 1;
    }
}

pub(crate) struct ProgramOptions {
    pub(crate) semantic_args: Vec<Value>,
    pub(crate) cwd: CwdChange,
}

const GIT_VALUE_OPTIONS: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
    "--config-env",
    "--super-prefix",
];

pub(crate) fn program_options(
    program: &Value,
    args: &[Value],
    cwd: Option<&str>,
    style: PathStyle,
) -> ProgramOptions {
    if program
        .known()
        .map(basename)
        .map(|name| name.strip_suffix(".exe").unwrap_or(name))
        != Some("git")
    {
        return ProgramOptions {
            semantic_args: args.to_vec(),
            cwd: CwdChange::Inherit,
        };
    }
    let mut change = CwdChange::Inherit;
    let mut current = cwd.map(str::to_string);
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        let Some(text) = arg.known() else {
            break;
        };
        if !text.starts_with('-') {
            break;
        }
        let (flag, inline) = match text.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_string())),
            None => (text, None),
        };
        if GIT_VALUE_OPTIONS.contains(&flag) {
            let value = match inline {
                Some(value) => Value::Known(value),
                None => {
                    i += 1;
                    args.get(i).cloned().unwrap_or(Value::Unknown)
                }
            };
            if matches!(flag, "-C" | "--work-tree") {
                change = match paths::resolve(&value, current.as_deref(), style) {
                    crate::model::Target::Path(dir) => CwdChange::To(dir),
                    crate::model::Target::Unresolved | crate::model::Target::Ephemeral => {
                        CwdChange::Unknown
                    }
                };
                current = change.apply(current.clone());
            }
        }
        i += 1;
    }
    ProgramOptions {
        semantic_args: args[i.min(args.len())..].to_vec(),
        cwd: change,
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        model::Value,
        testutil::{bash, programs},
    };

    fn k(s: &str) -> Value {
        Value::Known(s.into())
    }

    #[test]
    fn process_wrappers_are_removed_and_kept() {
        let a = bash("nohup timeout -k 5 30 env FOO=1 nice -n 5 python3 x.py");
        let inv = a
            .invocations
            .iter()
            .find(|i| i.program == k("python3"))
            .expect("python3");
        assert_eq!(inv.wrappers.len(), 4, "{:?}", inv.wrappers);
        assert_eq!(inv.args, vec![k("x.py")]);
    }

    #[test]
    fn a_runner_prefix_is_removed_with_its_options() {
        let a = bash("uv run --with rich --directory sub python -c 'print(1)'");
        let inv = a
            .invocations
            .iter()
            .find(|i| i.program == k("python"))
            .expect("python");
        assert_eq!(inv.cwd.as_deref(), Some("/repo/sub"));
        assert_eq!(
            bash("uvx ruff format src").invocations[0].program,
            k("ruff")
        );
        assert_eq!(
            bash("pnpm exec prettier --write .").invocations[0].program,
            k("prettier")
        );
        assert_eq!(
            bash("npx -y prettier --write .").invocations[0].program,
            k("prettier")
        );
        assert_eq!(bash("bunx --bun vite").invocations[0].program, k("vite"));
        assert_eq!(bash("npm exec vite").invocations[0].program, k("vite"));
        assert_eq!(bash("npm run dev").invocations[0].program, k("npm"));
    }

    #[test]
    fn a_doppler_wrapper_is_removed_in_both_forms() {
        let a = bash("doppler run -c dev -- bun test");
        assert_eq!(a.invocations[0].program, k("bun"));
        assert_eq!(
            super::doppler_flags(&a.invocations[0].wrappers[0]),
            (Some("dev".into()), None)
        );
        let b = bash("doppler run --config prd --command='bun test'");
        assert_eq!(b.invocations[0].program, k("bun"));
        assert_eq!(
            super::doppler_flags(&b.invocations[0].wrappers[0]),
            (Some("prd".into()), None)
        );
    }

    #[test]
    fn doppler_config_spellings_have_the_same_metadata() {
        let short = bash("doppler run -c dev -- x");
        let long = bash("doppler run --config dev -- x");
        assert_eq!(
            super::doppler_flags(&short.invocations[0].wrappers[0]),
            super::doppler_flags(&long.invocations[0].wrappers[0])
        );
    }

    #[test]
    fn doppler_metadata_handles_equals_and_dangling_flags() {
        let equals = bash("doppler run --config=dev -- vite");
        assert_eq!(
            super::doppler_flags(&equals.invocations[0].wrappers[0]),
            (Some("dev".into()), None)
        );
        let dangling = bash("doppler run -c -- vite");
        assert_eq!(
            super::doppler_flags(&dangling.invocations[0].wrappers[0]),
            (None, None)
        );
    }

    #[test]
    fn a_doppler_run_without_a_separator_is_left_alone() {
        assert_eq!(
            bash("doppler run -c dev").invocations[0].program,
            k("doppler")
        );
    }

    #[test]
    fn basename_and_assignment_normalization_match_guard_inputs() {
        assert_eq!(super::basename("./node_modules/.bin/vite"), "vite");
        assert!(bash("FOO=1").invocations.is_empty());
        assert_eq!(
            bash("FOO=1 env BAR=2 vite").invocations[0].program,
            k("vite")
        );
    }

    #[test]
    fn wrapper_options_are_skipped_before_the_command() {
        for (command, program) in [
            ("env -i FOO=1 vite", "vite"),
            ("timeout --foreground 30 vite", "vite"),
            ("npx --yes vite", "vite"),
            ("bunx --bun vite", "vite"),
        ] {
            assert_eq!(
                bash(command).invocations[0].program,
                k(program),
                "{command}"
            );
        }
    }

    #[test]
    fn a_compound_doppler_command_value_stays_wrapped() {
        assert_eq!(
            bash("doppler run --command='bun test && ls'").invocations[0].program,
            k("doppler")
        );
    }

    #[test]
    fn git_global_options_leave_the_semantic_arguments() {
        let a = bash("git -C /other -c core.x=1 --no-pager worktree add ../wt");
        let inv = &a.invocations[0];
        assert_eq!(inv.semantic_args, vec![k("worktree"), k("add"), k("../wt")]);
        assert_eq!(inv.args.len(), 8, "the full vector keeps -C and -c");
    }

    #[test]
    fn xargs_and_find_exec_run_their_command_with_unknown_arguments() {
        let a = bash("ls | xargs -n 1 rm -f");
        let rm = a
            .invocations
            .iter()
            .find(|i| i.program == k("rm"))
            .expect("rm");
        assert_eq!(rm.args.last(), Some(&Value::Unknown));

        let b = bash("find . -name '*.bak' -exec rm {} \\;");
        assert_eq!(programs(&b), ["rm", "find"]);
    }
}
