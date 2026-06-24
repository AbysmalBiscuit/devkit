# Prep-File Content Templating Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render each app's `prep_files[].content` through the existing minijinja template engine at `issue setup`, so prep files can reference the issue context.

**Architecture:** Widen `write_prep_files` to take a render context and `variables`, and render `pf.content` via `devkit_common::template::render` before writing (only for files that will actually be written). The caller in `issue setup` builds a per-app context (the existing branch context plus `app`/`branch`/`worktree`) and passes it in. Always-on; plain content with no `{{ }}`/`{% %}` passes through unchanged.

**Tech Stack:** Rust 2024, `minijinja` (already wrapped by `devkit-common::template`, strict-undefined), `anyhow`.

**Spec:** `docs/superpowers/specs/2026-06-24-prep-file-templating-design.md`

**Worktree:** Per `AGENTS.md`, implement in a worktree — `git worktree add ../devkit-worktrees/prep-templating -b prep-file-templating main` — never on the primary clone's `main`.

---

### Task 1: Render prep-file content through templates

**Files:**
- Modify: `src/bin/issue/setup.rs` — `write_prep_files` (lines 27-43), its caller (lines 138-144), and the test module (lines 177-255)

Context you need from the current file:
- `write_prep_files` today writes `pf.content` byte-for-byte:
  ```rust
  fn write_prep_files(app_dir: &Path, files: &[PrepFile]) -> Result<()> {
      for pf in files {
          let target = app_dir.join(&pf.path);
          if pf.overwrite || !target.exists() {
              if let Some(parent) = target.parent() {
                  std::fs::create_dir_all(parent)
                      .with_context(|| format!("creating parent dir for prep file `{}`", pf.path))?;
              }
              std::fs::write(&target, &pf.content)
                  .with_context(|| format!("writing prep file `{}`", pf.path))?;
          }
      }
      Ok(())
  }
  ```
- The caller loop (lines 138-144):
  ```rust
  for a in &args.apps {
      let app = &catalog[a];
      let app_dir = worktree.join(&app.path);
      std::fs::create_dir_all(&app_dir).ok();

      write_prep_files(&app_dir, &app.prep_files)
          .with_context(|| format!("preparing files for app `{a}`"))?;
      // ... `for cmd in &app.setup` follows
  }
  ```
- Already in scope at the loop: `ctx` (the base `serde_json::Value` built at lines 56-61: `{prefix, issue, slug, apps}`), `vars` (`&cfg.templates.variables`, line 62), `branch` (`String`, line 63), `worktree` (`PathBuf`, line 71). `BTreeMap`, `Context`, `Path` are imported at the top of the file.
- Test module imports (lines 179-182): `use super::*;`, `use devkit_ports::config::Templates;`, `use serde_json::json;`, `use std::path::PathBuf;`. `use super::*` brings the parent's `use std::collections::BTreeMap;` into scope (same pattern as `template.rs` tests).

- [ ] **Step 1: Add a failing test for context rendering**

Add to the `mod tests` block in `src/bin/issue/setup.rs` (after the `scratch` helper, before the existing `writes_content_verbatim_and_creates_parents` test) two helpers and one test that already call the *new* four-argument signature:

```rust
    fn novars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn ctx() -> serde_json::Value {
        json!({"prefix": "lev/", "issue": "eng-1", "slug": "fix", "apps": ["web"], "app": "web"})
    }

    #[test]
    fn renders_issue_context() {
        let dir = scratch("render");
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "ISSUE={{ issue }}\n".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(".env.local")).unwrap(),
            "ISSUE=eng-1\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
```

- [ ] **Step 2: Run the test to verify it fails (compile error)**

Run: `cargo test -p devkit --bin issue setup:: 2>&1 | head -30`
Expected: compile failure — `write_prep_files` takes 2 arguments but 4 were supplied (and the existing 2-arg call sites also now mismatch once you change the signature). A compile failure is the expected "red".

- [ ] **Step 3: Widen `write_prep_files` to render content**

Replace the `write_prep_files` function with:

```rust
/// Write each prep file into `app_dir`. `content` is rendered as a minijinja
/// template against `ctx`/`vars` (strict undefined) before writing; parent
/// directories are created; an existing file is left untouched unless the entry
/// opts into `overwrite`. Only files that will be written are rendered.
fn write_prep_files(
    app_dir: &Path,
    files: &[PrepFile],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<()> {
    for pf in files {
        let target = app_dir.join(&pf.path);
        if pf.overwrite || !target.exists() {
            let rendered = devkit_common::template::render(&pf.content, ctx, vars)
                .with_context(|| format!("rendering prep file `{}`", pf.path))?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating parent dir for prep file `{}`", pf.path))?;
            }
            std::fs::write(&target, &rendered)
                .with_context(|| format!("writing prep file `{}`", pf.path))?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Build the per-app context at the call site**

In `run()`, replace the call-site block (the `write_prep_files(&app_dir, &app.prep_files)…` lines inside `for a in &args.apps`) with:

```rust
        let mut file_ctx = ctx.clone();
        if let Some(obj) = file_ctx.as_object_mut() {
            obj.insert("app".into(), serde_json::Value::String(a.clone()));
            obj.insert("branch".into(), serde_json::Value::String(branch.clone()));
            obj.insert(
                "worktree".into(),
                serde_json::Value::String(worktree.to_string_lossy().into_owned()),
            );
        }
        write_prep_files(&app_dir, &app.prep_files, &file_ctx, vars)
            .with_context(|| format!("preparing files for app `{a}`"))?;
```

(Leave the `let app = …; let app_dir = …; std::fs::create_dir_all(&app_dir).ok();` lines above it and the `for cmd in &app.setup` block below it unchanged.)

- [ ] **Step 5: Update the three existing `write_prep_files` tests to the new signature**

In each of `writes_content_verbatim_and_creates_parents`, `write_if_absent_preserves_existing`, and `overwrite_replaces_existing`, change the call from `write_prep_files(&dir, &files).unwrap();` to:

```rust
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
```

(No other change — their assertions stand. `writes_content_verbatim_and_creates_parents` now also proves plain content with single `{` braces in JSON passes through minijinja unchanged.)

- [ ] **Step 6: Add the remaining new tests**

Add to `mod tests`:

```rust
    #[test]
    fn renders_app_name() {
        let dir = scratch("appvar");
        let files = vec![PrepFile {
            path: "app.txt".into(),
            content: "{{ app }}".into(),
            overwrite: false,
        }];
        write_prep_files(&dir, &files, &ctx(), &novars()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("app.txt")).unwrap(), "web");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_var_is_an_error() {
        let dir = scratch("badvar");
        let files = vec![PrepFile {
            path: ".env.local".into(),
            content: "{{ nope }}".into(),
            overwrite: false,
        }];
        assert!(write_prep_files(&dir, &files, &ctx(), &novars()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p devkit --bin issue 2>&1 | tail -20`
Expected: all `setup::tests` pass (the 3 updated + `renders_issue_context` + `renders_app_name` + `unknown_var_is_an_error` + the 2 template-default tests).

- [ ] **Step 8: Run the full gate**

Run: `cargo test --workspace 2>&1 | rg "test result:" ` then `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5` then `cargo fmt --all --check`
Expected: every `test result:` line shows `0 failed`; clippy prints no warnings; fmt exits 0.

- [ ] **Step 9: Commit**

```bash
git add src/bin/issue/setup.rs
git commit -m "feat(issue): template prep-file content at setup"
```

---

### Task 2: Document prep-file templating

**Files:**
- Modify: `README.md:168`
- Modify: `docs/configuration.md:66`
- Modify: `docs/next-features.md` ("Configurable per-app prep step" RESOLVED note)

- [ ] **Step 1: Update the README bullet**

In `README.md`, replace the `prep_files` bullet (line 168) with:

```markdown
- `prep_files` declares files written into an app's directory during `issue setup` (before its `setup` commands). Each is `{ path, content, overwrite }`; `content` is rendered as a minijinja template with the issue context (`prefix`, `issue`, `slug`, `apps`, `app`, `branch`, `worktree`), and existing files are kept unless `overwrite = true`.
```

- [ ] **Step 2: Update the configuration.md table row**

In `docs/configuration.md`, replace the `prep_files` table row (line 66) with:

```markdown
| `prep_files` | no | Files written into the app's directory during `issue setup`, before `setup` commands run. Each entry is `{ path, content, overwrite }` — `path` is relative to the app dir (parent dirs created), `content` is rendered as a minijinja template with the issue context (`prefix`, `issue`, `slug`, `apps`, `app`, `branch`, `worktree`) plus `[templates].variables`, and `overwrite` (default `false`) keeps an existing file unless set to `true`. Emit a literal `{{` with `{% raw %}…{% endraw %}`. As an array, a deeper `devkit.toml` replaces the whole list rather than appending. |
```

- [ ] **Step 3: Update the next-features.md RESOLVED note**

In `docs/next-features.md`, under "## Configurable per-app prep step", replace the sentence:

```markdown
write-if-absent-only strategy are gone. Format/template generation and symlink mode
stay deferred to the messages-templates feature. The analysis below is kept for context.
```

with:

```markdown
write-if-absent-only strategy are gone. Content is now rendered as a minijinja template
(shipped 2026-06-24 — see `docs/superpowers/specs/2026-06-24-prep-file-templating-design.md`);
symlink mode stays deferred. The analysis below is kept for context.
```

- [ ] **Step 4: Verify the docs build/read cleanly**

Run: `rg -n "written verbatim|verbatim \`content\`" README.md docs/configuration.md docs/next-features.md`
Expected: no remaining "written verbatim"/"verbatim `content`" claims for `prep_files`.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/configuration.md docs/next-features.md
git commit -m "docs: document prep-file content templating"
```

---

## Notes for the executor

- Render happens only inside the `if pf.overwrite || !target.exists()` guard — a skipped (existing, non-overwrite) file is never rendered, so a stale template in skipped content cannot fail setup. The `unknown_var_is_an_error` test therefore targets a fresh path that will be written.
- The error chain on a bad template is `preparing files for app '<app>': rendering prep file '<path>': <minijinja error>` — the app name comes from the caller's existing `.with_context`, the path from `write_prep_files`. This satisfies the spec's "name the path and the app" requirement without duplicating the app name inside `write_prep_files`.
- Do not expose ports to the prep context (out of scope per the spec; ports are reserved after this loop).
