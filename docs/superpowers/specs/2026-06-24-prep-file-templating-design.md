# prep-file content templating design

**Status:** Approved 2026-06-24
**Scope:** The "Format/template generation" half of the "Configurable per-app prep
step" follow-up in `docs/next-features.md`, unblocked now that the message-templates
feature shipped `devkit-common::template`. Symlink mode (the other half) stays
deferred to a separate pass.

## Goal

Let an app's `prep_files[].content` reference the issue context — the issue id, slug,
selected apps, and the app the file belongs to — instead of being written byte-for-byte.
A prep file like `LINEAR_ISSUE={{ issue }}` is rendered to `LINEAR_ISSUE=ENG-123` at
`issue setup` time. This makes per-app prep files first-class templates, consistent with
`branch`, `worktree_dir`, `pr_title`, `pr_body`, and `slack`, which are already minijinja
templates under `[templates]`.

## Background

`issue setup` writes each app's `prep_files` into the app directory before running the
app's `setup` commands (`src/bin/issue/setup.rs:138-144`). `PrepFile` is:

```rust
pub struct PrepFile {
    pub path: String,      // target, relative to the app dir
    pub content: String,   // currently written verbatim
    #[serde(default)]
    pub overwrite: bool,   // overwrite an existing file rather than skipping
}
```

`write_prep_files(app_dir, files)` (`setup.rs:30`) writes `pf.content` byte-for-byte,
creating parent dirs and skipping an existing file unless `overwrite` is set.

Three lines above the prep loop, `setup.rs` already renders the `branch` and
`worktree_dir` templates via `devkit_common::template::render(template, &ctx, vars)`
with `ctx = { prefix, issue, slug, apps }` and `vars = cfg.templates.variables`.
`template::render` (`crates/devkit-common/src/template.rs`) is minijinja with
`UndefinedBehavior::Strict`: an unknown `{{ var }}` is a render error, and `variables`
supply user constants merged underneath `ctx` (a context field of the same name wins).

## Design

### Behavior

`write_prep_files` renders each `pf.content` through `template::render` before writing.
Rendering replaces only the *content source*; the write policy (`overwrite` / skip-if-exists)
and parent-dir creation are unchanged. Templating is **always on** — every `content` is a
template, matching the other `[templates]` strings. Plain content (no `{{ }}` / `{% %}`)
passes through minijinja unchanged, so every existing `prep_files` config behaves exactly
as before; only content that uses template syntax changes, which is the feature.

### Template context

The render context mirrors the `branch`/`worktree_dir` context plus a per-app field:

| Var | Value |
|---|---|
| `prefix` | `cfg.defaults.branch_prefix` |
| `issue` | `args.issue` |
| `slug` | `args.slug` |
| `apps` | `args.apps` (the full selected-apps list) |
| `app` | the app the current file belongs to |
| `branch` | the rendered branch name (already computed at `setup.rs:63`) |
| `worktree` | the worktree path string (already computed at `setup.rs:71`) |

`vars` = `cfg.templates.variables`, the same user-constant layer used everywhere else.

`branch` and `worktree` are included because they are already in scope at the prep loop and
are the natural things a prep file might reference; exposing them costs nothing.

### Out of scope: ports

Allocated ports are **not** exposed to prep templates. Ports are reserved *after* the prep
step (`setup.rs:157`), prep files are for static config, and ports already flow into running
servers through launch templating. Exposing them would require reordering allocation before
the prep write for marginal value; deferred.

### Interface

`write_prep_files` gains the render inputs:

```rust
fn write_prep_files(
    app_dir: &Path,
    files: &[PrepFile],
    ctx: &serde_json::Value,
    vars: &BTreeMap<String, String>,
) -> Result<()>;
```

The caller builds a per-app context inside the existing loop (the base `branch` context
plus `app`/`branch`/`worktree`) and passes it in. Keeping `ctx` a concrete
`serde_json::Value` keeps `write_prep_files` unit-testable without the rest of setup.

### Error handling

`template::render` is strict-undefined, so an unknown `{{ var }}` in content aborts setup,
contextualized as `rendering prep file '<path>' for app '<app>'` — the same discipline the
`branch`/`worktree_dir` renders already use. A file that must contain a literal `{{` escapes
it the minijinja way: `{% raw %}…{% endraw %}` or `{{ '{{' }}`. This is documented.

## Testing

TDD; `cargo test --workspace` is the merge gate.

| Unit | Tests |
|---|---|
| `write_prep_files` | `ISSUE={{ issue }}` → `ISSUE=ENG-123`; plain content written unchanged; `{{ app }}` → the app name; unknown var → render error; `overwrite=false` still skips an existing file (after rendering); `overwrite=true` rewrites it |

The three existing `write_prep_files` tests (`setup.rs:211-248`) are updated to pass the new
`ctx`/`vars` arguments; their existing assertions about overwrite/skip behavior stand.

## Docs

- `docs/configuration.md`: in the prep-files section, state that `content` is a minijinja
  template, list the available variables, and show how to emit a literal `{{`.
- `docs/next-features.md`: update the "Configurable per-app prep step" RESOLVED note —
  templating now shipped; symlink mode still deferred.
- `README.md`: if it documents `prep_files` content, add a one-line note that content is
  templated.

## Out of scope (deferred follow-up)

- **Symlink mode.** Linking a prep file to a shared secrets file instead of writing
  content, with cross-platform (Windows) symlink handling. Tracked in
  `docs/next-features.md` under "Configurable per-app prep step".
