//! Built-in effects of the programs devkit models. A program outside this
//! table produces no file effect: an unmodeled program is one devkit has no
//! opinion about. The table grows by pull request, never by config.

use crate::model::{FileOp, Value};

pub(crate) enum Hit {
    File(FileOp, Value),
    /// The destination of a rename or move, with what was moved there.
    RenameInto {
        sources: Vec<Value>,
        dest: Value,
    },
    /// The destination of a copy that dereferences its source.
    CopyContent(Value),
    Tree {
        scope: Value,
        whole_checkout: bool,
        by: String,
    },
    /// A tree writer that only takes entries away.
    TreeRemoval {
        scope: Value,
        by: String,
    },
    Unresolved(String),
    ScriptFile(Value),
}

#[derive(Clone, Copy, PartialEq, Eq, strum::EnumString, strum::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum Program {
    Tee,
    Touch,
    Truncate,
    Dd,
    #[strum(serialize = "rm", serialize = "unlink", serialize = "shred")]
    Rm,
    Cp,
    Install,
    Mv,
    Ln,
    Sed,
    Perl,
    Git,
    Cargo,
    Rustfmt,
    Prettier,
    Biome,
    Eslint,
    Ruff,
    Black,
    Isort,
    Taplo,
    Dprint,
    Deno,
    Gofmt,
    Goimports,
    ClangFormat,
    Shfmt,
    Stylua,
    Mkdir,
    #[strum(serialize = "source", serialize = ".")]
    Source,
    Patch,
    Curl,
    Wget,
    Tar,
    Unzip,
}

pub(crate) fn is_cataloged(name: &str) -> bool {
    name.parse::<Program>().is_ok()
}

struct Parsed {
    operands: Vec<Value>,
    flags: Vec<String>,
    values: Vec<(String, Value)>,
}

impl Parsed {
    fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    /// A single-dash cluster containing `c`, such as `-rf` for `r`.
    fn has_short(&self, c: char) -> bool {
        self.flags
            .iter()
            .any(|f| f.starts_with('-') && !f.starts_with("--") && f[1..].contains(c))
    }

    fn value(&self, names: &[&str]) -> Option<&Value> {
        self.values
            .iter()
            .find(|(n, _)| names.contains(&n.as_str()))
            .map(|(_, v)| v)
    }
}

fn parse(args: &[Value], value_flags: &[&str]) -> Parsed {
    let mut p = Parsed {
        operands: Vec::new(),
        flags: Vec::new(),
        values: Vec::new(),
    };
    let mut i = 0;
    let mut options_done = false;
    while let Some(arg) = args.get(i) {
        match arg.known() {
            Some("--") if !options_done => options_done = true,
            Some(t) if !options_done && t.starts_with('-') && t.len() > 1 => {
                match t.split_once('=') {
                    Some((flag, v)) if flag.starts_with("--") => p
                        .values
                        .push((flag.to_string(), Value::Known(v.to_string()))),
                    _ if value_flags.contains(&t) => {
                        i += 1;
                        p.values.push((
                            t.to_string(),
                            args.get(i).cloned().unwrap_or(Value::Unknown),
                        ));
                    }
                    _ => p.flags.push(t.to_string()),
                }
            }
            _ => p.operands.push(arg.clone()),
        }
        i += 1;
    }
    p
}

fn each(op: FileOp, operands: &[Value]) -> Vec<Hit> {
    operands.iter().map(|v| file(op, v)).collect()
}

fn file(op: FileOp, v: &Value) -> Hit {
    match v {
        Value::Known(_) | Value::Ephemeral(_) | Value::Within(_) => Hit::File(op, v.clone()),
        Value::Unknown => Hit::Unresolved(format!("a {op:?} target could not be determined")),
    }
}

fn rename_into(sources: &[Value], dest: &Value) -> Hit {
    match dest {
        Value::Known(_) | Value::Ephemeral(_) | Value::Within(_) => Hit::RenameInto {
            sources: sources.to_vec(),
            dest: dest.clone(),
        },
        Value::Unknown => Hit::Unresolved("a Rename target could not be determined".into()),
    }
}

fn removal(scope: &Value, by: &str) -> Hit {
    Hit::TreeRemoval {
        scope: scope.clone(),
        by: by.to_string(),
    }
}

fn tree(scope: &Value, whole_checkout: bool, by: &str) -> Hit {
    Hit::Tree {
        scope: scope.clone(),
        whole_checkout,
        by: by.to_string(),
    }
}

/// Whether a `cp` copies a link as a link rather than the file it names.
fn keeps_links(p: &Parsed) -> bool {
    ['P', 'd', 'a', 'l', 's']
        .into_iter()
        .any(|c| p.has_short(c))
        || ["--no-dereference", "--archive", "--link", "--symbolic-link"]
            .into_iter()
            .any(|f| p.has(f))
        || p.value(&["--preserve"]).is_some_and(|v| {
            v.known()
                .is_none_or(|v| v.split(',').any(|v| matches!(v, "links" | "all")))
        })
}

fn join_name(dir: &Value, source: &Value) -> Value {
    match (dir.known(), source.known()) {
        (Some(d), Some(s)) => Value::Known(format!(
            "{}/{}",
            d.trim_end_matches('/'),
            crate::normalize::basename(s)
        )),
        _ => Value::Unknown,
    }
}

/// A formatter operand: a path with an extension is a file, anything else a
/// directory it rewrites.
fn formatter_operands(operands: &[Value], by: &str) -> Vec<Hit> {
    if operands.is_empty() {
        return vec![tree(&Value::Known(".".into()), false, by)];
    }
    operands
        .iter()
        .map(|v| match v {
            // A match may be a directory, which the formatter rewrites
            // throughout, and that stays within the bound too.
            Value::Within(_) => Hit::File(FileOp::Overwrite, v.clone()),
            Value::Known(p) if crate::normalize::basename(p).contains('.') && !p.ends_with('.') => {
                Hit::File(FileOp::Overwrite, v.clone())
            }
            Value::Known(_) => tree(v, false, by),
            Value::Unknown | Value::Ephemeral(_) => Hit::Unresolved(format!(
                "`{by}` rewrites a path that could not be determined"
            )),
        })
        .collect()
}

pub(crate) fn effects(name: &str, args: &[Value]) -> Vec<Hit> {
    let Ok(program) = name.parse::<Program>() else {
        return Vec::new();
    };
    match program {
        Program::Tee => {
            let p = parse(args, &[]);
            each(
                if p.has("-a") || p.has("--append") {
                    FileOp::Append
                } else {
                    FileOp::Overwrite
                },
                &p.operands,
            )
        }
        Program::Touch => each(
            FileOp::Create,
            &parse(args, &["-r", "-d", "-t", "--reference", "--date"]).operands,
        ),
        Program::Truncate => each(
            FileOp::Overwrite,
            &parse(args, &["-s", "--size", "-r", "--reference"]).operands,
        ),
        Program::Dd => args
            .iter()
            .filter_map(|a| match a.known() {
                Some(t) => t
                    .strip_prefix("of=")
                    .map(|p| Hit::File(FileOp::Overwrite, Value::Known(p.to_string()))),
                None => Some(Hit::Unresolved(
                    "a `dd` operand could not be determined".into(),
                )),
            })
            .collect(),
        Program::Rm => {
            let p = parse(args, &[]);
            if p.has_short('r') || p.has_short('R') || p.has("--recursive") {
                p.operands
                    .iter()
                    .map(|v| match v {
                        Value::Known(_) | Value::Ephemeral(_) | Value::Within(_) => {
                            removal(v, "rm -r")
                        }
                        Value::Unknown => Hit::Unresolved(
                            "`rm -r` removes a path that could not be determined".into(),
                        ),
                    })
                    .collect()
            } else {
                each(FileOp::Delete, &p.operands)
            }
        }
        Program::Cp | Program::Install => {
            let p = parse(args, &[
                "-t",
                "--target-directory",
                "-S",
                "--suffix",
                "-m",
                "--mode",
                "-o",
                "--owner",
                "-g",
                "--group",
            ]);
            if program == Program::Install && p.has("-d") {
                return Vec::new();
            }
            let recursive =
                p.has_short('r') || p.has_short('R') || p.has_short('a') || p.has("--recursive");
            let copy = |dest: &Value| match dest {
                Value::Unknown => file(FileOp::Copy, dest),
                _ if program == Program::Install || !keeps_links(&p) => {
                    Hit::CopyContent(dest.clone())
                }
                _ => file(FileOp::Copy, dest),
            };
            if let Some(dir) = p.value(&["-t", "--target-directory"]) {
                return p
                    .operands
                    .iter()
                    .map(|s| {
                        let target = join_name(dir, s);
                        if recursive {
                            match target {
                                Value::Known(_) | Value::Ephemeral(_) | Value::Within(_) => {
                                    tree(&target, false, "cp -r")
                                }
                                Value::Unknown => Hit::Unresolved(
                                    "`cp -r` destination could not be determined".into(),
                                ),
                            }
                        } else {
                            copy(&target)
                        }
                    })
                    .collect();
            }
            match p.operands.split_last() {
                Some((dest, sources)) if !sources.is_empty() => {
                    if recursive {
                        vec![match dest {
                            Value::Known(_) | Value::Ephemeral(_) | Value::Within(_) => {
                                tree(dest, false, "cp -r")
                            }
                            Value::Unknown => Hit::Unresolved(
                                "`cp -r` destination could not be determined".into(),
                            ),
                        }]
                    } else {
                        vec![copy(dest)]
                    }
                }
                _ => Vec::new(),
            }
        }
        Program::Mv => {
            let p = parse(args, &["-t", "--target-directory", "-S", "--suffix"]);
            if let Some(dir) = p.value(&["-t", "--target-directory"]) {
                return p
                    .operands
                    .iter()
                    .flat_map(|s| {
                        [
                            file(FileOp::Rename, s),
                            rename_into(std::slice::from_ref(s), &join_name(dir, s)),
                        ]
                    })
                    .collect();
            }
            match p.operands.split_last() {
                Some((dest, sources)) if !sources.is_empty() => sources
                    .iter()
                    .map(|s| file(FileOp::Rename, s))
                    .chain([rename_into(sources, dest)])
                    .collect(),
                _ => Vec::new(),
            }
        }
        Program::Ln => {
            let p = parse(args, &["-t", "--target-directory", "-S", "--suffix"]);
            if let Some(dir) = p.value(&["-t", "--target-directory"]) {
                return p
                    .operands
                    .iter()
                    .map(|source| file(FileOp::Create, &join_name(dir, source)))
                    .collect();
            }
            match p.operands.as_slice() {
                [_, .., Value::Ephemeral(_)] => vec![Hit::Unresolved(
                    "a link made in a fresh directory can point outside it".into(),
                )],
                [_, .., link] => vec![file(FileOp::Create, link)],
                _ => Vec::new(),
            }
        }
        Program::Sed => {
            let p = parse(args, &[
                "-e",
                "--expression",
                "-f",
                "--file",
                "-l",
                "--line-length",
            ]);
            let in_place = p.has("--in-place")
                || p.values.iter().any(|(f, _)| f == "--in-place")
                || p.flags.iter().any(|f| {
                    f.starts_with("-i")
                        || (f.starts_with('-') && !f.starts_with("--") && f[1..].contains('i'))
                });
            if !in_place {
                return Vec::new();
            }
            let files = if p.value(&["-e", "--expression", "-f", "--file"]).is_some() {
                &p.operands[..]
            } else {
                p.operands.get(1..).unwrap_or(&[])
            };
            each(FileOp::Overwrite, files)
        }
        Program::Perl => {
            let mut in_place = false;
            let mut code_given = false;
            let mut files = Vec::new();
            let mut i = 0;
            while let Some(arg) = args.get(i) {
                match arg.known() {
                    Some(t) if t.starts_with('-') && !t.starts_with("--") && t.len() > 1 => {
                        in_place |= t[1..].contains('i');
                        if t.ends_with('e') || t.ends_with('E') {
                            code_given = true;
                            i += 1;
                        }
                    }
                    _ => files.push(arg.clone()),
                }
                i += 1;
            }
            if !in_place {
                return Vec::new();
            }
            let files = if code_given {
                &files[..]
            } else {
                files.get(1..).unwrap_or(&[])
            };
            each(FileOp::Overwrite, files)
        }
        Program::Git => git(args),
        Program::Cargo => match args.first().and_then(Value::known) {
            Some("fmt") if !args.iter().any(|a| a.known() == Some("--check")) => {
                vec![tree(&Value::Known(".".into()), true, "cargo fmt")]
            }
            Some("clippy") if args.iter().any(|a| a.known() == Some("--fix")) => {
                vec![tree(&Value::Known(".".into()), true, "cargo clippy --fix")]
            }
            Some("fix") => vec![tree(&Value::Known(".".into()), true, "cargo fix")],
            _ => Vec::new(),
        },
        Program::Rustfmt => {
            let p = parse(args, &["--edition", "--config-path", "--config"]);
            if p.has("--check") {
                Vec::new()
            } else {
                each(FileOp::Overwrite, &p.operands)
            }
        }
        Program::Prettier => write_flag_formatter(program, args, &["--write", "-w"]),
        Program::Eslint => write_flag_formatter(program, args, &["--fix"]),
        Program::ClangFormat => write_flag_formatter(program, args, &["-i"]),
        Program::Gofmt | Program::Goimports | Program::Shfmt => {
            write_flag_formatter(program, args, &["-w"])
        }
        Program::Ruff | Program::Biome | Program::Taplo | Program::Dprint | Program::Deno => {
            let Some(sub) = args.first().and_then(Value::known) else {
                return Vec::new();
            };
            let rest = parse(&args[1..], &[
                "--config",
                "--line-length",
                "--target-version",
                "--select",
                "--ignore",
            ]);
            let checks = rest.has("--check") || rest.has("--diff");
            let writes = match (program, sub) {
                (Program::Ruff, "format")
                | (Program::Taplo, "fmt" | "format")
                | (Program::Deno, "fmt")
                | (Program::Dprint, "fmt") => !checks,
                (Program::Ruff, "check") => rest.has("--fix"),
                (Program::Biome, "format" | "check" | "lint") => {
                    rest.has("--write") || rest.has("--apply") || rest.has("--fix")
                }
                _ => false,
            };
            if writes {
                formatter_operands(&rest.operands, program.into())
            } else {
                Vec::new()
            }
        }
        Program::Black | Program::Isort | Program::Stylua => {
            let p = parse(args, &[
                "--config",
                "-l",
                "--line-length",
                "--settings-path",
            ]);
            if p.has("--check") || p.has("--diff") {
                Vec::new()
            } else {
                formatter_operands(&p.operands, program.into())
            }
        }
        Program::Source => args
            .first()
            .map(|s| vec![Hit::ScriptFile(s.clone())])
            .unwrap_or_default(),
        Program::Patch => {
            let p = parse(args, &[
                "-i",
                "--input",
                "-o",
                "--output",
                "-d",
                "--directory",
                "-p",
                "-D",
                "-B",
                "-z",
            ]);
            if let Some(out) = p.value(&["-o", "--output"]) {
                return vec![file(FileOp::Overwrite, out)];
            }
            match p.operands.first() {
                Some(original) => vec![file(FileOp::Overwrite, original)],
                None => vec![tree(
                    p.value(&["-d", "--directory"])
                        .unwrap_or(&Value::Known(".".into())),
                    false,
                    "patch",
                )],
            }
        }
        Program::Curl => {
            let p = parse(args, &[
                "-o", "--output", "-X", "-H", "-d", "--data", "-u", "-A", "-e", "--url",
            ]);
            if let Some(out) = p.value(&["-o", "--output"]) {
                if out.known() == Some("-") {
                    return Vec::new();
                }
                return vec![file(FileOp::Overwrite, out)];
            }
            if p.has("-O") || p.has("--remote-name") || p.has_short('O') {
                return vec![Hit::Unresolved(
                    "`curl -O` names its file from the URL".into(),
                )];
            }
            Vec::new()
        }
        Program::Wget => {
            let p = parse(args, &[
                "-O",
                "--output-document",
                "-P",
                "--directory-prefix",
                "-o",
                "-a",
            ]);
            match p.value(&["-O", "--output-document"]) {
                Some(out) if out.known() == Some("-") => Vec::new(),
                Some(out) => vec![file(FileOp::Overwrite, out)],
                None if p.operands.is_empty() => Vec::new(),
                None => vec![Hit::Unresolved("`wget` names its file from the URL".into())],
            }
        }
        Program::Tar => {
            let first = args.first().and_then(Value::known).unwrap_or("");
            let p = parse(args, &["-C", "--directory", "-f", "--file", "-T", "-X"]);
            let bundled = !first.starts_with('-');
            let extract = p.has("--extract")
                || p.has("--get")
                || p.has_short('x')
                || (bundled && first.contains('x'));
            let create = p.has("--create") || p.has_short('c') || (bundled && first.contains('c'));
            if extract {
                vec![tree(
                    p.value(&["-C", "--directory"])
                        .unwrap_or(&Value::Known(".".into())),
                    false,
                    "tar -x",
                )]
            } else if create {
                p.value(&["-f", "--file"])
                    .map(|f| vec![file(FileOp::Overwrite, f)])
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        Program::Unzip => {
            let p = parse(args, &["-d", "-x"]);
            if p.has("-l") || p.has("-t") {
                Vec::new()
            } else {
                vec![tree(
                    p.value(&["-d"]).unwrap_or(&Value::Known(".".into())),
                    false,
                    "unzip",
                )]
            }
        }
        Program::Mkdir => Vec::new(),
    }
}

/// A formatter that prints unless one of `write_flags` makes it rewrite its
/// operands in place.
fn write_flag_formatter(program: Program, args: &[Value], write_flags: &[&str]) -> Vec<Hit> {
    let p = parse(args, &[
        "--config",
        "-c",
        "--ignore-path",
        "--plugin",
        "--parser",
        "--style",
        "-i",
    ]);
    let clang_format = program == Program::ClangFormat;
    let writes = write_flags.iter().any(|f| p.has(f))
        || (clang_format && p.values.iter().any(|(f, _)| f == "-i"));
    if !writes {
        return Vec::new();
    }
    let mut operands = p.operands.clone();
    if clang_format && let Some((_, v)) = p.values.iter().find(|(f, _)| f == "-i") {
        operands.insert(0, v.clone());
    }
    formatter_operands(&operands, program.into())
}

fn git(args: &[Value]) -> Vec<Hit> {
    let Some(sub) = args.first().and_then(Value::known) else {
        return Vec::new();
    };
    let rest = &args[1..];
    let whole = |by: &str| vec![tree(&Value::Known(".".into()), true, by)];
    let has = |flag: &str| rest.iter().any(|a| a.known() == Some(flag));
    match sub {
        "checkout" => match rest.iter().position(|a| a.known() == Some("--")) {
            Some(sep) => each(FileOp::Overwrite, &rest[sep + 1..]),
            None => whole("git checkout"),
        },
        "restore" => {
            let p = parse(rest, &["-s", "--source"]);
            if p.has("--staged") && !p.has("--worktree") && !p.has("-W") {
                Vec::new()
            } else {
                each(FileOp::Overwrite, &p.operands)
            }
        }
        "switch" | "merge" | "rebase" | "cherry-pick" | "revert" | "pull" | "am" | "clean" => {
            if has("--abort") && sub != "merge" && sub != "rebase" {
                return Vec::new();
            }
            if sub == "clean" && (has("-n") || has("--dry-run")) {
                return Vec::new();
            }
            whole(&format!("git {sub}"))
        }
        "apply" => {
            if has("--cached") || has("--check") || has("--stat") || has("--numstat") {
                Vec::new()
            } else {
                whole("git apply")
            }
        }
        "stash" => match rest.first().and_then(Value::known) {
            None | Some("push" | "save" | "pop" | "apply" | "branch") => whole("git stash"),
            Some(flag) if flag.starts_with('-') => whole("git stash"),
            _ => Vec::new(),
        },
        "reset" => {
            if has("--hard") {
                whole("git reset --hard")
            } else if has("--merge") || has("--keep") {
                whole("git reset")
            } else {
                Vec::new()
            }
        }
        "mv" => {
            let p = parse(rest, &[]);
            match p.operands.split_last() {
                Some((dest, sources)) if !sources.is_empty() => sources
                    .iter()
                    .map(|s| file(FileOp::Rename, s))
                    .chain([rename_into(sources, dest)])
                    .collect(),
                _ => Vec::new(),
            }
        }
        "rm" => {
            let p = parse(rest, &[]);
            if p.has("--cached") {
                Vec::new()
            } else if p.has_short('r') {
                p.operands
                    .iter()
                    .map(|v| match v {
                        Value::Known(_) | Value::Ephemeral(_) | Value::Within(_) => {
                            removal(v, "git rm -r")
                        }
                        Value::Unknown => {
                            Hit::Unresolved("`git rm -r` path could not be determined".into())
                        }
                    })
                    .collect()
            } else {
                each(FileOp::Delete, &p.operands)
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{bash, targets};

    fn k(s: &str) -> Value {
        Value::Known(s.into())
    }

    fn hits(name: &str, words: &[&str]) -> Vec<String> {
        let args: Vec<Value> = words.iter().map(|w| k(w)).collect();
        effects(name, &args)
            .into_iter()
            .map(|h| match h {
                Hit::File(op, v) => format!("{op:?} {}", v.known().unwrap_or("?")),
                Hit::RenameInto { dest, .. } => {
                    format!("RenameInto {}", dest.known().unwrap_or("?"))
                }
                Hit::CopyContent(v) => format!("CopyContent {}", v.known().unwrap_or("?")),
                Hit::TreeRemoval { scope, by } => {
                    format!("Removal {} {by}", scope.known().unwrap_or("?"))
                }
                Hit::Tree {
                    scope,
                    whole_checkout,
                    by,
                } => format!(
                    "Tree {} {whole_checkout} {by}",
                    scope.known().unwrap_or("?")
                ),
                Hit::Unresolved(_) => "Unresolved".into(),
                Hit::ScriptFile(v) => format!("Script {}", v.known().unwrap_or("?")),
            })
            .collect()
    }

    #[test]
    fn file_management_commands() {
        assert_eq!(hits("tee", &["-a", "log.txt"]), ["Append log.txt"]);
        assert_eq!(hits("touch", &["a", "b"]), ["Create a", "Create b"]);
        assert_eq!(hits("rm", &["-f", "a"]), ["Delete a"]);
        assert_eq!(hits("rm", &["-rf", "build"]), ["Removal build rm -r"]);
        assert_eq!(hits("unlink", &["a"]), ["Delete a"]);
        assert_eq!(hits("shred", &["-u", "a"]), ["Delete a"]);
        assert_eq!(hits("cp", &["a", "b"]), ["CopyContent b"]);
        assert_eq!(hits("install", &["-P", "a", "b"]), ["CopyContent b"]);
        assert_eq!(hits("cp", &["-P", "a", "b"]), ["Copy b"]);
        assert_eq!(hits("cp", &["--preserve=mode,links", "a", "b"]), ["Copy b"]);
        assert_eq!(hits("cp", &["-r", "a", "b"]), ["Tree b false cp -r"]);
        assert_eq!(hits("mv", &["a", "b"]), ["Rename a", "RenameInto b"]);
        assert_eq!(hits("mv", &["-t", "dir", "a"]), [
            "Rename a",
            "RenameInto dir/a"
        ]);
        assert_eq!(hits("dd", &["if=/dev/zero", "of=img", "bs=1M"]), [
            "Overwrite img"
        ]);
        assert!(hits("mkdir", &["-p", "src"]).is_empty());
        assert!(hits("cat", &["a"]).is_empty());
    }

    #[test]
    fn recursive_copy_to_a_target_directory_claims_the_joined_tree() {
        assert_eq!(hits("cp", &["-r", "-t", "out", "src"]), [
            "Tree out/src false cp -r"
        ]);
    }

    #[test]
    fn link_target_directories_claim_joined_link_paths() {
        assert_eq!(hits("ln", &["-t", "links", "source"]), [
            "Create links/source"
        ]);
    }

    #[test]
    fn mkdir_is_cataloged_even_without_an_effect() {
        assert!(is_cataloged("mkdir"));
        assert!(effects("mkdir", &[k("-p"), k("src")]).is_empty());
    }

    #[test]
    fn in_place_editors_claim_their_file_operands() {
        assert_eq!(hits("sed", &["-i", "s/a/b/", "x.rs"]), ["Overwrite x.rs"]);
        assert_eq!(hits("sed", &["-i.bak", "-e", "s/a/b/", "x.rs", "y.rs"]), [
            "Overwrite x.rs",
            "Overwrite y.rs"
        ]);
        assert!(hits("sed", &["s/a/b/", "x.rs"]).is_empty());
        assert_eq!(hits("perl", &["-pi", "-e", "s/a/b/", "x.rs"]), [
            "Overwrite x.rs"
        ]);
        assert_eq!(hits("perl", &["-i", "-pe", "s/a/b/", "x.rs"]), [
            "Overwrite x.rs"
        ]);
    }

    #[test]
    fn git_verbs_that_rewrite_the_tree() {
        assert_eq!(hits("git", &["checkout", "main"]), [
            "Tree . true git checkout"
        ]);
        assert_eq!(hits("git", &["checkout", "--", "a.rs"]), ["Overwrite a.rs"]);
        assert_eq!(hits("git", &["restore", "a.rs"]), ["Overwrite a.rs"]);
        assert_eq!(hits("git", &["stash"]), ["Tree . true git stash"]);
        assert!(hits("git", &["stash", "list"]).is_empty());
        assert_eq!(hits("git", &["reset", "--hard", "HEAD"]), [
            "Tree . true git reset --hard"
        ]);
        assert!(hits("git", &["reset", "HEAD~1"]).is_empty());
        assert_eq!(hits("git", &["mv", "a", "b"]), ["Rename a", "RenameInto b"]);
        assert!(hits("git", &["status"]).is_empty());
        assert!(hits("git", &["worktree", "list"]).is_empty());
    }

    #[test]
    fn formatters_write_their_operands_or_the_tree() {
        assert_eq!(hits("cargo", &["fmt"]), ["Tree . true cargo fmt"]);
        assert!(hits("cargo", &["fmt", "--check"]).is_empty());
        assert_eq!(hits("prettier", &["--write", "src/a.ts"]), [
            "Overwrite src/a.ts"
        ]);
        assert_eq!(hits("prettier", &["--write", "."]), [
            "Tree . false prettier"
        ]);
        assert!(hits("prettier", &["--check", "."]).is_empty());
        assert_eq!(hits("clang-format", &["-i", "a.c"]), ["Overwrite a.c"]);
        assert_eq!(hits("gofmt", &["-w", "src"]), ["Tree src false gofmt"]);
        assert_eq!(hits("ruff", &["format"]), ["Tree . false ruff"]);
        assert!(hits("ruff", &["check", "src"]).is_empty());
        assert_eq!(hits("ruff", &["check", "--fix", "a.py"]), [
            "Overwrite a.py"
        ]);
    }

    #[test]
    fn a_formatter_writes_within_a_globbed_operands_bound() {
        assert_eq!(targets(&bash("prettier --write src/*.ts")), [
            "/repo/src/**"
        ]);
        assert_eq!(targets(&bash("black src/*")), ["/repo/src/**"]);
    }

    #[test]
    fn an_unknown_operand_is_unresolved() {
        let args = vec![k("-f"), Value::Unknown];
        assert!(matches!(effects("rm", &args)[..], [Hit::Unresolved(_)]));
    }

    #[test]
    fn source_and_dot_run_a_script_file() {
        assert_eq!(hits("source", &["env.sh"]), ["Script env.sh"]);
        assert_eq!(hits(".", &["env.sh"]), ["Script env.sh"]);
    }

    #[test]
    fn git_c_scopes_its_effects() {
        let a = bash("git -C sub checkout -- a.rs");
        assert_eq!(targets(&a), ["/repo/sub/a.rs"]);
        let b = bash("git -C \"$d\" checkout main");
        assert!(b.tree_effects.is_empty());
        assert!(!b.uncertainties.is_empty());
    }

    #[test]
    fn an_outer_redirect_around_a_devkit_command_is_still_a_write() {
        assert_eq!(targets(&bash("devrun task check > shared.txt")), [
            "/repo/shared.txt"
        ]);
    }
}
