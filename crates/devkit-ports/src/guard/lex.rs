//! Split a shell command string into the word vectors a shell would execute.
//!
//! A word splitter is not enough here. `shell-words` and `shlex` strip quotes
//! and return a flat vector with no operator positions, so there is nothing
//! left to split segments on, and `2>&1` would have its `&` read as a
//! separator. This is a single pass that tracks quote state and emits words,
//! operators and redirections separately.

/// Characters that end a segment when unquoted.
const BREAKS: [char; 6] = ['|', '&', ';', '(', ')', '\n'];

enum Tok {
    Word(String),
    Break,
    Redirect,
    /// `<<`, `<<-`, `<<'X'`: the next word names the terminator line.
    Heredoc,
}

/// Every command position in `command`, as word vectors. Quoted strings are one
/// opaque word, redirection targets are dropped, and heredoc bodies are skipped
/// entirely — an agent writing `cat > notes.md <<EOF` with "next dev" in the
/// body is writing a file, not launching a server.
///
/// An unterminated quote, or a heredoc whose `<<` names no delimiter, yields no
/// segments: the string cannot be read as a command, and guessing risks a
/// denial on text nobody will execute.
pub fn segments(command: &str) -> Vec<Vec<String>> {
    let Some(toks) = tokenize(command) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut iter = toks.into_iter();
    while let Some(tok) = iter.next() {
        match tok {
            Tok::Break => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            Tok::Redirect | Tok::Heredoc => {
                iter.next();
            }
            Tok::Word(w) => current.push(w),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Scan into words, breaks and redirections. `None` on an unterminated quote or
/// a heredoc with no delimiter.
fn tokenize(command: &str) -> Option<Vec<Tok>> {
    let mut toks = Vec::new();
    let mut word = String::new();
    let mut has_word = false;
    let mut chars = command.chars().peekable();
    let mut pending_heredocs: Vec<(String, bool)> = Vec::new();

    macro_rules! flush {
        () => {
            if has_word {
                toks.push(Tok::Word(std::mem::take(&mut word)));
                has_word = false;
            }
        };
    }

    while let Some(c) = chars.next() {
        if c == '\n' && !pending_heredocs.is_empty() {
            flush!();
            toks.push(Tok::Break);
            skip_heredoc_bodies(&mut chars, &mut pending_heredocs);
            continue;
        }
        match c {
            // An unquoted `#` that begins a word opens a comment running to the
            // end of the line. The newline is left in the stream so it still
            // breaks the segment and still releases any pending heredoc body.
            // A `#` inside a word (`echo foo#bar`) is an ordinary character.
            '#' if !has_word => {
                while chars.peek().is_some_and(|n| *n != '\n') {
                    chars.next();
                }
            }
            '\\' => {
                // A backslash with nothing after it is not an unterminated
                // escape: the shell has nothing left to escape and takes it
                // as a literal character, so `vite dev\` still execs `vite`.
                word.push(chars.next().unwrap_or('\\'));
                has_word = true;
            }
            '\'' => {
                has_word = true;
                loop {
                    let n = chars.next()?;
                    if n == '\'' {
                        break;
                    }
                    word.push(n);
                }
            }
            '"' => {
                has_word = true;
                loop {
                    let n = chars.next()?;
                    if n == '"' {
                        break;
                    }
                    if n == '\\' {
                        word.push(chars.next()?);
                        continue;
                    }
                    word.push(n);
                }
            }
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                flush!();
                toks.push(Tok::Break);
            }
            // A backtick is the older command-substitution syntax `$(...)`
            // replaced: both the opening and the closing backtick delimit a
            // command position, exactly as `$(` and its matching `)` do.
            '`' => {
                flush!();
                toks.push(Tok::Break);
            }
            '<' if chars.peek() == Some(&'(') => {
                // `<(cmd)` process substitution: `cmd` runs as its own
                // process, its output read through a pathname. It is a
                // command position, not a redirection target.
                chars.next();
                flush!();
                toks.push(Tok::Break);
            }
            '<' => {
                flush!();
                if chars.peek() == Some(&'<') {
                    chars.next();
                    let dash = chars.peek() == Some(&'-');
                    if dash {
                        chars.next();
                    }
                    let delim = read_delimiter(&mut chars)?;
                    pending_heredocs.push((delim, dash));
                    toks.push(Tok::Heredoc);
                    toks.push(Tok::Word(String::new()));
                } else {
                    toks.push(Tok::Redirect);
                }
            }
            '>' => {
                // `2>`, `&>` and `>>` are all redirections; the digit or `&`
                // already sits in `word` and is not a command.
                word.clear();
                has_word = false;
                if chars.peek() == Some(&'(') {
                    // `>(cmd)` process substitution: `cmd` runs as its own
                    // process, fed through the write end of a pipe. Same
                    // command position as `<(cmd)`, mirrored.
                    chars.next();
                    toks.push(Tok::Break);
                    continue;
                }
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                // `2>&1` duplicates a descriptor. The `&` belongs to the
                // redirection, not to a job-control break, so consume the whole
                // target here rather than letting `&` split the segment.
                if chars.peek() == Some(&'&') {
                    chars.next();
                    while chars
                        .peek()
                        .is_some_and(|c| c.is_ascii_digit() || *c == '-')
                    {
                        chars.next();
                    }
                    continue;
                }
                toks.push(Tok::Redirect);
            }
            c if BREAKS.contains(&c) => {
                flush!();
                if (c == '|' || c == '&') && chars.peek() == Some(&c) {
                    chars.next();
                }
                toks.push(Tok::Break);
            }
            c if c.is_whitespace() => flush!(),
            c => {
                word.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        toks.push(Tok::Word(word));
    }
    Some(toks)
}

/// Read a heredoc terminator, honouring `<<'EOF'` and `<<"EOF"` quoting.
fn read_delimiter(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
    while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
        chars.next();
    }
    let quote = match chars.peek() {
        Some('\'') => Some('\''),
        Some('"') => Some('"'),
        _ => None,
    };
    if quote.is_some() {
        chars.next();
    }
    let mut delim = String::new();
    while let Some(&c) = chars.peek() {
        match quote {
            Some(q) if c == q => {
                chars.next();
                break;
            }
            None if c.is_whitespace() || BREAKS.contains(&c) => break,
            _ => {
                delim.push(c);
                chars.next();
            }
        }
    }
    (!delim.is_empty()).then_some(delim)
}

/// Consume every queued heredoc body, up to and including its terminator line.
fn skip_heredoc_bodies(
    chars: &mut std::iter::Peekable<std::str::Chars>,
    pending: &mut Vec<(String, bool)>,
) {
    for (delim, dash) in pending.drain(..) {
        loop {
            let mut line = String::new();
            let mut saw_any = false;
            for c in chars.by_ref() {
                saw_any = true;
                if c == '\n' {
                    break;
                }
                line.push(c);
            }
            let candidate = if dash {
                line.trim_start()
            } else {
                line.as_str()
            };
            if candidate == delim || !saw_any {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heads(cmd: &str) -> Vec<String> {
        segments(cmd)
            .into_iter()
            .filter_map(|s| s.first().cloned())
            .collect()
    }

    #[test]
    fn a_plain_command_is_one_segment() {
        assert_eq!(segments("vite dev"), vec![vec!["vite", "dev"]]);
    }

    #[test]
    fn operators_start_a_new_segment() {
        assert_eq!(heads("foo && next dev"), vec!["foo", "next"]);
        assert_eq!(heads("cd x; uvicorn app"), vec!["cd", "uvicorn"]);
        assert_eq!(heads("a | b"), vec!["a", "b"]);
        assert_eq!(heads("(cd x && vite)"), vec!["cd", "vite"]);
        assert_eq!(heads("echo $(vite dev)"), vec!["echo", "vite"]);
    }

    #[test]
    fn a_quoted_operator_does_not_split() {
        assert_eq!(
            heads(r#"git commit -m "fix: crash under uvicorn; retry""#),
            vec!["git"]
        );
        assert_eq!(heads(r#"gh pr create --body "x && next dev""#), vec!["gh"]);
    }

    #[test]
    fn a_double_dash_does_not_split() {
        assert_eq!(heads("cargo run -- next dev"), vec!["cargo"]);
        assert_eq!(heads("rg -- vite"), vec!["rg"]);
    }

    #[test]
    fn a_redirection_ampersand_does_not_split() {
        assert_eq!(heads("bun run dev > /tmp/x.log 2>&1 &"), vec!["bun"]);
    }

    #[test]
    fn a_descriptor_duplication_leaves_no_stray_argument() {
        // `1` from `2>&1` must not survive as an argument to the command.
        assert_eq!(segments("vite dev 2>&1"), vec![vec!["vite", "dev"]]);
    }

    #[test]
    fn a_redirection_target_is_not_a_command_word() {
        assert_eq!(heads("cat notes.md > vite"), vec!["cat"]);
    }

    #[test]
    fn a_heredoc_body_is_inert() {
        let cmd = "cat > notes.md <<EOF\nnext dev\nuvicorn app\nEOF\nls";
        assert_eq!(heads(cmd), vec!["cat", "ls"]);
    }

    #[test]
    fn a_quoted_heredoc_delimiter_is_honoured() {
        let cmd = "cat <<'EOF'\nvite dev\nEOF";
        assert_eq!(heads(cmd), vec!["cat"]);
    }

    #[test]
    fn a_dash_heredoc_allows_an_indented_terminator() {
        let cmd = "cat <<-EOF\nvite dev\n\tEOF\nls";
        assert_eq!(heads(cmd), vec!["cat", "ls"]);
    }

    #[test]
    fn an_unterminated_quote_yields_no_segments() {
        assert!(segments("echo \"unterminated").is_empty());
    }

    #[test]
    fn a_backtick_substitution_starts_a_new_segment() {
        assert_eq!(heads("echo `vite dev`"), vec!["echo", "vite"]);
    }

    #[test]
    fn process_substitution_spawns_its_own_segment() {
        assert_eq!(
            heads("diff <(vite dev) <(true)"),
            vec!["diff", "vite", "true"]
        );
        assert_eq!(heads("echo hi >(cat)"), vec!["echo", "cat"]);
    }

    #[test]
    fn a_comment_is_not_command_text() {
        assert_eq!(
            segments("cargo build   # TODO(next dev)"),
            vec![vec!["cargo", "build"]]
        );
        assert_eq!(heads("ls # build && next dev"), vec!["ls"]);
        assert_eq!(heads("ls # then run: cd x; uvicorn app"), vec!["ls"]);
    }

    #[test]
    fn a_comment_ends_at_the_newline() {
        assert_eq!(
            heads("ls # build && next dev\nuvicorn app"),
            vec!["ls", "uvicorn"]
        );
    }

    #[test]
    fn a_hash_inside_a_word_is_an_ordinary_character() {
        assert_eq!(segments("echo foo#bar"), vec![vec!["echo", "foo#bar"]]);
    }

    #[test]
    fn a_quoted_hash_is_not_a_comment() {
        assert_eq!(
            segments(r##"echo "# not a comment""##),
            vec![vec!["echo", "# not a comment"]]
        );
    }

    #[test]
    fn a_trailing_backslash_is_a_literal_character() {
        assert_eq!(segments("vite dev\\"), vec![vec!["vite", "dev\\"]]);
    }
}
