# Hook progress steps implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every `after_worktree_create` hook its own progress step, and print the worktree table before the hooks rather than after it.

**Architecture:** `run_hook` splits into a render half and a spawn half so a step's label can carry the rendered command. Each hook then runs inside `Steps::during_result`, including one that fails to render, whose step is labelled from its template source and settles as a failure. The `Prepared` table moves above the hook loop, wrapped in `Steps::suspend` so a live bar on stderr cannot tear it.

**Tech Stack:** Rust edition 2024, `anyhow`, `indicatif` via `devkit_common::progress`, `minijinja` via `devkit_common::template`, `schemars` for the committed config schema.

**Spec:** `docs/superpowers/specs/2026-08-25-hook-progress-steps-design.md`
**Review log:** `docs/superpowers/specs/2026-08-25-hook-progress-steps-review-log.md`

## Global Constraints

- Merge gate is all three, green, before any commit: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`.
- The fail-open hook contract does not change: a hook that fails to render, spawn, or exit zero warns on stderr and the remaining hooks still run.
- `Steps` must stay `Send + Sync`. `crates/devkit-common/src/progress.rs` holds a compile-time assert for this; do not add a non-`Sync` field.
- Display widths are named constants with a comment explaining the number, following `ui::BRANCH_DISPLAY_MAX`. No bare integer literals at the call site.
- Comments are timeless. No issue or PR references, no `now we` / `previously` / `used to`, no RED/GREEN narration in non-test code.
- Conventional Commits, imperative mood, subject at most 50 characters, lowercase after the colon, no trailing period.
- `schema/devkit-config.json` is generated. Never hand-edit it; regenerate with `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`.

---

### Task 1: A progress step for every hook

**Files:**
- Modify: `crates/devkit-common/src/progress.rs` (add `Steps::started`, add its test in the existing `mod tests` at line 238)
- Modify: `src/bin/issue/setup.rs:139-179` (split `run_hook`, add `hook_label`, rewrite the loop)
- Modify: `src/bin/issue/setup.rs:343` (step total)
- Modify: `src/bin/issue/setup.rs:417` (pass `&steps`)
- Modify: `src/bin/issue/checkout.rs:429-434` (pass `&steps`)
- Test: `src/bin/issue/setup.rs`, the existing `mod tests` block

**Interfaces:**
- Consumes: `devkit_common::progress::Steps` (already imported in both binaries), `devkit_common::ui::truncate(&str, usize) -> String`, `devkit_common::template::render`.
- Produces:
  - `Steps::started(&self) -> usize`
  - `const HOOK_LABEL_MAX: usize` in `setup.rs`
  - `fn render_hook(hook: &[String], ctx: &serde_json::Value, vars: &BTreeMap<String, String>) -> anyhow::Result<Vec<String>>`
  - `fn run_rendered(worktree: &Path, argv: &[String]) -> anyhow::Result<()>`
  - `fn hook_label(argv: &[String]) -> String`
  - `pub(crate) fn run_after_worktree_create(worktree: &Path, hooks: &[Vec<String>], ctx: &serde_json::Value, vars: &BTreeMap<String, String>, steps: &Steps)`

- [ ] **Step 1: Write the failing test for `Steps::started`**

Add to the existing `mod tests` in `crates/devkit-common/src/progress.rs`:

```rust
    #[test]
    fn started_counts_every_step_that_began() {
        let steps = Steps::persistent_with_total(2);
        steps.during("first", || ());
        steps.during_result("second", || anyhow::Ok(())).unwrap();
        assert_eq!(steps.started(), 2);
        steps.clear();
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p devkit-common --lib progress::tests::started_counts_every_step_that_began`

Expected: FAIL to compile, `error[E0599]: no method named 'started' found for struct 'Steps'`.

- [ ] **Step 3: Add the accessor**

In `crates/devkit-common/src/progress.rs`, inside `impl Steps`, next to `clear`:

```rust
    /// How many steps have begun. A step counts from the moment its label is
    /// minted, so a step still running is included. Callers outside this
    /// module assert step coverage with it; `label` is private.
    pub fn started(&self) -> usize {
        self.n.load(Ordering::Relaxed)
    }
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p devkit-common --lib progress::tests::started_counts_every_step_that_began`

Expected: PASS.

- [ ] **Step 5: Write the failing tests for the hook helpers**

Add to `mod tests` in `src/bin/issue/setup.rs`. These use the existing `ctx()` and `novars()` helpers already defined there.

```rust
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
        assert_eq!(label.chars().count(), "Hook: ".chars().count() + HOOK_LABEL_MAX);
    }
```

- [ ] **Step 6: Run them and watch them fail**

Run: `cargo test -p devkit --bin issue render_hook hook_label`

Expected: FAIL to compile, with `cannot find function 'render_hook' in this scope`, `cannot find function 'hook_label' in this scope`, and `cannot find value 'HOOK_LABEL_MAX' in this scope`.

- [ ] **Step 7: Split `run_hook` and add the label helper**

In `src/bin/issue/setup.rs`, replace the whole `run_hook` function (currently lines 139-160) with:

```rust
/// Width a hook's command is elided to in its progress step: what fits an
/// 80-column terminal beside the mark, the `[i/n]` counter, and the elapsed
/// time.
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

/// Run an already-rendered hook argv in `worktree`.
fn run_rendered(worktree: &Path, argv: &[String]) -> Result<()> {
    let (prog, rest) = argv.split_first().context("empty hook command")?;
    capture(
        prog,
        &rest.iter().map(String::as_str).collect::<Vec<_>>(),
        worktree.to_str(),
    )?;
    Ok(())
}

/// The progress-step label for a hook command.
fn hook_label(argv: &[String]) -> String {
    format!(
        "Hook: {}",
        devkit_common::ui::truncate(&argv.join(" "), HOOK_LABEL_MAX)
    )
}
```

- [ ] **Step 8: Run the helper tests and watch them pass**

Run: `cargo test -p devkit --bin issue render_hook hook_label`

Expected: PASS, four tests.

- [ ] **Step 9: Write the failing test for step coverage**

This is the regression test for the defect the spec review caught: a hook that skips its step leaves the run ending short of its total. Add to `mod tests` in `src/bin/issue/setup.rs`:

```rust
    #[test]
    fn every_hook_consumes_a_step_even_when_it_cannot_render() {
        let dir = scratch("hook-steps");
        let hooks = vec![
            vec![
                "git".to_string(),
                "init".to_string(),
                "{{ nope }}".to_string(),
            ],
            vec!["git".to_string(), "init".to_string(), "after".to_string()],
        ];
        let steps = Steps::persistent_with_total(hooks.len());
        run_after_worktree_create(&dir, &hooks, &ctx(), &novars(), &steps);
        assert_eq!(
            steps.started(),
            2,
            "an unrenderable hook must still consume its step"
        );
        assert!(dir.join("after").exists(), "the next hook still ran");
        std::fs::remove_dir_all(&dir).ok();
    }
```

- [ ] **Step 10: Run it and watch it fail**

Run: `cargo test -p devkit --bin issue every_hook_consumes_a_step`

Expected: FAIL to compile, `this function takes 4 arguments but 5 arguments were supplied`.

- [ ] **Step 11: Rewrite the hook loop**

In `src/bin/issue/setup.rs`, replace `run_after_worktree_create` (currently lines 162-179) with:

```rust
/// Run each `hooks.after_worktree_create` command in the new worktree, in
/// order, one progress step each. Fail-open: the worktree already exists and
/// is usable by the time these run, so a hook that fails warns on stderr and
/// the rest still run.
pub(crate) fn run_after_worktree_create(
    worktree: &Path,
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
        if let Err(e) = steps.during_result(&label, || run_rendered(worktree, &rendered?)) {
            eprintln!(
                "warning: after_worktree_create hook `{}` failed: {e:#}",
                hook.join(" ")
            );
        }
    }
}
```

- [ ] **Step 12: Update the three existing hook tests to the new signature**

Three existing tests in `src/bin/issue/setup.rs` call the old four-argument form. Each call is identical, and each becomes:

```rust
        run_after_worktree_create(&dir, &hooks, &ctx(), &novars(), &Steps::persistent());
```

Apply it in all three:

- `hook_renders_args_and_runs_in_the_worktree`
- `failing_hook_does_not_stop_the_next_one`
- `unrenderable_hook_does_not_stop_the_next_one`

`Steps` draws nothing under `cargo test` because stderr is not a terminal.

- [ ] **Step 13: Update the two production call sites**

`src/bin/issue/setup.rs:417` becomes:

```rust
    run_after_worktree_create(
        &worktree,
        &cfg.hooks.after_worktree_create,
        &hook_ctx,
        vars,
        &steps,
    );
```

`src/bin/issue/checkout.rs:429` becomes:

```rust
    crate::setup::run_after_worktree_create(
        &worktree,
        &cfg.hooks.after_worktree_create,
        &hook_ctx,
        &cfg.templates.variables,
        &steps,
    );
```

- [ ] **Step 14: Fix the step total**

`src/bin/issue/setup.rs:343` currently reads:

```rust
    let total = 2 + usize::from(!args.apps.is_empty());
```

Replace with:

```rust
    let total = 2 + usize::from(!args.apps.is_empty()) + cfg.hooks.after_worktree_create.len();
```

The app term stays a flag, not a count: any nonempty `--apps` list runs inside the single `Preparing apps…` step regardless of length. Adding `args.apps.len()` would overshoot.

- [ ] **Step 15: Run the whole gate**

Run: `cargo test --workspace`

Expected: PASS. `cargo clippy --workspace --all-targets -- -D warnings` clean, then `cargo fmt --all`.

- [ ] **Step 16: Commit**

```bash
git add crates/devkit-common/src/progress.rs src/bin/issue/setup.rs src/bin/issue/checkout.rs
git commit -m "feat(issue): show a progress step per worktree hook

after_worktree_create hooks ran in the foreground and printed nothing,
so a slow hook was indistinguishable from a hung command.

Split rendering out of the spawn so a step's label carries the rendered
command, and give every hook a step. A hook that cannot render keeps its
step, labelled from the template source: the counter advances only inside
during_result, so skipping the step would end the run short of its total."
```

---

### Task 2: Report the worktree before the hooks

**Files:**
- Modify: `src/bin/issue/setup.rs` (move the `Prepared` construction and `out.report()` above the hook call)
- Modify: `src/bin/issue/checkout.rs` (move `report(...)` above the hook call)
- Modify: `crates/devkit-config/src/lib.rs` (the `after_worktree_create` doc comment)
- Modify: `schema/devkit-config.json` (regenerated, never hand-edited)
- Modify: `docs/configuration.md` (the hooks table row)

**Interfaces:**
- Consumes: `run_after_worktree_create(..., &Steps)` from Task 1, `Steps::suspend<T>(&self, f: impl FnOnce() -> T) -> T`.
- Produces: no new signatures. This task changes statement order and documentation only.

- [ ] **Step 1: Move the report above the hooks in `setup.rs`**

In `src/bin/issue/setup.rs`, the tail of the setup function currently builds `hook_ctx`, runs the hooks, then builds `out` and reports. Reorder so `out` is built and reported first. `hook_ctx` must stay above the `Prepared` construction because it clones `holder`, which `Prepared` then moves:

```rust
    let mut hook_ctx = ctx.clone();
    if let Some(obj) = hook_ctx.as_object_mut() {
        obj.insert("branch".into(), serde_json::Value::String(branch.clone()));
        obj.insert("worktree".into(), serde_json::Value::String(holder.clone()));
    }

    // Ports are not reserved here. A worktree's servers get their ports
    // dynamically from `devrun up`, which allocates against the live registry at
    // start time — so the numbers always reflect what is actually free and no
    // unused reservation can be reclaimed by another session in the meantime.
    let out = Prepared {
        issue: issue.clone(),
        worktree: holder,
        branch,
        summary: summary_path,
    };
    // The worktree, its record, its includes and its apps are all in place by
    // now, so the table is not a premature claim. `suspend` hides the live bars
    // for the write: they draw on stderr and the table prints on stdout, and a
    // redraw would tear it.
    steps.suspend(|| out.report())?;

    run_after_worktree_create(
        &worktree,
        &cfg.hooks.after_worktree_create,
        &hook_ctx,
        vars,
        &steps,
    );
    Ok(())
```

- [ ] **Step 2: Move the report above the hooks in `checkout.rs`**

In `src/bin/issue/checkout.rs`, `report(meta.number, &meta.head_ref_name, worktree_s)?;` currently sits after the hook call. Move it above, wrapped the same way. Both `report` and the hook context borrow `worktree_s`, so no ownership change is needed:

```rust
    steps.suspend(|| report(meta.number, &meta.head_ref_name, worktree_s))?;

    crate::setup::run_after_worktree_create(
        &worktree,
        &cfg.hooks.after_worktree_create,
        &hook_ctx,
        &cfg.templates.variables,
        &steps,
    );
    Ok(())
```

- [ ] **Step 3: Correct the config doc comment**

`crates/devkit-config/src/lib.rs`, the `after_worktree_create` field. Replace `before the command prints its JSON` with the new ordering:

```rust
    /// Runs once in the root of a worktree `issue setup` or
    /// `issue checkout-pr` has just created, after its apps are prepared and
    /// after the command has reported the worktree. Each argv element is
    /// rendered as minijinja over `worktree`, `branch`, `issue`, `slug`,
    /// `apps`, `prefix`, and `[templates.variables]`.
    pub after_worktree_create: Vec<Vec<String>>,
```

- [ ] **Step 4: Run the schema drift test and watch it fail**

Run: `cargo test --test config_schema`

Expected: FAIL with a diff, because `schema/devkit-config.json` still carries the old description. The failure message names the regeneration command.

- [ ] **Step 5: Regenerate the schema**

Run: `DEVKIT_UPDATE_SCHEMA=1 cargo test --test config_schema`

This rewrites `schema/devkit-config.json` from the derived types. Do not hand-edit the JSON.

- [ ] **Step 6: Run the schema drift test and watch it pass**

Run: `cargo test --test config_schema`

Expected: PASS.

- [ ] **Step 7: Correct the configuration guide**

`docs/configuration.md`, the hooks table. The `after_worktree_create` row currently ends `before the command prints its JSON`. Replace the row with:

```markdown
| `after_worktree_create` | `issue setup` and `issue checkout-pr`, once the worktree exists and its apps are prepared, after the command has reported the worktree | the new worktree's root |
```

- [ ] **Step 8: Run the whole gate**

Run: `cargo test --workspace`

Expected: PASS. Then `cargo clippy --workspace --all-targets -- -D warnings` clean, then `cargo fmt --all`.

The reorder itself has no automated test. `report` writes through `println!`, so asserting its position relative to a hook's side effects would mean threading a writer through it, which is more than this change is worth. What to verify by reading instead: in both binaries the `report` call sits above the `run_after_worktree_create` call, and it is wrapped in `steps.suspend`.

- [ ] **Step 9: Commit**

```bash
git add src/bin/issue/setup.rs src/bin/issue/checkout.rs \
        crates/devkit-config/src/lib.rs schema/devkit-config.json docs/configuration.md
git commit -m "feat(issue): report the worktree before running hooks

The table naming the worktree printed only after the last hook returned,
withholding the path the command exists to produce for as long as the
slowest hook took.

Everything durable is in place before the hooks run, so the report moves
above them, suspended so a bar redrawing on stderr cannot tear the stdout
write. Three places documented the old ordering; all three change with it."
```
