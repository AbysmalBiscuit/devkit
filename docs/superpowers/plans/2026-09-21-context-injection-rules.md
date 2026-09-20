# Context injection and rules implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the rules that govern a file in front of the agent at the moment it is about to write that file.

**Architecture:** A new `devkit-rules` library crate matches a prebuilt JSON rule index and a set of config-declared file dumps against a subject (the paths a tool call is about to write). Two callers drive it: a `pre-tool-use` stage that emits on the allow path only, and a `devkit rules` verb. Per-holder state on disk keeps a rule from firing twice in one session.

**Tech Stack:** Rust edition 2024, `serde`/`serde_json`, `strum` (new workspace dependency), `glob` and `ring` (both already workspace dependencies), `clap`, `devkit_common::ui` for tables, `tempfile` for tests, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-09-21-context-injection-rules-design.md`

## Global Constraints

- Rust edition 2024. The workspace pins `edition.workspace = true`; every new crate uses it.
- `cargo clippy --workspace --all-targets -- -D warnings` must pass. Zero warnings.
- `devrun task fmt` before every commit. Nightly rustfmt; stable silently ignores `rustfmt.toml`.
- `cargo nextest run --workspace --no-fail-fast` and `cargo test --workspace --doc` are the test gate.
- Errors use `anyhow` with `.context()`.
- Help text stays ASCII (`src/completions.rs`).
- A config type's `devkit.toml` example is a doctest on that type. `schema/devkit-config.json` is committed; regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test`.
- Match `Role` and `StateKind` exhaustively.
- Conventional Commits. Every commit message in this plan ends with:
  `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`
- Test scratch comes from `tempfile`, bound for as long as the path is used.
- No `hook` verb exits 2. Only `pre-tool-use` writes stdout from a `hook` verb.

## Review Focus

Five conditions the spec implies that no task's happy-path tests would otherwise reach. Each has a test assigned to the task that owns the code.

1. **An index whose top-level JSON shape has drifted** (the extractor changed its schema). Expected: nothing injects, one stderr line, exit unchanged. Never a panic and never a denial. Test in Task 5.
2. **A hook payload with no `cwd` key.** `resolve_against` falls back to the raw path, so a relative target cannot be relativized. Expected: that target is dropped, other targets still match. Test in Task 10.
3. **A `[[context.files]]` path that points at a directory, a missing file, or a file with invalid UTF-8.** Expected: that entry is skipped, the event's other injections still go out. Test in Task 7.
4. **A fired-set file containing a torn or garbage line** (concurrent appends from a parent and its subagents). Expected: the unparseable line is skipped, the rest of the set still suppresses. Test in Task 10.
5. **A repository path that cannot be canonicalized** (the worktree was removed underneath a running session). Expected: no index is found, nothing injects, no panic. Test in Task 5.

---

### Task 1: Reset the scaffold and stand up the crate

The working tree carries an abandoned copy of `devkit-locks` under `crates/devkit-rules/`, plus uncommitted `Cargo.toml` and `Cargo.lock` edits that register it. None of it is used. Start from empty.

**Files:**
- Delete: `crates/devkit-rules/` (entire directory, untracked)
- Revert: `Cargo.toml`, `Cargo.lock`
- Create: `crates/devkit-rules/Cargo.toml`
- Create: `crates/devkit-rules/src/lib.rs`
- Create: `crates/devkit-rules/src/model.rs`
- Create: `crates/devkit-rules/tests/fixtures/index.json`
- Modify: `Cargo.toml` (workspace members, `strum` and `devkit-rules` in `[workspace.dependencies]`)
- Modify: `AGENTS.md` (crate table)

**Interfaces:**
- Consumes: nothing.
- Produces: `devkit_rules::model::{Rule, RuleFile, RuleIndex}`, all `Deserialize` with serde defaults on every optional field.

- [ ] **Step 1: Remove the abandoned scaffold**

```bash
rm -rf crates/devkit-rules
git checkout -- Cargo.toml Cargo.lock
git status --short
```

Expected: only `?? docs/superpowers/` remains.

- [ ] **Step 2: Register the crate and its dependencies**

In `Cargo.toml`, add `"crates/devkit-rules"` to `[workspace] members`, and to `[workspace.dependencies]`:

```toml
strum = { version = "0.27", features = ["derive"] }
devkit-rules = { path = "crates/devkit-rules" }
```

- [ ] **Step 3: Create the crate manifest**

`crates/devkit-rules/Cargo.toml`:

```toml
[package]
name = "devkit-rules"
edition.workspace = true
version = "0.14.5" # x-release-please-version

[dependencies]
serde = { workspace = true }
serde_json.workspace = true
strum.workspace = true
glob.workspace = true
ring.workspace = true
devkit-config.workspace = true
devkit-common.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 4: Write the failing test**

`crates/devkit-rules/src/model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The extractor writes with `exclude_none=True`, so every pydantic default
    /// is absent from the JSON and has to be a serde default here.
    #[test]
    fn omitted_fields_take_the_extractor_defaults() {
        let raw = r#"{
            "repo": "/repo",
            "files": [{"path": "AGENTS.md", "tier": 1}],
            "rules": [{"id": "abc123", "title": "T", "description": "D", "source_file": "AGENTS.md"}]
        }"#;
        let index: RuleIndex = serde_json::from_str(raw).unwrap();
        let rule = &index.rules[0];
        assert_eq!(rule.languages, vec!["all".to_string()]);
        assert_eq!(rule.severity_raw, "should");
        assert_eq!(rule.scope_raw, "repo");
        assert!(rule.tasks.is_empty());
        assert!(rule.topics.is_empty());
        assert_eq!(rule.directory, "");
        assert_eq!(rule.category, "best_practice");
        assert_eq!(index.files[0].applies_to, "");
        assert!(index.files[0].content.is_none());
    }
}
```

- [ ] **Step 5: Run it and watch it fail**

Run: `cargo nextest run -p devkit-rules`
Expected: FAIL, `RuleIndex` is not defined.

- [ ] **Step 6: Write the model**

`crates/devkit-rules/src/model.rs`, above the test module:

```rust
//! The rule index as `repo-rules-agent` writes it.
//!
//! Severity and scope arrive as raw strings and are parsed into enums by
//! `vocab`, because a rule carrying a value outside the vocabulary is dropped
//! rather than defaulted: the extractor already drops it at validation, and
//! silently coercing it here would resurrect a rule its own producer refused.

use serde::Deserialize;

fn all_languages() -> Vec<String> {
    vec!["all".to_string()]
}

fn should() -> String {
    "should".to_string()
}

fn repo() -> String {
    "repo".to_string()
}

fn best_practice() -> String {
    "best_practice".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "best_practice")]
    pub category: String,
    #[serde(default)]
    pub tasks: Vec<String>,
    #[serde(default = "all_languages")]
    pub languages: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default = "repo", rename = "scope")]
    pub scope_raw: String,
    #[serde(default = "should", rename = "severity")]
    pub severity_raw: String,
    #[serde(default)]
    pub source_file: String,
    #[serde(default)]
    pub directory: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleFile {
    pub path: String,
    pub tier: i64,
    #[serde(default)]
    pub applies_to: String,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuleIndex {
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub files: Vec<RuleFile>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}
```

`crates/devkit-rules/src/lib.rs`:

```rust
//! Matching a `repo-rules-agent` index and config-declared file dumps against
//! the paths a tool call is about to write.
//!
//! The crate's only IO is reading one JSON file. It knows nothing about
//! harnesses, hook payloads or sessions, because it has three callers: the
//! `pre-tool-use` stage, `devkit rules context`, and `devkit rules query`.

pub mod model;
```

- [ ] **Step 7: Run it and watch it pass**

Run: `cargo nextest run -p devkit-rules`
Expected: PASS.

- [ ] **Step 8: Add the fixture index**

`crates/devkit-rules/tests/fixtures/index.json`. Later tasks match against this, so it carries every shape they need: a repo-scope `must`, a directory-scope `should` in Rust, a directory-scope `can`, a rule with no `tasks`, and a rule with an off-vocabulary severity.

```json
{
  "repo": "/repo",
  "files": [
    {"path": "AGENTS.md", "tier": 1, "applies_to": ""},
    {"path": "crates/foo/AGENTS.md", "tier": 1, "applies_to": "crates/foo"}
  ],
  "rules": [
    {"id": "r-root-must", "title": "Root must", "description": "Applies everywhere.",
     "category": "security", "tasks": ["code-generation"], "languages": ["all"],
     "scope": "repo", "severity": "must", "source_file": "AGENTS.md", "directory": ""},
    {"id": "r-foo-should", "title": "Foo should", "description": "Rust only, under crates/foo.",
     "category": "code_style", "tasks": ["code-generation"], "languages": ["rust"],
     "scope": "directory", "severity": "should", "source_file": "crates/foo/AGENTS.md",
     "directory": "crates/foo"},
    {"id": "r-foo-can", "title": "Foo can", "description": "Optional, under crates/foo.",
     "category": "readability", "topics": ["readability"], "tasks": ["code-generation"],
     "languages": ["all"],
     "scope": "directory", "severity": "can", "source_file": "crates/foo/AGENTS.md",
     "directory": "crates/foo"},
    {"id": "r-untagged", "title": "Untagged", "description": "No tasks listed.",
     "category": "best_practice", "languages": ["typescript"],
     "scope": "repo", "severity": "should", "source_file": "AGENTS.md", "directory": ""},
    {"id": "r-review-only", "title": "Review only", "description": "Code review task only.",
     "category": "best_practice", "tasks": ["code-review"], "languages": ["all"],
     "scope": "repo", "severity": "must", "source_file": "AGENTS.md", "directory": ""},
    {"id": "r-bad-severity", "title": "Bad severity", "description": "Off-vocabulary.",
     "category": "best_practice", "tasks": ["code-generation"], "languages": ["all"],
     "scope": "repo", "severity": "critical", "source_file": "AGENTS.md", "directory": ""}
  ]
}
```

- [ ] **Step 9: Add the crate to the AGENTS.md table**

In `AGENTS.md`, add a row after `devkit-locks`:

```
| `devkit-rules` | rule-index matching and context rendering |
```

- [ ] **Step 10: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run -p devkit-rules
git add Cargo.toml Cargo.lock AGENTS.md crates/devkit-rules
git commit -m "$(cat <<'MSG'
feat(rules): add devkit-rules crate with the index model

The extractor writes with exclude_none, so every optional field is a
serde default here rather than an Option the callers unwrap.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 2: Vocabulary

**Files:**
- Create: `crates/devkit-rules/src/vocab.rs`
- Modify: `crates/devkit-rules/src/lib.rs`

**Interfaces:**
- Consumes: `model::Rule`.
- Produces: `vocab::{Task, Severity, Scope, canonical_language, vocabulary_key}`. `Severity` derives `Ord` with `Must < Should < Can`, so a floor is `severity <= floor`. `Rule::severity()` and `Rule::scope()` return `Option`, `None` meaning the rule is dropped.

- [ ] **Step 1: Write the failing tests**

`crates/devkit-rules/src/vocab.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_aliases_resolve_to_one_canonical_name() {
        for input in ["TypeScript", "typescript", "ts", ".ts", "TSX", "  TS  ", ".tsx"] {
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
        assert_eq!("code-generation".parse::<Task>().ok(), Some(Task::CodeGeneration));
        assert_eq!("code-review".parse::<Task>().ok(), Some(Task::CodeReview));
        assert_eq!("code-questions".parse::<Task>().ok(), Some(Task::CodeQuestions));
        assert_eq!("codegen".parse::<Task>().ok(), None);
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p devkit-rules vocab`
Expected: FAIL, `canonical_language` is not defined.

- [ ] **Step 3: Write the vocabulary**

`crates/devkit-rules/src/vocab.rs`, above the test module:

```rust
//! The values a rule may carry, and how a spelling off the wire resolves onto
//! them.
//!
//! The closed half and the open half are split by what the extractor does with
//! each. `tasks`, `severity` and `scope` are pydantic `Literal`s over there, so
//! a rule carrying anything else fails validation and never reaches an index;
//! parsing them as closed enums and dropping the rule reproduces that. Language,
//! category and topic are coerced onto a vocabulary a repository extends, so an
//! index legitimately carries values no enum here knows, and they stay
//! normalized strings.

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
    value
        .trim()
        .to_lowercase()
        .replace(['-', ' '], "_")
}
```

- [ ] **Step 4: Add the rule accessors**

Append to `crates/devkit-rules/src/model.rs`, above its test module:

```rust
use crate::vocab::{Scope, Severity, Task, canonical_language, vocabulary_key};

impl Rule {
    /// `None` when the index carries a severity outside the vocabulary, which
    /// drops the rule. The extractor's own validation already refuses these.
    pub fn severity(&self) -> Option<Severity> {
        self.severity_raw.parse().ok()
    }

    pub fn scope(&self) -> Option<Scope> {
        self.scope_raw.parse().ok()
    }

    /// Parsed tasks, unknown spellings dropped. An empty result may mean the
    /// index listed none or listed only unrecognized ones; `matching` treats
    /// both as "applies to every task".
    pub fn tasks(&self) -> Vec<Task> {
        self.tasks.iter().filter_map(|t| t.parse().ok()).collect()
    }

    pub fn languages_canonical(&self) -> Vec<String> {
        self.languages.iter().map(|l| canonical_language(l)).collect()
    }

    pub fn topics_canonical(&self) -> Vec<String> {
        self.topics.iter().map(|t| vocabulary_key(t)).collect()
    }
}
```

- [ ] **Step 5: Declare the module**

In `crates/devkit-rules/src/lib.rs`, add `pub mod vocab;` above `pub mod model;`.

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo nextest run -p devkit-rules`
Expected: PASS, all tests.

- [ ] **Step 7: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run -p devkit-rules
git add crates/devkit-rules
git commit -m "$(cat <<'MSG'
feat(rules): resolve the index vocabulary

Task, severity and scope are closed enums and an off-vocabulary value
drops its rule, matching the extractor's own validation. Language,
category and topic stay normalized strings, because a repository extends
those through its extractor config.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 3: Path scoping and matching

**Files:**
- Create: `crates/devkit-rules/src/query.rs`
- Modify: `crates/devkit-rules/src/lib.rs`

**Interfaces:**
- Consumes: `model::{Rule, RuleIndex}`, `vocab::{Task, Severity}`.
- Produces: `query::{governs, relativize, Filter, matching}`. `Filter` has `task: Option<Task>`, `language: Option<String>`, `min_severity: Option<Severity>`, `severity: Option<Severity>`, `scope: Option<Scope>`, `paths: Vec<String>`. `matching(&RuleIndex, &Filter) -> Vec<&Rule>`, unranked.

- [ ] **Step 1: Write the failing tests**

`crates/devkit-rules/src/query.rs`:

```rust
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
        assert_eq!(relativize(root, Path::new("/repo/src/a.rs")).as_deref(), Some("src/a.rs"));
        assert_eq!(relativize(root, Path::new("/repo/./src/../src/a.rs")).as_deref(), Some("src/a.rs"));
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
        let ids: Vec<&str> = matching(&index, &filter).iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"r-untagged"), "got {ids:?}");
    }

    #[test]
    fn a_review_only_rule_does_not_match_code_generation() {
        let index = fixture();
        let filter = Filter { task: Some(Task::CodeGeneration), ..Filter::default() };
        let ids: Vec<&str> = matching(&index, &filter).iter().map(|r| r.id.as_str()).collect();
        assert!(!ids.contains(&"r-review-only"), "got {ids:?}");
    }

    #[test]
    fn min_severity_is_a_floor_and_severity_is_exact() {
        let index = fixture();
        let floor = Filter { min_severity: Some(Severity::Should), ..Filter::default() };
        let ids: Vec<&str> = matching(&index, &floor).iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"r-root-must"));
        assert!(ids.contains(&"r-foo-should"));
        assert!(!ids.contains(&"r-foo-can"), "the floor excludes can: {ids:?}");

        let exact = Filter { severity: Some(Severity::Should), ..Filter::default() };
        let ids: Vec<&str> = matching(&index, &exact).iter().map(|r| r.id.as_str()).collect();
        assert!(!ids.contains(&"r-root-must"), "exact excludes must: {ids:?}");
    }

    #[test]
    fn an_off_vocabulary_severity_drops_its_rule_from_every_query() {
        let index = fixture();
        let ids: Vec<&str> = matching(&index, &Filter::default()).iter().map(|r| r.id.as_str()).collect();
        assert!(!ids.contains(&"r-bad-severity"), "got {ids:?}");
    }

    #[test]
    fn a_path_filter_keeps_repo_rules_and_the_directorys_own() {
        let index = fixture();
        let filter = Filter { paths: vec!["crates/foo/src/a.rs".to_string()], ..Filter::default() };
        let ids: Vec<&str> = matching(&index, &filter).iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"r-root-must"));
        assert!(ids.contains(&"r-foo-should"));

        let filter = Filter { paths: vec!["crates/bar/src/a.rs".to_string()], ..Filter::default() };
        let ids: Vec<&str> = matching(&index, &filter).iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"r-root-must"));
        assert!(!ids.contains(&"r-foo-should"), "got {ids:?}");
    }

    #[test]
    fn a_language_filter_keeps_all_language_rules() {
        let index = fixture();
        let filter = Filter { language: Some("rust".to_string()), ..Filter::default() };
        let ids: Vec<&str> = matching(&index, &filter).iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"r-foo-should"), "the rust rule");
        assert!(ids.contains(&"r-root-must"), "the all-language rule");
        assert!(!ids.contains(&"r-untagged"), "the typescript rule: {ids:?}");
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p devkit-rules query`
Expected: FAIL, `governs` is not defined.

- [ ] **Step 3: Write the matcher**

`crates/devkit-rules/src/query.rs`, above the test module:

```rust
//! Which rules govern a path.
//!
//! Two divergences from `repo-rules-agent` `rules/query.py`, both leniency in
//! the face of imperfect extraction. A rule with no tasks matches every task
//! rather than none, because an empty list means the model did not answer. And
//! severity is available as a floor as well as an exact match, so the hook can
//! ask for "must and should" in one query.

use std::path::{Component, Path, PathBuf};

use crate::{
    model::{Rule, RuleIndex},
    vocab::{ALL_LANGUAGES, Scope, Severity, Task, canonical_language},
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
    Some(if parts.is_empty() { ".".to_string() } else { parts.join("/") })
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
    if filter.scope.is_some_and(|s| rule.scope() != Some(s)) {
        return false;
    }
    if !filter.paths.is_empty()
        && !filter.paths.iter().any(|p| governs(&rule.directory, p))
    {
        return false;
    }
    if let Some(task) = filter.task {
        let tasks = rule.tasks();
        if !tasks.is_empty() && !tasks.contains(&task) {
            return false;
        }
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
```

- [ ] **Step 4: Declare the module**

In `crates/devkit-rules/src/lib.rs`, add `pub mod query;`.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo nextest run -p devkit-rules`
Expected: PASS.

- [ ] **Step 6: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run -p devkit-rules
git add crates/devkit-rules
git commit -m "$(cat <<'MSG'
feat(rules): match rules against a path

An untagged rule matches every task and severity is available as a floor,
so an index the extractor tagged imperfectly still steers a write.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 4: Ranking

**Files:**
- Modify: `crates/devkit-rules/src/query.rs`

**Interfaces:**
- Consumes: `query::matching`, `model::RuleIndex`.
- Produces: `query::{rank, is_about}`. `rank(&RuleIndex, Vec<&Rule>, &[String]) -> Vec<&Rule>`.

- [ ] **Step 1: Write the failing tests**

Append to the test module in `crates/devkit-rules/src/query.rs`:

```rust
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
        assert!(!is_about(&rule, "migrations_runner"), "not a word in either");
        assert!(!is_about(&rule, "grat"), "a substring is not a word");
    }

    #[test]
    fn rank_puts_deeper_directories_and_harder_severity_first() {
        let index = fixture();
        let filter = Filter { paths: vec!["crates/foo/src/a.rs".to_string()], ..Filter::default() };
        let ranked = rank(&index, matching(&index, &filter), &[]);
        let ids: Vec<&str> = ranked.iter().map(|r| r.id.as_str()).collect();
        let foo = ids.iter().position(|i| *i == "r-foo-should").unwrap();
        let root = ids.iter().position(|i| *i == "r-root-must").unwrap();
        assert!(foo < root, "the directory rule outranks the repo rule: {ids:?}");
    }

    #[test]
    fn a_requested_topic_outranks_everything_else() {
        let index = fixture();
        let ranked = rank(&index, matching(&index, &Filter::default()), &["readability".to_string()]);
        assert_eq!(ranked[0].id, "r-foo-can", "the only rule tagged with the topic");
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p devkit-rules query`
Expected: FAIL, `is_about` is not defined.

- [ ] **Step 3: Write the ranker**

Append to `crates/devkit-rules/src/query.rs`, above the test module:

```rust
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
```

Add `vocabulary_key` to the `crate::vocab` import list at the top of the file.

- [ ] **Step 4: Run the tests and watch them pass**

Run: `cargo nextest run -p devkit-rules`
Expected: PASS.

- [ ] **Step 5: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run -p devkit-rules
git add crates/devkit-rules
git commit -m "$(cat <<'MSG'
feat(rules): rank matched rules

Topic match first, then directory depth, then severity, then tier. The
topic text fallback checks word boundaries by hand rather than pulling
in a regex engine for one predicate.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 5: Finding the index

**Files:**
- Create: `crates/devkit-rules/src/index.rs`
- Modify: `crates/devkit-rules/src/lib.rs`

**Interfaces:**
- Consumes: `model::RuleIndex`.
- Produces: `index::{cache_dir_name, default_index_path, load}`. `load(&Path) -> Option<RuleIndex>`, `None` on every failure, one stderr line when the file exists and does not parse.

- [ ] **Step 1: Write the failing tests**

`crates/devkit-rules/src/index.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Ported from `repo-rules-agent` `rules/paths.py`: basename with every run
    /// of disallowed characters collapsed to `-`, trimmed, lowercased, plus the
    /// first eight hex characters of the SHA-256 of the absolute path.
    #[test]
    fn the_cache_dir_name_matches_the_extractors() {
        let name = cache_dir_name(Path::new("/srv/checkouts/devkit"));
        let (basename, digest) = name.rsplit_once('-').unwrap();
        assert_eq!(basename, "devkit");
        assert_eq!(digest.len(), 8);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_basename_with_disallowed_characters_collapses() {
        assert!(cache_dir_name(Path::new("/tmp/My Repo!!")).starts_with("my-repo-"));
        assert!(cache_dir_name(Path::new("/tmp/--weird--")).starts_with("weird-"));
    }

    /// Review Focus 1: an index whose top-level shape drifted injects nothing
    /// and does not panic.
    #[test]
    fn a_drifted_index_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.json");
        std::fs::write(&path, r#"{"rules": "not an array"}"#).unwrap();
        assert!(load(&path).is_none());
    }

    #[test]
    fn a_missing_index_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("absent.json")).is_none());
    }

    /// Review Focus 5: a repository path that cannot be canonicalized still
    /// yields a name rather than panicking, so the lookup simply misses.
    #[test]
    fn a_vanished_repo_path_still_names_a_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("removed-worktree");
        let name = cache_dir_name(&gone);
        assert!(name.starts_with("removed-worktree-"), "got {name}");
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p devkit-rules index`
Expected: FAIL, `cache_dir_name` is not defined.

- [ ] **Step 3: Write the locator**

`crates/devkit-rules/src/index.rs`, above the test module:

```rust
//! Where the index lives, and reading it.
//!
//! The path is the one `repo-rules-agent` writes, so an index built by the
//! extractor is found with no configuration. The hashed repository path is the
//! main worktree rather than the current checkout: every devkit branch lives in
//! its own worktree, and an index built once in the main checkout would
//! otherwise be invisible from every worktree where the work happens.

use std::path::{Path, PathBuf};

use crate::model::RuleIndex;

/// The cache directory name for a repository, as `rules/paths.py` builds it.
pub fn cache_dir_name(repo: &Path) -> String {
    // Python hashes `str(Path.resolve())`. `canonicalize` agrees on Unix and
    // does not on Windows, where it returns a `\\?\C:\...` verbatim path that
    // `Path.resolve()` never produces, so the digest would differ for every
    // repository. Strip the prefix before hashing.
    let resolved = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let resolved = strip_verbatim(&resolved);
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        resolved.to_string_lossy().as_bytes(),
    );
    let hex: String = digest.as_ref()[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let raw = resolved
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // `re.sub(r"[^a-zA-Z0-9._-]+", "-", name)`: one dash per run of disallowed
    // characters, never merged with a neighbouring literal dash. Collapsing
    // `a-!b` to `a-b` where Python gives `a--b` names a different directory,
    // and the index is then silently not found.
    let mut basename = String::with_capacity(raw.len());
    let mut in_run = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            basename.push(c);
            in_run = false;
        } else if !in_run {
            basename.push('-');
            in_run = true;
        }
    }
    let basename = basename.trim_matches('-').to_lowercase();
    let basename = if basename.is_empty() { "repo" } else { &basename };
    format!("{basename}-{hex}")
}

/// A Windows `\\?\C:\...` path as `C:\...`. A no-op everywhere else.
fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path.to_path_buf(),
    }
}

/// The cache root platformdirs gives `repo-rules`.
///
/// Confirm the Windows nesting against platformdirs itself before trusting this
/// on Windows; the Unix arms are the documented XDG and Apple locations.
fn cache_root() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        devkit_common::paths::home().join("Library/Caches/repo-rules")
    }
    #[cfg(target_os = "windows")]
    {
        match std::env::var_os("LOCALAPPDATA") {
            Some(x) if !x.is_empty() => PathBuf::from(x).join("repo-rules\\repo-rules\\Cache"),
            _ => devkit_common::paths::home().join("AppData\\Local\\repo-rules\\repo-rules\\Cache"),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        match std::env::var_os("XDG_CACHE_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x).join("repo-rules"),
            _ => devkit_common::paths::home().join(".cache/repo-rules"),
        }
    }
}

/// Where the extractor would have written this repository's index.
pub fn default_index_path(repo: &Path) -> PathBuf {
    cache_root().join(cache_dir_name(repo)).join("index.json")
}

/// The index at `path`, or `None`. A file that exists and does not parse earns
/// one stderr line: that is a breakage rather than an absence, and every other
/// failure is silence.
pub fn load(path: &Path) -> Option<RuleIndex> {
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&raw) {
        Ok(index) => Some(index),
        Err(e) => {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("devkit: rules index at {} did not parse: {e}\n", path.display()),
            );
            None
        }
    }
}
```

If `devkit_common::paths::home` is private, make it `pub` in `crates/devkit-common/src/paths.rs` with a doc comment saying the rules cache path needs it to reach a non-XDG location.

- [ ] **Step 4: Declare the module**

In `crates/devkit-rules/src/lib.rs`, add `pub mod index;`.

- [ ] **Step 5: Run the tests and watch them pass**

Run: `cargo nextest run -p devkit-rules`
Expected: PASS.

- [ ] **Step 6: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run -p devkit-rules
git add crates/devkit-rules crates/devkit-common
git commit -m "$(cat <<'MSG'
feat(rules): locate and load the extractor's index

The hashed path is the main worktree, so an index built once in the main
checkout is found from every worktree. A malformed index is one stderr
line and no injection, never an error a caller could propagate.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 6: Config types

**Files:**
- Modify: `crates/devkit-config/src/lib.rs`
- Modify: `schema/devkit-config.json` (regenerated)
- Modify: `docs/configuration.md`

**Interfaces:**
- Consumes: nothing.
- Produces: `devkit_config::{RulesConfig, ContextConfig, ContextFile, FileCondition}`, reachable as `Config::rules` and `Config::context`.

- [ ] **Step 1: Write the failing doctests**

Add to `crates/devkit-config/src/lib.rs`:

```rust
/// Rule injection settings.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [rules]
/// enabled = true
/// min_severity = "should"
/// per_event_limit = 5
/// # "#).unwrap();
/// # assert!(cfg.rules.enabled);
/// # assert_eq!(cfg.rules.min_severity, "should");
/// # assert_eq!(cfg.rules.per_event_limit, 5);
/// ```
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct RulesConfig {
    /// Rule and file injection as a whole.
    pub enabled: bool,
    /// The least severe rule still injected: `must`, `should` or `can`.
    pub min_severity: String,
    /// How many rules and files one event may inject.
    pub per_event_limit: usize,
    /// A single injected file larger than this is skipped.
    pub max_file_bytes: usize,
    /// The rendered total for one event is truncated to this.
    pub max_event_bytes: usize,
    /// Where the rule index lives. Absent means the path the extractor writes.
    pub index: Option<String>,
}

impl Default for RulesConfig {
    fn default() -> Self {
        RulesConfig {
            enabled: true,
            min_severity: "should".to_string(),
            per_event_limit: 5,
            max_file_bytes: 16384,
            max_event_bytes: 65536,
            index: None,
        }
    }
}

/// Files injected into an agent's context when the condition on them holds.
///
/// ```
/// # use devkit_config::Config;
/// # let cfg = Config::parse(r#"
/// [[context.files]]
/// path = "crates/devkit-ports/AGENTS.md"
///
/// [[context.files]]
/// path = "docs/deploy.md"
/// when = { harness = ["codex"], env = { DEPLOY_TARGET = "staging" } }
/// # "#).unwrap();
/// # assert_eq!(cfg.context.files.len(), 2);
/// # assert!(cfg.context.files[0].when.is_none());
/// # assert_eq!(cfg.context.files[1].when.as_ref().unwrap().harness, vec!["codex".to_string()]);
/// ```
///
/// An entry is an array element, so a deeper `devkit.toml` replaces the whole
/// list rather than appending to it, the same as `[hooks]`.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct ContextConfig {
    pub files: Vec<ContextFile>,
}

/// One injected file.
#[derive(Debug, JsonSchema, Deserialize, Serialize)]
pub struct ContextFile {
    /// The file to inject, relative to the directory of the `devkit.toml` that
    /// declared it.
    pub path: String,
    /// When to inject it. Absent means: when the edited target is under this
    /// file's own directory.
    #[serde(default)]
    pub when: Option<FileCondition>,
}

/// Conditions on an injected file. Every condition present must hold.
#[derive(Debug, Default, JsonSchema, Deserialize, Serialize)]
#[serde(default)]
pub struct FileCondition {
    /// A glob over the repo-relative targets of the tool call.
    pub path: Option<String>,
    /// Harness names: `claude-code`, `codex`, `cursor`.
    pub harness: Vec<String>,
    /// Environment variables in the hook process. An empty value means
    /// "present with any value".
    pub env: std::collections::BTreeMap<String, String>,
}
```

Add the fields to `Config`:

```rust
    #[serde(default)]
    pub rules: RulesConfig,
    #[serde(default)]
    pub context: ContextConfig,
```

- [ ] **Step 2: Run the doctests and watch them fail**

Run: `cargo test -p devkit-config --doc`
Expected: FAIL, `cfg.rules` does not exist.

- [ ] **Step 3: Check the merge behaviour**

Read how `Config` merges layers in `crates/devkit-config/src/lib.rs`. `ContextConfig.files` is a `Vec`, so it must replace rather than append, matching `HooksConfig`. If the merge is field-by-field over the deserialized structs, a `Vec` already replaces and there is nothing to add. Confirm by reading, and add a merge test if the mechanism is not obvious from the types.

- [ ] **Step 4: Run the doctests and watch them pass**

Run: `cargo test -p devkit-config --doc`
Expected: PASS.

- [ ] **Step 5: Regenerate the schema**

```bash
DEVKIT_UPDATE_SCHEMA=1 cargo test -p devkit --test config_schema
git diff --stat schema/devkit-config.json
```

Expected: `schema/devkit-config.json` gains `rules` and `context`.

- [ ] **Step 6: Document the keys**

Add a `[rules]` and a `[[context.files]]` section to `docs/configuration.md`, following the shape of the sections already there. Describe what each key does, not its current default value.

- [ ] **Step 7: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast && cargo test --workspace --doc
git add crates/devkit-config schema docs/configuration.md
git commit -m "$(cat <<'MSG'
feat(config): add [rules] and [[context.files]]

An entry with no condition injects when the edited target is under that
file's own directory, so a subdirectory AGENTS.md is a one-line entry.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 7: Conditions and rendering

**Files:**
- Create: `crates/devkit-rules/src/context.rs`
- Create: `crates/devkit-rules/src/render.rs`
- Modify: `crates/devkit-rules/src/lib.rs`

**Interfaces:**
- Consumes: `devkit_config::{ContextFile, FileCondition}`, `model::Rule`.
- Produces: `context::{Subject, fires, read_capped}` and `render::block`. `Subject` has `targets: Vec<String>` (repo-relative), `harness: Option<String>`, `root: PathBuf`. `fires(&ContextFile, &Path, &Subject) -> bool` where the `&Path` is the declaring layer's directory. `block(&[&Rule], &[(String, String)], usize) -> String`.

- [ ] **Step 1: Write the failing tests**

`crates/devkit-rules/src/context.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use devkit_config::{ContextFile, FileCondition};

    fn subject(targets: &[&str]) -> Subject {
        Subject {
            root: PathBuf::from("/repo"),
            targets: targets.iter().map(|t| (*t).to_string()).collect(),
            harness: Some("codex".to_string()),
        }
    }

    #[test]
    fn no_condition_fires_for_a_target_under_the_files_own_directory() {
        let entry = ContextFile { path: "crates/foo/AGENTS.md".to_string(), when: None };
        let layer = Path::new("/repo");
        assert!(fires(&entry, layer, &subject(&["crates/foo/src/a.rs"])));
        assert!(fires(&entry, layer, &subject(&["crates/foo/AGENTS.md"])));
        assert!(!fires(&entry, layer, &subject(&["crates/bar/src/a.rs"])));
    }

    #[test]
    fn a_path_at_the_repo_root_fires_for_every_target() {
        let entry = ContextFile { path: "AGENTS.md".to_string(), when: None };
        assert!(fires(&entry, Path::new("/repo"), &subject(&["anywhere/x.rs"])));
    }

    #[test]
    fn the_layers_directory_anchors_a_relative_path() {
        let entry = ContextFile { path: "AGENTS.md".to_string(), when: None };
        let layer = Path::new("/repo/crates/foo");
        assert!(fires(&entry, layer, &subject(&["crates/foo/src/a.rs"])));
        assert!(!fires(&entry, layer, &subject(&["crates/bar/src/a.rs"])));
    }

    #[test]
    fn a_harness_condition_gates_on_the_calling_harness() {
        let entry = ContextFile {
            path: "docs/x.md".to_string(),
            when: Some(FileCondition { harness: vec!["claude-code".to_string()], ..Default::default() }),
        };
        assert!(!fires(&entry, Path::new("/repo"), &subject(&["a.rs"])));
    }

    #[test]
    fn an_env_condition_accepts_presence_or_a_value() {
        // SAFETY: single-threaded test, and the variable is unique to it.
        unsafe { std::env::set_var("DEVKIT_RULES_TEST_ENV", "staging") };
        let present = FileCondition {
            env: [("DEVKIT_RULES_TEST_ENV".to_string(), String::new())].into(),
            ..Default::default()
        };
        let exact = FileCondition {
            env: [("DEVKIT_RULES_TEST_ENV".to_string(), "staging".to_string())].into(),
            ..Default::default()
        };
        let wrong = FileCondition {
            env: [("DEVKIT_RULES_TEST_ENV".to_string(), "prod".to_string())].into(),
            ..Default::default()
        };
        let entry = |w| ContextFile { path: "docs/x.md".to_string(), when: Some(w) };
        assert!(fires(&entry(present), Path::new("/repo"), &subject(&["a.rs"])));
        assert!(fires(&entry(exact), Path::new("/repo"), &subject(&["a.rs"])));
        assert!(!fires(&entry(wrong), Path::new("/repo"), &subject(&["a.rs"])));
        unsafe { std::env::remove_var("DEVKIT_RULES_TEST_ENV") };
    }

    /// Review Focus 3: a directory, a missing file and invalid UTF-8 each read
    /// as nothing, and none of them is an error the caller has to handle.
    #[test]
    fn an_unreadable_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_capped(dir.path(), 4096), None, "a directory");
        assert_eq!(read_capped(&dir.path().join("absent.md"), 4096), None, "missing");

        let binary = dir.path().join("binary.md");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(read_capped(&binary, 4096), None, "invalid utf-8");

        let big = dir.path().join("big.md");
        std::fs::write(&big, "x".repeat(100)).unwrap();
        assert_eq!(read_capped(&big, 10), None, "over the cap");
        assert_eq!(read_capped(&big, 200).as_deref(), Some("x".repeat(100).as_str()));
    }
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p devkit-rules context`
Expected: FAIL, `Subject` is not defined.

- [ ] **Step 3: Write the conditions**

`crates/devkit-rules/src/context.rs`, above the test module:

```rust
//! Whether a configured file injects for this tool call, and reading it.

use std::path::{Path, PathBuf};

use devkit_config::ContextFile;

use crate::query::{governs, relativize};

/// What a tool call is about to do, as the matcher sees it.
#[derive(Debug, Clone)]
pub struct Subject {
    /// The checkout root every target is relative to.
    pub root: PathBuf,
    /// Repo-relative, '/'-separated targets of this call.
    pub targets: Vec<String>,
    /// The calling harness, as its manifest declares it.
    pub harness: Option<String>,
}

/// Whether `entry`, declared by a `devkit.toml` in `layer_dir`, injects for
/// `subject`. Every condition present must hold.
pub fn fires(entry: &ContextFile, layer_dir: &Path, subject: &Subject) -> bool {
    let Some(rel) = relativize(&subject.root, &layer_dir.join(&entry.path)) else {
        return false;
    };
    let Some(when) = &entry.when else {
        let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
        return subject.targets.iter().any(|t| governs(dir, t));
    };
    if !when.harness.is_empty()
        && !subject.harness.as_ref().is_some_and(|h| when.harness.contains(h))
    {
        return false;
    }
    for (key, want) in &when.env {
        match std::env::var(key) {
            Ok(got) if want.is_empty() || &got == want => {}
            _ => return false,
        }
    }
    if let Some(pattern) = &when.path {
        let Ok(glob) = glob::Pattern::new(pattern) else {
            return false;
        };
        if !subject.targets.iter().any(|t| glob.matches(t)) {
            return false;
        }
    }
    true
}

/// The file's text, or `None` when it is missing, unreadable, not UTF-8, or
/// over `cap` bytes. Every one of those is an absence rather than an error: an
/// injected file is an improvement to a tool call, never a precondition of it.
pub fn read_capped(path: &Path, cap: usize) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() as usize > cap {
        return None;
    }
    std::fs::read_to_string(path).ok()
}
```

- [ ] **Step 4: Write the failing render test**

`crates/devkit-rules/src/render.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RuleIndex;

    fn fixture() -> RuleIndex {
        serde_json::from_str(include_str!("../tests/fixtures/index.json")).unwrap()
    }

    #[test]
    fn a_block_names_each_rule_and_its_source() {
        let index = fixture();
        let rules: Vec<&_> = index.rules.iter().take(1).collect();
        let text = block(&rules, &[], 4096);
        assert!(text.contains("Root must"), "{text}");
        assert!(text.contains("AGENTS.md"), "the source file: {text}");
    }

    #[test]
    fn a_block_with_nothing_in_it_is_empty() {
        assert_eq!(block(&[], &[], 4096), "");
    }

    #[test]
    fn a_block_over_the_cap_is_truncated_and_says_so() {
        let index = fixture();
        let rules: Vec<&_> = index.rules.iter().collect();
        let text = block(&rules, &[], 120);
        assert!(text.len() <= 120 + TRUNCATION_NOTE.len(), "len {}", text.len());
        assert!(text.ends_with(TRUNCATION_NOTE), "{text}");
    }

    #[test]
    fn a_file_dump_carries_its_path_and_body() {
        let files = [("crates/foo/AGENTS.md".to_string(), "be careful".to_string())];
        let text = block(&[], &files, 4096);
        assert!(text.contains("crates/foo/AGENTS.md"), "{text}");
        assert!(text.contains("be careful"), "{text}");
    }
}
```

- [ ] **Step 5: Run it and watch it fail**

Run: `cargo nextest run -p devkit-rules render`
Expected: FAIL, `block` is not defined.

- [ ] **Step 6: Write the renderer**

`crates/devkit-rules/src/render.rs`, above the test module:

```rust
//! One markdown block per injection event.

use crate::model::Rule;

pub const TRUNCATION_NOTE: &str = "\n\n(truncated)";

/// The rules and files as one block, truncated to `cap` bytes at a character
/// boundary. Empty when there is nothing to say, which the caller reads as
/// "emit nothing" rather than as an empty context block.
pub fn block(rules: &[&Rule], files: &[(String, String)], cap: usize) -> String {
    if rules.is_empty() && files.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if !rules.is_empty() {
        out.push_str("## Rules for the files you are editing\n\n");
        for rule in rules {
            out.push_str(&format!(
                "- **{}** ({}) {} (from {})\n",
                rule.title,
                rule.severity_raw,
                rule.description,
                rule.source_file
            ));
        }
    }
    for (path, body) in files {
        out.push_str(&format!("\n## {path}\n\n{body}\n"));
    }
    if out.len() > cap {
        let mut end = cap;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str(TRUNCATION_NOTE);
    }
    out
}
```

- [ ] **Step 7: Declare the modules**

In `crates/devkit-rules/src/lib.rs`, add `pub mod context;` and `pub mod render;`.

- [ ] **Step 8: Run the tests and watch them pass**

Run: `cargo nextest run -p devkit-rules`
Expected: PASS.

- [ ] **Step 9: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run -p devkit-rules
git add crates/devkit-rules
git commit -m "$(cat <<'MSG'
feat(rules): evaluate file conditions and render a block

An unreadable, oversized or non-UTF-8 file reads as an absence, so one
bad entry never costs the event its other injections.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 8: `devkit rules query` and `devkit rules stats`

**Files:**
- Create: `src/bin/devkit/rules.rs`
- Modify: `src/bin/devkit/main.rs`
- Modify: `Cargo.toml` (root package depends on `devkit-rules`)
- Modify: `docs/commands.md`
- Create: `tests/rules_cli.rs`

**Interfaces:**
- Consumes: `devkit_rules::{index, query, model}`.
- Produces: `rules::{RulesCli, run}`, dispatched from `Commands::Rules`.

- [ ] **Step 1: Write the failing test**

`tests/rules_cli.rs`:

```rust
//! `devkit rules` against an index passed by path, so no cache lookup is
//! involved and the test is hermetic.

use std::process::Command;

fn devkit() -> Command {
    Command::new(env!("CARGO_BIN_EXE_devkit"))
}

fn fixture_index(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("index.json");
    std::fs::copy("crates/devkit-rules/tests/fixtures/index.json", &path).unwrap();
    path
}

#[test]
fn query_filters_by_path_and_prints_json() {
    let dir = tempfile::tempdir().unwrap();
    let index = fixture_index(dir.path());
    let out = devkit()
        .args(["rules", "query", index.to_str().unwrap()])
        .args(["--path", "crates/foo/src/a.rs", "--format", "json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("r-foo-should"), "{body}");
    assert!(!body.contains("r-bad-severity"), "off-vocabulary rule: {body}");
}

#[test]
fn stats_reports_counts_and_breakdowns() {
    let dir = tempfile::tempdir().unwrap();
    let index = fixture_index(dir.path());
    let out = devkit()
        .args(["rules", "stats", index.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("rules"), "{body}");
    assert!(body.contains("must"), "the severity breakdown: {body}");
}

#[test]
fn a_missing_index_exits_nonzero_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let out = devkit()
        .args(["rules", "stats", dir.path().join("absent.json").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no rules index"));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run -p devkit --test rules_cli`
Expected: FAIL, `rules` is not a devkit subcommand.

- [ ] **Step 3: Write the verb**

`src/bin/devkit/rules.rs`:

```rust
//! `devkit rules`: read the rule index the hooks inject from.
//!
//! The filters mirror `repo-rules-agent query` so the two agree on what a given
//! query means, with `--min-severity` added: the hook asks for a floor, and a
//! flag the hook uses is a flag a person can reproduce by hand.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use devkit_common::git::Checkout;
use devkit_rules::{index, model::RuleIndex, query, vocab};

#[derive(Args)]
pub struct RulesCli {
    #[command(subcommand)]
    pub command: RulesCommand,
}

#[derive(Subcommand)]
pub enum RulesCommand {
    /// Print the rules matching a filter.
    Query(QueryArgs),
    /// Summarize an index.
    Stats(StatsArgs),
    /// Print the session-start context block.
    Context(ContextArgs),
}

#[derive(Args)]
pub struct QueryArgs {
    /// The index file. Defaults to the one built for this checkout.
    pub index_path: Option<PathBuf>,
    #[arg(long, short = 't')]
    pub task: Option<String>,
    #[arg(long = "lang", short = 'l')]
    pub language: Option<String>,
    #[arg(long, short = 's')]
    pub scope: Option<String>,
    /// Exact severity: must, should or can.
    #[arg(long)]
    pub severity: Option<String>,
    /// Least severe value still printed.
    #[arg(long)]
    pub min_severity: Option<String>,
    /// Keep repo-wide rules plus those governing this path. Repeatable.
    #[arg(long = "path", short = 'p')]
    pub paths: Vec<String>,
    /// Rank rules about this topic first. Repeatable.
    #[arg(long = "topic")]
    pub topics: Vec<String>,
    #[arg(long, short = 'n')]
    pub limit: Option<usize>,
    #[arg(long, short = 'f', value_enum, default_value_t = Format::Table)]
    pub format: Format,
}

#[derive(Args)]
pub struct StatsArgs {
    pub index_path: Option<PathBuf>,
}

#[derive(Args)]
pub struct ContextArgs {
    /// Emit inside the JSON envelope Codex and Cursor read, rather than plain.
    #[arg(long)]
    pub additional_context: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Table,
    Json,
    Prompt,
}

/// The index for this checkout, or the one named. Errors name the path tried,
/// because a `devkit rules` run is a person asking a question and silence would
/// read as "no rules" rather than "no index".
fn load_or_default(explicit: Option<PathBuf>) -> Result<(PathBuf, RuleIndex)> {
    let path = match explicit {
        Some(p) => p,
        None => {
            let cwd = std::env::current_dir().context("getting current dir")?;
            let checkout = Checkout::at(&cwd);
            let repo = checkout
                .main_worktree()
                .map(|p| p.to_path_buf())
                .unwrap_or(cwd);
            index::default_index_path(&repo)
        }
    };
    let loaded = index::load(&path)
        .with_context(|| format!("no rules index at {}", path.display()))?;
    Ok((path, loaded))
}

pub fn run(cli: RulesCli) -> Result<()> {
    match cli.command {
        RulesCommand::Query(args) => query_cmd(args),
        RulesCommand::Stats(args) => stats_cmd(args),
        RulesCommand::Context(args) => context_cmd(args),
    }
}
```

Then the two commands:

```rust
/// A vocabulary value off the command line, with the accepted values named on
/// failure. A person mistyping `--severity` gets the list, not a parse error.
fn parse_vocab<T: std::str::FromStr>(value: Option<&str>, accepted: &str) -> Result<Option<T>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    raw.parse()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("unknown value {raw:?}; accepted: {accepted}"))
}

fn query_cmd(args: QueryArgs) -> Result<()> {
    let (_, index) = load_or_default(args.index_path)?;
    let filter = query::Filter {
        task: parse_vocab(args.task.as_deref(), "code-review, code-generation, code-questions")?,
        language: args.language.map(|l| vocab::canonical_language(&l)),
        scope: parse_vocab(args.scope.as_deref(), "repo, directory, file-pattern")?,
        severity: parse_vocab(args.severity.as_deref(), "must, should, can")?,
        min_severity: parse_vocab(args.min_severity.as_deref(), "must, should, can")?,
        paths: args.paths,
    };
    let mut rules = query::rank(&index, query::matching(&index, &filter), &args.topics);
    if let Some(limit) = args.limit.filter(|n| *n > 0) {
        rules.truncate(limit);
    }
    match args.format {
        Format::Json => println!("{}", serde_json::to_string_pretty(&rules)?),
        Format::Prompt => print!("{}", devkit_rules::render::block(&rules, &[], usize::MAX)),
        Format::Table => {
            let mut table = devkit_common::ui::table(&["SEVERITY", "DIRECTORY", "TITLE", "SOURCE"]);
            for rule in &rules {
                let directory = if rule.directory.is_empty() { "." } else { &rule.directory };
                table.add_row([&rule.severity_raw, directory, &rule.title, &rule.source_file]);
            }
            println!("{table}");
        }
    }
    Ok(())
}

fn stats_cmd(args: StatsArgs) -> Result<()> {
    use std::collections::BTreeMap;

    let (path, index) = load_or_default(args.index_path)?;
    println!("index  {}", path.display());
    println!("repo   {}", index.repo);
    println!("\n{} rules across {} files\n", index.rules.len(), index.files.len());

    let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for rule in &index.rules {
        *per_file.entry(rule.source_file.as_str()).or_default() += 1;
    }
    let mut table = devkit_common::ui::table(&["FILE", "RULES"]);
    for file in &index.files {
        let count = per_file.get(file.path.as_str()).copied().unwrap_or(0);
        table.add_row([file.path.clone(), count.to_string()]);
    }
    println!("{table}");

    let tally = |counts: BTreeMap<String, usize>| -> String {
        let mut rows: Vec<_> = counts.into_iter().collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        rows.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
    };
    let count_by = |f: &dyn Fn(&devkit_rules::model::Rule) -> Vec<String>| {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for rule in &index.rules {
            for key in f(rule) {
                *counts.entry(key).or_default() += 1;
            }
        }
        counts
    };
    println!("\nby severity:  {}", tally(count_by(&|r| vec![r.severity_raw.clone()])));
    println!("by task:      {}", tally(count_by(&|r| r.tasks.clone())));
    println!("by language:  {}", tally(count_by(&|r| r.languages_canonical())));
    println!("by directory: {}", tally(count_by(&|r| {
        vec![if r.directory.is_empty() { "(repo root)".to_string() } else { r.directory.clone() }]
    })));
    let topics = tally(count_by(&|r| r.topics_canonical()));
    if !topics.is_empty() {
        println!("by topic:     {topics}");
    }

    let failed: Vec<&devkit_rules::model::RuleFile> =
        index.files.iter().filter(|f| !f.errors.is_empty()).collect();
    if !failed.is_empty() {
        println!("\n{} files had failed extractions:", failed.len());
        for file in failed {
            for error in &file.errors {
                println!("  {}: {error}", file.path);
            }
        }
    }
    Ok(())
}
```

`Format::Json` serializes `Rule`, so add `Serialize` to its derive list in
`crates/devkit-rules/src/model.rs` alongside `Deserialize`.

Leave `context_cmd` as `todo!()`; Task 11 writes it.

- [ ] **Step 4: Register the subcommand**

In `src/bin/devkit/main.rs`, add `mod rules;`, a variant

```rust
    #[command(display_name = "devkit rules")]
    Rules(rules::RulesCli),
```

and its dispatch arm `Commands::Rules(c) => rules::run(c),`.

In the root `Cargo.toml`, add `devkit-rules.workspace = true` to `[dependencies]`.

- [ ] **Step 5: Run the test and watch it pass**

Run: `cargo nextest run -p devkit --test rules_cli`
Expected: PASS for the `query` and `stats` tests.

- [ ] **Step 6: Document the verb**

Add a `devkit rules` section to `docs/commands.md` describing `query` and `stats` and their flags, in the shape the sections around it use.

- [ ] **Step 7: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast
git add src/bin/devkit Cargo.toml tests/rules_cli.rs docs/commands.md
git commit -m "$(cat <<'MSG'
feat(rules): add devkit rules query and stats

The filters mirror the extractor's own so the two agree on what a query
means, plus --min-severity, which is the floor the hook asks for.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 9: One emission site in the edit guard

A pure refactor with no behaviour change, pinned by a characterization test written first. `claim` currently prints its own deny envelope and returns a `Vec<String>` to `guard`, which then falls through to the flush and the record exactly as an allow does. Task 10 attaches the rules stage to that fall-through, so the fall-through has to stop being reachable after a denial.

**Files:**
- Modify: `src/bin/devkit/hook/edit.rs`
- Modify: `src/bin/devkit/hook/shell.rs` (widen `print_envelope` to `pub(super)`; `edit.rs` also needs `shell` added to its `use super::{...}` list)
- Create: `tests/hook_edit_verdict.rs`

**Interfaces:**
- Consumes: nothing new.
- Produces: `enum WriteOutcome { Allow, Deny { envelope: serde_json::Value, reasons: Vec<String> } }` private to `edit.rs`. `claim` returns `WriteOutcome` and prints nothing. `guard` holds the edit path's only `print_envelope` call.

- [ ] **Step 1: Write the characterization test**

`tests/hook_edit_verdict.rs`:

```rust
//! The edit guard's stdout, pinned.
//!
//! Task 10 adds a rules stage to this hook, and this file is what keeps that
//! stage off the deny path. A second JSON object appended after a denial makes
//! the whole of stdout unparseable, and a harness that cannot parse a hook's
//! stdout treats it as plain text carrying no decision: the denial is lost and
//! the write proceeds.

#[path = "common/testenv.rs"]
mod testenv;

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

/// A private git project with the write harness enforced.
pub fn project() -> tempfile::TempDir {
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        "[harness]\nenforce_writes = true\n",
    )
    .unwrap();
    p
}

/// `devkit hook pre-tool-use` against a private HOME and state home, so a run
/// started from inside a coding agent resolves the same holder CI does.
pub fn run_hook(project: &Path, state: &Path, payload: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(["hook", "pre-tool-use", "--harness", "claude-code"])
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG")
        .env_remove("DEVKIT_ENFORCE_WRITES")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    testenv::scrub_identity(&mut cmd);
    let mut child = cmd.spawn().expect("spawn the devkit hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

/// Claim `path` for `holder`, the way another session would have. `devkit locks
/// acquire` is the same verb `lockm acquire` reaches, so no shim is needed.
pub fn hold(project: &Path, state: &Path, path: &str, holder: &str) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_devkit"));
    cmd.args(["locks", "acquire", path, "--as", holder])
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1");
    testenv::scrub_identity(&mut cmd);
    let out = cmd.output().expect("spawn devkit locks acquire");
    assert!(out.status.success(), "the other holder should acquire");
}

pub fn write_payload(session: &str, agent: Option<&str>, cwd: &Path, target: &str) -> String {
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Write",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_input": { "file_path": target }
    });
    if let Some(agent) = agent {
        payload["agent_id"] = serde_json::Value::String(agent.to_string());
    }
    payload.to_string()
}

/// Stdout as exactly one JSON object, or `None` when it is empty. Parsing is
/// the assertion: two objects would fail here, which is the whole point.
pub fn one_object(out: &Output) -> Option<serde_json::Value> {
    assert_eq!(
        out.status.code(),
        Some(0),
        "the guard always exits 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.trim().is_empty() {
        return None;
    }
    Some(serde_json::from_str(&stdout).expect("stdout parses as exactly one JSON object"))
}

#[test]
fn a_conflicting_write_denies_with_one_object() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    hold(proj.path(), state.path(), "src/a.rs", "other-session");

    let payload = write_payload("S", None, proj.path(), "src/a.rs");
    let out = run_hook(proj.path(), state.path(), &payload);

    let v = one_object(&out).expect("a conflict denies");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[test]
fn an_unconflicted_write_emits_nothing() {
    let proj = project();
    let state = tempfile::tempdir().unwrap();
    let payload = write_payload("S", None, proj.path(), "src/a.rs");
    let out = run_hook(proj.path(), state.path(), &payload);
    assert_eq!(one_object(&out), None, "an allow is silent");
}
```

- [ ] **Step 2: Run them and watch them pass**

Run: `cargo nextest run -p devkit --test hook_edit_verdict`
Expected: PASS, both. These pin current behaviour before the refactor; a failure here means the setup is wrong, not that the code is.

- [ ] **Step 3: Add the verdict type**

In `src/bin/devkit/hook/edit.rs`, above `claim`:

```rust
/// What the write stage decided.
///
/// `claim` returns this rather than printing, so `guard` owns the edit path's
/// single emission site. Anything appended to stdout after a denial makes the
/// whole output unparseable, and a harness that cannot parse a hook's stdout
/// proceeds with the call it was asked to gate.
enum WriteOutcome {
    Allow,
    Deny {
        envelope: serde_json::Value,
        reasons: Vec<String>,
    },
}
```

- [ ] **Step 4: Make `claim` return it**

Change `claim`'s signature to `-> WriteOutcome`. Then, in its body:

- the enforcement-disabled early return becomes `return WriteOutcome::Allow;`
- the registry-error arm becomes, in place of its `println!` and `return`:

```rust
            Err(e) => {
                // fail closed: a registry error must not silently reopen the
                // window
                let message = format!("devkit write-harness: registry error (fail-closed): {e:#}");
                return WriteOutcome::Deny {
                    envelope: hook::deny_json(&message),
                    reasons: vec![message],
                };
            }
```

- the `conflicts.is_empty()` early return becomes `return WriteOutcome::Allow;`
- the tail becomes:

```rust
    let envelope = conflict_envelope(&conflicts);
    let reason = envelope["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    WriteOutcome::Deny { envelope, reasons: vec![reason] }
```

- [ ] **Step 5: Emit once in `guard`**

In `guard`, widen the `match hook::parse_write(payload)` block so every arm
yields `(targets, holder, reasons, outcome)`:

- the `LockAction::Write` arm passes through the targets and holder it already
  binds, and takes `reasons` and `outcome` from `claim`
- the `Unusable` arm yields empty targets, `String::new()` for the holder,
  `vec![message]` for the reasons, and a `WriteOutcome::Deny` built from
  `hook::deny_json(&message)` when `hook::enforcement_enabled_in` is true or
  `WriteOutcome::Allow` when it is not
- the non-writing arm returns early, as it does now

`reasons` stays separate from `outcome` rather than being derived from it,
because one log record would otherwise move. Today the `Unusable` arm populates
`blocks` regardless of enforcement, so the harness log records `Decision::Deny`
even when nothing reached stdout. Deriving `blocks` from the outcome would flip
that record to `Allow`. No test pins it, which is exactly why it is worth
writing down.

Then, before the existing flush:

```rust
    let blocks = reasons;
    if let WriteOutcome::Deny { envelope, .. } = &outcome {
        shell::print_envelope(envelope);
    }
```

Everything after this is unchanged: the flush, the log settings, and the record
all run on both paths as they do now.

In `src/bin/devkit/hook/shell.rs`, change `fn print_envelope` to `pub(super) fn print_envelope`.

- [ ] **Step 6: Run the characterization test and watch it still pass**

Run: `cargo nextest run -p devkit --test hook_edit_verdict`
Expected: PASS, unchanged. Byte-identical stdout is the point of the refactor.

- [ ] **Step 7: Run the whole gate**

Run: `cargo nextest run --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 8: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast
git add src/bin/devkit/hook tests/hook_edit_verdict.rs
git commit -m "$(cat <<'MSG'
refactor(hook): give the edit guard one emission site

claim printed its own envelope and returned into a fall-through the allow
path shares, so anything appended there would follow a denial onto
stdout. It returns a verdict now and guard emits.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 10: The pre-tool-use rules stage

**Files:**
- Create: `src/bin/devkit/hook/rules.rs`
- Modify: `src/bin/devkit/hook/mod.rs`, `src/bin/devkit/hook/edit.rs`
- Modify: `tests/hook_edit_verdict.rs`

**Interfaces:**
- Consumes: `devkit_rules::{context::{Subject, fires, read_capped}, index, query, render, vocab}`, `devkit_config::RulesConfig`, `WriteOutcome::Allow` from Task 9.
- Produces: `hook::rules::{inject, fired_path, clear_for_holder}`. `inject(payload: &Value, checkout: &Checkout, cwd: &Path, declared: Option<Harness>, targets: &[String], holder: &str)` returns `()`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/hook_edit_verdict.rs`:

```rust
/// A project whose `[rules]` are on, carrying an index at a known path and a
/// `[[context.files]]` entry under `crates/foo`.
fn rules_project(state: &Path) -> tempfile::TempDir {
    let p = project();
    let index = p.path().join("index.json");
    std::fs::copy("crates/devkit-rules/tests/fixtures/index.json", &index).unwrap();
    std::fs::create_dir_all(p.path().join("crates/foo")).unwrap();
    std::fs::write(p.path().join("crates/foo/AGENTS.md"), "foo house rules").unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        format!(
            // A literal string, not a basic one: a Windows path carries
            // backslashes, and `"C:\\Users..."` is an invalid escape.
            "[harness]\nenforce_writes = true\n\n\
             [rules]\nenabled = true\nmin_severity = \"should\"\n\
             index = '{}'\n\n\
             [[context.files]]\npath = \"crates/foo/AGENTS.md\"\n",
            index.display()
        ),
    )
    .unwrap();
    let _ = state;
    p
}

fn injected_text(out: &Output) -> String {
    let v = one_object(out).expect("an allow with matching rules emits");
    assert!(
        v["hookSpecificOutput"].get("permissionDecision").is_none(),
        "the rules stage never carries a decision: {v}"
    );
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext")
        .to_string()
}

/// The deny path, byte-identical, with a rule *and* a file both matching. A
/// test over an unmatched deny would pass without ever reaching the code that
/// could break this.
#[test]
fn a_denial_emits_no_rules_and_stamps_nothing() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    hold(proj.path(), state.path(), "crates/foo/src/a.rs", "other-session");

    let payload = write_payload("S", None, proj.path(), "crates/foo/src/a.rs");
    let out = run_hook(proj.path(), state.path(), &payload);

    let v = one_object(&out).expect("a conflict denies");
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    let text = serde_json::to_string(&v).unwrap();
    assert!(!text.contains("Foo should"), "no rule rides along: {text}");
    assert!(!text.contains("foo house rules"), "no file rides along: {text}");

    // Byte-for-byte against the same conflict with rules switched off: the deny
    // path must be indistinguishable from what it emitted before this feature.
    let off_state = tempfile::tempdir().unwrap();
    let off = rules_project(off_state.path());
    std::fs::write(
        off.path().join("devkit.toml"),
        "[harness]\nenforce_writes = true\n\n[rules]\nenabled = false\n",
    )
    .unwrap();
    hold(off.path(), off_state.path(), "crates/foo/src/a.rs", "other-session");
    let baseline = run_hook(
        off.path(),
        off_state.path(),
        &write_payload("S", None, off.path(), "crates/foo/src/a.rs"),
    );
    // Each output is normalized against its own project root, so the two are
    // compared on content rather than on which tempdir produced them.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).replace(proj.path().to_string_lossy().as_ref(), ""),
        String::from_utf8_lossy(&baseline.stdout).replace(off.path().to_string_lossy().as_ref(), ""),
        "the deny envelope is unchanged by the rules feature"
    );

    let fired = state.path().join("devkit/rules");
    assert!(
        !fired.exists() || std::fs::read_dir(&fired).unwrap().next().is_none(),
        "a denial stamps nothing, so the retry carries the rules"
    );
}

#[test]
fn an_allow_emits_the_matching_rule_once_per_holder() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let payload = write_payload("S", None, proj.path(), "crates/foo/src/a.rs");

    let first = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(first.contains("Foo should"), "the directory rule: {first}");
    assert!(first.contains("Root must"), "the repo rule: {first}");
    assert!(!first.contains("Foo can"), "below the floor: {first}");
    assert!(first.contains("foo house rules"), "the file dump: {first}");

    let second = run_hook(proj.path(), state.path(), &payload);
    assert_eq!(one_object(&second), None, "the second call is silent");
}

#[test]
fn a_subagent_gets_a_rule_its_parent_already_fired() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let target = "crates/foo/src/a.rs";

    let parent = write_payload("S", None, proj.path(), target);
    injected_text(&run_hook(proj.path(), state.path(), &parent));

    let child = write_payload("S", Some("a1"), proj.path(), target);
    let text = injected_text(&run_hook(proj.path(), state.path(), &child));
    assert!(
        text.contains("Foo should"),
        "the subagent's context never held what the parent was shown: {text}"
    );
}

/// An `apply_patch` envelope, the only multi-target write in devkit's model:
/// every other write tool names one `tool_input.file_path`.
fn patch_payload(session: &str, cwd: Option<&Path>, targets: &[&str]) -> String {
    let mut patch = String::from("*** Begin Patch\n");
    for target in targets {
        patch.push_str(&format!("*** Update File: {target}\n"));
    }
    patch.push_str("*** End Patch\n");
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "apply_patch",
        "session_id": session,
        "tool_input": { "command": patch }
    });
    if let Some(cwd) = cwd {
        payload["cwd"] = serde_json::Value::String(cwd.to_string_lossy().into_owned());
    }
    payload.to_string()
}

/// Review Focus 2: no `cwd` key, so a relative target cannot be resolved.
/// `apply_patch_paths` takes paths verbatim, relative to the session's cwd.
#[test]
fn a_payload_without_cwd_drops_relative_targets_and_keeps_absolute_ones() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let absolute = proj.path().join("crates/foo/src/a.rs");
    let absolute = absolute.to_string_lossy().into_owned();
    let payload = patch_payload("S", None, &["relative/b.rs", &absolute]);

    let text = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(text.contains("Foo should"), "the absolute target still matches: {text}");
}

/// Review Focus 4: a parent and its subagents append to one file.
#[test]
fn a_torn_line_in_the_fired_set_does_not_suppress_the_rest() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let payload = write_payload("S", None, proj.path(), "crates/foo/src/a.rs");

    let first = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(first.contains("Foo should"));

    let dir = state.path().join("devkit/rules");
    let file = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
    let mut body = std::fs::read_to_string(&file).unwrap();
    body.push_str("\u{0}\u{1}torn\n");
    std::fs::write(&file, body).unwrap();

    let out = run_hook(proj.path(), state.path(), &payload);
    assert_eq!(one_object(&out), None, "the valid ids still suppress");
}

#[test]
fn a_multi_target_call_unions_the_rules_for_every_target() {
    let state = tempfile::tempdir().unwrap();
    let proj = rules_project(state.path());
    let payload = patch_payload(
        "S",
        Some(proj.path()),
        &["crates/foo/src/a.rs", "README.md"],
    );

    let text = injected_text(&run_hook(proj.path(), state.path(), &payload));
    assert!(text.contains("Foo should"), "the crates/foo target: {text}");
    assert!(text.contains("Root must"), "the root target: {text}");
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p devkit --test hook_edit_verdict`
Expected: FAIL. The allow cases emit nothing, so `injected_text` panics on `expect("an allow with matching rules emits")`.

- [ ] **Step 3: Write the stage**

`src/bin/devkit/hook/rules.rs`:

```rust
//! The `pre-tool-use` rules stage: the rules governing the files this call is
//! about to write.
//!
//! Four properties make this safe to hang off a permission gate, and each one
//! is the safety argument rather than a convention.
//!
//! Nothing here runs before the verdict is final. Loading config or parsing an
//! index earlier would put a fallible, slow step in front of a denial: a
//! malformed config under `?` exits non-zero, which the harness treats as a
//! non-blocking error, and a large parse can spend the manifest's timeout,
//! whose expiry also allows the call.
//!
//! `inject` returns `()`. With no `Result` and no `?` it has no path to an exit
//! code.
//!
//! It runs under `catch_unwind`, the profile `shell.rs` pins with a
//! `compile_error!` against an aborting panic strategy.
//!
//! It emits one write and flush before the log record, because the envelope has
//! to reach the harness ahead of anything that can stall on the log directory.
//!
//! The emitted object carries `additionalContext` and no permission decision. A
//! `permissionDecision` here would auto-approve writes the user would otherwise
//! be asked about.

use std::{
    collections::HashSet,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
};

use devkit_common::{git::Checkout, harness, harness::Harness, paths};
use devkit_rules::{
    context::{self, Subject},
    index,
    query::{self, Filter},
    render,
    vocab::{Severity, Task, canonical_language},
};
use serde_json::Value;

use super::shell::print_envelope;

/// The fired-set for one holder.
///
/// Hashing the complete raw holder id rather than sanitizing it: dropping
/// disallowed characters is lossy, and two holders differing only in what was
/// dropped would share one set.
pub fn fired_path(holder: &str) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    holder.hash(&mut hasher);
    paths::state_dir()
        .join("rules")
        .join(format!("{:016x}", hasher.finish()))
}

/// Forget everything a holder was shown. The release verbs call this, and they
/// already run when a session or a subagent ends; without it the directory
/// grows one file per session forever.
pub fn clear_for_holder(holder: &str) {
    let _ = std::fs::remove_file(fired_path(holder));
}

/// Record ids as injected for `holder`. `devkit rules context` calls this too,
/// so what a session-start block emitted is not emitted again by the first
/// write that happens to match it.
pub fn stamp_ids(holder: &str, ids: &[String]) {
    stamp(holder, ids);
}

/// Ids already injected for `holder`.
///
/// A parent and its subagents append to this file concurrently, so a line that
/// does not look like an id is skipped rather than trusted or fatal: a torn
/// line costs one duplicate injection, which is not worth a lock.
fn already_fired(holder: &str) -> HashSet<String> {
    std::fs::read_to_string(fired_path(holder))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        // A file entry's id is its configured path, which may hold a space or
        // non-ASCII. Only a control character marks a torn write.
        .filter(|line| !line.is_empty() && !line.chars().any(char::is_control))
        .map(str::to_string)
        .collect()
}

fn stamp(holder: &str, ids: &[String]) {
    let path = fired_path(holder);
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    for id in ids {
        let _ = writeln!(file, "{id}");
    }
}

/// The language a rule would be tagged with for this path, from its extension.
fn language_of(path: &str) -> Option<String> {
    let ext = Path::new(path).extension()?.to_str()?;
    Some(canonical_language(ext))
}

/// Inject the rules and files governing `targets`.
///
/// Every failure is silence. The caller's verdict has already been emitted, so
/// there is nothing this can report that would be worth the risk of reporting
/// it.
pub fn inject(
    payload: &Value,
    checkout: &Checkout,
    cwd: &Path,
    declared: Option<Harness>,
    targets: &[String],
    holder: &str,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(payload, checkout, cwd, declared, targets, holder);
    }));
}

fn run(
    payload: &Value,
    checkout: &Checkout,
    cwd: &Path,
    declared: Option<Harness>,
    targets: &[String],
    holder: &str,
) {
    // Every shipped manifest passes `--harness`, but the retired
    // `lockm hook pretooluse` spelling does not, and the shell path infers
    // rather than giving up. Match it.
    let Some(harness_name) = declared.or_else(|| devkit_common::harness::infer_harness(payload))
    else {
        return;
    };
    // `devkit_common::config::resolve_in` is the only permitted door to the
    // merged config: `tests/no_stray_config.rs` fails the build when anything
    // else calls `devkit_config::resolve`. `enforcement_enabled_in` reads raw
    // layer flags and never deserializes `Config`, so there is no read to share.
    let Ok((project, provenance)) = devkit_common::config::resolve_in(checkout, None, cwd) else {
        return;
    };
    let settings = &project.rules;
    if !settings.enabled {
        return;
    }
    let Some(root) = checkout.root() else {
        return;
    };

    // Both sides resolve before they compare. git reports a symlinked working
    // directory resolved, since it reads the directory rather than the spelling
    // used to reach it, so `root` is `/private/var/...` where the payload's cwd
    // is `/var/...`; `git.rs`'s own `containment` documents the same hazard. A
    // purely lexical strip would drop every target on macOS.
    let root_canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    // A target that escapes the root after normalization is dropped: a rule for
    // the repository root must not fire for a write outside the repository.
    let relative: Vec<String> = targets
        .iter()
        .map(|t| resolve(payload, t))
        .filter_map(|abs| query::relativize(&root_canon, &abs))
        .collect();
    if relative.is_empty() {
        return;
    }

    let subject = Subject {
        root: root.to_path_buf(),
        targets: relative.clone(),
        harness: Some(harness_slug(harness_name).to_string()),
    };

    let floor: Severity = settings.min_severity.parse().unwrap_or(Severity::Should);
    let fired = already_fired(holder);

    let index_path = match &settings.index {
        Some(p) => PathBuf::from(p),
        None => index::default_index_path(checkout.main_worktree().unwrap_or(root)),
    };

    // One matched set across every target, so a call touching two directories
    // gets both directories' rules.
    let mut chosen: Vec<devkit_rules::model::Rule> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(loaded) = index::load(&index_path) {
        for target in &relative {
            let filter = Filter {
                task: Some(Task::CodeGeneration),
                language: language_of(target),
                min_severity: Some(floor),
                paths: vec![target.clone()],
                ..Filter::default()
            };
            for rule in query::rank(&loaded, query::matching(&loaded, &filter), &[]) {
                if fired.contains(&rule.id) || !seen.insert(rule.id.clone()) {
                    continue;
                }
                chosen.push(rule.clone());
            }
        }
    }

    // A relative `path` anchors to the directory of the layer that declared the
    // entry. `[[context.files]]` is an array, and an array leaf replaces
    // wholesale rather than merging, so every surviving entry came from one
    // file and `origin` names it.
    let layer_dir = provenance
        .origin
        .get("context.files")
        .and_then(|f| f.parent())
        .unwrap_or(root);
    let mut files: Vec<(String, String)> = Vec::new();
    for entry in &project.context.files {
        if fired.contains(&entry.path) || !context::fires(entry, layer_dir, &subject) {
            continue;
        }
        let path = layer_dir.join(&entry.path);
        if let Some(body) = context::read_capped(&path, settings.max_file_bytes) {
            files.push((entry.path.clone(), body));
        }
    }

    let budget = settings.per_event_limit.saturating_sub(files.len());
    chosen.truncate(budget);

    let refs: Vec<&devkit_rules::model::Rule> = chosen.iter().collect();
    let block = render::block(&refs, &files, settings.max_event_bytes);
    if block.is_empty() {
        return;
    }
    let Some(envelope) = harness::warn_shell_json(harness_name, &block) else {
        return;
    };
    print_envelope(&envelope);
    let _ = std::io::stdout().flush();

    let mut ids: Vec<String> = chosen.into_iter().map(|r| r.id).collect();
    ids.extend(files.into_iter().map(|(path, _)| path));
    stamp(holder, &ids);
}

/// A payload path as an absolute one. Mirrors `edit::resolve_against`, which is
/// private to that module and runs only inside `claim`, which returns before
/// reaching it when enforcement is off.
fn resolve(payload: &Value, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(|cwd| {
            let base = Path::new(cwd);
            // The payload's cwd is the spelling the session was started with,
            // which on macOS is the symlink rather than the resolved path the
            // checkout root carries.
            std::fs::canonicalize(base)
                .unwrap_or_else(|_| base.to_path_buf())
                .join(p)
        })
        .unwrap_or_else(|| p.to_path_buf())
}

fn harness_slug(harness: Harness) -> &'static str {
    match harness {
        Harness::ClaudeCode => "claude-code",
        Harness::Codex => "codex",
        Harness::Cursor => "cursor",
    }
}
```

`resolve_in` calls `pool::configure` and may print a `baseline_path` warning on
stderr. Both are in-process and neither touches the verdict, which has already
been emitted by the time this runs.

- [ ] **Step 4: Call it from the allow arm**

In `src/bin/devkit/hook/edit.rs`, inside the `WriteOutcome::Allow` arm only, after the verdict is final and before `harness_log::record`:

```rust
        WriteOutcome::Allow => {
            rules::inject(payload, &checkout, &cwd, declared, &targets, &holder);
            Vec::new()
        }
```

Task 9 already widened the earlier match to yield `(targets, holder, reasons,
outcome)`, so `holder` is in scope here. It is `String::new()` on the `Unusable`
arm, which never reaches `WriteOutcome::Allow` with targets to match, so the
empty holder is never used.

Add `pub(crate) mod rules;` to `src/bin/devkit/hook/mod.rs`. It is crate-visible rather than private because Task 11's `devkit rules context` calls `stamp_ids` through it, so both injection points share one fired-set implementation.

- [ ] **Step 5: Clear the fired-set on release**

In `edit::release_subagent` and `edit::release_session`, after the registry release succeeds, call `rules::clear_for_holder(&holder)` for the holder each one releases.

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo nextest run -p devkit --test hook_edit_verdict`
Expected: PASS, all eight.

- [ ] **Step 7: Run the whole gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo test --workspace --doc`
Expected: PASS.

- [ ] **Step 8: Verify and commit**

```bash
devrun task fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace --no-fail-fast
git add src/bin/devkit/hook tests/hook_edit_verdict.rs
git commit -m "$(cat <<'MSG'
feat(hook): inject governing rules on an allowed write

The stage runs only once the verdict is final, returns no Result, and
emits additionalContext with no permission decision, so it has no path to
changing what the guard decided. A denial skips it and stamps nothing, so
the retry carries the rules.

The fired set is keyed by holder rather than session: a subagent's
context never held what its parent was shown.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

### Task 11: `devkit rules context` and the manifests

**Files:**
- Modify: `src/bin/devkit/rules.rs`, `src/bin/devkit/hook/mod.rs`
- Modify: `hooks/hooks.json`, `hooks/hooks-codex.json`, `hooks/hooks-cursor.json`
- Modify: `tests/rules_cli.rs`, `docs/commands.md`, `docs/agents.md`

**Interfaces:**
- Consumes: `rules::ContextArgs` from Task 8, `hook::rules::clear_for_holder` from Task 10.
- Produces: `devkit rules context [--additional-context]`.

- [ ] **Step 1: Write the failing tests**

Append to `tests/rules_cli.rs`:

```rust
fn context_project() -> (tempfile::TempDir, tempfile::TempDir) {
    let state = tempfile::tempdir().unwrap();
    let p = tempfile::tempdir().unwrap();
    devkit_common::git::Git::fixture(p.path())
        .args(["init", "-q", "-b", "main"])
        .output()
        .unwrap();
    let index = p.path().join("index.json");
    std::fs::copy("crates/devkit-rules/tests/fixtures/index.json", &index).unwrap();
    std::fs::write(
        p.path().join("devkit.toml"),
        // A literal string: a Windows path's backslashes are not escapes.
        format!("[rules]\nenabled = true\nindex = '{}'\n", index.display()),
    )
    .unwrap();
    (p, state)
}

fn run_context(project: &std::path::Path, state: &std::path::Path, args: &[&str]) -> std::process::Output {
    let mut cmd = devkit();
    cmd.args(["rules", "context"])
        .args(args)
        .current_dir(project)
        .env("HOME", state)
        .env("XDG_STATE_HOME", state)
        .env("DEVKIT_SKIP_AUTOLINK", "1")
        .env_remove("DEVKIT_CONFIG");
    testenv::scrub_identity(&mut cmd);
    cmd.output().unwrap()
}

#[test]
fn context_emits_repo_scope_must_rules_and_the_query_pointer() {
    let (proj, state) = context_project();
    let out = run_context(proj.path(), state.path(), &[]);
    assert!(out.status.success());
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("Root must"), "the repo-scope must rule: {body}");
    assert!(!body.contains("Foo should"), "not the directory rule: {body}");
    assert!(!body.contains("Review only"), "scope repo, severity must only: {body}");
    assert!(body.contains("devkit rules query --path"), "the pointer: {body}");
}

#[test]
fn context_outside_a_devkit_project_prints_nothing_and_exits_zero() {
    let state = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let out = run_context(empty.path(), state.path(), &[]);
    assert!(out.status.success(), "a session hook calls this unconditionally");
    assert!(out.stdout.is_empty(), "{:?}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn additional_context_wraps_the_block_in_the_json_envelope() {
    let (proj, state) = context_project();
    let out = run_context(proj.path(), state.path(), &["--additional-context"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext");
    assert!(text.contains("Root must"), "{text}");
}
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p devkit --test rules_cli`
Expected: FAIL, `context_cmd` is `todo!()`.

- [ ] **Step 3: Implement `context_cmd`**

Replace the `todo!()` in `src/bin/devkit/rules.rs`:

```rust
/// The session-start block: what governs this repository as a whole, plus how
/// to reach the rest.
///
/// Silence on every failure, unlike `query` and `stats`. This runs from a
/// session hook in any repository, so "no index" is the common case and an
/// error would be noise in every session that has none.
fn context_cmd(args: ContextArgs) -> Result<()> {
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    let checkout = Checkout::at(&cwd);
    let Ok((project, provenance)) = devkit_common::config::resolve_in(&checkout, None, &cwd) else {
        return Ok(());
    };
    let _ = &provenance;
    if !project.rules.enabled {
        return Ok(());
    }
    let path = match &project.rules.index {
        Some(p) => PathBuf::from(p),
        None => {
            let Some(repo) = checkout.main_worktree().or_else(|| checkout.root()) else {
                return Ok(());
            };
            index::default_index_path(repo)
        }
    };
    let Some(loaded) = index::load(&path) else {
        return Ok(());
    };
    let filter = query::Filter {
        scope: Some(vocab::Scope::Repo),
        severity: Some(vocab::Severity::Must),
        ..query::Filter::default()
    };
    let mut matched = query::rank(&loaded, query::matching(&loaded, &filter), &[]);
    matched.truncate(project.rules.per_event_limit);
    let mut text = devkit_rules::render::block(&matched, &[], project.rules.max_event_bytes);
    if text.is_empty() {
        return Ok(());
    }
    text.push_str(
        "\nThe rest of this repository's rules are reachable with \
         `devkit rules query --path <path>`.\n",
    );
    if args.additional_context {
        println!("{}", envelope(&text));
    } else {
        print!("{text}");
    }
    // A top-level session's holder is the bare session id. Without this the
    // first allowed write re-injects everything the session-start block just
    // showed the agent.
    if let Some(session) = session_id() {
        let ids: Vec<String> = matched.iter().map(|r| r.id.clone()).collect();
        crate::hook::rules::stamp_ids(&session, &ids);
    }
    Ok(())
}
```

Copy `envelope` and `session_id` from `brief.rs`, or make both `pub(crate)` and
call them. `brief.rs` already spells the Cursor and Codex fields apart and
already reads the session id off stdin behind an `is_terminal` check; one
spelling of each, not two.

The `rules_cli.rs` tests use `.output()`, which closes stdin, so `session_id`
returns `None` there and stamping is a no-op. That keeps them hermetic.

- [ ] **Step 4: Truncate the fired-set on post-compact**

In `src/bin/devkit/hook/mod.rs`, give `HookEvent::PostCompact` its own arm ahead of the record-only fall-through:

```rust
        // Compaction is what drops the injected rules out of the agent's
        // context, so clearing the set is what lets them inject again.
        HookEvent::PostCompact => with_payload(|p| {
            if let Some(session) = p.get("session_id").and_then(Value::as_str) {
                rules::clear_for_holder(session);
            }
            record_only(p, cli.event, harness);
            Ok(())
        }),
```

It writes no stdout, so it stays a silent verb.

- [ ] **Step 5: Wire the manifests**

In `hooks/hooks.json`, append to the SessionStart hooks array, after the `devkit brief` entry:

```json
          {
            "type": "command",
            "command": "devkit rules context"
          }
```

In `hooks/hooks-codex.json`, append to the default SessionStart matcher's hooks array and to the `compact` matcher's:

```json
          {
            "type": "command",
            "command": "devkit rules context --additional-context",
            "async": false,
            "timeout": 3
          }
```

The `compact` matcher gets it deliberately: post-compact clears the fired set, and the repository-scope rules are exactly what the compaction dropped.

In `hooks/hooks-cursor.json`, add the same `--additional-context` entry to its
session start block.

Claude Code needs one more. Its SessionStart matcher is `startup|resume|clear`,
not `compact`, so a compaction there clears the fired set and nothing re-injects
the repository rules until a write happens to match. Add to `hooks.json`'s
`PostCompact` hooks array, beside `devkit brief --pins-only`:

```json
          {
            "type": "command",
            "command": "devkit rules context",
            "timeout": 3,
            "statusMessage": "Re-injecting repository rules"
          }
```

Order matters within that array: `devkit hook post-compact` clears the fired set
and `devkit rules context` stamps it, so the clear must come first. Put the
`rules context` entry after the `post-compact` one. On Codex, verify the same
ordering holds between its `compact` SessionStart matcher and its `PostCompact`
block before trusting it; if `PostCompact` fires last there, it wipes the stamp
`rules context` just wrote and the next write duplicates the repository rules.

- [ ] **Step 6: Run the tests and watch them pass**

Run: `cargo nextest run -p devkit --test rules_cli`
Expected: PASS.

If `tests/hook_manifests.rs` asserts a fixed set of commands per event, update its expectations in the same commit.

- [ ] **Step 7: Document it**

Add `devkit rules context` to the `devkit rules` section of `docs/commands.md`, and note in `docs/agents.md` that SessionStart carries a rules block when an index exists.

- [ ] **Step 8: Run the whole gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo test --workspace --doc && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, all three.

- [ ] **Step 9: Verify and commit**

```bash
devrun task fmt
git add src/bin/devkit hooks docs tests
git commit -m "$(cat <<'MSG'
feat(rules): inject repo-scope rules at session start

A verb rather than a hook event, because only pre-tool-use may write
stdout from a hook verb and a SessionStart hook's stdout is appended to
the agent's context. Post-compact clears the fired set, so what the
compaction dropped injects again.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>
MSG
)"
```

---

## Follow-up

Open an issue, not part of this plan: teach devkit to read `.agents/repo-rules-agent.toml` when it is present, so a repository's extra languages, categories and topics are honoured by devkit as well as by the extractor. Until then `crates/devkit-rules/src/vocab.rs` holds a hand-synced copy of the built-in table.
