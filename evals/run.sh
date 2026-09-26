#!/usr/bin/env bash
# Usage: evals/run.sh <case> [label=devkit-binary ...]. See AGENTS.md.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
lib=$root/evals/lib
case_name=${1:?usage: evals/run.sh <case> [label=devkit-binary ...]}
shift
case_dir=$root/evals/$case_name
[[ -f $case_dir/questions.json ]] || { echo "no case at $case_dir" >&2; exit 1; }
reps=${EVAL_REPS:-3}
model=${EVAL_MODEL:-claude-opus-5-5}

target=${CARGO_TARGET_DIR:-$root/target}
if [[ $# -eq 0 ]]; then
  cargo build --quiet --locked --bin devkit --manifest-path "$root/Cargo.toml"
  set -- "worktree=$target/debug/devkit"
fi

out=$target/evals/$case_name/$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$out"
jq -f "$lib/schema.jq" "$case_dir/questions.json" > "$out/schema.json"

prompt() {
  cat "$case_dir/preamble.md"
  echo
  cat "$1"
  echo
  echo "Answer only from the text above. Set \`stated\` to true only when the text says the answer directly, and to false when you inferred or guessed it. Give a command as the command alone, with no prose or backticks. Then rate your overall confidence 1 to 5, and list every spot that was ambiguous or where you had to guess."
  echo
  jq -r '.[] | "- \(.id): \(.question)"' "$case_dir/questions.json"
}

# An empty working directory and --safe-mode keep this machine's CLAUDE.md,
# skills, plugins, hooks and MCP servers out of the agent's context.
empty=$(mktemp -d)
trap 'rm -rf "$empty"' EXIT

labels=()
for spec in "$@"; do
  label=${spec%%=*}
  binary=${spec#*=}
  labels+=("$label")
  "$case_dir/render.sh" "$binary" > "$out/context-$label.txt"
  prompt "$out/context-$label.txt" > "$out/prompt-$label.txt"
  for rep in $(seq 1 "$reps"); do
    (cd "$empty" && claude -p --safe-mode --strict-mcp-config --tools "" \
      --no-session-persistence --model "$model" --output-format json \
      --json-schema "$(cat "$out/schema.json")" \
      < "$out/prompt-$label.txt" > "$out/$label-$rep.json" 2> "$out/$label-$rep.err") &
  done
done
wait

for label in "${labels[@]}"; do
  for rep in $(seq 1 "$reps"); do
    jq -c --slurpfile questions "$case_dir/questions.json" --arg label "$label" \
      --arg rep "$rep" -f "$lib/grade.jq" "$out/$label-$rep.json" 2> /dev/null \
      || jq -nc --arg label "$label" --arg rep "$rep" --rawfile err "$out/$label-$rep.err" \
        '{label: $label, rep: ($rep | tonumber), cost: 0, error: $err}'
  done
done > "$out/graded.jsonl"

jq -r '.[] | select(.error | not) | "\(.label) run \(.rep): \(.unclear[])"' \
  --slurp "$out/graded.jsonl" > "$out/unclear.txt"

{
  for label in "${labels[@]}"; do
    size="$(wc -c < "$out/context-$label.txt") bytes"
    if command -v uvx > /dev/null; then
      size+=", $(uvx --quiet --with tiktoken python "$lib/tokens.py" "$out/context-$label.txt") tokens"
    fi
    printf '%s\t%s\n' "$label" "$size"
  done
  echo
  jq -rs --slurpfile questions "$case_dir/questions.json" \
    --argjson labels "$(printf '%s\n' "${labels[@]}" | jq -R . | jq -sc .)" \
    -f "$lib/summary.jq" "$out/graded.jsonl"
} | column -t -s $'\t' | tee "$out/summary.txt"
echo
echo "runs, prompts and the agents' ambiguity notes: $out"
