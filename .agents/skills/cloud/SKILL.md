---
name: cloud
description: Use when starting issue or feature work in a cloud agent session, or recovering that work after resume or compaction.
disable-model-invocation: false
user-invocable: false
---

# Cloud workflow

Read the repository's instructions before working. This file supplies my cloud workflow preferences. Its workflow choices override skill defaults that require brainstorming for every change or always prefer subagents.

## Tooling

This project uses `devkit` as its agentic task runner. Run the `/devkit:using-devkit` skill to learn about it.
You also have `mcpls` to interact with rust-analyzer. Run the `/mcpls:mcpls` skill to learn more about it.

## Find existing work first

Before implementing an issue or feature, read its description and available discussion. Check if the repository already has a committed spec (in `docs/superpowers/specs`) and/or plan (in `docs/superpowers/plans`).

Read matching documents fully. Check that they describe the current request, identify acceptance criteria, and provide an actionable implementation plan. Check existing code, commits, and progress records for completed work. Resume from the first unfinished task.

State which spec and plan you found, or where you searched if none matched. If an issue or linked document is inaccessible, report that limitation; inaccessible does not mean absent.

## Choose the workflow

- Spec and plan are ready: proceed to execution. Reuse their decisions. Revisit design only when current requirements or code expose a concrete conflict or missing decision.
- Spec is ready but the plan is missing: use /superpowers:writing-plans for work that needs multiple implementation steps. Preserve the existing design.
- Requirements are clear and the change is small: implement directly. A bounded bug fix, mechanical edit, or established pattern usually needs neither brainstorming nor a separate plan.
- Material design decisions remain: use /superpowers:brainstorming. Examples include unclear user behavior, competing architectures, changes to data ownership, or compatibility tradeoffs. Investigate facts yourself; ask me for decisions that depend on my intent.

End each new plan with unresolved questions, if any. Distinguish questions that block implementation from assumptions you can safely make.

For a ready implementation plan:

- Use `/superpowers:subagent-driven-development` when subagents are available and you can complete the work without expected human decisions. If something major or unexpected happens, stop and ask your human for help.
- Use `/superpowers:executing-plans subagent driven` when human decisions or review checkpoints are expected.

Read the selected skill before following it. If it is unavailable, report that and follow the same workflow directly where the available tools allow.

## Exercise judgment

An implementation request authorizes the necessary investigation, edits, tests, and fixes. Continue through completion without asking whether to continue. Questions asking for an assessment authorize a read-only answer.

Resolve routine implementation choices yourself. Record consequential assumptions and deviations, with reasons, for the final report. Ask when a missing decision would materially change scope or user behavior. Continue independent work while waiting; silence is not approval.

Use the provided isolated cloud checkout and assigned branch when suitable.

## Maintain progress

For multi-step work, update the selected workflow's progress ledger as tasks finish or the next action changes. If the workflow has no ledger, use `.superpowers/cloud-progress.md`. Record the objective, spec and plan paths, completed work, active task, verification results, consequential decisions, and next action. Include any pending question or approval and the state of running agents or commands.

After compaction or resume, follow [the recovery brief](references/recovery.md). The existing task and its progress determine the next action.

## Implement and verify

Keep changes focused. Comments explain non-obvious reasons and invariants; keep them standalone and timeless.

For a bug fix, reproduce the failure through the entry point where it occurs. When using TDD, observe the test fail for the actual bug before implementing the fix. Choose tests that verify behavior.

Use the repository's prescribed checks. Completion requires the requested behavior, relevant passing checks, and resolution of blocking review findings. Report anything you could not verify.

## Communicate and deliver

Lead with the result or recommendation, followed by necessary evidence. Be direct. Disagree when the facts warrant it and state uncertainty plainly. Report work you performed as results, not instructions for me to execute.

Keep living documentation timeless. Include changing counts only when necessary, with a way to regenerate or verify them.

When committing, stage only your changes and inspect the staged diff. Use Conventional Commits with an imperative, lowercase description.

Once your work is done, push and open a draft PR so CI can run. When opening the PR follow the repository template, link the issue, explain the problem and fix briefly, and identify the model and harness. Leave any "## TL;DR (human written)" section empty.

Keep these generated local instruction files out of commits.
