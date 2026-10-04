# One scenario run's `claude -p --output-format stream-json` transcript
# (slurped), plus the runner's `{type: "eval"}` line -> one graded run.
# Needs --slurpfile spec scenario.json, --arg label, --arg scenario, --arg rep.
#
# Each check has an `id` and one of these, where every pattern is a
# case-insensitive regex:
# - `call` and `match`: some call to that tool matches. A Bash call matches on
#   its command, a Skill call on the skill name, a file tool on its path, and
#   any other tool on its input as JSON.
# - `reply`: the agent's final message matches.
# - `changed`: a path the run left changed or untracked in the repository
#   matches.
# - `file` and `match`: that file's contents after the run match. A file the
#   run left missing never matches.
# - `any`: a list of checks without ids, one of which passes.
# `absent: true` inverts any check, including one inside `any`.
def tool_calls: [.[] | select(.type == "assistant") | .message.content[]? | select(.type == "tool_use")];
def tool_results: [.[] | select(.type == "user") | .message.content[]? | select(.type == "tool_result")];
def subject: .input.command // .input.skill // .input.file_path // (.input | tojson);
def text: if type == "array" then map(.text // "") | join("") else tostring end;

(map(select(.type == "result")) | last) as $result
| (map(select(.type == "eval")) | last) as $eval
| ($eval.changed // []) as $changed
| ($eval.files // {}) as $files
| tool_calls as $calls
| def passes:
    . as $c
    | if has("call") then $calls | any(.name == $c.call and (subject | test($c.match; "i")))
      elif has("reply") then $result.result // "" | test($c.reply; "i")
      elif has("changed") then $changed | any(test($c.changed; "i"))
      elif has("file") then $files[$c.file] | . != null and test($c.match; "i")
      elif has("any") then any($c.any[]; passes)
      else error("check \(.id // tojson) names none of call, reply, changed, file, any")
      end
    | . != ($c.absent // false);
  {
    label: $label,
    scenario: $scenario,
    rep: ($rep | tonumber),
    error: (if $result == null then "no result line" else null end),
    stop: $result.subtype,
    cost: ($result.total_cost_usd // 0),
    turns: ($result.num_turns // 0),
    seconds: (($result.duration_ms // 0) / 1000),
    denials: (tool_results | map(select(.is_error and (.content | text | test("^PreToolUse:\\w+ hook error"))))
      | length),
    checks: ($spec[0].checks | map({key: .id, value: passes}) | from_entries)
  }
