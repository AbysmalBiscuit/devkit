#!/usr/bin/env bash
# Removes what `up.sh` started on the same ports.
set -euo pipefail
name=devkit-todo-pg-${PG_PORT:-55432}
docker rm -f "$name" "$name-pooler" "$name-tls" "$name-rest" "$name-api" >/dev/null 2>&1 || true
docker network rm "$name" >/dev/null 2>&1 || true
