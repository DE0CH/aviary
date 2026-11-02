#!/usr/bin/env bash
# Start a 2-node Postgres with synchronous replication (magpie 6.10's write acceptance: a commit returns only after a
# synchronous standby has the WAL, as magpie-pg's CNPG cluster does): the production image, TLS on, scram passwords,
# fsync on, a primary and one streaming standby named in synchronous_standby_names. Prints the primary's URL.
#   postgres-sync.sh <image> <host port>
set -euo pipefail
image=$1
port=$2
net=aviary-ci-pgsync
dir=$(mktemp -d)
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj /CN=aviary-ci-postgres \
  -keyout "$dir/server.key" -out "$dir/server.crt" 2>/dev/null
password=$(openssl rand -hex 16)
printf '%s\n' "$password" > "$dir/pw"
chmod 0755 "$dir" && chmod 0644 "$dir"/*
docker network create "$net" >/dev/null
docker run -d --name aviary-ci-pg-primary --network "$net" -p "127.0.0.1:$port:5432" -v "$dir:/ci:ro" "$image" bash -ec '
  bin=$(ls -d /usr/lib/postgresql/*/bin)
  "$bin/initdb" -D /tmp/pgdata -U postgres --pwfile=/ci/pw -A scram-sha-256 >/dev/null
  cp /ci/server.crt /ci/server.key /tmp/pgdata/ && chmod 0600 /tmp/pgdata/server.key
  printf "local all all trust\nhostssl all all all scram-sha-256\nhost replication postgres all scram-sha-256\n" \
    > /tmp/pgdata/pg_hba.conf
  exec "$bin/postgres" -D /tmp/pgdata -c listen_addresses="*" -c ssl=on -c max_connections=200 \
    -c wal_level=replica -c max_wal_senders=4 -c synchronous_standby_names=standby1 -c synchronous_commit=on
' >/dev/null
for _ in $(seq 1 60); do
  docker exec aviary-ci-pg-primary pg_isready -q -h 127.0.0.1 2>/dev/null && break
  sleep 1
done
# the standby allows at least what the primary does (max_connections, max_wal_senders), or recovery stops
docker run -d --name aviary-ci-pg-standby --network "$net" -v "$dir:/ci:ro" "$image" bash -ec '
  bin=$(ls -d /usr/lib/postgresql/*/bin)
  export PGPASSWORD=$(cat /ci/pw)
  "$bin/pg_basebackup" -h aviary-ci-pg-primary -U postgres -D /tmp/pgdata -X stream -R \
    -d "application_name=standby1 sslmode=disable" >/dev/null
  chmod 0700 /tmp/pgdata
  exec "$bin/postgres" -D /tmp/pgdata -c hot_standby=on -c ssl=on -c max_connections=200 -c max_wal_senders=4
' >/dev/null
for _ in $(seq 1 60); do
  state=$(docker exec aviary-ci-pg-primary psql -U postgres -tAc \
    "select sync_state from pg_stat_replication where application_name = 'standby1'" 2>/dev/null || true)
  if [ "$state" = sync ]; then
    echo "postgres://postgres:$password@127.0.0.1:$port/postgres?sslmode=require"
    exit 0
  fi
  sleep 1
done
docker logs aviary-ci-pg-primary >&2
docker logs aviary-ci-pg-standby >&2
exit 1
