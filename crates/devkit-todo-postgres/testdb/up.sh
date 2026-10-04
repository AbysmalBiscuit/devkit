#!/usr/bin/env bash
# Starts a throwaway Postgres and, in front of it, the strict transaction-mode
# pooler `pgbouncer.ini` describes, then prints the variables the Postgres
# todo tests read. Run again to start both afresh; `down.sh` removes them.
#
#   eval "$(crates/devkit-todo-postgres/testdb/up.sh)"
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
pg_port=${PG_PORT:-55432}
pooler_port=${POOLER_PORT:-56432}
name=devkit-todo-pg-$pg_port

docker rm -f "$name" "$name-pooler" >/dev/null 2>&1 || true
docker network create "$name" >/dev/null 2>&1 || true
docker run -d --name "$name" --network "$name" --network-alias postgres \
  -e POSTGRES_PASSWORD=postgres -p "127.0.0.1:$pg_port:5432" \
  postgres:17-alpine >/dev/null
for _ in $(seq 60); do
  docker exec "$name" pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1 && break
  sleep 1
done
docker exec "$name" pg_isready -U postgres -h 127.0.0.1 >/dev/null
docker run -d --name "$name-pooler" --network "$name" \
  -v "$here/pgbouncer.ini:/etc/pgbouncer/pgbouncer.ini:ro" \
  -v "$here/userlist.txt:/etc/pgbouncer/userlist.txt:ro" \
  -p "127.0.0.1:$pooler_port:6432" edoburu/pgbouncer:v1.26.0-p0 >/dev/null

echo "export DEVKIT_TEST_POSTGRES_URL=postgres://postgres:postgres@127.0.0.1:$pg_port/postgres"
echo "export DEVKIT_TEST_POOLER_URL=postgres://postgres:postgres@127.0.0.1:$pooler_port/postgres"
