# Hooks on `issue end` implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two `[hooks]` keys, `after_worktree_remove` (once per worktree `issue end` removed) and `after_end` (once per run that removed at least one), so a project can react to a worktree going away the way `after_worktree_create` reacts to one appearing.

**Architecture:** The hook runner moves out of `src/bin/devkit/issue/setup.rs` into a new `src/bin/devkit/issue/hooks.rs` and gains the config key name as a parameter for its warning text. `issue end` learns which worktrees it actually removed instead of only counting them, builds each removed worktree's render context *before* the removal deletes the `.devkit/issue.toml` it reads from, and fires both keys serially in the main repository root after the removals join and the prune runs.

**Tech Stack:** Rust edition 2024, `anyhow`, `serde` + `schemars` (JsonSchema derives), `minijinja` via `devkit_common::template`, `tempfile` for test scratch, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-09-01-issue-end-hooks-design.md`

## Global Constraints

- Work happens in the worktree `../devkit-worktrees/issue-end-hooks` on branch `issue-end-hooks`. Never check a branch out in the primary clone.
- Gate, all three green before each commit: `cargo nextest run --workspace --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`.
- Commits follow Conventional Commits, imperative subject, 50 chars soft / 72 hard, lowercase after the colon, no trailing period.
- `schema/devkit-config.json` is committed and a test fails with a diff when it drifts. Regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`.
- Test scratch comes from `tempfile::tempdir()`. Bind the `TempDir` guard for as long as the path is used.
- Comments are timeless: no `this PR`, no `now we`, no issue or task references, no RED/GREEN narration.
- Hooks are fail-open everywhere. A hook that cannot render, cannot spawn, or exits non-zero prints a `warning:` line and the next hook still runs. No hook failure may turn a successful removal into a failed command.
- CI runs on ubuntu, macos and windows. Tests use `git` as the hook program, which every runner has.

---

### Task 1: Move the hook runner into its own module

Pure refactor. No behavior changes, no new config keys. `setup.rs` and `checkout.rs` keep calling the same public function; only its body moves.

**Files:**
- Create: `src/bin/devkit/issue/hooks.rs`
- Modify: `src/bin/devkit/issue/mod.rs` (add `mod hooks;`)
- Modify: `src/bin/devkit/issue/setup.rs` (delete the moved items, call the new module)
- Test: `src/bin/devkit/issue/hooks.rs` (the moved `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `devkit_common::cmd::capture`, `devkit_common::progress::Steps`, `devkit_common::template::render`, `devkit_common::ui::truncate`.
- Produces: `pub(crate) fn hooks::run_all(cwd: &Path, key: &str, hooks: &[Vec<String>], ctx: &serde_json::Value, vars: &BTreeMap<String, String>, steps: &Steps)`. Task 3 and Task 4 call it. `setup::run_after_worktree_create` keeps its existing signature `(worktree: &Path, hooks: &[Vec<String>], ctx: &serde_json::Value, vars: &BTreeMap<String, String>, steps: &Steps)` so `checkout.rs:514` is untouched.

- [ ] **Step 1: Create the module with the runner and its tests**

Create `src/bin/devkit/issue/hooks.rs`:

```rust
//! The `[hooks]` runner: render one command's argv, run it in a given
//! directory, and draw a progress step per command. Every hook key shares it.

use anyhow::{Context, Result};
use devkit_common::cmd::capture;
use devkit_common::progress::Steps;
use std::collections::BTreeMap;
use std::path::Path;

/// Width a hook's command is elided to in its progress step: sized for an
/// 80-column terminal alongside the mark, the `[i/n]` counter, and the
/// elapsed time.
const HOOK_LABEL_MAX: usize = 56;

/// Render one hook's argv against `ctx`/`vars`.
fn render_hook(
    hook: &[String],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    hook.iter()
        .map(|part| {
            devkit_common::template::render(part, ctx, vars)
                .with_context(|| format!("rendering hook argument `{part}`"))
        })
        .collect()
}

/// Run an already-rendered hook argv in `cwd`.
fn run_rendered(cwd: &Path, argv: &[String]) -> Result<()> {
    let (prog, rest) = argv.split_first().context("empty hook command")?;
    capture(
        prog,
        &rest.iter().map(String::as_str).collect::<Vec<_>>(),
        cwd.to_str(),
    )?;
    Ok(())
}

/// The progress-step label for a hook command.
fn hook_label(argv: &[String]) -> String {
    format!(
        "Hook: {}",
        devkit_common::ui::truncate(
            &argv.join(" ").replace(['\n', '\r', '\t'], " "),
            HOOK_LABEL_MAX
        )
    )
}

/// Run each command in `hooks` in `cwd`, in order, one progress step each.
/// `key` names the config key the commands came from, for the warning a
/// failure prints. Fail-open: the state a hook reacts to has already happened
/// by the time it runs, so a hook that fails warns on stderr and the rest
/// still run.
pub(crate) fn run_all(
    cwd: &Path,
    key: &str,
    hooks: &[Vec<String>],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    steps: &Steps,
) {
    for hook in hooks {
        let rendered = render_hook(hook, ctx, vars);
        // A hook that cannot render still draws its step, labelled from the
        // template source so the offending argument is visible. The step
        // counter only advances inside `during_result`, so skipping the step
        // would leave the run ending short of its total.
        let label = hook_label(rendered.as_deref().unwrap_or(hook));
        if let Err(e) = steps.during_result(&label, || run_rendered(cwd, &rendered?)) {
            eprintln!("warning: {key} hook `{}` failed: {e:#}", hook.join(" "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> serde_json::Value {
        json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix", "apps": ["web"], "app": "web"})
    }

    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn run(dir: &Path, hooks: &[Vec<String>], steps: &Steps) {
        run_all(dir, "after_worktree_create", hooks, &ctx(), &novars(), steps);
    }

    #[test]
    fn render_hook_expands_each_argument() {
        let hook = vec![
            "git".to_string(),
            "init".to_string(),
            "{{ slug }}-wt".to_string(),
        ];
        let argv = render_hook(&hook, &ctx(), &novars()).unwrap();
        assert_eq!(argv, vec!["git", "init", "fix-wt"]);
    }

    #[test]
    fn render_hook_reports_the_argument_it_could_not_render() {
        let hook = vec!["git".to_string(), "{{ nope }}".to_string()];
        let err = render_hook(&hook, &ctx(), &novars()).unwrap_err();
        assert!(
            format!("{err:#}").contains("{{ nope }}"),
            "the error names the offending argument: {err:#}"
        );
    }

    #[test]
    fn hook_label_keeps_a_short_command_whole() {
        let argv = vec!["bun".to_string(), "install".to_string()];
        assert_eq!(hook_label(&argv), "Hook: bun install");
    }

    #[test]
    fn hook_label_elides_a_long_command() {
        let argv = vec!["bash".to_string(), "-c".to_string(), "x".repeat(200)];
        let label = hook_label(&argv);
        assert!(label.starts_with("Hook: bash -c "), "label was {label}");
        assert!(label.ends_with('…'), "label was {label}");
        assert_eq!(
            label.chars().count(),
            "Hook: ".chars().count() + HOOK_LABEL_MAX
        );
    }

    #[test]
    fn hook_label_collapses_embedded_newlines() {
        let argv = vec![
            "bash".to_string(),
            "-c".to_string(),
            "echo one\necho two".to_string(),
        ];
        let label = hook_label(&argv);
        assert!(!label.contains('\n'), "label was {label:?}");
        assert_eq!(label, "Hook: bash -c echo one\necho two".replace('\n', " "));
    }

    #[test]
    fn hook_renders_args_and_runs_in_the_given_directory() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![vec![
            "git".to_string(),
            "init".to_string(),
            "{{ slug }}-wt".to_string(),
        ]];
        run(dir.path(), &hooks, &Steps::persistent());
        assert!(dir.path().join("fix-wt").exists());
    }

    #[test]
    fn failing_hook_does_not_stop_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec!["devkit-no-such-program-xyz".to_string()],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        run(dir.path(), &hooks, &Steps::persistent());
        assert!(dir.path().join("after").exists());
    }

    #[test]
    fn unrenderable_hook_does_not_stop_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec![
                "git".to_string(),
                "init".to_string(),
                "{{ nope }}".to_string(),
            ],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        run(dir.path(), &hooks, &Steps::persistent());
        assert!(dir.path().join("after").exists());
    }

    #[test]
    fn every_hook_consumes_a_step_even_when_it_cannot_render() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![
            vec![
                "git".to_string(),
                "init".to_string(),
                "{{ nope }}".to_string(),
            ],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        let steps = Steps::persistent_with_total(hooks.len());
        run(dir.path(), &hooks, &steps);
        assert_eq!(
            steps.started(),
            2,
            "an unrenderable hook must still consume its step"
        );
        assert!(dir.path().join("after").exists(), "the next hook still ran");
    }

    #[test]
    fn the_warning_names_the_key_the_command_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = vec![vec!["devkit-no-such-program-xyz".to_string()]];
        // The warning goes to stderr, which a unit test cannot capture; the
        // key reaching `run_all` unchanged is what this pins.
        run_all(
            dir.path(),
            "after_end",
            &hooks,
            &ctx(),
            &novars(),
            &Steps::persistent_with_total(1),
        );
    }
}
```

- [ ] **Step 2: Register the module**

In `src/bin/devkit/issue/mod.rs`, add `mod hooks;` to the alphabetical list of `mod` declarations, between `mod end;` and `mod info;`.

- [ ] **Step 3: Run the new tests and watch them pass**

Run: `cargo nextest run -p devkit --bin devkit hooks::`
Expected: PASS. The module is new but the code inside it is a copy, so this is a GREEN-first refactor step, not a TDD cycle. The RED that matters is Step 5.

- [ ] **Step 4: Delete the moved items from `setup.rs`**

In `src/bin/devkit/issue/setup.rs`, delete `HOOK_LABEL_MAX`, `render_hook`, `run_rendered`, `hook_label`, and the body of `run_after_worktree_create` (lines 141-207 in the current file), replacing the last with a thin forwarder:

```rust
/// Run each `hooks.after_worktree_create` command in the new worktree, in
/// order. The worktree already exists and is usable by the time these run.
pub(crate) fn run_after_worktree_create(
    worktree: &Path,
    hooks: &[Vec<String>],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
    steps: &Steps,
) {
    crate::issue::hooks::run_all(
        worktree,
        "after_worktree_create",
        hooks,
        ctx,
        vars,
        steps,
    );
}
```

Delete these tests from `setup.rs`'s `mod tests`, which now live in `hooks.rs`: `render_hook_expands_each_argument`, `render_hook_reports_the_argument_it_could_not_render`, `hook_label_keeps_a_short_command_whole`, `hook_label_elides_a_long_command`, `hook_label_collapses_embedded_newlines`, `hook_renders_args_and_runs_in_the_worktree`, `failing_hook_does_not_stop_the_next_one`, `unrenderable_hook_does_not_stop_the_next_one`, `every_hook_consumes_a_step_even_when_it_cannot_render`.

Keep `fn ctx()` and `fn novars()` in `setup.rs` only if other tests there still use them. Check with `rg -n "novars\(\)|ctx\(\)" src/bin/devkit/issue/setup.rs` and delete whichever has no remaining caller, or clippy's dead-code lint will fail the gate.

- [ ] **Step 5: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS. A failure here means an import went stale (`capture`, `truncate`, or `BTreeMap` may now be unused in `setup.rs`) or a helper lost its last caller.

- [ ] **Step 6: Commit**

```bash
git add src/bin/devkit/issue/hooks.rs src/bin/devkit/issue/mod.rs src/bin/devkit/issue/setup.rs
git commit -m "refactor(issue): move the hook runner to its own module" -m "The runner is about to serve issue end as well as issue setup and
issue checkout-pr, so it takes the config key name for its warning
instead of naming after_worktree_create in the message."
```

---

### Task 2: Add the two config keys

**Files:**
- Modify: `crates/devkit-config/src/lib.rs` (the `HooksConfig` struct at line 153-166, and its `mod tests`)
- Modify: `docs/configuration.md` (the `[hooks]` section at lines 413-434)
- Modify: `schema/devkit-config.json` (regenerated, not hand-edited)
- Test: `crates/devkit-config/src/lib.rs` `mod tests`

**Interfaces:**
- Produces: `HooksConfig::after_worktree_remove: Vec<Vec<String>>` and `HooksConfig::after_end: Vec<Vec<String>>`. Task 3 reads the first, Task 4 the second.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/devkit-config/src/lib.rs`:

```rust
    #[test]
    fn parses_the_issue_end_hook_keys() {
        let cfg: Config = toml::from_str(
            "[hooks]\n\
             after_worktree_remove = [[\"zoxide\", \"remove\", \"{{ worktree }}\"]]\n\
             after_end = [[\"alacritree\", \"project\", \"refresh\"]]\n",
        )
        .unwrap();
        assert_eq!(cfg.hooks.after_worktree_remove.len(), 1);
        assert_eq!(
            cfg.hooks.after_worktree_remove[0],
            ["zoxide", "remove", "{{ worktree }}"]
        );
        assert_eq!(cfg.hooks.after_end.len(), 1);
        assert_eq!(cfg.hooks.after_end[0], ["alacritree", "project", "refresh"]);
    }

    #[test]
    fn the_issue_end_hook_keys_default_empty() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.hooks.after_worktree_remove.is_empty());
        assert!(cfg.hooks.after_end.is_empty());
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p devkit-config parses_the_issue_end_hook_keys`
Expected: FAIL to compile with `error[E0609]: no field 'after_worktree_remove' on type 'HooksConfig'`. A compile error is the RED here: `[hooks]` has no `deny_unknown_fields`, so the TOML itself parses fine and only the field access can fail.

- [ ] **Step 3: Add the fields**

In `crates/devkit-config/src/lib.rs`, add to `HooksConfig` after `after_worktree_create`:

```rust
    /// Runs once per worktree `issue end` removed, after every removal in the
    /// run has finished and the stale worktree entries are pruned. The
    /// worktree is gone by then, so these run in the main repository root;
    /// `issue`, `slug` and `apps` come from the `.devkit/issue.toml` record
    /// read before the removal. Rendered over `worktree`, `branch`, `issue`,
    /// `slug`, `apps`, `prefix`, `worktree_root`, `primary`, and
    /// `[templates.variables]`.
    pub after_worktree_remove: Vec<Vec<String>>,

    /// Runs once at the end of an `issue end` run that removed at least one
    /// worktree, after every `after_worktree_remove` hook, in the main
    /// repository root. A run-level event: it carries `removed` (the removed
    /// worktree paths, in the order they were confirmed), `count`, `prefix`,
    /// `worktree_root`, `primary`, and `[templates.variables]`, and none of
    /// the single-worktree keys.
    pub after_end: Vec<Vec<String>>,
```

Update the `HooksConfig` doc comment above the struct so it no longer implies one key:

```rust
/// Commands run on a devkit lifecycle event. Most keys are named
/// `{before,after}_<event>` after the state change rather than the command
/// that reached it; `after_end` is the exception and names its run, because a
/// run-level event has exactly one caller and no worktree state to be named
/// for. Each key holds a list of argv arrays — no shell, so pipes, `&&`, and
/// globs are not available. A hook that fails to render, spawn, or exit zero
/// warns on stderr and the remaining hooks still run.
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p devkit-config issue_end_hook`
Expected: PASS, both tests.

- [ ] **Step 5: Regenerate the committed schema**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`
Then: `git diff --stat schema/devkit-config.json`
Expected: `schema/devkit-config.json` gains `after_worktree_remove` and `after_end` properties under `HooksConfig` with the doc comments as descriptions. Do not hand-edit the file.

- [ ] **Step 6: Update `docs/configuration.md`**

Replace the table under `### [hooks]` with:

```markdown
| Key | Fires on | Cwd |
|---|---|---|
| `after_worktree_create` | `issue setup` and `issue checkout-pr`, once the worktree exists and its apps are prepared, after the command has reported the worktree | the new worktree's root |
| `after_worktree_remove` | `issue end`, once per worktree it removed, after every removal in the run has finished and the stale worktree entries are pruned | the main repository root |
| `after_end` | `issue end`, once per run that removed at least one worktree, after every `after_worktree_remove` hook | the main repository root |
```

Replace the paragraph beginning "The event names a state change, not the caller." with:

```markdown
Most keys name a state change rather than the caller. `after_worktree_create` fires from both `issue setup` and `issue checkout-pr`; naming it after either command would have been wrong for the other, and a new state key fires from every command that reaches its state. `after_end` is the deliberate exception: a run-level event has exactly one caller by construction and no worktree state it could be named for.

A worktree kept back by a required `[preserve]` failure, refused as dirty, or skipped at the confirmation prompt never fires `after_worktree_remove`. A run that removed nothing fires neither key, including the early exits that report nothing to clean up.
```

Replace the paragraph beginning "Each argv element is a minijinja template over" with:

```markdown
Each argv element is a minijinja template over `[templates.variables]` plus the keys its own event carries. `after_worktree_create` renders over `worktree`, `branch`, `issue`, `slug`, `apps`, and `prefix`. `after_worktree_remove` adds `worktree_root` and `primary`; its worktree is already gone, so those values come from the `.devkit/issue.toml` record read before the removal. `after_end` carries `removed` (the removed worktree paths, in the order they were confirmed), `count`, `prefix`, `worktree_root`, and `primary`, and none of the single-worktree keys. Hooks run in the order listed, and every `after_worktree_remove` finishes before `after_end` starts.
```

Append to the fail-open paragraph:

```markdown
The `issue end` keys fire after the removals have joined and the run has printed its summary, so a failing hook cannot keep a removed worktree from being reported as removed. When the main repository root does not resolve, both are skipped with a warning rather than run in a directory the removal just deleted.
```

Extend the example block:

```toml
[hooks]
after_worktree_create = [["zoxide", "add", "{{ worktree }}"]]
after_worktree_remove = [["zoxide", "remove", "{{ worktree }}"]]
after_end = [["alacritree", "project", "refresh"]]
```

Then check whether any other doc names the hook table and update it the same way:

Run: `rg -n "after_worktree_create" docs/ README.md AGENTS.md`
Expected: hits in `docs/configuration.md` (just edited) and in `docs/superpowers/` archives, which are historical records and must not be edited. If `docs/commands.md`, `README.md`, or `AGENTS.md` hits, add the two keys there in the same shape.

- [ ] **Step 7: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS, including `config_schema` now that the schema is regenerated.

- [ ] **Step 8: Commit**

```bash
git add crates/devkit-config/src/lib.rs schema/devkit-config.json docs/configuration.md
git commit -m "feat(config): add the issue end hook keys" -m "after_worktree_remove fires per worktree issue end removed;
after_end fires once per run that removed at least one. Both are
declared here and wired up in issue end separately."
```

---

### Task 3: Fire `after_worktree_remove`

**Files:**
- Modify: `src/bin/devkit/issue/end.rs` (imports at line 1-9, the removal phase at 357-390, the summary at 392-407, plus two new helpers)
- Test: `src/bin/devkit/issue/end.rs` `mod tests`

**Interfaces:**
- Consumes: `hooks::run_all` from Task 1, `HooksConfig::after_worktree_remove` from Task 2, and the existing `crate::issue::preserve::context(worktree: &Path, branch: &str, record: Option<&IssueRecord>, prefix: &str, worktree_root: &Path, primary: Option<&Path>) -> serde_json::Value`.
- Produces: `fn removed_in_order(approved: &[IssueWorktree], removed: &HashSet<String>) -> Vec<String>` and `fn remove_contexts(approved: &[IssueWorktree], prefix: &str, worktree_root: &Path, primary: Option<&Path>) -> HashMap<String, serde_json::Value>`. Task 4 reads the `Vec<String>` that `removed_in_order` returns.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/bin/devkit/issue/end.rs`:

```rust
    fn approved_row(worktree: &str, branch: &str, issue_id: &str) -> IssueWorktree {
        IssueWorktree {
            worktree: worktree.into(),
            branch: branch.into(),
            issue_id: issue_id.into(),
            dirty: false,
            pr: devkit_issue::status::PrStatus::None,
            state: None,
            finished: true,
            reason_not_finished: None,
        }
    }

    #[test]
    fn removed_worktrees_come_back_in_approval_order() {
        let approved = vec![
            approved_row("/wt/a", "lev/a", "ENG-1"),
            approved_row("/wt/b", "lev/b", "ENG-2"),
            approved_row("/wt/c", "lev/c", "ENG-3"),
        ];
        // The parallel removal phase finishes in whatever order it finishes.
        let done: std::collections::HashSet<String> =
            ["/wt/c".to_string(), "/wt/a".to_string()].into_iter().collect();
        assert_eq!(
            removed_in_order(&approved, &done),
            vec!["/wt/a".to_string(), "/wt/c".to_string()]
        );
    }

    #[test]
    fn a_worktree_that_was_not_removed_is_left_out() {
        let approved = vec![approved_row("/wt/a", "lev/a", "ENG-1")];
        let done = std::collections::HashSet::new();
        assert!(removed_in_order(&approved, &done).is_empty());
    }

    #[test]
    fn the_remove_context_reads_the_record_before_removal() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("eng-9-fix");
        std::fs::create_dir_all(&wt).unwrap();
        devkit_common::record::write(
            &wt,
            &devkit_common::record::IssueRecord {
                issue: "ENG-9".into(),
                slug: "fix".into(),
                apps: vec!["web".into()],
                summary: None,
                pr: None,
            },
        )
        .unwrap();
        let approved = vec![approved_row(
            wt.to_str().unwrap(),
            "lev/eng-9-fix",
            "ENG-9",
        )];

        let ctxs = remove_contexts(&approved, "lev/", dir.path(), None);

        let ctx = &ctxs[wt.to_str().unwrap()];
        assert_eq!(ctx["issue"], "ENG-9");
        assert_eq!(ctx["slug"], "fix");
        assert_eq!(ctx["branch"], "lev/eng-9-fix");
        assert_eq!(ctx["worktree"], wt.display().to_string());
    }

    #[test]
    fn a_worktree_with_no_record_still_gets_a_context() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join("no-record");
        std::fs::create_dir_all(&wt).unwrap();
        let approved = vec![approved_row(wt.to_str().unwrap(), "lev/x", "ENG-0")];

        let ctxs = remove_contexts(&approved, "lev/", dir.path(), None);

        assert_eq!(ctxs[wt.to_str().unwrap()]["issue"], "");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit --bin devkit issue::end`
Expected: FAIL to compile with `cannot find function 'removed_in_order' in this scope` and the same for `remove_contexts`.

- [ ] **Step 3: Add the two helpers**

In `src/bin/devkit/issue/end.rs`, above `pub fn run`:

```rust
/// The approved worktrees that were actually removed, in approval order. The
/// removal phase runs in parallel and finishes out of order, so its unordered
/// result is put back into the order the prompts ran in before any hook or
/// report reads it.
fn removed_in_order(
    approved: &[IssueWorktree],
    removed: &std::collections::HashSet<String>,
) -> Vec<String> {
    approved
        .iter()
        .map(|r| r.worktree.clone())
        .filter(|w| removed.contains(w))
        .collect()
}

/// The `after_worktree_remove` render context for each approved worktree,
/// keyed by its path. Built before the removal phase: `issue`, `slug` and
/// `apps` come from `.devkit/issue.toml`, which the removal deletes along with
/// everything else in the worktree.
fn remove_contexts(
    approved: &[IssueWorktree],
    prefix: &str,
    worktree_root: &Path,
    primary: Option<&Path>,
) -> std::collections::HashMap<String, serde_json::Value> {
    approved
        .iter()
        .map(|row| {
            let wt = Path::new(&row.worktree);
            let record = devkit_common::record::read(wt);
            (
                row.worktree.clone(),
                crate::issue::preserve::context(
                    wt,
                    &row.branch,
                    record.as_ref(),
                    prefix,
                    worktree_root,
                    primary,
                ),
            )
        })
        .collect()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p devkit --bin devkit issue::end`
Expected: PASS, including the pre-existing `cleanup` tests.

- [ ] **Step 5: Read the hook lists and build the contexts before the removal phase**

In `pub fn run`, immediately after the existing `let (wt_root, prefix) = …;` block and before the `let mut blocked: … = HashSet::new();` line, insert:

```rust
    // Read before the removal phase so a hook can name the issue and slug of a
    // worktree whose record no longer exists.
    let empty: &[Vec<String>] = &[];
    let cfg_hooks = sel.config.as_ref().map(|c| &c.hooks);
    let after_worktree_remove = cfg_hooks.map_or(empty, |h| h.after_worktree_remove.as_slice());
    let remove_ctxs = if after_worktree_remove.is_empty() {
        std::collections::HashMap::new()
    } else {
        remove_contexts(
            &approved,
            &prefix,
            &wt_root,
            main.as_deref().map(Path::new),
        )
    };
```

- [ ] **Step 6: Make the removal phase report which worktrees it removed**

In `src/bin/devkit/issue/end.rs`, in the Phase 3 block:

Replace `let removed = AtomicUsize::new(0);` with:

```rust
    let removed: Mutex<std::collections::HashSet<String>> =
        Mutex::new(std::collections::HashSet::new());
```

Replace the success arm:

```rust
                    Ok(()) => {
                        removed.fetch_add(1, Ordering::Relaxed);
                    }
```

with:

```rust
                    Ok(()) => {
                        removed.lock().unwrap().insert(row.worktree.clone());
                    }
```

Immediately after the `std::thread::scope(…);` call, add:

```rust
    let removed = removed_in_order(&approved, &removed.into_inner().unwrap());
```

Change the prune block from `if let Some(main) = main {` to `if let Some(main) = &main {` so `main` stays available for the hooks below, and change `Git::at(Path::new(&main))` to `Git::at(Path::new(main))`.

Change the summary line from:

```rust
    println!("Removed {} of {}.", removed.load(Ordering::Relaxed), total);
```

to:

```rust
    println!("Removed {} of {}.", removed.len(), total);
```

Delete the now-unused import on line 6: `use std::sync::atomic::{AtomicUsize, Ordering};`.

- [ ] **Step 7: Fire the hooks**

After the `println!("Removed {} of {}.", …);` line and before the `anyhow::ensure!(required_failures == 0, …)`, add:

```rust
    // After the summary and after the prune: a hook sees every removal
    // finished, and its progress step cannot tear the report. A run that
    // removed nothing changed nothing on disk, so nothing fires.
    if !removed.is_empty() && !after_worktree_remove.is_empty() {
        match main.as_deref() {
            Some(root) => {
                let root = Path::new(root);
                for wt in &removed {
                    let Some(ctx) = remove_ctxs.get(wt) else {
                        continue;
                    };
                    crate::issue::hooks::run_all(
                        root,
                        "after_worktree_remove",
                        after_worktree_remove,
                        ctx,
                        &vars,
                        &steps,
                    );
                }
            }
            // The worktree the command was run from is usually the one just
            // removed, so there is no directory left to inherit.
            None => eprintln!(
                "warning: after_worktree_remove hooks skipped: the main repository root did not resolve"
            ),
        }
    }
```

- [ ] **Step 8: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS. If clippy flags `empty` as unused, Task 4 uses it too; keep it and re-check after Task 4 rather than deleting it.

- [ ] **Step 9: Verify by hand against a real worktree**

```bash
git -C . worktree add ../devkit-worktrees/hook-smoke -b hook-smoke main
printf '[hooks]\nafter_worktree_remove = [["git", "init", "removed-{{ slug }}"]]\n' >> ~/.config/devkit/config.toml
```

Then remove it with `issue end --clean-worktree hook-smoke` from the primary clone, confirm a `Hook: git init removed-…` step appears, and confirm the directory it created is in the primary clone's root. Delete the directory and revert the `~/.config/devkit/config.toml` edit afterwards. This touches your personal config, so revert it in the same session.

- [ ] **Step 10: Commit**

```bash
git add src/bin/devkit/issue/end.rs
git commit -m "feat(issue): run after_worktree_remove hooks on end" -m "The removal phase reports which worktrees it removed rather than only
counting them, so each removed worktree's hooks fire once, in approval
order, in the main repository root. Contexts are built before the
removal because the record they read is inside the worktree."
```

---

### Task 4: Fire `after_end`

**Files:**
- Modify: `src/bin/devkit/issue/end.rs` (the hook-firing block from Task 3, plus one new helper)
- Test: `src/bin/devkit/issue/end.rs` `mod tests`

**Interfaces:**
- Consumes: `removed_in_order`'s `Vec<String>` and the `after_worktree_remove` firing block, both from Task 3; `HooksConfig::after_end` from Task 2.
- Produces: `fn end_context(removed: &[String], prefix: &str, worktree_root: &Path, primary: Option<&Path>) -> serde_json::Value`. Nothing downstream consumes it.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/bin/devkit/issue/end.rs`:

```rust
    #[test]
    fn the_end_context_carries_every_removed_worktree() {
        let removed = vec!["/wt/a".to_string(), "/wt/b".to_string()];
        let ctx = end_context(&removed, "lev/", Path::new("/wt"), Some(Path::new("/repo")));
        assert_eq!(ctx["removed"][0], "/wt/a");
        assert_eq!(ctx["removed"][1], "/wt/b");
        assert_eq!(ctx["count"], 2);
        assert_eq!(ctx["prefix"], "lev/");
        assert_eq!(ctx["worktree_root"], "/wt");
        assert_eq!(ctx["primary"], "/repo");
    }

    #[test]
    fn the_end_context_omits_a_primary_that_did_not_resolve() {
        let ctx = end_context(&["/wt/a".to_string()], "lev/", Path::new("/wt"), None);
        assert!(ctx.get("primary").is_none());
    }

    #[test]
    fn the_end_context_names_no_single_worktree() {
        let ctx = end_context(&["/wt/a".to_string()], "lev/", Path::new("/wt"), None);
        for key in ["worktree", "branch", "issue", "slug", "apps"] {
            assert!(
                ctx.get(key).is_none(),
                "a run-level context must not carry `{key}`"
            );
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo nextest run -p devkit --bin devkit end_context`
Expected: FAIL to compile with `cannot find function 'end_context' in this scope`.

- [ ] **Step 3: Add the helper**

In `src/bin/devkit/issue/end.rs`, directly below `remove_contexts`:

```rust
/// The `after_end` render context. A run may have removed several worktrees,
/// so it carries the list rather than any one worktree's identity, and omits
/// `worktree`, `branch`, `issue` and `slug` entirely: there is no honest single
/// value for them.
fn end_context(
    removed: &[String],
    prefix: &str,
    worktree_root: &Path,
    primary: Option<&Path>,
) -> serde_json::Value {
    let mut ctx = serde_json::json!({
        "removed": removed,
        "count": removed.len(),
        "prefix": prefix,
        "worktree_root": worktree_root.display().to_string(),
    });
    if let Some(primary) = primary {
        ctx["primary"] = serde_json::Value::String(primary.display().to_string());
    }
    ctx
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run -p devkit --bin devkit end_context`
Expected: PASS, all three.

- [ ] **Step 5: Read the second hook list**

In `pub fn run`, extend the block added in Task 3 Step 5 with one line, directly after the `after_worktree_remove` binding:

```rust
    let after_end = cfg_hooks.map_or(empty, |h| h.after_end.as_slice());
```

- [ ] **Step 6: Extend the firing block**

Replace the whole hook-firing block added in Task 3 Step 7 with:

```rust
    // After the summary and after the prune: a hook sees every removal
    // finished, and its progress step cannot tear the report. A run that
    // removed nothing changed nothing on disk, so nothing fires.
    if !removed.is_empty() && !(after_worktree_remove.is_empty() && after_end.is_empty()) {
        match main.as_deref() {
            Some(root) => {
                let root = Path::new(root);
                for wt in &removed {
                    let Some(ctx) = remove_ctxs.get(wt) else {
                        continue;
                    };
                    crate::issue::hooks::run_all(
                        root,
                        "after_worktree_remove",
                        after_worktree_remove,
                        ctx,
                        &vars,
                        &steps,
                    );
                }
                let ctx = end_context(&removed, &prefix, &wt_root, Some(root));
                crate::issue::hooks::run_all(root, "after_end", after_end, &ctx, &vars, &steps);
            }
            // The worktree the command was run from is usually the one just
            // removed, so there is no directory left to inherit.
            None => eprintln!(
                "warning: after_worktree_remove and after_end hooks skipped: the main repository root did not resolve"
            ),
        }
    }
```

The per-worktree loop stays first: `after_end` runs after every `after_worktree_remove` has finished, which is the ordering the docs promise.

- [ ] **Step 7: Run the gate**

Run: `cargo nextest run --workspace --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 8: Verify by hand that `after_end` fires once per run**

```bash
git -C . worktree add ../devkit-worktrees/hook-smoke-1 -b hook-smoke-1 main
git -C . worktree add ../devkit-worktrees/hook-smoke-2 -b hook-smoke-2 main
printf '[hooks]\nafter_end = [["git", "init", "ran-once"]]\n' >> ~/.config/devkit/config.toml
```

Remove both in one `issue end --clean-worktree hook-smoke-1 hook-smoke-2` run from the primary clone. Confirm exactly one `Hook: git init ran-once` step. Then run `issue end` again with nothing to remove and confirm no hook step appears at all. Delete `ran-once`, the two branches, and revert the `~/.config/devkit/config.toml` edit.

- [ ] **Step 9: Commit**

```bash
git add src/bin/devkit/issue/end.rs
git commit -m "feat(issue): run after_end hooks once per end run" -m "Fires after every after_worktree_remove hook, only when the run removed
at least one worktree, so a tool whose view spans every worktree is
refreshed once instead of once per removal."
```

---

## Self-review

**Spec coverage.** Both keys (Task 2), the main-repo-root cwd and its skip-with-warning (Tasks 3 and 4), the firing conditions including the no-op run (Tasks 3 and 4), the serial ordering with `after_end` last (Task 4 Step 6), both context shapes (Tasks 3 and 4), fail-open inherited unchanged (Task 1), the runner extraction (Task 1), docs and schema (Task 2). The rejected alternatives need no task. The spec's testing section maps to Task 1 Step 1, Task 3 Step 1, and Task 4 Step 1.

**Types.** `run_all(cwd, key, hooks, ctx, vars, steps)` is called with the same argument order in Task 1 Step 4, Task 3 Step 7, and Task 4 Step 6. `removed` is a `Mutex<HashSet<String>>` inside the removal scope and a `Vec<String>` after it, shadowed once at Task 3 Step 6. `remove_ctxs` is a `HashMap<String, serde_json::Value>` keyed by the row's `worktree` string, which is also what `removed_in_order` returns, so the lookup in Task 3 Step 7 matches.

**Known follow-up, deliberately out of scope.** `end.rs` is past 650 lines and gaining three helpers. Splitting it is not part of this change.
