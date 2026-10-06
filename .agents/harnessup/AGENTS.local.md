# Cloud session rules

This is a cloud session with no one watching it live. These rules add to `AGENTS.md`; where they conflict, they win for this session only.

## Autonomy

An implementation request authorizes the investigation, edits, tests, commits, push and PR it takes to finish. Continue to completion without asking whether to continue. A question asking for an assessment authorizes a read-only answer.

Resolve routine choices yourself and record each consequential assumption, with its reason, for the final report. Ask only when the answer would change scope or user-visible behaviour, and keep doing independent work while waiting; silence is not approval.

Before starting an issue or feature, load the `cloud` skill to find existing work and pick a workflow. Use the provided cloud checkout and assigned branch.

## Progress ledger

Keep `.superpowers/cloud-progress.md` current for multi-step work: the objective, the issue, the chosen workflow, completed work, the active step, verification results, consequential decisions, pending questions, the state of running agents or commands, and the next action. It is gitignored and outlives context compaction.

## Tooling and verification

Before using devkit's task runner or coordination tools, read `/devkit:using-devkit`. Before using mcpls for code intelligence, read `/mcpls:mcpls`.

For a bug fix, reproduce the failure through the entry point where it occurs, and watch the test fail for the actual bug before fixing it. Completion requires the requested behaviour, the checks `AGENTS.md` names passing, and blocking review findings resolved. Report anything you could not verify.

## Communication

Lead with the result or recommendation, followed by the evidence it needs. Disagree when the facts warrant it and state uncertainty plainly. Report work you did as results, not as instructions for someone else to run.

## Delivery

Commit through `devrun task commit`, staging only your own changes, and pass your model's name and email in its `coauthors` argument.

When the work is done, push and open the PR with `issue pr create`. It opens ready for review, not as a draft, because review bots skip drafts. Link the issue, explain the problem and the fix briefly, name the model and harness, and leave the `## TL;DR (human written)` section empty.
