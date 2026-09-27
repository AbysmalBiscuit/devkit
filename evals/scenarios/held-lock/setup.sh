#!/usr/bin/env bash
# Another session is partway through rewriting the file the prompt asks about.
set -euo pipefail
lockm acquire --as other-agent --ttl 0 --note "rewriting add to check for overflow" src/lib.rs > /dev/null
