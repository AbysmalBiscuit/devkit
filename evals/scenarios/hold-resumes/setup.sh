#!/usr/bin/env bash
# The three steps of a rename on this run's own list, as a session that
# planned them before a compaction would have left them.
set -euo pipefail
devkit todo add "Rename greet to welcome in src/greet.py" > /dev/null
devkit todo add "Update the call to greet in src/cli.py" > /dev/null
devkit todo add "Update the greet example in README.md" > /dev/null
