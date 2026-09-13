//! PowerShell: pipelines, redirects, variables, cmdlet parameter binding, the
//! file cmdlets and .NET file APIs, here-strings, and external programs.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    catalog, embed,
    model::{FileOp, Language, Location, UncertaintyKind, Value},
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
    };
    if tree.root_node().has_error() && w.recover_here_string(tree.root_node(), &mut scope) {
        return;
    }
    if tree.root_node().has_error() && tree.root_node().kind() == "ERROR" {
        w.broken(tree.root_node());
        return;
    }
    w.statements(tree.root_node(), &mut scope);
}

struct Walker<'a, 'c, 's, 't> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
    root: Node<'t>,
}

impl<'t> Walker<'_, '_, '_, 't> {
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
                    value: Value::Known(here_string_body(ts::text(here, self.source))),
                    span: here.byte_range(),
                },
                cwd: scope.cwd.clone(),
                language: Language::PowerShell,
                location: self.at(root),
            },
            self.frame,
        );
        true
    }

    fn find_kind(&self, node: Node<'t>, kind: &str) -> Option<Node<'t>> {
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
        if node.has_error()
            && !matches!(
                node.kind(),
                "program"
                    | "statement_list"
                    | "statement_block"
                    | "script_block"
                    | "script_block_body"
            )
        {
            self.broken(node);
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
                    .find(|n| n.kind() == "script_block");
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

    fn broken(&mut self, node: Node<'t>) {
        let text = ts::text(node, self.source);
        let mut program = None;
        let mut redirects = false;
        let mut cursor = node.walk();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == kinds::REDIRECTION || n.kind() == "file_redirection_operator" {
                redirects = true
            }
            if program.is_none() && matches!(n.kind(), "command_name" | "generic_token") {
                program = Some(ts::text(n, self.source).to_string())
            }
            stack.extend(
                n.children(&mut cursor)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev(),
            );
        }
        if program.is_none() {
            program = text.split_whitespace().next().map(str::to_string)
        }
        redirects |= text
            .split_whitespace()
            .any(|w| matches!(w, ">" | ">>" | "*>" | "2>" | "2>>"));
        if self.recover_command(node, text, redirects) {
            return;
        }
        let could_write = match &program {
            None => true,
            Some(p) => {
                let base = normalize::basename(p);
                cmdlet(base).is_some_and(|c| {
                    !matches!(
                        c.verb,
                        Verb::JoinPath | Verb::GetLocation | Verb::PopLocation
                    )
                }) || catalog::is_cataloged(base)
                    || embed::is_interpreter(base)
            }
        };
        if redirects || could_write {
            self.a.uncertain(
                UncertaintyKind::ParseError,
                "a PowerShell statement could not be parsed",
                self.at(node),
            );
        }
    }

    fn recover_command(&mut self, node: Node<'t>, text: &str, redirects: bool) -> bool {
        let Some(first) = text.split_whitespace().next() else {
            return false;
        };
        let Some(command) = self.find_kind(node, kinds::COMMAND_NAME) else {
            return false;
        };
        if ts::text(command, self.source) != first {
            return false;
        }
        let values: Vec<Value> = text
            .split_whitespace()
            .map(|word| Value::Known(word.trim_matches(['\'', '"']).to_string()))
            .collect();
        let Some(program) = values.first().and_then(Value::known) else {
            return false;
        };
        let args = values[1..].to_vec();
        let effectful =
            redirects || !catalog::effects(normalize::basename(program), &args).is_empty();
        let words = values
            .iter()
            .map(|value| Word {
                typed: value.known().unwrap_or("?").to_string(),
                value: value.clone(),
                span: node.byte_range(),
            })
            .collect();
        self.a.invocation(
            RawInvocation {
                words,
                stdin: Stdin::None,
                cwd: self.frame.cwd.clone(),
                language: Language::PowerShell,
                location: self.at(node),
            },
            self.frame,
        );
        if effectful {
            self.a.uncertain(
                UncertaintyKind::ParseError,
                "a PowerShell statement could not be parsed",
                self.at(node),
            );
        }
        true
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
        let value = self.value(dest, scope);
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
        match node.kind() {
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
        }
    }

    fn lookup(&self, text: &str, scope: &Scope) -> Value {
        let name = text
            .trim_start_matches('$')
            .trim_start_matches('{')
            .trim_end_matches('}')
            .to_ascii_lowercase();
        match name.as_str() {
            "pwd" => scope.cwd.clone().map_or(Value::Unknown, Value::Known),
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
                        Value::Unknown => return Value::Unknown,
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
        let at = self.at(node);
        match (type_name, method.as_str()) {
            ("io.path", "combine") => args
                .iter()
                .map(Value::known)
                .collect::<Option<Vec<_>>>()
                .map_or(Value::Unknown, |p| Value::Known(p.join("/"))),
            (
                "io.file",
                "writealltext" | "writealllines" | "writeallbytes" | "create" | "createtext"
                | "openwrite",
            )
            | ("io.streamwriter", "new") => {
                self.a
                    .file_effect(FileOp::Overwrite, &arg(0), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "appendalltext" | "appendalllines" | "appendtext") => {
                self.a
                    .file_effect(FileOp::Append, &arg(0), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "delete") | ("io.directory", "delete") => {
                self.a
                    .file_effect(FileOp::Delete, &arg(0), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "move" | "replace") => {
                self.a
                    .file_effect(FileOp::Rename, &arg(0), scope.cwd.as_deref(), at.clone());
                self.a
                    .file_effect(FileOp::Rename, &arg(1), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.file", "copy") => {
                self.a
                    .file_effect(FileOp::Copy, &arg(1), scope.cwd.as_deref(), at);
                Value::Unknown
            }
            ("io.directory", "move") => {
                self.a.tree_effect(
                    &arg(0),
                    false,
                    scope.cwd.as_deref(),
                    "[IO.Directory]::Move",
                    at.clone(),
                );
                self.a.tree_effect(
                    &arg(1),
                    false,
                    scope.cwd.as_deref(),
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
                        (Some(p), Some(child)) => {
                            Value::Known(format!("{}/{}", p.trim_end_matches(['/', '\\']), child))
                        }
                        _ => Value::Unknown,
                    };
                }
                Verb::GetLocation => return scope.cwd.clone().map_or(Value::Unknown, Value::Known),
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
            w.a.file_effect(op, &v, cwd.as_deref(), w.at(node));
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
                    self.a.tree_effect(
                        &get("path").unwrap_or(Value::Unknown),
                        false,
                        cwd.as_deref(),
                        "Remove-Item -Recurse",
                        self.at(node),
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
                    self.a.tree_effect(
                        &get("destination").unwrap_or(Value::Unknown),
                        false,
                        cwd.as_deref(),
                        "Copy-Item -Recurse",
                        self.at(node),
                    );
                } else {
                    file(self, FileOp::Copy, get("destination"), true);
                }
            }
            Verb::ExpandArchive => self.a.tree_effect(
                &get("destinationpath").unwrap_or(Value::Known(".".into())),
                false,
                cwd.as_deref(),
                "Expand-Archive",
                self.at(node),
            ),
            Verb::WebRequest => {
                if let Some(out) = get("outfile") {
                    file(self, FileOp::Overwrite, Some(out), true)
                }
            }
            Verb::SetLocation => {
                scope.cwd = match get("path") {
                    Some(v) => {
                        match paths::resolve(&v, scope.cwd.as_deref(), self.a.ctx.path_style) {
                            crate::model::Target::Path(d) => Some(d),
                            crate::model::Target::Unresolved => None,
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
        model::UncertaintyKind,
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
    fn a_here_string_piped_to_python_is_python_source() {
        let a = ps("@'\nopen('h.txt', 'w')\n'@ | python -");
        assert_eq!(targets(&a), ["C:/repo/h.txt"]);
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
    fn external_programs_reach_the_catalog() {
        assert_eq!(targets(&ps("git checkout -- a.rs")), ["C:/repo/a.rs"]);
    }
}
