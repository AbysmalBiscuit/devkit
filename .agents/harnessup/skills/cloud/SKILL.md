---
name: cloud
description: Use in a cloud session before starting an issue or feature, to find work already done on it and choose how to carry it out.
---

# Cloud workflow

## Bind the checkout to the issue

Start work on an issue with `issue setup --here <issue> --summary`. It binds this checkout and its assigned branch to the issue, so `issue pr create` writes the line that closes it, and it writes the issue summary, whose path it prints. What else it does and when it refuses is in the `--here` paragraph of the `using-devkit` skill's `references/issues.md`.

## Find existing work first

The GitHub issue is the plan. Read its description, `Done when`, `Stop and ask if`, `Out of scope`, comments and linked issues. Then look for work already done: commits on this branch, an open PR for it, and a committed spec or plan under `docs/superpowers/specs` and `docs/superpowers/plans`. Read a matching document fully and check that it still describes the request.

Say what you found, or where you looked. If the issue or a linked document is inaccessible, say so; inaccessible does not mean absent.

## Choose the workflow

- A spec and plan are ready: execute them, keeping their decisions. Revisit the design only when the current request or code exposes a concrete conflict or a missing decision.
- Requirements are clear and the change is small: implement directly. A bounded bug fix, a mechanical edit or an established pattern needs neither brainstorming nor a separate plan.
- Requirements are clear but the work takes several steps: write a plan with `/superpowers:writing-plans`, keeping the issue's and the spec's decisions.
- Material design decisions remain, such as unclear user behaviour, competing architectures, data ownership or compatibility trade-offs: use `/superpowers:brainstorming`. Investigate facts yourself; ask only for decisions that depend on the requester's intent.

End each new plan with its unresolved questions, separating those that block implementation from assumptions you can safely make.

To execute a plan, use `/superpowers:subagent-driven-development` when you can finish without human decisions, and `/superpowers:executing-plans` when decisions or review checkpoints are expected. Read the selected skill before following it; if it is unavailable, say so and follow the same workflow directly.
