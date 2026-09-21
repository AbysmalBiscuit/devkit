//! JavaScript and TypeScript run by Node, Bun, Deno, tsx and ts-node.

use std::{collections::HashMap, ops::Range};

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    model::{FileOp, Language, Location, TempLocation, UncertaintyKind, Value},
    normalize, paths, ts,
};

const KNOWN_MODULES: &[&str] = &[
    "fs",
    "fs/promises",
    "path",
    "child_process",
    "os",
    "util",
    "url",
    "crypto",
    "assert",
    "events",
    "process",
    "module",
    "readline",
    "stream",
    "buffer",
    "zlib",
    "querystring",
];
const WRITES: &[(&str, FileOp, usize)] = &[
    ("writeFileSync", FileOp::Overwrite, 0),
    ("writeFile", FileOp::Overwrite, 0),
    ("appendFileSync", FileOp::Append, 0),
    ("appendFile", FileOp::Append, 0),
    ("createWriteStream", FileOp::Overwrite, 0),
    ("unlinkSync", FileOp::Delete, 0),
    ("unlink", FileOp::Delete, 0),
    ("truncateSync", FileOp::Overwrite, 0),
    ("truncate", FileOp::Overwrite, 0),
    ("copyFileSync", FileOp::Copy, 1),
    ("copyFile", FileOp::Copy, 1),
    ("symlinkSync", FileOp::Create, 1),
    ("symlink", FileOp::Create, 1),
    ("linkSync", FileOp::Create, 1),
    ("link", FileOp::Create, 1),
];
const WRITE_METHOD_NAMES: &[&str] = &[
    "writeFileSync",
    "writeFile",
    "appendFileSync",
    "appendFile",
    "createWriteStream",
    "unlinkSync",
    "unlink",
    "renameSync",
    "rename",
    "rmSync",
    "rm",
    "copyFileSync",
    "copyFile",
    "cpSync",
    "cp",
    "write",
    "writeTextFile",
];

#[derive(Debug, Clone, PartialEq)]
enum Js {
    Str(String),
    /// A directory `fs.mkdtemp` created fresh under a random name, carrying
    /// where it was made. The name needs no claim of its own, but the directory
    /// it was made in can be held by someone.
    Ephemeral(TempLocation),
    Num(i64),
    Array(Vec<Js>),
    Api(String),
    Method(Box<Js>, String),
    Argv,
    Env,
    Foreign(String),
    Function(Range<usize>),
    FunctionExpression,
    Data,
    Unknown,
}
impl Js {
    fn as_value(&self) -> Value {
        match self {
            Self::Str(s) => Value::Known(s.clone()),
            _ => match self.ephemeral() {
                Some(at) => Value::Ephemeral(at),
                None => Value::Unknown,
            },
        }
    }

    /// Where a freshly created temp path was made, when this value is one.
    fn ephemeral(&self) -> Option<TempLocation> {
        match self {
            Self::Ephemeral(at) => Some(at.clone()),
            _ => None,
        }
    }

    /// Whether this value is, or was derived from, a freshly created temp path.
    fn is_ephemeral(&self) -> bool {
        self.ephemeral().is_some()
    }
}
#[derive(Debug, Clone, Default)]
struct Scope {
    names: HashMap<String, Js>,
    argv: Vec<Value>,
    cwd: Option<String>,
}

pub(crate) fn walk(a: &mut Analyzer<'_>, language: Language, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(language, source) else {
        a.uncertain(
            UncertaintyKind::ParseError,
            "script source did not parse",
            frame.locate(0..source.len()),
        );
        return;
    };
    let mut scope = Scope {
        argv: frame.script_args.clone(),
        cwd: frame.cwd.clone(),
        ..Scope::default()
    };
    Walker {
        a,
        source,
        frame,
        root: tree.root_node(),
    }
    .statements(tree.root_node(), &mut scope);
}
struct Walker<'a, 'c, 's, 't> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
    root: Node<'t>,
}
impl<'t> Walker<'_, '_, '_, 't> {
    fn at(&self, node: Node<'_>) -> Location {
        self.frame.locate(node.byte_range())
    }

    fn unresolved(&mut self, node: Node<'_>, detail: impl Into<String>) {
        self.a
            .uncertain(UncertaintyKind::UnresolvedWrite, detail, self.at(node));
    }

    fn value_limit(&mut self, node: Node<'_>) {
        self.a.uncertain(
            UncertaintyKind::LimitExhausted(crate::model::Limit::ValueSize),
            "a JavaScript value exceeded the configured size limit",
            self.at(node),
        );
    }

    /// A link created under a fresh directory is where the exemption ends.
    /// Writes through the link land wherever it points, and the name it was
    /// given is no evidence of that: a hard link is a second name for a file
    /// that already exists somewhere, and a symbolic link's target is read
    /// through whatever links precede it, so a target that stays inside the
    /// directory by spelling can still lead out of it.
    fn link_in_fresh(&mut self, node: Node<'t>, link: &Js) {
        if link.is_ephemeral() {
            self.unresolved(
                node,
                "a link made in a fresh directory can point outside it",
            );
        }
    }

    fn js_effect(&mut self, op: FileOp, target: &Js, cwd: Option<&str>, at: Location) {
        self.a.file_effect(op, &target.as_value(), cwd, at);
    }

    fn js_tree(&mut self, scope: &Js, whole: bool, cwd: Option<&str>, by: &str, at: Location) {
        self.a.tree_effect(&scope.as_value(), whole, cwd, by, at);
    }

    /// A join whose first argument is a freshly created temp path. It keeps the
    /// exemption only while every later argument is known and lands inside that
    /// directory: an unknown one cannot show containment, and a temp path
    /// anywhere but first is not the thing being extended.
    fn temp_join(&self, args: &[(Js, Node<'t>)]) -> Js {
        let Some(at) = args.first().and_then(|(v, _)| v.ephemeral()) else {
            return Js::Unknown;
        };
        let Some(rest) = args[1..]
            .iter()
            .map(|(v, _)| match v {
                Js::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .collect::<Option<Vec<&str>>>()
        else {
            return Js::Unknown;
        };
        if paths::stays_within(&rest, self.a.ctx.path_style) {
            Js::Ephemeral(at)
        } else {
            Js::Unknown
        }
    }

    fn known(&mut self, node: Node<'_>, value: String) -> Js {
        if self.a.budget.value_fits(value.len()) {
            Js::Str(value)
        } else {
            self.value_limit(node);
            Js::Unknown
        }
    }

    fn statements(&mut self, node: Node<'t>, scope: &mut Scope) {
        for child in ts::named_children(node) {
            if child.kind() == "function_declaration"
                && let Some(name) = child.child_by_field_name("name")
            {
                scope.names.insert(
                    ts::text(name, self.source).to_string(),
                    Js::Function(child.byte_range()),
                );
            }
        }
        for child in ts::named_children(node) {
            self.statement(child, scope);
        }
    }

    fn statement(&mut self, node: Node<'t>, scope: &mut Scope) {
        if self.a.budget.visit().is_err() {
            return;
        }
        if node.has_error() && node.kind() != "program" && node.kind() != "statement_block" {
            self.a.uncertain(
                UncertaintyKind::ParseError,
                "a script statement could not be parsed",
                self.at(node),
            );
            return;
        }
        match node.kind() {
            "comment" | "empty_statement" | "type_alias_declaration" | "interface_declaration" => {}
            "import_statement" => self.import(node, scope),
            "lexical_declaration" | "variable_declaration" => {
                for d in ts::named_children(node)
                    .into_iter()
                    .filter(|d| d.kind() == "variable_declarator")
                {
                    let value = d
                        .child_by_field_name("value")
                        .map_or(Js::Unknown, |v| self.eval(v, scope));
                    if let Some(name) = d.child_by_field_name("name") {
                        self.bind(name, value, scope);
                    }
                }
            }
            "statement_block" => {
                let mut inner = scope.clone();
                self.statements(node, &mut inner);
            }
            "function_declaration" => {}
            "class_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    scope
                        .names
                        .insert(ts::text(name, self.source).to_string(), Js::Data);
                }
            }
            "if_statement" | "for_statement" | "for_in_statement" | "while_statement"
            | "do_statement" | "try_statement" | "switch_statement" => {
                let mut branch = scope.clone();
                for child in ts::named_children(node) {
                    if child.kind().ends_with("statement")
                        || child.kind() == "statement_block"
                        || child.kind().ends_with("clause")
                        || child.kind() == "switch_body"
                    {
                        self.statement(child, &mut branch);
                    } else {
                        self.eval(child, &mut branch);
                    }
                }
                for (name, value) in &branch.names {
                    if scope.names.get(name) != Some(value) {
                        scope.names.insert(name.clone(), Js::Unknown);
                    }
                }
            }
            _ => {
                for child in ts::named_children(node) {
                    self.eval(child, scope);
                }
            }
        }
    }

    fn module(specifier: &str) -> Js {
        let s = specifier.strip_prefix("node:").unwrap_or(specifier);
        if KNOWN_MODULES.contains(&s) {
            Js::Api(s.to_string())
        } else {
            Js::Foreign(s.to_string())
        }
    }

    fn import(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(source) = node.child_by_field_name("source") else {
            return;
        };
        let module = match self.eval(source, scope) {
            Js::Str(s) => Self::module(&s),
            _ => Js::Unknown,
        };
        for clause in ts::named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "import_clause")
        {
            for part in ts::named_children(clause) {
                match part.kind() {
                    "identifier" => {
                        scope
                            .names
                            .insert(ts::text(part, self.source).to_string(), module.clone());
                    }
                    "namespace_import" => {
                        if let Some(id) = ts::named_children(part)
                            .into_iter()
                            .find(|n| n.kind() == "identifier")
                        {
                            scope
                                .names
                                .insert(ts::text(id, self.source).to_string(), module.clone());
                        }
                    }
                    "named_imports" => {
                        for spec in ts::named_children(part)
                            .into_iter()
                            .filter(|s| s.kind() == "import_specifier")
                        {
                            let name = spec
                                .child_by_field_name("name")
                                .map(|n| ts::text(n, self.source).to_string());
                            let alias = spec
                                .child_by_field_name("alias")
                                .map(|n| ts::text(n, self.source).to_string());
                            if let Some(name) = name {
                                scope.names.insert(
                                    alias.unwrap_or_else(|| name.clone()),
                                    member(&module, &name),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    fn bind(&mut self, pattern: Node<'t>, value: Js, scope: &mut Scope) {
        match pattern.kind() {
            "identifier" => {
                scope
                    .names
                    .insert(ts::text(pattern, self.source).to_string(), value);
            }
            "object_pattern" => {
                for prop in ts::named_children(pattern) {
                    match prop.kind() {
                        "shorthand_property_identifier_pattern" => {
                            let name = ts::text(prop, self.source).to_string();
                            scope.names.insert(name.clone(), member(&value, &name));
                        }
                        "pair_pattern" => {
                            if let (Some(key), Some(target)) = (
                                prop.child_by_field_name("key"),
                                prop.child_by_field_name("value"),
                            ) {
                                self.bind(
                                    target,
                                    member(
                                        &value,
                                        ts::text(key, self.source).trim_matches(['\'', '"']),
                                    ),
                                    scope,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            "array_pattern" => {
                for (i, part) in ts::named_children(pattern).into_iter().enumerate() {
                    self.bind(
                        part,
                        match &value {
                            Js::Array(items) => items.get(i).cloned().unwrap_or(Js::Unknown),
                            _ => Js::Unknown,
                        },
                        scope,
                    );
                }
            }
            _ => {}
        }
    }

    fn eval(&mut self, node: Node<'t>, scope: &mut Scope) -> Js {
        if self.a.budget.visit().is_err() {
            return Js::Unknown;
        }
        let text = ts::text(node, self.source);
        match node.kind() {
            "string" => self.known(node, text[1..text.len().saturating_sub(1)].to_string()),
            "template_string" => {
                let mut out = String::new();
                let mut last = node.start_byte() + 1;
                for sub in ts::named_children(node)
                    .into_iter()
                    .filter(|n| n.kind() == "template_substitution")
                {
                    out.push_str(&self.source[last..sub.start_byte()]);
                    last = sub.end_byte();
                    match ts::named_children(sub)
                        .first()
                        .map(|e| self.eval(*e, scope))
                    {
                        Some(Js::Str(s)) => out.push_str(&s),
                        Some(Js::Num(n)) => out.push_str(&n.to_string()),
                        _ => return Js::Unknown,
                    }
                }
                out.push_str(&self.source[last..node.end_byte() - 1]);
                self.known(node, out)
            }
            "number" => text.parse().map_or(Js::Data, Js::Num),
            "true" | "false" | "null" | "undefined" | "regex" => Js::Data,
            "arrow_function" | "function_expression" | "function" => Js::FunctionExpression,
            "identifier" => scope
                .names
                .get(text)
                .cloned()
                .unwrap_or_else(|| match text {
                    "require" => Js::Api("require".into()),
                    "process" => Js::Api("process".into()),
                    "Bun" => Js::Api("Bun".into()),
                    "Deno" => Js::Api("Deno".into()),
                    "eval" | "Function" => Js::Api(text.into()),
                    "console" | "JSON" | "Math" | "Object" | "Array" | "String" | "Number"
                    | "Promise" | "Date" | "Map" | "Set" | "Error" => Js::Data,
                    _ => Js::Unknown,
                }),
            "member_expression" => {
                if let Some(value) = scope.names.get(text) {
                    if value == &Js::Unknown {
                        let property = node
                            .child_by_field_name("property")
                            .map_or("", |p| ts::text(p, self.source));
                        return if text == "process.argv" {
                            Js::Unknown
                        } else {
                            Js::Method(Box::new(Js::Unknown), property.to_string())
                        };
                    }
                    return value.clone();
                }
                let object = node
                    .child_by_field_name("object")
                    .map_or(Js::Unknown, |o| self.eval(o, scope));
                let property = node
                    .child_by_field_name("property")
                    .map_or("", |p| ts::text(p, self.source));
                match (&object, property) {
                    (Js::Api(p), "argv") if p == "process" => Js::Argv,
                    (Js::Api(p), "env") if p == "process" => Js::Env,
                    (Js::Api(p), "promises") if p == "fs" => Js::Api("fs/promises".into()),
                    _ => member(&object, property),
                }
            }
            "subscript_expression" => match (
                node.child_by_field_name("object")
                    .map_or(Js::Unknown, |o| self.eval(o, scope)),
                node.child_by_field_name("index")
                    .map_or(Js::Unknown, |i| self.eval(i, scope)),
            ) {
                (Js::Argv, Js::Num(i)) => usize::try_from(i)
                    .ok()
                    .and_then(|i| scope.argv.get(i))
                    .map_or(Js::Unknown, |v| match v {
                        Value::Known(s) if self.a.budget.value_fits(s.len()) => Js::Str(s.clone()),
                        Value::Known(_) => {
                            self.value_limit(node);
                            Js::Unknown
                        }
                        Value::Unknown => Js::Unknown,
                        Value::Ephemeral(at) => Js::Ephemeral(at.clone()),
                    }),
                (Js::Array(items), Js::Num(i)) => usize::try_from(i)
                    .ok()
                    .and_then(|i| items.get(i).cloned())
                    .unwrap_or(Js::Unknown),
                _ => Js::Unknown,
            },
            "array" => Js::Array(
                ts::named_children(node)
                    .into_iter()
                    .map(|c| self.eval(c, scope))
                    .collect(),
            ),
            "binary_expression" => {
                let left = node
                    .child_by_field_name("left")
                    .map_or(Js::Unknown, |l| self.eval(l, scope));
                let right = node
                    .child_by_field_name("right")
                    .map_or(Js::Unknown, |r| self.eval(r, scope));
                let operator = node
                    .child_by_field_name("operator")
                    .map_or("", |o| ts::text(o, self.source));
                match (operator, left, right) {
                    ("+", Js::Str(left), Js::Str(right)) => self.known(node, left + &right),
                    _ => Js::Data,
                }
            }
            "call_expression" => self.call(node, scope),
            "await_expression"
            | "parenthesized_expression"
            | "as_expression"
            | "satisfies_expression"
            | "non_null_expression"
            | "type_assertion" => ts::named_children(node)
                .into_iter()
                .map(|c| self.eval(c, scope))
                .find(|v| *v != Js::Data)
                .unwrap_or(Js::Data),
            "assignment_expression" => {
                let value = node
                    .child_by_field_name("right")
                    .map_or(Js::Unknown, |r| self.eval(r, scope));
                if let Some(left) = node.child_by_field_name("left") {
                    if left.kind() == "member_expression" {
                        scope
                            .names
                            .insert(ts::text(left, self.source).to_string(), Js::Unknown);
                    } else {
                        self.bind(left, value.clone(), scope);
                    }
                }
                value
            }
            "import" => Js::Api("import".into()),
            _ => {
                for child in ts::named_children(node) {
                    self.eval(child, scope);
                }
                Js::Unknown
            }
        }
    }

    fn call(&mut self, node: Node<'t>, scope: &mut Scope) -> Js {
        let callee = node
            .child_by_field_name("function")
            .map_or(Js::Unknown, |f| self.eval(f, scope));
        let args: Vec<(Js, Node<'t>)> = node
            .child_by_field_name("arguments")
            .map(|list| {
                ts::named_children(list)
                    .into_iter()
                    .map(|arg| (self.eval(arg, scope), arg))
                    .collect()
            })
            .unwrap_or_default();
        let arg = |i: usize| args.get(i).map_or(Js::Unknown, |(value, _)| value.clone());
        let cwd = scope.cwd.clone();
        let at = self.at(node);
        let api = match &callee {
            Js::Api(name) => Some(name.as_str()),
            _ => None,
        };
        match (api, &callee) {
            (Some("require"), _) => match arg(0) {
                Js::Str(s) => Self::module(&s),
                _ => {
                    self.unresolved(node, "a `require` of a module that could not be determined");
                    Js::Unknown
                }
            },
            (Some("require.resolve"), _) => Js::Data,
            (Some("import"), _) => match arg(0) {
                Js::Str(s) => Self::module(&s),
                _ => {
                    self.unresolved(node, "a dynamic `import()` that could not be determined");
                    Js::Unknown
                }
            },
            (Some("eval" | "Function"), _) => {
                self.unresolved(node, "dynamically evaluated script source");
                Js::Unknown
            }
            // The argument is a path prefix, not a directory: the fresh name
            // is appended to it, so the directory is its parent.
            (Some("fs.mkdtemp" | "fs.mkdtempSync"), _) => match arg(0) {
                Js::Str(prefix) => {
                    match paths::parent_dir(&prefix, self.a.ctx.path_style).map(|parent| {
                        paths::resolve(&Value::Known(parent), cwd.as_deref(), self.a.ctx.path_style)
                    }) {
                        Some(crate::Target::Path(dir)) => Js::Ephemeral(TempLocation::In(dir)),
                        _ => Js::Unknown,
                    }
                }
                _ => Js::Unknown,
            },
            (Some("path.join" | "path.posix.join"), _)
                if args.iter().any(|(v, _)| v.is_ephemeral()) =>
            {
                self.temp_join(&args)
            }
            (Some("path.join" | "path.posix.join"), _) => args
                .iter()
                .map(|(v, _)| match v {
                    Js::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .filter(|p| !p.is_empty())
                .map_or(Js::Unknown, |parts| {
                    self.known(
                        node,
                        parts.iter().skip(1).fold(parts[0].clone(), |base, part| {
                            paths::join(&base, part, self.a.ctx.path_style)
                        }),
                    )
                }),
            (Some("path.resolve"), _) => match (
                args.iter()
                    .map(|(v, _)| match v {
                        Js::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>(),
                cwd,
            ) {
                (Some(parts), Some(dir)) => self.known(
                    node,
                    parts.iter().fold(dir, |base, part| {
                        if part.starts_with('/') {
                            part.clone()
                        } else {
                            paths::join(&base, part, self.a.ctx.path_style)
                        }
                    }),
                ),
                _ => Js::Unknown,
            },
            (Some("path.dirname"), _) => match arg(0) {
                Js::Str(s) => Js::Str(paths::parent(&s).unwrap_or(".").to_string()),
                _ => Js::Unknown,
            },
            (Some("path.basename"), _) => match arg(0) {
                Js::Str(s) => Js::Str(normalize::basename(&s).to_string()),
                _ => Js::Unknown,
            },
            (Some("process.cwd"), _) => cwd.map_or(Js::Unknown, |path| self.known(node, path)),
            (Some("process.chdir"), _) => {
                scope.cwd = match arg(0) {
                    Js::Str(s) => match paths::resolve(
                        &Value::Known(s),
                        scope.cwd.as_deref(),
                        self.a.ctx.path_style,
                    ) {
                        crate::Target::Path(path) if self.a.budget.value_fits(path.len()) => {
                            Some(path)
                        }
                        crate::Target::Path(_) => {
                            self.value_limit(node);
                            None
                        }
                        crate::Target::Unresolved | crate::Target::Ephemeral { .. } => None,
                    },
                    _ => None,
                };
                Js::Data
            }
            (_, Js::Function(range)) => {
                if let Some(function) = self.root.descendant_for_byte_range(range.start, range.end)
                    && let Some(body) = function.child_by_field_name("body")
                {
                    let mut local = scope.clone();
                    self.statements(body, &mut local);
                }
                Js::Data
            }
            (_, Js::FunctionExpression) => {
                self.unresolved(
                    node,
                    "a function expression whose effects could not be analyzed",
                );
                Js::Unknown
            }
            (Some(name), _) if name.starts_with("fs.") || name.starts_with("fs/promises.") => {
                let method = name.rsplit('.').next().unwrap_or("");
                if let Some((_, op, index)) = WRITES
                    .iter()
                    .find(|(method_name, ..)| *method_name == method)
                {
                    if matches!(method, "symlink" | "symlinkSync" | "link" | "linkSync") {
                        self.link_in_fresh(node, &arg(*index));
                    }
                    self.js_effect(*op, &arg(*index), cwd.as_deref(), at);
                } else if matches!(method, "renameSync" | "rename") {
                    self.js_effect(FileOp::Rename, &arg(0), cwd.as_deref(), at.clone());
                    self.a.rename_into(&arg(1).as_value(), cwd.as_deref(), at);
                } else if matches!(method, "rmSync" | "rm" | "cpSync" | "cp") {
                    let recursive = args
                        .get(if method.starts_with("cp") { 2 } else { 1 })
                        .is_some_and(|(_, arg)| {
                            ts::text(*arg, self.source).contains("recursive: true")
                        });
                    let target = if method.starts_with("cp") {
                        arg(1)
                    } else {
                        arg(0)
                    };
                    let by = format!("fs.{method}");
                    if recursive && method.starts_with("cp") {
                        self.js_tree(&target, false, cwd.as_deref(), &by, at);
                    } else if recursive {
                        self.a
                            .tree_removal(&target.as_value(), cwd.as_deref(), &by, at);
                    } else {
                        self.js_effect(
                            if method.starts_with("cp") {
                                FileOp::Copy
                            } else {
                                FileOp::Delete
                            },
                            &target,
                            cwd.as_deref(),
                            at,
                        );
                    }
                } else if matches!(method, "openSync" | "open") {
                    match arg(1) {
                        Js::Str(flags) if flags.contains(['w', 'a', '+']) => {
                            self.js_effect(FileOp::Overwrite, &arg(0), cwd.as_deref(), at)
                        }
                        Js::Str(_) => {}
                        _ if args.len() < 2 => {}
                        _ => self.unresolved(
                            node,
                            "a file opened with flags that could not be determined",
                        ),
                    }
                }
                Js::Data
            }
            (Some("Bun.write"), _) => {
                self.js_effect(FileOp::Overwrite, &arg(0), cwd.as_deref(), at);
                Js::Data
            }
            (Some(name), _) if name.starts_with("Deno.") => {
                match &name[5..] {
                    "writeTextFile" | "writeFile" => {
                        self.js_effect(FileOp::Overwrite, &arg(0), cwd.as_deref(), at)
                    }
                    "remove" => self.js_effect(FileOp::Delete, &arg(0), cwd.as_deref(), at),
                    "rename" => {
                        self.js_effect(FileOp::Rename, &arg(0), cwd.as_deref(), at.clone());
                        self.a.rename_into(&arg(1).as_value(), cwd.as_deref(), at);
                    }
                    "copyFile" => self.js_effect(FileOp::Copy, &arg(1), cwd.as_deref(), at),
                    _ => {}
                }
                Js::Data
            }
            (Some("child_process.execSync" | "child_process.exec"), _) => {
                match arg(0) {
                    Js::Str(command) => self.a.source(Language::Bash, &command, Frame {
                        depth: self.frame.depth + 1,
                        script_args: Vec::new(),
                        base: Some(at),
                        cwd: scope.cwd.clone(),
                    }),
                    _ => self.unresolved(node, "a shell command that could not be determined"),
                }
                Js::Data
            }
            (
                Some(
                    "child_process.spawnSync"
                    | "child_process.spawn"
                    | "child_process.execFileSync"
                    | "child_process.execFile",
                ),
                _,
            ) => {
                let mut argv = vec![arg(0).as_value()];
                match arg(1) {
                    Js::Array(items) => argv.extend(items.iter().map(Js::as_value)),
                    Js::Unknown if args.len() > 1 => argv.push(Value::Unknown),
                    _ => {}
                }
                self.run_argv(node, argv, scope.cwd.clone());
                Js::Data
            }
            (Some("Bun.spawn" | "Bun.spawnSync"), _) => {
                match arg(0) {
                    Js::Array(items) => self.run_argv(
                        node,
                        items.iter().map(Js::as_value).collect(),
                        scope.cwd.clone(),
                    ),
                    _ => {
                        self.unresolved(node, "a `Bun.spawn` command that could not be determined")
                    }
                }
                Js::Data
            }
            (_, Js::Foreign(module)) => {
                self.unresolved(
                    node,
                    format!("a call into `{module}`, which devkit does not model"),
                );
                Js::Unknown
            }
            (_, Js::Method(receiver, method))
                if matches!(**receiver, Js::Unknown)
                    && WRITE_METHOD_NAMES.contains(&method.as_str()) =>
            {
                self.unresolved(
                    node,
                    format!("`.{method}()` on a value whose type could not be determined"),
                );
                Js::Unknown
            }
            (_, Js::Method(receiver, _)) if matches!(**receiver, Js::Argv) => {
                self.unresolved(node, "process.argv was mutated");
                Js::Unknown
            }
            _ => Js::Unknown,
        }
    }

    fn run_argv(&mut self, node: Node<'t>, argv: Vec<Value>, cwd: Option<String>) {
        let words = argv
            .into_iter()
            .map(|value| Word {
                typed: value.known().unwrap_or("?").to_string(),
                value,
                span: node.byte_range(),
            })
            .collect();
        self.a.invocation(
            RawInvocation {
                words,
                stdin: Stdin::None,
                cwd,
                language: Language::JavaScript,
                location: self.at(node),
            },
            self.frame,
        );
    }
}
fn member(object: &Js, property: &str) -> Js {
    match object {
        Js::Api(base) => Js::Api(format!("{base}.{property}")),
        Js::Foreign(module) => Js::Foreign(module.clone()),
        Js::Env => Js::Unknown,
        Js::Data => Js::Data,
        other => Js::Method(Box::new(other.clone()), property.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Context, Dialect, Limits, PathStyle,
        model::{Limit, Target, UncertaintyKind},
        testutil::{bash, targets},
    };

    #[test]
    fn an_mkdtemp_directory_write_is_ephemeral() {
        let a = bash(
            "node -e \"const fs = require('fs'); const path = require('path'); const d = fs.mkdtempSync('pre'); fs.writeFileSync(path.join(d, 'out.txt'), 'x')\"",
        );
        assert_eq!(targets(&a), ["<ephemeral>"]);
        assert!(a.uncertainties.is_empty(), "{:?}", a.uncertainties);
    }

    fn unresolved(a: &crate::Analysis) -> bool {
        a.uncertainties
            .iter()
            .any(|u| u.kind == UncertaintyKind::UnresolvedWrite)
            || targets(a).contains(&"?".to_string())
    }

    #[test]
    fn node_fs_writes_through_require_and_imports() {
        assert_eq!(
            targets(&bash(
                "node -e \"require('fs').writeFileSync('a.txt', 'x')\""
            )),
            ["/repo/a.txt"]
        );
        assert_eq!(
            targets(&bash(
                "node -e \"const { appendFileSync: add } = require('node:fs'); add('b.txt', 'x')\""
            )),
            ["/repo/b.txt"]
        );
        assert_eq!(
            targets(&bash(
                "bun -e \"import { writeFile } from 'node:fs/promises'; await writeFile('c.txt', 'x')\""
            )),
            ["/repo/c.txt"]
        );
    }

    #[test]
    fn path_join_templates_and_argv_resolve() {
        assert_eq!(
            targets(&bash(
                "node -e 'const fs = require(\"fs\"); const path = require(\"path\"); const d = \"gen\"; fs.writeFileSync(path.join(d, `${\"x\"}.txt`), \"\")'"
            )),
            ["/repo/gen/x.txt"]
        );
        assert_eq!(
            targets(&bash(
                "f=out.txt; node -e \"require('fs').writeFileSync(process.argv[1], '')\" \"$f\""
            )),
            ["/repo/out.txt"]
        );
    }

    #[test]
    fn bun_and_deno_writers() {
        assert_eq!(
            targets(&bash("bun -e \"await Bun.write('d.txt', 'x')\"")),
            ["/repo/d.txt"]
        );
        assert_eq!(
            targets(&bash(
                "deno eval \"await Deno.writeTextFile('e.txt', 'x')\""
            )),
            ["/repo/e.txt"]
        );
    }

    #[test]
    fn shadowing_drops_the_api_binding() {
        let a = bash(
            "node -e \"const fs = require('fs'); { const fs = { writeFileSync() {} }; fs.writeFileSync('a.txt') }\"",
        );
        assert!(a.file_effects.is_empty(), "{:?}", a.file_effects);
    }

    #[test]
    fn a_method_name_alone_is_not_a_known_write() {
        let a = bash("node -e \"thing.writeFile('a.txt')\"");
        assert!(
            a.file_effects
                .iter()
                .all(|e| e.target == crate::Target::Unresolved)
        );
        assert!(unresolved(&a));
    }

    #[test]
    fn resolution_probes_and_reads_are_silent() {
        let a = bash(
            "node -e \"console.log(require.resolve('vite')); const fs = require('fs'); fs.readFileSync('a.json', 'utf8'); fs.existsSync('b')\"",
        );
        assert!(
            a.file_effects.is_empty() && a.uncertainties.is_empty(),
            "{a:?}"
        );
    }

    #[test]
    fn child_process_commands_are_analyzed_when_constant() {
        assert_eq!(
            targets(&bash(
                "node -e \"require('child_process').execSync('rm a.txt')\""
            )),
            ["/repo/a.txt"]
        );
        assert_eq!(
            targets(&bash(
                "node -e \"require('child_process').spawnSync('rm', ['b.txt'])\""
            )),
            ["/repo/b.txt"]
        );
        assert!(unresolved(&bash(
            "node -e \"require('child_process').execSync(process.env.CMD)\""
        )));
    }

    #[test]
    fn typescript_syntax_parses_as_syntax() {
        assert_eq!(
            targets(&bash(
                "bun -e \"import fs from 'node:fs'; const p: string = 'f.txt'; fs.writeFileSync(p as string, '')\""
            )),
            ["/repo/f.txt"]
        );
    }

    #[test]
    fn dynamic_code_is_unresolved() {
        assert!(unresolved(&bash("node -e \"eval(process.env.X)\"")));
        assert!(unresolved(&bash("node -e \"import(process.env.M)\"")));
    }

    #[test]
    fn reassigned_process_argv_does_not_use_outer_arguments() {
        let a = bash(
            "node -e \"process.argv = ['node', 'other.txt']; require('fs').writeFileSync(process.argv[1], '')\" original.txt",
        );
        assert!(!targets(&a).contains(&"/repo/original.txt".to_string()));
        assert!(unresolved(&a));
    }

    #[test]
    fn reassigned_fs_member_is_not_a_known_write() {
        let a = bash(
            "node -e \"const fs = require('fs'); fs.writeFileSync = () => {}; fs.writeFileSync('a.txt')\"",
        );
        assert!(!targets(&a).contains(&"/repo/a.txt".to_string()));
        assert!(unresolved(&a));
    }

    #[test]
    fn oversized_js_values_are_unresolved() {
        let context = Context {
            dialect: Dialect::Bash,
            cwd: Some("/repo".into()),
            path_style: PathStyle::Unix,
            limits: Limits {
                value: 128,
                ..Limits::default()
            },
        };
        let a = crate::analyze(
            "node -e \"x='1234567890';require('fs').writeFileSync(x+x+x+x+x+x+x+x+x+x+x+x+x+x+x+x, '')\"",
            &context,
        );
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::LimitExhausted(Limit::ValueSize)),
            "{a:?}"
        );
        assert!(a
            .file_effects
            .iter()
            .all(|effect| effect.target != Target::Path("/repo/123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890123456789012345678901234567890".into())));
    }

    #[test]
    fn directly_called_local_function_is_analyzed() {
        let a = bash(
            "node -e \"function write() { require('fs').writeFileSync('a.txt', '') } write()\"",
        );
        assert_eq!(targets(&a), ["/repo/a.txt"]);
    }

    #[test]
    fn oversized_process_cwd_is_unresolved() {
        let cwd = format!("/repo/{}", "a".repeat(121));
        let context = Context {
            dialect: Dialect::Bash,
            cwd: Some(cwd.clone()),
            path_style: PathStyle::Unix,
            limits: Limits {
                value: 128,
                ..Limits::default()
            },
        };
        let a = crate::analyze(
            "node -e \"process.chdir('x'); require('fs').writeFileSync('out.txt', '')\"",
            &context,
        );
        assert!(
            a.uncertainties
                .iter()
                .any(|u| u.kind == UncertaintyKind::LimitExhausted(Limit::ValueSize)),
            "{a:?}"
        );
        assert!(
            a.file_effects
                .iter()
                .all(|effect| { effect.target != Target::Path(format!("{cwd}/x/out.txt")) })
        );
    }

    #[test]
    fn local_function_calls_are_hoisted() {
        let a = bash(
            "node -e \"write(); function write() { require('fs').writeFileSync('before.txt', '') }\"",
        );
        assert_eq!(targets(&a), ["/repo/before.txt"]);
    }

    #[test]
    fn called_function_expression_is_uncertain() {
        let a = bash(
            "node -e \"const write = () => require('fs').writeFileSync('expression.txt', ''); write()\"",
        );
        assert!(!targets(&a).contains(&"/repo/expression.txt".to_string()));
        assert!(unresolved(&a));
    }
}
