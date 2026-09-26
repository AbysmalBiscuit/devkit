//! Bash: statements in execution order, literal bindings, redirects,
//! substitutions, heredocs, and the commands handed to the pipeline.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    catalog, embed,
    model::{FileOp, Language, Limit, TempLocation, UncertaintyKind, Value},
    normalize, paths, ts,
};

const MAX_LOOP_WORDS: usize = 32;

#[derive(Debug, Clone, Default, PartialEq)]
struct Scope {
    vars: HashMap<String, Value>,
    cwd: Option<String>,
    positional: Vec<Value>,
    functions: HashMap<String, std::ops::Range<usize>>,
}

impl Scope {
    /// Fold the bindings of a branch that may or may not have run back into
    /// this scope: anything the branch changed is no longer a single value.
    fn merge_uncertain(&mut self, branch: &Scope) {
        for (name, value) in &branch.vars {
            if self.vars.get(name) != Some(value) {
                self.vars.insert(name.clone(), Value::Unknown);
            }
        }
        if branch.cwd != self.cwd {
            self.cwd = None;
        }
    }
}

pub(crate) fn walk(a: &mut Analyzer<'_>, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(Language::Bash, source) else {
        a.uncertain(
            UncertaintyKind::ParseError,
            "bash source did not parse",
            frame.locate(0..source.len()),
        );
        return;
    };
    let mut scope = Scope {
        cwd: frame.cwd.clone(),
        positional: frame.script_args.clone(),
        ..Scope::default()
    };
    let mut w = Walker {
        a,
        source,
        frame,
        root: tree.root_node(),
        exhausted: false,
    };
    w.statements(tree.root_node(), &mut scope);
}

/// One plain command with every word known, or `None`.
#[allow(dead_code)]
pub(crate) fn simple_argv(source: &str) -> Option<Vec<String>> {
    let tree = ts::parse(Language::Bash, source)?;
    let root = tree.root_node();
    let [command] = ts::named_children(root)[..] else {
        return None;
    };
    if command.kind() != "command" || command.has_error() {
        return None;
    }
    ts::named_children(command)
        .into_iter()
        .map(|n| simple_argv_word(n, source))
        .map(|w| w.filter(|w| !w.contains(['$', '*', '?', '`'])))
        .collect()
}

fn simple_argv_word(node: Node<'_>, source: &str) -> Option<String> {
    let text = ts::text(node, source);
    match node.kind() {
        "command_name" | "word" if has_unescaped(text, &['{', '}']) => None,
        "command_name" | "word" => Some(unescape(text)),
        "raw_string" => Some(text.trim_matches('\'').to_string()),
        "string_content" => Some(unescape_dquoted(text)),
        "string" => {
            let mut out = String::new();
            let mut last = node.start_byte() + 1;
            for part in ts::named_children(node) {
                out.push_str(&unescape_dquoted(&source[last..part.start_byte()]));
                out.push_str(&simple_argv_word(part, source)?);
                last = part.end_byte();
            }
            out.push_str(&unescape_dquoted(
                &source[last..node.end_byte().saturating_sub(1).max(last)],
            ));
            Some(out)
        }
        "concatenation" => ts::named_children(node)
            .into_iter()
            .map(|part| simple_argv_word(part, source))
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.concat()),
        _ => None,
    }
}

struct Walker<'a, 'c, 's, 't> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
    root: Node<'t>,
    exhausted: bool,
}

impl<'t> Walker<'_, '_, '_, 't> {
    fn visit(&mut self, node: Node<'t>) -> bool {
        if self.exhausted {
            return false;
        }
        if self.a.budget.visit().is_err() {
            self.exhausted = true;
            let at = self.frame.locate(node.byte_range());
            self.a.uncertain(
                UncertaintyKind::LimitExhausted(Limit::Nodes),
                "bash source was only partly analyzed",
                at,
            );
            return false;
        }
        true
    }

    fn statements(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            self.statement(child, scope, Stdin::None);
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        if !self.visit(node) {
            return;
        }
        if node.has_error() && !matches!(node.kind(), "program" | "compound_statement" | "subshell")
        {
            self.broken(node);
            return;
        }
        match node.kind() {
            "comment" => {}
            "command" => self.command(node, scope, stdin),
            "redirected_statement" => self.redirected(node, scope, stdin),
            "variable_assignment" => self.assign(node, scope),
            "variable_assignments" | "declaration_command" => {
                for child in ts::named_children(node) {
                    if child.kind() == "variable_assignment" {
                        self.assign(child, scope);
                    }
                }
            }
            "list" => self.list(node, scope),
            "pipeline" => self.pipeline(ts::named_children(node), scope, stdin),
            "subshell" => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
            }
            "compound_statement" | "negated_command" => self.statements(node, scope),
            "for_statement" => self.for_loop(node, scope),
            "if_statement"
            | "while_statement"
            | "case_statement"
            | "c_style_for_statement"
            | "elif_clause"
            | "else_clause"
            | "do_group"
            | "case_item" => {
                let mut branch = scope.clone();
                self.statements(node, &mut branch);
                scope.merge_uncertain(&branch);
            }
            "function_definition" => {
                if let (Some(name), Some(body)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("body"),
                ) {
                    scope
                        .functions
                        .insert(ts::text(name, self.source).to_string(), body.byte_range());
                }
            }
            _ => self.substitutions(node, scope),
        }
    }

    /// A statement the parser could not read. Uncertain only if it could write.
    fn broken(&mut self, node: Node<'t>) {
        let mut could_write = false;
        let mut program: Option<String> = None;
        let mut cursor = node.walk();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if !n.is_named() && matches!(ts::text(n, self.source), ">" | ">>" | "&>" | "&>>" | ">|")
            {
                could_write = true;
            }
            // Inside an ERROR node the grammar often emits bare `word` nodes
            // with no `command_name`; the first one stands in for it.
            if program.is_none() && matches!(n.kind(), "command_name" | "word") {
                program = Some(normalize::basename(ts::text(n, self.source)).to_string());
            }
            stack.extend(
                n.children(&mut cursor)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
        let program_could_write = match &program {
            None => true,
            Some(p) => catalog::is_cataloged(p) || embed::is_interpreter(p),
        };
        if could_write || program_could_write {
            let at = self.frame.locate(node.byte_range());
            self.a.uncertain(
                UncertaintyKind::ParseError,
                "a statement could not be parsed",
                at,
            );
        }
    }

    fn list(&mut self, node: Node<'t>, scope: &mut Scope) {
        let mut cursor = node.walk();
        let mut after_or = false;
        for child in node.children(&mut cursor).collect::<Vec<_>>() {
            if !child.is_named() {
                after_or = ts::text(child, self.source) == "||";
                continue;
            }
            if after_or {
                let mut branch = scope.clone();
                self.statement(child, &mut branch, Stdin::None);
                scope.merge_uncertain(&branch);
            } else {
                self.statement(child, scope, Stdin::None);
            }
        }
    }

    /// Each element runs in its own subshell. What a known producer writes to
    /// stdout becomes the next element's stdin; any other producer hands the
    /// next element stdin of unknown content.
    fn pipeline(&mut self, elements: Vec<Node<'t>>, scope: &Scope, first_stdin: Stdin) {
        let mut stdin = first_stdin;
        for element in elements {
            let produced = self.produced_output(element, scope);
            let mut inner = scope.clone();
            self.statement(element, &mut inner, stdin);
            stdin = Stdin::Source {
                value: produced,
                span: element.byte_range(),
            };
        }
    }

    /// The stdout of `echo`, `printf '%s'`, or `cat` fed by a heredoc, when
    /// statically known.
    fn produced_output(&mut self, node: Node<'t>, scope: &Scope) -> Value {
        let (command, heredoc) = match node.kind() {
            "command" => (node, None),
            "redirected_statement" => (
                match node.child_by_field_name("body") {
                    Some(b) if b.kind() == "command" => b,
                    _ => return Value::Unknown,
                },
                ts::named_children(node)
                    .into_iter()
                    .find(|n| matches!(n.kind(), "heredoc_redirect" | "herestring_redirect")),
            ),
            _ => return Value::Unknown,
        };
        let mut probe = scope.clone();
        let words: Vec<Word> = ts::named_children(command)
            .into_iter()
            .filter(|n| n.kind() != "variable_assignment")
            .map(|n| self.word(n, &mut probe))
            .collect();
        let Some(program) = words
            .first()
            .and_then(|w| w.value.known())
            .map(normalize::basename)
        else {
            return Value::Unknown;
        };
        let rest: Option<Vec<&str>> = words[1..].iter().map(|w| w.value.known()).collect();
        match (program, rest, heredoc) {
            ("echo", Some(rest), None) if rest.first().is_none_or(|w| !w.starts_with('-')) => {
                Value::Known(format!("{}\n", rest.join(" ")))
            }
            ("printf", Some(rest), None) => match rest.as_slice() {
                [single] if !single.contains('%') && !single.contains('\\') => {
                    Value::Known((*single).to_string())
                }
                [fmt, arg] if matches!(*fmt, "%s" | "%s\\n") => Value::Known((*arg).to_string()),
                _ => Value::Unknown,
            },
            ("cat", Some(rest), Some(h)) if rest.is_empty() => match self.stdin_of(h, &mut probe) {
                Stdin::Source { value, .. } => value,
                Stdin::None => Value::Unknown,
            },
            _ => Value::Unknown,
        }
    }

    fn redirected(&mut self, node: Node<'t>, scope: &mut Scope, mut stdin: Stdin) {
        if node
            .child_by_field_name("body")
            .is_some_and(|body| body.kind() == "list")
        {
            let body = node.child_by_field_name("body").expect("checked above");
            self.statement(body, scope, stdin);
            for redirect in ts::named_children(node) {
                if redirect.kind() == "file_redirect" {
                    self.file_redirect(redirect, scope);
                }
            }
            return;
        }
        let mut trailing_pipeline: Option<Node<'t>> = None;
        for redirect in ts::named_children(node) {
            match redirect.kind() {
                "file_redirect" => self.file_redirect(redirect, scope),
                "heredoc_redirect" | "herestring_redirect" => {
                    stdin = self.stdin_of(redirect, scope);
                    trailing_pipeline = ts::named_children(redirect).into_iter().find(|n| {
                        matches!(
                            n.kind(),
                            "pipeline" | "command" | "list" | "redirected_statement"
                        )
                    });
                }
                _ => {}
            }
        }
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        match trailing_pipeline {
            None => self.statement(body, scope, stdin),
            Some(rest) => {
                let produced = self.produced_output(node, scope);
                let mut inner = scope.clone();
                self.statement(body, &mut inner, stdin);
                let elements = match rest.kind() {
                    "pipeline" => ts::named_children(rest),
                    _ => vec![rest],
                };
                self.pipeline(elements, scope, Stdin::Source {
                    value: produced,
                    span: body.byte_range(),
                });
            }
        }
    }

    fn file_redirect(&mut self, node: Node<'t>, scope: &mut Scope) {
        let mut cursor = node.walk();
        let operator = node
            .children(&mut cursor)
            .find(|c| !c.is_named())
            .map(|c| ts::text(c, self.source))
            .unwrap_or("");
        let op = match operator {
            ">" | ">|" | "&>" => FileOp::Overwrite,
            ">>" | "&>>" => FileOp::Append,
            _ => return,
        };
        let Some(dest) = node.child_by_field_name("destination") else {
            return;
        };
        let word = if ts::text(dest, self.source).contains('{')
            && ts::text(dest, self.source).contains(',')
        {
            Word {
                typed: ts::text(dest, self.source).to_string(),
                value: Value::Unknown,
                span: dest.byte_range(),
            }
        } else {
            self.word(dest, scope)
        };
        if matches!(
            word.value.known(),
            Some("/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty")
        ) || dest.kind() == "number"
        {
            return;
        }
        let at = self.frame.locate(node.byte_range());
        self.a
            .file_effect(op, &word.value, scope.cwd.as_deref(), at);
    }

    fn stdin_of(&mut self, redirect: Node<'t>, scope: &mut Scope) -> Stdin {
        let span = redirect.byte_range();
        if redirect.kind() == "herestring_redirect" {
            let value = ts::named_children(redirect)
                .into_iter()
                .last()
                .map(|n| self.word(n, scope).value)
                .map(|v| match v {
                    Value::Known(s) => Value::Known(format!("{s}\n")),
                    Value::Unknown | Value::Ephemeral(_) | Value::Within(_) => Value::Unknown,
                })
                .unwrap_or(Value::Unknown);
            return Stdin::Source { value, span };
        }
        let quoted = ts::named_children(redirect)
            .into_iter()
            .find(|n| n.kind() == "heredoc_start")
            .is_some_and(|s| ts::text(s, self.source).contains(['\'', '"', '\\']));
        let Some(body) = ts::named_children(redirect)
            .into_iter()
            .find(|n| n.kind() == "heredoc_body")
        else {
            return Stdin::Source {
                value: Value::Known(String::new()),
                span,
            };
        };
        if quoted {
            return Stdin::Source {
                value: Value::Known(ts::text(body, self.source).to_string()),
                span: body.byte_range(),
            };
        }
        let mut expands = false;
        for part in ts::named_children(body) {
            match part.kind() {
                "command_substitution" | "process_substitution" => {
                    expands = true;
                    let mut inner = scope.clone();
                    self.statements(part, &mut inner);
                }
                "simple_expansion" | "expansion" | "arithmetic_expansion" => expands = true,
                _ => {}
            }
        }
        let value = if expands {
            Value::Unknown
        } else {
            Value::Known(ts::text(body, self.source).to_string())
        };
        Stdin::Source {
            value,
            span: body.byte_range(),
        }
    }

    fn assign(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(name) = node.child_by_field_name("name") else {
            return;
        };
        let name = ts::text(name, self.source).to_string();
        let appends = ts::text(node, self.source).contains("+=");
        let value = match node.child_by_field_name("value") {
            None => Value::Known(String::new()),
            Some(v) if v.kind() == "array" || appends => {
                self.substitutions(v, scope);
                Value::Unknown
            }
            Some(v) => self.word(v, scope).value,
        };
        scope.vars.insert(name, value);
    }

    fn for_loop(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(var) = node
            .child_by_field_name("variable")
            .map(|v| ts::text(v, self.source).to_string())
        else {
            return;
        };
        let Some(body) = node.child_by_field_name("body") else {
            return;
        };
        let mut cursor = node.walk();
        let value_nodes: Vec<Node<'t>> =
            node.children_by_field_name("value", &mut cursor).collect();
        let values: Vec<Value> = value_nodes
            .iter()
            .map(|n| self.word(*n, scope).value)
            .collect();
        let literal = !values.is_empty()
            && values.len() <= MAX_LOOP_WORDS
            && values
                .iter()
                .all(|v| matches!(v, Value::Known(_) | Value::Within(_)));
        let mut after = scope.clone();
        if literal {
            for value in values {
                let mut iteration = scope.clone();
                iteration.vars.insert(var.clone(), value);
                self.statements(body, &mut iteration);
                after.merge_uncertain(&iteration);
            }
        } else {
            let mut iteration = scope.clone();
            iteration.vars.insert(var.clone(), Value::Unknown);
            self.statements(body, &mut iteration);
            after.merge_uncertain(&iteration);
        }
        after.vars.insert(var, Value::Unknown);
        *scope = after;
    }

    fn command(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        let mut words: Vec<Word> = Vec::new();
        for child in ts::named_children(node) {
            match child.kind() {
                "variable_assignment" => self.substitutions(child, scope),
                "file_redirect" => self.file_redirect(child, scope),
                _ => words.push(self.word(child, scope)),
            }
        }
        let Some(first) = words.first() else {
            return;
        };
        match first.value.known() {
            Some("cd" | "pushd" | "Set-Location") => {
                scope.cwd = match words.get(1).map(|w| &w.value) {
                    Some(v @ Value::Known(p)) if p != "-" => {
                        match paths::resolve(v, scope.cwd.as_deref(), self.a.ctx.path_style) {
                            crate::model::Target::Path(dir) => Some(dir),
                            crate::model::Target::Unresolved
                            | crate::model::Target::Ephemeral { .. }
                            | crate::model::Target::Within(_) => None,
                        }
                    }
                    _ => None,
                };
                return;
            }
            Some("popd") => {
                scope.cwd = None;
                return;
            }
            Some("read" | "mapfile" | "readarray") => {
                for w in &words[1..] {
                    if let Some(name) = w.value.known().filter(|n| !n.starts_with('-')) {
                        scope.vars.insert(name.to_string(), Value::Unknown);
                    }
                }
                return;
            }
            Some(name) if scope.functions.contains_key(name) => {
                let body = scope.functions[name].clone();
                if let Some(body) = self.root.descendant_for_byte_range(body.start, body.end) {
                    let mut call = scope.clone();
                    call.positional = std::iter::once(Value::Known(name.to_string()))
                        .chain(words[1..].iter().map(|w| w.value.clone()))
                        .collect();
                    self.statements(body, &mut call);
                }
                return;
            }
            _ => {}
        }
        let raw = RawInvocation {
            words,
            stdin,
            cwd: scope.cwd.clone(),
            language: Language::Bash,
            location: self.frame.locate(node.byte_range()),
        };
        self.a.invocation(raw, self.frame);
    }

    /// Run every substitution under `node` for its effects.
    fn substitutions(&mut self, node: Node<'t>, scope: &Scope) {
        for child in ts::named_children(node) {
            match child.kind() {
                "command_substitution" | "process_substitution" => {
                    let mut inner = scope.clone();
                    self.statements(child, &mut inner);
                }
                _ => self.substitutions(child, scope),
            }
        }
    }

    fn word(&mut self, node: Node<'t>, scope: &mut Scope) -> Word {
        let typed = ts::text(node, self.source);
        let value = self.value(node, scope);
        let value = match value {
            Value::Known(s) if !self.a.budget.value_fits(s.len()) => Value::Unknown,
            v => v,
        };
        Word {
            typed: value
                .known()
                .map_or_else(|| typed.to_string(), str::to_string),
            value,
            span: node.byte_range(),
        }
    }

    fn value(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let text = ts::text(node, self.source);
        match node.kind() {
            "word" | "number" | "command_name" => {
                if text.starts_with('~') || (text.contains('{') && text.contains(',')) {
                    return Value::Unknown;
                }
                if has_unescaped(text, GLOB) {
                    return glob_bound(text);
                }
                if let Some(inner) = ts::named_children(node).into_iter().next() {
                    return self.value(inner, scope);
                }
                Value::Known(unescape(text))
            }
            "raw_string" => Value::Known(text[1..text.len() - 1].to_string()),
            "ansi_c_string" if !text.contains('\\') => {
                Value::Known(text[2..text.len() - 1].to_string())
            }
            "string" => {
                let mut out = String::new();
                let mut known = true;
                let mut whole_word = None;
                let mut parts = 0;
                let mut last = node.start_byte() + 1;
                for part in ts::named_children(node) {
                    out.push_str(&unescape_dquoted(&self.source[last..part.start_byte()]));
                    last = part.end_byte();
                    parts += 1;
                    let value = match part.kind() {
                        "simple_expansion" | "expansion" => self.expansion(part, scope),
                        _ => self.value(part, scope),
                    };
                    match value {
                        Value::Known(s) => out.push_str(&s),
                        Value::Unknown => known = false,
                        v @ (Value::Ephemeral(_) | Value::Within(_)) => whole_word = Some(v),
                    }
                }
                out.push_str(&unescape_dquoted(
                    &self.source[last..node.end_byte().saturating_sub(1).max(last)],
                ));
                match whole_word {
                    // A temp path or a match stays bounded only as the whole
                    // word: anything appended can climb out, and a failed
                    // `mktemp` leaves the rest standing alone as a path.
                    Some(v) if parts == 1 && out.is_empty() => v,
                    Some(_) => Value::Unknown,
                    None if known => Value::Known(out),
                    None => Value::Unknown,
                }
            }
            "string_content" => Value::Known(unescape_dquoted(text)),
            "concatenation" => {
                let parts = ts::named_children(node);
                if parts
                    .iter()
                    .any(|p| p.kind() == "word" && has_unescaped(ts::text(*p, self.source), GLOB))
                {
                    // A brace expansion spans several parts.
                    if text.contains('{') && text.contains(',') {
                        self.substitutions(node, scope);
                        return Value::Unknown;
                    }
                    return self.glob_concatenation(&parts, scope);
                }
                let mut out = String::new();
                for part in parts {
                    match self.value(part, scope) {
                        Value::Known(s) => out.push_str(&s),
                        // A concatenation always has something beside the temp
                        // path or the match, which is exactly what stops it
                        // being bounded.
                        Value::Unknown | Value::Ephemeral(_) | Value::Within(_) => {
                            return Value::Unknown;
                        }
                    }
                }
                Value::Known(out)
            }
            // Word splitting can cut an unquoted match into pieces that lie
            // outside its bound.
            "simple_expansion" | "expansion" => match self.expansion(node, scope) {
                Value::Within(_) => Value::Unknown,
                v => v,
            },
            "command_substitution" => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
                match self.makes_temp_path(node, scope) {
                    Some(at) => Value::Ephemeral(at),
                    None => Value::Unknown,
                }
            }
            "process_substitution" => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
                Value::Unknown
            }
            _ => {
                self.substitutions(node, scope);
                Value::Unknown
            }
        }
    }

    /// What a parameter expansion stands for, before word splitting.
    fn expansion(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let text = ts::text(node, self.source);
        if node.kind() == "simple_expansion" {
            return self.lookup(&text[1..], scope);
        }
        let inner = &text[2..text.len() - 1];
        if inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            self.lookup(inner, scope)
        } else {
            self.substitutions(node, scope);
            Value::Unknown
        }
    }

    /// A word joining quoted text to an unquoted glob, such as `"$d"/*.rs`.
    /// The quoted parts stay literal, so they are escaped before the pattern
    /// is bounded. An unquoted expansion is split and matched by the shell, so
    /// it leaves the word unknown.
    fn glob_concatenation(&mut self, parts: &[Node<'t>], scope: &mut Scope) -> Value {
        let mut pattern = String::new();
        for (i, part) in parts.iter().enumerate() {
            let text = ts::text(*part, self.source);
            match part.kind() {
                "word" if i == 0 && text.starts_with('~') => return Value::Unknown,
                "word" => pattern.push_str(text),
                "string" | "raw_string" | "ansi_c_string" => match self.value(*part, scope) {
                    Value::Known(s) => pattern.push_str(&escape_glob(&s)),
                    _ => return Value::Unknown,
                },
                _ => {
                    self.value(*part, scope);
                    return Value::Unknown;
                }
            }
        }
        glob_bound(&pattern)
    }

    /// Where a lone `mktemp` substitution creates its entry, when the
    /// substitution is one: its stdout is then a path the command has already
    /// created under a random name. An argument carrying an expansion reads as
    /// not a temp path at all, because the expansion could be `-u`.
    fn makes_temp_path(&self, node: Node<'t>, scope: &Scope) -> Option<TempLocation> {
        let children = ts::named_children(node);
        let [command] = children.as_slice() else {
            return None;
        };
        if command.kind() != "command" {
            return None;
        }
        // The words the shell would pass, not their source text: `-p 'held'`
        // names the directory `held`, quotes and all removed.
        let words: Vec<String> = ts::named_children(*command)
            .into_iter()
            .filter(|n| n.kind() != "variable_assignment")
            .map(|n| simple_argv_word(n, self.source))
            .collect::<Option<Vec<String>>>()?;
        let (program, args) = words.split_first()?;
        if normalize::basename(program) != "mktemp"
            || args.iter().any(|a| a.contains('$') || is_dry_run(a))
        {
            return None;
        }
        let mut named = None;
        let mut system = false;
        let mut template = None;
        let mut i = 0;
        while let Some(arg) = args.get(i) {
            if let Some(dir) = arg.strip_prefix("--tmpdir=") {
                named = Some(dir);
            } else if arg == "--tmpdir" {
                system = true;
            } else if arg == "-p" {
                named = Some(args.get(i + 1)?.as_str());
                i += 1;
            } else if !arg.starts_with('-') {
                template = Some(arg.as_str());
            }
            i += 1;
        }
        // A template is a path prefix the random characters are appended to, so
        // the entry is made in its parent. A relative template hangs off the
        // directory `-p` names rather than replacing it.
        let style = self.a.ctx.path_style;
        let dir = match template {
            // Once a directory is named, `mktemp` refuses an absolute template
            // outright and creates nothing at all.
            Some(t) if paths::is_absolute(t, style) => {
                if named.is_some() || system {
                    return None;
                }
                paths::parent_dir(t, style)?
            }
            Some(t) => {
                let under = paths::parent_dir(t, style)?;
                match (named, system) {
                    (Some(base), _) => paths::join(base, &under, style),
                    // `$TMPDIR` is not knowable, so a template carrying a
                    // directory of its own lands somewhere this cannot name.
                    (None, true) if under != "." => return None,
                    (None, true) => return Some(TempLocation::SystemTemp),
                    (None, false) => under,
                }
            }
            None => match named {
                Some(base) => base.to_string(),
                None => return Some(TempLocation::SystemTemp),
            },
        };
        match paths::resolve(
            &Value::Known(dir),
            scope.cwd.as_deref(),
            self.a.ctx.path_style,
        ) {
            crate::model::Target::Path(dir) => Some(TempLocation::In(dir)),
            _ => None,
        }
    }

    fn lookup(&self, name: &str, scope: &Scope) -> Value {
        if let Ok(i) = name.parse::<usize>() {
            return scope.positional.get(i).cloned().unwrap_or(Value::Unknown);
        }
        scope.vars.get(name).cloned().unwrap_or(Value::Unknown)
    }
}

/// `mktemp -u` prints a name without creating anything, leaving the path free
/// for another process to take.
fn is_dry_run(arg: &str) -> bool {
    arg == "--dry-run" || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('u'))
}

const GLOB: &[char] = &['*', '?', '['];

/// The directory every match of an unquoted glob lies under: the components
/// before the first one that pattern-matches. A later `..` climbs back out,
/// and so can a pattern starting with a dot, which matches `..`.
fn glob_bound(pattern: &str) -> Value {
    let components: Vec<&str> = pattern.split('/').collect();
    let Some(first) = components.iter().position(|c| has_unescaped(c, GLOB)) else {
        return Value::Known(unescape(pattern));
    };
    let climbs = |c: &&str| {
        let literal = unescape(c);
        literal == ".." || (literal.starts_with('.') && has_unescaped(c, GLOB))
    };
    if components[first..].iter().any(climbs) {
        return Value::Unknown;
    }
    let dir = unescape(&components[..first].join("/"));
    Value::Within(match (dir.is_empty(), first) {
        (false, _) => dir,
        (true, 0) => ".".to_string(),
        (true, _) => "/".to_string(),
    })
}

fn escape_glob(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '\\' || GLOB.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn has_unescaped(text: &str, chars: &[char]) -> bool {
    let mut escaped = false;
    text.chars().any(|c| {
        let hit = !escaped && chars.contains(&c);
        escaped = !escaped && c == '\\';
        hit
    })
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

/// Inside double quotes a backslash escapes only `$`, `` ` ``, `"`, `\` and a
/// newline.
fn unescape_dquoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some('$' | '`' | '"' | '\\')) => out.extend(chars.next()),
            ('\\', Some('\n')) => {
                chars.next();
            }
            (c, _) => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod shapes {
    use crate::{model::Language, ts};

    #[test]
    fn node_shapes_this_adapter_relies_on() {
        let cases = [
            (
                "echo x > a.txt",
                "(redirected_statement body: (command name: (command_name (word)) argument: (word)) redirect: (file_redirect destination: (word)))",
            ),
            (
                "cat > n.md <<'EOF'\nhi\nEOF\n",
                "redirect: (heredoc_redirect (heredoc_start) (heredoc_body)",
            ),
            (
                "cat <<EOF | python3 -\nprint(1)\nEOF\n",
                "redirect: (heredoc_redirect (heredoc_start) (pipeline",
            ),
            (
                "f=a; echo \"$f\"",
                "(variable_assignment name: (variable_name) value: (word))",
            ),
            (
                "for f in a b; do echo $f; done",
                "(for_statement variable: (variable_name) value: (word) value: (word) body: (do_group",
            ),
        ];
        for (source, expected) in cases {
            let tree = ts::parse(Language::Bash, source).unwrap();
            let sexp = tree.root_node().to_sexp();
            assert!(sexp.contains(expected), "{source:?}\n{sexp}");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        model::{FileOp, Target, UncertaintyKind, Value},
        testutil::{bash, programs, targets, writes},
    };

    #[test]
    fn operators_start_new_invocations() {
        assert_eq!(
            programs(&bash("foo && next dev; cd sub; uvicorn app:app")),
            ["foo", "next", "uvicorn"]
        );
        let mut substitution = programs(&bash("echo $(vite dev)"));
        substitution.sort();
        assert_eq!(substitution, ["echo", "vite"]);
    }

    #[test]
    fn quoted_operators_and_double_dash_stay_arguments() {
        assert_eq!(programs(&bash(r#"git commit -m "fix: uvicorn; retry""#)), [
            "git"
        ]);
        assert_eq!(programs(&bash("cargo run -- next dev")), ["cargo"]);
    }

    #[test]
    fn redirects_do_not_create_invocations_or_stray_arguments() {
        assert_eq!(programs(&bash("bun test > /tmp/x.log 2>&1 &")), ["bun"]);
        let a = bash("vite dev 2>&1");
        assert_eq!(a.invocations[0].args, [Value::Known("dev".into())]);
        assert_eq!(programs(&bash("cat notes.md > vite")), ["cat"]);
    }

    #[test]
    fn redirects_overwrite_and_append() {
        let a = bash("echo hi > a.txt; echo x >> b.txt");
        assert_eq!(targets(&a), ["/repo/a.txt", "/repo/b.txt"]);
        assert_eq!(a.file_effects[0].op, FileOp::Overwrite);
        assert_eq!(a.file_effects[1].op, FileOp::Append);
    }

    #[test]
    fn an_mktemp_path_is_ephemeral_only_as_the_whole_word() {
        assert_eq!(targets(&bash("T=$(mktemp); echo x > \"$T\"")), [
            "<ephemeral>"
        ]);
        assert_eq!(targets(&bash("T=$(mktemp); echo x > $T")), ["<ephemeral>"]);
        // A `mktemp` that failed leaves `$D` empty, which would make the rest
        // of the word an absolute path of its own.
        assert_eq!(targets(&bash("D=$(mktemp -d); echo x > \"$D/out.txt\"")), [
            "?"
        ]);
        assert_eq!(targets(&bash("T=$(mktemp -u); echo x > \"$T\"")), ["?"]);
    }

    #[test]
    fn an_mktemp_template_names_the_directory_the_entry_is_made_in() {
        let dir = |source| match &bash(source).file_effects[0].target {
            Target::Ephemeral { at } => at.named().map(str::to_string),
            other => panic!("{other:?}"),
        };
        assert_eq!(dir("T=$(mktemp); echo x > \"$T\""), None);
        assert_eq!(
            dir("T=$(mktemp ./fresh.XXXX); echo x > \"$T\""),
            Some("/repo".to_string())
        );
        assert_eq!(
            dir("T=$(mktemp -p sub); echo x > \"$T\""),
            Some("/repo/sub".to_string())
        );
        assert_eq!(
            dir("T=$(mktemp --tmpdir=/var/tmp); echo x > \"$T\""),
            Some("/var/tmp".to_string())
        );
        // A relative template hangs off `-p`, it does not replace it.
        assert_eq!(
            dir("T=$(mktemp -p /held fresh.XXXX); echo x > \"$T\""),
            Some("/held".to_string())
        );
        assert_eq!(
            dir("T=$(mktemp -p /held sub/fresh.XXXX); echo x > \"$T\""),
            Some("/held/sub".to_string())
        );
        // On its own an absolute template is taken as it stands.
        assert_eq!(
            dir("T=$(mktemp /other/fresh.XXXX); echo x > \"$T\""),
            Some("/other".to_string())
        );
        // Named a directory as well, `mktemp` refuses the template and creates
        // nothing, so the substitution is empty rather than a fresh path.
        assert_eq!(
            targets(&bash(
                "T=$(mktemp -p /held /other/fresh.XXXX); echo x > \"$T\""
            )),
            ["?"]
        );
    }

    #[test]
    fn device_redirects_and_descriptor_duplication_are_not_writes() {
        let a = bash("make >/dev/null 2>&1; ls 2>/dev/stderr");
        assert!(a.file_effects.is_empty(), "{:?}", a.file_effects);
    }

    #[test]
    fn quoted_text_is_data() {
        let a = bash(r#"git commit -m "echo x > y.txt && rm -rf src""#);
        assert!(a.file_effects.is_empty());
        assert_eq!(programs(&a), ["git"]);
    }

    #[test]
    fn cd_moves_relative_targets_in_execution_order() {
        assert_eq!(
            targets(&bash("echo a > a.txt; cd sub && echo b > b.txt")),
            ["/repo/a.txt", "/repo/sub/b.txt"]
        );
    }

    #[test]
    fn a_subshell_directory_change_does_not_leak() {
        assert_eq!(targets(&bash("(cd sub); echo x > a.txt")), ["/repo/a.txt"]);
    }

    #[test]
    fn a_literal_assignment_resolves_a_later_word() {
        assert_eq!(targets(&bash("f=out.txt; echo x > \"$f\"")), [
            "/repo/out.txt"
        ]);
        assert_eq!(targets(&bash("d=gen; echo x > ${d}/a.txt")), [
            "/repo/gen/a.txt"
        ]);
    }

    #[test]
    fn an_unbound_or_conditional_variable_is_unresolved() {
        assert_eq!(targets(&bash("echo x > \"$OUT\"")), ["?"]);
        assert_eq!(targets(&bash("false || f=a.txt; echo x > \"$f\"")), ["?"]);
        assert_eq!(
            targets(&bash("if true; then f=a.txt; fi; echo x > \"$f\"")),
            ["?"]
        );
    }

    #[test]
    fn brace_expansion_is_unresolved() {
        assert_eq!(targets(&bash("echo x > {a,b}.txt")), ["?"]);
        assert_eq!(writes(&bash("rm -f {a,b}/*.rs")), ["?"]);
    }

    #[test]
    fn a_glob_is_bounded_by_the_directory_before_its_first_pattern() {
        for (source, bound) in [
            ("sed -i s/a/b/ src/*.rs", "/repo/src/**"),
            ("echo x > *.txt", "/repo/**"),
            ("rm -f src/gen/*/x.rs", "/repo/src/gen/**"),
            ("rm -f /var/log/app/*.log", "/var/log/app/**"),
            ("rm -rf build/*", "/repo/build/**"),
            ("d=src; rm -f \"$d\"/*.orig", "/repo/src/**"),
            ("rm -f 'my dir'/*.orig", "/repo/my dir/**"),
        ] {
            assert_eq!(writes(&bash(source)), [bound], "{source}");
        }
    }

    #[test]
    fn a_glob_whose_matches_can_leave_its_directory_is_unresolved() {
        for source in [
            "rm -f src/*/../x",
            "rm -f src/.*",
            "rm -f ~/src/*.rs",
            "rm -f \"$X\"/*.rs",
            "d=src; rm -f $d/*.rs",
        ] {
            assert_eq!(writes(&bash(source)), ["?"], "{source}");
        }
    }

    #[test]
    fn a_loop_over_a_glob_binds_each_match_within_its_bound() {
        for (source, expected) in [
            (
                "for f in src/*.rs; do sed -i x \"$f\"; done",
                &["/repo/src/**"][..],
            ),
            ("for f in a.txt src/*; do rm -f \"$f\"; done", &[
                "/repo/a.txt",
                "/repo/src/**",
            ]),
            // Word splitting can cut an unquoted match into pieces outside the
            // bound, and anything appended can climb out of it.
            ("for f in src/*.rs; do rm -f $f; done", &["?"]),
            ("for f in src/*.rs; do rm -f \"$f/../x\"; done", &["?"]),
        ] {
            assert_eq!(writes(&bash(source)), expected, "{source}");
        }
    }

    #[test]
    fn a_command_substitution_is_analyzed_and_its_value_is_unknown() {
        let a = bash("x=$(echo hi > s.txt); echo y > \"$x\"");
        assert_eq!(targets(&a), ["/repo/s.txt", "?"]);
    }

    #[test]
    fn heredoc_content_is_data() {
        let a = bash("cat > notes.md <<'EOF'\nrm -rf src\necho x > y.txt\nEOF\n");
        assert_eq!(programs(&a), ["cat"]);
        assert_eq!(targets(&a), ["/repo/notes.md"]);
    }

    #[test]
    fn quoted_and_indented_heredoc_delimiters_keep_body_inert() {
        assert_eq!(programs(&bash("cat <<'EOF'\nvite dev\nEOF")), ["cat"]);
        assert_eq!(programs(&bash("cat <<-EOF\nvite dev\n\tEOF\nls")), [
            "cat", "ls"
        ]);
    }

    #[test]
    fn a_substitution_inside_an_unquoted_heredoc_runs() {
        let a = bash("cat > n.md <<EOF\n$(echo t > t.txt)\nEOF\n");
        assert!(
            targets(&a).contains(&"/repo/t.txt".to_string()),
            "{:?}",
            targets(&a)
        );
    }

    #[test]
    fn backtick_and_process_substitutions_are_invocations() {
        let mut backtick = programs(&bash("echo `vite dev`"));
        backtick.sort();
        assert_eq!(backtick, ["echo", "vite"]);
        let mut process = programs(&bash("diff <(vite dev) <(true)"));
        process.sort();
        assert_eq!(process, ["diff", "true", "vite"]);
        let mut output = programs(&bash("echo hi >(cat)"));
        output.sort();
        assert_eq!(output, ["cat", "echo"]);
    }

    #[test]
    fn comments_end_at_newlines_and_hashes_inside_words_are_data() {
        assert_eq!(programs(&bash("cargo build # TODO(next dev)")), ["cargo"]);
        assert_eq!(
            programs(&bash("ls # build && next dev\nuvicorn app:app")),
            ["ls", "uvicorn"]
        );
        let a = bash(r##"echo foo#bar "# not a comment""##);
        assert_eq!(a.invocations[0].args, [
            Value::Known("foo#bar".into()),
            Value::Known("# not a comment".into())
        ]);
    }

    #[test]
    fn malformed_quotes_and_trailing_backslashes_are_grammar_values() {
        assert_eq!(programs(&bash("echo \"unterminated")), ["echo"]);
        assert_eq!(bash("vite dev\\").invocations[0].args, [Value::Known(
            "dev".into()
        )]);
    }

    #[test]
    fn a_loop_over_literal_words_is_analyzed_per_word() {
        assert_eq!(
            targets(&bash("for f in a.txt b.txt; do echo x > \"$f\"; done")),
            ["/repo/a.txt", "/repo/b.txt"]
        );
        assert_eq!(
            targets(&bash("for f in $(ls); do echo x > \"$f\"; done")),
            ["?"]
        );
    }

    #[test]
    fn a_function_is_analyzed_where_it_is_called() {
        assert_eq!(targets(&bash("w() { echo x > w.txt; }; w")), [
            "/repo/w.txt"
        ]);
        assert!(bash("w() { echo x > w.txt; }").file_effects.is_empty());
    }

    #[test]
    fn pipeline_elements_are_invocations() {
        assert_eq!(programs(&bash("cat a | grep b | sort")), [
            "cat", "grep", "sort"
        ]);
    }

    #[test]
    fn a_broken_statement_does_not_discard_its_siblings() {
        let a = bash("echo a > a.txt\necho b > b.txt )\necho c > c.txt\n");
        let t = targets(&a);
        assert!(t.contains(&"/repo/a.txt".to_string()), "{t:?}");
        assert!(t.contains(&"/repo/c.txt".to_string()), "{t:?}");
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::ParseError)
        );
    }

    #[test]
    fn a_broken_read_only_statement_is_silent() {
        let source = "if (ls -la\n";
        let tree = crate::ts::parse(crate::model::Language::Bash, source).unwrap();
        let errors: Vec<_> = crate::ts::named_children(tree.root_node())
            .into_iter()
            .filter(|node| node.kind() == "ERROR")
            .collect();
        assert_eq!(errors.len(), 1, "{}", tree.root_node().to_sexp());
        assert_eq!(crate::ts::text(errors[0], source), "if (ls -la\n");
        assert!(errors[0].to_sexp().contains("(command name:"));

        let a = bash(source);
        assert!(a.uncertainties.is_empty(), "{:?}", a.uncertainties);
    }

    #[test]
    fn simple_argv_reads_one_plain_command() {
        assert_eq!(
            super::simple_argv("bun test --watch"),
            Some(vec!["bun".into(), "test".into(), "--watch".into()])
        );
        assert_eq!(super::simple_argv("bun test && ls"), None);
        assert_eq!(super::simple_argv("bun $X"), None);
        assert_eq!(
            super::simple_argv("bun foo/bar"),
            Some(vec!["bun".into(), "foo/bar".into()])
        );
        assert_eq!(
            super::simple_argv("bun foo\"bar\""),
            Some(vec!["bun".into(), "foobar".into()])
        );
        assert_eq!(super::simple_argv("bun {a,b}"), None);
    }
}
