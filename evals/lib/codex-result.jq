# One `codex exec --json --output-schema` run (slurped) -> the
# `claude -p --output-format json` result grade.jq reads. Codex reports
# tokens rather than cost, so cost reads 0.
(map(select(.type == "item.completed" and .item.type == "agent_message")) | last | .item.text) as $reply
| {
    result: ($reply // (map(select(.type == "turn.failed" or .type == "error")) | last | .error.message // .message // "no reply")),
    structured_output: ($reply | try fromjson catch null),
    total_cost_usd: 0
  }
