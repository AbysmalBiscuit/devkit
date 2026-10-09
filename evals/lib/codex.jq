# One Codex session rollout (`$CODEX_HOME/sessions/**/rollout-*.jsonl`,
# slurped) -> the `claude -p --output-format stream-json` lines transcript.jq
# grades. Needs --argjson ms, the run's duration in milliseconds.
#
# Codex runs a shell command through `exec_command`, called directly or from
# a code-mode `exec` script, and both become Bash calls. Codex loads a skill
# by reading its SKILL.md, so a command naming one is also a Skill call. A
# call a PreToolUse hook blocks never starts, so its output is its only
# record; it becomes the error Claude Code reports for a hook denial.
def text: if type == "string" then . elif type == "array" then map(.text // "") | join("") else tojson end;
def args: if has("arguments") then .arguments | try fromjson catch {} else {input: .input} end;
def bash($command):
  {name: "Bash", input: {command: $command}},
  ($command | scan("skills/([^/\\s'\"]+)/SKILL\\.md") | {name: "Skill", input: {skill: .[0]}});
def uses:
  if .name == "exec_command" then args.cmd // empty | bash(.)
  elif .name == "exec" and .type == "custom_tool_call" then
    .input | scan("exec_command\\(\\{\\s*cmd:\\s*(\"(?:[^\"\\\\]|\\\\.)*\")")
    | .[0] | (try fromjson catch .[1:-1]) | bash(.)
  elif .name == "spawn_agent" then
    args | {name: "Agent", input: ({prompt: .message, subagent_type: .agent_type} | with_entries(select(.value != null)))}
  else {name: .name, input: args}
  end;

map(select(.type == "response_item") | .payload) as $items
| map(select(.type == "event_msg") | .payload) as $events
| ($items | map(select(.call_id != null and .name != null) | {key: .call_id, value: .name}) | from_entries) as $names
| ($events | map(select(.type == "task_complete")) | last) as $done
| ($items[]
    | if .type == "function_call" or .type == "custom_tool_call" then
        {type: "assistant", message: {content: [uses | {type: "tool_use"} + .]}}
      elif .type == "function_call_output" or .type == "custom_tool_call_output" then
        (.output | text) as $out
        | ($out | test("blocked by PreToolUse hook: ")) as $blocked
        | {type: "user", message: {content: [{
            type: "tool_result",
            is_error: $blocked,
            content: (if $blocked then "PreToolUse:\($names[.call_id] // "tool") hook error: \($out)" else $out end)
          }]}}
      else empty
      end),
  (if $done then
    {
      type: "result",
      subtype: "success",
      result: ($done.last_agent_message // ""),
      num_turns: ($events | map(select(.type == "token_count" and .info != null)) | length),
      total_cost_usd: 0,
      duration_ms: $ms
    }
  else empty
  end)
