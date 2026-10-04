# Native task tool payloads

`PostToolUse` payloads the todo capture tests feed to `devkit hook post-tool-use`, and a `PreToolUse` shell payload the attribution tests feed to `devkit hook pre-tool-use`. Each line is one hook invocation's stdin, recorded by a hook that appended its stdin to a file. `{{cwd}}` and `{{transcript_path}}` replace the recording machine's paths; tests substitute their own.

## Recording

Claude Code fixtures come from Claude Code 2.1.288 (`claude --version`), run headless as `claude -p --setting-sources project --settings <file>` from an empty git repository, with `CLAUDE_CODE_ENABLE_TODO_TOOLS=1` and a `PostToolUse` hook on `TaskCreate|TaskUpdate|TodoWrite` running `sh -c 'cat; echo' >> <file>`.

Codex fixtures come from codex-cli 0.160.0 (`codex --version`), run as `codex exec --dangerously-bypass-hook-trust` with a scratch `CODEX_HOME` whose `config.toml` sets `[features] hooks = true`, `[tools.update_plan] enabled = true`, and the same capture hook on `PostToolUse` (matcher `.*`) and `SubagentStart`. `codex-subagent-shell.jsonl` used the hook on `PreToolUse` (matcher `.*`) and `SubagentStart`, with `--sandbox workspace-write`.

| Fixture | Prompt |
|---|---|
| `claude-task-create.jsonl`, `claude-task-update.jsonl`, `claude-task-deleted.jsonl` | Create tasks `alpha` and `beta`; set `alpha` to `in_progress`, then `completed`; set `beta` to `deleted`. |
| `claude-subagent-tasks.jsonl` | Create `parent-step`, then launch a general-purpose sub-agent that creates `gamma`, sets it `in_progress`, then `completed`; then complete `parent-step`. |
| `claude-todowrite.jsonl` | Not recorded; see below. |
| `codex-update-plan.jsonl` | Call `update_plan` three times: `run tests`, `build`, `run tests` all pending; then `run tests` completed, `build` in progress, `run tests` pending; then `run tests` completed and `run tests` in progress. |
| `codex-subagent-plan.jsonl`, `codex-subagent-start.jsonl` | Spawn one sub-agent that calls `update_plan` once and reports any secret word from its context. The `SubagentStart` hook added `The secret word is PELICAN.` as context. |
| `codex-subagent-shell.jsonl` | Spawn exactly one sub-agent; it runs `echo hello` once and reports its output. Run no shell command yourself. The file keeps the sub-agent's `Bash` call only. |

## Answers

- **`TaskUpdate` with `deleted`.** Accepted. The response reports `"statusChange": {"from": "pending", "to": "deleted"}`.
- **Claude Code sub-agent task ids.** Shared with the parent, not restarted: the parent's task was `1` and the sub-agent's next task was `2`. The payload carries the root `session_id` with the sub-agent's `agent_id` and `agent_type`.
- **Codex sub-agent payload.** Carries the root `session_id`, plus `agent_id` and `agent_type` (`default`).
- **Codex sub-agent shell call.** `PreToolUse` names it `Bash`, with a `tool_input` holding only `command`, and carries the root `session_id` with the sub-agent's `agent_id` and `agent_type`.
- **`SubagentStart` context in Codex.** Reaches the sub-agent: it answered `PELICAN`.
- **`TodoWrite`.** Not offered by Claude Code 2.1.288, with or without `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`, nor with `--tools TodoWrite`; the model reported having no such tool each time. `claude-todowrite.jsonl` is written by hand in the input shape earlier Claude Code versions sent, `{"todos": [{"content", "status", "activeForm"}]}`, so capture still handles a harness version that offers it. That shape is unverified against a live harness.
- **Codex `update_plan`.** Off unless `[tools.update_plan] enabled = true`. In code mode the model calls it from inside its `exec` tool, and `PostToolUse` still fires once per `update_plan` call with `tool_name: "update_plan"`.

## Capture rule

Capture records a `TaskCreate` mapping under the session's holder, whichever agent created the task, and looks a `TaskUpdate` up under its own holder first, then its session's. Claude Code shares one native task list between a session and its sub-agents, so a parent can update a task its sub-agent created, and the session's mapping is the one both of them find. Status changes are attributed to the payload's own holder.
