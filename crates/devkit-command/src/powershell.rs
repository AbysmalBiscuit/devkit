//! PowerShell: pipelines, redirects, variables, cmdlet parameter binding, the
//! file cmdlets and .NET file APIs, here-strings, and external programs.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    embed,
    model::{FileOp, Language, Limit, Location, UncertaintyKind, Value},
    normalize, paths, ts,
};

mod kinds {
    pub const PIPELINE: &str = "pipeline";
    pub const PIPELINE_CHAIN: &str = "pipeline_chain";
    pub const COMMAND: &str = "command";
    pub const COMMAND_NAME: &str = "command_name";
    pub const COMMAND_NAME_EXPR: &str = "command_name_expr";
    pub const COMMAND_ELEMENTS: &str = "command_elements";
    pub const PARAMETER: &str = "command_parameter";
    pub const REDIRECTION: &str = "redirection";
    pub const REDIRECTED_FILE: &str = "redirected_file_name";
    pub const ASSIGNMENT: &str = "assignment_expression";
    pub const VARIABLE: &str = "variable";
    pub const INVOKATION: &str = "invokation_expression";
    pub const TYPE_LITERAL: &str = "type_literal";
    pub const MEMBER_NAME: &str = "member_name";
    pub const ARGUMENT_LIST: &str = "argument_list";
    pub const FOREACH: &str = "foreach_statement";
    pub const FUNCTION: &str = "function_statement";
    pub const SUB_EXPRESSION: &str = "sub_expression";
    pub const SCRIPT_BLOCK: &str = "script_block";
    pub const SCRIPT_BLOCK_BODY: &str = "script_block_body";
    pub const STATEMENT_BLOCK: &str = "statement_block";
}

const MAX_LOOP_ITEMS: usize = 32;

#[derive(Debug, Clone, Default, PartialEq)]
struct Scope {
    vars: HashMap<String, Value>,
    cwd: Option<String>,
    functions: HashMap<String, std::ops::Range<usize>>,
}

impl Scope {
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

struct Cmdlet {
    verb: Verb,
    params: &'static [&'static str],
    positional: &'static [&'static str],
    switches: &'static [&'static str],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    SetContent,
    AddContent,
    ClearContent,
    OutFile,
    TeeObject,
    NewItem,
    RemoveItem,
    MoveItem,
    RenameItem,
    CopyItem,
    ExportFile,
    ExpandArchive,
    WebRequest,
    SetLocation,
    PopLocation,
    InvokeExpression,
    JoinPath,
    GetLocation,
}

const PATH_ALIASES: &[(&str, &str)] =
    &[("literalpath", "path"), ("pspath", "path"), ("lp", "path")];

fn cmdlet(name: &str) -> Option<Cmdlet> {
    let c = |verb, params, positional, switches| {
        Some(Cmdlet {
            verb,
            params,
            positional,
            switches,
        })
    };
    match name.to_ascii_lowercase().as_str() {
        "set-content" => c(
            Verb::SetContent,
            &["path", "value", "encoding"],
            &["path", "value"],
            &["force", "nonewline"],
        ),
        "add-content" | "ac" => c(
            Verb::AddContent,
            &["path", "value", "encoding"],
            &["path", "value"],
            &["force", "nonewline"],
        ),
        "clear-content" | "clc" => c(Verb::ClearContent, &["path"], &["path"], &["force"]),
        "out-file" => c(
            Verb::OutFile,
            &["filepath", "path", "encoding", "inputobject", "width"],
            &["filepath"],
            &["append", "force", "noclobber", "nonewline"],
        ),
        "tee-object" | "tee" => c(
            Verb::TeeObject,
            &["filepath", "path", "variable", "inputobject"],
            &["filepath"],
            &["append"],
        ),
        "new-item" | "ni" => c(
            Verb::NewItem,
            &["path", "name", "itemtype", "value"],
            &["path"],
            &["force"],
        ),
        "remove-item" | "ri" | "rm" | "del" | "erase" | "rd" | "rmdir" => c(
            Verb::RemoveItem,
            &["path", "include", "exclude", "filter"],
            &["path"],
            &["recurse", "force"],
        ),
        "move-item" | "mi" | "mv" | "move" => c(
            Verb::MoveItem,
            &["path", "destination"],
            &["path", "destination"],
            &["force"],
        ),
        "rename-item" | "rni" | "ren" => c(
            Verb::RenameItem,
            &["path", "newname"],
            &["path", "newname"],
            &["force"],
        ),
        "copy-item" | "cpi" | "cp" | "copy" => c(
            Verb::CopyItem,
            &["path", "destination", "include", "exclude", "filter"],
            &["path", "destination"],
            &["recurse", "force", "container"],
        ),
        "export-csv" | "export-clixml" => c(
            Verb::ExportFile,
            &["path", "inputobject", "delimiter", "encoding"],
            &["path"],
            &["append", "force", "notypeinformation"],
        ),
        "expand-archive" => c(
            Verb::ExpandArchive,
            &["path", "destinationpath"],
            &["path", "destinationpath"],
            &["force"],
        ),
        "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm" | "curl" | "wget" => c(
            Verb::WebRequest,
            &["uri", "outfile", "method", "headers", "body"],
            &["uri"],
            &["usebasicparsing"],
        ),
        "set-location" | "sl" | "cd" | "chdir" | "push-location" | "pushd" => {
            c(Verb::SetLocation, &["path"], &["path"], &[])
        }
        "pop-location" | "popd" => c(Verb::PopLocation, &[], &[], &[]),
        "invoke-expression" | "iex" => c(Verb::InvokeExpression, &["command"], &["command"], &[]),
        "join-path" => c(
            Verb::JoinPath,
            &["path", "childpath"],
            &["path", "childpath"],
            &["resolve"],
        ),
        "get-location" | "gl" | "pwd" => c(Verb::GetLocation, &[], &[], &[]),
        _ => None,
    }
}

const KEYWORDS: &[&str] = &[
    "if", "elseif", "else", "foreach", "for", "while", "do", "until", "switch", "try", "catch",
    "finally", "trap", "return", "exit", "throw", "break", "continue", "param", "function",
    "filter", "begin", "process", "end", "in", "data", "class", "enum", "using",
];

#[derive(Debug, Default)]
struct SimpleCommand {
    words: Vec<String>,
    /// Preceded by the `&` call operator, so a variable first word is a
    /// program.
    call: bool,
    /// Redirects output anywhere but `$null`.
    redirect: bool,
}

/// Splits PowerShell source into simple commands without the grammar.
///
/// Commands end at unquoted separators, pipes, parentheses and braces; a
/// `$(...)` inside a double-quoted string contributes its own commands.
/// `None` means the source cannot be split with confidence: an unbalanced
/// quote or a here-string.
fn simple_commands(source: &str) -> Option<Vec<SimpleCommand>> {
    let chars: Vec<char> = source.chars().collect();
    let mut out = Vec::new();
    let mut command = SimpleCommand::default();
    let mut word = String::new();
    let mut redirect_target = false;

    let end_word = |word: &mut String, command: &mut SimpleCommand, target: &mut bool| {
        if word.is_empty() {
            return;
        }
        let w = std::mem::take(word);
        if std::mem::take(target) {
            command.redirect |= !w.eq_ignore_ascii_case("$null");
        } else {
            command.words.push(w);
        }
    };
    let end_command = |word: &mut String,
                       command: &mut SimpleCommand,
                       target: &mut bool,
                       out: &mut Vec<SimpleCommand>| {
        end_word(word, command, target);
        command.redirect |= std::mem::take(target);
        let done = std::mem::take(command);
        if !done.words.is_empty() || done.redirect {
            out.push(done);
        }
    };

    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let next = chars.get(i + 1).copied();
        match c {
            '@' if word.is_empty() && matches!(next, Some('\'' | '"')) => return None,
            '\'' => {
                let mut j = i + 1;
                loop {
                    match chars.get(j)? {
                        '\'' if chars.get(j + 1) == Some(&'\'') => j += 2,
                        '\'' => break,
                        _ => j += 1,
                    }
                }
                word.extend(&chars[i..=j]);
                i = j + 1;
            }
            '"' => {
                let mut j = i + 1;
                loop {
                    match chars.get(j)? {
                        '`' => j += 2,
                        '"' if chars.get(j + 1) == Some(&'"') => j += 2,
                        '"' => break,
                        '$' if chars.get(j + 1) == Some(&'(') => {
                            let close = subexpression_end(&chars, j + 1)?;
                            let body: String = chars[j + 2..close].iter().collect();
                            out.extend(simple_commands(&body)?);
                            j = close + 1;
                        }
                        _ => j += 1,
                    }
                }
                word.extend(&chars[i..=j]);
                i = j + 1;
            }
            '`' => {
                word.push(c);
                word.extend(next);
                i += 2;
            }
            '#' if word.is_empty() => {
                while chars.get(i).is_some_and(|&c| c != '\n') {
                    i += 1;
                }
            }
            '>' => {
                if matches!(word.as_str(), "1" | "2" | "3" | "4" | "5" | "6" | "*") {
                    word.clear();
                } else {
                    end_word(&mut word, &mut command, &mut redirect_target);
                }
                i += if next == Some('>') { 2 } else { 1 };
                if chars.get(i) == Some(&'&') {
                    i += 1;
                    while chars.get(i).is_some_and(char::is_ascii_digit) {
                        i += 1;
                    }
                } else {
                    redirect_target = true;
                }
            }
            '&' => {
                end_command(&mut word, &mut command, &mut redirect_target, &mut out);
                if next == Some('&') {
                    i += 2;
                } else {
                    command.call = true;
                    i += 1;
                }
            }
            '|' | ';' | '\n' | '(' | ')' | '{' | '}' => {
                end_command(&mut word, &mut command, &mut redirect_target, &mut out);
                i += 1;
            }
            c if c.is_whitespace() => {
                end_word(&mut word, &mut command, &mut redirect_target);
                i += 1;
            }
            c => {
                word.push(c);
                i += 1;
            }
        }
    }
    end_command(&mut word, &mut command, &mut redirect_target, &mut out);
    Some(out)
}

/// The index of the `)` closing the `(` at `open`, skipping quoted text.
fn subexpression_end(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = open;
    loop {
        match chars.get(j)? {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            '`' => j += 1,
            quote @ ('\'' | '"') => {
                j += 1;
                while chars.get(j)? != quote {
                    j += if chars[j] == '`' { 2 } else { 1 };
                }
            }
            _ => {}
        }
        j += 1;
    }
}

/// A recovered word's value: literal text, or unknown when it expands.
fn recovered_value(typed: &str) -> Value {
    let quoted = |q: char| typed.len() >= 2 && typed.starts_with(q) && typed.ends_with(q);
    if quoted('\'') {
        return Value::Known(typed[1..typed.len() - 1].replace("''", "'"));
    }
    let expands = typed.contains(['$', '`', '(', '@']);
    if quoted('"') && !expands {
        return Value::Known(typed[1..typed.len() - 1].to_string());
    }
    if expands || typed.contains(['"', '\'']) {
        return Value::Unknown;
    }
    Value::Known(typed.to_string())
}

pub(crate) fn walk(a: &mut Analyzer<'_>, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(Language::PowerShell, source) else {
        a.uncertain(
            UncertaintyKind::ParseError,
            "PowerShell source did not parse",
            frame.locate(0..source.len()),
        );
        return;
    };
    let mut scope = Scope {
        cwd: frame.cwd.clone(),
        ..Scope::default()
    };
    let mut w = Walker {
        a,
        source,
        frame,
        root: tree.root_node(),
        exhausted: false,
    };
    if tree.root_node().has_error() && w.recover_here_string(tree.root_node(), &mut scope) {
        return;
    }
    if tree.root_node().has_error() && tree.root_node().kind() == "ERROR" {
        w.broken(tree.root_node(), &scope);
        return;
    }
    w.statements(tree.root_node(), &mut scope);
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
            self.a.uncertain(
                UncertaintyKind::LimitExhausted(crate::model::Limit::Nodes),
                "PowerShell source was only partly analyzed",
                self.at(node),
            );
            return false;
        }
        true
    }

    fn recover_here_string(&mut self, root: Node<'t>, scope: &mut Scope) -> bool {
        let Some(here) = self.find_kind(root, "verbatim_here_string_characters") else {
            return false;
        };
        let Some(command) = self.find_kind(root, kinds::COMMAND_NAME) else {
            return false;
        };
        let program = ts::text(command, self.source).to_string();
        if !embed::is_interpreter(&program) {
            return false;
        }
        let here_value = self.bounded_value(
            here,
            Value::Known(here_string_body(ts::text(here, self.source))),
        );
        self.a.invocation(
            RawInvocation {
                words: vec![
                    Word {
                        value: Value::Known(program.clone()),
                        typed: program,
                        span: command.byte_range(),
                    },
                    Word {
                        value: Value::Known("-".into()),
                        typed: "-".into(),
                        span: command.byte_range(),
                    },
                ],
                stdin: Stdin::Source {
                    value: here_value,
                    span: here.byte_range(),
                },
                cwd: scope.cwd.clone(),
                language: Language::PowerShell,
                location: self.at(root),
            },
            self.frame,
        );
        let tail_start = self.source[command.end_byte()..]
            .find(['\n', ';'])
            .map(|offset| command.end_byte() + offset + 1)
            .unwrap_or(self.source.len());
        if tail_start < self.source.len() {
            self.a
                .source(Language::PowerShell, &self.source[tail_start..], Frame {
                    depth: self.frame.depth,
                    script_args: Vec::new(),
                    base: Some(self.at(root)),
                    cwd: scope.cwd.clone(),
                });
        }
        true
    }

    fn find_kind(&mut self, node: Node<'t>, kind: &str) -> Option<Node<'t>> {
        if !self.visit(node) {
            return None;
        }
        if node.kind() == kind {
            return Some(node);
        }
        for child in ts::named_children(node) {
            if let Some(found) = self.find_kind(child, kind) {
                return Some(found);
            }
        }
        None
    }

    fn at(&self, node: Node<'_>) -> Location {
        self.frame.locate(node.byte_range())
    }

    fn statements(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            self.statement(child, scope)
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope) {
        if self.a.budget.visit().is_err() {
            return;
        }
        let container = matches!(node.kind(), "program" | "statement_list")
            || [
                kinds::STATEMENT_BLOCK,
                kinds::SCRIPT_BLOCK,
                kinds::SCRIPT_BLOCK_BODY,
            ]
            .contains(&node.kind());
        if node.has_error() && !container {
            self.broken(node, scope);
            return;
        }
        match node.kind() {
            "comment" => {}
            k if k == kinds::PIPELINE || k == kinds::PIPELINE_CHAIN => self.pipeline(node, scope),
            k if k == kinds::ASSIGNMENT => self.assign(node, scope),
            k if k == kinds::FOREACH => self.foreach(node, scope),
            k if k == kinds::FUNCTION => {
                let name = ts::named_children(node)
                    .into_iter()
                    .find(|n| n.kind() == "function_name");
                let body = ts::named_children(node)
                    .into_iter()
                    .find(|n| n.kind() == kinds::SCRIPT_BLOCK);
                if let (Some(name), Some(body)) = (name, body) {
                    scope.functions.insert(
                        ts::text(name, self.source).to_ascii_lowercase(),
                        body.byte_range(),
                    );
                }
            }
            "if_statement" | "while_statement" | "do_statement" | "for_statement"
            | "switch_statement" | "try_statement" | "trap_statement" => {
                let mut branch = scope.clone();
                self.statements(node, &mut branch);
                scope.merge_uncertain(&branch);
            }
            _ => self.statements(node, scope),
        }
    }

    /// Recovers the simple commands of a statement the grammar rejected.
    ///
    /// Each command is analyzed on its own; the statement is a parse error
    /// only when a recovered command could write, since the split may have
    /// lost a word the full parse would have bound.
    fn broken(&mut self, node: Node<'t>, scope: &Scope) {
        let mut cursor = node.walk();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if !self.visit(n) {
                return;
            }
            stack.extend(
                n.children(&mut cursor)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
        let mut could_write = true;
        if let Some(commands) = simple_commands(ts::text(node, self.source)) {
            could_write = false;
            for command in commands {
                could_write |= self.recovered_command(node, command, scope);
            }
        }
        if could_write {
            self.a.uncertain(
                UncertaintyKind::ParseError,
                "a PowerShell statement could not be parsed",
                self.at(node),
            );
        }
    }

    /// Analyzes one recovered command and reports whether it could write.
    fn recovered_command(&mut self, node: Node<'t>, command: SimpleCommand, scope: &Scope) -> bool {
        let mut words = command.words;
        if !command.call && words.first().is_some_and(|w| w.starts_with('$')) {
            let first = words.remove(0);
            match first.split_once('=') {
                Some((_, rest)) if !rest.is_empty() => words.insert(0, rest.to_string()),
                Some(_) => {}
                None if words
                    .first()
                    .is_some_and(|w| w.len() <= 2 && w.ends_with('=')) =>
                {
                    words.remove(0);
                }
                None => return command.redirect,
            }
        }
        let Some(program) = words.first() else {
            return command.redirect;
        };
        let not_a_command = KEYWORDS.contains(&program.to_ascii_lowercase().as_str())
            || program.starts_with(['$', '"', '\'', '-', '[', '@', '!', ','])
            || program.chars().all(|c| c.is_ascii_digit() || c == '.');
        if !command.call && not_a_command {
            return command.redirect;
        }
        if let Some(c) = cmdlet(normalize::basename(program)) {
            return command.redirect
                || !matches!(
                    c.verb,
                    Verb::JoinPath | Verb::GetLocation | Verb::PopLocation
                );
        }
        let words = words
            .iter()
            .map(|typed| Word {
                value: self.bounded_value(node, recovered_value(typed)),
                typed: typed.clone(),
                span: node.byte_range(),
            })
            .collect();
        let out = &self.a.out;
        let before = (
            out.file_effects.len(),
            out.tree_effects.len(),
            out.script_files.len(),
            out.uncertainties.len(),
        );
        self.a.invocation(
            RawInvocation {
                words,
                stdin: Stdin::None,
                cwd: scope.cwd.clone(),
                language: Language::PowerShell,
                location: self.at(node),
            },
            self.frame,
        );
        let out = &self.a.out;
        let after = (
            out.file_effects.len(),
            out.tree_effects.len(),
            out.script_files.len(),
            out.uncertainties.len(),
        );
        command.redirect || before != after
    }

    fn pipeline(&mut self, node: Node<'t>, scope: &mut Scope) {
        let mut stdin = Stdin::None;
        for element in ts::named_children(node) {
            match element.kind() {
                k if k == kinds::COMMAND => {
                    self.command(element, scope, stdin);
                    stdin = Stdin::Source {
                        value: Value::Unknown,
                        span: element.byte_range(),
                    };
                }
                k if k == kinds::ASSIGNMENT => self.assign(element, scope),
                k if k == kinds::REDIRECTION => self.redirection(element, scope),
                k if k == kinds::PIPELINE || k == kinds::PIPELINE_CHAIN => {
                    self.pipeline(element, scope)
                }
                _ => {
                    stdin = Stdin::Source {
                        value: self.value(element, scope),
                        span: element.byte_range(),
                    }
                }
            }
        }
    }

    fn redirection(&mut self, node: Node<'t>, scope: &mut Scope) {
        let mut cursor = node.walk();
        let text = node
            .children(&mut cursor)
            .find(|c| c.kind().contains("redirection_operator"))
            .map(|c| ts::text(c, self.source))
            .unwrap_or("");
        let op = if text.ends_with(">>") {
            FileOp::Append
        } else if text.ends_with('>') && !text.contains('&') {
            FileOp::Overwrite
        } else {
            return;
        };
        let Some(dest) = ts::named_children(node)
            .into_iter()
            .find(|n| n.kind() == kinds::REDIRECTED_FILE)
        else {
            return;
        };
        if ts::text(dest, self.source)
            .trim()
            .eq_ignore_ascii_case("$null")
        {
            return;
        }
        let raw_value = self.value(dest, scope);
        let value = self.bounded_resolved(dest, raw_value, scope.cwd.as_deref());
        self.a
            .file_effect(op, &value, scope.cwd.as_deref(), self.at(node));
    }

    fn assign(&mut self, node: Node<'t>, scope: &mut Scope) {
        let children = ts::named_children(node);
        let (Some(left), Some(right)) = (children.first(), children.last()) else {
            return;
        };
        let value = if right.kind() == kinds::PIPELINE {
            self.pipeline_value(*right, scope)
        } else {
            self.value(*right, scope)
        };
        let name = ts::text(*left, self.source)
            .trim_start_matches('$')
            .to_ascii_lowercase();
        if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            scope.vars.insert(name, value);
        }
    }

    fn pipeline_value(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        match node.kind() {
            k if k == kinds::COMMAND => self.command_value(node, scope),
            k if k == kinds::PIPELINE || k == kinds::PIPELINE_CHAIN => {
                match ts::named_children(node).as_slice() {
                    [only] => self.pipeline_value(*only, scope),
                    _ => {
                        self.pipeline(node, scope);
                        Value::Unknown
                    }
                }
            }
            _ => self.value(node, scope),
        }
    }

    fn foreach(&mut self, node: Node<'t>, scope: &mut Scope) {
        let children = ts::named_children(node);
        let Some(var) = children
            .iter()
            .find(|n| n.kind() == kinds::VARIABLE)
            .map(|v| {
                ts::text(*v, self.source)
                    .trim_start_matches('$')
                    .to_ascii_lowercase()
            })
        else {
            return;
        };
        let Some(body) = children.iter().find(|n| n.kind() == "statement_block") else {
            return;
        };
        let items: Vec<Value> = children
            .iter()
            .filter(|n| n.kind() != kinds::VARIABLE && n.kind() != "statement_block")
            .flat_map(|n| self.list(*n, scope))
            .collect();
        let mut after = scope.clone();
        if !items.is_empty()
            && items.len() <= MAX_LOOP_ITEMS
            && items.iter().all(|v| v.known().is_some())
        {
            for item in items {
                let mut iteration = scope.clone();
                iteration.vars.insert(var.clone(), item);
                self.statements(*body, &mut iteration);
                after.merge_uncertain(&iteration);
            }
        } else {
            let mut iteration = scope.clone();
            iteration.vars.insert(var.clone(), Value::Unknown);
            self.statements(*body, &mut iteration);
            after.merge_uncertain(&iteration);
        }
        after.vars.insert(var, Value::Unknown);
        *scope = after;
    }

    fn list(&mut self, node: Node<'t>, scope: &mut Scope) -> Vec<Value> {
        match node.kind() {
            "array_literal_expression" => ts::named_children(node)
                .into_iter()
                .flat_map(|c| self.list(c, scope))
                .collect(),
            "pipeline"
            | "pipeline_chain"
            | "logical_expression"
            | "bitwise_expression"
            | "comparison_expression"
            | "additive_expression"
            | "multiplicative_expression"
            | "format_expression"
            | "range_expression"
            | "unary_expression"
                if ts::named_children(node).len() == 1 =>
            {
                self.list(ts::named_children(node)[0], scope)
            }
            _ => vec![self.value(node, scope)],
        }
    }

    fn value(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let text = ts::text(node, self.source);
        let value = match node.kind() {
            "verbatim_string_characters" | "verbatim_string_literal" => Value::Known(
                text.trim_start_matches('\'')
                    .trim_end_matches('\'')
                    .replace("''", "'"),
            ),
            "verbatim_here_string_characters" | "verbatim_here_string_literal" => {
                Value::Known(here_string_body(text))
            }
            "expandable_string_literal" | "expandable_here_string_literal" => {
                let body = if node.kind().contains("here") {
                    here_string_body(text)
                } else {
                    text.trim_matches('"').to_string()
                };
                self.expand(&body, node, scope)
            }
            "generic_token" | "command_argument" | "decimal_integer_literal" => {
                if text.contains(['*', '?', '[']) {
                    Value::Unknown
                } else {
                    Value::Known(text.replace('`', ""))
                }
            }
            k if k == kinds::COMMAND_NAME => Value::Known(text.to_string()),
            k if k == kinds::REDIRECTED_FILE => ts::named_children(node)
                .into_iter()
                .find(|child| child.kind() != "command_argument_sep")
                .map_or(Value::Unknown, |child| self.value(child, scope)),
            k if k == kinds::VARIABLE => self.lookup(text, scope),
            k if k == kinds::SUB_EXPRESSION => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
                Value::Unknown
            }
            k if k == kinds::INVOKATION => self.invokation(node, scope),
            k if k == kinds::COMMAND => self.command_value(node, scope),
            k if k == kinds::PIPELINE && ts::named_children(node).len() == 1 => {
                self.value(ts::named_children(node)[0], scope)
            }
            _ => match ts::named_children(node).as_slice() {
                [only] => self.value(*only, scope),
                _ => {
                    self.statements(node, scope);
                    Value::Unknown
                }
            },
        };
        self.bounded_value(node, value)
    }

    fn bounded_value(&mut self, node: Node<'t>, value: Value) -> Value {
        match value {
            Value::Known(value) if !self.a.budget.value_fits(value.len()) => {
                self.value_limit(node);
                Value::Unknown
            }
            value => value,
        }
    }

    fn bounded_resolved(&mut self, node: Node<'t>, value: Value, cwd: Option<&str>) -> Value {
        let value = self.bounded_value(node, value);
        if let crate::model::Target::Path(path) = paths::resolve(&value, cwd, self.a.ctx.path_style)
            && !self.a.budget.value_fits(path.len())
        {
            self.value_limit(node);
            Value::Unknown
        } else {
            value
        }
    }

    fn value_limit(&mut self, node: Node<'t>) {
        self.a.uncertain(
            UncertaintyKind::LimitExhausted(Limit::ValueSize),
            "a PowerShell value exceeded the configured size limit",
            self.at(node),
        );
    }

    fn lookup(&self, text: &str, scope: &Scope) -> Value {
        let name = text
            .trim_start_matches('$')
            .trim_start_matches('{')
            .trim_end_matches('}')
            .to_ascii_lowercase();
        match name.as_str() {
            "pwd" => scope.cwd.clone().map_or(Value::Unknown, Value::Known),
            "true" => Value::Known("true".into()),
            "false" => Value::Known("false".into()),
            n if n.contains(':') => Value::Unknown,
            n => scope.vars.get(n).cloned().unwrap_or(Value::Unknown),
        }
    }

    fn expand(&mut self, body: &str, node: Node<'t>, scope: &mut Scope) -> Value {
        if ts::named_children(node)
            .iter()
            .any(|sub| sub.kind() == kinds::SUB_EXPRESSION)
        {
            return Value::Unknown;
        }
        let mut out = String::new();
        let mut chars = body.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            match c {
                '`' => out.extend(chars.next().map(|(_, c)| c)),
                '$' => {
                    let rest = &body[i + 1..];
                    let name: String = if let Some(braced) = rest.strip_prefix('{') {
                        braced.split('}').next().unwrap_or("").to_string()
                    } else {
                        rest.chars()
                            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
                            .collect()
                    };
                    if name.is_empty() {
                        out.push('$');
                        continue;
                    }
                    let consumed = if rest.starts_with('{') {
                        name.len() + 2
                    } else {
                        name.len()
                    };
                    for _ in 0..consumed {
                        chars.next();
                    }
                    match self.lookup(&name, scope) {
                        Value::Known(v) => out.push_str(&v),
                        Value::Unknown | Value::Ephemeral => return Value::Unknown,
                    }
                }
                c => out.push(c),
            }
        }
        Value::Known(out)
    }

    fn invokation(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let children = ts::named_children(node);
        let type_name = children
            .iter()
            .find(|n| n.kind() == kinds::TYPE_LITERAL)
            .map(|t| {
                ts::text(*t, self.source)
                    .trim_matches(['[', ']'])
                    .to_ascii_lowercase()
            });
        let method = children
            .iter()
            .find(|n| n.kind() == kinds::MEMBER_NAME)
            .map(|m| ts::text(*m, self.source).to_ascii_lowercase());
        let args: Vec<Value> = children
            .iter()
            .find(|n| n.kind() == kinds::ARGUMENT_LIST)
            .map(|list| {
                ts::named_children(*list)
                    .into_iter()
                    .flat_map(|n| {
                        if n.kind() == "argument_expression_list" {
                            ts::named_children(n)
                        } else {
                            vec![n]
                        }
                    })
                    .map(|n| self.value(n, scope))
                    .collect()
            })
            .unwrap_or_default();
        let (Some(type_name), Some(method)) = (type_name, method) else {
            return Value::Unknown;
        };
        let type_name = type_name.strip_prefix("system.").unwrap_or(&type_name);
        let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Unknown);
        let cwd = scope.cwd.clone();
        let path = |w: &mut Self, i: usize| w.bounded_resolved(node, arg(i), cwd.as_deref());
        let at = self.at(node);
        match (type_name, method.as_str()) {
            ("io.path", "combine") => args
                .iter()
                .map(Value::known)
                .collect::<Option<Vec<_>>>()
                .map_or(Value::Unknown, |p| {
                    self.bounded_value(node, Value::Known(p.join("/")))
                }),
            (
                "io.file",
                "writealltext" | "writealllines" | "writeallbytes" | "create" | "createtext"
                | "openwrite",
            )
            | ("io.streamwriter", "new") => {
                let target = path(self, 0);
                self.a
                    .file_effect(FileOp::Overwrite, &target, cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "appendalltext" | "appendalllines" | "appendtext") => {
                let target = path(self, 0);
                self.a
                    .file_effect(FileOp::Append, &target, cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "delete") => {
                let target = path(self, 0);
                self.a
                    .file_effect(FileOp::Delete, &target, cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.directory", "delete") => {
                if arg(1).known().is_some_and(|value| {
                    value.eq_ignore_ascii_case("$true") || value.eq_ignore_ascii_case("true")
                }) {
                    let target = path(self, 0);
                    self.a.tree_effect(
                        &target,
                        false,
                        cwd.as_deref(),
                        "[IO.Directory]::Delete",
                        at,
                    );
                } else {
                    let target = path(self, 0);
                    self.a
                        .file_effect(FileOp::Delete, &target, cwd.as_deref(), at);
                }
                Value::Unknown
            }
            ("io.file", "move" | "replace") => {
                let source = path(self, 0);
                let destination = path(self, 1);
                self.a
                    .file_effect(FileOp::Rename, &source, cwd.as_deref(), at.clone());
                self.a
                    .file_effect(FileOp::Rename, &destination, cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "copy") => {
                let target = path(self, 1);
                self.a
                    .file_effect(FileOp::Copy, &target, cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.directory", "move") => {
                let source = path(self, 0);
                let destination = path(self, 1);
                self.a.tree_effect(
                    &source,
                    false,
                    cwd.as_deref(),
                    "[IO.Directory]::Move",
                    at.clone(),
                );
                self.a.tree_effect(
                    &destination,
                    false,
                    cwd.as_deref(),
                    "[IO.Directory]::Move",
                    at,
                );
                Value::Unknown
            }
            _ => Value::Unknown,
        }
    }

    fn command_value(&mut self, node: Node<'t>, scope: &mut Scope) -> Value {
        let (name, elements) = self.command_parts(node, scope);
        if let Some(Value::Known(name)) = &name
            && let Some(c) = cmdlet(name)
        {
            match c.verb {
                Verb::JoinPath => {
                    let bound = bind(&c, &elements);
                    return match (
                        bound.get("path").and_then(|v| v.known()),
                        bound.get("childpath").and_then(|v| v.known()),
                    ) {
                        (Some(p), Some(child)) => self.bounded_value(
                            node,
                            Value::Known(format!("{}/{}", p.trim_end_matches(['/', '\\']), child)),
                        ),
                        _ => Value::Unknown,
                    };
                }
                Verb::GetLocation => {
                    return self.bounded_value(
                        node,
                        scope.cwd.clone().map_or(Value::Unknown, Value::Known),
                    );
                }
                _ => {}
            }
        }
        self.command(node, scope, Stdin::None);
        Value::Unknown
    }

    fn command_parts(
        &mut self,
        node: Node<'t>,
        scope: &mut Scope,
    ) -> (Option<Value>, Vec<(Option<String>, Word)>) {
        let mut name = None;
        let mut elements = Vec::new();
        let mut pending = None;
        for child in ts::named_children(node) {
            match child.kind() {
                k if k == kinds::COMMAND_NAME || k == kinds::COMMAND_NAME_EXPR => {
                    name = Some(self.value(child, scope))
                }
                k if k == kinds::COMMAND_ELEMENTS => {
                    for el in ts::named_children(child) {
                        match el.kind() {
                            k if k == kinds::PARAMETER => {
                                if let Some(p) = pending.take() {
                                    elements.push((Some(p), Word {
                                        value: Value::Known("$true".into()),
                                        typed: String::new(),
                                        span: el.byte_range(),
                                    }));
                                }
                                pending = Some(
                                    ts::text(el, self.source)
                                        .trim_start_matches('-')
                                        .to_ascii_lowercase(),
                                );
                            }
                            k if k == kinds::REDIRECTION => self.redirection(el, scope),
                            "command_argument_sep" => {}
                            _ => {
                                for value in self.list(el, scope) {
                                    elements.push((pending.take(), Word {
                                        typed: value.known().map_or_else(
                                            || ts::text(el, self.source).to_string(),
                                            str::to_string,
                                        ),
                                        value,
                                        span: el.byte_range(),
                                    }));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(p) = pending {
            elements.push((Some(p), Word {
                value: Value::Known("$true".into()),
                typed: String::new(),
                span: node.byte_range(),
            }));
        }
        (name, elements)
    }

    fn command(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        let (name, elements) = self.command_parts(node, scope);
        let at = self.at(node);
        let Some(name_value) = name else { return };
        let Some(name) = name_value.known().map(str::to_string) else {
            self.a.uncertain(
                UncertaintyKind::UnresolvedInvocation,
                "a command invoked through a value that could not be determined",
                at,
            );
            return;
        };
        if let Some(body) = scope.functions.get(&name.to_ascii_lowercase()).cloned() {
            if let Some(body) = self.root.descendant_for_byte_range(body.start, body.end) {
                let mut call = scope.clone();
                self.statements(body, &mut call);
            }
            return;
        }
        let Some(c) = cmdlet(&name) else {
            let words = std::iter::once(Word {
                value: name_value.clone(),
                typed: name,
                span: node.byte_range(),
            })
            .chain(elements.into_iter().flat_map(|(param, word)| {
                param
                    .map(|p| Word {
                        value: Value::Known(format!("-{p}")),
                        typed: format!("-{p}"),
                        span: word.span.clone(),
                    })
                    .into_iter()
                    .chain((!word.typed.is_empty()).then_some(word))
            }))
            .collect();
            self.a.invocation(
                RawInvocation {
                    words,
                    stdin,
                    cwd: scope.cwd.clone(),
                    language: Language::PowerShell,
                    location: at,
                },
                self.frame,
            );
            return;
        };
        let bound = bind(&c, &elements);
        let get = |k| bound.get(k).cloned();
        let cwd = scope.cwd.clone();
        let file = |w: &mut Self, op: FileOp, v: Option<Value>, literal: bool| {
            let v = v.unwrap_or(Value::Unknown);
            let v = match &v {
                Value::Known(p) if !literal && p.contains(['*', '?', '[']) => Value::Unknown,
                _ => v,
            };
            let v = w.bounded_resolved(node, v, cwd.as_deref());
            w.a.file_effect(op, &v, cwd.as_deref(), w.at(node));
        };
        let tree = |w: &mut Self, v: Value, by: &str| {
            let v = w.bounded_resolved(node, v, cwd.as_deref());
            w.a.tree_effect(&v, false, cwd.as_deref(), by, w.at(node));
        };
        let switch = |k| bound.contains_key(k);
        let literal = elements.iter().any(|(p, _)| {
            p.as_deref()
                .is_some_and(|p| "literalpath".starts_with(p) && p.len() > 1)
        });
        match c.verb {
            Verb::SetContent | Verb::ClearContent | Verb::ExportFile => {
                file(self, FileOp::Overwrite, get("path"), literal)
            }
            Verb::AddContent => file(self, FileOp::Append, get("path"), literal),
            Verb::OutFile | Verb::TeeObject => file(
                self,
                if switch("append") {
                    FileOp::Append
                } else {
                    FileOp::Overwrite
                },
                get("filepath").or_else(|| get("path")),
                true,
            ),
            Verb::NewItem => {
                let kind = get("itemtype").and_then(|v| v.known().map(str::to_ascii_lowercase));
                if !matches!(kind.as_deref(), Some("directory" | "dir")) {
                    let target = match (get("path"), get("name")) {
                        (Some(Value::Known(p)), Some(Value::Known(n))) => {
                            Some(Value::Known(format!("{p}/{n}")))
                        }
                        (None, Some(n)) => Some(n),
                        (p, _) => p,
                    };
                    file(self, FileOp::Create, target, literal);
                }
            }
            Verb::RemoveItem => {
                if switch("recurse") {
                    tree(
                        self,
                        get("path").unwrap_or(Value::Unknown),
                        "Remove-Item -Recurse",
                    );
                } else {
                    for (_, w) in elements.iter().filter(|(p, _)| {
                        p.as_deref()
                            .is_none_or(|p| "path".starts_with(p) || "literalpath".starts_with(p))
                    }) {
                        file(self, FileOp::Delete, Some(w.value.clone()), literal);
                    }
                }
            }
            Verb::MoveItem => {
                file(self, FileOp::Rename, get("path"), literal);
                file(self, FileOp::Rename, get("destination"), true);
            }
            Verb::RenameItem => {
                let path = get("path");
                file(self, FileOp::Rename, path.clone(), literal);
                let renamed = match (path.as_ref().and_then(Value::known), get("newname")) {
                    (Some(p), Some(Value::Known(n))) => Value::Known(match paths::parent(p) {
                        Some(dir) => format!("{dir}/{n}"),
                        None => n,
                    }),
                    _ => Value::Unknown,
                };
                file(self, FileOp::Rename, Some(renamed), true);
            }
            Verb::CopyItem => {
                if switch("recurse") {
                    tree(
                        self,
                        get("destination").unwrap_or(Value::Unknown),
                        "Copy-Item -Recurse",
                    );
                } else {
                    file(self, FileOp::Copy, get("destination"), true);
                }
            }
            Verb::ExpandArchive => tree(
                self,
                get("destinationpath").unwrap_or(Value::Known(".".into())),
                "Expand-Archive",
            ),
            Verb::WebRequest => {
                if let Some(out) = get("outfile") {
                    file(self, FileOp::Overwrite, Some(out), true)
                }
            }
            Verb::SetLocation => {
                scope.cwd = match get("path") {
                    Some(v) => {
                        let v = self.bounded_resolved(node, v, scope.cwd.as_deref());
                        match paths::resolve(&v, scope.cwd.as_deref(), self.a.ctx.path_style) {
                            crate::model::Target::Path(d) => Some(d),
                            crate::model::Target::Unresolved | crate::model::Target::Ephemeral => {
                                None
                            }
                        }
                    }
                    None => None,
                }
            }
            Verb::PopLocation => scope.cwd = None,
            Verb::InvokeExpression => match get("command").or_else(|| match &stdin {
                Stdin::Source { value, .. } => Some(value.clone()),
                Stdin::None => None,
            }) {
                Some(Value::Known(source)) => self.a.source(Language::PowerShell, &source, Frame {
                    depth: self.frame.depth + 1,
                    script_args: Vec::new(),
                    base: Some(self.at(node)),
                    cwd: scope.cwd.clone(),
                }),
                _ => self.a.uncertain(
                    UncertaintyKind::UnresolvedWrite,
                    "`Invoke-Expression` of source that could not be determined",
                    self.at(node),
                ),
            },
            Verb::JoinPath | Verb::GetLocation => {}
        }
    }
}

fn bind(c: &Cmdlet, elements: &[(Option<String>, Word)]) -> HashMap<&'static str, Value> {
    let mut out = HashMap::new();
    let mut positional = Vec::new();
    for (param, word) in elements {
        let Some(p) = param else {
            positional.push(word);
            continue;
        };
        match resolve_param(c, p) {
            Some(name) if c.switches.contains(&name) => {
                out.insert(name, Value::Known("$true".into()));
                if !word.typed.is_empty() {
                    positional.push(word)
                }
            }
            Some(name) => {
                out.insert(name, word.value.clone());
            }
            None => {}
        }
    }
    let open: Vec<_> = c
        .positional
        .iter()
        .copied()
        .filter(|n| !out.contains_key(n))
        .collect();
    for (name, word) in open.into_iter().zip(positional) {
        out.insert(name, word.value.clone());
    }
    out
}
fn resolve_param(c: &Cmdlet, typed: &str) -> Option<&'static str> {
    let p = typed.trim_end_matches(':');
    if let Some((_, canonical)) = PATH_ALIASES.iter().find(|(alias, _)| *alias == p)
        && c.params.contains(canonical)
    {
        return Some(canonical);
    }
    let names = || c.params.iter().chain(c.switches).copied();
    if let Some(exact) = names().find(|n| *n == p) {
        return Some(exact);
    }
    let mut prefixed = names().filter(|n| n.starts_with(p));
    match (prefixed.next(), prefixed.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}
fn here_string_body(text: &str) -> String {
    let inner = text
        .trim_start_matches(['@'])
        .trim_start_matches(['\'', '"'])
        .trim_end_matches('@')
        .trim_end_matches(['\'', '"']);
    let inner = inner
        .strip_prefix("\r\n")
        .or_else(|| inner.strip_prefix('\n'))
        .unwrap_or(inner);
    inner
        .strip_suffix("\r\n")
        .or_else(|| inner.strip_suffix('\n'))
        .unwrap_or(inner)
        .to_string()
}

#[cfg(test)]
mod shapes {
    use crate::{model::Language, ts};

    #[test]
    fn node_shapes_this_adapter_relies_on() {
        for (source, fragment) in [
            ("Set-Content -Path a.txt -Value x", "command_name"),
            ("Set-Content -Path a.txt -Value x", "command_parameter"),
            ("echo x > a.txt", "redirection"),
            ("$p = 'a.txt'", "assignment_expression"),
            (
                "[System.IO.File]::WriteAllText('a.txt', 'x')",
                "invokation_expression",
            ),
            ("@'\nprint(1)\n'@ | python -", "verbatim_here_string"),
            (
                "foreach ($f in 'a','b') { Remove-Item $f }",
                "foreach_statement",
            ),
        ] {
            let tree = ts::parse(Language::PowerShell, source).unwrap();
            let sexp = tree.root_node().to_sexp();
            assert!(sexp.contains(fragment), "{source:?}\\n{sexp}");
        }
    }

    #[test]
    fn the_grammar_still_fails_on_the_recorded_argument_shapes() {
        for source in [
            "Get-ChildItem | Format-Table Mode, Name -AutoSize",
            "git -C 'C:/repo' log --format='%h %s'",
            "git push --force-with-lease=a:b origin c",
        ] {
            let tree = ts::parse(Language::PowerShell, source).unwrap();
            assert!(
                tree.root_node().has_error(),
                "{source:?} now parses; the recovery fixtures below may no longer exercise recovery"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Analysis, Dialect, PathStyle,
        model::{Limit, UncertaintyKind},
        testutil::{ctx, targets},
    };

    fn ps(source: &str) -> Analysis {
        let mut c = ctx(Dialect::PowerShell);
        c.cwd = Some("C:/repo".into());
        c.path_style = PathStyle::Windows;
        crate::analyze(source, &c)
    }

    #[test]
    fn content_cmdlets_by_name_prefix_and_position() {
        assert_eq!(targets(&ps("Set-Content -Path a.txt -Value x")), [
            "C:/repo/a.txt"
        ]);
        assert_eq!(targets(&ps("set-content -pa b.txt -va x")), [
            "C:/repo/b.txt"
        ]);
        assert_eq!(targets(&ps("Add-Content c.txt 'x'")), ["C:/repo/c.txt"]);
        assert_eq!(targets(&ps("'x' | Out-File -FilePath:d.txt -Append")), [
            "C:/repo/d.txt"
        ]);
    }

    #[test]
    fn item_cmdlets_and_aliases() {
        assert_eq!(targets(&ps("Remove-Item old.txt")), ["C:/repo/old.txt"]);
        assert_eq!(targets(&ps("Move-Item a.txt b.txt")), [
            "C:/repo/a.txt",
            "C:/repo/b.txt"
        ]);
        assert_eq!(
            targets(&ps("Rename-Item -Path src/a.txt -NewName b.txt")),
            ["C:/repo/src/a.txt", "C:/repo/src/b.txt"]
        );
        assert_eq!(targets(&ps("ni new.txt -ItemType File")), [
            "C:/repo/new.txt"
        ]);
        assert!(targets(&ps("New-Item -ItemType Directory -Path gen")).is_empty());
        assert_eq!(
            ps("Remove-Item build -Recurse -Force").tree_effects[0].scope,
            "C:/repo/build"
        );
    }

    #[test]
    fn redirects_and_null() {
        assert_eq!(targets(&ps("echo x > a.txt; echo y >> b.txt")), [
            "C:/repo/a.txt",
            "C:/repo/b.txt"
        ]);
        assert!(targets(&ps("git status > $null")).is_empty());
    }

    #[test]
    fn variables_join_path_and_set_location() {
        assert_eq!(
            targets(&ps("$p = Join-Path 'gen' 'a.txt'; Set-Content $p x")),
            ["C:/repo/gen/a.txt"]
        );
        assert_eq!(targets(&ps("$d = 'gen'; Set-Content \"$d/b.txt\" x")), [
            "C:/repo/gen/b.txt"
        ]);
        assert_eq!(targets(&ps("Set-Location sub; Set-Content a.txt x")), [
            "C:/repo/sub/a.txt"
        ]);
        assert_eq!(targets(&ps("Set-Content $env:OUT x")), ["?"]);
    }

    #[test]
    fn dotnet_file_apis() {
        assert_eq!(
            targets(&ps("[System.IO.File]::WriteAllText('a.txt', 'x')")),
            ["C:/repo/a.txt"]
        );
        assert_eq!(
            targets(&ps("[IO.File]::AppendAllLines('b.txt', @('x'))")),
            ["C:/repo/b.txt"]
        );
    }

    #[test]
    fn dotnet_directory_delete_is_tree_only_when_recursive() {
        let nonrecursive = ps("[IO.Directory]::Delete('build')");
        assert_eq!(targets(&nonrecursive), ["C:/repo/build"]);
        assert!(nonrecursive.tree_effects.is_empty());

        let source = "[IO.Directory]::Delete('build', $true)";
        let recursive = ps(source);
        assert!(recursive.file_effects.is_empty(), "{recursive:?}");
        assert_eq!(recursive.tree_effects[0].scope, "C:/repo/build");
    }

    #[test]
    fn a_here_string_piped_to_python_is_python_source() {
        let a = ps("@'\nopen('h.txt', 'w')\n'@ | python -");
        assert_eq!(targets(&a), ["C:/repo/h.txt"]);
    }

    #[test]
    fn a_malformed_here_string_does_not_skip_following_writes() {
        let source = "@'\nopen('h.txt', 'w')\n'@ | python -\nSet-Content after.txt y";
        let a = ps(source);
        assert_eq!(targets(&a), ["C:/repo/h.txt", "C:/repo/after.txt"], "{a:?}");
    }

    #[test]
    fn a_literal_foreach_runs_per_item_and_wildcards_are_unresolved() {
        assert_eq!(
            targets(&ps("foreach ($f in 'a.txt','b.txt') { Remove-Item $f }")),
            ["C:/repo/a.txt", "C:/repo/b.txt"]
        );
        assert_eq!(targets(&ps("Remove-Item *.log")), ["?"]);
    }

    #[test]
    fn dynamic_invocation_is_unresolved() {
        let a = ps("Invoke-Expression $cmd");
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::UnresolvedWrite)
        );
        assert_eq!(targets(&ps("iex 'Set-Content e.txt x'")), ["C:/repo/e.txt"]);
    }

    #[test]
    fn a_statement_that_fails_to_parse_leaves_its_siblings_enforced() {
        let a = ps(
            "Set-Content a.txt x\ngit push --force-with-lease=a:b origin c\nSet-Content b.txt y",
        );
        let t = targets(&a);
        assert!(
            t.contains(&"C:/repo/a.txt".to_string()) && t.contains(&"C:/repo/b.txt".to_string()),
            "{t:?}"
        );
    }

    #[test]
    fn read_only_statements_that_fail_to_parse_are_silent() {
        for source in [
            "Get-ChildItem | Format-Table Mode, Name -AutoSize",
            "git -C 'C:/repo' log --format='%h %s'",
        ] {
            let a = ps(source);
            assert!(
                a.uncertainties.is_empty() && a.file_effects.is_empty(),
                "{source:?}: {a:?}"
            );
        }
    }

    #[test]
    fn a_broken_statement_that_redirects_is_uncertain() {
        let a = ps("Get-ChildItem | Format-Table Mode, Name -AutoSize > out.txt");
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::ParseError),
            "{a:?}"
        );
    }

    #[test]
    fn a_broken_statement_of_read_only_programs_is_silent() {
        for source in [
            "$s14 = git -C $repo log --format=%s a..b",
            "$x=(git -C $repo diff --binary -- a.rs) -join \"`n\"",
            "git -C $r push origin x --force-with-lease=a:b 2>&1 | Select-Object -Last 3",
            "cargo fmt --all -- --check; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }",
            "rg -n -A12 \"a\\(cfg\\)|b\" $p",
        ] {
            let a = ps(source);
            assert!(a.uncertainties.is_empty(), "{source}\n{a:?}");
            assert!(
                a.file_effects.is_empty() && a.tree_effects.is_empty(),
                "{source}\n{a:?}"
            );
        }
    }

    #[test]
    fn a_broken_statement_hiding_a_write_is_uncertain() {
        for source in [
            "$s = git log --format=%s; cargo fmt",
            "$x=(git diff -- a.rs) -join \"$(Remove-Item b.txt)\"",
            "$s = git log --format=%s > log.txt",
            "$s = git log --format=%s | Set-Content c.txt",
            "$s = git log --format=%s; & $tool --write",
        ] {
            let a = ps(source);
            assert!(
                !a.uncertainties.is_empty()
                    || !a.file_effects.is_empty()
                    || !a.tree_effects.is_empty(),
                "{source}\n{a:?}"
            );
        }
    }

    #[test]
    fn a_malformed_known_cmdlet_preserves_parse_error() {
        let source = "Set-Content -Path a.txt -Value x |";
        let tree = crate::ts::parse(crate::model::Language::PowerShell, source).unwrap();
        let a = ps(source);
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::ParseError),
            "{}\n{a:?}",
            tree.root_node().to_sexp()
        );
    }

    #[test]
    fn malformed_recovery_uses_current_location() {
        let a = ps("Set-Location sub\ngit checkout -- a.rs |");
        let targets = targets(&a);
        assert!(targets.contains(&"C:/repo/sub/a.rs".to_string()), "{a:?}");
        assert!(!targets.contains(&"C:/repo/a.rs".to_string()), "{a:?}");
    }

    #[test]
    fn malformed_here_string_recovery_respects_node_limit() {
        let mut c = ctx(Dialect::PowerShell);
        c.cwd = Some("C:/repo".into());
        c.path_style = PathStyle::Windows;
        c.limits.nodes = 1;
        let a = crate::analyze("@'\n'@ | python - |", &c);
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::LimitExhausted(Limit::Nodes)),
            "{a:?}"
        );
    }

    #[test]
    fn malformed_error_recovery_respects_node_limit() {
        let source = "Get-ChildItem | Format-Table Mode, Name -AutoSize";
        let tree = crate::ts::parse(crate::model::Language::PowerShell, source).unwrap();
        let mut c = ctx(Dialect::PowerShell);
        c.cwd = Some("C:/repo".into());
        c.path_style = PathStyle::Windows;
        c.limits.nodes = 20;
        let a = crate::analyze(source, &c);
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::LimitExhausted(Limit::Nodes)),
            "{}\n{a:?}",
            tree.root_node().to_sexp()
        );
    }

    #[test]
    fn oversized_powershell_values_are_not_emitted_as_targets() {
        let path = "a".repeat(65 * 1024);
        let a = ps(&format!("Set-Content '{path}' x"));
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::LimitExhausted(Limit::ValueSize)),
            "{a:?}"
        );
        assert_eq!(targets(&a), ["?"]);
    }

    #[test]
    fn oversized_cwd_resolved_powershell_values_are_not_emitted_as_targets() {
        let path = "a".repeat(64 * 1024 - "C:/repo/".len() + 1);
        let a = ps(&format!("$p = '{path}'; Set-Content $p x"));
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::LimitExhausted(Limit::ValueSize)),
            "{a:?}"
        );
        assert!(
            targets(&a).iter().all(|target| target.len() <= 64 * 1024),
            "{a:?}"
        );
    }

    #[test]
    fn external_programs_reach_the_catalog() {
        assert_eq!(targets(&ps("git checkout -- a.rs")), ["C:/repo/a.rs"]);
    }
}
