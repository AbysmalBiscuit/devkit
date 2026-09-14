//! Python: bindings, path construction, and the filesystem, process and
//! dynamic-execution calls that can write. A receiver's name alone never
//! establishes its type; knowledge comes from imports and assignments, and a
//! rebinding drops it.

use std::{collections::HashMap, ops::Range};

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    model::{FileOp, Language, Location, TempLocation, UncertaintyKind, Value},
    normalize, paths, ts,
};

const MAX_LOOP_ITEMS: usize = 32;

const KNOWN_MODULES: &[&str] = &[
    "pathlib",
    "tempfile",
    "os",
    "os.path",
    "shutil",
    "sys",
    "subprocess",
    "io",
    "codecs",
    "builtins",
    "importlib",
    "json",
    "re",
    "math",
    "collections",
    "itertools",
    "functools",
    "typing",
    "dataclasses",
    "datetime",
    "time",
    "textwrap",
    "pprint",
    "string",
    "hashlib",
    "base64",
    "difflib",
    "statistics",
    "random",
    "uuid",
    "enum",
    "ast",
    "tomllib",
    "csv",
    "glob",
    "fnmatch",
    "argparse",
    "copy",
    "operator",
    "decimal",
    "fractions",
    "heapq",
    "bisect",
    "shlex",
    "contextlib",
    "types",
    "inspect",
    "platform",
    "locale",
    "unicodedata",
    "struct",
    "html",
    "urllib.parse",
    "configparser",
    "traceback",
    "warnings",
    "keyword",
    "tokenize",
    "calendar",
    "zoneinfo",
];

const BUILTINS: &[&str] = &[
    "print",
    "len",
    "sorted",
    "reversed",
    "enumerate",
    "zip",
    "range",
    "map",
    "filter",
    "min",
    "max",
    "sum",
    "any",
    "all",
    "abs",
    "round",
    "isinstance",
    "issubclass",
    "hasattr",
    "repr",
    "str",
    "int",
    "float",
    "bool",
    "list",
    "dict",
    "set",
    "tuple",
    "frozenset",
    "bytes",
    "bytearray",
    "type",
    "id",
    "hash",
    "iter",
    "next",
    "chr",
    "ord",
    "hex",
    "oct",
    "bin",
    "format",
    "vars",
    "dir",
    "divmod",
    "pow",
    "input",
    "object",
    "super",
    "property",
    "staticmethod",
    "classmethod",
    "slice",
    "callable",
    "ascii",
    "memoryview",
    "Exception",
    "ValueError",
    "KeyError",
    "TypeError",
    "RuntimeError",
    "SystemExit",
    "OSError",
    "FileNotFoundError",
    "StopIteration",
    "NotImplementedError",
    "AssertionError",
];

const WRITE_METHODS: &[&str] = &[
    "write_text",
    "write_bytes",
    "touch",
    "unlink",
    "rename",
    "replace",
    "symlink_to",
    "hardlink_to",
    "to_csv",
    "to_json",
    "to_parquet",
    "to_excel",
    "to_pickle",
    "savefig",
    "save",
];

/// `pathlib` methods that write, whatever the receiver turns out to be.
const MUTATING_PATH_METHODS: &[&str] = &[
    "write_text",
    "write_bytes",
    "touch",
    "symlink_to",
    "hardlink_to",
    "mkdir",
    "unlink",
    "rmdir",
    "rename",
    "replace",
    "copy",
    "copy_into",
    "move",
    "move_into",
];

/// The mode a call to `open` was given, and whether it was given one at all.
fn open_mode<'t>(args: &[Py], keywords: &HashMap<String, (Py, Node<'t>)>) -> (Py, bool) {
    (
        args.first()
            .cloned()
            .or_else(|| keywords.get("mode").map(|(value, _)| value.clone()))
            .unwrap_or(Py::Unknown),
        !args.is_empty() || keywords.contains_key("mode"),
    )
}

/// Where a `tempfile` entry is created. `None` when the call named a directory
/// that could not be resolved, leaving nothing to check a claim against.
fn temp_dir(
    named: Option<&Py>,
    cwd: Option<&str>,
    style: crate::context::PathStyle,
) -> Option<TempLocation> {
    match named {
        None => Some(TempLocation::SystemTemp),
        Some(Py::Str(s) | Py::Path(s)) => {
            match paths::resolve(&Value::Known(s.clone()), cwd, style) {
                crate::model::Target::Path(dir) => Some(TempLocation::In(dir)),
                _ => None,
            }
        }
        Some(_) => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Py {
    Str(String),
    Path(String),
    /// A path `tempfile` created fresh under a random name, carrying where it
    /// was made. The name needs no claim of its own, but the directory it was
    /// made in can be held by someone.
    Ephemeral(TempLocation),
    Int(i64),
    Bool(bool),
    List(Vec<Py>),
    Api(String),
    Method(Box<Py>, String),
    Argv,
    WriteHandle {
        target: Box<Py>,
        write_op: Option<FileOp>,
    },
    Foreign(String),
    Def(Range<usize>),
    Data,
    Unknown,
}

impl Py {
    fn as_value(&self) -> Value {
        match self {
            Py::Str(s) | Py::Path(s) => Value::Known(s.clone()),
            _ => match self.ephemeral() {
                Some(at) => Value::Ephemeral(at),
                None => Value::Unknown,
            },
        }
    }

    /// Where a freshly created temp path was made, when this value is one.
    fn ephemeral(&self) -> Option<TempLocation> {
        match self {
            Py::Ephemeral(at) => Some(at.clone()),
            Py::WriteHandle { target, .. } => target.ephemeral(),
            // No bare member stands in for the path. `.parent` is the directory
            // the entry was made in, which a fresh name says nothing about, and
            // `.name` is a basename in the working directory.
            _ => None,
        }
    }

    /// Whether this value is, or was derived from, a freshly created temp path.
    fn is_ephemeral(&self) -> bool {
        self.ephemeral().is_some()
    }

    fn truthiness(&self) -> Option<bool> {
        match self {
            Py::Bool(value) => Some(*value),
            Py::Int(value) => Some(*value != 0),
            Py::Str(value) | Py::Path(value) => Some(!value.is_empty()),
            Py::List(items) => Some(!items.is_empty()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Scope {
    names: HashMap<String, Py>,
    cwd: Option<String>,
    argv: Option<Vec<Value>>,
}

impl Scope {
    fn merge_uncertain(&mut self, branch: &Scope) {
        for (name, value) in &branch.names {
            if self.names.get(name) != Some(value) {
                self.names.insert(name.clone(), Py::Unknown);
            }
        }
        if branch.cwd != self.cwd {
            self.cwd = None;
        }
        if branch.argv != self.argv {
            self.argv = None;
        }
    }
}

pub(crate) fn walk(a: &mut Analyzer<'_>, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(Language::Python, source) else {
        a.uncertain(
            UncertaintyKind::ParseError,
            "Python source did not parse",
            frame.locate(0..source.len()),
        );
        return;
    };
    let mut scope = Scope {
        cwd: frame.cwd.clone(),
        argv: Some(frame.script_args.clone()),
        ..Scope::default()
    };
    let mut w = Walker {
        a,
        source,
        frame,
        root: tree.root_node(),
        calls: 0,
        exhausted: false,
    };
    w.block(tree.root_node(), &mut scope);
}

struct Walker<'a, 'c, 's, 't> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
    root: Node<'t>,
    calls: usize,
    exhausted: bool,
}

impl<'t> Walker<'_, '_, '_, 't> {
    fn at(&self, node: Node<'_>) -> Location {
        self.frame.locate(node.byte_range())
    }

    fn value_limit(&mut self, node: Node<'_>) {
        self.a.uncertain(
            UncertaintyKind::LimitExhausted(crate::model::Limit::ValueSize),
            "a Python value exceeded the configured size limit",
            self.at(node),
        );
    }

    fn known(&mut self, node: Node<'_>, value: String, path: bool) -> Py {
        if !self.a.budget.value_fits(value.len()) {
            self.value_limit(node);
            Py::Unknown
        } else if path {
            Py::Path(value)
        } else {
            Py::Str(value)
        }
    }

    fn bounded_value(&mut self, node: Node<'_>, value: Value) -> Value {
        match value {
            Value::Known(value) if !self.a.budget.value_fits(value.len()) => {
                self.value_limit(node);
                Value::Unknown
            }
            value => value,
        }
    }

    fn bounded_resolved(&mut self, node: Node<'t>, value: Value, cwd: Option<&str>) -> Value {
        if let crate::model::Target::Path(path) = paths::resolve(&value, cwd, self.a.ctx.path_style)
            && !self.a.budget.value_fits(path.len())
        {
            self.value_limit(node);
            Value::Unknown
        } else {
            value
        }
    }

    fn resolved_path(
        &mut self,
        node: Node<'t>,
        value: &Value,
        cwd: Option<&str>,
    ) -> Option<String> {
        match paths::resolve(value, cwd, self.a.ctx.path_style) {
            crate::model::Target::Path(path) if self.a.budget.value_fits(path.len()) => Some(path),
            crate::model::Target::Path(_) => {
                self.value_limit(node);
                None
            }
            crate::model::Target::Unresolved | crate::model::Target::Ephemeral { .. } => None,
        }
    }

    fn visit(&mut self, node: Node<'t>) -> bool {
        if self.exhausted {
            return false;
        }
        if self.a.budget.visit().is_err() {
            self.exhausted = true;
            self.a.uncertain(
                UncertaintyKind::LimitExhausted(crate::model::Limit::Nodes),
                "Python source was only partly analyzed",
                self.at(node),
            );
            return false;
        }
        true
    }

    fn unresolved(&mut self, node: Node<'_>, detail: impl Into<String>) {
        self.a
            .uncertain(UncertaintyKind::UnresolvedWrite, detail, self.at(node));
    }

    fn block(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            self.statement(child, scope);
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope) {
        if !self.visit(node) {
            return;
        }
        if ts::is_broken(node)
            || (node.has_error() && node.kind() != "module" && node.kind() != "block")
        {
            self.a.uncertain(
                UncertaintyKind::ParseError,
                "a Python statement could not be parsed",
                self.at(node),
            );
            return;
        }
        match node.kind() {
            "comment" | "pass_statement" | "break_statement" | "continue_statement"
            | "global_statement" | "nonlocal_statement" => {}
            "import_statement" => self.import(node, scope),
            "import_from_statement" => self.import_from(node, scope),
            "expression_statement" => {
                for child in ts::named_children(node) {
                    self.expression_or_assignment(child, scope);
                }
            }
            "with_statement" => self.with(node, scope),
            "for_statement" => self.for_loop(node, scope),
            "if_statement" | "while_statement" | "try_statement" | "match_statement" => {
                for child in ts::named_children(node) {
                    match child.kind() {
                        "block" | "elif_clause" | "else_clause" | "except_clause"
                        | "finally_clause" | "case_clause" => {
                            let mut branch = scope.clone();
                            self.block_like(child, &mut branch);
                            scope.merge_uncertain(&branch);
                        }
                        _ => {
                            self.eval(child, scope);
                        }
                    }
                }
            }
            "decorated_definition" => {
                for child in ts::named_children(node) {
                    if child.kind() == "decorator" {
                        self.eval_children(child, scope);
                    } else {
                        self.statement(child, scope);
                    }
                }
            }
            "function_definition" => {
                if let Some(params) = node.child_by_field_name("parameters") {
                    for p in ts::named_children(params) {
                        if let Some(default) = p.child_by_field_name("value") {
                            self.eval(default, scope);
                        }
                    }
                }
                if let Some(name) = node.child_by_field_name("name") {
                    scope.names.insert(
                        ts::text(name, self.source).to_string(),
                        Py::Def(node.byte_range()),
                    );
                }
            }
            "class_definition" => {
                if let Some(body) = node.child_by_field_name("body") {
                    let mut class_scope = scope.clone();
                    self.block(body, &mut class_scope);
                }
                if let Some(name) = node.child_by_field_name("name") {
                    scope
                        .names
                        .insert(ts::text(name, self.source).to_string(), Py::Data);
                }
            }
            _ => {
                self.eval_children(node, scope);
            }
        }
    }

    fn block_like(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            match child.kind() {
                "block" => self.block(child, scope),
                k if k.ends_with("_statement")
                    || k == "expression_statement"
                    || k == "decorated_definition"
                    || k == "function_definition"
                    || k == "class_definition" =>
                {
                    self.statement(child, scope)
                }
                _ => {
                    self.eval(child, scope);
                }
            }
        }
    }

    fn module_value(qualified: &str) -> Py {
        if qualified == "sys.argv" {
            return Py::Argv;
        }
        let root = qualified.split('.').next().unwrap_or(qualified);
        if KNOWN_MODULES.contains(&qualified) || KNOWN_MODULES.contains(&root) {
            Py::Api(qualified.to_string())
        } else {
            Py::Foreign(root.to_string())
        }
    }

    fn import(&mut self, node: Node<'t>, scope: &mut Scope) {
        for item in ts::named_children(node) {
            match item.kind() {
                "dotted_name" => {
                    let full = ts::text(item, self.source);
                    let top = full.split('.').next().unwrap_or(full);
                    scope.names.insert(top.to_string(), Self::module_value(top));
                }
                "aliased_import" => {
                    if let (Some(name), Some(alias)) = (
                        item.child_by_field_name("name"),
                        item.child_by_field_name("alias"),
                    ) {
                        scope.names.insert(
                            ts::text(alias, self.source).to_string(),
                            Self::module_value(ts::text(name, self.source)),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn import_from(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(module) = node
            .child_by_field_name("module_name")
            .map(|m| ts::text(m, self.source).to_string())
        else {
            return;
        };
        let mut cursor = node.walk();
        for item in node.children_by_field_name("name", &mut cursor) {
            let (name, alias) = match item.kind() {
                "aliased_import" => (
                    item.child_by_field_name("name")
                        .map(|n| ts::text(n, self.source)),
                    item.child_by_field_name("alias")
                        .map(|n| ts::text(n, self.source)),
                ),
                _ => (Some(ts::text(item, self.source)), None),
            };
            if let Some(name) = name {
                scope.names.insert(
                    alias.unwrap_or(name).to_string(),
                    Self::module_value(&format!("{module}.{name}")),
                );
            }
        }
    }

    fn expression_or_assignment(&mut self, node: Node<'t>, scope: &mut Scope) {
        match node.kind() {
            "assignment" => {
                let value = node
                    .child_by_field_name("right")
                    .map_or(Py::Unknown, |r| self.eval(r, scope));
                if let Some(left) = node.child_by_field_name("left") {
                    self.bind(left, value, scope);
                }
            }
            "augmented_assignment" => {
                if let Some(right) = node.child_by_field_name("right") {
                    self.eval(right, scope);
                }
                if let Some(left) = node.child_by_field_name("left") {
                    self.bind(left, Py::Unknown, scope);
                }
            }
            _ => {
                self.eval(node, scope);
            }
        }
    }

    fn bind(&mut self, target: Node<'t>, value: Py, scope: &mut Scope) {
        match target.kind() {
            "identifier" => {
                scope
                    .names
                    .insert(ts::text(target, self.source).to_string(), value);
            }
            "pattern_list" | "tuple_pattern" | "list_pattern" => {
                let parts = ts::named_children(target);
                let items = match value {
                    Py::List(items) if items.len() == parts.len() => items,
                    _ => vec![Py::Unknown; parts.len()],
                };
                for (part, item) in parts.into_iter().zip(items) {
                    self.bind(part, item, scope);
                }
            }
            "attribute" => {
                let object = target
                    .child_by_field_name("object")
                    .map_or(Py::Unknown, |o| self.eval(o, scope));
                let attr = target
                    .child_by_field_name("attribute")
                    .map(|a| ts::text(a, self.source));
                if object == Py::Api("sys".into()) && attr == Some("argv") {
                    scope.argv = None;
                }
            }
            "subscript"
                if target
                    .child_by_field_name("value")
                    .map(|v| self.eval(v, scope))
                    == Some(Py::Argv) =>
            {
                scope.argv = None;
            }
            _ => {}
        }
    }

    fn with(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            match child.kind() {
                "with_clause" => {
                    for item in ts::named_children(child) {
                        let Some(value) = item.child_by_field_name("value") else {
                            continue;
                        };
                        let bound = if value.kind() == "as_pattern" {
                            ts::named_children(value)
                                .into_iter()
                                .next()
                                .map_or(Py::Unknown, |resource| self.eval(resource, scope))
                        } else {
                            self.eval(value, scope)
                        };
                        let patterns = if value.kind() == "as_pattern" {
                            vec![value]
                        } else {
                            ts::named_children(item)
                                .into_iter()
                                .filter(|child| child.kind() == "as_pattern")
                                .collect()
                        };
                        for pattern in patterns {
                            if let Some(alias) = pattern
                                .child_by_field_name("alias")
                                .and_then(|alias| ts::named_children(alias).into_iter().next())
                            {
                                self.bind(alias, bound.clone(), scope);
                            }
                        }
                    }
                }
                "block" => self.block(child, scope),
                _ => {}
            }
        }
    }

    fn for_loop(&mut self, node: Node<'t>, scope: &mut Scope) {
        let items = node
            .child_by_field_name("right")
            .map_or(Py::Unknown, |r| self.eval(r, scope));
        let (Some(left), Some(body)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("body"),
        ) else {
            return;
        };
        let mut after = scope.clone();
        match items {
            Py::List(items) if items.len() <= MAX_LOOP_ITEMS => {
                for item in items {
                    let mut iteration = scope.clone();
                    self.bind(left, item, &mut iteration);
                    self.block(body, &mut iteration);
                    after.merge_uncertain(&iteration);
                }
            }
            _ => {
                let mut iteration = scope.clone();
                self.bind(left, Py::Unknown, &mut iteration);
                self.block(body, &mut iteration);
                after.merge_uncertain(&iteration);
            }
        }
        let mut unbind = after.clone();
        self.bind(left, Py::Unknown, &mut unbind);
        *scope = unbind;
        if let Some(alternative) = node.child_by_field_name("alternative") {
            self.block_like(alternative, scope);
        }
    }

    fn eval_children(&mut self, node: Node<'t>, scope: &mut Scope) -> Py {
        for child in ts::named_children(node) {
            self.eval(child, scope);
        }
        Py::Unknown
    }

    fn eval(&mut self, node: Node<'t>, scope: &mut Scope) -> Py {
        if !self.visit(node) {
            return Py::Unknown;
        }
        let text = ts::text(node, self.source);
        match node.kind() {
            "string" => self.string(node, scope),
            "concatenated_string" => {
                let mut out = String::new();
                for part in ts::named_children(node) {
                    match self.eval(part, scope) {
                        Py::Str(s) => out.push_str(&s),
                        _ => return Py::Unknown,
                    }
                }
                self.known(node, out, false)
            }
            "integer" => text.parse().map_or(Py::Data, Py::Int),
            "true" => Py::Bool(true),
            "false" => Py::Bool(false),
            "none" => Py::Bool(false),
            "float" | "lambda" => Py::Data,
            "identifier" => match scope.names.get(text) {
                Some(v) => v.clone(),
                None if text == "open" => Py::Api("builtins.open".into()),
                None if matches!(text, "exec" | "eval" | "compile" | "__import__" | "getattr") => {
                    Py::Api(format!("builtins.{text}"))
                }
                None if BUILTINS.contains(&text) => Py::Api(format!("builtins.{text}")),
                None => Py::Unknown,
            },
            "attribute" => {
                let object = node
                    .child_by_field_name("object")
                    .map_or(Py::Unknown, |o| self.eval(o, scope));
                let attr = node
                    .child_by_field_name("attribute")
                    .map_or("", |a| ts::text(a, self.source))
                    .to_string();
                self.attribute(node, object, &attr, scope)
            }
            "subscript" => {
                let value = node
                    .child_by_field_name("value")
                    .map_or(Py::Unknown, |v| self.eval(v, scope));
                let index = node
                    .child_by_field_name("subscript")
                    .map_or(Py::Unknown, |s| self.eval(s, scope));
                match (value, index) {
                    (Py::Argv, Py::Int(i)) => match scope
                        .argv
                        .as_ref()
                        .and_then(|argv| usize::try_from(i).ok().and_then(|i| argv.get(i)))
                    {
                        Some(Value::Known(s)) => self.known(node, s.clone(), false),
                        _ => Py::Unknown,
                    },
                    (Py::List(items), Py::Int(i)) => usize::try_from(i)
                        .ok()
                        .and_then(|i| items.get(i).cloned())
                        .unwrap_or(Py::Unknown),
                    _ => Py::Unknown,
                }
            }
            "binary_operator" => {
                let left = node
                    .child_by_field_name("left")
                    .map_or(Py::Unknown, |l| self.eval(l, scope));
                let right = node
                    .child_by_field_name("right")
                    .map_or(Py::Unknown, |r| self.eval(r, scope));
                let op = node
                    .child_by_field_name("operator")
                    .map_or("", |o| ts::text(o, self.source));
                match (op, left, right) {
                    ("/", Py::Path(l), Py::Str(r) | Py::Path(r)) => {
                        self.known(node, join(&l, &r), true)
                    }
                    ("+", Py::Str(l), Py::Str(r)) => self.known(node, l + &r, false),
                    _ => Py::Unknown,
                }
            }
            "call" => self.call(node, scope),
            "list" | "tuple" => Py::List(
                ts::named_children(node)
                    .into_iter()
                    .map(|c| self.eval(c, scope))
                    .collect(),
            ),
            "parenthesized_expression" | "await" => ts::named_children(node)
                .into_iter()
                .map(|c| self.eval(c, scope))
                .last()
                .unwrap_or(Py::Unknown),
            "conditional_expression" => {
                let values: Vec<Py> = ts::named_children(node)
                    .into_iter()
                    .map(|c| self.eval(c, scope))
                    .collect();
                match values.as_slice() {
                    [a, _, b] if a == b => a.clone(),
                    _ => Py::Unknown,
                }
            }
            _ => self.eval_children(node, scope),
        }
    }

    fn string(&mut self, node: Node<'t>, scope: &mut Scope) -> Py {
        let mut out = String::new();
        let mut known = true;
        for part in ts::named_children(node) {
            match part.kind() {
                "string_start" | "string_end" => {}
                "string_content" => out.push_str(ts::text(part, self.source)),
                "escape_sequence" => out.push_str(match ts::text(part, self.source) {
                    "\\n" => "\n",
                    "\\t" => "\t",
                    "\\\\" => "\\",
                    "\\'" => "'",
                    "\\\"" => "\"",
                    other => other,
                }),
                "interpolation" => match part
                    .child_by_field_name("expression")
                    .map(|e| self.eval(e, scope))
                {
                    Some(Py::Str(s) | Py::Path(s)) => out.push_str(&s),
                    Some(Py::Int(i)) => out.push_str(&i.to_string()),
                    _ => known = false,
                },
                _ => known = false,
            }
        }
        if known {
            self.known(node, out, false)
        } else {
            Py::Unknown
        }
    }

    fn attribute(&mut self, node: Node<'t>, object: Py, attr: &str, scope: &Scope) -> Py {
        match object {
            Py::Api(module) => Self::module_value(&format!("{module}.{attr}")),
            Py::Foreign(module) => Py::Foreign(module),
            Py::Path(p) => match attr {
                "parent" => self.known(node, paths::parent(&p).unwrap_or(".").to_string(), true),
                "name" => self.known(node, normalize::basename(&p).to_string(), false),
                "stem" | "suffix" => Py::Data,
                _ => Py::Method(Box::new(Py::Path(p)), attr.to_string()),
            },
            Py::Argv if scope.argv.is_some() => Py::Method(Box::new(Py::Argv), attr.to_string()),
            other => Py::Method(Box::new(other), attr.to_string()),
        }
    }

    fn call(&mut self, node: Node<'t>, scope: &mut Scope) -> Py {
        let callee = node
            .child_by_field_name("function")
            .map_or(Py::Unknown, |f| self.eval(f, scope));
        let mut positional = Vec::new();
        let mut keywords = HashMap::new();
        if let Some(arguments) = node.child_by_field_name("arguments") {
            for arg in ts::named_children(arguments) {
                if arg.kind() == "keyword_argument" {
                    if let (Some(name), Some(value)) = (
                        arg.child_by_field_name("name"),
                        arg.child_by_field_name("value"),
                    ) {
                        let v = self.eval(value, scope);
                        keywords.insert(ts::text(name, self.source).to_string(), (v, value));
                    }
                } else {
                    positional.push(self.eval(arg, scope));
                }
            }
        }
        let arg = |i, name| {
            positional
                .get(i)
                .cloned()
                .or_else(|| keywords.get(name).map(|(v, _)| v.clone()))
                .unwrap_or(Py::Unknown)
        };
        let cwd = scope.cwd.clone();
        match callee {
            Py::Api(api) => match api.as_str() {
                "builtins.open" | "io.open" | "codecs.open" => self.open(
                    node,
                    &arg(0, "file"),
                    &arg(1, "mode"),
                    keywords.contains_key("mode") || positional.len() > 1,
                    cwd.as_deref(),
                ),
                "tempfile.mkdtemp" | "tempfile.TemporaryDirectory" | "tempfile.mkstemp" => {
                    let named = keywords
                        .get("dir")
                        .map(|(v, _)| v)
                        .or_else(|| positional.get(2));
                    match temp_dir(named, cwd.as_deref(), self.a.ctx.path_style) {
                        Some(at) if api == "tempfile.mkstemp" => {
                            Py::List(vec![Py::Unknown, Py::Ephemeral(at)])
                        }
                        Some(at) => Py::Ephemeral(at),
                        None => Py::Unknown,
                    }
                }
                "tempfile.NamedTemporaryFile" | "tempfile.TemporaryFile" => {
                    // `dir` is the seventh parameter of both, so a call passing
                    // that many positionally is one this cannot read.
                    let named = keywords.get("dir").map(|(v, _)| v);
                    let at = if positional.len() > 4 {
                        None
                    } else {
                        temp_dir(named, cwd.as_deref(), self.a.ctx.path_style)
                    };
                    match at {
                        Some(at) => Py::WriteHandle {
                            target: Box::new(Py::Ephemeral(at)),
                            write_op: Some(FileOp::Overwrite),
                        },
                        None => Py::Unknown,
                    }
                }
                "pathlib.Path"
                | "pathlib.PurePath"
                | "pathlib.PosixPath"
                | "pathlib.WindowsPath" => {
                    if let Some(joined) = self.temp_join(&positional) {
                        return joined;
                    }
                    let mut out = None;
                    for part in &positional {
                        match (part, &out) {
                            (Py::Str(s) | Py::Path(s), None) => out = Some(s.clone()),
                            (Py::Str(s) | Py::Path(s), Some(base)) => out = Some(join(base, s)),
                            _ => return Py::Unknown,
                        }
                    }
                    self.known(node, out.unwrap_or_else(|| ".".into()), true)
                }
                "pathlib.Path.cwd" | "os.getcwd" => cwd
                    .map(|cwd| self.known(node, cwd, true))
                    .unwrap_or(Py::Unknown),
                "os.path.join" => {
                    if let Some(joined) = self.temp_join(&positional) {
                        return joined;
                    }
                    let parts: Option<Vec<String>> = positional
                        .iter()
                        .map(|p| match p {
                            Py::Str(s) | Py::Path(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect();
                    parts.map_or(Py::Unknown, |parts| {
                        self.known(
                            node,
                            parts
                                .iter()
                                .skip(1)
                                .fold(parts[0].clone(), |acc, p| join(&acc, p)),
                            false,
                        )
                    })
                }
                "os.path.dirname" => match arg(0, "p") {
                    Py::Str(s) | Py::Path(s) => {
                        self.known(node, paths::parent(&s).unwrap_or("").to_string(), false)
                    }
                    _ => Py::Unknown,
                },
                "os.path.basename" => match arg(0, "p") {
                    Py::Str(s) | Py::Path(s) => {
                        self.known(node, normalize::basename(&s).to_string(), false)
                    }
                    _ => Py::Unknown,
                },
                "os.path.abspath" | "os.path.realpath" => match (arg(0, "path"), cwd) {
                    (Py::Str(s) | Py::Path(s), _) if s.starts_with('/') => {
                        self.known(node, s, false)
                    }
                    (Py::Str(s) | Py::Path(s), Some(dir)) => {
                        self.known(node, join(&dir, &s), false)
                    }
                    _ => Py::Unknown,
                },
                "os.chdir" => {
                    scope.cwd = match arg(0, "path") {
                        Py::Str(s) | Py::Path(s) => match paths::resolve(
                            &Value::Known(s),
                            scope.cwd.as_deref(),
                            self.a.ctx.path_style,
                        ) {
                            crate::model::Target::Path(d) if self.a.budget.value_fits(d.len()) => {
                                Some(d)
                            }
                            crate::model::Target::Path(_) => {
                                self.value_limit(node);
                                None
                            }
                            crate::model::Target::Unresolved
                            | crate::model::Target::Ephemeral { .. } => None,
                        },
                        _ => None,
                    };
                    Py::Data
                }
                "os.remove" | "os.unlink" => {
                    self.effect(node, FileOp::Delete, &arg(0, "path"), scope)
                }
                "os.rename" | "os.replace" | "os.renames" | "shutil.move" => {
                    self.effect(node, FileOp::Rename, &arg(0, "src"), scope);
                    self.effect(node, FileOp::Rename, &arg(1, "dst"), scope)
                }
                "os.truncate" => self.effect(node, FileOp::Overwrite, &arg(0, "path"), scope),
                "os.symlink" | "os.link" => {
                    let link = arg(1, "dst");
                    self.link_escape(node, &link, &arg(0, "src"));
                    self.effect(node, FileOp::Create, &link, scope)
                }
                "os.open" => {
                    self.unresolved(node, "`os.open` flags were not analyzed");
                    Py::WriteHandle {
                        target: Box::new(Py::Unknown),
                        write_op: None,
                    }
                }
                "shutil.copy" | "shutil.copy2" | "shutil.copyfile" => {
                    self.effect(node, FileOp::Copy, &arg(1, "dst"), scope)
                }
                "shutil.rmtree" => self.tree(node, &arg(0, "path"), "shutil.rmtree", scope),
                "shutil.copytree" => self.tree(node, &arg(1, "dst"), "shutil.copytree", scope),
                "shutil.unpack_archive" => {
                    let dir = match arg(1, "extract_dir") {
                        Py::Unknown
                            if positional.len() < 2 && !keywords.contains_key("extract_dir") =>
                        {
                            Py::Str(".".into())
                        }
                        d => d,
                    };
                    self.tree(node, &dir, "shutil.unpack_archive", scope)
                }
                "shutil.make_archive" => {
                    self.unresolved(node, "`shutil.make_archive` output was not analyzed");
                    Py::Data
                }
                "os.system" | "os.popen" => self.shell_source(node, &arg(0, "command"), scope),
                "subprocess.run"
                | "subprocess.call"
                | "subprocess.check_call"
                | "subprocess.check_output"
                | "subprocess.Popen" => {
                    let shell = keywords
                        .get("shell")
                        .map(|(value, _)| value.clone())
                        .unwrap_or(Py::Bool(false));
                    let cwd_arg = keywords.get("cwd").map(|(v, _)| v.clone());
                    self.subprocess(node, &arg(0, "args"), &shell, cwd_arg, scope)
                }
                "builtins.exec" | "builtins.eval" => match arg(0, "source") {
                    Py::Str(source) => {
                        let child = Frame {
                            depth: self.frame.depth + 1,
                            script_args: scope.argv.clone().unwrap_or_default(),
                            base: Some(self.at(node)),
                            cwd: scope.cwd.clone(),
                        };
                        self.a.source(Language::Python, &source, child);
                        Py::Unknown
                    }
                    _ => {
                        self.unresolved(node, "dynamically evaluated Python source");
                        Py::Unknown
                    }
                },
                "builtins.compile" => Py::Data,
                "builtins.__import__" | "importlib.import_module" | "builtins.getattr" => {
                    Py::Unknown
                }
                _ => Py::Data,
            },
            Py::Method(receiver, method) => {
                self.method(node, *receiver, &method, &positional, &keywords, scope)
            }
            Py::Foreign(module) => {
                self.unresolved(
                    node,
                    format!("a call into `{module}`, which devkit does not model"),
                );
                Py::Unknown
            }
            Py::Def(range) => {
                self.calls += 1;
                if self.frame.depth + self.calls > self.a.budget.limits().depth {
                    self.a.uncertain(
                        UncertaintyKind::LimitExhausted(crate::model::Limit::Depth),
                        "a Python call chain was not followed",
                        self.at(node),
                    );
                } else if let Some(def) =
                    self.root.descendant_for_byte_range(range.start, range.end)
                    && let Some(body) = def.child_by_field_name("body")
                {
                    let mut call_scope = scope.clone();
                    if let Some(params) = def.child_by_field_name("parameters") {
                        for (i, p) in ts::named_children(params).into_iter().enumerate() {
                            let name = if p.kind() == "identifier" {
                                Some(p)
                            } else {
                                p.child_by_field_name("name")
                            };
                            if let Some(name) = name {
                                call_scope.names.insert(
                                    ts::text(name, self.source).to_string(),
                                    positional.get(i).cloned().unwrap_or(Py::Unknown),
                                );
                            }
                        }
                    }
                    self.block(body, &mut call_scope);
                }
                self.calls -= 1;
                Py::Unknown
            }
            Py::Unknown => {
                let name = node
                    .child_by_field_name("function")
                    .map_or("", |f| ts::text(f, self.source))
                    .to_string();
                self.unresolved(
                    node,
                    format!("a call to `{name}`, which could not be resolved"),
                );
                Py::Unknown
            }
            Py::Str(_)
            | Py::Path(_)
            | Py::Ephemeral(_)
            | Py::Int(_)
            | Py::Bool(_)
            | Py::List(_)
            | Py::Argv
            | Py::WriteHandle { .. }
            | Py::Data => Py::Data,
        }
    }

    fn method(
        &mut self,
        node: Node<'t>,
        receiver: Py,
        method: &str,
        args: &[Py],
        keywords: &HashMap<String, (Py, Node<'t>)>,
        scope: &mut Scope,
    ) -> Py {
        match receiver {
            Py::Path(p) => {
                let this = Py::Path(p.clone());
                if let Some(result) = self.path_mutation(node, &this, method, args, keywords, scope)
                {
                    return result;
                }
                match method {
                    "with_suffix" => match args.first() {
                        Some(Py::Str(s)) => self.known(
                            node,
                            format!(
                                "{}{s}",
                                p.rsplit_once('.')
                                    .filter(|(stem, _)| !stem.ends_with('/'))
                                    .map_or(p.as_str(), |(stem, _)| stem)
                            ),
                            true,
                        ),
                        _ => Py::Unknown,
                    },
                    "with_name" => match args.first() {
                        Some(Py::Str(s)) => {
                            self.known(node, join(paths::parent(&p).unwrap_or("."), s), true)
                        }
                        _ => Py::Unknown,
                    },
                    "joinpath" => args
                        .iter()
                        .try_fold(p, |acc, a| match a {
                            Py::Str(s) | Py::Path(s) => Some(join(&acc, s)),
                            _ => None,
                        })
                        .map_or(Py::Unknown, |path| self.known(node, path, true)),
                    "resolve" | "absolute" => match &scope.cwd {
                        Some(dir) if !p.starts_with('/') => self.known(node, join(dir, &p), true),
                        _ => self.known(node, p, true),
                    },
                    "iterdir" | "glob" | "rglob" | "expanduser" => Py::Unknown,
                    _ => Py::Data,
                }
            }
            Py::Argv => {
                if matches!(
                    method,
                    "append"
                        | "extend"
                        | "insert"
                        | "pop"
                        | "remove"
                        | "clear"
                        | "reverse"
                        | "sort"
                ) {
                    scope.argv = None;
                }
                Py::Data
            }
            Py::WriteHandle { target, write_op } if method == "write" => {
                if let Some(op) = write_op {
                    self.effect(node, op, &target, scope);
                }
                Py::Data
            }
            Py::Str(s) if method == "join" => match args.first() {
                Some(Py::List(items)) => items
                    .iter()
                    .map(|i| match i {
                        Py::Str(x) => Some(x.as_str()),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .map_or(Py::Unknown, |parts| self.known(node, parts.join(&s), false)),
                _ => Py::Unknown,
            },
            // `str.replace` takes the old and new text; `Path.replace` takes one target.
            Py::Unknown if method == "replace" && args.len() >= 2 => Py::Unknown,
            // A path this could not determine is still opened for writing
            // when the mode says so, and a read mode still writes nothing.
            Py::Unknown if method == "open" => {
                let (mode, given) = open_mode(args, keywords);
                self.open(node, &Py::Unknown, &mode, given, scope.cwd.as_deref())
            }
            Py::Unknown if WRITE_METHODS.contains(&method) => {
                self.unresolved(
                    node,
                    format!("`.{method}()` on a value whose type could not be determined"),
                );
                Py::Unknown
            }
            Py::Foreign(module) => {
                self.unresolved(
                    node,
                    format!("a call into `{module}`, which devkit does not model"),
                );
                Py::Unknown
            }
            receiver if receiver.is_ephemeral() => {
                let this = receiver.clone();
                if let Some(result) = self.path_mutation(node, &this, method, args, keywords, scope)
                {
                    return result;
                }
                match method {
                    "joinpath" => {
                        let mut parts = vec![this];
                        parts.extend(args.iter().cloned());
                        self.temp_join(&parts).unwrap_or(Py::Unknown)
                    }
                    _ => Py::Unknown,
                }
            }
            // Losing track of a receiver must not lose the write with it: a
            // method known to mutate a path still reports what it could not
            // determine.
            _ if MUTATING_PATH_METHODS.contains(&method) => {
                self.unresolved(
                    node,
                    format!("`{method}` writes through a value that could not be determined"),
                );
                Py::Unknown
            }
            _ => Py::Unknown,
        }
    }

    fn open(
        &mut self,
        node: Node<'t>,
        target: &Py,
        mode: &Py,
        mode_given: bool,
        cwd: Option<&str>,
    ) -> Py {
        let op = if !mode_given {
            None
        } else {
            Some(match mode {
                Py::Str(m) if m.contains('w') => FileOp::Overwrite,
                Py::Str(m) if m.contains('a') => FileOp::Append,
                Py::Str(m) if m.contains('x') => FileOp::Create,
                Py::Str(m) if m.contains('+') => FileOp::Overwrite,
                Py::Str(_) => return Py::Data,
                _ => {
                    self.unresolved(
                        node,
                        "a file opened with a mode that could not be determined",
                    );
                    return Py::WriteHandle {
                        target: Box::new(target.clone()),
                        write_op: None,
                    };
                }
            })
        };
        if let Some(op) = op {
            self.file_effect(node, op, target, cwd);
        }
        Py::WriteHandle {
            target: Box::new(target.clone()),
            write_op: if mode_given {
                None
            } else {
                Some(FileOp::Overwrite)
            },
        }
    }

    /// The effect of a `pathlib` method that mutates the path it is called on,
    /// whatever that receiver turned out to be. Covers every name in
    /// [`MUTATING_PATH_METHODS`], and `open`, whose mode decides. `None` for
    /// any other method, leaving the caller to read it against its own
    /// receiver.
    fn path_mutation(
        &mut self,
        node: Node<'t>,
        this: &Py,
        method: &str,
        args: &[Py],
        keywords: &HashMap<String, (Py, Node<'t>)>,
        scope: &Scope,
    ) -> Option<Py> {
        let dest = || args.first().cloned().unwrap_or(Py::Unknown);
        Some(match method {
            "write_text" | "write_bytes" => self.effect(node, FileOp::Overwrite, this, scope),
            "touch" | "mkdir" => self.effect(node, FileOp::Create, this, scope),
            "symlink_to" | "hardlink_to" => {
                self.link_escape(node, this, &dest());
                self.effect(node, FileOp::Create, this, scope)
            }
            "unlink" | "rmdir" => self.effect(node, FileOp::Delete, this, scope),
            "copy" => self.effect(node, FileOp::Copy, &dest(), scope),
            "copy_into" => self.tree(node, &dest(), method, scope),
            // A move renames the source away, so the source is written too.
            "move" => {
                self.effect(node, FileOp::Rename, this, scope);
                self.effect(node, FileOp::Rename, &dest(), scope)
            }
            "move_into" => {
                self.effect(node, FileOp::Rename, this, scope);
                self.tree(node, &dest(), method, scope)
            }
            "rename" | "replace" => {
                self.effect(node, FileOp::Rename, this, scope);
                let dest = dest();
                self.effect(node, FileOp::Rename, &dest, scope);
                dest
            }
            "open" => {
                let (mode, given) = open_mode(args, keywords);
                self.open(node, this, &mode, given, scope.cwd.as_deref())
            }
            _ => return None,
        })
    }

    /// A link made inside a freshly created directory can point out of it, and
    /// every write through the link lands wherever it points. Containment of
    /// the fresh subtree is then only as good as the link target, so a target
    /// that cannot be shown to stay inside costs the exemption.
    fn link_escape(&mut self, node: Node<'t>, link: &Py, target: &Py) {
        if !link.is_ephemeral() {
            return;
        }
        let inside = match target {
            Py::Str(s) | Py::Path(s) => paths::stays_within(&[s.as_str()], self.a.ctx.path_style),
            _ => false,
        };
        if !inside {
            self.unresolved(
                node,
                "a link made in a fresh directory could point outside it",
            );
        }
    }

    fn effect(&mut self, node: Node<'t>, op: FileOp, target: &Py, scope: &Scope) -> Py {
        self.file_effect(node, op, target, scope.cwd.as_deref());
        Py::Data
    }

    /// A join whose first component is a freshly created temp path. `None` when
    /// there is no temp path to bound. It keeps the exemption only while every
    /// later component is known and lands inside that directory: an unknown one
    /// cannot show containment, and a temp path anywhere but first is not the
    /// thing being extended.
    fn temp_join(&self, parts: &[Py]) -> Option<Py> {
        if !parts.iter().any(Py::is_ephemeral) {
            return None;
        }
        let Some(at) = parts.first().and_then(Py::ephemeral) else {
            return Some(Py::Unknown);
        };
        let Some(rest) = parts[1..]
            .iter()
            .map(|p| match p {
                Py::Str(s) | Py::Path(s) => Some(s.as_str()),
                _ => None,
            })
            .collect::<Option<Vec<&str>>>()
        else {
            return Some(Py::Unknown);
        };
        Some(if paths::stays_within(&rest, self.a.ctx.path_style) {
            Py::Ephemeral(at)
        } else {
            Py::Unknown
        })
    }

    fn file_effect(&mut self, node: Node<'t>, op: FileOp, target: &Py, cwd: Option<&str>) {
        let value = self.bounded_resolved(node, target.as_value(), cwd);
        self.a.file_effect(op, &value, cwd, self.at(node));
    }

    fn tree(&mut self, node: Node<'t>, scope_path: &Py, by: &str, scope: &Scope) -> Py {
        let value = self.bounded_resolved(node, scope_path.as_value(), scope.cwd.as_deref());
        self.a
            .tree_effect(&value, false, scope.cwd.as_deref(), by, self.at(node));
        Py::Data
    }

    fn shell_source(&mut self, node: Node<'t>, command: &Py, scope: &Scope) -> Py {
        match command {
            Py::Str(source) => {
                let child = Frame {
                    depth: self.frame.depth + 1,
                    script_args: Vec::new(),
                    base: Some(self.at(node)),
                    cwd: scope.cwd.clone(),
                };
                self.a.source(Language::Bash, source, child);
            }
            _ => self.unresolved(node, "a shell command that could not be determined"),
        }
        Py::Data
    }

    fn subprocess(
        &mut self,
        node: Node<'t>,
        args: &Py,
        shell: &Py,
        cwd_arg: Option<Py>,
        scope: &Scope,
    ) -> Py {
        let cwd = match cwd_arg {
            None => scope.cwd.clone(),
            Some(Py::Str(d) | Py::Path(d)) => {
                self.resolved_path(node, &Value::Known(d), scope.cwd.as_deref())
            }
            Some(_) => None,
        };
        let inner_scope = Scope {
            cwd: cwd.clone(),
            ..scope.clone()
        };
        match (args, shell.truthiness()) {
            (Py::Str(_), Some(true)) => self.shell_source(node, args, &inner_scope),
            (Py::Str(program), Some(false)) => {
                self.run_argv(node, vec![Value::Known(program.clone())], cwd)
            }
            (Py::List(items), Some(false)) => {
                self.run_argv(node, items.iter().map(Py::as_value).collect(), cwd)
            }
            (Py::Str(_), None) => {
                self.unresolved(node, "a subprocess shell mode that could not be determined");
                Py::Data
            }
            _ => {
                self.unresolved(node, "a subprocess command that could not be determined");
                Py::Data
            }
        }
    }

    fn run_argv(&mut self, node: Node<'t>, argv: Vec<Value>, cwd: Option<String>) -> Py {
        let words = argv
            .into_iter()
            .map(|value| {
                let value = self.bounded_value(node, value);
                Word {
                    typed: value.known().unwrap_or("?").to_string(),
                    value,
                    span: node.byte_range(),
                }
            })
            .collect();
        self.a.invocation(
            RawInvocation {
                words,
                stdin: Stdin::None,
                cwd,
                language: Language::Python,
                location: self.at(node),
            },
            self.frame,
        );
        Py::Data
    }
}

fn join(base: &str, rel: &str) -> String {
    if rel.starts_with('/') {
        return rel.to_string();
    }
    if base == "." {
        return rel.to_string();
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        rel.strip_prefix("./").unwrap_or(rel)
    )
}

#[cfg(test)]
mod tests {
    use crate::{
        Analysis,
        model::{FileOp, Limit, UncertaintyKind},
        testutil::{bash, targets},
    };

    fn py(source: &str) -> Analysis {
        bash(&format!("python3 - <<'PY'\n{source}\nPY\n"))
    }

    fn unresolved(a: &Analysis) -> bool {
        a.uncertainties
            .iter()
            .any(|u| u.kind == UncertaintyKind::UnresolvedWrite)
            || targets(a).contains(&"?".to_string())
    }

    #[test]
    fn a_tempfile_directory_write_is_ephemeral() {
        let a = py(
            "import tempfile, os\nd = tempfile.mkdtemp()\nopen(os.path.join(d, 'out.txt'), 'w').write('x')",
        );
        assert_eq!(targets(&a), ["<ephemeral>"]);
        assert!(!unresolved(&a), "{:?}", a.uncertainties);
    }

    #[test]
    fn a_named_temporary_file_is_ephemeral() {
        let a = py("import tempfile\nf = tempfile.NamedTemporaryFile(dir='src')\nf.write(b'x')");
        assert_eq!(targets(&a), ["<ephemeral>"]);
        assert!(!unresolved(&a), "{:?}", a.uncertainties);
    }

    #[test]
    fn a_path_under_gettempdir_stays_unresolved() {
        let a = py(
            "import tempfile, os\nopen(os.path.join(tempfile.gettempdir(), 'out.txt'), 'w').write('x')",
        );
        assert!(unresolved(&a), "{:?}", targets(&a));
    }

    #[test]
    fn pathlib_writes_a_literal_path() {
        let a = py("from pathlib import Path\nPath('src/a.ts').write_text('x')");
        assert_eq!(targets(&a), ["/repo/src/a.ts"]);
        assert!(a.file_effects[0].location.embedded.is_some());
    }

    /// Every method the analyzer calls mutating has to record something when
    /// the receiver is a path it resolved, or the list and the dispatch that
    /// reads it have drifted apart.
    #[test]
    fn every_mutating_path_method_records_an_effect() {
        for method in crate::python::MUTATING_PATH_METHODS {
            let a = py(&format!(
                "import pathlib\npathlib.Path('a.txt').{method}('b.txt')"
            ));
            assert!(
                !a.file_effects.is_empty() || !a.tree_effects.is_empty(),
                "`{method}` recorded nothing"
            );
        }
    }

    #[test]
    fn open_modes_decide_the_operation() {
        let a = py(
            "open('a.txt', 'w')\nopen('b.txt', 'a')\nopen('c.txt')\nwith open('d.txt', mode='x') as f:\n    f.write('1')",
        );
        assert_eq!(targets(&a), ["/repo/a.txt", "/repo/b.txt", "/repo/d.txt"]);
        let ops: Vec<FileOp> = a.file_effects.iter().map(|e| e.op).collect();
        assert_eq!(ops, [FileOp::Overwrite, FileOp::Append, FileOp::Create]);
    }

    #[test]
    fn a_two_argument_replace_is_a_string_method() {
        let a = py("print(row['command'][:1000].replace('\\n', ' | '))");
        assert!(!unresolved(&a), "{a:?}");
        assert!(unresolved(&py("src.replace('b.txt')")));
    }

    #[test]
    fn an_unknown_mode_is_unresolved() {
        assert!(unresolved(&py(
            "import sys\nopen('a.txt', sys.stdin.read())"
        )));
    }

    #[test]
    fn aliases_and_path_composition_resolve() {
        assert_eq!(
            targets(&py(
                "import pathlib as pl\nroot = pl.Path('src')\n(root / 'b.rs').write_text('')"
            )),
            ["/repo/src/b.rs"]
        );
        assert_eq!(
            targets(&py(
                "import os\nname = 'c'\nopen(os.path.join('gen', f'{name}.txt'), 'w')"
            )),
            ["/repo/gen/c.txt"]
        );
    }

    #[test]
    fn a_shell_variable_reaches_sys_argv() {
        let a = bash(
            "f=src/a.ts; python3 - \"$f\" <<'PY'\nimport sys\nfrom pathlib import Path\np = Path(sys.argv[1])\np.write_text('x')\nPY\n",
        );
        assert_eq!(targets(&a), ["/repo/src/a.ts"]);
        let c = bash("python3 -c \"import sys; open(sys.argv[1], 'w')\" out.txt");
        assert_eq!(targets(&c), ["/repo/out.txt"]);
    }

    #[test]
    fn argv_that_cannot_be_bound_is_unresolved() {
        assert!(unresolved(&bash(
            "for f in $(ls); do python3 - \"$f\" <<'PY'\nimport sys\nopen(sys.argv[1], 'w')\nPY\ndone"
        )));
        assert!(unresolved(&bash(
            "python3 - a.txt <<'PY'\nimport sys\nopen(sys.argv[2], 'w')\nPY\n"
        )));
        assert!(unresolved(&bash(
            "python3 - a.txt <<'PY'\nimport sys\nsys.argv = ['x', 'b.txt']\nopen(sys.argv[1], 'w')\nPY\n"
        )));
    }

    #[test]
    fn rebinding_a_name_drops_what_it_meant() {
        let a = py("def open(p, m):\n    pass\nopen('a.txt', 'w')");
        assert!(
            a.file_effects.is_empty() && a.uncertainties.is_empty(),
            "{a:?}"
        );
    }

    #[test]
    fn a_function_body_runs_only_when_called() {
        assert!(
            py("def f():\n    open('a.txt', 'w')\n")
                .file_effects
                .is_empty()
        );
        assert_eq!(targets(&py("def f():\n    open('a.txt', 'w')\nf()")), [
            "/repo/a.txt"
        ]);
        assert_eq!(targets(&py("def f(x=open('d.txt', 'w')):\n    pass")), [
            "/repo/d.txt"
        ]);
    }

    #[test]
    fn subprocess_commands_are_analyzed_when_constant() {
        assert_eq!(
            targets(&py("import subprocess\nsubprocess.run(['rm', 'a.txt'])")),
            ["/repo/a.txt"]
        );
        assert_eq!(
            targets(&py(
                "import subprocess\nsubprocess.run('echo x > s.txt', shell=True)"
            )),
            ["/repo/s.txt"]
        );
        assert!(unresolved(&py(
            "import subprocess, sys\nsubprocess.run(sys.stdin.read(), shell=True)"
        )));
    }

    #[test]
    fn subprocess_shell_truthiness_analyzes_a_truthy_integer() {
        assert_eq!(
            targets(&py(
                "import subprocess\nsubprocess.run('echo x > shared.txt', shell=1)"
            )),
            ["/repo/shared.txt"]
        );
    }

    #[test]
    fn subprocess_unknown_shell_truthiness_stays_unresolved() {
        assert!(unresolved(&py(
            "import subprocess\nshell_mode = object()\nsubprocess.run('echo x > shared.txt', shell=shell_mode)"
        )));
    }

    #[test]
    fn pathlib_open_accepts_a_keyword_mode() {
        assert_eq!(
            targets(&py(
                "from pathlib import Path\nPath('shared.txt').open(mode='w')"
            )),
            ["/repo/shared.txt"]
        );
    }

    #[test]
    fn compile_does_not_execute_constant_python_source() {
        let a = py("compile(\"open('compiled.txt', 'w')\", '<string>', 'exec')");
        assert!(a.file_effects.is_empty(), "{a:?}");
        assert!(a.uncertainties.is_empty(), "{a:?}");
    }

    #[test]
    fn oversized_python_values_are_not_emitted_as_targets() {
        let path = "x".repeat(64 * 1024 - "/repo/".len() + 1);
        let a = py(&format!("open('{path}', 'w')"));
        assert!(
            a.uncertainties
                .iter()
                .any(|u| { u.kind == UncertaintyKind::LimitExhausted(Limit::ValueSize) })
        );
        assert!(targets(&a).iter().all(|target| target.len() <= 64 * 1024));
    }

    #[test]
    fn with_open_binds_the_file_handle_alias() {
        let a = py("with open('shared.txt') as f:\n    f.write('x')");
        assert_eq!(targets(&a), ["/repo/shared.txt"]);
    }

    #[test]
    fn a_call_into_an_unmodeled_module_is_unresolved() {
        assert!(unresolved(&py(
            "import pandas as pd\npd.DataFrame().to_csv('o.csv')"
        )));
        assert!(unresolved(&py("undefined_helper('a.txt')")));
    }

    #[test]
    fn a_read_only_script_is_silent() {
        let a = py(
            "import json, sys\nfrom pathlib import Path\ndata = json.loads(Path('a.json').read_text())\nprint(len(data), sorted(data))\nfor k in data:\n    print(k.upper())",
        );
        assert!(a.file_effects.is_empty(), "{:?}", a.file_effects);
        assert!(a.uncertainties.is_empty(), "{:?}", a.uncertainties);
    }

    #[test]
    fn recursive_removal_is_a_tree_effect() {
        let a = py("import shutil\nshutil.rmtree('build')");
        assert_eq!(a.tree_effects[0].scope, "/repo/build");
    }

    #[test]
    fn a_loop_over_a_literal_list_runs_per_element() {
        assert_eq!(
            targets(&py(
                "from pathlib import Path\nfor n in ['a.txt', 'b.txt']:\n    Path(n).write_text('')"
            )),
            ["/repo/a.txt", "/repo/b.txt"]
        );
    }

    #[test]
    fn exec_of_a_constant_is_python_source_and_chdir_moves_the_cwd() {
        assert_eq!(targets(&py("exec(\"open('e.txt', 'w')\")")), [
            "/repo/e.txt"
        ]);
        assert_eq!(
            targets(&py("import os\nos.chdir('sub')\nopen('a.txt', 'w')")),
            ["/repo/sub/a.txt"]
        );
    }
}
