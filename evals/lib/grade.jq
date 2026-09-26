# One `claude -p --output-format json` result -> one graded run.
# Needs --slurpfile questions questions.json, --arg label, --arg rep.
#
# Kinds: `command` and `text` match one of `expect` exactly; `tokens` matches
# a command's words in any order, ignoring `--arg`; `set` and `list` match
# `expect` unordered and ordered; `bool` and `enum` match `expect`. Matching
# ignores case, backticks, repeated whitespace and a trailing period.
def norm: tostring | gsub("`"; "") | gsub("\\s+"; " ") | ltrimstr(" ") | rtrimstr(" ")
  | rtrimstr(".") | ascii_downcase;
def words: norm | split(" ") | map(select(. != "--arg")) | sort;

def correct($q; $a):
  if $q.kind == "command" or $q.kind == "text" then ($a | norm) as $n | any($q.expect[]; norm == $n)
  elif $q.kind == "tokens" then ($a | words) == ($q.expect | words)
  elif $q.kind == "set" then ($a | map(norm) | unique) == ($q.expect | map(norm) | unique)
  elif $q.kind == "list" then ($a | map(norm)) == ($q.expect | map(norm))
  else $a == $q.expect
  end;

{label: $label, rep: ($rep | tonumber), cost: (.total_cost_usd // 0)}
+ if .structured_output == null then
    {error: (.result // "no structured output")}
  else
    .structured_output as $out
    | {
        confidence: $out.confidence,
        unclear: $out.unclear,
        questions: ($questions[0] | map(
          . as $q | $out.answers[$q.id] as $a
          | {key: $q.id, value: {correct: correct($q; $a.answer), stated: $a.stated, answer: $a.answer}}
        ) | from_entries)
      }
  end
