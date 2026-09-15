# Cloud session rules

Read the repository's instructions before working. These cloud preferences govern autonomy and delivery; the cloud workflow's choices override skill defaults that require brainstorming for every change or always prefer subagents.

## Select or resume work

Before starting a new issue or feature, read `.agents/skills/cloud/SKILL.md` to find existing specs and plans and select the workflow. During recovery, follow `.agents/skills/cloud/references/recovery.md` to resume the active task.

Use the provided isolated cloud checkout and assigned branch when suitable.

## Exercise judgment

An implementation request authorizes the necessary investigation, edits, tests, and fixes. Continue through completion without asking whether to continue. Questions asking for an assessment authorize a read-only answer.

Resolve routine implementation choices yourself. Record consequential assumptions and deviations, with reasons, for the final report. Ask when a missing decision would materially change scope or user behavior. Continue independent work while waiting; silence is not approval.

## Maintain progress

For multi-step work, update the selected workflow's progress ledger as tasks finish or the next action changes. If the workflow has no ledger, use `.superpowers/cloud-progress.md`. Record the objective, spec and plan paths, completed work, active task, verification results, consequential decisions, and next action. Include pending questions or approvals and the state of running agents or commands.

## Tooling and verification

Before using devkit's task runner or coordination tools, read `/devkit:using-devkit`. Before using mcpls for code intelligence, read `/mcpls:mcpls`.

Keep changes focused. Comments explain non-obvious reasons and invariants; keep them standalone and timeless.

For a bug fix, reproduce the failure through the entry point where it occurs. When using TDD, observe the test fail for the actual bug before implementing the fix. Choose tests that verify behavior.

Use the repository's prescribed checks. Completion requires the requested behavior, relevant passing checks, and resolution of blocking review findings. Report anything you could not verify.

## Communicate and deliver

Lead with the result or recommendation, followed by necessary evidence. Be direct. Disagree when the facts warrant it and state uncertainty plainly. Report work you performed as results, not instructions for me to execute.

Keep living documentation timeless. Include changing counts only when necessary, with a way to regenerate or verify them.

When committing, stage only your changes and inspect the staged diff. Use Conventional Commits with an imperative, lowercase description. Keep generated local configuration and instruction files out of commits.

Preserve the author identity configured in the VM environment. Pass your agent name and email through the commit task's `coauthors` argument, then confirm the resulting commit includes its `Co-authored-by` trailer. Keep the installed commit hooks enabled.

Once your work is done, push and open a draft PR so CI can run. Follow the repository template, link the issue, explain the problem and fix briefly, and identify the model and harness. Leave any "## TL;DR (human written)" section empty.
