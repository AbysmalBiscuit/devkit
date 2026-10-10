#!/usr/bin/env bash
# Usage: render.sh <devkit-binary>. Prints what an agent reads to learn how to
# attach proof to a PR: `devkit pr create -h`, then the using-devkit reference
# section on opening a PR, taken from this checkout.
set -euo pipefail

devkit=$(realpath "$1")
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/home"

echo '<output>'
echo '$ devkit pr create -h'
env -u DEVKIT_CONFIG HOME="$scratch/home" XDG_STATE_HOME="$scratch/home" \
  XDG_CONFIG_HOME="$scratch/home/config" DEVKIT_SKIP_AUTOLINK=1 DEVKIT_CALLER=agent \
  "$devkit" pr create -h < /dev/null
echo '</output>'
echo
echo '<reference file="using-devkit/references/pr.md">'
awk '/^## `create`/ { on = 1; print; next } on && /^## / { exit } on' \
  "$root/plugin/skills/using-devkit/references/pr.md"
echo '</reference>'
