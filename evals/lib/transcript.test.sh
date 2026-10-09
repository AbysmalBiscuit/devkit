#!/usr/bin/env bash
# Usage: bash evals/lib/transcript.test.sh
# Grades each hand-written transcript under testdata/<scenario>/ with
# transcript.jq and compares its checks to the <case>.checks.json beside it.
# A <case>.codex.jsonl Codex rollout goes through codex.jq first. The spec is
# testdata/<scenario>/scenario.json, else evals/scenarios/<scenario>/'s.
set -euo pipefail

lib=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
scenarios=$lib/../scenarios
failed=0

# The Claude transcript a testdata file stands for.
claude_transcript() {
  if [[ $1 == *.codex.jsonl ]]; then
    jq -sc --argjson ms 1000 -f "$lib/codex.jq" "$1"
    jq -c 'select(.type == "eval")' "$1"
  else
    cat "$1"
  fi
}

for transcript in "$lib"/testdata/*/*.jsonl; do
  dir=$(dirname "$transcript")
  scenario=$(basename "$dir")
  case=$(basename "$transcript" .jsonl)
  spec=$dir/scenario.json
  [[ -f $spec ]] || spec=$scenarios/$scenario/scenario.json
  want=$(jq -Sc . "$dir/$case.checks.json")
  if got=$(claude_transcript "$transcript" | jq -sc --slurpfile spec "$spec" --arg label test \
    --arg scenario "$scenario" --arg rep 1 -f "$lib/transcript.jq" 2>&1) &&
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
