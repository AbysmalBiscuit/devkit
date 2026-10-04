#!/usr/bin/env bash
# Starts a throwaway Postgres, in front of it the strict transaction-mode
# pooler `pgbouncer.ini` describes, and a Postgres that offers TLS with a
# certificate from a CA of its own, then prints the variables the Postgres
# todo tests read. Run again to start them afresh; `down.sh` removes them.
#
#   eval "$(crates/devkit-todo-postgres/testdb/up.sh)"
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
pg_port=${PG_PORT:-55432}
pooler_port=${POOLER_PORT:-56432}
name=devkit-todo-pg-$pg_port

docker rm -f "$name" "$name-pooler" "$name-tls" >/dev/null 2>&1 || true
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

# A third server that offers TLS, its certificate signed by a CA of its own,
# for the tests that check the client verifies it.
tls_port=${TLS_PORT:-57432}
certs=${TMPDIR:-/tmp}/$name-tls
rm -rf "$certs" && mkdir -p "$certs"
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=devkit-test-ca \
  -keyout "$certs/ca.key" -out "$certs/ca.crt" 2>/dev/null
openssl req -newkey rsa:2048 -nodes -subj /CN=localhost \
  -keyout "$certs/server.key" -out "$certs/server.csr" 2>/dev/null
printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\n' > "$certs/san.ext"
openssl x509 -req -in "$certs/server.csr" -CA "$certs/ca.crt" -CAkey "$certs/ca.key" \
  -CAcreateserial -days 2 -extfile "$certs/san.ext" -out "$certs/server.crt" 2>/dev/null
chmod 644 "$certs"/*
docker run -d --name "$name-tls" -e POSTGRES_PASSWORD=postgres \
  -v "$certs:/certs:ro" -p "127.0.0.1:$tls_port:5432" --entrypoint sh \
  postgres:17-alpine -c '
    install -o postgres -m 600 /certs/server.key /tmp/server.key &&
    install -o postgres -m 644 /certs/server.crt /tmp/server.crt &&
    exec docker-entrypoint.sh postgres -c ssl=on \
      -c ssl_cert_file=/tmp/server.crt -c ssl_key_file=/tmp/server.key' >/dev/null
for _ in $(seq 60); do
  docker exec "$name-tls" pg_isready -U postgres -h 127.0.0.1 >/dev/null 2>&1 && break
  sleep 1
done
docker exec "$name-tls" pg_isready -U postgres -h 127.0.0.1 >/dev/null

echo "export DEVKIT_TEST_POSTGRES_URL=postgres://postgres:postgres@127.0.0.1:$pg_port/postgres"
echo "export DEVKIT_TEST_POOLER_URL=postgres://postgres:postgres@127.0.0.1:$pooler_port/postgres"
echo "export DEVKIT_TEST_POSTGRES_TLS_URL=postgres://postgres:postgres@127.0.0.1:$tls_port/postgres"
echo "export DEVKIT_TEST_POSTGRES_CA=$certs/ca.crt"
