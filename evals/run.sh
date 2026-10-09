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
# shellcheck source=lib/codex.sh
source "$lib/codex.sh"

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
# skills, plugins, hooks and MCP servers out of the agent's context. For
# Codex, a fresh CODEX_HOME does the same.
empty=$(mktemp -d)
codex_dir=$(mktemp -d)
trap 'rm -rf "$empty" "$codex_dir"' EXIT
[[ $harness == codex ]] && codex_home "$codex_dir"

labels=()
for spec in "$@"; do
  label=${spec%%=*}
  binary=${spec#*=}
  labels+=("$label")
  "$case_dir/render.sh" "$binary" > "$out/context-$label.txt"
  prompt "$out/context-$label.txt" > "$out/prompt-$label.txt"
  for rep in $(seq 1 "$reps"); do
    if [[ $harness == codex ]]; then
      (cd "$empty" && env CODEX_HOME="$codex_dir" codex exec --json --ephemeral \
        --skip-git-repo-check --sandbox read-only --output-schema "$out/schema.json" \
        ${model:+--model "$model"} - \
        < "$out/prompt-$label.txt" > "$out/$label-$rep.exec.jsonl" 2> "$out/$label-$rep.err"
      jq -s -f "$lib/codex-result.jq" "$out/$label-$rep.exec.jsonl" > "$out/$label-$rep.json") &
    else
      (cd "$empty" && claude -p --safe-mode --strict-mcp-config --tools "" \
        --no-session-persistence --model "$model" --output-format json \
        --json-schema "$(cat "$out/schema.json")" \
        < "$out/prompt-$label.txt" > "$out/$label-$rep.json" 2> "$out/$label-$rep.err") &
    fi
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
