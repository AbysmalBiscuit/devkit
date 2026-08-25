# Hook progress steps spec: review log

Adversarial cross-model review of
`2026-08-25-hook-progress-steps-design.md`. Critic: OpenAI Codex
(`gpt-5.6-sol`, reasoning effort `xhigh`), read-only, scoped to major issues
only, at most two rounds. Claude is the final arbiter on every point.

## Round 1: Codex

Verdict: REVISE. Three material problems.

1. **Render failures do not consume a step.** The total counts every hook, but
   a hook that fails to render was specified to warn before any bar exists.
   `Steps` advances its counter only inside `during_result`, so one unrenderable
   hook followed by one valid hook ends the run at `[3/4]`.

2. **`2 + apps + hooks.len()` is wrong if `apps` means `args.apps.len()`.** A
   nonempty app list runs inside a single `Preparing apps…` step, so the app
   term is `usize::from(!args.apps.is_empty())`, a flag rather than a count.

3. **The public config contract says hooks run before the JSON.** Reordering
   requires updating the `after_worktree_create` doc comment, the generated
   `schema/devkit-config.json`, and the `docs/configuration.md` hooks table.

Codex also confirmed, unprompted, that `MultiProgress::suspend` is the right
primitive, that `checkout-pr` has a `Steps` in scope at its hook call site, and
that no MCP action, test, or in-tree script depends on the old output ordering.

### Claude's response

All three verified against the source before being accepted, and all three
hold. Finding 1 is the one this review exists to catch: it is a real defect in
the design, not a wording problem.

**Accepted as written: 1, 2, 3.**

- **1** is confirmed at `crates/devkit-common/src/progress.rs:127`. `label()`
  performs the `n.fetch_add`, and it is reachable only through
  `during`/`during_result`. The fix makes every hook consume exactly one step:
  the render moves outside the closure to produce the label, and its `Err` is
  carried into `during_result` so the step still draws and settles `✗`. A
  failed render labels from the unrendered argv, which also makes the failing
  template visible in the step log rather than only in a stderr warning. That
  is better than the alternative fix of excluding failures from the total,
  which would have hidden a config error in a stray warning line.
- **2** is confirmed at `src/bin/issue/setup.rs:343`. The spec's prose was
  ambiguous where the code is not. It now carries the exact expression and
  states why `args.apps.len()` would overshoot.
- **3** was found independently before this round returned, at
  `docs/configuration.md:509` and `schema/devkit-config.json:318`. A new design
  section lists all three locations and requires regenerating the schema with
  `DEVKIT_UPDATE_SCHEMA=1 cargo test`. The existing drift test then keeps a
  stale description off `main`.

**Rejected: none.**

## Round 2: Codex

Verdict: APPROVED. No remaining material problems.

Confirmed against the revised spec:

- Render failures consume exactly one failed step while fail-open execution
  is preserved.
- The setup total matches the single optional app-preparation step plus every
  hook.
- The reorder covers all three affected documentation artifacts and the schema
  regeneration.
- No new caller breakage, deadlock, or output corruption was introduced.

### Outcome

Two rounds, converged. The review's material contribution was finding 1: the
step counter advances only inside `during_result`, so the original design would
have ended a run at `[3/4]` whenever a hook failed to render. That defect was
in the design, not the prose, and would have shipped.
