# Foreground hook progress steps

**Date:** 2026-08-25
**Status:** proposed

## Problem

`hooks.after_worktree_create` commands run in the foreground of `issue setup`
and `issue checkout-pr`, and they print nothing while they run.
`run_after_worktree_create` is a bare loop over `capture`, which collects the
child's stdout instead of forwarding it, so a slow hook is indistinguishable
from a hung command.

A representative `issue setup --timing=trace` run:

```
+  5.59s     10ms  zoxide          zoxide add /home/lev/Git/adaptyv/swe-11265-…
+  5.60s   11.23s  bun             bun install
+ 16.83s    487ms  bash            bash -c alacritree.exe project refresh …
timing — wall 17.31s
```

Two thirds of the wall clock is a dependency install the user watches in
silence. The worktree itself is ready at roughly 5.6s, but the table naming its
path prints only after the last hook returns, so the one fact the command exists
to produce is withheld for the duration.

## What already works (verified, no change needed)

- **Hooks are already timed.** `capture` opens a `timing::subprocess_span` on
  every spawn (`crates/devkit-common/src/cmd.rs:6`), so `--timing=trace`
  attributes wall time per hook today, keyed by program with the full rendered
  argv as the detail.
- **`Steps` is the house progress facility.** `issue setup` uses
  `Steps::persistent_with_total`, `issue checkout-pr` uses `Steps::persistent`.
  Both hide themselves when stderr is not a terminal, so pipes, MCP, and tests
  emit nothing.
- **`Steps::suspend` exists for exactly the tearing case** this design
  introduces: a stdout write racing a live bar redrawing on stderr.
- **`ui::truncate` elides on a glyph boundary**, so a long rendered command can
  be shortened without splitting an escape sequence.
- **`report()` already branches on `stdout_is_tty()`**
  (`src/bin/issue/setup.rs:40`): JSON when stdout is a pipe, a key/value table
  when it is a terminal.

## Design

### 1. Split rendering from spawning

`run_hook` renders each argv element and spawns the result in one function.
Split the render half out as `render_hook(hook, ctx, vars) -> Result<Vec<String>>`.

The loop renders a hook before it draws that hook's bar, so the step label
carries the rendered command (`zoxide add /home/lev/…`) rather than its
template source (`zoxide add {{ worktree }}`).

Rendering stays interleaved with execution rather than hoisted into one upfront
pass. It could be hoisted safely, since rendering is pure expansion over a
`ctx`/`vars` pair no hook can alter, but interleaving keeps the change smaller
and keeps a hook's failure adjacent to its own step.

### 2. One step per hook

`run_after_worktree_create` takes a `&Steps` and runs each hook inside
`Steps::during_result`, so a hook that fails settles as `✗` rather than `✓`.
The error is still swallowed with the existing stderr warning: the documented
fail-open contract does not change, only its visibility.

**Every hook consumes exactly one step, including one that fails to render.**
`Steps::label` calls `n.fetch_add` (`crates/devkit-common/src/progress.rs:127`)
and is reached only through `during`/`during_result`, so a hook that skips its
step never advances the counter and a run with one unrenderable hook would end
at `[3/4]`. The render therefore happens outside the closure to produce the
label, and its `Err` is carried *into* `during_result` so the step still draws
and settles `✗`:

- render succeeds: label from the rendered argv, closure spawns it.
- render fails: label from the unrendered argv, so the failing template is
  visible in the step log; the closure returns the render error immediately.

The unrendered fallback label is the reason the failure is legible at all. A
config typo shows as `✗ [3/5] Hook: git init {{ nope }}` in the same log as
everything else, not only as a stray stderr warning.

`during_result` returns the hook's `Result`, and the loop warns on the `Err`
after the step has settled. The mark and the warning therefore always agree,
and the warning lands below the `✗` line rather than being overdrawn by it.

The label is `Hook: <command>`, passed through
`ui::truncate` at 56 characters. That is what fits an 80-column terminal
alongside the mark, the `[i/n]` counter, and the trailing elapsed time. Unlike
the other step messages the label carries no trailing `…`, because `truncate`
appends its own ellipsis when it elides and two would read as a typo.

### 3. Honest numbering

`issue setup`'s step total is currently
`2 + usize::from(!args.apps.is_empty())` (`src/bin/issue/setup.rs:343`). The
app term is a flag, not a count: a nonempty `--apps` list runs inside one
`Preparing apps…` step regardless of length. The new total is therefore

```rust
let total = 2 + usize::from(!args.apps.is_empty())
    + cfg.hooks.after_worktree_create.len();
```

Adding `args.apps.len()` instead would overshoot on any multi-app setup.

Because every hook consumes a step, `hooks.len()` is exactly right and the run
always ends on `[n/n]`. `issue checkout-pr` is unnumbered and needs no change.

### 4. Report before the hooks, not after

Move `out.report()` above `run_after_worktree_create` in both binaries, wrapped
in `Steps::suspend`.

Everything durable already happens before the hooks run: the `.devkit/issue.toml`
record, the global gitignore entry, the `worktree_include` backfill, and per-app
prep. The worktree is genuinely ready at that point, so printing there is not a
premature claim. `suspend` keeps the table from being torn by a bar redraw in
the case where stdout and stderr are both terminals.

Resulting output:

```
✓ [1/5] Fetching from origin… (1.8s)
✓ [2/5] Creating worktree… (3.5s)
 issue     SWE-11265
 worktree  /home/lev/Git/adaptyv/swe-11265-non-idempotent-create-foreign
 branch    lev/swe-11265-non-idempotent-create-foreign
✓ [3/5] Hook: zoxide add /home/lev/Git/adaptyv/swe-11265-non-… (10ms)
✓ [4/5] Hook: bun install (11.2s)
✓ [5/5] Hook: bash -c alacritree.exe project refresh "$(dirna… (487ms)
```

The table sits between steps 2 and 3. That is the honest picture: the worktree
exists and here is where, and the remaining steps are extras configured on top.

### 5. Correct the documented hook contract

The hook timing is documented in three places, all of which currently say hooks
run *before* the command prints its JSON. Reordering makes all three false, so
they change with the code:

| Location | What it is |
|---|---|
| `crates/devkit-config/src/lib.rs`, the `after_worktree_create` doc comment | the source of truth; `JsonSchema` derives the schema description from it |
| `schema/devkit-config.json` | generated and committed; a test fails with a diff when it drifts |
| `docs/configuration.md`, the hooks table row | the user-facing description |

The wording becomes "after its apps are prepared, once the worktree is
reported" or equivalent. Regenerate the schema with
`DEVKIT_UPDATE_SCHEMA=1 cargo test` rather than hand-editing the JSON.

Leaving these stale would be worse than the reorder itself: the schema is
attached to every GitHub Release, so a wrong description ships to anyone
pointing their editor at it.

## The one contract this changes

A consumer that streams `issue setup`'s stdout line by line now sees the JSON
before the hooks have finished, and could act on a worktree whose hooks are
still running. A `$(...)` capture is unaffected, because the pipe does not
reach EOF until the process exits.

No streaming consumer is known in-tree or in the MCP surface. This is recorded
because it is the only externally observable behavior change, not because a
caller is known to depend on the old ordering.

## What this does not do

It does not give the shell back. The process still runs until the last hook
exits, so the worktree path becomes readable earlier but not usable in the same
terminal any sooner. Detached hook execution is
[issue #20](https://github.com/AbysmalBiscuit/devkit/issues/20).

Hook output is still captured rather than forwarded. A hook that prints
progress of its own stays invisible; the step shows only that it is running and
how long it took.

## Testing

- The three existing hook tests (`hook_renders_args_and_runs_in_the_worktree`,
  `failing_hook_does_not_stop_the_next_one`,
  `unrenderable_hook_does_not_stop_the_next_one`) take the new signature. They
  stay silent under `cargo test` because `Steps` hides itself when stderr is not
  a terminal.
- New: `render_hook` returns the rendered argv. The rendered form is now
  user-visible text rather than an internal detail, so it earns a test.
- No test asserts that a bar drew. `Steps` is hidden off-TTY by design, so such
  a test would either pin nothing or pin the hiding rule that already has
  coverage.
- The step-coverage rule in section 2 earns a regression test, since a stray
  early return on a render failure would silently reintroduce the short count.
  Asserting it needs a reader for the counter: `progress.rs` already asserts
  step counts in its own tests through the private `label`, which the `issue`
  binary cannot reach, so `Steps` gains a small `started()` accessor.
- The existing schema drift test covers the doc-comment change in section 5: it
  fails until `schema/devkit-config.json` is regenerated, so the stale
  description cannot reach `main` unnoticed.

## Out of scope

- The two-lane hook model (`[hooks.background]`), `.devkit/hooks.toml`,
  `.devkit/hooks.log`, the `issue status` row, the `devkit brief` section, and
  the `issue end` live-chain guard. All of that is issue #20, which lists this
  work as its prerequisite.
- Streaming hook output to the terminal.
- Any change to `capture`, the timing spans, or the hook config schema.
