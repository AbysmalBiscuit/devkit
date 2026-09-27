# Graded scenario runs (slurped) -> a tab-separated table: one row per
# scenario check, then each scenario's means, one column per label. A check
# cell reads `passed/runs`; `all checks` counts the runs that passed every one.
# Needs --argjson labels '["a", ...]'.
def runs($label; $scenario): map(select(.label == $label and .scenario == $scenario and (.error | not)));
def mean: if length == 0 then "-" else (add / length * 10 | round / 10 | tostring) end;

. as $graded
| def row($scenario; $what; f): [$scenario, $what] + ($labels | map(. as $l | $graded | runs($l; $scenario) | f));
($graded | map(.scenario) | unique) as $scenarios
| (["scenario", "check"] + $labels),
  ($scenarios[] as $s
    | ($graded | map(select(.scenario == $s and (.error | not))) | first | .checks // {} | keys_unsorted) as $ids
    | ($ids[] as $id | row($s; $id; "\(map(select(.checks[$id])) | length)/\(length)")),
      row($s; "all checks"; "\(map(select(.checks | all)) | length)/\(length)"),
      row($s; "turns"; map(.turns) | mean),
      row($s; "guard denials"; map(.denials) | mean),
      row($s; "seconds"; map(.seconds) | mean),
      row($s; "cost usd"; map(.cost) | add // 0 | . * 100 | round / 100 | tostring),
      ([$s, "failed runs"] + ($labels | map(. as $l | $graded
        | map(select(.label == $l and .scenario == $s and .error)) | length | tostring))))
| @tsv
