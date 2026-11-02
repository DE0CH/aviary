#!/usr/bin/env bash
# Start the CI Postgres: the production image (CloudNativePG's, which has no entrypoint of its own, so it cannot be a
# GitHub service container), TLS on with a throwaway self-signed certificate (aviary-pg connects with
# sslmode=require), scram passwords. Prints the URL for AVIARY_TEST_DATABASE_URL.
#   postgres.sh <image> <host port>
set -euo pipefail
image=$1
port=$2
dir=$(mktemp -d)
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=aviary-ci-postgres \
  -keyout "$dir/server.key" -out "$dir/server.crt" 2>/dev/null
password=$(openssl rand -hex 16)
printf '%s\n' "$password" > "$dir/pw"
chmod 0755 "$dir" && chmod 0644 "$dir"/*
docker run -d --name aviary-ci-postgres -p "127.0.0.1:$port:5432" -v "$dir:/ci:ro" "$image" bash -ec '
  bin=$(ls -d /usr/lib/postgresql/*/bin)
  "$bin/initdb" -D /tmp/pgdata -U postgres --pwfile=/ci/pw -A scram-sha-256 >/dev/null
  cp /ci/server.crt /ci/server.key /tmp/pgdata/ && chmod 0600 /tmp/pgdata/server.key
  printf "local all all trust\nhostssl all all all scram-sha-256\n" > /tmp/pgdata/pg_hba.conf
  exec "$bin/postgres" -D /tmp/pgdata -c listen_addresses="*" -c ssl=on -c fsync=off -c max_connections=200
' >/dev/null
for _ in $(seq 1 60); do
  if docker exec aviary-ci-postgres pg_isready -q -h 127.0.0.1 2>/dev/null; then
    echo "postgres://postgres:$password@127.0.0.1:$port/postgres?sslmode=require"
    exit 0
  fi
  sleep 1
done
docker logs aviary-ci-postgres >&2
exit 1
