#!/usr/bin/env bash
# Usage: render.sh <devkit-binary>. Prints what an agent sees after asking
# `devkit config tasks` about each of the fixture's tasks.
set -euo pipefail

devkit=$(realpath "$1")
fixture=$(cd "$(dirname "${BASH_SOURCE[0]}")/fixture" && pwd)

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
repo=$scratch/project
mkdir -p "$repo" "$scratch/home"
git -C "$repo" init -q -b main
cp "$fixture/devkit.toml" "$repo/devkit.toml"

cd "$repo"
echo '<output>'
for task in release ship build-web; do
  echo "\$ devkit config tasks $task"
  env -u DEVKIT_CONFIG HOME="$scratch/home" XDG_STATE_HOME="$scratch/home" \
    XDG_CONFIG_HOME="$scratch/home/config" DEVKIT_SKIP_AUTOLINK=1 DEVKIT_CALLER=agent \
    "$devkit" config tasks "$task" < /dev/null
  echo
done
echo '</output>'
