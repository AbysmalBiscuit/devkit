#!/usr/bin/env bash
# Usage: bash evals/lib/transcript.test.sh
# Grades each hand-written transcript under testdata/<scenario>/ with
# transcript.jq and compares its checks to the <case>.checks.json beside it.
# The spec is testdata/<scenario>/scenario.json when present, otherwise the
# real evals/scenarios/<scenario>/scenario.json.
set -euo pipefail

lib=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
scenarios=$lib/../scenarios
failed=0
for transcript in "$lib"/testdata/*/*.jsonl; do
  dir=$(dirname "$transcript")
  scenario=$(basename "$dir")
  case=$(basename "$transcript" .jsonl)
  spec=$dir/scenario.json
  [[ -f $spec ]] || spec=$scenarios/$scenario/scenario.json
  want=$(jq -Sc . "$dir/$case.checks.json")
  if got=$(jq -sc --slurpfile spec "$spec" --arg label test --arg scenario "$scenario" \
    --arg rep 1 -f "$lib/transcript.jq" "$transcript" 2>&1) &&
    got=$(jq -Sc .checks <<< "$got") && [[ $got == "$want" ]]; then
    echo "ok   $scenario/$case"
  else
    echo "FAIL $scenario/$case"
    echo "  want: $want"
    echo "  got:  $got"
    failed=1
  fi
done
exit "$failed"
