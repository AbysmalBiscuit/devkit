#!/usr/bin/env bash
# Stops what devrun started, then any server the agent launched by hand from
# this checkout.
set -uo pipefail
devrun down > /dev/null 2>&1
repo=$(pwd -P)
for pid in $(pgrep -f http.server); do
  [[ $(readlink "/proc/$pid/cwd" 2> /dev/null) == "$repo"* ]] && kill "$pid"
done
exit 0
