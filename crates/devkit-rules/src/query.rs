//! Which rules govern a path.
//!
//! Two divergences from `repo-rules-agent` `rules/query.py`, both leniency in
//! the face of imperfect extraction. A rule with no tasks matches every task
//! rather than none, because an empty list means the model did not answer. And
//! severity is available as a floor as well as an exact match, so an index the
//! extractor tagged imperfectly still steers a write.

use std::path::{Component, Path, PathBuf};

use crate::{
    model::{Rule, RuleIndex},
    vocab::{ALL_LANGUAGES, Scope, Severity, Task, canonical_language, vocabulary_key},
};

/// Whether rules for the repo-relative `directory` apply to `path`. The empty
/// directory is the repository root and governs everything.
pub fn governs(directory: &str, path: &str) -> bool {
    directory.is_empty()
        || path == directory
        || path
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `target` as a '/'-separated path under `root`, or `None` when it escapes.
///
/// Normalizing before the prefix strip is the point: `/repo/src/../../etc/x`
/// is lexically under `/repo` as a string and is not a file in the repository,
/// and a rule for the repository root must not fire for it.
pub fn relativize(root: &Path, target: &Path) -> Option<String> {
    let normalized = normalize(target);
    let rel = normalized.strip_prefix(normalize(root)).ok()?;
    let parts: Vec<&str> = rel
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

/// Lexical `..` and `.` resolution. Purely textual: the target of a write need
/// not exist yet, so asking the filesystem is not available.
fn normalize(path: &Path) -> PathBuf {
    let mut stack: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match stack.last() {
                Some(Component::Normal(_)) => {
                    stack.pop();
                }
                _ => stack.push(component),
            },
            other => stack.push(other),
        }
    }
    stack.into_iter().collect()
}

#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub task: Option<Task>,
    pub language: Option<String>,
    pub scope: Option<Scope>,
    /// Exact match, the extractor's own meaning.
    pub severity: Option<Severity>,
    /// Least severe value still kept. `Should` keeps `must` and `should`.
    pub min_severity: Option<Severity>,
    /// Repo-relative, '/'-separated. Empty keeps every rule.
    pub paths: Vec<String>,
}

/// Rules matching `filter`, in index order. Ranking is a separate step.
pub fn matching<'a>(index: &'a RuleIndex, filter: &Filter) -> Vec<&'a Rule> {
    index
        .rules
        .iter()
        .filter(|rule| keeps(rule, filter))
        .collect()
}

fn keeps(rule: &Rule, filter: &Filter) -> bool {
    let Some(severity) = rule.severity() else {
        return false;
    };
    if filter.severity.is_some_and(|s| s != severity) {
        return false;
    }
    if filter.min_severity.is_some_and(|floor| severity > floor) {
        return false;
    }
    let Some(scope) = rule.scope() else {
        return false;
    };
    if filter.scope.is_some_and(|s| s != scope) {
        return false;
    }
    if !filter.paths.is_empty() && !filter.paths.iter().any(|p| governs(&rule.directory, p)) {
        return false;
    }
    let Some(tasks) = rule.tasks() else {
        return false;
    };
    if filter
        .task
        .is_some_and(|task| !tasks.is_empty() && !tasks.contains(&task))
    {
        return false;
    }
    if let Some(language) = &filter.language {
        let wanted = canonical_language(language);
        if !rule
            .languages_canonical()
            .iter()
            .any(|l| l == &wanted || l == ALL_LANGUAGES)
        {
            return false;
        }
    }
    true
}

/// The tier a source file with no entry in the index falls back to. Matches
/// `repo-rules-agent`'s `DOCS_TIER`, so a rule whose file the index does not
/// list sinks below every listed one.
const DOCS_TIER: i64 = 5;

/// Whether `rule` is tagged with `topic`, or names it as a word in its title or
/// description. The text fallback covers a rule extracted before the repository
/// defined the topic, and one the model left untagged.
pub fn is_about(rule: &Rule, topic: &str) -> bool {
    let wanted = vocabulary_key(topic);
    if rule.topics_canonical().iter().any(|t| t == &wanted) {
        return true;
    }
    let haystack = vocabulary_key(&format!("{} {}", rule.title, rule.description));
    haystack
        .match_indices(&wanted)
        .any(|(at, _)| is_word_boundary(&haystack, at, wanted.len()))
}

/// Whether the match at `at` stands alone rather than sitting inside a longer
/// word. Both sides are already `vocabulary_key`'d, so a separator here is `_`.
fn is_word_boundary(haystack: &str, at: usize, len: usize) -> bool {
    let before = haystack[..at].chars().next_back();
    let after = haystack[at + len..].chars().next();
    let free = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    free(before) && free(after)
}

/// Order rules most useful first: those about a requested topic, then deeper
/// directories, then severity, then the discovery tier of their source file.
pub fn rank<'a>(index: &RuleIndex, rules: Vec<&'a Rule>, topics: &[String]) -> Vec<&'a Rule> {
    let mut ranked = rules;
    ranked.sort_by_cached_key(|rule| {
        let about = topics.iter().any(|t| is_about(rule, t));
        let depth = if rule.directory.is_empty() {
            0
        } else {
            rule.directory.split('/').count() as i64
        };
        let tier = index
            .files
            .iter()
            .find(|f| f.path == rule.source_file)
            .map_or(DOCS_TIER, |f| f.tier);
        (!about, -depth, rule.severity(), tier)
    });
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RuleIndex;

    fn fixture() -> RuleIndex {
        serde_json::from_str(include_str!("../tests/fixtures/index.json")).unwrap()
    }

    #[test]
    fn governs_stops_at_a_component_boundary() {
        assert!(governs("", "anything/at/all.rs"));
        assert!(governs("crates/foo", "crates/foo"));
        assert!(governs("crates/foo", "crates/foo/src/a.rs"));
        assert!(!governs("crates/foo", "crates/foobar/src/a.rs"));
        assert!(!governs("crates/foo", "crates/bar/a.rs"));
    }

    #[test]
    fn relativize_normalizes_and_drops_escapes() {
        let root = Path::new("/repo");
        assert_eq!(
            relativize(root, Path::new("/repo/src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            relativize(root, Path::new("/repo/./src/../src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(relativize(root, Path::new("/repo")).as_deref(), Some("."));
        assert_eq!(relativize(root, Path::new("/repo/src/../../etc/x")), None);
        assert_eq!(relativize(root, Path::new("/elsewhere/x")), None);
    }

    /// The extractor drops a rule whose `tasks` list omits the queried task.
    /// devkit keeps it: an empty list means the model did not answer, not that
    /// the rule governs nothing.
    #[test]
    fn an_untagged_rule_matches_every_task() {
        let index = fixture();
        let filter = Filter {
            task: Some(Task::CodeGeneration),
            language: Some("typescript".to_string()),
            ..Filter::default()
        };
        let ids: Vec<&str> = matching(&index, &filter)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(ids.contains(&"r-untagged"), "got {ids:?}");
    }

    #[test]
    fn a_review_only_rule_does_not_match_code_generation() {
        let index = fixture();
        let filter = Filter {
            task: Some(Task::CodeGeneration),
            ..Filter::default()
        };
        let ids: Vec<&str> = matching(&index, &filter)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(!ids.contains(&"r-review-only"), "got {ids:?}");
    }

    #[test]
    fn min_severity_is_a_floor_and_severity_is_exact() {
        let index = fixture();
        let floor = Filter {
            min_severity: Some(Severity::Should),
            ..Filter::default()
        };
        let ids: Vec<&str> = matching(&index, &floor)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(ids.contains(&"r-root-must"));
        assert!(ids.contains(&"r-foo-should"));
        assert!(
            !ids.contains(&"r-foo-can"),
            "the floor excludes can: {ids:?}"
        );

        let exact = Filter {
            severity: Some(Severity::Should),
            ..Filter::default()
        };
        let ids: Vec<&str> = matching(&index, &exact)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(
            !ids.contains(&"r-root-must"),
            "exact excludes must: {ids:?}"
        );
    }

    #[test]
    fn an_off_vocabulary_severity_drops_its_rule_from_every_query() {
        let index = fixture();
        let ids: Vec<&str> = matching(&index, &Filter::default())
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(!ids.contains(&"r-bad-severity"), "got {ids:?}");
    }

    #[test]
    fn an_off_vocabulary_scope_drops_its_rule_from_every_query() {
        let json = r#"{"repo": "/repo", "files": [], "rules": [{"id": "r-bad-scope", "title": "Bad scope", "description": "Off-vocabulary scope.", "scope": "nonexistent", "severity": "must", "tasks": ["code-generation"]}]}"#;
        let index: RuleIndex = serde_json::from_str(json).unwrap();
        let ids: Vec<&str> = matching(&index, &Filter::default())
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(!ids.contains(&"r-bad-scope"), "got {ids:?}");
    }

    #[test]
    fn a_path_filter_keeps_repo_rules_and_the_directorys_own() {
        let index = fixture();
        let filter = Filter {
            paths: vec!["crates/foo/src/a.rs".to_string()],
            ..Filter::default()
        };
        let ids: Vec<&str> = matching(&index, &filter)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(ids.contains(&"r-root-must"));
        assert!(ids.contains(&"r-foo-should"));

        let filter = Filter {
            paths: vec!["crates/bar/src/a.rs".to_string()],
            ..Filter::default()
        };
        let ids: Vec<&str> = matching(&index, &filter)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(ids.contains(&"r-root-must"));
        assert!(!ids.contains(&"r-foo-should"), "got {ids:?}");
    }

    #[test]
    fn a_language_filter_keeps_all_language_rules() {
        let index = fixture();
        let filter = Filter {
            language: Some("rust".to_string()),
            ..Filter::default()
        };
        let ids: Vec<&str> = matching(&index, &filter)
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert!(ids.contains(&"r-foo-should"), "the rust rule");
        assert!(ids.contains(&"r-root-must"), "the all-language rule");
        assert!(!ids.contains(&"r-untagged"), "the typescript rule: {ids:?}");
    }

    #[test]
    fn is_about_matches_a_tag_or_the_text_across_separators() {
        let rule = Rule {
            title: "Kysely migrations".to_string(),
            description: "Use the migration runner.".to_string(),
            topics: vec!["code_style".to_string()],
            ..fixture().rules[0].clone()
        };
        assert!(is_about(&rule, "code_style"), "tagged");
        assert!(is_about(&rule, "Code-Style"), "tagged, other spelling");
        assert!(is_about(&rule, "kysely"), "named in the title");
        assert!(is_about(&rule, "migration"), "named in the description");
        assert!(
            !is_about(&rule, "migrations_runner"),
            "not a word in either"
        );
        assert!(!is_about(&rule, "grat"), "a substring is not a word");
    }

    #[test]
    fn rank_puts_deeper_directories_and_harder_severity_first() {
        let index = fixture();
        let filter = Filter {
            paths: vec!["crates/foo/src/a.rs".to_string()],
            ..Filter::default()
        };
        let ranked = rank(&index, matching(&index, &filter), &[]);
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        let foo = ids.iter().position(|i| *i == "r-foo-should").unwrap();
        let root = ids.iter().position(|i| *i == "r-root-must").unwrap();
        assert!(
            foo < root,
            "the directory rule outranks the repo rule: {ids:?}"
        );
    }

    #[test]
    fn a_requested_topic_outranks_everything_else() {
        let index = fixture();
        let ranked = rank(&index, matching(&index, &Filter::default()), &[
            "readability".to_string(),
        ]);
        assert_eq!(
            ranked[0].id, "r-foo-can",
            "the only rule tagged with the topic"
        );
    }

    #[test]
    fn severity_breaks_ties_when_depth_and_topics_match() {
        let index = fixture();
        let ranked = rank(&index, matching(&index, &Filter::default()), &[]);
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        let must_pos = ids
            .iter()
            .position(|i| *i == "r-root-must")
            .expect("r-root-must in results");
        let should_pos = ids
            .iter()
            .position(|i| *i == "r-untagged")
            .expect("r-untagged in results");
        assert!(
            must_pos < should_pos,
            "must ranks before should at same depth: {ids:?}"
        );
    }

    #[test]
    fn tier_breaks_ties_when_depth_severity_and_topics_match() {
        let json = r#"{"repo": "/repo", "files": [{"path": "listed.rs", "tier": 2}], "rules": [{"id": "r-listed", "title": "Listed", "description": "In files.", "scope": "repo", "severity": "must", "source_file": "listed.rs"}, {"id": "r-unlisted", "title": "Unlisted", "description": "Not in files.", "scope": "repo", "severity": "must", "source_file": "unlisted.rs"}]}"#;
        let index: RuleIndex = serde_json::from_str(json).unwrap();
        let ranked = rank(&index, matching(&index, &Filter::default()), &[]);
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        let listed = ids
            .iter()
            .position(|i| *i == "r-listed")
            .expect("r-listed in results");
        let unlisted = ids
            .iter()
            .position(|i| *i == "r-unlisted")
            .expect("r-unlisted in results");
        assert!(
            listed < unlisted,
            "listed tier ranks before docs_tier fallback: {ids:?}"
        );
    }
}
