# Configurable slug caps implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a project set how long `issue setup` and `issue checkout-pr` may make a branch name and a worktree directory name, so a generated directory cannot push paths inside it past Windows' 260-character ceiling.

**Architecture:** Three optional `[templates]` keys carry the limits. One measurement function renders a template twice against the real context to count how many times the named variables appear, derives the fixed cost from the difference, and returns a per-variable character budget. `setup` gains a `short_slug` variable so its directory name can be shorter than its branch; `checkout-pr` caps the `pr_title` and `linear_title` it already has, since its branch comes from the remote.

**Tech Stack:** Rust 2024, `anyhow`, `serde_json`, `minijinja` (via `devkit_common::template`), `schemars` for the committed JSON Schema.

**Spec:** `docs/superpowers/specs/2026-08-30-configurable-slug-caps-design.md`

## Global Constraints

- Work happens in the worktree `../devkit-worktrees/slug-length-caps` on branch `slug-length-caps`. Never check out a branch in the primary clone at `C:/Users/Lev/Git/lev/devkit`.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` must both be green before every commit. Zero-warning policy.
- `cargo fmt --all` before every commit.
- TDD: write the failing test, watch it fail for the right reason, then implement.
- Commits follow Conventional Commits, imperative mood, subject under 50 characters, lowercase after the colon, no trailing period.
- Every commit message ends with the trailer `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
- Comments are timeless: no `this PR`, no `now we`, no `previously`, no issue references, no RED/GREEN narration.
- Test scratch directories come from `tempfile::tempdir()`, never a hand-built path under `std::env::temp_dir()`.
- Default values, verbatim from the spec: `branch_max` 46, `worktree_dir_max` 24, `checkout_worktree_dir_max` 46, `MIN_SLUG` 12.
- Overflow policy, verbatim from the spec: `branch_max` clamps up to `MIN_SLUG` and never fails; both directory limits return an error.

---

## File structure

| File | Responsibility after this change |
|---|---|
| `crates/devkit-config/src/lib.rs` | Owns the three limit keys, their `DEFAULT_*` constants, and their accessors. Owns `DEFAULT_BRANCH_MAX`, the single definition of 46. |
| `crates/devkit-common/src/ui.rs` | Re-exports `DEFAULT_BRANCH_MAX` as `BRANCH_DISPLAY_MAX` for table rendering. Holds no copy of the number. |
| `src/bin/devkit/issue/slug.rs` | Owns slug derivation and both length operations: `cap` shortens a slug, `budget` measures how much room a template leaves for one. |
| `src/bin/devkit/issue/setup.rs` | `branch_budget` and `short_slug` wrap the two measurements so `run` holds no length logic and both are reachable from a test. |
| `src/bin/devkit/issue/checkout.rs` | `dir_ctx` owns the measurement and the shortening for the directory context, for the same reason. |

---

### Task 1: Move the branch-width constant into `devkit-config`

Pure refactor, no behavior change. There is no meaningful test to add: after the move, asserting the two names hold the same value is tautological because one is a re-export of the other. The gate is the existing workspace suite plus clippy.

While editing `ui.rs`, note that its doc comment is currently misattached: the two lines describing `truncate` sit above `BRANCH_DISPLAY_MAX` instead of above the function. Fix that as part of the same edit.

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (near the `DEFAULT_*` block at line 389)
- Modify: `crates/devkit-common/src/ui.rs:95-106`

**Interfaces:**
- Consumes: nothing.
- Produces: `devkit_config::DEFAULT_BRANCH_MAX: usize` (value 46). `devkit_common::ui::BRANCH_DISPLAY_MAX` keeps its name, type, and value, so `src/bin/devkit/issue/triage.rs:17` and every other caller is untouched.

- [ ] **Step 1: Add the constant to `devkit-config`**

In `crates/devkit-config/src/lib.rs`, directly above `pub const DEFAULT_BRANCH`:

```rust
/// Longest branch `issue setup` renders before it shortens the slug to fit,
/// and the width the `issue status` branch column renders before eliding. One
/// number so a branch devkit created is never the one the table has to cut.
pub const DEFAULT_BRANCH_MAX: usize = 46;
```

- [ ] **Step 2: Turn `BRANCH_DISPLAY_MAX` into a re-export**

In `crates/devkit-common/src/ui.rs`, replace lines 95-106 (the merged doc comment, the const, and the `truncate` signature line) with:

```rust
pub use devkit_config::DEFAULT_BRANCH_MAX as BRANCH_DISPLAY_MAX;

/// Truncate to at most `max` visible characters, marking elision with `…`.
///
/// Operates on plain text (no escape awareness); apply before adding colour or
/// links so the ellipsis lands on a glyph boundary, not inside an escape.
pub fn truncate(s: &str, max: usize) -> String {
```

Leave the body of `truncate` exactly as it is.

- [ ] **Step 3: Confirm the workspace still builds and passes**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings. If `devkit-common` reports an unused-import or unresolved-crate error, confirm `devkit-config` is in its `[dependencies]` in `crates/devkit-common/Cargo.toml` (it already is; the crate is a leaf that `devkit-common` depends on).

- [ ] **Step 4: Commit**

```bash
git add crates/devkit-config/src/lib.rs crates/devkit-common/src/ui.rs
git commit -m "refactor(config): own the branch width constant

`ui::BRANCH_DISPLAY_MAX` is about to become one of two numbers that
decide how long a generated name may be, and the other lives in config.
Define it in the leaf crate and re-export it so the two cannot drift.

Also reattach the doc comment describing \`truncate\`, which sat on the
constant above it.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: `slug::budget`

The measurement function. Standalone and fully unit-testable, so it lands before anything calls it.

`slug_budget` in `setup.rs` currently renders the branch template once with a one-character slug and subtracts 1, which is only correct when `{{ slug }}` appears exactly once. `budget` renders twice, at one and two characters, and reads the occurrence count from the length difference. That makes it correct for a template that omits the variable, repeats it, or names several.

**Files:**
- Modify: `src/bin/devkit/issue/slug.rs`

**Interfaces:**
- Consumes: `devkit_common::template::render(template: &str, ctx: &impl Serialize, variables: &BTreeMap<String, String>) -> Result<String>`.
- Produces:

```rust
pub(crate) fn budget(
    template: &str,
    ctx: &serde_json::Value,
    vars: &std::collections::BTreeMap<String, String>,
    names: &[&str],
    max: usize,
    floor: Option<usize>,
) -> Result<usize>
```

  Returns the number of characters *each* name in `names` may spend. `usize::MAX` when the template renders none of them. With `floor: Some(n)` it never fails and clamps up to `n`; with `floor: None` it returns `Err` when the fixed text leaves no room.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `src/bin/devkit/issue/slug.rs`. The file's existing tests already have `use super::*;` at the top of that block; add `use std::collections::BTreeMap;` and `use serde_json::json;` alongside it.

```rust
    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn budget_subtracts_measured_fixed_text() {
        let ctx = json!({"prefix": "lev/", "slug": ""});
        // "lev/" is four characters the slug does not get.
        let b = budget(
            "{{ prefix }}{{ slug }}",
            &ctx,
            &novars(),
            &["slug"],
            46,
            Some(12),
        )
        .unwrap();
        assert_eq!(b, 42);
    }

    #[test]
    fn budget_is_unconstrained_when_the_template_omits_the_name() {
        let ctx = json!({"issue": "142", "slug": ""});
        let b = budget("{{ issue }}", &ctx, &novars(), &["slug"], 46, None).unwrap();
        assert_eq!(b, usize::MAX);
    }

    #[test]
    fn budget_halves_for_a_name_rendered_twice() {
        let ctx = json!({"slug": ""});
        let b = budget("{{ slug }}{{ slug }}", &ctx, &novars(), &["slug"], 40, None).unwrap();
        assert_eq!(b, 20);
    }

    #[test]
    fn budget_splits_between_two_names() {
        let ctx = json!({"pr_title": "", "linear_title": ""});
        // One dash of fixed text, 40 characters left, two names.
        let b = budget(
            "{{ pr_title }}-{{ linear_title }}",
            &ctx,
            &novars(),
            &["pr_title", "linear_title"],
            41,
            None,
        )
        .unwrap();
        assert_eq!(b, 20);
    }

    /// The default `checkout_worktree_dir` wraps a conditional around the
    /// tracker id, so the fixed cost depends on whether one resolved. Probing
    /// against the real context is what makes that measurable.
    #[test]
    fn budget_measures_a_conditional_block_against_the_real_context() {
        let tmpl = "{{ pr_number }}-{{ pr_title }}{% if linear_id %}_[{{ linear_id }}]{% endif %}";
        let with = json!({"pr_number": 142, "pr_title": "", "linear_id": "ENG-1234"});
        let without = json!({"pr_number": 142, "pr_title": "", "linear_id": ""});
        // "142-" is 4; "_[ENG-1234]" adds 11 more.
        assert_eq!(
            budget(tmpl, &with, &novars(), &["pr_title"], 46, None).unwrap(),
            31
        );
        assert_eq!(
            budget(tmpl, &without, &novars(), &["pr_title"], 46, None).unwrap(),
            42
        );
    }

    #[test]
    fn budget_clamps_up_to_a_floor() {
        let ctx = json!({"prefix": "a-very-long-branch-prefix-indeed/", "slug": ""});
        // 33 characters of prefix against a limit of 36 leaves 3, below the floor.
        let b = budget(
            "{{ prefix }}{{ slug }}",
            &ctx,
            &novars(),
            &["slug"],
            36,
            Some(12),
        )
        .unwrap();
        assert_eq!(b, 12);
    }

    #[test]
    fn budget_without_a_floor_errors_when_fixed_text_fills_the_limit() {
        let ctx = json!({"pr_number": 142, "pr_title": ""});
        let err = budget(
            "worktree-for-pr-{{ pr_number }}-{{ pr_title }}",
            &ctx,
            &novars(),
            &["pr_title"],
            16,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("20"), "{err}");
        assert!(err.contains("16"), "{err}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit --bin devkit budget`
Expected: FAIL to compile with `cannot find function 'budget' in this scope`.

- [ ] **Step 3: Implement `budget`**

Add to `src/bin/devkit/issue/slug.rs`, directly below `cap`. The file already imports `anyhow::Result`; add `use anyhow::Context;` and `use std::collections::BTreeMap;` to the file's imports.

```rust
/// Characters each name in `names` may spend before `template` renders longer
/// than `max`.
///
/// The fixed cost is measured rather than assumed. The template renders twice
/// against the real context, once with every name one character long and once
/// with two, so the difference in length counts how many times those names are
/// rendered in total. A template that renders none of them constrains nothing.
///
/// `floor` carries the overflow policy, because the two kinds of limit want
/// opposite answers when the fixed text leaves no room. `Some(n)` clamps up to
/// `n` and cannot fail, which suits a limit on a display width. `None` fails,
/// which suits a limit on a filesystem path, where a cap that silently does not
/// hold is the whole problem.
pub(crate) fn budget(
    template: &str,
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    names: &[&str],
    max: usize,
    floor: Option<usize>,
) -> Result<usize> {
    let probe = |len: usize| -> Result<usize> {
        let mut probe_ctx = ctx.clone();
        let obj = probe_ctx
            .as_object_mut()
            .context("template probe context is not an object")?;
        for name in names {
            obj.insert(
                (*name).to_string(),
                serde_json::Value::String("x".repeat(len)),
            );
        }
        Ok(devkit_common::template::render(template, &probe_ctx, vars)?
            .trim()
            .chars()
            .count())
    };
    let one = probe(1)?;
    let occurrences = probe(2)?.saturating_sub(one);
    if occurrences == 0 {
        return Ok(usize::MAX);
    }
    let fixed = one.saturating_sub(occurrences);
    let per_name = max.saturating_sub(fixed) / occurrences;
    match floor {
        Some(n) => Ok(per_name.max(n)),
        None if per_name == 0 => anyhow::bail!(
            "`{template}` renders {fixed} characters of fixed text, \
             which leaves no room within a limit of {max}"
        ),
        None => Ok(per_name),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin devkit budget`
Expected: PASS, seven tests.

- [ ] **Step 5: Run the full gate**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 6: Commit**

```bash
git add src/bin/devkit/issue/slug.rs
git commit -m "feat(issue): measure a template's room for a slug

Add \`slug::budget\`, which renders a template at two slug lengths and
reads the occurrence count from the difference. That makes the fixed cost
correct for a template that omits the variable, repeats it, or names
several, where subtracting one from a single probe is not.

The floor argument carries the overflow policy: a display width clamps
up to a minimum, a filesystem path fails instead.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: The three `[templates]` limit keys

Config surface only. Nothing reads these until Tasks 4 and 5.

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (the `DEFAULT_*` block near line 389, the `Templates` struct at 425, the `impl Templates` accessors at 469, and its `mod tests`)
- Modify: `schema/devkit-config.json` (regenerated, not hand-edited)
- Modify: `docs/configuration.md:387-410`

**Interfaces:**
- Consumes: `devkit_config::DEFAULT_BRANCH_MAX` from Task 1.
- Produces: `Templates::branch_max() -> usize`, `Templates::worktree_dir_max() -> usize`, `Templates::checkout_worktree_dir_max() -> usize`, plus `DEFAULT_WORKTREE_DIR_MAX: usize` (24) and `DEFAULT_CHECKOUT_WORKTREE_DIR_MAX: usize` (46).

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `crates/devkit-config/src/lib.rs`, beside the existing `default_checkout_worktree_dir_template` test:

```rust
    #[test]
    fn default_length_limits() {
        let t: Templates = toml::from_str("").unwrap();
        assert_eq!(t.branch_max(), 46);
        assert_eq!(t.worktree_dir_max(), 24);
        assert_eq!(t.checkout_worktree_dir_max(), 46);
    }

    #[test]
    fn length_limit_overrides_win() {
        let t: Templates = toml::from_str(
            "branch_max = 60\nworktree_dir_max = 18\ncheckout_worktree_dir_max = 30\n",
        )
        .unwrap();
        assert_eq!(t.branch_max(), 60);
        assert_eq!(t.worktree_dir_max(), 18);
        assert_eq!(t.checkout_worktree_dir_max(), 30);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit-config length_limit`
Expected: FAIL to compile with `no method named 'branch_max' found`.

- [ ] **Step 3: Add the constants**

In `crates/devkit-config/src/lib.rs`, beside `DEFAULT_BRANCH_MAX` from Task 1:

```rust
/// Longest worktree directory name `issue setup` renders. Shorter than the
/// branch limit because a directory name is charged against a filesystem path
/// limit and a branch name is not.
pub const DEFAULT_WORKTREE_DIR_MAX: usize = 24;
/// Longest worktree directory name `issue checkout-pr` renders.
pub const DEFAULT_CHECKOUT_WORKTREE_DIR_MAX: usize = 46;
```

- [ ] **Step 4: Add the three fields**

In the `Templates` struct, put `branch_max` directly after the `branch` field, `worktree_dir_max` after `worktree_dir`, and `checkout_worktree_dir_max` after `checkout_worktree_dir`:

```rust
    /// Longest branch `issue setup` will render. A derived slug is shortened on
    /// a word boundary to fit. A template whose fixed text already fills this
    /// yields the shortest slug still worth reading rather than an error, since
    /// a git ref has no hard length limit. Defaults to 46.
    pub branch_max: Option<usize>,
```

```rust
    /// Longest worktree directory name `issue setup` will render from
    /// `{{ short_slug }}`. A template whose fixed text already fills this is an
    /// error: a limit on a filesystem path that does not hold is worse than a
    /// setup that stops. Has no effect on a `worktree_dir` that does not render
    /// `{{ short_slug }}`. Defaults to 24.
    pub worktree_dir_max: Option<usize>,
```

```rust
    /// Longest worktree directory name `issue checkout-pr` will render.
    /// `pr_title` and `linear_title` are shortened to fit, splitting the budget
    /// when a template renders both. A template whose fixed text already fills
    /// this is an error. Defaults to 46.
    pub checkout_worktree_dir_max: Option<usize>,
```

- [ ] **Step 5: Add the three accessors**

In `impl Templates`, each beside the accessor for the template it limits:

```rust
    pub fn branch_max(&self) -> usize {
        self.branch_max.unwrap_or(DEFAULT_BRANCH_MAX)
    }
```

```rust
    pub fn worktree_dir_max(&self) -> usize {
        self.worktree_dir_max.unwrap_or(DEFAULT_WORKTREE_DIR_MAX)
    }
```

```rust
    pub fn checkout_worktree_dir_max(&self) -> usize {
        self.checkout_worktree_dir_max
            .unwrap_or(DEFAULT_CHECKOUT_WORKTREE_DIR_MAX)
    }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p devkit-config length_limit`
Expected: PASS, two tests.

- [ ] **Step 7: Regenerate the committed schema**

The repository commits `schema/devkit-config.json` and a test fails with a diff when it drifts.

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`
Then: `cargo test --test config_schema`
Expected: the first rewrites the file, the second passes. Confirm `git diff schema/devkit-config.json` shows the three new keys and nothing else.

- [ ] **Step 8: Document the keys**

In `docs/configuration.md`, the `[templates]` section opens with "renders seven strings from optional minijinja templates". Change `seven` to `eight` only if you also counted a new template — you did not, so leave that sentence alone and instead add a sentence after it:

```markdown
Three further keys cap how long the rendered result may be: `branch_max` (default 46), `worktree_dir_max` (default 24), and `checkout_worktree_dir_max` (default 46). A branch that cannot fit falls back to the shortest slug still worth reading; a worktree directory name that cannot fit is an error, because a limit on a filesystem path that silently does not hold is the reason these keys exist.
```

Then add three rows to the key table, after the `checkout_worktree_dir` row:

```markdown
| `branch_max` | `46` | characters; the derived slug is shortened on a word boundary to fit |
| `worktree_dir_max` | `24` | characters; caps `{{ short_slug }}`, and does nothing to a template without it |
| `checkout_worktree_dir_max` | `46` | characters; caps `pr_title` and `linear_title`, splitting the budget when both are rendered |
```

- [ ] **Step 9: Run the full gate**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 10: Commit**

```bash
git add crates/devkit-config/src/lib.rs schema/devkit-config.json docs/configuration.md
git commit -m "feat(config): add rendered-name length limits

Add branch_max, worktree_dir_max and checkout_worktree_dir_max under
[templates]. A generated worktree directory name is charged against a
filesystem path limit, which the branch column width these commands cap
against today has no relationship to.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: `setup` derives `slug` and `short_slug`

Replaces `slug_budget` with two `budget` calls and puts a second, shorter slug in the render context.

**Files:**
- Modify: `src/bin/devkit/issue/setup.rs:277-298` (delete `slug_budget`), `:327-340` (the call sites and the context), and its `mod tests`
- Modify: `docs/commands.md:94`

**Interfaces:**
- Consumes: `slug::budget(template, ctx, vars, names, max, floor) -> Result<usize>` and `slug::cap(slug: &str, budget: usize) -> String` from Task 2; `Templates::branch_max()` and `Templates::worktree_dir_max()` from Task 3.
- Produces: `short_slug` as a context key for the `branch` template, the `worktree_dir` template, `prep_apps`, and `after_worktree_create` hooks.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `src/bin/devkit/issue/setup.rs`. That block already has `use devkit_config::Templates;` and a `novars()` helper used by the prep-file tests; reuse them.

```rust
    fn cfg_with(prefix: &str, templates: Templates) -> devkit_config::Config {
        let mut cfg = devkit_config::Config::default();
        cfg.defaults.branch_prefix = prefix.into();
        cfg.templates = templates;
        cfg
    }

    /// One title, two limits: the branch keeps a slug worth reading while the
    /// worktree directory gets a shorter one.
    #[test]
    fn short_slug_is_shorter_than_the_branch_slug() {
        let cfg = cfg_with(
            "lev/",
            Templates {
                worktree_dir: Some("{{ short_slug }}".into()),
                ..Templates::default()
            },
        );
        let budget = branch_budget(&cfg, &novars(), "142", &[]).unwrap();
        let slug = crate::issue::slug::cap("group-sync-includes-file-lists-in-the-output", budget);
        assert_eq!(slug, "group-sync-includes-file-lists-in-the");
        assert_eq!(
            short_slug(&cfg, &novars(), "142", &[], &slug).unwrap(),
            "group-sync-includes-file"
        );
    }

    /// The shipped `worktree_dir` renders `{{ slug }}`, so the directory limit
    /// finds nothing to constrain and the directory keeps matching the branch.
    #[test]
    fn the_default_worktree_dir_ignores_its_limit() {
        let cfg = cfg_with("lev/", Templates::default());
        let slug = "group-sync-includes-file-lists-in-the";
        assert_eq!(short_slug(&cfg, &novars(), "142", &[], slug).unwrap(), slug);
    }

    /// A directory limit its own template's fixed text already fills is an
    /// error rather than a silently longer directory. The limit exists because
    /// a path that overruns cannot be removed by every tool that meets it.
    #[test]
    fn a_worktree_dir_limit_the_template_cannot_meet_is_an_error() {
        let cfg = cfg_with(
            "lev/",
            Templates {
                worktree_dir: Some("worktree-for-issue-{{ issue }}-{{ short_slug }}".into()),
                worktree_dir_max: Some(16),
                ..Templates::default()
            },
        );
        let err = short_slug(&cfg, &novars(), "142", &[], "fix-the-export")
            .unwrap_err()
            .to_string();
        assert!(err.contains("worktree_dir_max = 16"), "{err}");
    }

    /// A `branch_prefix` long enough to eat the budget yields the shortest slug
    /// still worth reading, not an error: a git ref has no hard length limit.
    #[test]
    fn a_branch_limit_the_prefix_fills_falls_back_to_the_floor() {
        // 39 characters of prefix against a limit of 46 leaves 7, below the floor.
        let cfg = cfg_with("an-extremely-long-branch-prefix-indeed/", Templates::default());
        assert_eq!(branch_budget(&cfg, &novars(), "142", &[]).unwrap(), MIN_SLUG);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit --bin devkit slug`
Expected: FAIL to compile with `cannot find function 'branch_budget' in this scope` and `cannot find function 'short_slug' in this scope`.

- [ ] **Step 3: Replace `slug_budget` with the two functions**

Delete the whole `slug_budget` function at `src/bin/devkit/issue/setup.rs:277-298`, including its doc comment. Keep `MIN_SLUG` at line 268 and its doc comment; it becomes the floor argument. In its place:

```rust
/// Render context both templates are measured against. The two slug values are
/// the ones being measured, so a caller passes whichever it already knows and
/// the empty string for the other.
fn probe_ctx(
    cfg: &devkit_config::Config,
    issue: &str,
    apps: &[String],
    slug: &str,
    short_slug: &str,
) -> serde_json::Value {
    serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": issue,
        "slug": slug,
        "short_slug": short_slug,
        "apps": apps,
    })
}

/// Characters the `branch` template leaves for the slug it renders. A template
/// whose fixed text fills `branch_max` falls back to `MIN_SLUG` rather than
/// failing, since an over-long branch is elided by the status table and
/// nothing worse.
fn branch_budget(
    cfg: &devkit_config::Config,
    vars: &BTreeMap<String, String>,
    issue: &str,
    apps: &[String],
) -> Result<usize> {
    crate::issue::slug::budget(
        cfg.templates.branch(),
        &probe_ctx(cfg, issue, apps, "", ""),
        vars,
        &["slug"],
        cfg.templates.branch_max(),
        Some(MIN_SLUG),
    )
    .context("measuring the `branch` template")
}

/// `slug` shortened again to whatever the `worktree_dir` template leaves for
/// `{{ short_slug }}`, and `slug` unchanged when that template does not render
/// it, which is the shipped default. Unlike the branch, this one fails rather
/// than overrunning: the directory name is charged against a filesystem path
/// limit.
fn short_slug(
    cfg: &devkit_config::Config,
    vars: &BTreeMap<String, String>,
    issue: &str,
    apps: &[String],
    slug: &str,
) -> Result<String> {
    let budget = crate::issue::slug::budget(
        cfg.templates.worktree_dir(),
        &probe_ctx(cfg, issue, apps, slug, ""),
        vars,
        &["short_slug"],
        cfg.templates.worktree_dir_max(),
        None,
    )
    .with_context(|| {
        format!(
            "measuring the `worktree_dir` template against templates.worktree_dir_max = {}",
            cfg.templates.worktree_dir_max()
        )
    })?;
    Ok(crate::issue::slug::cap(slug, budget))
}
```

The file already imports `anyhow::{Context, Result}` and `std::collections::BTreeMap`; confirm both are in scope and add whichever is missing.

- [ ] **Step 4: Call them from `run`**

In `run`, replace line 327 (`let budget = slug_budget(...)`) through the end of the `ctx` literal at line 340 with:

```rust
    let budget = branch_budget(cfg, vars, &issue, &args.apps)?;
    let details = want_summary(&args, cfg)
        .then(|| fetch_details(t, &issue))
        .transpose()?;
    let slug = resolve_slug(t, &issue_ref, args.slug.clone(), budget, details.as_ref())?;
    let dir_slug = short_slug(cfg, vars, &issue, &args.apps, &slug)?;

    let wt_root = expand_tilde(&cfg.defaults.worktree_root);
    let ctx = serde_json::json!({
        "prefix": cfg.defaults.branch_prefix,
        "issue": issue,
        "slug": slug,
        "short_slug": dir_slug,
        "apps": args.apps,
    });
```

The `details` fetch keeps the position it has today, between the branch budget and `resolve_slug`: the tracker round trip has to happen before a slug is derived from a title, and after the budget that shortens it is known.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin devkit slug` then `cargo test -p devkit --bin devkit setup`
Expected: PASS.

- [ ] **Step 6: Update `docs/commands.md`**

Replace the third paragraph of the `setup` section (line 94), the one beginning "Without `--slug`", with:

```markdown
Without `--slug`, a Linear URL supplies the slug from its own `…/issue/<ID>/<title-slug>` path and no lookup happens; otherwise the tracker is asked for the issue's title and that is slugified, which needs the tracker's credential. Either way a leading copy of the issue id is stripped so the branch template does not repeat it, and the derived slug is shortened on a word boundary to fit `templates.branch_max` (default 46, the width `issue status` prints). The budget is measured by rendering your own `branch` template, so a longer `branch_prefix` or issue id takes from the slug rather than overflowing. A slug passed to `--slug` is used verbatim.

`{{ short_slug }}` is the same slug shortened again to fit `templates.worktree_dir_max` (default 24). Render it instead of `{{ slug }}` in `worktree_dir` when a worktree path needs to be shorter than the branch name, which on Windows is what keeps paths inside the worktree under the 260-character ceiling that third-party tools still enforce. It shortens an explicit `--slug` too: `slug` is verbatim because a slug you typed is a decision, while being shorter is the entire purpose of `short_slug`.
```

- [ ] **Step 7: Run the full gate**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 8: Commit**

```bash
git add src/bin/devkit/issue/setup.rs docs/commands.md
git commit -m "feat(issue): add short_slug for worktree directories

A worktree directory name is charged against a filesystem path limit and
a branch name is not, but setup derived both from one slug capped against
the status table's column width. Derive a second, shorter slug for the
directory and put both in the render context.

Measurement moves to slug::budget, which counts occurrences instead of
assuming the slug is rendered exactly once.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 5: `checkout-pr` shortens its title slugs

`checkout-pr` applies no cap at all today, so a long PR title becomes a long directory name. Its branch comes from `gh pr checkout` against the remote, so only the directory is devkit's to cap.

The context that feeds `checkout_worktree_dir` moves into a small named function so the shortening is testable without a network call or a worktree.

**Files:**
- Modify: `src/bin/devkit/issue/checkout.rs:338-343` (the context literal), imports at line 1, and its `mod tests` at line 495
- Modify: `docs/commands.md` (the `checkout-pr` section at line 100)

**Interfaces:**
- Consumes: `slug::budget` and `slug::cap` from Task 2; `Templates::checkout_worktree_dir_max()` from Task 3.
- Produces:

```rust
fn dir_ctx(
    templates: &devkit_config::Templates,
    number: u64,
    title: &str,
    linear_id: &str,
    linear_title: Option<&str>,
) -> Result<serde_json::Value>
```

  Private to `checkout.rs`. It owns the budget measurement as well as the shortening, so `run` holds no length logic and a test reaches the same code path `run` does.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block in `src/bin/devkit/issue/checkout.rs`:

```rust
    /// The shipped template against its default limit: a long PR title is
    /// shortened on a word boundary rather than growing the directory name.
    #[test]
    fn the_default_template_shortens_a_long_pr_title() {
        let t = devkit_config::Templates::default();
        let ctx = dir_ctx(
            &t,
            142,
            "Group sync-includes file lists in the output",
            "ENG-1234",
            None,
        )
        .unwrap();
        let name =
            devkit_common::template::render(t.checkout_worktree_dir(), &ctx, &t.variables).unwrap();
        assert_eq!(name, "142-group-sync-includes-file-lists_[ENG-1234]");
        assert!(name.chars().count() <= t.checkout_worktree_dir_max());
    }

    /// Without a tracker id the conditional block drops out, so the same limit
    /// leaves eleven more characters for the title.
    #[test]
    fn a_pr_without_a_tracker_id_gets_the_conditional_block_back() {
        let t = devkit_config::Templates::default();
        let ctx = dir_ctx(
            &t,
            142,
            "Group sync-includes file lists in the output",
            "",
            None,
        )
        .unwrap();
        let name =
            devkit_common::template::render(t.checkout_worktree_dir(), &ctx, &t.variables).unwrap();
        assert_eq!(name, "142-group-sync-includes-file-lists-in-the");
        assert_eq!(ctx["linear_title"], "");
    }

    /// A template rendering both titles splits the budget between them.
    #[test]
    fn two_titles_split_the_budget() {
        let t = devkit_config::Templates {
            checkout_worktree_dir: Some("{{ pr_title }}-{{ linear_title }}".into()),
            checkout_worktree_dir_max: Some(41),
            ..devkit_config::Templates::default()
        };
        let ctx = dir_ctx(
            &t,
            142,
            "Group sync-includes file lists in the output",
            "ENG-1234",
            Some("Fix the export crash on save"),
        )
        .unwrap();
        // 41 less one dash of fixed text, split two ways, is 20 each.
        assert_eq!(ctx["pr_title"], "group-sync-includes");
        assert_eq!(ctx["linear_title"], "fix-the-export-crash");
    }

    /// A limit its own template's fixed text already fills is an error rather
    /// than a silently longer directory.
    #[test]
    fn a_checkout_limit_the_template_cannot_meet_is_an_error() {
        let t = devkit_config::Templates {
            checkout_worktree_dir: Some("worktree-for-pr-{{ pr_number }}-{{ pr_title }}".into()),
            checkout_worktree_dir_max: Some(16),
            ..devkit_config::Templates::default()
        };
        let err = dir_ctx(&t, 142, "Group sync-includes file lists", "", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("checkout_worktree_dir_max = 16"), "{err}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p devkit --bin devkit issue::checkout`
Expected: FAIL to compile with `cannot find function 'dir_ctx' in this scope`.

- [ ] **Step 3: Add `dir_ctx`**

In `src/bin/devkit/issue/checkout.rs`, change the import at line 1 from `use crate::issue::slug::slugify;` to `use crate::issue::slug::{budget, cap, slugify};`, then add above `run`:

```rust
/// Render context for `checkout_worktree_dir`, with both title slugs shortened
/// to whatever the template leaves inside `checkout_worktree_dir_max`. A
/// template rendering both splits that room between them.
///
/// Nothing here reaches the branch: `gh pr checkout` takes that from the
/// remote. This context names a directory, so a limit it cannot meet is an
/// error rather than an overrun.
fn dir_ctx(
    templates: &devkit_config::Templates,
    number: u64,
    title: &str,
    linear_id: &str,
    linear_title: Option<&str>,
) -> Result<serde_json::Value> {
    let max = templates.checkout_worktree_dir_max();
    let room = budget(
        templates.checkout_worktree_dir(),
        &serde_json::json!({
            "pr_number": number,
            "pr_title": "",
            "linear_id": linear_id,
            "linear_title": "",
        }),
        &templates.variables,
        &["pr_title", "linear_title"],
        max,
        None,
    )
    .with_context(|| {
        format!(
            "measuring the `checkout_worktree_dir` template against \
             templates.checkout_worktree_dir_max = {max}"
        )
    })?;
    Ok(serde_json::json!({
        "pr_number": number,
        "pr_title": cap(&slugify(title), room),
        "linear_id": linear_id,
        "linear_title": linear_title
            .map(|t| cap(&slugify(t), room))
            .unwrap_or_default(),
    }))
}
```

- [ ] **Step 4: Call it from `run`**

Replace the context literal at lines 338-343 with:

```rust
    let linear_id = resolved.linear_id.clone().unwrap_or_default();
    let ctx = dir_ctx(
        &cfg.templates,
        meta.number,
        &meta.title,
        &linear_id,
        resolved.linear_title.as_deref(),
    )?;
```

Leave the two other contexts in this file alone. The `setup_ctx` at line 436 and the hook context at line 454 pass `slug` to `prep_files` and to `after_worktree_create`, neither of which names a directory, so neither is charged against a path limit.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin devkit checkout`
Expected: PASS.

- [ ] **Step 6: Document it**

In `docs/commands.md`, at the end of the `checkout-pr` section's opening prose (before the `--setup` discussion), add:

```markdown
The worktree directory name comes from `templates.checkout_worktree_dir`, whose `pr_title` and `linear_title` are shortened on a word boundary to fit `templates.checkout_worktree_dir_max` (default 46); a template rendering both splits the budget between them. The branch is not devkit's to shorten — `gh pr checkout` takes it from the remote.
```

- [ ] **Step 7: Run the full gate**

Run: `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS, zero warnings.

- [ ] **Step 8: Commit**

```bash
git add src/bin/devkit/issue/checkout.rs docs/commands.md
git commit -m "fix(issue): cap the checkout-pr directory name

A PR title went into checkout_worktree_dir unshortened, so a long one
produced a directory name long enough to push paths inside the worktree
past Windows' 260-character ceiling.

Shorten pr_title and linear_title to fit checkout_worktree_dir_max, and
move the context into a named function so the shortening is testable
without a network call.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Verification before opening a PR

- [ ] `cargo fmt --all --check` reports no diff.
- [ ] `cargo test --workspace` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green.
- [ ] `git diff main --stat` touches only the eight files this plan names.
- [ ] A real run against this repository's own config, which sets `tracker.kind = "github"`: `cargo run --bin devkit -- issue setup <a real issue number> --dry-run` prints a branch and a worktree path, and adding `worktree_dir = "{{ short_slug }}"` plus `worktree_dir_max = 18` to `devkit.toml` shortens the printed directory without shortening the branch. Revert that config edit before committing.
