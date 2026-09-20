//! The values a rule may carry, and how a spelling off the wire resolves onto
//! them.
//!
//! The closed half and the open half are split by what the extractor does with
//! each. `tasks`, `severity` and `scope` are pydantic `Literal`s over there, so
//! a rule carrying anything else fails validation and never reaches an index;
//! parsing them as closed enums and dropping the rule reproduces that.
//! Language, category and topic are coerced onto a vocabulary a repository
//! extends, so an index legitimately carries values no enum here knows, and
//! they stay normalized strings.

use strum::{Display, EnumString};

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, Display)]
#[strum(serialize_all = "kebab-case")]
pub enum Task {
    CodeReview,
    CodeGeneration,
    CodeQuestions,
}

/// Ordered most severe first, so a floor is `severity <= floor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, EnumString, Display)]
#[strum(serialize_all = "lowercase")]
pub enum Severity {
    Must,
    Should,
    Can,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, Display)]
#[strum(serialize_all = "kebab-case")]
pub enum Scope {
    Repo,
    Directory,
    FilePattern,
}

pub const ALL_LANGUAGES: &str = "all";

/// Ported from `repo-rules-agent` `rules/vocabulary.py`. Kept in sync by hand
/// until devkit reads a repository's own extractor config.
const LANGUAGE_ALIASES: &[(&str, &str)] = &[
    ("ts", "typescript"),
    ("tsx", "typescript"),
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("mjs", "javascript"),
    ("node", "javascript"),
    ("nodejs", "javascript"),
    ("py", "python"),
    ("rs", "rust"),
    ("golang", "go"),
    ("kt", "kotlin"),
    ("rb", "ruby"),
    ("c#", "csharp"),
    ("cs", "csharp"),
    ("c++", "cpp"),
    ("sh", "bash"),
    ("shell", "bash"),
    ("zsh", "bash"),
    ("terraform", "hcl"),
    ("tf", "hcl"),
    ("opentofu", "hcl"),
    ("yml", "yaml"),
    ("postgres", "sql"),
    ("postgresql", "sql"),
    ("plpgsql", "sql"),
    ("pl/pgsql", "sql"),
    ("sqlite", "sql"),
    ("md", "markdown"),
    ("mdx", "markdown"),
    ("docker", "dockerfile"),
];

/// One language spelling, canonical. Serves four inputs: a CLI argument, a
/// value out of an index, a repository's own vocabulary extra, and a file
/// extension off disk.
///
/// The alias key is lowercase rather than snake_case, because `c#`, `c++` and
/// `pl/pgsql` are real entries and snake_casing would destroy them.
pub fn canonical_language(value: &str) -> String {
    let lowered = value.trim().trim_start_matches('.').to_lowercase();
    LANGUAGE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lowered)
        .map_or(lowered, |(_, canonical)| (*canonical).to_string())
}

/// The snake_case form a category or topic is stored under, so `Code-Style`,
/// `code style` and `code_style` agree.
pub fn vocabulary_key(value: &str) -> String {
    value.trim().to_lowercase().replace(['-', ' '], "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_aliases_resolve_to_one_canonical_name() {
        for input in [
            "TypeScript",
            "typescript",
            "ts",
            ".ts",
            "TSX",
            "  TS  ",
            ".tsx",
        ] {
            assert_eq!(canonical_language(input), "typescript", "input: {input}");
        }
        assert_eq!(canonical_language("c#"), "csharp");
        assert_eq!(canonical_language("C++"), "cpp");
        assert_eq!(canonical_language("PL/pgSQL"), "sql");
        assert_eq!(canonical_language("zsh"), "bash");
        assert_eq!(canonical_language("all"), "all");
    }

    /// An unknown language is kept as its normalized self rather than dropped:
    /// a repository extends the vocabulary through its own extractor config,
    /// and devkit does not read that file yet.
    #[test]
    fn an_unknown_language_keeps_its_normalized_form() {
        assert_eq!(canonical_language("Zig"), "zig");
    }

    #[test]
    fn categories_and_topics_agree_across_separators() {
        for input in ["Code-Style", "code style", "code_style", "CODE STYLE"] {
            assert_eq!(vocabulary_key(input), "code_style", "input: {input}");
        }
    }

    #[test]
    fn severity_orders_must_first() {
        assert!(Severity::Must < Severity::Should);
        assert!(Severity::Should < Severity::Can);
    }

    #[test]
    fn an_off_vocabulary_severity_drops_the_rule() {
        assert_eq!("critical".parse::<Severity>().ok(), None);
        assert_eq!("must".parse::<Severity>().ok(), Some(Severity::Must));
    }

    #[test]
    fn tasks_parse_from_their_hyphenated_spelling() {
        assert_eq!(
            "code-generation".parse::<Task>().ok(),
            Some(Task::CodeGeneration)
        );
        assert_eq!("code-review".parse::<Task>().ok(), Some(Task::CodeReview));
        assert_eq!(
            "code-questions".parse::<Task>().ok(),
            Some(Task::CodeQuestions)
        );
        assert_eq!("codegen".parse::<Task>().ok(), None);
    }
}
