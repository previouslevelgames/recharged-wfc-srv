#!/usr/bin/env bash
set -Eeuo pipefail

: "${POSTGRES_PASSWORD:?Set POSTGRES_PASSWORD to a non-empty database password}"

postgres_pid=
server_pid=

stop_services() {
    trap - EXIT
    trap '' TERM INT
    if [[ -n "$server_pid" ]]; then
        kill -TERM "$server_pid" 2>/dev/null || true
    fi
    if [[ -n "$postgres_pid" ]]; then
        # PostgreSQL's fast shutdown flushes data and aborts active transactions.
        kill -INT "$postgres_pid" 2>/dev/null || true
    fi
    if [[ -n "$server_pid" ]]; then
        wait "$server_pid" 2>/dev/null || true
    fi
    if [[ -n "$postgres_pid" ]]; then
        wait "$postgres_pid" 2>/dev/null || true
    fi
}
trap 'stop_services' EXIT
trap 'exit 0' TERM INT

# Reuse the official image's initialization and ownership handling. Existing
# databases are left intact. Keep this foreground step together during shutdown.
docker-ensure-initdb.sh

gosu postgres postgres -c listen_addresses=127.0.0.1 -p 5432 &
postgres_pid=$!

export PGHOST=127.0.0.1 PGPORT=5432
export PGUSER="$POSTGRES_USER" PGDATABASE="$POSTGRES_DB"
export PGPASSWORD="$POSTGRES_PASSWORD"

until pg_isready -q; do
    if ! kill -0 "$postgres_pid" 2>/dev/null; then
        echo "PostgreSQL exited before it was ready." >&2
        exit 1
    fi
    sleep 0.2
done

# Check the supplied credentials against the persisted database before serving.
psql -X --no-password -v ON_ERROR_STOP=1 -c 'SELECT 1' >/dev/null

gosu 10001:10001 /usr/local/bin/recharged-wfc-srv &
server_pid=$!

status=0
wait -n "$postgres_pid" "$server_pid" || status=$?
echo "A service exited unexpectedly (status $status); stopping the container." >&2
stop_services
exit 1
