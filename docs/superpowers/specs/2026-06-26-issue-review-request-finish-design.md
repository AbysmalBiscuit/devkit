# Design: split `issue review` into `request` and `finish`

**Date:** 2026-06-26
**Status:** approved

## Goal

Today `issue review` only covers the author side of a review: push the branch,
open/reuse the PR, add a reviewer, and Slack them the PR link. There is no
counterpart for the reviewer to announce that they have *finished* reviewing and
what the outcome was.

Turn `review` into a subcommand container with two halves of the loop:

- `issue review request` — the author asks for review (today's behaviour, generalized).
- `issue review finish` — the reviewer announces, over Slack, that they are done.

Both gain a multi-target recipient model (`--to`, repeatable, people *or* channels)
and call-time template arguments (`--arg`). The engine stays project-agnostic; all
project-specific values (people aliases, templates, PR base) stay in `devkit.toml`.

This is a **breaking change**: bare `issue review …` no longer works, and the
`templates.slack` config key is renamed.

## CLI surface

`review` becomes a subcommand container:

```
issue review request [BODY] [--to <alias|#channel>]… [--base <ref>] \
                     [--pr-title <T>] [--pr-body <B>] [--no-push] [--arg k=v]…
issue review finish  [BODY] [--to <alias|#channel>]… [--pr <number>] [--arg k=v]…
```

`BODY` fills each template's `{{ input }}`, as today. The global `-C/--dir` and
`--config` flags carry over.

## Recipient model (`--to`, shared by both commands)

`--to` is repeatable. Each value is exactly one of:

- **Channel** — starts with `#`. Passed verbatim to `chat.postMessage`. Slack-only;
  never added as a GitHub reviewer. (`#` is a safe discriminator: a `[people]`
  alias cannot start with it.)
- **Person** — anything else; must be a key under `[people]`. Not found ⇒ hard
  error. Resolves to `person.slack` (for Slack) and `person.github` (for GitHub).

A shared helper resolves a `--to` list into a list of *targets*, each carrying its
Slack destination, an optional GitHub login, and a display `name` (see Templates).

### Reverse lookup (login → person)

A shared helper maps a GitHub login to the `[people]` alias whose `github` matches
(case-insensitive). Used when `--to` is omitted. Logins with no matching alias are
**warned and skipped**, never fatal.

## `request` behaviour

Worktree-first, as today: `guard_branch` (refuse `main`/`staging`/`HEAD`), `git push`
unless `--no-push` (never force-push), create-or-reuse the PR, render `pr_title` /
`pr_body`.

Reviewer set + recipients:

- **With `--to`:** the people among the targets are added as GitHub reviewers via
  their `github` login. Aliases with no/unresolvable `github` are **warned and
  skipped for GitHub** but still Slacked. Channels are Slack-only.
- **Without `--to`:** read the PR's existing **human** reviewers (`reviewRequests`,
  filtering out `[bot]` logins), reverse-lookup each login to a `[people]` alias,
  then **re-request their review on GitHub and Slack them**.
  - If the PR has zero human reviewers and no `--to` was given ⇒ hard error
    (`no reviewers on the PR and no --to given`).

Then render `review_request` per target and Slack each one.

## `finish` behaviour

No GitHub mutation at all — Slack only.

- **PR resolution:** inside a worktree, resolve the branch from `HEAD` and look up
  its PR (`gh pr list --head`). Otherwise `--pr <number>` is required (errors if
  neither a worktree PR nor `--pr` is available).
- **Recipients:** `--to` if given; otherwise reverse-lookup the **PR author**
  (author login → alias → slack).
- Render `review_finish` per target and Slack each one.

## Templates

Breaking config rename plus one new key. Each falls back to a `DEFAULT_*` constant.

| Key | Default | Used by |
|---|---|---|
| `templates.review_request` (was `templates.slack`) | `{{ input }} {{ pr_url }}` | `request` |
| `templates.review_finish` (new) | `{{ input }} {{ pr_url }}` | `finish` |

The `finish` default is intentionally minimal and parallel to `request`; override
it in config for "done reviewing" phrasing.

`pr_title` / `pr_body` templates are unchanged and used only by `request`'s create
path.

### Render context

Rendered **once per recipient** (so messages can personalize), then sent. Fields:

- Always: `branch`, `pr_url`, `pr_title`, `input`, plus `[templates.variables]`
  (and any `--arg` overrides).
- When the issue record (`.devkit/issue.toml`) exists: `issue`, `slug`, `apps`.
- `finish` also exposes `author` (the PR author login).
- **Per recipient:**
  - `name` — the `[people]` alias for a person target, or the channel string
    (e.g. `#eng`) for a channel.
  - `slack_id` — the person's Slack `U…` id (empty for channels), so a template can
    ping with `Hey <@{{ slack_id }}>, …` rather than printing the alias.

## `--arg key=value`

Repeatable, both commands. Each key **must** already exist in
`[templates.variables]` (the pre-declared allowlist) or it is a hard error; the
value overrides that variable's default for this render only. Parsed once,
validated against the variables map, then merged over the base variables in the
render context.

## Code structure

Promote `src/bin/issue/review.rs` to a `review/` module:

- `review/mod.rs` — subcommand dispatch + shared helpers: `--to` target
  resolution, login→person reverse lookup, `--arg` parse/validate, the render
  wrappers, and the per-recipient Slack fan-out.
- `review/request.rs` — today's logic, adapted to the multi-target model.
- `review/finish.rs` — new.

`Cmd::Review` in `main.rs` becomes a container whose variant holds a
`ReviewCmd { Request{…}, Finish{…} }` subcommand enum.

No MCP changes: `review` is CLI-only, and the `devkit-issue` / MCP facades stay
read-only triage. The `Slack` fan-out reuses `devkit_common::slack::post_message`
and the env→`secrets.toml` token resolution (`SLACK_TOKEN`); with no token, fall
back to printing the resolved intent (as today), now one entry per target.

## Out of scope / follow-ups

- `issue review` (bare) is removed; the `issue-review` skill must be updated to
  `issue review request`, and a new finish skill is a follow-up.
- `finish` posts nothing to GitHub (no `gh pr review`); it is an announcement only.
- No per-PR outcome enum (approve/request-changes/comment) — nothing is posted to
  GitHub, so the outcome lives in the free-text `BODY` / `review_finish` template.

## Testing

Pure-function unit tests (mirroring the existing `review.rs` tests):

- `--to` classification (channel vs person vs unknown-alias error).
- Reverse lookup (login → alias, case-insensitive; unmatched → skipped).
- `--arg` validation (unknown key errors; known key overrides default).
- Per-recipient context binding (`name` / `slack_id` for person vs channel).
- `request` reviewer partition (people with/without `github`; channels excluded).
- `finish` PR resolution precedence (worktree branch vs `--pr`).

`cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`
stay the merge gate.
