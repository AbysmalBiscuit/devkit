//! Which paths in a repository are test files, by `[pr] test_files`.

use anyhow::{Context, Result};

/// A compiled set of test-file patterns.
#[derive(Debug, Clone)]
pub struct TestFiles {
    patterns: Vec<glob::Pattern>,
}

impl TestFiles {
    /// Compile `patterns`, each relative to the repository root. An invalid
    /// pattern is an error naming it.
    pub fn new(patterns: &[String]) -> Result<Self> {
        let patterns = patterns
            .iter()
            .map(|p| {
                glob::Pattern::new(p).with_context(|| format!("[pr] test_files pattern `{p}`"))
            })
            .collect::<Result<_>>()?;
        Ok(Self { patterns })
    }

    /// Whether `path`, relative to the repository root with `/` separators,
    /// is a test file.
    pub fn matches(&self, path: &str) -> bool {
        let opts = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        self.patterns.iter().any(|p| p.matches_with(path, opts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> TestFiles {
        TestFiles::new(&devkit_config::default_test_files()).unwrap()
    }

    #[test]
    fn the_defaults_cover_the_usual_layouts() {
        let t = defaults();
        for path in [
            "tests/cli.rs",
            "crates/x/tests/deep/a.rs",
            "src/parser_test.rs",
            "web/src/app.test.tsx",
            "web/src/app.spec.ts",
            "web/__tests__/a.js",
            "test/a.js",
            "pkg/test_models.py",
            "pkg/models_test.py",
        ] {
            assert!(t.matches(path), "{path}");
        }
        for path in [
            "src/main.rs",
            "web/src/app.tsx",
            "pkg/models.py",
            "contest.py",
        ] {
            assert!(!t.matches(path), "{path}");
        }
    }

    #[test]
    fn a_star_stays_within_one_directory() {
        let t = TestFiles::new(&["spec/*.rb".to_string()]).unwrap();
        assert!(t.matches("spec/a.rb"));
        assert!(!t.matches("spec/deep/a.rb"));
    }

    #[test]
    fn an_invalid_pattern_is_named() {
        let err = TestFiles::new(&["a/[".to_string()]).unwrap_err();
        assert!(format!("{err:#}").contains("a/["), "{err:#}");
    }
}
