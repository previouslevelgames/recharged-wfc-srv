#!/usr/bin/env bash
set -Eeuo pipefail

: "${POSTGRES_PASSWORD:?Set POSTGRES_PASSWORD to a non-empty database password}"

export PGUSER="$POSTGRES_USER" PGDATABASE="$POSTGRES_DB"
export PGPASSWORD="$POSTGRES_PASSWORD" PGPORT="${POSTGRES_PORT:-5432}"

# The server writes its daily log files as 10001; a newly mapped folder belongs
# to root.
prepare_log_dir() {
    if [[ -n "${LOG_DIR:-}" ]]; then
        mkdir -p "$LOG_DIR"
        chown 10001:10001 "$LOG_DIR"
    fi
}

# With POSTGRES_HOST, the database runs in a container of its own, which
# several servers can share, and this container runs only the server.
if [[ -n "${POSTGRES_HOST:-}" ]]; then
    export PGHOST="$POSTGRES_HOST"
    if [[ -f "${PGDATA:-}/PG_VERSION" ]]; then
        echo "POSTGRES_HOST is set, so the SQL data folder mapped at $PGDATA is unused here; remove the mapping." >&2
    fi
    # The database container may start after this one, e.g. at boot.
    waited=0
    until pg_isready -q; do
        if (( waited % 30 == 0 )); then
            echo "Waiting for PostgreSQL at $PGHOST:$PGPORT..." >&2
        fi
        sleep 1
        waited=$((waited + 1))
    done
    # Check the supplied credentials against the database before serving.
    psql -X --no-password -v ON_ERROR_STOP=1 -c 'SELECT 1' >/dev/null
    prepare_log_dir
    exec gosu 10001:10001 /usr/local/bin/recharged-wfc-srv
fi

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

gosu postgres postgres -c listen_addresses=127.0.0.1 -p "$PGPORT" &
postgres_pid=$!

export PGHOST=127.0.0.1

until pg_isready -q; do
    if ! kill -0 "$postgres_pid" 2>/dev/null; then
        echo "PostgreSQL exited before it was ready." >&2
        exit 1
    fi
    sleep 0.2
done

# Check the supplied credentials against the persisted database before serving.
psql -X --no-password -v ON_ERROR_STOP=1 -c 'SELECT 1' >/dev/null

prepare_log_dir

gosu 10001:10001 /usr/local/bin/recharged-wfc-srv &
server_pid=$!

status=0
wait -n "$postgres_pid" "$server_pid" || status=$?
echo "A service exited unexpectedly (status $status); stopping the container." >&2
stop_services
exit 1
