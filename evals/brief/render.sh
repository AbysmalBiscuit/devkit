#!/usr/bin/env bash
# Usage: render.sh <devkit-binary>. Prints the session-start hook output a
# Claude Code session sees in the brief fixture.
set -euo pipefail

devkit=$(realpath "$1")
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
fixture=$root/tests/fixtures/brief-monorepo

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
repo=$scratch/acme
mkdir -p "$repo" "$scratch/home"
git -C "$repo" init -q -b main
cp "$fixture/index.json" "$repo/index.json"
{
  cat "$fixture/devkit.toml"
  printf "\n[rules]\nindex = '%s'\n" "$repo/index.json"
} > "$repo/devkit.toml"

cd "$repo"
run() {
  env -u DEVKIT_CONFIG -u CURSOR_PROJECT_DIR -u CURSOR_PLUGIN_ROOT \
    HOME="$scratch/home" XDG_STATE_HOME="$scratch/home" \
    XDG_CONFIG_HOME="$scratch/home/config" DEVKIT_SKIP_AUTOLINK=1 \
    "$devkit" "$@" < /dev/null
}
echo '<hook command="devkit brief">'
run brief
echo
echo '</hook>'
echo '<hook command="devkit rules context">'
run rules context
echo '</hook>'
