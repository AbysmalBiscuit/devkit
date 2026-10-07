#!/usr/bin/env bash
# Usage: render.sh <devkit-binary>. Prints the reason a Claude Code session's
# stop is refused with, over a store seeded with two of its own todos.
set -euo pipefail

devkit=$(realpath "$1")
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
repo=$scratch/acme
mkdir -p "$scratch/home"
git init -q -b main "$repo"

run() {
  (cd "$repo" && env -u DEVKIT_CONFIG -u CLAUDE_CODE_SESSION_ID -u CODEX_SESSION_ID \
    -u DEVKIT_TODO_BACKEND DEVKIT_TODO_HOLD_STOP=always \
    HOME="$scratch/home" XDG_STATE_HOME="$scratch/home" \
    XDG_CONFIG_HOME="$scratch/home/config" DEVKIT_SKIP_AUTOLINK=1 "$@")
}
run env CLAUDE_CODE_SESSION_ID=s1 "$devkit" todo add "write the migration" > /dev/null
run env CLAUDE_CODE_SESSION_ID=s1 "$devkit" todo add "update the docs" > /dev/null

echo '<hook event="Stop">'
jq -nc --arg cwd "$repo" '{hook_event_name: "Stop", session_id: "s1", stop_hook_active: false, cwd: $cwd}' |
  run "$devkit" hook stop --harness claude-code | jq -r '.reason'
echo '</hook>'
