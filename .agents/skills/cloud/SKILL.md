---
name: cloud
description: Find existing issue work and select a cloud execution workflow.
disable-model-invocation: true
user-invocable: false
---

# Cloud workflow

Read this procedure when starting a new issue or feature. Standing session rules live in the generated `AGENTS.local.md` at the repository root.

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
