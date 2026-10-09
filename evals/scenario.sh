#!/usr/bin/env bash
# Usage: evals/scenario.sh <scenario|all> [label=devkit-checkout ...]. See AGENTS.md.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
lib=$root/evals/lib
name=${1:?usage: evals/scenario.sh <scenario|all> [label=devkit-checkout ...]}
shift
# shellcheck source=lib/codex.sh
source "$lib/codex.sh"
scenarios=()
if [[ $name == all ]]; then
  for dir in "$root"/evals/scenarios/*/; do
    jq -e --arg h "$harness" '.harnesses // [$h] | index($h)' "$dir/scenario.json" > /dev/null &&
      scenarios+=("$dir")
  done
else
  dir=$root/evals/scenarios/$name/
  [[ -f $dir/scenario.json ]] || { echo "no scenario at $dir" >&2; exit 1; }
  jq -e --arg h "$harness" '.harnesses // [$h] | index($h)' "$dir/scenario.json" > /dev/null ||
    { echo "$name does not run on $harness" >&2; exit 1; }
  scenarios=("$dir")
fi
reps=${EVAL_REPS:-3}
[[ $# -eq 0 ]] && set -- "worktree=$root"

# The session running this script must not lend its identity to the hooks of
# the sessions it starts.
for var in $(compgen -e); do
  case $var in CLAUDE* | CODEX* | CURSOR* | DEVKIT*) unset "$var" ;; esac
done

target=${CARGO_TARGET_DIR:-$root/target}
out=$target/evals/scenarios/$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$out"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

# Runs a command against the label's build, with state, config, cache and
# todos in its scratch instead of this machine's registry, global config and
# todo store. No one watches a run, so its stop hook holds the main agent too.
isolated() {
  local label_dir=$1
  shift
  env PATH="$label_dir/bin:$PATH" \
    XDG_STATE_HOME="$label_dir/state" XDG_CONFIG_HOME="$label_dir/config" \
    XDG_CACHE_HOME="$label_dir/cache" GIT_CONFIG_GLOBAL="$label_dir/gitconfig" \
    DEVKIT_NO_BOOTSTRAP=1 DEVKIT_SKIP_AUTOLINK=1 ENABLE_CLAUDEAI_MCP_SERVERS=false \
    DEVKIT_TODO_HOLD_STOP=always DEVKIT_TODO_BACKEND=builtin \
    "$@"
}

# A lowercase UUID, the form `claude --session-id` takes.
mint_session() {
  if command -v uuidgen > /dev/null; then
    uuidgen | tr '[:upper:]' '[:lower:]'
  else
    cat /proc/sys/kernel/random/uuid
  fi
}

# The scenario's files that `file` checks name, as `{path: contents}`, null
# for a file the run left missing.
captured_files() {
  local spec=$1 repo=$2 files='{}' path
  while IFS= read -r path; do
    if [[ -f $repo/$path ]]; then
      files=$(jq -c --arg p "$path" --rawfile c "$repo/$path" '. + {($p): $c}' <<< "$files")
    else
      files=$(jq -c --arg p "$path" '. + {($p): null}' <<< "$files")
    fi
  done < <(jq -r '[.checks | .. | objects | .file // empty] | unique[]' "$spec")
  printf '%s\n' "$files"
}

# A Codex run in its own copy of the label's CODEX_HOME. Codex has no flag to
# name the session or cap its turns, so setup.sh runs from a SessionStart hook
# and `max_turns` does not apply.
run_codex() {
  local label_dir=$1 dir=$2 work=$3 base=$4 start thread rollout
  cp -R "$label_dir/codex-home" "$work/codex"
  if [[ -x $dir/setup.sh ]]; then
    cat >> "$work/codex/config.toml" << EOF

[[hooks.SessionStart]]
matcher = "startup"
[[hooks.SessionStart.hooks]]
type = "command"
command = "'$lib/codex-seed.sh' '$dir/setup.sh' '$work/seeded'"
EOF
  fi
  start=$(date +%s)
  # The plugin's hooks are this checkout's own, and they run before the
  # sandbox and approval policy are consulted, so bypassing both leaves every
  # devkit verdict in force.
  (cd "$work/repo" && isolated "$label_dir" env CODEX_HOME="$work/codex" timeout "${EVAL_TIMEOUT:-600}" \
    codex exec --json --dangerously-bypass-hook-trust --dangerously-bypass-approvals-and-sandbox \
    ${model:+--model "$model"} - < "$dir/prompt.md" > "$base.exec.jsonl" 2> "$base.err") || true
  : > "$base.jsonl"
  thread=$(jq -r 'select(.type == "thread.started") | .thread_id' "$base.exec.jsonl")
  [[ -n $thread ]] || return 0
  for rollout in "$work"/codex/sessions/*/*/*/rollout-*-"$thread".jsonl; do
    [[ -f $rollout ]] || continue
    cp "$rollout" "$base.rollout.jsonl"
    jq -sc --argjson ms "$((($(date +%s) - start) * 1000))" -f "$lib/codex.jq" "$rollout" > "$base.jsonl"
  done
}

run_one() {
  local label=$1 dir=$2 rep=$3
  local scenario plugin label_dir=$scratch/$1 work session
  scenario=$(basename "$dir")
  session=$(mint_session)
  plugin=$(cat "$label_dir/plugin")
  work=$label_dir/$scenario-$rep
  mkdir -p "$work/repo"
  git init -q -b main "$work/repo"
  [[ -d $dir/fixture ]] && cp -R "$dir/fixture/." "$work/repo/"
  git -C "$work/repo" add -A
  git -C "$work/repo" -c user.name=eval -c user.email=eval@example.invalid \
    commit -q --allow-empty -m fixture
  local transcript=$out/$label-$scenario-$rep.jsonl
  if [[ $harness == codex ]]; then
    run_codex "$label_dir" "$dir" "$work" "${transcript%.jsonl}"
  else
    # setup.sh alone sees the session, so the todos it seeds land on the node
    # the run's own hooks resolve.
    if [[ -x $dir/setup.sh ]]; then
      (cd "$work/repo" && isolated "$label_dir" env CLAUDE_CODE_SESSION_ID="$session" "$dir/setup.sh")
    fi
    # The guard hooks run before the permission mode is consulted, so bypassing
    # permission prompts leaves every devkit verdict in force.
    (cd "$work/repo" && isolated "$label_dir" timeout "${EVAL_TIMEOUT:-600}" \
      claude -p --setting-sources project --plugin-dir "$plugin" \
      --permission-mode bypassPermissions --no-session-persistence --session-id "$session" \
      --model "$model" --max-turns "$(jq -r '.max_turns' "$dir/scenario.json")" \
      --output-format stream-json --verbose \
      < "$dir/prompt.md" > "$transcript" 2> "$out/$label-$scenario-$rep.err") || true
  fi
  git -C "$work/repo" status --porcelain --untracked-files=all |
    jq -Rsc --argjson files "$(captured_files "$dir/scenario.json" "$work/repo")" \
      '{type: "eval", changed: (split("\n") | map(select(. != "") | .[3:])), files: $files}' >> "$transcript"
  if [[ -x $dir/teardown.sh ]]; then
    (cd "$work/repo" && isolated "$label_dir" "$dir/teardown.sh")
  fi
}

labels=()
for spec in "$@"; do
  label=${spec%%=*}
  checkout=$(realpath "${spec#*=}")
  labels+=("$label")
  cargo build --quiet --locked --bins --manifest-path "$checkout/Cargo.toml"
  bin=$(cargo metadata --format-version 1 --no-deps --manifest-path "$checkout/Cargo.toml" |
    jq -r .target_directory)/debug
  label_dir=$scratch/$label
  mkdir -p "$label_dir/bin"
  printf '%s\n' "$checkout/plugin" > "$label_dir/plugin"
  printf '[user]\n\tname = eval\n\temail = eval@example.invalid\n' > "$label_dir/gitconfig"
  for link in devkit issue devrun portm lockm docm devkit-mcp devrules; do
    ln -s "$bin/devkit" "$label_dir/bin/$link"
  done
  [[ -x $bin/devkitd ]] && ln -s "$bin/devkitd" "$label_dir/bin/devkitd"
  if [[ $harness == codex ]]; then
    codex_home "$label_dir/codex-home"
    marketplace=$(jq -r .name "$checkout/.agents/plugins/marketplace.json")
    isolated "$label_dir" env CODEX_HOME="$label_dir/codex-home" \
      codex plugin marketplace add "$checkout" >> "$out/$label-codex-setup.log" 2>&1
    isolated "$label_dir" env CODEX_HOME="$label_dir/codex-home" \
      codex plugin add "devkit@$marketplace" >> "$out/$label-codex-setup.log" 2>&1
  fi
done

for label in "${labels[@]}"; do
  for dir in "${scenarios[@]}"; do
    for rep in $(seq 1 "$reps"); do
      run_one "$label" "${dir%/}" "$rep" &
    done
  done
done
wait

for label in "${labels[@]}"; do
  for dir in "${scenarios[@]}"; do
    scenario=$(basename "$dir")
    for rep in $(seq 1 "$reps"); do
      jq -sc --slurpfile spec "$dir/scenario.json" --arg label "$label" \
        --arg scenario "$scenario" --arg rep "$rep" -f "$lib/transcript.jq" \
        "$out/$label-$scenario-$rep.jsonl"
    done
  done
done > "$out/graded.jsonl"

jq -rs --argjson labels "$(printf '%s\n' "${labels[@]}" | jq -R . | jq -sc .)" \
  -f "$lib/scenario-summary.jq" "$out/graded.jsonl" | column -t -s $'\t' | tee "$out/summary.txt"
echo
echo "transcripts and graded runs: $out"
