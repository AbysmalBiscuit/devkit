#!/usr/bin/env bash
# Usage: render.sh <devkit-binary>. Prints the brief a Claude Code session sees
# in the monorepo fixture: at session start in the repository root, then after
# the session moves into apps/web.
set -euo pipefail

devkit=$(realpath "$1")
fixture=$(cd "$(dirname "${BASH_SOURCE[0]}")/fixture" && pwd)

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
repo=$scratch/acme
mkdir -p "$scratch/home"
git init -q -b main "$repo"
cp "$fixture/devkit.toml" "$repo/devkit.toml"
for dir in $(sed -n 's/^path = "\(.*\)"$/\1/p' "$fixture/devkit.toml"); do
  mkdir -p "$repo/$dir"
done

run() {
  (cd "$1" && shift && echo '{"session_id":"eval"}' |
    env -u DEVKIT_CONFIG -u CURSOR_PROJECT_DIR -u CURSOR_PLUGIN_ROOT \
      HOME="$scratch/home" XDG_STATE_HOME="$scratch/home" \
      XDG_CONFIG_HOME="$scratch/home/config" DEVKIT_SKIP_AUTOLINK=1 \
      "$devkit" "$@")
}
echo '<hook event="SessionStart" command="devkit brief">'
run "$repo" brief
echo '</hook>'
echo
echo '<hook event="CwdChanged" cwd="apps/web" command="devkit brief --if-changed">'
run "$repo/apps/web" brief --if-changed
echo '</hook>'
