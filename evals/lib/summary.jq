# Graded runs (slurped) -> a tab-separated table: one row per question, one
# column per label. A cell reads `correct/runs`, then how many of the correct
# answers the agent said the text stated outright.
# Needs --slurpfile questions questions.json, --argjson labels '["a", ...]'.
def runs($label): map(select(.label == $label and (.error | not)));
def mean: if length == 0 then "-" else (add / length * 10 | round / 10 | tostring) end;

. as $graded
| (["question"] + $labels),
  ($questions[0][] | .id as $id | [$id] + ($labels | map(
    . as $l | ($graded | runs($l)) as $r
    | ($r | map(.questions[$id]) | map(select(.correct))) as $ok
    | "\($ok | length)/\($r | length) stated \($ok | map(select(.stated)) | length)"
  ))),
  (["confidence"] + ($labels | map(. as $l | $graded | runs($l) | map(.confidence) | mean))),
  (["failed runs"] + ($labels | map(. as $l | $graded | map(select(.label == $l and .error)) | length | tostring))),
  (["cost usd"] + ($labels | map(. as $l | $graded | map(select(.label == $l) | .cost) | add * 100 | round / 100 | tostring)))
| @tsv
