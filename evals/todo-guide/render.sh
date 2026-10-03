#!/usr/bin/env bash
# Usage: render.sh <devkit-binary>. Prints the todo context a Claude Code
# session sees at session start, over a store seeded with a workspace todo,
# a pending session todo and one a sub-agent has in progress.
set -euo pipefail

devkit=$(realpath "$1")
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
repo=$scratch/acme
mkdir -p "$scratch/home"
git init -q -b main "$repo"

run() {
  (cd "$repo" && env -u DEVKIT_CONFIG -u CLAUDE_CODE_SESSION_ID -u CODEX_SESSION_ID \
    HOME="$scratch/home" XDG_STATE_HOME="$scratch/home" \
    XDG_CONFIG_HOME="$scratch/home/config" DEVKIT_SKIP_AUTOLINK=1 "$@")
}
run env DEVKIT_CALLER=human "$devkit" todo add --node acme.main "set up the schema" > /dev/null
run env CLAUDE_CODE_SESSION_ID=s1 "$devkit" todo add "write the migration" > /dev/null
run env CLAUDE_CODE_SESSION_ID=s1 "$devkit" todo add "update the docs" > /dev/null
jq -nc --arg cwd "$repo" '{hook_event_name: "PreToolUse", session_id: "s1", agent_id: "a7",
  agent_type: "general-purpose", tool_name: "Bash", tool_input: {command: "devkit todo start 3"}, cwd: $cwd}' |
  run "$devkit" hook pre-tool-use --harness claude-code > /dev/null

echo '<hook event="SessionStart" command="devkit todo context --harness claude-code">'
jq -nc --arg cwd "$repo" '{hook_event_name: "SessionStart", session_id: "s1", source: "startup", cwd: $cwd}' |
  run "$devkit" todo context --harness claude-code | jq -r '.hookSpecificOutput.additionalContext'
echo '</hook>'
