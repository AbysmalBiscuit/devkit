# CLI progress spinners for blocking commands

## Problem

Several user-facing CLI commands block for seconds-to-minutes on network, git, or
server-readiness work while printing nothing, so the user can't tell whether the
tool is working or hung. A few commands (`issue status`, `issue prs`,
`issue dashboard`) already animate `indicatif` spinners via a `Steps` helper; the
rest do not. Every command with notable delay should show progress, on a TTY,
without changing piped/MCP/CI output.

## Scope

Add progress indication to the eight blocking-but-silent commands:

| Command | Blocking work |
|---|---|
| `issue setup` | `git fetch origin`, worktree add, per-app `prep_apps` |
| `issue checkout-pr` | Linear/GitHub resolve, `gh pr view`, `git fetch`, worktree add, `gh pr checkout`, optional setup |
| `issue info` | `gather()` fetch (skipped under `--cache-only`) |
| `issue end` | `gather()` fetch + git worktree ops |
| `issue review` | `gh` PR ops, `git push`, optional Slack post |
| `devrun up` | baseline fetch/reset, server launch + readiness wait |
| `devkit auth` | Linear + Slack token validation |
| `devkit doctor` | concurrent Linear/Slack validation |

Out of scope (fast, local-only — no spinner): all of `portm`, `lockm`,
`devrun down/status/logs/config`, every `completions` subcommand.

## Design

### 1. Promote the helper to a shared crate

Move `src/bin/issue/spin.rs` to `crates/devkit-common/src/progress.rs`, exported as
`devkit_common::progress::Steps`. The `issue` binary drops `mod spin;` and imports
from `devkit_common::progress`; `devrun` and `devkit` gain access to the same
helper. Behavior is unchanged: bars draw on stderr and the whole group is hidden
when stderr is not a terminal, so pipes, redirects, MCP, and tests produce no
progress output.

### 2. Runtime step counts + auto-numbering

`Steps` gains an optional total and an interior counter so numbered, sequential
flows don't hand-maintain `[i/N]` prefixes:

```rust
pub struct Steps {
    mp: MultiProgress,
    total: Option<usize>,
    n: Cell<usize>,
}

impl Steps {
    pub fn new() -> Steps;              // unnumbered (concurrent displays)
    pub fn with_total(total: usize) -> Steps;  // numbered sequential flow

    /// Run `f` under a spinner, clearing the bar before returning — so the
    /// spinner never stays live across a `?`, a stdin prompt, or stdout output.
    pub fn during<T>(&self, msg: &str, f: impl FnOnce() -> T) -> T;
}
```

In numbered mode, `during` prefixes `[i/total] ` and increments the counter; in
unnumbered mode it passes the message through. `spinner()`/`bar()` are unchanged
and stay for the concurrent displays (`status`, `prs`, `devkit doctor`), which
show several labeled bars at once — numbering doesn't fit a parallel display.

Each command computes `total` before the first step, from already-known inputs:
`args.apps.len()`, `args.setup`, `--cache-only`, the classified identifier kind,
the count of servers to launch, which tokens were supplied.

### 3. Per-command wiring

Sequential commands build `Steps::with_total(N)` and wrap each blocking call in
`during(...)`. The spinner clears before any interactive prompt:

- **`issue checkout-pr`** — `resolve()` wraps only the Linear/GitHub probes and
  clears before `prompt_choice` reads stdin.
- **`issue review`** / **`devrun up`** — spinner cleared before any confirmation
  prompt.

Concurrent commands keep the existing `Steps::new()` + `spinner()` idiom:

- **`devkit doctor`** — one labeled spinner per validation, shown together.

`issue info` under `--cache-only` performs no fetch and shows no spinner.

## Invariants preserved

- Bars draw on stderr; hidden off-TTY. Piped / MCP / CI / test output is
  byte-for-byte unchanged.
- No spinner is ever live across an interactive prompt or across stdout result
  output (`during` clears first; concurrent callers `clear()` before printing).

## Testing

- Port the existing "bars hidden off-TTY" test to the new crate location; add a
  test that `with_total` + `during` numbers sequential steps and that an
  unnumbered `Steps` passes messages through.
- `cargo test --workspace` must stay green — existing command tests assert
  stdout, which is unaffected.
- Per-command spinner wiring is not meaningfully unit-testable (hidden off-TTY,
  and the harness is not a TTY); verified by running the binaries against a real
  terminal and by the unchanged-output guarantee.

## Open questions

None.
