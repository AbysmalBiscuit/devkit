//! Parser construction and the node helpers every adapter shares.

use tree_sitter::{Node, Parser, Tree};

use crate::model::Language;

/// A syntax tree for `source`, or `None` when the parser refused to produce
/// one. A tree containing `ERROR` or `MISSING` nodes is still returned: the
/// adapters recover per statement.
pub(crate) fn parse(language: Language, source: &str) -> Option<Tree> {
    let grammar: tree_sitter::Language = match language {
        Language::Bash => tree_sitter_bash::LANGUAGE.into(),
        Language::PowerShell => tree_sitter_powershell::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::Fish => tree_sitter_fish::language(),
    };
    let mut parser = Parser::new();
    parser.set_language(&grammar).ok()?;
    parser.parse(source, None)
}

pub(crate) fn text<'s>(node: Node<'_>, source: &'s str) -> &'s str {
    &source[node.byte_range()]
}

pub(crate) fn is_broken(node: Node<'_>) -> bool {
    node.is_error() || node.is_missing()
}

pub(crate) fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_grammar_parses_a_trivial_program() {
        for (language, source) in [
            (Language::Bash, "echo hi > out.txt"),
            (Language::PowerShell, "Set-Content -Path out.txt -Value hi"),
            (Language::Python, "open('out.txt', 'w').write('hi')"),
            (
                Language::JavaScript,
                "require('fs').writeFileSync('out.txt', 'hi')",
            ),
            (Language::TypeScript, "const p: string = 'out.txt'"),
            (Language::Fish, "echo hi > out.txt;"),
        ] {
            let tree = parse(language, source).expect("a tree");
            assert!(
                !tree.root_node().has_error(),
                "{language:?}: {}",
                tree.root_node().to_sexp()
            );
        }
    }
}
