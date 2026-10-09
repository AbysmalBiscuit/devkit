#!/usr/bin/env bash
# Usage: codex-seed.sh <setup.sh> <marker>, as a Codex SessionStart hook.
# Runs a scenario's setup.sh once, as the session Codex just started.
set -euo pipefail
[[ -e $2 ]] && exit 0
: > "$2"
CODEX_SESSION_ID=$(jq -r .session_id) "$1" > /dev/null
