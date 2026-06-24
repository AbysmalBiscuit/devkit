# Config templating design

**Status:** Approved 2026-06-24
**Scope:** Items 2 & 3 from `docs/next-steps.md` — "Configurable templates for
messages" and "Configurable templates for issue start".

## Goal

Let users define reusable, config-driven templates for the strings devkit
generates during the issue lifecycle, instead of hardcoding their format or
retyping them on every invocation. Five targets:

| Target | Today | Render site |
|---|---|---|
| `branch` | hardcoded `{prefix}{slug}` | `issue setup` |
| `worktree_dir` | hardcoded `{slug}` | `issue setup` |
| `pr_title` | `--pr-title`, required to create | `issue review` |
| `pr_body` | `--pr-body`, else empty | `issue review` |
| `slack` | `compose_text(body, pr_url) = "{body} {pr_url}"` | `issue review` |

## Engine

A new `template` module in `devkit-common` (alongside `slack`/`linear`) wrapping
[minijinja]:

```rust
pub fn render(template: &str, ctx: &impl Serialize) -> Result<String>
```

- minijinja chosen over tera: its only hard dependency is `serde` (already in the
  tree with `default-features = false`), so it adds ≈one crate; tera pulls
  `pest`/`regex`/`chrono`/`globwalk`/`unic-*`. Jinja syntax (`{{ name }}`,
  `{% if %}`, `{% for %}`) gives conditionals/loops for PR bodies.
- One `Environment` per render with `UndefinedBehavior::Strict`, so a typo'd
  `{{ isue }}` errors loudly instead of rendering empty.
- The context is a per-site `Serialize` struct. `[templates.variables]` constants
  merge in as the lowest layer; a context variable of the same name wins.
- The launch-argv `{port}` substitution in `run.rs` is unrelated and unchanged —
  it is a literal replace, not Jinja, so there is no collision.

## Config schema

A single global `[templates]` table in `devkit.toml`, layered and deep-merged
through the existing resolver (a repo-level `devkit.toml` overrides the home one
per key). Not per-app, not per-person.

```toml
[templates]
branch       = "{{ prefix }}{{ issue }}-{{ slug }}"
worktree_dir = "{{ slug }}"
pr_title     = "{{ issue }}: {{ input }}"
pr_body      = "Closes {{ issue }}.\n\n{{ input }}"
slack        = "{{ pr_title }}\n{{ input }}\n{{ pr_url }}"

[templates.variables]          # optional user constants
team           = "platform"
review_channel = "#code-review"
```

```rust
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct Templates {
    pub branch: Option<String>,
    pub worktree_dir: Option<String>,
    pub pr_title: Option<String>,
    pub pr_body: Option<String>,
    pub slack: Option<String>,
    #[serde(default)]
    pub variables: std::collections::BTreeMap<String, String>,
}
```

- Added to `Config` as `#[serde(default)] pub templates: Templates`, so a config
  with no `[templates]` table deserializes to all-defaults (every field `None`,
  empty `variables`).
- Each `Option<String>`: `Some` → use it; `None` → the built-in default below.
  Resolved once at the render site
  (`cfg.templates.branch.as_deref().unwrap_or(DEFAULT_BRANCH)`).
- `Templates` derives `Serialize`, so merged templates and their per-leaf
  provenance appear in `devrun config show` like any other config.

### Built-in defaults (backward compatibility)

The defaults reproduce today's exact output, so an unconfigured `devkit.toml`
behaves identically:

| Target | Default template |
|---|---|
| `branch` | `{{ prefix }}{{ slug }}` |
| `worktree_dir` | `{{ slug }}` |
| `pr_title` | `{{ input }}` |
| `pr_body` | `{{ input }}` |
| `slack` | `{{ input }} {{ pr_url }}` |

## Per-site contexts and render order

### Setup site (`issue setup`)

Renders `branch` then `worktree_dir`. No `input` (setup has no free-text flag).

| Variable | Source |
|---|---|
| `prefix` | `defaults.branch_prefix` |
| `issue` | `setup` arg |
| `slug` | `setup` arg |
| `apps` | `setup` arg (list) |

Order: render `branch` → render `worktree_dir` (joined under `worktree_root`) →
create worktree → write `.devkit/issue.toml` record.

### Review site (`issue review`)

Base context, shared by all three templates:

| Variable | Source |
|---|---|
| `issue`, `slug`, `apps` | `.devkit/issue.toml` record |
| `branch` | `git rev-parse --abbrev-ref HEAD` |
| `reviewer` | resolved GitHub handle |
| `to` | reviewer alias (`--to`) |

Each template additionally gets an `input` (its flag, empty string when omitted)
and the prior render outputs:

| Template | `input` flag | extra context |
|---|---|---|
| `pr_title` | `--pr-title` | — |
| `pr_body` | `--pr-body` | `pr_title` |
| `slack` | `body` (positional) | `pr_title`, `pr_url` |

Order: `pr_title` → `pr_body` → create/reuse PR (yields `pr_url`) → `slack` →
post. For a **reused** PR (add-reviewer path), `pr_title` is still rendered for
the Slack context but not sent to GitHub; `pr_url` comes from the existing PR.

### CLI changes

- **`body` becomes optional.** It is a required positional today; to let the
  template alone drive the message it becomes optional (default `""`). No
  behavior change when supplied.
- **Empty-title guard preserved.** When a PR must be *created* and the rendered
  `pr_title` is empty (no flag + default template), error with the existing
  message `--pr-title is required to create a PR`. The reuse path is unaffected.

## Setup record

`issue setup` writes `<worktree>/.devkit/issue.toml` after creating the worktree
(skipped under `--dry-run`):

```toml
issue = "ABC-123"
slug  = "fix-login"
apps  = ["web", "api"]
```

A small `#[derive(Serialize, Deserialize)]` struct written with `toml`.
`issue review` finds the worktree root via `git rev-parse --show-toplevel` and
reads it.

**Missing-record behavior (graceful).** If the file is absent (worktree predates
this feature, or is hand-made), `issue`/`slug`/`apps` are simply not in the
context. The default review templates reference only `input`/`pr_url`, so the
unconfigured path keeps working. A custom template that references `{{ issue }}`
with no record hits strict-undefined, which is caught and re-raised as:
*"template references `issue` but no `.devkit/issue.toml` found in `<worktree>` —
was it created by `issue setup`?"*

## Global gitignore

During `issue setup` (unless `--no-gitignore` is passed), ensure `.devkit/` is
globally ignored so the record never appears in any repo and the monorepo's
tracked `.gitignore` is never touched:

1. Resolve git's global excludes path: `git config --global core.excludesfile`
   if set (expand `~`), else the XDG default `${XDG_CONFIG_HOME:-~/.config}/git/ignore`
   (the path git reads by default).
2. Create parent dirs + the file if missing.
3. If no line equals `.devkit/` or `.devkit`, append `.devkit/` and print a
   one-line notice (*"added `.devkit/` to `<path>`"*). Idempotent — a no-op on
   every later run.

Append-only; never rewrites the file. `--no-gitignore` skips this step entirely
(the record is still written). I/O errors here are **non-fatal**: warn and
proceed — failing to update a convenience file must not abort `issue setup`
(same fail-open instinct as cgroup cap setup).

## Error handling

- Render failures (strict-undefined, malformed `{% %}`) surface as `anyhow`
  errors with `.context()` naming the offending template key (e.g. *"rendering
  `pr_body` template"*), so a config typo points at the right line.
- Missing-record-but-template-needs-`issue` → the dedicated message above.
- Empty rendered `pr_title` on the PR-create path → the preserved
  `--pr-title is required to create a PR` error.
- Global gitignore I/O errors → warn and proceed.

## Test plan

TDD; `cargo test --workspace` is the merge gate.

| Unit | Tests |
|---|---|
| `template::render` | substitution; `{% if %}`/`{% for apps %}`; strict-undefined errors; `variables` merge + context-wins-over-constant |
| Default templates | each default reproduces today's exact output |
| `Templates` deserialize | absent `[templates]` → all-`None` + empty map; partial table → only set keys override |
| Setup record | round-trips `issue/slug/apps`; review reads it; missing file → default templates render, custom-with-`{{ issue }}` errors with the helpful message |
| Render order | `pr_title` feeds `pr_body`; `pr_title`+`pr_url` feed `slack`; reuse-PR path still renders slack context |
| Global gitignore | appends `.devkit/` when absent; idempotent when `.devkit/` or `.devkit` present; `--no-gitignore` skips; unwritable path warns, does not abort |

Pure-function tests (`render`, defaults, record round-trip) are unit tests; the
git/`gh`-touching paths follow the existing review/setup test patterns.

[minijinja]: https://docs.rs/minijinja
