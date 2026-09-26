# One `claude -p --output-format json` result -> one graded run.
# Needs --slurpfile questions questions.json, --arg label, --arg rep.
def norm: tostring | gsub("`"; "") | gsub("\\s+"; " ") | ltrimstr(" ") | rtrimstr(" ")
  | rtrimstr(".") | ascii_downcase;

def correct($q; $a):
  if $q.kind == "command" then ($a | norm) as $n | any($q.expect[]; norm == $n)
  elif $q.kind == "set" then ($a | map(norm) | unique) == ($q.expect | map(norm) | unique)
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
