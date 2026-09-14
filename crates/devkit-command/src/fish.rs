//! fish: commands, redirects, `set`, `cd`, pipes, conditionals, loops, and
//! command substitution.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::{
    analyzer::{Analyzer, Frame, RawInvocation, Stdin, Word},
    model::{FileOp, Language, Limit, UncertaintyKind, Value},
    paths, ts,
};

const MAX_LOOP_VALUES: usize = 32;

#[derive(Debug, Clone, Default, PartialEq)]
struct Scope {
    vars: HashMap<String, Value>,
    cwd: Option<String>,
}

impl Scope {
    fn merge_uncertain(&mut self, branch: &Scope) {
        let names: Vec<String> = self
            .vars
            .keys()
            .chain(branch.vars.keys())
            .cloned()
            .collect();
        for name in names {
            if self.vars.get(&name) != branch.vars.get(&name) {
                self.vars.insert(name, Value::Unknown);
            }
        }
        if branch.cwd != self.cwd {
            self.cwd = None;
        }
    }
}

pub(crate) fn walk(a: &mut Analyzer<'_>, source: &str, frame: &Frame) {
    let Some(tree) = ts::parse(Language::Fish, source) else {
        a.uncertain(
            UncertaintyKind::ParseError,
            "fish source did not parse",
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
        exhausted: false,
    };
    w.statements(tree.root_node(), &mut scope);
}

struct Walker<'a, 'c, 's> {
    a: &'a mut Analyzer<'c>,
    source: &'s str,
    frame: &'a Frame,
    exhausted: bool,
}

impl<'t> Walker<'_, '_, '_> {
    fn visit(&mut self, node: Node<'t>) -> bool {
        if self.exhausted {
            return false;
        }
        if self.a.budget.visit().is_err() {
            self.exhausted = true;
            self.a.uncertain(
                UncertaintyKind::LimitExhausted(Limit::Nodes),
                "fish source was only partly analyzed",
                self.frame.locate(node.byte_range()),
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
        if node.has_error() && node.kind() != "ERROR" {
            let at = self.frame.locate(node.byte_range());
            self.a.uncertain(
                UncertaintyKind::ParseError,
                "a fish statement could not be parsed",
                at,
            );
            return;
        }
        match node.kind() {
            "comment" | "break" | "continue" | "return" => {}
            "ERROR" => self.statements(node, scope),
            "command" => self.command(node, scope, stdin),
            "redirect_statement" => {
                for child in ts::named_children(node) {
                    match child.kind() {
                        "file_redirect" => self.redirect(child, scope),
                        "stream_redirect" => {}
                        _ => self.statement(child, scope, stdin.clone()),
                    }
                }
            }
            "pipe" => {
                let mut input = stdin;
                for element in ts::named_children(node) {
                    let mut inner = scope.clone();
                    self.statement(element, &mut inner, input);
                    input = Stdin::Source {
                        value: Value::Unknown,
                        span: element.byte_range(),
                    };
                }
            }
            "conditional_execution" | "negated_statement" => {
                let mut branch = scope.clone();
                self.statements(node, &mut branch);
                scope.merge_uncertain(&branch);
            }
            "if_statement" | "while_statement" | "switch_statement" | "begin_statement"
            | "else_clause" | "else_if_clause" | "case_clause" => {
                let mut branch = scope.clone();
                self.statements(node, &mut branch);
                scope.merge_uncertain(&branch);
            }
            "for_statement" => self.for_statement(node, scope),
            "function_definition" => {}
            _ => self.statements(node, scope),
        }
    }

    fn redirect(&mut self, node: Node<'t>, scope: &Scope) {
        let Some(operator) = node.child_by_field_name("operator") else {
            return;
        };
        let operator = ts::text(operator, self.source);
        let op = if operator.contains(">>") {
            FileOp::Append
        } else if operator.contains('>') {
            FileOp::Overwrite
        } else {
            return;
        };
        let Some(destination) = node.child_by_field_name("destination") else {
            return;
        };
        let mut destination_scope = scope.clone();
        let value = self.word(destination, &mut destination_scope).value;
        if matches!(
            value.known(),
            Some("/dev/null" | "/dev/stderr" | "/dev/stdout" | "/dev/tty")
        ) {
            return;
        }
        let at = self.frame.locate(node.byte_range());
        self.a.file_effect(op, &value, scope.cwd.as_deref(), at);
    }

    fn for_statement(&mut self, node: Node<'t>, scope: &mut Scope) {
        let Some(variable) = node.child_by_field_name("variable") else {
            return;
        };
        let variable = ts::text(variable, self.source).to_string();
        let mut cursor = node.walk();
        let values: Vec<(Node<'t>, Value)> = node
            .children_by_field_name("value", &mut cursor)
            .map(|value| {
                let resolved = self.value(value, &mut scope.clone());
                (value, resolved)
            })
            .collect();
        let value_ranges: Vec<_> = values.iter().map(|(node, _)| node.byte_range()).collect();
        let literal = values.len() <= MAX_LOOP_VALUES
            && values.iter().all(|(_, value)| value.known().is_some());
        let mut after = scope.clone();
        if literal {
            for (_, value) in values {
                let mut iteration = scope.clone();
                iteration.vars.insert(variable.clone(), value);
                self.for_body(node, &value_ranges, &mut iteration);
                after.merge_uncertain(&iteration);
            }
        } else {
            let mut iteration = scope.clone();
            iteration.vars.insert(variable.clone(), Value::Unknown);
            self.for_body(node, &value_ranges, &mut iteration);
            after.merge_uncertain(&iteration);
        }
        after.vars.insert(variable, Value::Unknown);
        *scope = after;
    }

    fn for_body(
        &mut self,
        node: Node<'t>,
        value_ranges: &[std::ops::Range<usize>],
        scope: &mut Scope,
    ) {
        for child in ts::named_children(node) {
            if child.kind() == "variable_name"
                || value_ranges
                    .iter()
                    .any(|range| *range == child.byte_range())
            {
                continue;
            }
            self.statement(child, scope, Stdin::None);
        }
    }

    fn command(&mut self, node: Node<'t>, scope: &mut Scope, stdin: Stdin) {
        let mut words = Vec::new();
        for child in ts::named_children(node) {
            match child.kind() {
                "file_redirect" => self.redirect(child, scope),
                "stream_redirect" => {}
                _ => words.push(self.word(child, scope)),
            }
        }
        let Some(first) = words.first() else {
            return;
        };
        match first.value.known() {
            Some("set") => {
                let rest: Vec<&Word> = words[1..]
                    .iter()
                    .filter(|word| {
                        word.value
                            .known()
                            .is_none_or(|value| !value.starts_with('-'))
                    })
                    .collect();
                let erase = words[1..]
                    .iter()
                    .any(|word| matches!(word.value.known(), Some("-e" | "--erase")));
                if erase {
                    for name in rest.iter().filter_map(|word| word.value.known()) {
                        scope.vars.remove(name);
                    }
                } else if let Some(name) = rest.first().and_then(|word| word.value.known()) {
                    let value = match rest.get(1..) {
                        Some([single]) => single.value.clone(),
                        _ => Value::Unknown,
                    };
                    scope.vars.insert(name.to_string(), value);
                }
                return;
            }
            Some("cd") => {
                scope.cwd = words.get(1).and_then(|word| {
                    if word.value.known() == Some("-") {
                        return None;
                    }
                    match paths::resolve(&word.value, scope.cwd.as_deref(), self.a.ctx.path_style) {
                        crate::model::Target::Path(directory) => Some(directory),
                        crate::model::Target::Unresolved | crate::model::Target::Ephemeral => None,
                    }
                });
                return;
            }
            _ => {}
        }
        if let Some(name) = first.value.known()
            && matches!(crate::normalize::basename(name), "rm" | "unlink" | "shred")
            && words[1..].iter().any(|word| word.value == Value::Unknown)
        {
            self.a.file_effect(
                FileOp::Delete,
                &Value::Unknown,
                scope.cwd.as_deref(),
                self.frame.locate(node.byte_range()),
            );
        }
        let raw = RawInvocation {
            words,
            stdin,
            cwd: scope.cwd.clone(),
            language: Language::Fish,
            location: self.frame.locate(node.byte_range()),
        };
        self.a.invocation(raw, self.frame);
    }

    fn word(&mut self, node: Node<'t>, scope: &mut Scope) -> Word {
        let typed = ts::text(node, self.source);
        let value = match self.value(node, scope) {
            Value::Known(value) if !self.a.budget.value_fits(value.len()) => Value::Unknown,
            value => value,
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
            "word" | "integer" | "float" => {
                if text.contains(['*', '?', '~', '{']) {
                    Value::Unknown
                } else {
                    Value::Known(text.replace('\\', ""))
                }
            }
            "escape_sequence" => Value::Known(text.strip_prefix('\\').unwrap_or(text).to_string()),
            "glob" | "home_dir_expansion" | "brace_expansion" => {
                self.substitutions(node, scope);
                Value::Unknown
            }
            "single_quote_string" => {
                Value::Known(text[1..text.len().saturating_sub(1)].replace("\\'", "'"))
            }
            "double_quote_string" => {
                let mut out = String::new();
                let mut last = node.start_byte() + 1;
                for part in ts::named_children(node) {
                    out.push_str(&self.source[last..part.start_byte()]);
                    last = part.end_byte();
                    match self.value(part, scope) {
                        Value::Known(value) => out.push_str(&value),
                        Value::Unknown => return Value::Unknown,
                    }
                }
                out.push_str(&self.source[last..node.end_byte().saturating_sub(1).max(last)]);
                Value::Known(out)
            }
            "variable_expansion" => {
                let name = text
                    .strip_prefix('$')
                    .unwrap_or(text)
                    .split('[')
                    .next()
                    .unwrap_or_default();
                scope.vars.get(name).cloned().unwrap_or(Value::Unknown)
            }
            "concatenation" => {
                let mut out = String::new();
                for part in ts::named_children(node) {
                    match self.value(part, scope) {
                        Value::Known(value) => out.push_str(&value),
                        Value::Unknown => return Value::Unknown,
                    }
                }
                Value::Known(out)
            }
            "command_substitution" => {
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

    fn substitutions(&mut self, node: Node<'t>, scope: &Scope) {
        for child in ts::named_children(node) {
            if child.kind() == "command_substitution" {
                let mut inner = scope.clone();
                self.statements(child, &mut inner);
            } else {
                self.substitutions(child, scope);
            }
        }
    }
}

#[cfg(test)]
mod shapes {
    use crate::{model::Language, ts};

    #[test]
    fn node_shapes_this_adapter_relies_on() {
        for (source, fragment) in [
            ("echo x > a.txt", "redirect"),
            ("set f a.txt", "command"),
            ("echo (pwd)", "command_substitution"),
            ("for f in a b; echo $f; end", "for_statement"),
            ("true; and echo x", "conditional_execution"),
            ("echo \"$f\"", "variable_expansion"),
        ] {
            let tree = ts::parse(Language::Fish, source).unwrap();
            let sexp = tree.root_node().to_sexp();
            assert!(sexp.contains(fragment), "{source:?}\n{sexp}");
        }
    }

    #[test]
    fn node_fields_this_adapter_relies_on() {
        let source = "echo x > a.txt";
        let tree = ts::parse(Language::Fish, source).unwrap();
        let command = ts::named_children(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "command")
            .expect("command");
        let redirect = command
            .child_by_field_name("redirect")
            .expect("redirect field");
        assert_eq!(redirect.kind(), "file_redirect");
        assert_eq!(
            ts::text(
                redirect
                    .child_by_field_name("operator")
                    .expect("operator field"),
                source,
            ),
            ">"
        );
        assert_eq!(
            ts::text(
                redirect
                    .child_by_field_name("destination")
                    .expect("destination field"),
                source,
            ),
            "a.txt"
        );

        let source = "for f in a b; echo $f; end";
        let tree = ts::parse(Language::Fish, source).unwrap();
        let for_statement = ts::named_children(tree.root_node())
            .into_iter()
            .find(|node| node.kind() == "for_statement")
            .expect("for_statement");
        assert_eq!(
            ts::text(
                for_statement
                    .child_by_field_name("variable")
                    .expect("variable field"),
                source,
            ),
            "f"
        );
        let mut cursor = for_statement.walk();
        let values: Vec<_> = for_statement
            .children_by_field_name("value", &mut cursor)
            .map(|node| ts::text(node, source))
            .collect();
        assert_eq!(values, ["a", "b"]);
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Analysis, Dialect,
        model::{Limit, Target, UncertaintyKind},
        testutil::{ctx, programs, targets},
    };

    fn fish(source: &str) -> Analysis {
        crate::analyze(source, &ctx(Dialect::Fish))
    }

    #[test]
    fn redirects_set_and_cd() {
        assert_eq!(targets(&fish("echo x > a.txt; echo y >> b.txt")), [
            "/repo/a.txt",
            "/repo/b.txt"
        ]);
        assert_eq!(targets(&fish("set f out.txt; echo x > $f")), [
            "/repo/out.txt"
        ]);
        assert_eq!(targets(&fish("cd sub; and echo x > a.txt")), [
            "/repo/sub/a.txt"
        ]);
    }

    #[test]
    fn node_budget_exhaustion_is_reported_once() {
        let mut context = ctx(Dialect::Fish);
        context.limits.nodes = 1;
        let a = crate::analyze("echo x > a.txt; echo y > b.txt", &context);
        assert_eq!(
            a.uncertainties
                .iter()
                .filter(|uncertainty| {
                    uncertainty.kind == UncertaintyKind::LimitExhausted(Limit::Nodes)
                })
                .count(),
            1,
            "{a:?}"
        );
    }

    #[test]
    fn conditional_cwd_is_merged_as_uncertain() {
        assert_eq!(targets(&fish("false; and cd sub; touch a.txt")), ["?"]);
    }

    #[test]
    fn oversized_redirect_destination_is_unresolved() {
        let path = "a".repeat(64 * 1024 + 1);
        let a = fish(&format!("echo x > {path}"));
        assert!(
            a.file_effects
                .iter()
                .all(|effect| effect.target == Target::Unresolved),
            "{a:?}"
        );
    }

    #[test]
    fn set_erase_invalidates_variable() {
        let a = fish("set f out; set -e f; echo x > $f");
        assert!(
            a.file_effects
                .iter()
                .all(|effect| effect.target == Target::Unresolved),
            "{a:?}"
        );
    }

    #[test]
    fn catalog_commands_and_substitutions() {
        assert_eq!(targets(&fish("rm (echo a.txt)")), ["?"]);
        assert_eq!(targets(&fish("echo (touch t.txt)")), ["/repo/t.txt"]);
    }

    #[test]
    fn read_only_completion_probes_are_silent() {
        let a = fish("complete -C 'git che' | head -n 5");
        assert!(
            a.file_effects.is_empty() && a.uncertainties.is_empty(),
            "{a:?}"
        );
        assert_eq!(programs(&a), ["complete", "head"]);
    }

    #[test]
    fn fish_c_from_bash_is_fish_source() {
        let a = crate::testutil::bash("fish -c 'echo x > f.txt'");
        assert_eq!(targets(&a), ["/repo/f.txt"]);
    }
}
